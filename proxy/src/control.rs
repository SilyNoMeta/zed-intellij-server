//! Control channel: lets sibling processes (the `--dap` adapter mode) drive
//! `workspace/executeCommand` on the backend through the running LSP proxy.
//!
//! The proxy listens on 127.0.0.1 (ephemeral port) and writes a session file
//! `<system-path>/ij-zed-proxy.session.json` with the port, a bearer token
//! and its pid. Requests and responses are newline-delimited JSON.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::sync::Arc;

use crate::{call_server, logln, Shared};

#[derive(Serialize, Deserialize)]
pub struct SessionInfo {
    pub port: u16,
    pub token: String,
    pub pid: u32,
}

pub const SESSION_FILE_NAME: &str = "ij-zed-proxy.session.json";

pub fn start(shared: Arc<Shared>, system_path: &Path) -> std::io::Result<()> {
    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    let port = listener.local_addr()?.port();
    let token = random_token();
    let session = SessionInfo {
        port,
        token: token.clone(),
        pid: std::process::id(),
    };
    let file = system_path.join(SESSION_FILE_NAME);
    std::fs::write(&file, serde_json::to_string(&session)?)?;
    logln(
        &shared,
        &format!("control channel on 127.0.0.1:{port}, session file {}", file.display()),
    );
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            match conn {
                Ok(stream) => {
                    let shared = Arc::clone(&shared);
                    let token = token.clone();
                    std::thread::spawn(move || handle_conn(shared, stream, token));
                }
                Err(_) => break,
            }
        }
    });
    Ok(())
}

fn handle_conn(shared: Arc<Shared>, stream: TcpStream, expected_token: String) {
    let mut reader = BufReader::new(match stream.try_clone() {
        Ok(s) => s,
        Err(_) => return,
    });
    let mut writer = stream;
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {
                let response = handle_request(&shared, &expected_token, &line);
                if writeln!(writer, "{response}").and_then(|_| writer.flush()).is_err() {
                    break;
                }
            }
        }
    }
}

fn handle_request(shared: &Arc<Shared>, expected_token: &str, line: &str) -> Value {
    let Ok(request) = serde_json::from_str::<Value>(line) else {
        return json!({"error": "invalid JSON"});
    };
    let id = request.get("id").cloned().unwrap_or(Value::Null);
    if request.get("token").and_then(Value::as_str) != Some(expected_token) {
        return json!({"id": id, "error": "bad token"});
    }
    let command = request.get("command").and_then(Value::as_str).unwrap_or("");
    let arguments = request.get("arguments").cloned().unwrap_or(Value::Array(vec![]));
    if command == CLEAR_CACHES_COMMAND {
        return handle_clear_caches(shared, id);
    }
    if command == RELOAD_WORKSPACE_COMMAND {
        if shared.init_options.lock().unwrap().is_null() {
            return json!({"id": id, "error": "server not initialized yet"});
        }
        crate::trigger_reload_workspace(Arc::clone(shared), "manual reload");
        return json!({"id": id, "result": "workspace reload requested"});
    }
    if command == LSP_COMMAND {
        // Raw LSP passthrough (e.g. jetbrains/licensing/state/get on the
        // live instance). Localhost + token gated, same as executeCommand.
        let method = request.get("method").and_then(Value::as_str).unwrap_or("");
        if method.is_empty() {
            return json!({"id": id, "error": "missing `method` for __lsp"});
        }
        let params = request.get("params").cloned().unwrap_or(Value::Null);
        return match call_server(shared, method, params) {
            Ok(result) => json!({"id": id, "result": result}),
            Err(error) => json!({"id": id, "error": error}),
        };
    }
    match call_server(
        shared,
        "workspace/executeCommand",
        json!({"command": command, "arguments": arguments}),
    ) {
        Ok(result) => json!({"id": id, "result": result}),
        Err(error) => json!({"id": id, "error": error}),
    }
}

/// `__clear_caches_and_restart`: asks the backend to shut down; once it has
/// exited (so its index files are released), the proxy deletes the index and
/// exits with a crash code so the host restarts everything fresh.
pub const CLEAR_CACHES_COMMAND: &str = "__clear_caches_and_restart";

/// `__reload_workspace`: re-imports the project model without restarting
/// the server — the manual equivalent of IntelliJ's "Load Changes" button.
pub const RELOAD_WORKSPACE_COMMAND: &str = "__reload_workspace";

/// `__lsp`: raw LSP method passthrough to the backend, for maintenance and
/// inspection commands that are not `workspace/executeCommand` (licensing…).
pub const LSP_COMMAND: &str = "__lsp";

fn handle_clear_caches(shared: &Arc<Shared>, id: Value) -> Value {
    if shared.index_dir.lock().unwrap().is_none() {
        return json!({"id": id, "error": "server index directory unknown (not initialized?)"});
    }
    shared
        .cache_clear_requested
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let shared = Arc::clone(shared);
    std::thread::spawn(move || {
        // Best effort: if the shutdown exchange fails, the next server exit
        // still triggers the cache clear (the flag is latched).
        let _ = call_server(&shared, "shutdown", Value::Null);
        crate::to_server(&shared, b"{\"jsonrpc\":\"2.0\",\"method\":\"exit\"}");
    });
    json!({"id": id, "result": "server stopping; index will be deleted and the server restarted"})
}

fn random_token() -> String {
    let mut bytes = [0u8; 16];
    if std::fs::File::open("/dev/urandom")
        .and_then(|mut f| std::io::Read::read_exact(&mut f, &mut bytes))
        .is_err()
    {
        // Fallback: hash of time and pid — localhost token, not a secret.
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let seed = nanos ^ u128::from(std::process::id());
        bytes = seed.to_le_bytes();
    }
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
