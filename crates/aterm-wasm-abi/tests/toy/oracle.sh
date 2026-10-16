#!/bin/sh
# Copyright 2026 Andrew Yates
# SPDX-License-Identifier: Apache-2.0
#
# The WP.6 oracle: drive.mjs's expectations against the code this layer
# replaces. It writes a #[wasm_bindgen] twin of the toy (the toy's source,
# with `#[export]` spelled `#[wasm_bindgen]` and its host calls bound through
# wasm-bindgen imports), builds it, binds it with the pinned wasm-bindgen CLI
# in both flavors and runs drive.mjs on that glue. Every check must hold but
# the two drive.mjs marks STRICTER (`another class`, and the misuse throws),
# and `another class` must fail in both flavors, which shows the run was
# wasm-bindgen's and not vacuous.
#
# Then the .d.ts: every declaration in the toy's golden (each function,
# method, static and constructor, parameter names and types included) must
# be one in wasm-bindgen's, and the other way round; class members' order
# and the raw `InitOutput` rows (the ABI's own) are not compared. A copy of
# the golden with one parameter renamed must fail the comparison.
#
# STOCK EXCEPTION: rustup's `stable` toolchain with the wasm32-unknown-unknown
# std (Trust has none), and wasm-bindgen 0.2.108 from the local registry
# cache, offline. Nothing it builds is in the workspace. Run by hand.
#
# Usage: ./oracle.sh         (from anywhere; exits non-zero on any failure)
# Env:   RUSTUP (default: rustup on PATH, else ~/.cargo/bin/rustup)
#        WASM_BINDGEN (default: target/tooling/wasm-bindgen-0.2.108/bin/
#        wasm-bindgen, which `verify perf`'s wasm lane installs)
#        ORACLE_DIR (default: $TMPDIR/aterm-wasm-abi-oracle)
set -eu

here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../../../.." && pwd)
rustup=${RUSTUP:-$(command -v rustup || echo "$HOME/.cargo/bin/rustup")}
bindgen=${WASM_BINDGEN:-$root/target/tooling/wasm-bindgen-0.2.108/bin/wasm-bindgen}
dir=${ORACLE_DIR:-${TMPDIR:-/tmp}/aterm-wasm-abi-oracle}
stem=aterm_wasm_abi_toy

"$bindgen" --version | grep -qx 'wasm-bindgen 0.2.108' || {
    echo "oracle: $bindgen is not wasm-bindgen 0.2.108"
    exit 2
}

rm -rf "$dir/twin" "$dir/glue"
mkdir -p "$dir/twin/src"
src=$here/src/lib.rs
for needle in 'use aterm_wasm_abi::{export, host, panic_hook};' \
    'aterm_wasm_abi::export_runtime!();' '#[export]' '#[export('; do
    grep -qF "$needle" "$src" || {
        echo "oracle: the toy no longer has \`$needle\`, which the twin rewrites"
        exit 2
    }
