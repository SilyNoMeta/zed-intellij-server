//! `ij-zed-proxy --dap` — DAP adapter mode for Zed.
//!
//! Zed spawns this binary as the debug adapter for `intellij-debugger` and
//! speaks DAP on stdio. We locate the running LSP proxy instance via its
//! session file in `--system-path`, ask it (control channel) to run
//! `start_debug_server` on the backend, connect to the returned TCP port,
//! enrich `launch` requests with the four `intellij.java.resolve*` commands
//! (like the VS Code client does), then relay DAP frames raw in both
//! directions.

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, BufWriter, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::control::{SessionInfo, SESSION_FILE_NAME};
use crate::frame;

const SESSION_WAIT: Duration = Duration::from_secs(30);
const CONTROL_TIMEOUT: Duration = Duration::from_secs(60);

pub struct DapArgs {
    pub system_path: PathBuf,
    pub root_uri: String,
}

pub fn run(args: DapArgs) -> i32 {
    match run_inner(args) {
        Ok(code) => code,
        Err(e) => {
            report_start_error(&e);
            eprintln!("ij-zed-proxy --dap: {e}");
            1
        }
    }
}

fn run_inner(args: DapArgs) -> Result<i32, String> {
    let mut control = connect_control(&args.system_path)?;

    // 1. Start the backend DAP server; returns a TCP port.
    let port = control
        .execute_command("start_debug_server", json!([args.root_uri]))?
        .as_u64()
        .ok_or("start_debug_server did not return a port")?;
    let port = u16::try_from(port).map_err(|_| format!("invalid DAP port {port}"))?;

    // 2. Connect to the backend DAP server.
    let tcp = TcpStream::connect(("127.0.0.1", port))
        .map_err(|e| format!("cannot connect to backend DAP on port {port}: {e}"))?;
    let tcp_read = tcp.try_clone().map_err(|e| e.to_string())?;

    // 3. Relay: client stdio → backend TCP (with launch enrichment) in one
    //    thread, backend TCP → client stdout (raw) in this one.
    let writer = std::thread::spawn(move || client_to_backend(tcp, control));
    backend_to_client(tcp_read);
    let _ = writer.join();
    Ok(0)
}

