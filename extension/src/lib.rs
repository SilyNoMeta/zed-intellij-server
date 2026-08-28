mod config;
mod eula;
mod server;

use config::LaunchConfig;
use zed_extension_api::{
    self as zed,
    serde_json::{json, Value},
    settings::LspSettings,
    DebugAdapterBinary, DebugTaskDefinition, Extension, LanguageServerId,
    StartDebuggingRequestArguments, StartDebuggingRequestArgumentsRequest, Worktree,
};

const SERVER_ID: &str = "intellij-server";
const DEBUG_ADAPTER_NAME: &str = "intellij-debugger";

struct IntelliJServer;

impl Extension for IntelliJServer {
    fn new() -> Self {
        Self
    }

    fn language_server_command(
        &mut self,
        language_server_id: &LanguageServerId,
        worktree: &Worktree,
    ) -> zed::Result<zed::Command> {
        if language_server_id.as_ref() != SERVER_ID {
            return Err(format!("Unknown language server: {language_server_id}"));
        }
        let lsp_settings = LspSettings::for_worktree(SERVER_ID, worktree)?;
        let launch = LaunchConfig::from_settings(lsp_settings.settings.as_ref());

        let binary_path = lsp_settings
            .binary
            .as_ref()
            .and_then(|b| b.path.as_deref());
        let server_dir = server::resolve_server_dir(
            binary_path,
            &worktree.shell_env(),
            language_server_id,
        )?;

        let eula_path = server_dir.join("EULA.txt");
        if !launch.accept_eula {
            return Err(eula::acceptance_error(&eula_path));
        }
        let system_path = server::system_path_for(&worktree.root_path())?;

        // With a proxy, the server runs behind `ij-zed-proxy`: reloadWorkspace
        // on build-file save, decompiled jar/jrt sources as real files, and
        // the `intellij/*` protocol extensions. Without one: direct mode.
        let proxy = server::resolve_proxy_path(launch.proxy_path.as_deref(), Some(language_server_id))?;

        // The WASM sandbox cannot read files outside the extension working
        // dir (e.g. a user-provided `binary.path`). The proxy is native: when
        // the EULA is unreadable from here, it computes and appends `--eula`
        // itself (the acceptance above still gates the launch).
        let eula_hash = match eula::eula_hash(&eula_path) {
            Ok(hash) => Some(hash),
            Err(_e) if proxy.is_some() => None,
            Err(e) => {
                return Err(format!(
                    "{e}\nThe EULA is not readable from the extension sandbox. \
                     Configure lsp.intellij-server.settings.proxy_path to ij-zed-proxy, \
                     which computes the acceptance hash natively."
                ))
            }
        };

        let mut args = vec![
            "--stdio".to_string(),
            "--system-path".to_string(),
            system_path.to_string_lossy().into_owned(),
        ];
        if let Some(hash) = eula_hash {
            args.push("--eula".to_string());
            args.push(hash);
        }
        if let Some(extra) = lsp_settings.binary.as_ref().and_then(|b| b.arguments.clone()) {
            args.extend(extra);
        }

        let mut env = launch.launch_env();
        if let Some(user_env) = lsp_settings.binary.as_ref().and_then(|b| b.env.clone()) {
            env.extend(user_env);
        }

        let launcher = server_dir
            .join("bin")
            .join(server::launcher_name())
            .to_string_lossy()
            .into_owned();

        if let Some(proxy) = proxy {
            let proxy_log = server::working_dir()?.join("proxy.log");
            let mut proxy_args = vec![
                "--log".to_string(),
                proxy_log.to_string_lossy().into_owned(),
                "--".to_string(),
                launcher,
            ];
            proxy_args.extend(args);
            return Ok(zed::Command {
                command: proxy.to_string_lossy().into_owned(),
                args: proxy_args,
                env,
            });
        }

        Ok(zed::Command {
            command: launcher,
            args,
            env,
        })
    }

