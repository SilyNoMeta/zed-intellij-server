#!/usr/bin/env bash
# Packages the Zed extension into dist/extension/ (as installed by Zed, minus
# the tree-sitter grammars which Zed compiles at install time).
set -euo pipefail
cd "$(dirname "$0")/../extension"

OUT=../dist/extension
rm -rf "$OUT"
mkdir -p "$OUT/languages/java" "$OUT/languages/kotlin" "$OUT/debug_adapter_schemas"

cargo build --release --target wasm32-wasip2
cp extension.toml server-bundles.json proxy-bundles.json "$OUT/"
cp target/wasm32-wasip2/release/zed_intellij.wasm "$OUT/extension.wasm"
cp languages/java/* "$OUT/languages/java/"
cp languages/kotlin/* "$OUT/languages/kotlin/"
cp debug_adapter_schemas/intellij-debugger.json "$OUT/debug_adapter_schemas/"
[ -f ../LICENSE ] && cp ../LICENSE "$OUT/" || true

echo "packed:"
find "$OUT" -type f | sort
