#!/usr/bin/env python3
"""Mini registre d'extensions Zed, pour pointer un Zed compilé maison dessus.

Sert l'API /extensions* attendue par Zed (crates/extension_host) à partir du
package produit par le CLI officiel `zed-extension` (manifest.json +
archive.tar.gz), et de zéro dépendance (stdlib).

Usage :
  python3 scripts/registry-server.py --packaged out/ --port 8788

Côté Zed (compilé) :
  ZED_SERVER_URL=http://localhost:8788 ./zed/target/release/zed

Attention : ZED_SERVER_URL redirige TOUTES les API Zed (collab, auth, AI)
vers ce serveur — à utiliser pour un build de test, pas au quotidien.
"""
import argparse
import io
import json
import tarfile
import tomllib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

PROVIDES = ["languages", "grammars", "language-servers", "debug-adapters"]


def extension_id_from_archive(archive_path: Path) -> str:
    with tarfile.open(archive_path, "r:gz") as tar:
        for member in tar.getmembers():
            if member.name.lstrip("./") == "extension.toml":
                data = tomllib.loads(tar.extractfile(member).read().decode())
                return data["id"]
    raise ValueError("extension.toml introuvable dans l'archive")


def load_meta(packaged: Path) -> dict:
    manifest = json.loads((packaged / "manifest.json").read_text())
    return {
        "id": extension_id_from_archive(packaged / "archive.tar.gz"),
        "name": manifest["name"],
        "version": manifest["version"],
        "description": manifest.get("description"),
        "authors": manifest.get("authors", []),
        "repository": manifest.get("repository", ""),
        "schema_version": manifest.get("schema_version", 1),
        "wasm_api_version": manifest.get("wasm_api_version", "0.7.0"),
        "provides": manifest.get("provides", PROVIDES),
        "published_at": "2026-01-01T00:00:00Z",
        "download_count": 0,
    }


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--packaged", type=Path, required=True,
                    help="dossier produit par zed-extension (manifest.json + archive.tar.gz)")
    ap.add_argument("--port", type=int, default=8788)
    args = ap.parse_args()

    packaged = args.packaged.resolve()
    archive = packaged / "archive.tar.gz"
    manifest_json = packaged / "manifest.json"
    if not manifest_json.exists():
        ap.error(f"{manifest_json} introuvable")
    meta = load_meta(packaged)
    payload = json.dumps({"data": [meta]}).encode()
    archive_bytes = archive.read_bytes() if archive.exists() else None
    if archive_bytes is None:
        ap.error(f"{archive} introuvable")

    class Handler(BaseHTTPRequestHandler):
        def _send(self, body, content_type="application/json"):
            self.send_response(200)
            self.send_header("Content-Type", content_type)
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def do_GET(self):
            path = self.path.split("?", 1)[0]
            if path == "/extensions" or path.startswith("/extensions/updates"):
                self._send(payload)
            elif path.rstrip("/") == "/extensions/" + meta["id"]:
                self._send(payload)
            elif path.startswith(f"/extensions/{meta['id']}/") and path.endswith("/download"):
                self._send(archive_bytes, "application/gzip")
            elif path == f"/extensions/{meta['id']}/download":
                self._send(archive_bytes, "application/gzip")
            else:
                self.send_error(404, path)

        def log_message(self, fmt, *fmt_args):
            print(f"[registry] {fmt % fmt_args}", flush=True)

    server = ThreadingHTTPServer(("127.0.0.1", args.port), Handler)
    print(f"registre local sur http://127.0.0.1:{args.port} — extension: {meta['id']} {meta['version']}")
    print(f"lancer Zed avec : ZED_SERVER_URL=http://localhost:{args.port} <zed>")
    server.serve_forever()


if __name__ == "__main__":
    main()
