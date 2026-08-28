# Java & Kotlin (IntelliJ engine) — for Zed

IntelliJ-powered code engine for **Java** and **Kotlin** in [Zed](https://zed.dev/),
driven by the JetBrains **`intellij-server`** backend (the IntelliJ IDEA engine running
headless, the same backend as the official VS Code extension).
Not affiliated with or endorsed by JetBrains.

Diagnostics come from real IntelliJ inspections, completion/navigation/refactorings from
the IntelliJ project model (Gradle, Maven, Bazel), and debugging from the IntelliJ
debugger — all over standard LSP/DAP.

## How it works

```
Zed ──stdio──> this extension (WASM) ──spawns──> ij-zed-proxy ──stdio──> intellij-server
                    │                                │                     (JVM, headless)
                    │  resolves binary, env, EULA    │  jar:/jrt: → real decompiled files
                    │                                │  build-file save → reloadWorkspace
                    │                                │  intellij/* protocol extensions
                    └── [debug] ij-zed-proxy --dap ──┴──> backend DAP server (TCP)
```

- The backend is **downloaded from `download.jetbrains.com`** at first launch
  (~380 MB, SHA-256 verified, per-platform). It is never redistributed by this extension.
- A JVM runs permanently (1–4 GB depending on the project); initial indexing on large
  monorepos takes a while. Tune with `jvm_args` below.
- The pinned backend is a **preview build that expires 30 days after its build date**
  (exit code 7). Update the extension to get a fresh pinned build.

## Setup

1. **Accept the EULA.** The backend is distributed under the *JetBrains LSP Extension
   Public EAP Agreement*. Read it after the first install at
   `<extension workdir>/servers/<version>/EULA.txt`, then in your Zed settings:

   ```json
   "lsp": {
     "intellij-server": {
       "settings": {
         "accept_jetbrains_eula": true
       }
     }
   }
   ```

   Until then the server refuses to start and the error message points to the file.
   This is the product's documented acceptance mechanism — the extension passes the
   accepted agreement's hash to the server, it does not bypass anything.

2. **Avoid duplicate servers.** If you also use `jdtls` or `kotlin-lsp`:

   ```json
   "languages": {
     "Java": { "language_servers": ["intellij-server", "!jdtls"] },
     "Kotlin": { "language_servers": ["intellij-server", "!kotlin-lsp"] }
   }
   ```

## Settings (`lsp.intellij-server.settings`)

| Key | Default | Description |
|---|---|---|
| `accept_jetbrains_eula` | `false` | Required. Accept the JetBrains EAP agreement. |
| `proxy_path` | auto | Path to an `ij-zed-proxy` binary. Usually unnecessary: the extension downloads the pinned proxy when available for your platform. Without a proxy, the server runs in direct mode (no decompiled sources, no hot build-file reload). |
| `jvm_args` | `[]` | Extra JVM flags for the backend, e.g. `["-Xmx4g"]` (passed via `IJ_JAVA_OPTIONS`). |
| `data_sharing` | unset | `"full"` or `"anonymous"` diagnostics sharing with JetBrains. Unset = nothing sent. |
| `region` | unset | JetBrains data-processing region (`europe`, `americas`, `apac`, `china`, …). |

Project import and SDK selection use `lsp.intellij-server.initialization_options`,
forwarded to the server as-is (`defaultSdk`, `projects`, `buildTools`,
`disableRocksDBWriteAheadLog` — see the official extension's documentation).

## Debugging

Requires the proxy (see `proxy_path`). Example `.zed/debug.json`:

```json
[
  {
    "adapter": "intellij-debugger",
    "label": "Run Main",
    "request": "launch",
    "mainClass": "com.example.Main"
  },
  {
    "adapter": "intellij-debugger",
    "label": "Attach 5005",
    "request": "attach",
    "hostName": "localhost",
    "port": 5005
  }
]
```

Classpath (JPMS included), working directory and `java` executable are resolved from
the IntelliJ project model at launch. **Build your classes before launching** — the
backend does not compile before running (e.g. `gradle classes`, or a Zed task).

## Licensing

**Preview builds need no license.** The pinned backend ships with its own EAP
license (`EAP User`), valid for 30 days from the build's release date. A paid
license does **not** extend a preview build — expiry is baked into the build.
Keep the extension updated instead; the weekly bump workflow tracks new builds.

Check the current license state any time (works without any UI capability):

```sh
python3 tests/licensing_probe.py
```

If you have a JetBrains license and want it used anyway (1.0, or to stay square
with the EAP terms), three channels:

1. **`INTELLIJ_SERVER_LICENSE` environment variable (simplest, global).** The
   backend honors it and it takes precedence over panel activation. Put your
   JetBrains activation code in your Zed settings:

   ```json
   "lsp": {
     "intellij-server": {
       "settings": { "accept_jetbrains_eula": true },
       "binary": {
         "env": { "INTELLIJ_SERVER_LICENSE": "<your activation code>" }
       }
     }
   }
   ```

   The value stays in plaintext in `settings.json`. Restart the language server
   after editing.

2. **Automatic license discovery.** After every `initialize`, the proxy runs
   `jetbrains/licensing/discovery/autoActivate` — the same headless discovery
   the official client runs at startup. On machines with a licensed JetBrains
   IDE or Toolbox installation, the backend can pick up the existing license
   by itself; the outcome is logged in the proxy log (`license discovery: …`).

3. **Activation code over the protocol.** `jetbrains/licensing/activationCode/use`
   is accepted by the backend without the `licensingUi` capability, but the
   activated state persists in the server's config directory — which is
   per-project in this extension (per-worktree `--system-path`), so you would
   need to re-activate for each project. Prefer channel 1.

## Known limitations

- The backend preview **expires 30 days** after its build date (exit code 7): update
  the extension when this happens (see Licensing above).
- No run/debug CodeLens (Zed has no custom LSP command support yet); use `debug.json`.
- External build-file edits (git checkout, CLI) are not auto-detected; saving a build
  file in the editor re-imports the project. Restart the language server otherwise.
- The license-activation UI of the official extension does not exist in Zed;
  see Licensing above for the headless channels.

## Repository layout

- `extension/` — the Zed extension (Rust → WASM): backend acquisition, EULA gate, DAP wiring.
- `proxy/` — the native LSP proxy and DAP adapter (`ij-zed-proxy`).
- `tests/` — protocol-level integration tests (`spike.py`, `dap_test.py`) and a test project.
- `scripts/` — release tooling: backend bump check, proxy cross-build, extension packaging.

## Development

```sh
cd extension && cargo build --release --target wasm32-wasip2   # the extension
cd proxy && cargo build --release                              # the proxy
cd proxy && cargo test                                         # unit tests
```

To try it in Zed, run `scripts/package-extension.sh` and copy `dist/extension/`
into `~/Library/Application Support/Zed/extensions/installed/intellij-server/`
(equivalent paths on Linux/Windows), then restart Zed. Set `proxy_path` to your
local proxy build for the full feature set.

## License

The extension and proxy are Apache-2.0 (see `LICENSE`). The `intellij-server` backend
is proprietary JetBrains software under the JetBrains LSP Extension Public EAP
Agreement; it is downloaded from JetBrains at install time and never bundled.
This project is not affiliated with or endorsed by JetBrains.
