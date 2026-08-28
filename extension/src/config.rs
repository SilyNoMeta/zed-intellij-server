//! User-facing settings of the extension, read from
//! `lsp.intellij-server.settings` in the Zed configuration.

use zed_extension_api::serde_json::Value;

const REGIONS: [&str; 7] = [
    "africa",
    "americas",
    "apac",
    "china",
    "europe",
    "middle_east",
    "oceania",
];

/// Launch-time configuration (environment, EULA acceptance).
pub struct LaunchConfig {
    pub accept_eula: bool,
    pub jvm_args: Vec<String>,
    /// `full` or `anonymous`; `none`/unset means the variable is not passed at all.
    pub data_sharing: Option<String>,
    pub region: Option<String>,
    /// Path to an `ij-zed-proxy` binary; when set, the server runs behind the proxy.
    pub proxy_path: Option<String>,
}

impl LaunchConfig {
    pub fn from_settings(settings: Option<&Value>) -> Self {
        let get = |key: &str| settings.and_then(|s| s.get(key));

        let accept_eula = get("accept_jetbrains_eula")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        let jvm_args = get("jvm_args")
            .and_then(Value::as_array)
            .map(|args| {
                args.iter()
                    .filter_map(|a| a.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();

        let data_sharing = get("data_sharing")
            .and_then(Value::as_str)
            .filter(|v| matches!(*v, "full" | "anonymous"))
            .map(str::to_owned);

        let region = get("region")
            .and_then(Value::as_str)
            .filter(|v| REGIONS.contains(v))
            .map(str::to_owned);

        let proxy_path = get("proxy_path")
            .and_then(Value::as_str)
            .filter(|p| !p.trim().is_empty())
            .map(str::to_owned);

        Self {
            accept_eula,
            jvm_args,
            data_sharing,
            region,
            proxy_path,
        }
    }

    /// Environment variables for the server process.
    pub fn launch_env(&self) -> Vec<(String, String)> {
        let mut env = Vec::new();
        // NOTE: `IJ_LAUNCHER_DEBUG` makes the launcher log to stdout (the LSP
        // stream) by its mere presence — even empty. The extension API cannot
        // unset inherited variables, so we cannot scrub it here; the M3 proxy
        // removes it from the server environment when spawning the backend.
        if !self.jvm_args.is_empty() {
            let joined = self
                .jvm_args
                .iter()
                .map(|a| shell_quote_if_needed(a))
                .collect::<Vec<_>>()
                .join(" ");
            env.push(("IJ_JAVA_OPTIONS".to_string(), joined));
        }
        if let Some(data_sharing) = &self.data_sharing {
            env.push(("INTELLIJ_DATA_SHARING".to_string(), data_sharing.clone()));
        }
        if let Some(region) = &self.region {
            env.push(("INTELLIJ_REGION".to_string(), region.clone()));
        }
        env
    }
}

/// Mirrors `shellQuoteIfNeeded` in the VS Code client (lspClient.ts).
fn shell_quote_if_needed(arg: &str) -> String {
    if arg
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "._=:/@-".contains(c))
    {
        return arg.to_string();
    }
    let mut quoted = String::from("\"");
    for c in arg.chars() {
        if matches!(c, '"' | '\\' | '$' | '`') {
            quoted.push('\\');
        }
        quoted.push(c);
    }
    quoted.push('"');
    quoted
}
