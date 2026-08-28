#!/usr/bin/env python3
"""Affiche le classpath résolu par le modèle de projet IntelliJ pour une
classe (FQN) ou un fichier — utile pour compiler en terminal avec exactement
le classpath du projet.

Usage : python3 tests/print_classpath.py <racine-du-projet> <fqn-ou-fichier> [workdir-extension]
Exemples :
  python3 tests/print_classpath.py . com.example.Main
  python3 tests/print_classpath.py . src/main/java/com/example/Main.java
"""
import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from control_client import session_file_for, send

root = Path(sys.argv[1]).resolve()
target = sys.argv[2]
session = session_file_for(root, *(sys.argv[3:4] or []))
if not session.exists():
    sys.exit(f"pas de session proxy pour {root} ({session} introuvable)")

if target.endswith(".java") or target.endswith(".kt"):
    uri = "file://" + str((root / target).resolve() if not Path(target).is_absolute() else target)
else:
    doc = send(session, "intellij.java.resolveClassDocument", [{"fqn": target}])
    uri = doc.get("uri")
    if not uri:
        sys.exit(f"classe introuvable dans le modèle: {target}")

cp = send(session, "intellij.java.resolveClasspath", [{"uri": uri}])
for entry in cp.get("classpath", []):
    print(entry)
module_path = cp.get("modulePath") or []
if module_path:
    print("\n-- module path --")
    for entry in module_path:
        print(entry)
if cp.get("moduleName"):
    print(f"\nmodule JPMS : {cp['moduleName']}")
