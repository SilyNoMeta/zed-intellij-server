//! `ij-zed-proxy` — JSON-RPC pump between Zed (stdin/stdout) and the
//! JetBrains intellij-server (spawned child process).
//!
//! Passthrough byte-exact for everything except the interception table:
//! - `initialize` (c→s): memorize options, inject `intellijExtensions: true`
//! - navigation responses (s→c): materialize `jar:`/`jrt:` targets via the
//!   server `decompile` command into real files, rewrite to `file://`
//! - `didSave` of build descriptors (c→s): relay, then `intellij/reloadWorkspace`
//! - `intellij/chooseAction` (s→c): → `window/showMessageRequest`, answer via
//!   `chooseModCommandAction`
//! - `intellij/copyToClipboard` (s→c): system clipboard
//! - `intellij/importLog` (s→c): → `window/logMessage` (+ error message on failure)

mod buildfiles;
mod control;
mod dap;
mod frame;
mod rewrite;

use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const PROXY_ID_PREFIX: &str = "ij-proxy:";
const DECOMPILE_TIMEOUT: Duration = Duration::from_secs(60);

/// Navigation methods whose responses may contain `jar:`/`jrt:` locations.
const NAV_METHODS: &[&str] = &[
    "textDocument/definition",
    "textDocument/typeDefinition",
    "textDocument/implementation",
    "textDocument/references",
    "textDocument/documentSymbol",
    "workspace/symbol",
    "inlayHint/resolve",
    "textDocument/hover",
];

struct Shared {
    server_stdin: Mutex<ChildStdin>,
    client_stdout: Mutex<BufWriter<std::io::Stdout>>,
    /// Pending proxy-originated requests to the server.
    pending_server: Mutex<HashMap<String, Sender<Value>>>,
    /// Pending proxy-originated requests to the client (chooseAction).
    pending_client: Mutex<HashMap<String, ChooseActionPending>>,
    /// Client request id → method, for navigation requests in flight.
    nav_requests: Mutex<HashMap<String, ()>>,
    next_proxy_id: AtomicU64,
    init_options: Mutex<Value>,
    cache_root: PathBuf,
    log: Mutex<Box<dyn Write + Send>>,
}

struct ChooseActionPending {
    session_id: u64,
    /// entry index by action title
    indexes: Vec<(String, u64)>,
}

fn logln(shared: &Arc<Shared>, msg: &str) {
    if let Ok(mut log) = shared.log.lock() {
        let _ = writeln!(log, "[{}] {msg}", now_iso());
        let _ = log.flush();
    }
}

fn now_iso() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    format!("{secs}")
}

fn main() {
    let mode = parse_args();
    match mode {
        Mode::Dap(args) => std::process::exit(dap::run(args)),
        Mode::Lsp { log_path, server_cmd } => run_lsp(log_path, server_cmd),
    }
}

