"""Client partagé pour le canal de contrôle d'ij-zed-proxy.

Résout la session d'un projet (même calcul que system_path_for() de
l'extension : sha256 de la racine, 16 hex) et envoie des commandes.
"""
import hashlib
import json
import socket
from pathlib import Path

DEFAULT_WORKDIR = Path.home() / "Library/Application Support/Zed/extensions/work/intellij-server"


def session_file_for(root, workdir=DEFAULT_WORKDIR):
    digest = hashlib.sha256(str(root).encode()).hexdigest()[:16]
    return Path(workdir) / "system-path" / digest / "ij-zed-proxy.session.json"


def send(session_file, command, arguments=None, method=None, params=None, timeout=60):
    session = json.load(open(session_file))
    request = {"id": 1, "token": session["token"], "command": command}
    if arguments is not None:
        request["arguments"] = arguments
    if method is not None:
        request["method"] = method
    if params is not None:
        request["params"] = params
    with socket.create_connection(("127.0.0.1", session["port"]), timeout=timeout) as s:
        f = s.makefile("rw")
        f.write(json.dumps(request) + "\n")
        f.flush()
        s.settimeout(timeout)
        response = json.loads(f.readline())
    if "error" in response:
        raise RuntimeError(response["error"])
    return response.get("result")


def send_lsp(session_file, method, params=None, timeout=60):
    return send(session_file, "__lsp", method=method, params=params, timeout=timeout)
