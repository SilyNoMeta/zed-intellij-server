#!/usr/bin/env python3
"""Demande au proxy en cours de vider les caches du backend et de redémarrer.

Usage : python3 tests/clear_caches.py <system-path>
Le <system-path> est celui du worktree (contient ij-zed-proxy.session.json) ;
sous Zed il ressemble à :
  ~/Library/Application Support/Zed/extensions/work/intellij-server/system-path/<hash>
"""
import json
import socket
import sys
from pathlib import Path

session = json.load(open(Path(sys.argv[1]) / "ij-zed-proxy.session.json"))
with socket.create_connection(("127.0.0.1", session["port"]), timeout=15) as s:
    f = s.makefile("rw")
    f.write(json.dumps({
        "id": 1,
        "token": session["token"],
        "command": "__clear_caches_and_restart",
        "arguments": [],
    }) + "\n")
    f.flush()
    s.settimeout(15)
    print(f.readline().strip(), flush=True)
