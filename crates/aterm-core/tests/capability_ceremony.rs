// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! #8001: capability-ceremony structural audit.
//!
//! Validates that each capability-token auth module in `aterm-core`
//! exposes the `as_host_auth_token` ceremony defined by the
//! `aterm_provenance::HostAuthorizationToken<'_>` lifetime gate, and that
//! the capability structs cannot be constructed outside their owning
//! module.
//!
//! # Why a source-scanning test (and not a `trybuild` compile-fail)
//!
//! The capability types are `pub(super)`/`pub(crate)` and carry a
//! private `_seal: ()` field. Rust's visibility rules already make them
//! unconstructable from outside the owning module; adding a `trybuild`
//! matrix would duplicate what rustc enforces. What a refactor *can*
//! silently regress is:
//!
//! 1. Deleting `as_host_auth_token` on a capability struct (the lift
//!    becomes unreachable without reintroducing the old bool gate).
//! 2. Making `_seal` public (downstream code can forge a capability).
//! 3. Making a capability struct `#[derive(Default)]` or adding a
//!    `new()` constructor outside the minting path.
//!
//! The test below scans the committed sources of the capability modules
//! listed in [`CAPABILITY_MODULES`] and fails on any of those
//! regressions. Together with the
//! per-module unit tests (`as_host_auth_token_lifts_pty_to_host`) in
//! each `_auth.rs` file, this closes the #8001 acceptance criterion
//! that "handler code cannot construct the token without going through
//! `authorize()`."

use std::fs;
use std::path::{Path, PathBuf};

/// All capability-token auth modules that must expose the ceremony.
///
/// Each entry is the module file name (under
/// `crates/aterm-core/src/terminal/`) paired with a list of capability
/// struct names that must carry a private `_seal: ()` field and — for
/// every module except `modal_auth` — an `as_host_auth_token` method.
///
/// `modal_auth` is the exception: its capability types live in
/// `aterm-ssh-conductor` and `aterm-tmux` (`ConductorActivationToken`
/// and `TmuxActivationToken`), which already carry the ceremony
/// upstream. We still audit the module for private-seal preservation.
///
/// `hyperlink_auth.rs` is deliberately ABSENT: OSC 8 acceptance is not a
/// host decision — a URI that clears the byte cap, the control/BiDi scans
/// and the scheme allowlist is carried on every host, and what a click may
/// OPEN is decided in the GUI at press time. A capability whose only
/// reachable setting is "granted" audits as a decision nobody makes, so
/// that module keeps only its host-minted scheme set. Add a row here when
/// a capability gains a policy, never to restore ceremony for its own sake.
const CAPABILITY_MODULES: &[(&str, &[&str], bool)] = &[
    // (file, capability struct names, requires_as_host_auth_token_in_file)
    (
        "clipboard_auth.rs",
        &["ClipboardWriteCapability", "ClipboardQueryCapability"],
        true,
    ),
    // Shell integration attaches `as_host_auth_token` to the auth state,
    // not to a capability struct — it has no per-invocation capability
    // type. Still must carry the ceremony method in-file.
    ("shell_integration_auth.rs", &[], true),
    ("response_capability.rs", &["ResponseCapability"], true),
    ("window_auth.rs", &["WindowOpsCapability"], true),
    ("dcs_auth.rs", &["DcsEmitCapability"], true),
];

/// The modules whose ceremony would be a lie, paired with the reason. A
/// capability token is only meaningful while some reachable policy can
/// WITHHOLD it; minted from a bit that is always `true`, it audits as a
/// decision nobody makes, and a reader trusts a gate that gates nothing.
///
/// `hyperlink_auth` is that module: OSC 8 admission is decided by the URI
/// (byte cap, control/BiDi scans, scheme allowlist) plus the host-minted
/// extra-scheme set, and no host — on any platform — can turn OSC 8 off.
/// What a click may OPEN is a separate boundary in `aterm-gui`
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

