#!/usr/bin/env python3
"""Déclenche intellij/reloadWorkspace sur l'instance proxy du projet —
l'équivalent manuel du bouton « Load Gradle Changes » d'IntelliJ.

Usage : python3 tests/reload_workspace.py <racine-du-projet> [workdir-extension]
"""
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from control_client import session_file_for, send

root = sys.argv[1]
session = session_file_for(root, *(sys.argv[2:3] or []))
if not session.exists():
    sys.exit(f"pas de session proxy pour {root} ({session} introuvable — serveur démarré ?)")

print(send(session, "__reload_workspace", []))
