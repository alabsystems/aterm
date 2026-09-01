// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The gate's decision, free of process state so it can be unit-tested. Compiled
//! into `build.rs` (via `#[path]`) and, under `cfg(test)` only, into the library.

/// The one-line installer from README.md: installs aterm AND the Trust
/// toolchain, and links rustup's `trust` toolchain at the managed store.
pub const INSTALL_URL: &str =
    "https://raw.githubusercontent.com/alabsystems/aterm/HEAD/tools/install.sh";

/// `true` when this `rustc -vV` output is a Trust compiler's.
///
/// THE MARKER is the `trust: <version>` key in `-vV`'s key/value block —
/// measured 2026-08-29 on the store build acb08e761 (`trust: 0.1.0`). Upstream
/// rustc (Homebrew 1.96.0, rustup stable, any nightly) prints `rustc`, `binary`,
/// `commit-hash`, `commit-date`, `host`, `release`, `LLVM version` and no
/// `trust:` line. Weaker markers rejected on purpose: the release string
/// (`1.99.0-dev` is what every upstream nightly says too), the commit hash
/// (changes with every store build), and `--print cfg`'s `trust_verify`
/// (disappears under the very `-Ztrust-verify=off` this workspace applies).
/// Anchored at line start so a commit message or a `mistrust:` key cannot pass.
pub fn is_trust_compiler(verbose_version: &str) -> bool {
    verbose_version
        .lines()
        .any(|line| line.starts_with("trust:"))
}

/// Does this TREE pin the Trust toolchain? The gate enforces the pin the tree
/// actually carries, because there are two sanctioned trees: the dev workspace
/// (rust-toolchain.toml pins `channel = "trust"` — the 2026-08-30 standing
/// directive, enforce) and the PUBLIC SNAPSHOT, whose export deliberately
/// swaps that file for the stock pin (publish/transforms.sh copies
/// public-rust-toolchain.toml over it; publish/DECISIONS.md's public
/// stock-Rust gate then builds it with upstream 1.97.1 under anonymous git).
/// Enforcing the trust marker in a tree whose own committed pin says stock
/// made the public snapshot unbuildable by its own gate. Fail-closed: a
/// missing or unreadable pin file, or one with no channel line, ENFORCES —
/// only an explicit non-trust channel stands the gate down, and changing that
/// is a committed-file edit, never an environment variable.
pub fn tree_pins_trust(rust_toolchain_toml: Option<&str>) -> bool {
    let Some(text) = rust_toolchain_toml else {
        return true; // no pin to read: enforce
    };
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        let Some(rest) = line.strip_prefix("channel") else {
            continue;
        };
        let Some(value) = rest.trim_start().strip_prefix('=') else {
            continue;
        };
        let value = value.trim().trim_matches('"');
        return value == "trust";
    }
    true // a pin file with no channel line: enforce
}

/// The refusal to print (then exit 1), or `None` when the build may proceed.
///
/// `host`/`target` are cargo's `HOST`/`TARGET`; the gate applies ONLY when they
/// are equal. The one deliberate upstream lane this repo has — the release's
/// x86_64-apple-darwin compat slice (`crates/aterm-release/src/buildplan.rs`,
/// `--target` + `RUSTUP_TOOLCHAIN=stable`) — is a CROSS build from the aarch64
/// host the release is cut on, as is the Windows cfg-validation build
/// (`cargo +stable build --target x86_64-pc-windows-gnu`); a native build on an
/// upstream compiler is never a lane, so it is exactly the case to refuse.
/// `verbose_version` is `rustc -vV`'s stdout, or a parenthesised note when it
/// could not run — which is refused too: a compiler we could not see is not a
/// Trust compiler we saw. `aterm_on_path` picks the state-aware remedy.
pub fn refusal(
    host: &str,
    target: &str,
    rustc: &str,
    verbose_version: &str,
    aterm_on_path: bool,
    rust_toolchain_toml: Option<&str>,
) -> Option<String> {
    if host != target || is_trust_compiler(verbose_version) || !tree_pins_trust(rust_toolchain_toml)
    {
        return None;
    }
    let reported = verbose_version.lines().next().unwrap_or("(nothing)").trim();
    let fix = if aterm_on_path {
        "fix:  aterm pkg doctor\n      \
         (aterm is on PATH — doctor audits and re-links the toolchain; then build\n      \
         through rustup's cargo, ~/.cargo/bin/cargo, which honours the pin)"
            .to_string()
    } else {
        format!(
            "fix:  install aterm — it installs the Trust toolchain and links it for rustup:\n      \
             curl -fsSL {INSTALL_URL} | bash"
        )
    };
    Some(format!(
        "error: aterm compiles ONLY with the Trust toolchain, and this build is not using it.\n\
         \n  \
         compiler  {rustc}\n  \
         reports   {reported}\n  \
         missing   the `trust: <version>` line a Trust rustc prints in `rustc -vV`\n\
         \n  \
         rust-toolchain.toml pins `channel = \"trust\"`: a rustup toolchain atpkg maintains as a\n  \
         symlink, ~/.rustup/toolchains/trust -> <prefix>/store/trust/current. This build reached\n  \
         an upstream compiler instead — a `RUSTUP_TOOLCHAIN` / `cargo +stable` override, a cargo\n  \
         that is not rustup's (Homebrew's ignores the pin), or a missing/dangling `trust` link.\n\
         \n  \
         {fix}\n"
    ))
}
