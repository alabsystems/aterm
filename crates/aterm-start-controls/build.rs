// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What the `all`, `cli` and `binds` controls link: the linker flags `verify perf cli-start`
//! passes in `ATERM_START_CONTROL_ALL`, `ATERM_START_CONTROL_CLI` and `ATERM_START_CONTROL_BINDS`
//! (whitespace-separated, one per dylib, from the aterm it measures), each handed to its program
//! alone, and the symbols the `binds` control takes the address of (`ATERM_START_CONTROL_IMPORTS`,
//! the Mach-O names of the aterm's imports, whitespace-separated). A build without them links
//! `all` and `cli` as `start-control-none` is linked, and `binds` so too but with chained fixups
//! on Apple silicon (below), importing nothing beyond what every Rust program imports.

use std::fmt::Write as _;

fn main() {
    for (bin, var) in [
        ("start-control-all", "ATERM_START_CONTROL_ALL"),
        ("start-control-cli", "ATERM_START_CONTROL_CLI"),
        ("start-control-binds", "ATERM_START_CONTROL_BINDS"),
    ] {
        println!("cargo::rerun-if-env-changed={var}");
        if let Some(flags) = std::env::var_os(var) {
            for flag in flags.to_string_lossy().split_whitespace() {
                println!("cargo::rustc-link-arg-bin={bin}={flag}");
            }
        }
    }
    // Its fixups in aterm's format: chained (aterm's build script links it so on Apple silicon).
    // This dyld binds every import at load in either format, the older opcodes' lazy binds
    // included (`docs/measured/cli-start-own-2026-11-04.md`), so the format is matched for what
    // binding costs per symbol, not for which symbols are bound.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos")
        && std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("aarch64")
    {
        println!("cargo::rustc-link-arg-bin=start-control-binds=-Wl,-fixup_chains");
    }
    let out = std::path::PathBuf::from(std::env::var_os("OUT_DIR").expect("cargo sets OUT_DIR"));
    std::fs::write(out.join("imports.rs"), imports()).expect("OUT_DIR is writable");
    println!("cargo::rerun-if-changed=build.rs");
}

/// The `binds` control's source for `ATERM_START_CONTROL_IMPORTS`: one extern static per name
/// (its Mach-O name less the leading `_` the target adds back) and a table of their addresses,
/// each one a bind. A name without that `_` cannot be spelled this way and is left out.
fn imports() -> String {
    const IMPORTS: &str = "ATERM_START_CONTROL_IMPORTS";
    println!("cargo::rerun-if-env-changed={IMPORTS}");
    let names = std::env::var(IMPORTS).unwrap_or_default();
    let names: Vec<&str> = names
        .split_whitespace()
        .filter_map(|n| n.strip_prefix('_'))
        .collect();
    let mut text = String::with_capacity(192 + names.len() * 88);
    text.push_str("unsafe extern \"C\" {\n");
    for (i, name) in names.iter().enumerate() {
        let _ = writeln!(text, "    #[link_name = {name:?}]\n    static I{i}: u8;");
    }
    let _ = write!(
        text,
        "}}\n\n/// The imports' addresses.\nstruct Refs([*const u8; {}]);\n\n\
         // SAFETY: the table is never written and nothing reads through its pointers.\n\
         unsafe impl Sync for Refs {{}}\n\n/// Every import, each one bound at load.\n#[used]\n\
         static IMPORTS: Refs = Refs([",
        names.len()
    );
    for i in 0..names.len() {
        let _ = write!(text, "&raw const I{i}, ");
    }
    text.push_str("]);\n");
    text
}
