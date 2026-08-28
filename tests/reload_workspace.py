#!/usr/bin/env python3
"""Déclenche intellij/reloadWorkspace sur l'instance proxy du projet —
l'équivalent manuel du bouton « Load Gradle Changes » d'IntelliJ.

Usage : python3 tests/reload_workspace.py <racine-du-projet> [workdir-extension]

Le <workdir-extension> est celui de l'extension Zed
(défaut : ~/Library/Application Support/Zed/extensions/work/intellij-server).

Bindable dans Zed, par exemple dans tasks.json :
  {
    "label": "IntelliJ: reload workspace",
    "command": "python3 /chemin/vers/tests/reload_workspace.py \\"$ZED_WORKTREE_ROOT\\"",
    "hide": "never"
  }
"""
import hashlib
import json
import socket
import sys
from pathlib import Path

root = sys.argv[1]
default_workdir = Path.home() / "Library/Application Support/Zed/extensions/work/intellij-server"
workdir = Path(sys.argv[2]) if len(sys.argv) > 2 else default_workdir

# Doit reproduire exactement system_path_for() de l'extension (sha256, 16 hex).
digest = hashlib.sha256(root.encode()).hexdigest()[:16]
session_file = workdir / "system-path" / digest / "ij-zed-proxy.session.json"
if not session_file.exists():
    sys.exit(f"pas de session proxy pour {root} ({session_file} introuvable — serveur démarré ?)")

session = json.load(open(session_file))
with socket.create_connection(("127.0.0.1", session["port"]), timeout=30) as s:
    f = s.makefile("rw")
    f.write(json.dumps({
        "id": 1,
        "token": session["token"],
        "command": "__reload_workspace",
        "arguments": [],
    }) + "\n")
    f.flush()
    s.settimeout(30)
    print(f.readline().strip(), flush=True)
