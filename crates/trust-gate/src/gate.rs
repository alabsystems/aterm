// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The gate's decision, free of process state so it can be unit-tested. Compiled
//! into `build.rs` (via `#[path]`) and, under `cfg(test)` only, into the library.

/// The one-line installer from README.md: installs aterm and, on Apple-silicon
/// macOS, the Trust toolchain, linking rustup's `trust` toolchain at the store.
pub const INSTALL_URL: &str =
    "https://raw.githubusercontent.com/alabsystems/aterm/HEAD/tools/install.sh";

/// The one host triple atpkg ships the Trust toolchain for
/// (tools/atpkg-programs.spec: the `rustc` coherence group is aarch64-only).
pub const TRUST_HOST: &str = "aarch64-apple-darwin";

/// `true` when this `rustc -vV` output is a Trust compiler's.
///
/// THE MARKER is the `trust: <version>` key in `-vV`'s key/value block —
/// measured 2026-08-29 on the store build acb08e761 (`trust: 0.1.0`). Upstream
/// rustc (Homebrew 1.96.0, rustup stable, any nightly) prints `rustc`, `binary`,
/// `commit-hash`, `commit-date`, `host`, `release`, `LLVM version` and no
/// `trust:` line. Weaker markers rejected on purpose: the release string
/// (`1.99.0-dev` is what every upstream nightly says too), the commit hash
/// (changes with every store build), and `--print cfg`'s `trust_verify`
/// (disappears under the very `-Ztrust-verify=off` this workspace's build lane
/// applies).
/// Anchored at line start so a commit message or a `mistrust:` key cannot pass.
pub fn is_trust_compiler(verbose_version: &str) -> bool {
    verbose_version
        .lines()
        .any(|line| line.starts_with("trust:"))
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
/// Trust compiler we saw. `aterm_on_path` picks the state-aware remedy; without
/// aterm on a host Trust is not packaged for (atpkg's `rustc` group is
/// aarch64-darwin only) the installer would not help, and the refusal says so.
/// With aterm there, a locally linked seal (`aterm pkg link trust`) can serve.
/// `rustup_toolchain` is `RUSTUP_TOOLCHAIN` as the build script sees it:
/// rustup's proxy sets it for cargo and every build script (measured: `cargo
/// +stable` gives `stable-aarch64-apple-darwin`), while `RUSTC` is then a bare
/// `rustc` that names no toolchain. Unset means a cargo that is not rustup's,
/// `trust` means rustup's link, anything else an override.
pub fn refusal(
    host: &str,
    target: &str,
    rustc: &str,
    verbose_version: &str,
    aterm_on_path: bool,
    rustup_toolchain: Option<&str>,
) -> Option<String> {
    if host != target || is_trust_compiler(verbose_version) {
        return None;
    }
    let reported = verbose_version.lines().next().unwrap_or("(nothing)").trim();
    let toolchain = rustup_toolchain.filter(|name| !name.is_empty());
    let cause = match toolchain {
        Some("trust") => "rustup's `trust` toolchain is not a Trust compiler".to_string(),
        Some(name) => format!(
            "a rustup override (`RUSTUP_TOOLCHAIN`, `cargo +{name}` or `rustup override`) picked \
             `{name}` over the pin"
        ),
        None => "this cargo is not rustup's and ignores the pin".to_string(),
    };
    let fix = if !aterm_on_path && host != TRUST_HOST {
        format!(
            "fix:  none on this machine: Trust is packaged for Apple-silicon macOS only \
             ({TRUST_HOST}), this is {host}. Build on an Apple-silicon Mac."
        )
    } else if !aterm_on_path {
        format!(
            "fix:  install aterm (it installs the Trust toolchain):\n      \
             curl -fsSL {INSTALL_URL} | bash"
        )
    } else if toolchain == Some("trust") {
        "fix:  aterm pkg repair   (re-points rustup's `trust` at the store)".to_string()
    } else {
        "fix:  targo --unverified build   (Trust's cargo)".to_string()
    };
    Some(format!(
        "error: aterm compiles ONLY with the Trust toolchain, and this build is not using it.\n\
         \n  \
         compiler  {rustc}\n  \
         reports   {reported}\n  \
         cause     {cause}\n\
         \n  \
         {fix}\n"
    ))
}