/// Each capability module (except `modal_auth`) exposes
/// `as_host_auth_token` returning `HostAuthorizationToken<'_>`. Deleting
/// this method would silently remove the #8001 lift ceremony.
#[test]
fn every_capability_module_exposes_as_host_auth_token() {
    for (file, _caps, requires_method) in CAPABILITY_MODULES {
        if !*requires_method {
            continue;
        }
        let src = read_module(file);
        assert!(
            src.contains("fn as_host_auth_token("),
            "{file}: expected an `fn as_host_auth_token(` definition (#8001 ceremony)"
        );
        assert!(
            src.contains("aterm_provenance::HostAuthorizationToken"),
            "{file}: expected the ceremony to return a `aterm_provenance::HostAuthorizationToken`"
        );
        assert!(
            src.contains("__new_for_capability_only"),
            "{file}: expected `HostAuthorizationToken::__new_for_capability_only()` call \
             (the only public constructor of the capability-seal token)"
        );
    }
}

/// Each capability struct carries a private `_seal: ()` field. This is
/// the structural guarantee that handler code outside the module cannot
/// construct the capability — the type's only field is inaccessible.
#[test]
fn every_capability_struct_has_private_seal() {
    for (file, caps, _) in CAPABILITY_MODULES {
        if caps.is_empty() {
            continue;
        }
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
    for (file, caps, _) in CAPABILITY_MODULES {
        if caps.is_empty() {
            continue;
        }
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

/// The `HostAuthorizationToken` public constructor is deliberately
/// verbosely named and `#[doc(hidden)]`. Regression-guarding against a
/// future refactor that renames it back to `new()` or drops the
/// `#[doc(hidden)]` attribute — either would make the capability seal
/// look like an inviting public API.
///
/// After #8013 the constructor MUST also carry
/// `#[cfg(any(test, feature = "internal-mint"))]` so that non-allow-listed
/// workspace crates cannot name it even at compile time.
#[test]
fn host_auth_token_constructor_is_capability_sealed() {
    let provenance_authorize = {
        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        let workspace_root = Path::new(manifest_dir)
            .parent()
            .expect("crates/")
            .parent()
            .expect("workspace root");
        workspace_root
            .join("crates")
            .join("aterm-provenance")
            .join("src")
            .join("authorize.rs")
    };
    let src = fs::read_to_string(&provenance_authorize)
        .unwrap_or_else(|err| panic!("failed to read {}: {err}", provenance_authorize.display()));
    // The constructor name and its `#[doc(hidden)]` gate are both part
    // of the capability-seal contract.
    assert!(
        src.contains("pub fn __new_for_capability_only()"),
        "HostAuthorizationToken constructor must stay named `__new_for_capability_only` \
         to preserve its deliberately-unwelcoming public shape"
    );
    assert!(
        src.contains("#[doc(hidden)]"),
        "HostAuthorizationToken constructor must stay `#[doc(hidden)]` \
         so cargo doc does not surface it as an inviting public API"
    );
    // `NetworkAuthorizationToken` mirrors the same seal.
    assert!(
        src.contains("NetworkAuthorizationToken"),
        "NetworkAuthorizationToken capability type must remain defined alongside \
         HostAuthorizationToken (the two ceremonies are the bottom edges of the lattice)"
    );
    // #8013: the constructor MUST also carry the feature gate. Without the
    // gate, any workspace crate can mint a token and bypass the provenance
    // lattice. `aterm audit policy --seals` enforces the same
    // invariant in CI; this test catches the regression locally too.
    assert!(
        src.contains("#[cfg(any(test, feature = \"internal-mint\"))]"),
        "HostAuthorizationToken / NetworkAuthorizationToken \
         __new_for_capability_only constructors must be gated behind \
         #[cfg(any(test, feature = \"internal-mint\"))]. See #8013 and \
         aterm audit policy --seals."
    );
}

/// Every capability module listed in `CAPABILITY_MODULES` actually
/// exists on disk. Guards against a rename that silently drops coverage
/// from this test matrix.
#[test]
fn all_capability_modules_exist() {
    let dir = terminal_src_dir();
    for (file, _, _) in CAPABILITY_MODULES {
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
    let listed: Vec<&str> = CAPABILITY_MODULES.iter().map(|(f, _, _)| *f).collect();
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
