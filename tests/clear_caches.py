#!/usr/bin/env python3
"""Demande au proxy en cours de vider les caches du backend et de redémarrer.

Usage : python3 tests/clear_caches.py <system-path>
Le <system-path> est celui du worktree (contient ij-zed-proxy.session.json) ;
sous Zed il ressemble à :
  ~/Library/Application Support/Zed/extensions/work/intellij-server/system-path/<hash>
"""
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from control_client import send

session = Path(sys.argv[1]) / "ij-zed-proxy.session.json"
if not session.exists():
    sys.exit(f"pas de session proxy dans {sys.argv[1]}")

print(send(session, "__clear_caches_and_restart", []))
