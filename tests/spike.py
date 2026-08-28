#!/usr/bin/env python3
"""M1 spike — pilote LSP générique pour intellij-server, sans VS Code.

Imite un client LSP standard (type Zed) : pas de capability licensingUi,
pas d'opt-in intellijExtensions (sauf flags explicites).

Usage:
  python3 spike.py [--server-dir DIR] [--no-eula] [--no-system-path]
                   [--licensing-ui] [--intellij-extensions] [--dap]
                   [--wait SECONDS] [--transcript FILE]
"""
import argparse
import hashlib
import json
import os
import queue
import socket
import subprocess
import sys
import threading
import time
from pathlib import Path

SPIKE_DIR = Path(__file__).resolve().parent
PROJECT_DIR = SPIKE_DIR / "test-project"
STORAGE_DIR = SPIKE_DIR / "storage"
MAIN_JAVA = PROJECT_DIR / "src/main/java/com/example/Main.java"
DEFAULT_SDK = "/Library/Java/JavaVirtualMachines/zulu-25.jdk/Contents/Home"

REPORT = {"checks": [], "questions": {}}


def check(name, ok, detail=""):
    REPORT["checks"].append({"name": name, "ok": bool(ok), "detail": detail})
    print(f"[{'PASS' if ok else 'FAIL'}] {name}" + (f" — {detail}" if detail else ""), flush=True)


def find_server_dir() -> Path:
    for p in sorted(SPIKE_DIR.glob("server/**/bin/intellij-server"), key=lambda p: len(str(p))):
        return p.parent.parent
    sys.exit("serveur introuvable sous m1-spike/server/**/bin/intellij-server")


def eula_hash(server_dir: Path) -> str:
    data = (server_dir / "EULA.txt").read_bytes()
    return hashlib.sha256(data).hexdigest()[:16]


