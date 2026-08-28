#!/usr/bin/env python3
"""Affiche l'état de licence du backend sur l'instance en cours.

Usage : python3 tests/license_state.py <racine-du-projet> [workdir-extension]
"""
import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from control_client import session_file_for, send_lsp

root = sys.argv[1]
session = session_file_for(root, *(sys.argv[2:3] or []))
if not session.exists():
    sys.exit(f"pas de session proxy pour {root} ({session} introuvable)")

state = send_lsp(session, "jetbrains/licensing/state/get")
lic = state.get("activeLicense")
if lic:
    print(f"licence   : {lic.get('source')} — {lic.get('status')}"
          f" (produit {lic.get('product')}, {lic.get('licensedTo', '?')})")
    if lic.get("validThrough"):
        print(f"validité  : jusqu'au {lic['validThrough']}"
              + (f" ({lic['daysLeft']} j restants)" if lic.get("daysLeft") is not None else ""))
else:
    print("licence   : aucune")
jba = state.get("jbaLicensingState", {})
print(f"JBA       : {jba.get('authenticationStatus')}")
lsf = state.get("licenseServerFlowState", {})
print(f"srv lic.  : {lsf.get('kind')}")
if state.get("graceDeadline"):
    print(f"grace     : jusqu'à {state['graceDeadline']}")
print(f"features  : {'REFUSÉES (featuresDeclined)' if state.get('featuresDeclined') else 'actives'}")
