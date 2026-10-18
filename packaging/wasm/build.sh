#!/usr/bin/env bash
# Build the lean headless PdfCraft WebAssembly library (crates/wasm) into an npm package directory:
# cargo (wasm-release profile) → wasm-bindgen (--target web) → wasm-opt -Oz (when installed).
#
#   packaging/wasm/build.sh [OUT_DIR]          # default: target/wasm-pkg
#
# Environment: VERSION (package version; default: the workspace version). Needs wasm-bindgen-cli
# at the version in Cargo.lock.
set -euo pipefail

root="$(cd "$(dirname "$0")/../.." && pwd)"
out="${1:-$root/target/wasm-pkg}"
target_dir="${CARGO_TARGET_DIR:-$root/target}"

want="$(grep -A1 '^name = "wasm-bindgen"$' "$root/Cargo.lock" | sed -n 's/^version = "\(.*\)"/\1/p')"
have="$(wasm-bindgen --version | awk '{print $2}')"
if [ "$want" != "$have" ]; then
  echo "wasm-bindgen $have found, Cargo.lock wants $want: cargo install wasm-bindgen-cli --locked --version $want" >&2
  exit 1
fi

args=(build --profile wasm-release --target wasm32-unknown-unknown -p pdfcraft-wasm)
(cd "$root" && cargo "${args[@]}")

rm -rf "$out"
mkdir -p "$out"
wasm-bindgen --target web --out-name pdfcraft_wasm --out-dir "$out" \
  "$target_dir/wasm32-unknown-unknown/wasm-release/pdfcraft_wasm.wasm"

bg="$out/pdfcraft_wasm_bg.wasm"
if command -v wasm-opt >/dev/null 2>&1; then
  wasm-opt -Oz --enable-bulk-memory --enable-nontrapping-float-to-int --enable-sign-ext \
    --enable-mutable-globals --enable-reference-types --enable-multivalue "$bg" -o "$bg.opt"
  mv -f "$bg.opt" "$bg"
else
  echo "wasm-opt not found (npm i -g binaryen): the module is not size-optimised" >&2
fi

version="${VERSION:-$(sed -n 's/^version = "\(.*\)"/\1/p' "$root/Cargo.toml" | head -n 1)}"
sed "s/\"0\.0\.0-template\"/\"$version\"/" "$root/packaging/wasm/package.json" >"$out/package.json"
cp "$root/packaging/wasm/README.md" "$root/LICENSE-MIT" "$root/LICENSE-APACHE" "$root/NOTICE" "$out/"

raw="$(wc -c <"$bg")"
gz="$(gzip -9 -c "$bg" | wc -c)"
br="n/a"
if command -v brotli >/dev/null 2>&1; then
  br="$(brotli -c -q 11 "$bg" | wc -c)"
fi
echo "pdfcraft_wasm_bg.wasm: $raw bytes raw, $gz gzip -9, $br brotli -q 11 → $out"
