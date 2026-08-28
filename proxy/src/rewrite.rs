//! Materialization of `jar:`/`jrt:` targets into real decompiled files, and
//! rewriting of LSP locations to `file://` URIs.

use serde_json::Value;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

/// Deterministic cache path for a decompiled document.
///
/// `<root>/<internal-dirs>/<name>-<hash8>.<ext>` where the internal dirs come
/// from the part after `!/` (e.g. `modules/java.base/java/util`) and `hash8`
/// disambiguates identical paths from different archives.
pub fn cache_path(root: &Path, uri: &str, language: &str) -> PathBuf {
    let without_scheme = uri
        .strip_prefix("jrt://")
        .or_else(|| uri.strip_prefix("jar://"))
        .unwrap_or(uri);
    let internal = without_scheme
        .split("!/")
        .nth(1)
        .unwrap_or(without_scheme);
    let internal = percent_decode(internal);
    let mut hasher = DefaultHasher::new();
    uri.hash(&mut hasher);
    let hash = hasher.finish() as u32;

    let mut dirs: Vec<&str> = internal.split('/').filter(|s| !s.is_empty()).collect();
    let file = dirs.pop().unwrap_or("decompiled");
    let stem = file.strip_suffix(".class").unwrap_or(file);
    let ext = match language {
        "kotlin" => "kt",
        _ => "java",
    };

    let mut path = root.to_path_buf();
    for dir in dirs {
        path.push(sanitize(dir));
    }
    path.push(format!("{}-{:08x}.{}", sanitize(stem), hash, ext));
    path
}

fn sanitize(component: &str) -> String {
    component
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "._-".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect()
}

pub fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(v) = u8::from_str_radix(&input[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Rewrites every `jar:`/`jrt:` location in a JSON-LSP payload via `materialize`,
/// which maps an archive URI to a `file://` URI (or `None` to leave untouched).
/// Returns `true` if anything was rewritten.
pub fn rewrite_locations<F>(value: &mut Value, materialize: &mut F) -> bool
where
    F: FnMut(&str) -> Option<String>,
{
    let mut rewritten = false;
    rewrite_inner(value, materialize, &mut rewritten);
    rewritten
}

fn rewrite_inner<F>(value: &mut Value, materialize: &mut F, rewritten: &mut bool)
where
    F: FnMut(&str) -> Option<String>,
{
    match value {
        Value::Object(map) => {
            // Location: {"uri": ..., "range": ...}
            // LocationLink: {"targetUri": ..., ...}
            for key in ["uri", "targetUri"] {
                if let Some(Value::String(uri)) = map.get(key) {
                    if uri.starts_with("jar:") || uri.starts_with("jrt:") {
                        if let Some(file_uri) = materialize(uri) {
                            map.insert(key.to_string(), Value::String(file_uri));
                            *rewritten = true;
                        }
                    }
                }
            }
            for v in map.values_mut() {
                rewrite_inner(v, materialize, rewritten);
            }
        }
        Value::Array(items) => {
            for v in items {
                rewrite_inner(v, materialize, rewritten);
            }
        }
        _ => {}
    }
}

/// Rewrites `command:jetbrains.navigateToLocation?<args>` links in hover
/// markdown: the command is client-registered in VS Code and dead in Zed, so
/// point the link at the materialized file instead.
pub fn rewrite_command_links(markdown: &str, materialize: &mut impl FnMut(&str) -> Option<String>) -> String {
    const PREFIX: &str = "command:jetbrains.navigateToLocation?";
    let mut out = String::with_capacity(markdown.len());
    let mut rest = markdown;
    while let Some(pos) = rest.find(PREFIX) {
        out.push_str(&rest[..pos]);
        let after = &rest[pos + PREFIX.len()..];
        // The encoded argument array ends at the first character that cannot
        // be part of a URI-encoded JSON array.
        let end = after
            .find(|c: char| !(c.is_ascii_alphanumeric() || "%-._~".contains(c)))
            .unwrap_or(after.len());
        let encoded = &after[..end];
        let decoded = percent_decode(encoded);
        let args: Option<Vec<Value>> = serde_json::from_str(&decoded).ok();
        let rewritten = args.as_ref().and_then(|args| {
            let uri = args.first()?.as_str()?;
            if !uri.starts_with("jar:") && !uri.starts_with("jrt:") {
                return None;
            }
            let line = args.get(1).and_then(Value::as_u64).unwrap_or(0) + 1;
            let file_uri = materialize(uri)?;
            Some(format!("{file_uri}#L{line}"))
        });
        match rewritten {
            Some(link) => out.push_str(&link),
            None => {
                out.push_str(PREFIX);
                out.push_str(encoded);
            }
        }
        rest = &after[end..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fake_materialize(uri: &str) -> Option<String> {
        Some(format!(
            "file:///cache/{}",
            uri.rsplit('/').next().unwrap().replace(".class", ".java")
        ))
    }

    #[test]
    fn cache_path_layout() {
        let p = cache_path(
            Path::new("/cache"),
            "jrt:///opt/jbr/Contents/Home!/modules/java.base/java/util/List.class",
            "java",
        );
        let s = p.to_string_lossy().to_string();
        assert!(s.starts_with("/cache/modules/java.base/java/util/"), "{s}");
        assert!(s.contains("List-"), "{s}");
        assert!(s.ends_with(".java"), "{s}");
    }

    #[test]
    fn rewrites_location_and_location_link() {
        let mut def = json!({"uri": "jrt:///x!/modules/java.base/List.class", "range": {}});
        assert!(rewrite_locations(&mut def, &mut fake_materialize));
        assert_eq!(def["uri"], "file:///cache/List.java");

        let mut link = json!([{"targetUri": "jar:///x.jar!/com/Bar.class", "targetRange": {}}]);
        assert!(rewrite_locations(&mut link, &mut fake_materialize));
        assert_eq!(link[0]["targetUri"], "file:///cache/Bar.java");

        let mut plain = json!({"uri": "file:///src/Main.java"});
        assert!(!rewrite_locations(&mut plain, &mut |_| None));
    }

    #[test]
    fn rewrites_inlay_hint_label_parts() {
        let mut hint = json!({"position": {}, "label": [{"value": "List", "location": {"uri": "jrt:///x!/List.class", "range": {}}}]});
        assert!(rewrite_locations(&mut hint, &mut fake_materialize));
        assert_eq!(hint["label"][0]["location"]["uri"], "file:///cache/List.java");
    }

    #[test]
    fn rewrites_hover_command_links() {
        let args = serde_json::json!(["jrt:///x!/List.class", 41, 7]).to_string();
        let encoded: String = args
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || "-._~".contains(c) {
                    c.to_string()
                } else {
                    format!("%{:02X}", c as u32)
                }
            })
            .collect();
        let md = format!("[Go to declaration](command:jetbrains.navigateToLocation?{encoded})");
        let out = rewrite_command_links(&md, &mut fake_materialize);
        assert_eq!(
            out,
            "[Go to declaration](file:///cache/List.java#L42)"
        );
    }
}
