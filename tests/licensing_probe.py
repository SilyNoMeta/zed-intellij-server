#!/usr/bin/env python3
"""Sonde le protocole jetbrains/licensing/* SANS la capability licensingUi
(= notre mode de production dans Zed)."""
import json
import subprocess
import sys
import threading
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from spike import LspClient, eula_hash, find_server_dir, STORAGE_DIR, PROJECT_DIR, DEFAULT_SDK

server_dir = find_server_dir()
launcher = server_dir / "bin" / "intellij-server"
cmd = [str(launcher), "--stdio", "--system-path", str(STORAGE_DIR), "--eula", eula_hash(server_dir)]
print("lancement:", " ".join(cmd), flush=True)
proc = subprocess.Popen(cmd, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                        stderr=subprocess.DEVNULL)
transcript = open(Path(__file__).parent / "transcript-licensing.jsonl", "w")
client = LspClient(proc, transcript)

root_uri = PROJECT_DIR.as_uri()
resp = client.request("initialize", {
    "processId": None,
    "rootUri": root_uri,
    "capabilities": {"textDocument": {}, "workspace": {"workspaceFolders": True}},
    "initializationOptions": {"defaultSdk": DEFAULT_SDK, "projects": []},
    "workspaceFolders": [{"uri": root_uri, "name": "test-project"}],
}, timeout=120)
print("initialize:", "OK" if "error" not in resp else resp["error"], flush=True)
client.notify("initialized", {})


def probe(label, method, params=None):
    r = client.request(method, params, timeout=60)
    if "error" in r:
        print(f"{label}: ERREUR {json.dumps(r['error'])[:250]}", flush=True)
        return None
    print(f"{label}: {json.dumps(r.get('result'), ensure_ascii=False)[:500]}", flush=True)
    return r.get("result")


probe("state/get (avant)", "jetbrains/licensing/state/get")
probe("discovery/autoActivate", "jetbrains/licensing/discovery/autoActivate")
time.sleep(2)
state = probe("state/get (après)", "jetbrains/licensing/state/get")
if isinstance(state, dict):
    lic = state.get("activeLicense")
    print("→ licence active:", json.dumps(lic, ensure_ascii=False)[:300] if lic else "aucune", flush=True)

client.request("shutdown", timeout=15)
client.notify("exit")
proc.wait(timeout=15)
print("fin.", flush=True)
