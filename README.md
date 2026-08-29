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
Changing `initialization_options` takes effect on the next server start
(restart the language server after editing).

Other keys the backend understands, passed through the same way:

```jsonc
// lsp.intellij-server.settings — inlay hints (server-side rendering)
{
  "jetbrains.kotlin.hints.parameters": true,
  "jetbrains.java.hints.types.local variable": true
}

// lsp.intellij-server.initialization_options — per-folder Bazel settings
{
  "bazelSettings": {
    "file:///abs/path/to/workspace": { "projectview": ".bazelproject", "build": true }
  }
}
```

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

## Project sync (the IntelliJ "Load Changes" button)

In IntelliJ IDEA, editing a build file shows a button to re-sync the project.
Here you rarely need it: **saving a build descriptor in the editor re-imports
the project automatically** (`intellij/reloadWorkspace` through the proxy).
Watched files: `pom.xml`, `build.gradle(.kts)`, `settings.gradle(.kts)`,
`gradle.properties`, `gradle-wrapper.properties`, `BUILD(.bazel)`,
`MODULE.bazel`, `WORKSPACE(.bazel)`, `.bazelproject`, `*.bzl`.

For the cases auto-sync misses (edits made outside the editor: git checkout,
CLI, scripts), trigger a reload manually — the proxy exposes it on its control
channel, and `tests/reload_workspace.py` wraps it (it locates the right server
instance from the project root):

```sh
python3 tests/reload_workspace.py /path/to/your/project
```

Bind it to a key for a true "sync button", in `.zed/tasks.json`:

```json
[
  {
    "label": "IntelliJ: reload workspace",
    "command": "python3 /path/to/zed-intellij-server/tests/reload_workspace.py \"$ZED_WORKTREE_ROOT\"",
    "hide": "never"
  }
]
```

and in `keymap.json`:

```json
[
  {
    "context": "Workspace",
    "bindings": {
      "ctrl-alt-r": ["task::Spawn", { "task_name": "IntelliJ: reload workspace" }]
    }
  }
]
```

Restarting the language server (Zed's built-in action) also works, but is much
slower: reload keeps the JVM and the index, restart rebuilds neither but pays
the full startup.

To debug a JVM started elsewhere (tests, a server, a Gradle run), start it with
`-agentlib:jdwp=transport=dt_socket,server=y,suspend=n,address=*:5005` and use the
attach configuration above.

## Control commands (maintenance & inspection)

While a project server runs, the proxy exposes a localhost control channel
(token-gated, one instance per project). The scripts in `tests/` wrap it;
each resolves the right instance from the project root:

| Script | What it does |
|---|---|
| `reload_workspace.py <root>` | Re-import the project model (`intellij/reloadWorkspace`). |
| `clear_caches.py <system-path>` | Wipe the backend index and restart (see Troubleshooting). |
| `license_state.py <root>` | Print the licensing state (source, validity, JBA, license server). |
| `export_workspace.py <root>` | Write the IntelliJ project model to `<root>/workspace.json`. |
| `print_classpath.py <root> <fqn-or-file>` | Print the exact resolved classpath (handy for terminal compiles). |

The channel itself speaks newline-delimited JSON on `127.0.0.1` (session file:
`…/system-path/<hash>/ij-zed-proxy.session.json`). Commands: any
`workspace/executeCommand` (e.g. `decompile`, `exportWorkspace`,
`intellij.java.resolve*`), `__lsp` (raw LSP passthrough, e.g.
`jetbrains/licensing/state/get`), `__reload_workspace`,
`__clear_caches_and_restart`. `tests/control_client.py` is a tiny shared
client if you want to script your own.

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

## Troubleshooting

**"Failed to install dev extension: failed to compile Rust extension".** Zed's
builder runs `cargo` with the PATH it inherits. If a Homebrew-installed Rust
(`brew install rust`) shadows rustup's in your PATH, the build fails with
`can't find crate for core` — Homebrew Rust has no `wasm32-wasip2` target.
Fix: `brew uninstall rust` (or put `export PATH="$HOME/.cargo/bin:$PATH"` in
your `~/.zshrc`), then fully quit and relaunch Zed so it picks up the new
environment, and retry.

**Clear caches and restart.** When the index gets corrupted (stale or missing
symbols, odd diagnostics after a crash), ask the running proxy to wipe the
backend index and restart everything:

```sh
python3 tests/clear_caches.py \
  "$HOME/Library/Application Support/Zed/extensions/work/intellij-server/system-path/<hash>"
```

There is one `<hash>` directory per project; each holds the proxy session file.
The backend shuts down, the proxy deletes the index and exits so Zed restarts
everything with a fresh index (re-indexing takes a while on large projects).
Manual alternative: stop the language server, delete that `<hash>` directory,
restart the server.

## Known limitations

- The backend preview **expires 30 days** after its build date (exit code 7): update
  the extension when this happens (see Licensing above).
- No run/debug CodeLens (Zed has no custom LSP command support yet); use `debug.json`.
- External build-file edits (git checkout, CLI) are not auto-detected; saving a build
  file in the editor re-imports the project. Restart the language server otherwise.
- The license-activation UI of the official extension does not exist in Zed;
  see Licensing above for the headless channels.
- File templates are not applied when creating new files (Zed has no
  file-creation hook for extensions), and the workspace-structure JSON export
  is unavailable (no access to the command palette from extensions).
- Debug scenario generation from main classes (DAP locators) is not wired yet;
  write `debug.json` entries by hand.

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