fn run_lsp(log_path: Option<String>, server_cmd: Vec<String>) {
    let log: Box<dyn Write + Send> = match &log_path {
        Some(path) => Box::new(
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .expect("cannot open proxy log"),
        ),
        None => Box::new(std::io::stderr()),
    };

    let mut child = spawn_server(&server_cmd).unwrap_or_else(|e| {
        eprintln!("ij-zed-proxy: cannot spawn {}: {e}", server_cmd[0]);
        std::process::exit(1);
    });
    let server_stdin = child.stdin.take().unwrap();
    let server_stdout = child.stdout.take().unwrap();
    let server_stderr = child.stderr.take().unwrap();

    let cache_root = cache_root_for(&server_cmd);
    let shared = Arc::new(Shared {
        server_stdin: Mutex::new(server_stdin),
        client_stdout: Mutex::new(BufWriter::new(std::io::stdout())),
        pending_server: Mutex::new(HashMap::new()),
        pending_client: Mutex::new(HashMap::new()),
        nav_requests: Mutex::new(HashMap::new()),
        next_proxy_id: AtomicU64::new(1),
        init_options: Mutex::new(Value::Null),
        cache_root,
        log: Mutex::new(log),
    });
    logln(&shared, &format!("starting; server: {server_cmd:?}; cache: {}", shared.cache_root.display()));

    // Control channel for the --dap adapter mode.
    {
        let system_path = system_path_of(&server_cmd);
        if let Err(e) = control::start(Arc::clone(&shared), &system_path) {
            logln(&shared, &format!("control channel failed to start: {e}"));
        }
    }

    // Server stderr → proxy log.
    {
        let shared = Arc::clone(&shared);
        std::thread::spawn(move || {
            let mut reader = BufReader::new(server_stderr);
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => logln(&shared, &format!("[server] {}", line.trim_end())),
                }
            }
        });
    }

    // Child exit → relay exit code.
    {
        let shared = Arc::clone(&shared);
        std::thread::spawn(move || {
            let status = child.wait();
            let code = match &status {
                Ok(s) => s.code().unwrap_or(1),
                Err(_) => 1,
            };
            logln(&shared, &format!("server exited with code {code}; proxy exiting"));
            std::process::exit(code);
        });
    }

    // Decompile workers: nav responses needing materialization.
    let (work_tx, work_rx) = channel::<(Value, Vec<u8>)>();
    let work_rx = Arc::new(Mutex::new(work_rx));
    for _ in 0..2 {
        let shared = Arc::clone(&shared);
        let work_rx = Arc::clone(&work_rx);
        std::thread::spawn(move || loop {
            let (id, body) = match work_rx.lock().unwrap().recv() {
                Ok(item) => item,
                Err(_) => break,
            };
            handle_nav_response(&shared, id, body);
        });
    }

    // Server → client pump.
    {
        let shared = Arc::clone(&shared);
        std::thread::spawn(move || {
            let mut reader = frame::FrameReader::new(BufReader::new(server_stdout));
            while let Ok(Some(body)) = reader.read_frame() {
                server_to_client(&shared, &work_tx, body);
            }
            logln(&shared, "server stdout closed");
        });
    }

    // Client → server pump (main thread). EOF → stop everything.
    let mut reader = frame::FrameReader::new(BufReader::new(std::io::stdin()));
    while let Ok(Some(body)) = reader.read_frame() {
        client_to_server(&shared, body);
    }
    logln(&shared, "client stdin closed; exiting");
    std::process::exit(0);
}

enum Mode {
    Lsp {
        log_path: Option<String>,
        server_cmd: Vec<String>,
    },
    Dap(dap::DapArgs),
}

fn parse_args() -> Mode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    if argv.first().map(String::as_str) == Some("--dap") {
        let mut system_path = None;
        let mut root_uri = None;
        let mut i = 1;
        while i < argv.len() {
            match argv[i].as_str() {
                "--system-path" => {
                    i += 1;
                    system_path = argv.get(i).map(PathBuf::from);
                }
                "--root-uri" => {
                    i += 1;
                    root_uri = argv.get(i).cloned();
                }
                _ => {}
            }
            i += 1;
        }
        match (system_path, root_uri) {
            (Some(system_path), Some(root_uri)) => {
                return Mode::Dap(dap::DapArgs {
                    system_path,
                    root_uri,
                })
            }
            _ => {
                eprintln!("usage: ij-zed-proxy --dap --system-path <dir> --root-uri <uri>");
                std::process::exit(2);
            }
        }
    }

    let mut args = argv.into_iter();
    let mut log_path = None;
    let mut server_cmd = Vec::new();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--log" => log_path = args.next(),
            "--" => {
                server_cmd.extend(args);
                break;
            }
            other => {
                server_cmd.push(other.to_string());
                server_cmd.extend(args);
                break;
            }
        }
    }
    if server_cmd.is_empty() {
        eprintln!("usage: ij-zed-proxy [--log <path>] -- <server-launcher> [server args...]");
        std::process::exit(2);
    }
    Mode::Lsp {
        log_path,
        server_cmd,
    }
}

fn spawn_server(cmd: &[String]) -> std::io::Result<Child> {
    let mut command = Command::new(&cmd[0]);
    command
        .args(eula_args(cmd))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // The launcher debug log goes to stdout (the LSP stream) by the mere
        // presence of this variable — the extension cannot unset it, we can.
        .env_remove("IJ_LAUNCHER_DEBUG");
    command.spawn()
}

/// Server args, with `--eula <hash>` appended when the caller could not
/// compute it (e.g. the WASM extension sandbox cannot read a user-provided
/// server path) and the launcher's `EULA.txt` is readable from here.
fn eula_args(cmd: &[String]) -> Vec<String> {
    let mut args: Vec<String> = cmd[1..].to_vec();
    if cmd.iter().any(|a| a == "--eula") {
        return args;
    }
    let eula_path = Path::new(&cmd[0]).parent().map(|bin| bin.join("..").join("EULA.txt"));
    if let Some(path) = eula_path {
        if let Ok(bytes) = std::fs::read(&path) {
            use sha2::{Digest, Sha256};
            let hash: String = Sha256::digest(&bytes)
                .iter()
                .take(8)
                .map(|b| format!("{b:02x}"))
                .collect();
            args.push("--eula".to_string());
            args.push(hash);
        }
    }
    args
}