done
perl -0pe '
    s/use aterm_wasm_abi::\{export, host, panic_hook\};/use aterm_wasm_abi::panic_hook;\nuse wasm_bindgen::prelude::*;/;
    s/aterm_wasm_abi::export_runtime!\(\);/mod host {\n    use wasm_bindgen::prelude::*;\n\n    #[wasm_bindgen]\n    extern "C" {\n        #[wasm_bindgen(js_namespace = console, js_name = error)]\n        fn console_error(s: &str);\n        type Error;\n        #[wasm_bindgen(constructor)]\n        fn new() -> Error;\n        #[wasm_bindgen(structural, method, getter)]\n        fn stack(error: &Error) -> String;\n        #[wasm_bindgen(js_namespace = Date, js_name = now)]\n        pub fn date_now() -> f64;\n        #[wasm_bindgen(js_namespace = performance, js_name = now)]\n        pub fn performance_now() -> f64;\n    }\n\n    pub const CONSOLE: aterm_wasm_abi::panic_hook::Console = aterm_wasm_abi::panic_hook::Console {\n        error: |s| console_error(s),\n        stack: || Error::new().stack(),\n    };\n}/;
    s/#\[export\(/#[wasm_bindgen(/g;
    s/#\[export\]/#[wasm_bindgen]/g;
' "$src" >"$dir/twin/src/lib.rs"

cat >"$dir/twin/Cargo.toml" <<EOF
[package]
name = "aterm-wasm-abi-toy"
version = "0.0.0"
edition = "2024"
publish = false

[lib]
crate-type = ["cdylib"]

[dependencies]
wasm-bindgen = "=0.2.108"
aterm-wasm-abi = { path = "$root/crates/aterm-wasm-abi" }

[workspace]

[profile.release]
opt-level = "s"
lto = true
codegen-units = 1
panic = "abort"
strip = "symbols"
EOF

echo "oracle: building the wasm-bindgen twin (wasm32, release, stable)"
(cd "$dir/twin" && env -u RUSTFLAGS -u CARGO_ENCODED_RUSTFLAGS \
    "$rustup" run stable cargo build --release --offline --quiet \
    --target wasm32-unknown-unknown --target-dir "$dir/target")
module=$dir/target/wasm32-unknown-unknown/release/$stem.wasm
"$bindgen" --target nodejs --out-dir "$dir/glue/nodejs" "$module"
"$bindgen" --target web --out-dir "$dir/glue/web" "$module"

out=$(node --expose-gc "$here/drive.mjs" "$dir/glue" 2>&1) || true
fails=$(printf '%s\n' "$out" | grep 'wasm-glue: FAIL ' || true)
unexpected=$(printf '%s\n' "$fails" | grep -v -e '^wasm-glue: FAIL [a-z]* classes another class:' \
    -e '^wasm-glue: FAIL [a-z]* misuse throws ' | grep . || true)
status=0
if ! printf '%s\n' "$out" | grep -q '^wasm-glue: [0-9]* checks FAILED$'; then
    echo "oracle: drive.mjs did not end in the STRICTER checks' failures on wasm-bindgen's glue:"
    printf '%s\n' "$out" | tail -n 20
    status=1
fi
if [ -n "$unexpected" ]; then
    echo "oracle: checks wasm-bindgen's glue fails beyond the STRICTER ones:"
    printf '%s\n' "$unexpected"
    status=1
fi
for flavor in nodejs web; do
    printf '%s\n' "$fails" | grep -q "^wasm-glue: FAIL $flavor classes another class:" || {
        echo "oracle: \`another class\` held on $flavor: not wasm-bindgen's glue, or a vacuous run"
        status=1
    }
done
if [ "$status" -eq 0 ]; then
    echo "oracle: wasm-bindgen 0.2.108 meets every drive.mjs check but the STRICTER ones"
fi

# A .d.ts's declarations with a parameter list, one per line, sorted. Keep
# the producer and sort separate: a failed read must not look like an empty
# declaration set just because sort accepted the failed producer's output.
decls() {
    if ! sed -n 's/^ *//; /^[*\/]/d; /^readonly /d; /(.*;$/p' "$1" >"$2.unsorted"; then
        echo "oracle: could not read declarations from $1" >&2
        rm -f "$2.unsorted"
        return 1
    fi
    if ! LC_ALL=C sort "$2.unsorted" >"$2"; then
        echo "oracle: could not sort declarations from $1" >&2
        rm -f "$2.unsorted" "$2"
        return 1
    fi
    rm -f "$2.unsorted"
}
for flavor in nodejs web; do
    golden=$here/golden/$flavor/$stem.d.ts
    if ! decls "$dir/glue/$flavor/$stem.d.ts" "$dir/bindgen-$flavor.decls" ||
       ! decls "$golden" "$dir/golden-$flavor.decls"; then
        status=1
        continue
    fi
    if ! diff "$dir/bindgen-$flavor.decls" "$dir/golden-$flavor.decls" >"$dir/decls-$flavor.diff"; then
        echo "oracle: the $flavor golden .d.ts declares otherwise than wasm-bindgen (< wasm-bindgen, > golden):"
        cat "$dir/decls-$flavor.diff"
        status=1
    fi
    perl -0pe 's/\(v: /(planted: /' "$golden" >"$dir/planted-$flavor.d.ts"
    if cmp -s "$golden" "$dir/planted-$flavor.d.ts"; then
        echo "oracle: NEGATIVE CONTROL: no \`(v: \` in the $flavor golden .d.ts to plant in"
        status=1
        continue
    else
        cmp_status=$?
        if [ "$cmp_status" -ne 1 ]; then
            echo "oracle: NEGATIVE CONTROL: could not inspect the $flavor planted file (cmp exit $cmp_status)" >&2
            status=1
            continue
        fi
    fi
    if ! decls "$dir/planted-$flavor.d.ts" "$dir/planted-$flavor.decls"; then
        status=1
    elif diff -q "$dir/bindgen-$flavor.decls" "$dir/planted-$flavor.decls" >/dev/null; then
        echo "oracle: NEGATIVE CONTROL: a $flavor .d.ts with a parameter renamed matched wasm-bindgen's"
        status=1
    else
        diff_status=$?
        if [ "$diff_status" -ne 1 ]; then
            echo "oracle: NEGATIVE CONTROL: could not compare the $flavor planted declarations (diff exit $diff_status)" >&2
            status=1
        fi
    fi
done
if [ "$status" -eq 0 ]; then
    echo "oracle: the golden .d.ts declarations are wasm-bindgen 0.2.108's, parameter names included"
fi
exit "$status"