    fn language_server_initialization_options(
        &mut self,
        language_server_id: &LanguageServerId,
        worktree: &Worktree,
    ) -> zed::Result<Option<Value>> {
        if language_server_id.as_ref() != SERVER_ID {
            return Ok(None);
        }
        let lsp_settings = LspSettings::for_worktree(SERVER_ID, worktree)?;
        // Defaults mirror the official client; user options win on conflicts.
        // `intellijExtensions` and `runMainCodeLens` stay off: Zed ignores the
        // custom notifications they would trigger.
        let mut options = json!({
            "projects": [],
            "disableRocksDBWriteAheadLog": false,
        });
        if let Some(Value::Object(user)) = lsp_settings.initialization_options {
            if let Value::Object(ref mut base) = options {
                base.extend(user);
            }
        }
        Ok(Some(options))
    }

    fn language_server_workspace_configuration(
        &mut self,
        language_server_id: &LanguageServerId,
        worktree: &Worktree,
    ) -> zed::Result<Option<Value>> {
        if language_server_id.as_ref() != SERVER_ID {
            return Ok(None);
        }
        Ok(LspSettings::for_worktree(SERVER_ID, worktree)?.settings)
    }

    fn get_dap_binary(
        &mut self,
        adapter_name: String,
        config: DebugTaskDefinition,
        _user_provided_debug_adapter_path: Option<String>,
        worktree: &Worktree,
    ) -> Result<DebugAdapterBinary, String> {
        if adapter_name != DEBUG_ADAPTER_NAME {
            return Err(format!("Unknown debug adapter: {adapter_name}"));
        }
        let lsp_settings = LspSettings::for_worktree(SERVER_ID, worktree)?;
        let launch = LaunchConfig::from_settings(lsp_settings.settings.as_ref());
        let proxy = server::resolve_proxy_path(launch.proxy_path.as_deref(), None)?
            .ok_or_else(|| {
                "Debugging requires ij-zed-proxy (not available for this platform yet); \
                 set lsp.intellij-server.settings.proxy_path to a local build"
                    .to_string()
            })?;
        let system_path = server::system_path_for(&worktree.root_path())?;
        let root_uri = file_uri(&worktree.root_path());
        let request = self.dap_request_kind(
            adapter_name,
            zed::serde_json::from_str(&config.config)
                .map_err(|e| format!("Invalid debug configuration JSON: {e}"))?,
        )?;
        Ok(DebugAdapterBinary {
            command: Some(proxy.to_string_lossy().into_owned()),
            arguments: vec![
                "--dap".to_string(),
                "--system-path".to_string(),
                system_path.to_string_lossy().into_owned(),
                "--root-uri".to_string(),
                root_uri,
            ],
            cwd: Some(worktree.root_path()),
            envs: vec![],
            request_args: StartDebuggingRequestArguments {
                request,
                configuration: config.config,
            },
            connection: None,
        })
    }

    fn dap_request_kind(
        &mut self,
        adapter_name: String,
        config: Value,
    ) -> Result<StartDebuggingRequestArgumentsRequest, String> {
        if adapter_name != DEBUG_ADAPTER_NAME {
            return Err(format!("Unknown debug adapter: {adapter_name}"));
        }
        match config.get("request") {
            Some(Value::String(s)) if s == "launch" => {
                Ok(StartDebuggingRequestArgumentsRequest::Launch)
            }
            Some(Value::String(s)) if s == "attach" => {
                Ok(StartDebuggingRequestArgumentsRequest::Attach)
            }
            Some(other) => Err(format!(
                "Unexpected `request` value in debug configuration: {other}"
            )),
            None => Err("Missing `request` in debug configuration".to_string()),
        }
    }
}

/// `file://` URI for an absolute host path (minimal percent-encoding).
fn file_uri(path: &str) -> String {
    let mut uri = String::from("file://");
    for segment in path.split('/') {
        if segment.is_empty() {
            continue;
        }
        uri.push('/');
        for byte in segment.bytes() {
            if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
                uri.push(byte as char);
            } else {
                uri.push_str(&format!("%{byte:02X}"));
            }
        }
    }
    uri
}

zed::register_extension!(IntelliJServer);