/// `<system-path>` from the server args (falling back to a temp dir).
fn system_path_of(server_cmd: &[String]) -> PathBuf {
    server_cmd
        .windows(2)
        .find(|w| w[0] == "--system-path")
        .map(|w| PathBuf::from(&w[1]))
        .unwrap_or_else(|| std::env::temp_dir().join("ij-zed-proxy"))
}

/// `<system-path>/decompiled/<server-dir-name>` — invalidated by server version.
fn cache_root_for(server_cmd: &[String]) -> PathBuf {
    let launcher = Path::new(&server_cmd[0]);
    let server_name = launcher
        .parent()
        .and_then(Path::parent)
        .and_then(Path::file_name)
        .and_then(|n| n.to_str())
        .unwrap_or("unknown")
        .to_string();
    system_path_of(server_cmd).join("decompiled").join(server_name)
}

/// Writes a message to the client.
fn to_client(shared: &Arc<Shared>, body: &[u8]) {
    if let Ok(mut out) = shared.client_stdout.lock() {
        let _ = frame::write_frame(&mut *out, body);
    }
}

/// Writes a message to the server.
fn to_server(shared: &Arc<Shared>, body: &[u8]) {
    if let Ok(mut stdin) = shared.server_stdin.lock() {
        let _ = frame::write_frame(&mut *stdin, body);
    }
}

/// Sends a proxy-originated request to the server and waits for the response.
fn call_server(shared: &Arc<Shared>, method: &str, params: Value) -> Result<Value, String> {
    let id = format!(
        "{PROXY_ID_PREFIX}{}",
        shared.next_proxy_id.fetch_add(1, Ordering::SeqCst)
    );
    let (tx, rx) = channel();
    shared
        .pending_server
        .lock()
        .map_err(|e| e.to_string())?
        .insert(id.clone(), tx);
    let request = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
    to_server(shared, request.to_string().as_bytes());
    let response = rx
        .recv_timeout(DECOMPILE_TIMEOUT)
        .map_err(|_| format!("timeout waiting for {method}"))?;
    if let Some(error) = response.get("error") {
        return Err(format!("{method} failed: {error}"));
    }
    Ok(response.get("result").cloned().unwrap_or(Value::Null))
}

// ---------------------------------------------------------------------------
// Client → server
// ---------------------------------------------------------------------------

fn client_to_server(shared: &Arc<Shared>, body: Vec<u8>) {
    // Fast path: plain responses and uninteresting methods go through raw.
    let interesting = contains(&body, PROXY_ID_PREFIX.as_bytes()) || contains(&body, b"\"method\"");
    if !interesting {
        to_server(shared, &body);
        return;
    }
    let Ok(mut msg) = serde_json::from_slice::<Value>(&body) else {
        to_server(shared, &body);
        return;
    };

    // Response to a proxy-originated client request (chooseAction).
    if msg.get("method").is_none() {
        if let Some(id) = msg.get("id").and_then(Value::as_str) {
            if id.starts_with(PROXY_ID_PREFIX) {
                if let Some(pending) = shared
                    .pending_client
                    .lock()
                    .ok()
                    .and_then(|mut p| p.remove(id))
                {
                    handle_choose_action_reply(shared, pending, msg.clone());
                }
                return; // swallowed: the server never sees proxy traffic
            }
        }
        to_server(shared, &body);
        return;
    }

    let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
    match method {
        "initialize" => {
            if let Some(params) = msg.get_mut("params") {
                let options = params
                    .get_mut("initializationOptions")
                    .filter(|o| o.is_object());
                let options = match options {
                    Some(o) => o,
                    None => {
                        params["initializationOptions"] = json!({});
                        params.get_mut("initializationOptions").unwrap()
                    }
                };
                options["intellijExtensions"] = Value::Bool(true);
                *shared.init_options.lock().unwrap() = options.clone();
            }
            to_server(shared, msg.to_string().as_bytes());
        }
        "textDocument/didSave" => {
            to_server(shared, &body);
            let uri = msg["params"]["textDocument"]["uri"].as_str().unwrap_or("");
            if buildfiles::is_build_file_path(uri) {
                trigger_reload_workspace(Arc::clone(shared));
            }
        }
        m if NAV_METHODS.contains(&m) => {
            if let Some(id) = msg.get("id") {
                shared
                    .nav_requests
                    .lock()
                    .unwrap()
                    .insert(id.to_string(), ());
            }
            to_server(shared, &body);
        }
        _ => to_server(shared, &body),
    }
}

