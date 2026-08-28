//! EULA handling: the server validates `--eula <hash>` at startup, where the
//! hash is the first 16 hex chars of the SHA-256 of the bundled `EULA.txt`.
//! Passing the flag is the documented acceptance mechanism of the product; the
//! extension only passes it after the user explicitly accepted the agreement
//! (`accept_jetbrains_eula` setting).

use sha2::{Digest, Sha256};
use std::path::Path;

pub fn eula_hash(eula_path: &Path) -> Result<String, String> {
    let bytes = std::fs::read(eula_path)
        .map_err(|e| format!("Cannot read {}: {e}", eula_path.display()))?;
    // The server expects the first 16 hex characters of the SHA-256.
    Ok(hex_prefix(&Sha256::digest(&bytes), 16))
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    hex_prefix(&Sha256::digest(bytes), 64)
}

/// First `len` hexadecimal characters of the digest (`len` must be even).
fn hex_prefix(digest: &[u8], len: usize) -> String {
    digest
        .iter()
        .take(len / 2)
        .map(|b| format!("{b:02x}"))
        .collect()
}

pub fn acceptance_error(eula_path: &Path) -> String {
    format!(
        "The JetBrains intellij-server backend is distributed under the JetBrains LSP \
         Extension Public EAP Agreement, which you must read and accept before the server \
         can start.\n\nRead the agreement at:\n  {}\n\nThen accept it in your Zed settings:\n  \
         \"lsp\": {{ \"intellij-server\": {{ \"settings\": {{ \"accept_jetbrains_eula\": true }} }} }}",
        eula_path.display()
    )
}