/// Waits for the LSP proxy session file, then connects to the control
/// channel (retrying while the instance may still be starting).
fn connect_control(system_path: &Path) -> Result<Control, String> {
    let file = system_path.join(SESSION_FILE_NAME);
    let deadline = Instant::now() + SESSION_WAIT;
    loop {
        if let Ok(text) = std::fs::read_to_string(&file) {
            if let Ok(session) = serde_json::from_str::<SessionInfo>(&text) {
                if let Ok(control) = Control::connect(&session) {
                    return Ok(control);
                }
            }
        }
        if Instant::now() > deadline {
            return Err(format!(
                "no running intellij-server instance for {} (is the language server up?)",
                system_path.display()
            ));
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

/// Newline-delimited JSON control channel to the LSP proxy instance.
pub struct Control {
    stream: BufReader<TcpStream>,
    token: String,
    next_id: u64,
}

impl Control {
    pub fn connect(session: &SessionInfo) -> Result<Self, String> {
        let stream = TcpStream::connect(("127.0.0.1", session.port))
            .map_err(|e| format!("control channel connect failed: {e}"))?;
        stream
            .set_read_timeout(Some(CONTROL_TIMEOUT))
            .map_err(|e| e.to_string())?;
        Ok(Self {
            stream: BufReader::new(stream),
            token: session.token.clone(),
            next_id: 1,
        })
    }

    pub fn execute_command(&mut self, command: &str, arguments: Value) -> Result<Value, String> {
        let id = self.next_id;
        self.next_id += 1;
        let request = json!({
            "id": id,
            "token": self.token,
            "command": command,
            "arguments": arguments,
        });
        writeln!(self.stream.get_mut(), "{request}")
            .and_then(|_| self.stream.get_mut().flush())
            .map_err(|e| e.to_string())?;
        let mut line = String::new();
        self.stream.read_line(&mut line).map_err(|e| e.to_string())?;
        let response: Value = serde_json::from_str(&line).map_err(|e| e.to_string())?;
        if let Some(error) = response.get("error") {
            return Err(format!("{command}: {error}"));
        }
        Ok(response.get("result").cloned().unwrap_or(Value::Null))
    }
}

fn client_to_backend(tcp: TcpStream, mut control: Control) {
    let stdin = std::io::stdin();
    let mut reader = frame::FrameReader::new(BufReader::new(stdin));
    let mut writer = BufWriter::new(tcp.try_clone().expect("tcp clone"));
    while let Ok(Some(body)) = reader.read_frame() {
        let body = match serde_json::from_slice::<Value>(&body) {
            Ok(mut msg)
                if msg.get("type").and_then(Value::as_str) == Some("request")
                    && msg.get("command").and_then(Value::as_str) == Some("initialize") =>
            {
                // The backend only recognizes its own debug type
                // (package.json: "intellij_debugger"), whatever the client sent.
                msg["arguments"]["adapterID"] = json!("intellij_debugger");
                msg.to_string().into_bytes()
            }
            Ok(mut msg)
                if msg.get("type").and_then(Value::as_str) == Some("request")
                    && msg.get("command").and_then(Value::as_str) == Some("launch") =>
            {
                enrich_launch(&mut control, &mut msg);
                msg.to_string().into_bytes()
            }
            _ => body,
        };
        if frame::write_frame(&mut writer, &body).is_err() {
            break;
        }
    }
    let _ = tcp.shutdown(std::net::Shutdown::Both);
}

fn backend_to_client(mut tcp: TcpStream) {
    let stdout = std::io::stdout();
    let mut writer = BufWriter::new(stdout);
    let mut buf = [0u8; 65536];
    loop {
        match tcp.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                if writer
                    .write_all(&buf[..n])
                    .and_then(|_| writer.flush())
                    .is_err()
                {
                    break;
                }
            }
        }
    }
}

/// The four `intellij.java.resolve*` round-trips of the VS Code client
/// (dap.ts resolveLaunchConfig), applied only to fields the user left unset.
fn enrich_launch(control: &mut Control, msg: &mut Value) {
    let Some(args) = msg.get_mut("arguments") else {
        return;
    };
    let Some(main_class) = args
        .get("mainClass")
        .and_then(Value::as_str)
        .map(str::to_owned)
    else {
        return;
    };

    // Document URI: from the explicit file, else resolved from the class name.
    let uri = if let Some(file) = args.get("file").and_then(Value::as_str) {
        Some(format!("file://{file}"))
    } else {
        control
            .execute_command("intellij.java.resolveClassDocument", json!([{"fqn": main_class}]))
            .ok()
            .and_then(|r| r.get("uri").and_then(Value::as_str).map(str::to_owned))
    };
    let Some(uri) = uri else {
        return;
    };

    let is_empty_array = |v: Option<&Value>| v.and_then(Value::as_array).map_or(true, Vec::is_empty);
    if let Ok(classpath) =
        control.execute_command("intellij.java.resolveClasspath", json!([{"uri": uri}]))
    {
        if is_empty_array(args.get("classPaths")) {
            if let Some(cp) = classpath.get("classpath") {
                args["classPaths"] = cp.clone();
            }
        }
        if is_empty_array(args.get("modulePaths")) {
            if let Some(mp) = classpath.get("modulePath") {
                args["modulePaths"] = mp.clone();
            }
        }
        if args.get("moduleName").is_none() {
            if let Some(mn) = classpath.get("moduleName") {
                args["moduleName"] = mn.clone();
            }
        }
    }
    if args.get("cwd").is_none() {
        if let Ok(wd) =
            control.execute_command("intellij.java.resolveWorkingDirectory", json!([{"uri": uri}]))
        {
            if let Some(dir) = wd.get("workingDirectory") {
                args["cwd"] = dir.clone();
            }
        }
    }
    if args.get("javaExec").is_none() {
        if let Ok(je) =
            control.execute_command("intellij.java.resolveJavaExecutable", json!([{"uri": uri}]))
        {
            if let Some(exec) = je.get("javaExec") {
                args["javaExec"] = exec.clone();
            }
        }
    }
}

fn report_start_error(message: &str) {
    // Best-effort DAP event so Zed surfaces something readable.
    let msg = json!({"seq": 1, "type": "event", "event": "output",
        "body": {"category": "stderr", "output": format!("ij-zed-proxy --dap: {message}\n")}});
    let _ = frame::write_frame(&mut std::io::stdout(), msg.to_string().as_bytes());
}