class LspClient:
    def __init__(self, proc, transcript):
        self.proc = proc
        self.transcript = transcript
        self.next_id = 1
        self.pending = {}
        self.pending_lock = threading.Lock()
        self.write_lock = threading.Lock()
        self.diagnostics = {}
        self.server_requests = []
        self.alive = True
        self.reader = threading.Thread(target=self._read_loop, daemon=True)
        self.reader.start()

    def _log(self, direction, obj):
        line = json.dumps({"dir": direction, "msg": obj}, ensure_ascii=False)
        if len(line) > 8000:
            line = line[:8000] + "…<truncated>"
        self.transcript.write(line + "\n")
        self.transcript.flush()

    def _read_loop(self):
        buf = bytearray()
        out = self.proc.stdout
        try:
            while True:
                chunk = out.read1(65536) if hasattr(out, "read1") else out.read(65536)
                if not chunk:
                    break
                buf.extend(chunk)
                while True:
                    sep = buf.find(b"\r\n\r\n")
                    if sep < 0:
                        break
                    headers = buf[:sep].decode("ascii", "replace")
                    length = None
                    for h in headers.split("\r\n"):
                        if h.lower().startswith("content-length:"):
                            length = int(h.split(":", 1)[1].strip())
                    if length is None or len(buf) < sep + 4 + length:
                        break
                    body = bytes(buf[sep + 4: sep + 4 + length])
                    del buf[: sep + 4 + length]
                    try:
                        msg = json.loads(body)
                    except json.JSONDecodeError:
                        continue
                    self._dispatch(msg)
        except (OSError, ValueError):
            pass
        self.alive = False
        with self.pending_lock:
            for ev, box in self.pending.values():
                box["error"] = "connection closed"
                ev.set()

    def _dispatch(self, msg):
        self._log("in", msg)
        if "method" in msg and "id" in msg:  # requête serveur→client
            self._handle_server_request(msg)
        elif "method" in msg:  # notification
            self._handle_notification(msg)
        elif "id" in msg:  # réponse
            with self.pending_lock:
                entry = self.pending.pop(msg["id"], None)
            if entry:
                ev, box = entry
                box["response"] = msg
                ev.set()

    # --- réponses minimales aux requêtes serveur, fidèles à un client type Zed ---
    KNOWN_REQUESTS = {
        "window/workDoneProgress/create": lambda p: None,
        "client/registerCapability": lambda p: None,
        "client/unregisterCapability": lambda p: None,
        "workspace/workspaceFolders": lambda p: [
            {"uri": PROJECT_DIR.as_uri(), "name": "test-project"}
        ],
        "workspace/configuration": lambda p: [None] * len(p.get("items", [])),
        "window/showMessageRequest": lambda p: None,
        "workspace/applyEdit": lambda p: {"applied": True},
        "workspace/codeLens/refresh": lambda p: None,
        "workspace/semanticTokens/refresh": lambda p: None,
        "workspace/inlayHint/refresh": lambda p: None,
        "workspace/diagnostic/refresh": lambda p: None,
    }

    def _handle_server_request(self, msg):
        self.server_requests.append(msg["method"])
        handler = self.KNOWN_REQUESTS.get(msg["method"])
        if handler:
            self._respond(msg["id"], handler(msg.get("params") or {}))
        else:  # comme Zed : méthode inconnue → -32601
            self._respond_error(msg["id"], -32601, "Method not found")

    def _handle_notification(self, msg):
        m = msg["method"]
        if m == "textDocument/publishDiagnostics":
            p = msg["params"]
            self.diagnostics[p["uri"]] = p.get("diagnostics", [])
        elif m == "window/logMessage":
            p = msg["params"]
            print(f"  [log:{p.get('type')}] {str(p.get('message'))[:200]}", flush=True)
        elif m == "window/showMessage":
            p = msg["params"]
            print(f"  [msg:{p.get('type')}] {str(p.get('message'))[:200]}", flush=True)

    def _send(self, obj):
        data = json.dumps(obj).encode()
        with self.write_lock:
            self.proc.stdin.write(b"Content-Length: %d\r\n\r\n" % len(data) + data)
            self.proc.stdin.flush()
        self._log("out", obj)

    def _respond(self, rid, result):
        self._send({"jsonrpc": "2.0", "id": rid, "result": result})

    def _respond_error(self, rid, code, message):
        self._send({"jsonrpc": "2.0", "id": rid, "error": {"code": code, "message": message}})

    def notify(self, method, params=None):
        self._send({"jsonrpc": "2.0", "method": method, "params": params or {}})

    def request(self, method, params=None, timeout=120):
        rid = self.next_id
        self.next_id += 1
        ev = threading.Event()
        box = {}
        with self.pending_lock:
            self.pending[rid] = (ev, box)
        self._send({"jsonrpc": "2.0", "id": rid, "method": method, "params": params or {}})
        if not ev.wait(timeout):
            return {"error": {"code": -1, "message": f"client timeout {timeout}s"}}
        if "error" in box and "response" not in box:
            return {"error": {"code": -2, "message": box["error"]}}
        return box["response"]


