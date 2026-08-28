#!/usr/bin/env python3
"""Exporte la structure du projet (modèle IntelliJ) dans <racine>/workspace.json —
utile pour déboguer l'import Gradle/Maven/Bazel.

Usage : python3 tests/export_workspace.py <racine-du-projet> [workdir-extension]
"""
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from control_client import session_file_for, send

root = sys.argv[1]
session = session_file_for(root, *(sys.argv[2:3] or []))
if not session.exists():
    sys.exit(f"pas de session proxy pour {root} ({session} introuvable)")

# La commande écrit <racine>/workspace.json elle-même et retourne null.
send(session, "exportWorkspace", [root])
out = Path(root) / "workspace.json"
if out.exists():
    print(f"écrit: {out} ({out.stat().st_size} o)")
else:
    sys.exit("la commande a répondu mais workspace.json est introuvable")
