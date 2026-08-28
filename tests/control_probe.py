#!/usr/bin/env python3
"""Sonde les commandes intellij.java.resolve* via le canal de contrôle."""
import json
import socket
import sys
from pathlib import Path

session = json.load(open(Path(sys.argv[1]) / "ij-zed-proxy.session.json"))
with socket.create_connection(("127.0.0.1", session["port"]), timeout=60) as s:
    f = s.makefile("rw")

    def call(i, command, arguments):
        f.write(json.dumps({"id": i, "token": session["token"], "command": command,
                            "arguments": arguments}) + "\n")
        f.flush()
        s.settimeout(60)
        return json.loads(f.readline())

    r = call(1, "intellij.java.resolveClassDocument", [{"fqn": "com.example.Main"}])
    print("resolveClassDocument:", json.dumps(r)[:300], flush=True)
    uri = (r.get("result") or {}).get("uri")
    if uri:
        r2 = call(2, "intellij.java.resolveClasspath", [{"uri": uri}])
        print("resolveClasspath:", json.dumps(r2)[:600], flush=True)
        r3 = call(3, "intellij.java.resolveWorkingDirectory", [{"uri": uri}])
        print("resolveWorkingDirectory:", json.dumps(r3)[:300], flush=True)
        r4 = call(4, "intellij.java.resolveJavaExecutable", [{"uri": uri}])
        print("resolveJavaExecutable:", json.dumps(r4)[:300], flush=True)