fn trigger_reload_workspace(shared: Arc<Shared>) {
    std::thread::spawn(move || {
        let options = shared.init_options.lock().unwrap().clone();
        let result = call_server(
            &shared,
            "intellij/reloadWorkspace",
            json!({"initializationOptions": options}),
        );
        logln(
            &shared,
            &match result {
                Ok(_) => "workspace reloaded after build file save".to_string(),
                Err(e) => format!("reloadWorkspace failed: {e}"),
            },
        );
    });
}

// ---------------------------------------------------------------------------
// Server → client
// ---------------------------------------------------------------------------

fn server_to_client(shared: &Arc<Shared>, work_tx: &Sender<(Value, Vec<u8>)>, body: Vec<u8>) {
    let interesting = body.windows(PROXY_ID_PREFIX.len()).any(|w| w == PROXY_ID_PREFIX.as_bytes())
        || contains(&body, b"intellij/")
        || contains(&body, b"jar:")
        || contains(&body, b"jrt:")
        || contains(&body, b"navigateToLocation");
    if !interesting {
        to_client(shared, &body);
        return;
    }
    let Ok(msg) = serde_json::from_slice::<Value>(&body) else {
        to_client(shared, &body);
        return;
    };

    // Response to a proxy-originated server request.
    if msg.get("method").is_none() {
        if let Some(id) = msg.get("id").and_then(Value::as_str) {
            if id.starts_with(PROXY_ID_PREFIX) {
                if let Some(tx) = shared
                    .pending_server
                    .lock()
                    .ok()
                    .and_then(|mut p| p.remove(id))
                {
                    let _ = tx.send(msg.clone());
                }
                return; // swallowed
            }
        }
        // Response to a tracked navigation request?
        let id_key = msg.get("id").map(Value::to_string).unwrap_or_default();
        let is_nav = shared
            .nav_requests
            .lock()
            .map(|mut m| m.remove(&id_key).is_some())
            .unwrap_or(false);
        if is_nav {
            let _ = work_tx.send((msg.get("id").cloned().unwrap_or(Value::Null), body));
        } else {
            to_client(shared, &body);
        }
        return;
    }

    match msg.get("method").and_then(Value::as_str).unwrap_or("") {
        "intellij/chooseAction" => handle_choose_action(shared, &msg),
        "intellij/copyToClipboard" => {
            if let Some(content) = msg["params"]["content"].as_str() {
                match arboard::Clipboard::new().and_then(|mut c| c.set_text(content)) {
                    Ok(()) => logln(shared, "clipboard set by server"),
                    Err(e) => logln(shared, &format!("clipboard failed: {e}")),
                }
            }
        }
        "intellij/importLog" => handle_import_log(shared, &msg),
        _ => to_client(shared, &body),
    }
}

fn handle_nav_response(shared: &Arc<Shared>, id: Value, body: Vec<u8>) {
    let Ok(mut msg) = serde_json::from_slice::<Value>(&body) else {
        to_client(shared, &body);
        return;
    };
    let mut changed = false;
    if let Some(result) = msg.get_mut("result") {
        changed |= rewrite::rewrite_locations(result, &mut |uri| materialize(shared, uri));
        // Hover markdown: command: links → materialized file links.
        for content in hover_markdown_strings_mut(result) {
            let rewritten = rewrite::rewrite_command_links(content, &mut |uri| materialize(shared, uri));
            if rewritten != *content {
                *content = rewritten;
                changed = true;
            }
        }
    }
    if changed {
        msg["id"] = id;
        to_client(shared, msg.to_string().as_bytes());
    } else {
        to_client(shared, &body);
    }
}

