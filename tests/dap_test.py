#!/usr/bin/env python3
"""Test M4 de bout en bout : ij-zed-proxy --dap face à une instance LSP vivante.

Prérequis : une instance ij-zed-proxy (mode LSP) tourne avec son fichier de
session dans --system-path (lancée par spike.py --proxy --hold, ou à la main).

Usage : python3 dap_test.py <proxy-bin> <system-path> <root-uri> [main-class]
"""
import json
import subprocess
import sys
import threading
import time
from pathlib import Path

SPIKE_DIR = Path(__file__).resolve().parent
MAIN_JAVA = SPIKE_DIR / "test-project/src/main/java/com/example/Main.java"

CHECKS = []


def check(name, ok, detail=""):
    CHECKS.append((name, bool(ok)))
    print(f"[{'PASS' if ok else 'FAIL'}] {name}" + (f" — {detail}" if detail else ""), flush=True)


class DapClient:
    def __init__(self, proc):
        self.proc = proc
        self.seq = 0
        self.events = []
        self.responses = {}
        self.lock = threading.Lock()
        self.alive = True
        threading.Thread(target=self._read, daemon=True).start()

    def _read(self):
        buf = bytearray()
        out = self.proc.stdout
        try:
            while True:
                chunk = out.read1(65536)
                if not chunk:
                    break
                buf.extend(chunk)
                while True:
                    sep = buf.find(b"\r\n\r\n")
                    if sep < 0:
                        break
                    length = None
                    for h in buf[:sep].decode("ascii", "replace").split("\r\n"):
                        if h.lower().startswith("content-length:"):
                            length = int(h.split(":", 1)[1].strip())
                    if length is None or len(buf) < sep + 4 + length:
                        break
                    body = json.loads(buf[sep + 4: sep + 4 + length])
                    del buf[: sep + 4 + length]
                    self._dispatch(body)
        except (OSError, ValueError):
            pass
        self.alive = False
        with self.lock:
            for ev in self.responses.values():
                ev.set()

    def _dispatch(self, msg):
        t = msg.get("type")
        if t == "event":
            self.events.append(msg)
            print(f"  [event:{msg.get('event')}] {json.dumps(msg.get('body'))[:180]}", flush=True)
        elif t == "response":
            with self.lock:
                slot = self.responses.setdefault(msg["request_seq"], {"event": threading.Event(), "msg": None})
                slot["msg"] = msg
                slot["event"].set()
            print(f"  [resp:{msg.get('command')}] success={msg.get('success')} {str(msg.get('message'))[:120]}", flush=True)

    def request(self, command, arguments=None, timeout=90):
        self.seq += 1
        seq = self.seq
        msg = {"seq": seq, "type": "request", "command": command}
        if arguments is not None:
            msg["arguments"] = arguments
        data = json.dumps(msg).encode()
        with self.lock:
            slot = self.responses.setdefault(seq, {"event": threading.Event(), "msg": None})
            event = slot["event"]
        self.proc.stdin.write(b"Content-Length: %d\r\n\r\n" % len(data) + data)
        self.proc.stdin.flush()
        if not event.wait(timeout):
            return {"success": False, "message": f"timeout {timeout}s"}
        with self.lock:
            return self.responses.get(seq, {}).get("msg") or {"success": False, "message": "closed"}


def main():
    proxy_bin, system_path, root_uri = sys.argv[1], sys.argv[2], sys.argv[3]
    main_class = sys.argv[4] if len(sys.argv) > 4 else "com.example.Main"

    proc = subprocess.Popen(
        [proxy_bin, "--dap", "--system-path", system_path, "--root-uri", root_uri],
        stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    threading.Thread(target=lambda: [print(f"  [dap-stderr] {l.decode().rstrip()[:200]}", flush=True)
                                     for l in proc.stderr], daemon=True).start()
    dap = DapClient(proc)

    init = dap.request("initialize", {"adapterID": "intellij-debugger"})
    check("initialize DAP", init.get("success"), str(init.get("message"))[:150])

    bp_line = 10  # System.out.println(message);
    bps = dap.request("setBreakpoints", {
        "source": {"path": str(MAIN_JAVA), "name": "Main.java"},
        "breakpoints": [{"line": bp_line}],
    })
    verified = [b.get("verified") for b in (bps.get("body") or {}).get("breakpoints", [])]
    check("setBreakpoints vérifié", bps.get("success") and all(verified), f"{verified}")

    launch = dap.request("launch", {
        "mainClass": main_class,
        "console": "internalConsole",
        "stopOnEntry": False,
    })
    check("launch accepté", launch.get("success") is not False, str(launch.get("message"))[:200])

    dap.request("configurationDone")

    stopped = False
    hello = False
    terminated = False
    deadline = time.time() + 120
    while time.time() < deadline and not terminated:
        time.sleep(0.5)
        for ev in list(dap.events):
            if ev.get("_handled"):
                continue
            ev["_handled"] = True
            name = ev.get("event")
            if name == "stopped":
                stopped = True
                thread_id = (ev.get("body") or {}).get("threadId", 1)
                print(f"  → breakpoint atteint (thread {thread_id}), continue", flush=True)
                dap.request("continue", {"threadId": thread_id})
            elif name == "output":
                out = str((ev.get("body") or {}).get("output", ""))
                if "Hello, world!" in out:
                    hello = True
            elif name == "terminated":
                terminated = True
    check("breakpoint atteint (stopped)", stopped)
    check("sortie 'Hello, world!' reçue", hello)
    check("session terminée proprement (terminated)", terminated)

    try:
        proc.stdin.close()
        proc.wait(timeout=10)
    except Exception:
        proc.kill()
    fails = [c for c in CHECKS if not c[1]]
    print(f"\n===== {len(CHECKS) - len(fails)}/{len(CHECKS)} checks OK =====", flush=True)
    sys.exit(1 if fails else 0)


if __name__ == "__main__":
    main()