def dap_probe(port, timeout=30):
    """Question 4 : le port de start_debug_server parle-t-il un DAP complet ?"""
    try:
        with socket.create_connection(("127.0.0.1", port), timeout=10) as s:
            s.settimeout(timeout)
            req = {"seq": 1, "type": "request", "command": "initialize",
                   "arguments": {"adapterID": "intellij_debugger"}}
            data = json.dumps(req).encode()
            s.sendall(b"Content-Length: %d\r\n\r\n" % len(data) + data)
            buf = b""
            deadline = time.time() + timeout
            while time.time() < deadline:
                try:
                    chunk = s.recv(65536)
                except socket.timeout:
                    break
                if not chunk:
                    return False, "connexion fermée sans réponse"
                buf += chunk
                sep = buf.find(b"\r\n\r\n")
                if sep >= 0:
                    length = None
                    for h in buf[:sep].decode("ascii", "replace").split("\r\n"):
                        if h.lower().startswith("content-length:"):
                            length = int(h.split(":", 1)[1].strip())
                    if length is not None and len(buf) >= sep + 4 + length:
                        body = json.loads(buf[sep + 4: sep + 4 + length])
                        ok = body.get("type") == "response" and body.get("command") == "initialize"
                        return ok, json.dumps(body)[:600]
            return False, "pas de trame DAP complète dans le délai"
    except OSError as e:
        return False, f"connexion impossible: {e}"


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--server-dir", type=Path, default=None)
    ap.add_argument("--no-eula", action="store_true")
    ap.add_argument("--no-system-path", action="store_true")
    ap.add_argument("--licensing-ui", action="store_true")
    ap.add_argument("--intellij-extensions", action="store_true")
    ap.add_argument("--dap", action="store_true", help="tester start_debug_server + handshake DAP")
    ap.add_argument("--proxy", type=Path, default=None,
                    help="lancer le serveur via ij-zed-proxy (chemin du binaire)")
    ap.add_argument("--hold", action="store_true",
                    help="après les checks, maintenir l'instance (fichier sentinelle dap-test-done)")
    ap.add_argument("--wait", type=int, default=300, help="budget complétion (s)")
    ap.add_argument("--transcript", type=Path, default=None)
    args = ap.parse_args()

    server_dir = (args.server_dir or find_server_dir()).resolve()
    launcher = server_dir / "bin" / ("intellij-server.exe" if os.name == "nt" else "intellij-server")
    if not launcher.exists():
        sys.exit(f"launcher introuvable: {launcher}")

    transcript_path = args.transcript or (SPIKE_DIR / "transcript.jsonl")
    transcript = open(transcript_path, "w", encoding="utf-8")
    print(f"serveur   : {server_dir}", flush=True)
    print(f"transcript: {transcript_path}", flush=True)

    cmd = [str(launcher), "--stdio"]
    if not args.no_system_path:
        STORAGE_DIR.mkdir(exist_ok=True)
        cmd += ["--system-path", str(STORAGE_DIR)]
    if not args.no_eula:
        cmd += ["--eula", eula_hash(server_dir)]
    if args.proxy:
        proxy_log = SPIKE_DIR / "proxy.log"
        proxy_log.write_text("")
        cmd = [str(args.proxy), "--log", str(proxy_log), "--"] + cmd

    env = dict(os.environ)
    env.pop("IJ_LAUNCHER_DEBUG", None)
    print(f"lancement : {' '.join(cmd)}", flush=True)
    proc = subprocess.Popen(cmd, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                            stderr=subprocess.PIPE, env=env)
    stderr_lines = []

    def drain_stderr():
        for line in proc.stderr:
            stderr_lines.append(line.decode("utf-8", "replace").rstrip())
            print(f"  [stderr] {stderr_lines[-1][:300]}", flush=True)

    threading.Thread(target=drain_stderr, daemon=True).start()
    client = LspClient(proc, transcript)
    root_uri = PROJECT_DIR.as_uri()

    capabilities = {
        "textDocument": {
            "synchronization": {"didSave": True, "dynamicRegistration": False},
            "completion": {"completionItem": {"documentationFormat": ["markdown", "plaintext"],
                                              "snippetSupport": False}},
            "hover": {"contentFormat": ["markdown", "plaintext"]},
            "definition": {"linkSupport": False},
            "typeDefinition": {}, "implementation": {}, "references": {},
            "documentSymbol": {}, "rename": {"prepareSupport": False},
            "formatting": {}, "signatureHelp": {},
            "publishDiagnostics": {"relatedInformation": True},
            "inlayHint": {"resolveSupport": {"properties": []}},
            "codeLens": {},
        },
        "workspace": {"workspaceFolders": True, "configuration": True, "applyEdit": True},
        "window": {"workDoneProgress": True},
    }
    if args.licensing_ui:
        capabilities["experimental"] = {"licensingUi": True}

    init_options = {
        "defaultSdk": DEFAULT_SDK,
        "projects": [],
        "disableRocksDBWriteAheadLog": False,
    }
    if args.intellij_extensions:
        init_options["intellijExtensions"] = True

    t0 = time.time()
    init_resp = client.request("initialize", {
        "processId": os.getpid(),
        "rootUri": root_uri,
        "capabilities": capabilities,
        "initializationOptions": init_options,
        "workspaceFolders": [{"uri": root_uri, "name": "test-project"}],
    }, timeout=180)
    elapsed = time.time() - t0

    if "error" in init_resp:
        check("initialize", False, f"{init_resp['error']} (après {elapsed:.1f}s)")
        if args.no_eula:
            REPORT["questions"]["initialize sans --eula"] = f"rejeté: {init_resp['error']}"
        finish(client, proc, stderr_lines, transcript, graceful=False)
        return

    result = init_resp.get("result") or {}
    server_caps = result.get("capabilities", {})
    check("initialize", True, f"{elapsed:.1f}s")
    print("capacités serveur : " + ", ".join(sorted(server_caps.keys())), flush=True)
    if args.no_eula:
        REPORT["questions"]["initialize sans --eula"] = "accepté (inattendu)"
    if args.no_system_path:
        REPORT["questions"]["initialize sans --system-path"] = "accepté"
    experimental = server_caps.get("experimental") or {}
    if "indexDir" in experimental:
        print(f"indexDir : {experimental['indexDir']}", flush=True)

    client.notify("initialized", {})

    # --- ouverture du fichier et attente de l'indexation ---
    text = MAIN_JAVA.read_text()
    doc_uri = MAIN_JAVA.as_uri()
    client.notify("textDocument/didOpen", {"textDocument": {
        "uri": doc_uri, "languageId": "java", "version": 1, "text": text}})

    deadline = time.time() + args.wait
    completion = None
    while time.time() < deadline:
        resp = client.request("textDocument/completion", {
            "textDocument": {"uri": doc_uri}, "position": {"line": 12, "character": 14}},
            timeout=60)
        if "error" not in resp:
            result = resp.get("result")
            items = result.get("items") if isinstance(result, dict) else result
            if items:
                completion = items
                break
        time.sleep(5)
    if completion is None:
        check("completion", False, "aucun item dans le budget imparti")
    else:
        labels = [i.get("label") for i in completion[:10]]
        check("completion", True, f"{len(completion)} items, ex: {labels}")
        check("completion pertinente (add/equals…)",
              any(str(l).startswith(("add", "equals", "size")) for l in labels), str(labels))

    hover = client.request("textDocument/hover", {
        "textDocument": {"uri": doc_uri}, "position": {"line": 8, "character": 27}}, timeout=60)
    hover_ok = "error" not in hover and (hover.get("result") or {}).get("contents")
    check("hover", bool(hover_ok), str(hover.get("result"))[:150] if hover_ok else str(hover)[:200])

    # définition intra-projet : Greeter (ligne 7, col 10)
    defin = client.request("textDocument/definition", {
        "textDocument": {"uri": doc_uri}, "position": {"line": 7, "character": 10}}, timeout=60)
    def_result = defin.get("result") if "error" not in defin else None
    if isinstance(def_result, dict):
        def_result = [def_result]
    intra_ok = def_result and any("Greeter.java" in (loc.get("uri") or loc.get("targetUri", ""))
                                  for loc in def_result)
    check("definition intra-projet (Greeter)", bool(intra_ok),
          json.dumps(def_result)[:300] if def_result else str(defin)[:200])

    # définition vers le JDK : java.util.List (ligne 11, col 10) → question 3 (decompile)
    defin_jdk = client.request("textDocument/definition", {
        "textDocument": {"uri": doc_uri}, "position": {"line": 11, "character": 10}}, timeout=60)
    jdk_result = defin_jdk.get("result") if "error" not in defin_jdk else None
    if isinstance(jdk_result, dict):
        jdk_result = [jdk_result]
    jdk_uri = None
    if jdk_result:
        jdk_uri = (jdk_result[0].get("uri") or jdk_result[0].get("targetUri"))
    check("definition vers JDK (List)", bool(jdk_uri), str(jdk_uri)[:200])
    if jdk_uri and jdk_uri.startswith(("jar:", "jrt:")) and not args.proxy:
        dec = client.request("workspace/executeCommand", {
            "command": "decompile", "arguments": [jdk_uri]}, timeout=60)
        if "error" not in dec and isinstance(dec.get("result"), dict) and dec["result"].get("code"):
            code = dec["result"]["code"]
            check("decompile sans opt-in intellijExtensions", True,
                  f"lang={dec['result'].get('language')}, {len(code)} chars")
            REPORT["questions"]["decompile sans intellijExtensions"] = "fonctionne"
        else:
            check("decompile sans opt-in intellijExtensions", False, str(dec)[:300])
            REPORT["questions"]["decompile sans intellijExtensions"] = f"échoue: {str(dec)[:200]}"
    elif jdk_uri and jdk_uri.startswith("file://") and args.proxy:
        from urllib.parse import unquote, urlparse
        materialized = Path(unquote(urlparse(jdk_uri).path))
        check("proxy: definition JDK matérialisée en fichier réel", materialized.exists(),
              f"{materialized} ({materialized.stat().st_size if materialized.exists() else 0} o)")
        if materialized.exists():
            head = materialized.read_text(errors="replace")[:200]
            check("proxy: contenu décompilé plausible", "java" in head or "class" in head
                  or "interface" in head, head.replace("\n", " ")[:120])
        REPORT["questions"]["définition JDK via proxy"] = f"file:// matérialisé: {materialized.name}"
    else:
        REPORT["questions"]["decompile sans intellijExtensions"] = \
            f"non testé (uri={str(jdk_uri)[:120]})"

    if args.proxy:
        # didSave sur un descripteur de build → le proxy doit émettre intellij/reloadWorkspace
        client.notify("textDocument/didSave", {"textDocument": {"uri": (
            PROJECT_DIR / "build.gradle").as_uri()}})
        proxy_log = SPIKE_DIR / "proxy.log"
        reloaded = False
        for _ in range(20):
            time.sleep(1)
            if "workspace reloaded" in proxy_log.read_text(errors="replace"):
                reloaded = True
                break
        check("proxy: didSave build.gradle → reloadWorkspace", reloaded)

    # rename : variable `message` (ligne 8, col 17)
    ren = client.request("textDocument/rename", {
        "textDocument": {"uri": doc_uri}, "position": {"line": 8, "character": 17},
        "newName": "renamedMessage"}, timeout=60)
    ren_result = ren.get("result") or {}
    ren_ok = "error" not in ren and (ren_result.get("changes") or ren_result.get("documentChanges"))
    check("rename", bool(ren_ok), str(ren.get("result"))[:200] if ren_ok else str(ren)[:200])

    # question 4 : DAP
    if args.dap:
        dap_resp = client.request("workspace/executeCommand", {
            "command": "start_debug_server", "arguments": [root_uri]}, timeout=60)
        port = dap_resp.get("result") if "error" not in dap_resp else None
        if isinstance(port, int) and port > 0:
            check("start_debug_server → port", True, f"port {port}")
            ok, detail = dap_probe(port)
            check("handshake DAP sur le port", ok, detail)
            REPORT["questions"]["DAP sur start_debug_server"] = \
                ("complet" if ok else f"incomplet: {detail}")
        else:
            check("start_debug_server → port", False, str(dap_resp)[:300])
            REPORT["questions"]["DAP sur start_debug_server"] = f"pas de port: {str(dap_resp)[:200]}"

    check("diagnostics reçus", True, f"{len(client.diagnostics)} document(s)")
    for uri, diags in client.diagnostics.items():
        print(f"  {uri}: {len(diags)} diagnostic(s)", flush=True)

    if args.hold:
        done = SPIKE_DIR / "dap-test-done"
        done.unlink(missing_ok=True)
        print("READY — instance maintenue pour dap_test.py", flush=True)
        deadline = time.time() + 600
        while time.time() < deadline and not done.exists():
            time.sleep(1)
        done.unlink(missing_ok=True)

    finish(client, proc, stderr_lines, transcript, graceful=True)


def finish(client, proc, stderr_lines, transcript, graceful):
    if graceful:
        shut = client.request("shutdown", timeout=30)
        check("shutdown", "error" not in shut, str(shut.get("error", ""))[:150])
        client.notify("exit")
    else:
        try:
            client.notify("exit")
        except BrokenPipeError:
            pass
    try:
        code = proc.wait(timeout=20)
        print(f"exit code serveur : {code}", flush=True)
    except subprocess.TimeoutExpired:
        proc.kill()
        print("serveur tué (pas de sortie sous 20s)", flush=True)
    transcript.close()
    print("\n===== QUESTIONS OUVERTES (§7 du plan) =====")
    for q, r in REPORT["questions"].items():
        print(f"- {q}: {r}")
    fails = [c for c in REPORT["checks"] if not c["ok"]]
    print(f"\n===== {len(REPORT['checks']) - len(fails)}/{len(REPORT['checks'])} checks OK =====")


if __name__ == "__main__":
    main()
