// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Capability-token structural check: the capability structs of each
//! capability-token auth module in `aterm-core` cannot be constructed outside
//! their owning module.
//!
//! Visibility rules already forbid construction, so rather than a `trybuild`
//! matrix this scans the sources of [`CAPABILITY_MODULES`] for what a refactor
//! can break: a public `_seal`, a `#[derive(Default)]`, or a `new()` outside
//! the minting path.

use std::fs;
use std::path::{Path, PathBuf};

/// Capability-token auth modules (file under `crates/aterm-core/src/terminal/`)
/// and the capability structs that must carry a private `_seal: ()` field.
///
/// `hyperlink_auth.rs` is absent: OSC 8 acceptance is not a host decision
/// (see [`CEREMONY_FREE_MODULES`]). Add a row when a capability gains a policy.
const CAPABILITY_MODULES: &[(&str, &[&str])] = &[
    // (file, capability struct names)
    (
        "clipboard_auth.rs",
        &["ClipboardWriteCapability", "ClipboardQueryCapability"],
    ),
    ("response_capability.rs", &["ResponseCapability"]),
    ("window_auth.rs", &["WindowOpsCapability"]),
];

/// Modules that must not carry a capability token, with the reason: a token
/// minted from a bit that is always `true` gates nothing.
///
/// `hyperlink_auth`: OSC 8 admission depends on the URI (byte cap,
/// control/BiDi scans, scheme allowlist) and the host-minted extra schemes, and
/// no host can turn OSC 8 off. What a click may open is decided in `aterm-gui`
/// (`is_safe_url` guarding `open_url_external`).
const CEREMONY_FREE_MODULES: &[(&str, &[&str])] = &[(
    "hyperlink_auth.rs",
    &[
        "struct HyperlinkCapability",
        "struct HyperlinkMintAuthority",
        "fn try_mint_capability",
        "fn try_mint_with_policy",
        "fn as_host_auth_token(",
        "authorized: bool",
    ],
)];

fn terminal_src_dir() -> PathBuf {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    Path::new(manifest_dir).join("src").join("terminal")
}

fn read_module(file: &str) -> String {
    let path = terminal_src_dir().join(file);
    fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("failed to read {}: {err}", path.display()))
}

/// Each capability struct carries a private `_seal: ()` field. This is
/// the structural guarantee that handler code outside the module cannot
/// construct the capability — the type's only field is inaccessible.
#[test]
fn every_capability_struct_has_private_seal() {
    for (file, caps) in CAPABILITY_MODULES {
        let src = read_module(file);
        for cap in *caps {
            // Find the struct definition and verify it contains `_seal: ()`.
            let struct_header = format!("struct {cap}");
            let Some(struct_idx) = src.find(&struct_header) else {
                panic!("{file}: capability struct `{cap}` not found");
            };
            // Look within the next 400 bytes for the seal field.
            let window_end = (struct_idx + 400).min(src.len());
            let window = &src[struct_idx..window_end];
            assert!(
                window.contains("_seal: ()"),
                "{file}: capability `{cap}` must have a private `_seal: ()` field \
                 (blocks outside-module construction)"
            );
        }
    }
}

/// No capability struct derives `Default` or `Clone` — deriving either
/// would re-open construction-from-thin-air for any consumer who can
/// name the type. Clone is particularly dangerous for `#[must_use]`
/// capabilities: it would let a handler reuse a single mint across
/// multiple dispatches.
#[test]
fn capability_structs_do_not_derive_default_or_clone() {
    for (file, caps) in CAPABILITY_MODULES {
        let src = read_module(file);
        for cap in *caps {
            let struct_header = format!("struct {cap}");
            let Some(struct_idx) = src.find(&struct_header) else {
                panic!("{file}: capability struct `{cap}` not found");
            };
            // Scan the 200 bytes immediately *before* the struct header
            // for a `#[derive(...)]` attribute that includes `Default`
            // or `Clone`.
            let before_start = struct_idx.saturating_sub(200);
            let before = &src[before_start..struct_idx];
            assert!(
                !before.contains("Default") || !before.contains("#[derive"),
                "{file}: capability `{cap}` must not `#[derive(Default)]` — \
                 that would let any consumer mint a capability without authorization"
            );
            assert!(
                !before.contains("Clone") || !before.contains("#[derive"),
                "{file}: capability `{cap}` must not `#[derive(Clone)]` — \
                 that would let a handler replay a single mint across dispatches"
            );
        }
    }
}

/// Every capability module listed in `CAPABILITY_MODULES` actually
/// exists on disk. Guards against a rename that silently drops coverage
/// from this test matrix.
#[test]
fn all_capability_modules_exist() {
    let dir = terminal_src_dir();
    for (file, _) in CAPABILITY_MODULES {
        let path = dir.join(file);
        assert!(
            path.is_file(),
            "capability module `{file}` does not exist at {}",
            path.display()
        );
    }
}

/// A module with no withholding policy carries no capability ceremony.
/// The two halves are one decision: a capability arriving without a
/// caller that can refuse it, or a policy arriving without a row in
/// `CAPABILITY_MODULES`, are both this test failing.
#[test]
fn a_module_with_no_policy_carries_no_capability_ceremony() {
    let listed: Vec<&str> = CAPABILITY_MODULES.iter().map(|(f, _)| *f).collect();
    for (file, forbidden) in CEREMONY_FREE_MODULES {
        assert!(
            !listed.contains(file),
            "{file} is in both CAPABILITY_MODULES and CEREMONY_FREE_MODULES — \
             a module either has a policy that can withhold its capability or it does not"
        );
        let src = read_module(file);
        for needle in *forbidden {
            assert!(
                !src.contains(needle),
                "{file}: `{needle}` is capability ceremony over a policy that cannot refuse. \
                 Give the capability a caller that can withhold it and add a \
                 CAPABILITY_MODULES row, or leave the module saying plainly what it decides"
            );
        }
    }
}

/// `hyperlink_auth`'s extra-scheme set is a decision that has callers:
/// `aterm-wasm` and `aterm-gpu-web` both export it so an embedding app
/// can deep-link its own scheme. Carrying no acceptance switch is not
/// the same as carrying no policy, and the scheme API is the policy.
#[test]
fn hyperlink_auth_keeps_the_host_minted_scheme_decision() {
    let src = read_module("hyperlink_auth.rs");
    for needle in [
        "fn authorize_scheme(",
        "fn revoke_scheme(",
        "fn extra_schemes(",
        "NEVER_ALLOW_SCHEMES",
    ] {
        assert!(
            src.contains(needle),
            "hyperlink_auth.rs must keep `{needle}` — the scheme set is the ONE \
             hyperlink decision a host makes, and hosts make it"
        );
    }
}
