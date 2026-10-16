#!/bin/sh
# Copyright 2026 Andrew Yates
# SPDX-License-Identifier: Apache-2.0
#
# The WB.1 oracle: crates/aterm-windows-sys-052 against the registry
# windows-sys 0.52.0 on both Windows triples aterm builds.
#
# STOCK EXCEPTION: rustup's `stable` toolchain with the x86_64- and
# aarch64-pc-windows-msvc std, which Trust does not ship. Nothing is linked or
# run: `cargo check` decides the const-asserts and `cargo build` of an rlib
# gives the objects the symbol phase reads.
#
# Per triple:
#   1. conformance: the facade's types, sizes, alignments, constant and fn
#      signature equal the registry crate's (conformance/src/lib.rs);
#   2. import: a call through each side references the same symbol, and the
#      declaring library resolves it from the same DLL (the facade's rlib
#      bundles its raw-dylib import members; upstream ships
#      windows.0.52.0.lib);
#   3. NEGATIVE CONTROLS: every `plant-*` copy must fail 1 or 2.
#
# Usage: ./run.sh            (from anywhere; exits non-zero on any failure)
# Env:   RUSTUP (default: rustup on PATH, else ~/.cargo/bin/rustup)
#        NM (default: llvm-nm, else nm; must read COFF archives)
#        ORACLE_TARGET_DIR (default: $TMPDIR/aterm-windows-sys-052-oracle)
set -eu

here=$(cd "$(dirname "$0")" && pwd)
rustup=${RUSTUP:-$(command -v rustup || echo "$HOME/.cargo/bin/rustup")}
nm=${NM:-$(command -v llvm-nm || command -v nm)}
target_dir=${ORACLE_TARGET_DIR:-${TMPDIR:-/tmp}/aterm-windows-sys-052-oracle}
pkg=windows-sys-052-conformance
fails=0

cargo() {
    (cd "$here" && "$rustup" run stable cargo "$@" --locked --offline --target-dir "$target_dir")
}

fail() {
    echo "  FAIL $*"
    fails=$((fails + 1))
}

# The first rlib cargo reports for library target $2 in JSON messages $1.
rlib_of() {
    grep "\"name\":\"$2\"" "$1" | grep -o '"[^"]*\.rlib"' | head -n 1 | tr -d '"'
}

# The undefined symbol naming IsProcessorFeaturePresent in rlib $1.
caller_symbol() {
    caller_output=$("$nm" -u "$1" 2>/dev/null) || return 1
    printf '%s\n' "$caller_output" | awk '$1 == "U" && $2 ~ /IsProcessorFeaturePresent/ {print $2}' |
        sort -u | tr '\n' ' '
}

# The archive members (DLL names) of library $1 that define symbol $2.
dll_of() {
    library_output=$("$nm" "$1" 2>/dev/null) || return 1
    printf '%s\n' "$library_output" |
        awk -v sym="$2" '/:$/ {m = substr($0, 1, length($0) - 1)} $2 == "T" && $3 == sym {print m}' |
        sort -u | tr '\n' ' '
}

# Build the probe for `features` on `triple`; print "symbol|dll".
import_of() {
    triple=$1
    features=$2
    side=$3
    json="$target_dir/import-$triple-$(echo "$features" | tr ',' '_').json"
    if ! cargo build -q -p "$pkg" --target "$triple" --features "$features" \
        --message-format=json >"$json"; then
        echo "BUILD-FAILED|"
        return
    fi
    probe=$(rlib_of "$json" conformance)
    sym=$(caller_symbol "$probe" | awk '{print $1}')
    case $side in
    upstream)
        dir=$(grep '"reason":"build-script-executed"' "$json" | grep -o '"native=[^"]*windows_[a-z0-9_]*msvc[^"]*"' |
            head -n 1 | sed 's/^"native=//; s/"$//')
        library="$dir/windows.0.52.0.lib"
        ;;
    facade) library=$(rlib_of "$json" facade) ;;
    planted) library=$probe ;;
    esac
    echo "$sym|$(dll_of "$library" "$sym")"
}

for triple in x86_64-pc-windows-msvc aarch64-pc-windows-msvc; do
    echo "== $triple"

    # 1. conformance
    if cargo check -q -p "$pkg" --target "$triple"; then
        echo "  ok   conformance: types, sizes, alignments, constant, fn signature"
    else
        fail "conformance: the facade differs from windows-sys 0.52.0 (compiler output above)"
    fi

    # 2. the import
    want=$(import_of "$triple" probe-upstream upstream)
    got=$(import_of "$triple" probe-facade facade)
    case $want in
    "|"* | *"|" | BUILD-FAILED*) fail "import: upstream's probe gave no symbol or DLL ($want)" ;;
    *)
        if [ "$got" = "$want" ]; then
            echo "  ok   import: $got"
        else
            fail "import: facade $got, upstream $want"
        fi
        ;;
    esac

    # 3. negative controls
    for plant in type size const abi arg ret; do
        if cargo check -q -p "$pkg" --target "$triple" --features "plant-$plant" 2>/dev/null; then
            fail "plant-$plant compiled: a check passes vacuously"
        else
            echo "  ok   plant-$plant caught by conformance"
        fi
    done
    for plant in link-name dll; do
        planted=$(import_of "$triple" "probe-facade,plant-$plant" planted)
        case $planted in
        "|"* | *"|" | BUILD-FAILED*)
            fail "plant-$plant could not build or inspect its probe ($planted)"
            ;;
        *)
            case $want in
            "|"* | *"|" | BUILD-FAILED*) fail "plant-$plant has no upstream import to compare" ;;
            *)
                if [ "$planted" = "$want" ]; then
                    fail "plant-$plant gave upstream's import ($planted): the import check passes vacuously"
                else
                    echo "  ok   plant-$plant caught by import ($planted)"
                fi
                ;;
            esac
            ;;
        esac
    done
done

if [ "$fails" -ne 0 ]; then
    echo "windows-sys 0.52 oracle: FAILED ($fails)"
    exit 1
fi
echo "windows-sys 0.52 oracle: PASS"