fn hover_markdown_strings_mut(result: &mut Value) -> Vec<&mut String> {
    let mut strings = Vec::new();
    let contents = result.get_mut("contents");
    match contents {
        // MarkupContent: {"kind": "markdown", "value": "..."}
        Some(Value::Object(map)) => {
            if let Some(Value::String(value)) = map.get_mut("value") {
                strings.push(value);
            }
        }
        // MarkedString or MarkedString[]: "..." or {"language","value"}
        Some(Value::String(value)) => strings.push(value),
        Some(Value::Array(items)) => {
            for item in items {
                match item {
                    Value::String(value) => strings.push(value),
                    Value::Object(map) => {
                        if let Some(Value::String(value)) = map.get_mut("value") {
                            strings.push(value);
                        }
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }
    strings
}

/// Decompiles an archive URI into a cached real file; returns its `file://` URI.
fn materialize(shared: &Arc<Shared>, uri: &str) -> Option<String> {
    // Fast path: any existing file for this URI.
    for language in ["java", "kotlin"] {
        let path = rewrite::cache_path(&shared.cache_root, uri, language);
        if path.exists() {
            return Some(file_uri(&path));
        }
    }
    let result = call_server(
        shared,
        "workspace/executeCommand",
        json!({"command": "decompile", "arguments": [uri]}),
    )
    .map_err(|e| logln(shared, &format!("decompile {uri}: {e}")))
    .ok()?;
    let code = result.get("code").and_then(Value::as_str)?;
    let language = result
        .get("language")
        .and_then(Value::as_str)
        .unwrap_or("java");
    let path = rewrite::cache_path(&shared.cache_root, uri, language);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| logln(shared, &format!("mkdir {}: {e}", parent.display())))
            .ok()?;
    }
    std::fs::write(&path, code)
        .map_err(|e| logln(shared, &format!("write {}: {e}", path.display())))
        .ok()?;
    // Read-only marker: Zed has no read-only documents; make accidental edits obvious.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(mut perms) = std::fs::metadata(&path).map(|m| m.permissions()) {
            perms.set_mode(0o444);
            let _ = std::fs::set_permissions(&path, perms);
        }
    }
    logln(shared, &format!("decompiled {uri} → {}", path.display()));
    Some(file_uri(&path))
}

fn file_uri(path: &Path) -> String {
    let mut uri = String::from("file://");
    for component in path.to_string_lossy().split('/') {
        if component.is_empty() {
            continue;
        }
        uri.push('/');
        uri.push_str(&percent_encode_path(component));
    }
    uri
}

fn percent_encode_path(segment: &str) -> String {
    let mut out = String::new();
    for byte in segment.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

fn handle_choose_action(shared: &Arc<Shared>, msg: &Value) {
    let params = &msg["params"];
    let Some(session_id) = params["sessionId"].as_u64() else {
        return;
    };
    let title = params["title"].as_str().unwrap_or("Choose an action");
    let entries = params["entries"].as_array().cloned().unwrap_or_default();
    let mut actions = Vec::new();
    let mut indexes = Vec::new();
    for entry in entries {
        let name = entry["name"].as_str().unwrap_or("?").to_string();
        let index = entry["index"].as_u64().unwrap_or(0);
        actions.push(json!({"title": name}));
        indexes.push((name, index));
    }
    let id = format!(
        "{PROXY_ID_PREFIX}{}",
        shared.next_proxy_id.fetch_add(1, Ordering::SeqCst)
    );
    shared.pending_client.lock().unwrap().insert(
        id.clone(),
        ChooseActionPending {
            session_id,
            indexes,
        },
    );
    let request = json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "window/showMessageRequest",
        "params": {"type": 3, "message": title, "actions": actions},
    });
    to_client(shared, request.to_string().as_bytes());
}

fn handle_choose_action_reply(shared: &Arc<Shared>, pending: ChooseActionPending, response: Value) {
    // MessageActionItem {title} or null (dismissed → no reply, like VS Code).
    let Some(title) = response["result"]["title"].as_str() else {
        return;
    };
    let Some((_, index)) = pending.indexes.iter().find(|(t, _)| t == title) else {
        return;
    };
    let shared = Arc::clone(shared);
    let session_id = pending.session_id;
    let index = *index;
    std::thread::spawn(move || {
        let _ = call_server(
            &shared,
            "workspace/executeCommand",
            json!({"command": "chooseModCommandAction", "arguments": [session_id, index]}),
        );
    });
}

fn handle_import_log(shared: &Arc<Shared>, msg: &Value) {
    let params = &msg["params"];
    let tool = params["tool"].as_str().unwrap_or("Build");
    let message = params["message"].as_str().unwrap_or("");
    let failed = params["failed"].as_bool().unwrap_or(false);
    let started = params["started"].as_bool().unwrap_or(false);
    if !message.is_empty() {
        let text = if started {
            format!("{tool}: {message}")
        } else {
            format!("[{tool}] {message}")
        };
        let log_msg = json!({
            "jsonrpc": "2.0",
            "method": "window/logMessage",
            "params": {"type": 4, "message": text},
        });
        to_client(shared, log_msg.to_string().as_bytes());
    }
    if failed {
        let show_msg = json!({
            "jsonrpc": "2.0",
            "method": "window/showMessage",
            "params": {"type": 2, "message": format!("{tool}: Build Error")},
        });
        to_client(shared, show_msg.to_string().as_bytes());
    }
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}
