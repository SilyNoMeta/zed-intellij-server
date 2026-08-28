#!/usr/bin/env python3
"""Compare le backend épinglé avec la dernière publication Open VSX et,
avec --write, régénère extension/server-bundles.json.

Usage : python3 scripts/check-backend-update.py [--write]
Codes de sortie : 0 = à jour, 1 = erreur, 2 = mise à jour disponible.
"""
import json
import subprocess
import sys
import tempfile
import urllib.request
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
BUNDLES = ROOT / "extension" / "server-bundles.json"
API = "https://open-vsx.org/api/JetBrains/intellij-server/latest"

PLATFORMS = {
    "darwin-aarch64": "darwin-arm64",
    "darwin-x86_64": "darwin-x64",
    "linux-aarch64": "linux-arm64",
    "linux-x86_64": "linux-x64",
    "windows-aarch64": "win32-arm64",
    "windows-x86_64": "win32-x64",
}


def main():
    pinned = json.loads(BUNDLES.read_text())["version"]
    print(f"pinned backend : {pinned}")
    with urllib.request.urlopen(API, timeout=30) as r:
        meta = json.load(r)

    platforms = {}
    latest = None
    with tempfile.TemporaryDirectory() as tmp:
        for key, target in PLATFORMS.items():
            url = meta["downloads"][target]
            vsix = Path(tmp) / f"{target}.vsix"
            urllib.request.urlretrieve(url, vsix)
            with zipfile.ZipFile(vsix) as z:
                bundle = json.loads(z.read("extension/server-bundle.json"))
            if latest is None:
                latest = bundle["version"]
            elif bundle["version"] != latest:
                sys.exit(f"MISMATCH: {key} a {bundle['version']} vs {latest}")
            platforms[key] = {
                "url": bundle["url"],
                "archiveName": bundle["archiveName"],
                "sha256": bundle["sha256"],
            }

    print(f"latest backend : {latest}")
    if latest == pinned:
        print("up to date")
        sys.exit(0)
    print(f"UPDATE AVAILABLE: {pinned} -> {latest}")
    if "--write" in sys.argv:
        BUNDLES.write_text(
            json.dumps({"version": latest, "platforms": platforms}, indent=2) + "\n"
        )
        print(f"{BUNDLES} régénéré ; lancer le smoke test avant release :")
        print("  python3 m1-spike/spike.py --server-dir <dir> --transcript /tmp/t.jsonl")
    else:
        print("relancer avec --write pour régénérer server-bundles.json")
    sys.exit(2)


if __name__ == "__main__":
    main()
