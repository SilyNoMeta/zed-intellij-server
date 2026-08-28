# Smoke tests

Protocol-level integration tests that drive the real backend without any
editor, plus a minimal Gradle project to run them against.

## Files

- `spike.py` — generic LSP client (stdlib Python). Launches `intellij-server`
  (optionally through `ij-zed-proxy`), runs initialize → completion → hover →
  definition (intra-project and into the JDK) → rename → diagnostics → shutdown,
  with per-check PASS/FAIL output. Flags: `--server-dir`, `--proxy`, `--dap`,
  `--no-eula`, `--no-system-path`, `--hold`, `--transcript`.
- `dap_test.py` — DAP end-to-end test through `ij-zed-proxy --dap`: initialize,
  breakpoints, launch (with launch-config enrichment), stopped event, program
  output, clean termination. Needs a live LSP proxy instance (see below).
- `control_probe.py` — manual probe for the proxy control channel.
- `licensing_probe.py` — reports the backend licensing state
  (`jetbrains/licensing/state/get`) and runs `discovery/autoActivate`,
  without the `licensingUi` capability. Use it to check whether a license
  (EAP, env var, or discovered) is in effect. See "Licensing" in the README.
- `clear_caches.py` — asks the running proxy to wipe the backend index and
  restart everything. See "Troubleshooting" in the README.
- `reload_workspace.py` — triggers `intellij/reloadWorkspace` on the server
  instance of a given project (the manual "Load Changes" button).
  See "Project sync" in the README.
- `test-project/` — minimal Gradle + Java project used by the tests.

## Running

1. Download the backend for your platform (URL + SHA-256 in
   `extension/server-bundles.json`) and unpack it, e.g. into `tests/server/<version>/`
   (the `.sit` on macOS is a plain tar).

2. LSP smoke test, direct:

   ```sh
   python3 tests/spike.py --server-dir tests/server/<version> --dap
   ```

3. LSP smoke test through the proxy (`cargo build --release` in `proxy/` first):

   ```sh
   python3 tests/spike.py --server-dir tests/server/<version> \
     --proxy proxy/target/release/ij-zed-proxy
   ```

4. DAP end-to-end (needs the test classes compiled and a live proxy session):

   ```sh
   javac --release 25 -d test-project/build/classes/java/main \
     test-project/src/main/java/com/example/*.java   # run from tests/test-project
   python3 tests/spike.py --proxy proxy/target/release/ij-zed-proxy --hold &
   # wait for "READY", then:
   python3 tests/dap_test.py proxy/target/release/ij-zed-proxy \
     "$PWD/tests/storage" "file://$PWD/tests/test-project"
   touch tests/dap-test-done   # releases the held instance
   ```

Use the JBR bundled with the backend (`tests/server/<version>/jbr/`) to compile
the test classes when the version matters.
