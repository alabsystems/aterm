// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Hyperlink capability authority — structural gate on OSC 8 URI
//! acceptance (#8009 CF-014).
//!
//! # The privilege-conflation problem
//!
//! OSC 8 (`OSC 8 ; params ; URI ST`) sets the `current_hyperlink` field
//! that subsequent printable characters are tagged with. The host's UI
//! can then render those cells as clickable hyperlinks. Before this
//! module the URI's scheme was validated against a safe allowlist
//! (`is_safe_scheme` in [`crate::perception`]) — but the allowlist
//! decision was conflated with the "should this terminal accept OSC 8
//! at all" decision: every live terminal session unconditionally ran
//! the allowlist check.
//!
//! Per `reports/2026-04-18-privilege-conflation-audit.md` — **CF-014** —
//! the conflation is: "Denylist (`javascript:`, `file:`, etc.) but not
//! allowlist. New custom schemes (`tel:`, `sms:`, `custom-app://`) can
//! route to registered handlers without review." (The existing check is
//! in fact an allowlist, so the header is slightly imprecise; the
//! actual gap is that the allowlist is **engine-wide** rather than
//! **host-mintable**.)
//!
//! # The structural fix — mirror of [`super::multipart_file_auth`]
//!
//! The OSC 8 handler no longer writes
//! `self.transient.current_hyperlink = Some(Arc::from(uri))` directly.
//! Instead, [`HyperlinkAuth::try_mint_capability`] returns
//! `Option<HyperlinkCapability>`; on `Some`, the handler calls
//! [`invoke_set_hyperlink`] with the token. On `None` (authorization
//! revoked) the write is structurally unreachable — the
//! `current_hyperlink` slot remains `pub(super)` for the
//! [`super::transient_state::TransientState`] plumbing, but the
//! OSC-dispatch write path requires the capability argument.
//!
//! The capability is **zero-sized with a private `_seal: ()`** — no
//! code outside this module can construct one. Authorization
//! transitions ([`HyperlinkAuth::authorize`] / [`revoke`]) are
//! `pub(crate)` and reachable only via the host-facing `Terminal` API
//! ([`super::Terminal::authorize_hyperlinks`] /
//! [`super::Terminal::revoke_hyperlinks`]).
//!
//! # Relationship to the URL scheme allowlist
//!
//! This capability does **not** replace the scheme allowlist — it
//! layers on top of it. The OSC 8 handler still rejects schemes that
//! fail [`crate::perception::is_safe_scheme`]; the capability gate is
//! an orthogonal "is the host accepting OSC 8 at all" switch. The
//! design admits future extension to a capability that carries an
//! explicit `Vec<SchemeId>` so the host can mint "accept these schemes
//! this session" tokens, but the v1 landing keeps the allowlist
//! unchanged and adds the structural gate so new OSC 8 variants
//! (e.g. a hypothetical OSC 8.1 that bypasses the allowlist check)
//! cannot land without going through this type.
//!
//! # Default posture
//!
//! The capability defaults to **authorized** (matches the pre-refactor
//! behavior — OSC 8 hyperlinks are a long-standing terminal feature
//! most hosts want enabled). Hosts that ship a hardened profile can
//! revoke via [`super::Terminal::revoke_hyperlinks`]; the scheme
//! allowlist still runs even when the capability is authorized.

// ---------------------------------------------------------------------------
// Capability token — zero-sized, unforgeable outside this module.
// ---------------------------------------------------------------------------

/// Capability for OSC 8 hyperlink URI acceptance (#8009 CF-014).
///
/// Zero-sized. Its constructor is private to this module, so the only
/// way to obtain one is through
/// [`HyperlinkAuth::try_mint_with_policy`] (or
/// [`HyperlinkAuth::try_mint_capability`]), which requires the host to
/// have previously called [`HyperlinkAuth::authorize`] (itself
/// reachable only through [`super::Terminal::authorize_hyperlinks`]).
///
/// Consumed by value when passed to [`invoke_set_hyperlink`]; Rust
/// move semantics prevent a stashed token from silently replaying a
/// single authorization across future dispatches.
#[must_use = "HyperlinkCapability has no effect unless passed to invoke_set_hyperlink"]
pub(super) struct HyperlinkCapability {
    _seal: (),
}

impl HyperlinkCapability {
    /// Provenance ceremony: lift this hyperlink capability into a
    /// [`aterm_provenance::HostAuthorizationToken`] borrowed for the
    /// capability's lifetime.
    ///
    /// Holding a `HyperlinkCapability` proves the host authorized OSC 8
    /// hyperlink acceptance; the returned token lets downstream
    /// `authorize_pty_to_host` consumers lift Pty-origin URI strings
    /// into `Host`-origin values for the grid's transient hyperlink
    /// slot.
    #[allow(
        dead_code,
        reason = "audit-only provenance ceremony retained until hyperlink payloads are lifted at a production call site"
    )]
    #[must_use]
    pub(crate) fn as_host_auth_token(&self) -> aterm_provenance::HostAuthorizationToken<'_> {
        let _ = self;
        aterm_provenance::HostAuthorizationToken::__new_for_capability_only()
    }
}

/// Zero-sized minting authority for [`HyperlinkCapability`].
///
/// Held implicitly by the terminal module — no field is required
/// because the authority has no state. Named ZST for parity with the
/// other capability-mint authorities in this module tree.
#[derive(Debug, Default)]
pub(super) struct HyperlinkMintAuthority {
    _seal: (),
}

impl HyperlinkMintAuthority {
    /// Construct the authority.
    #[inline]
    #[must_use]
    pub(super) const fn new() -> Self {
        Self { _seal: () }
    }

    /// Attempt to mint a [`HyperlinkCapability`] given a boolean
    /// policy bit.
    ///
    /// Returns `Some` iff `authorized` is `true`. This is the only
    /// public constructor of [`HyperlinkCapability`]; adding a new
    /// OSC 8-shaped handler that forgets to consult the auth state
    /// produces a compile error at the `invoke_set_hyperlink` call
    /// site rather than silently reopening the hole.
    #[inline]
    #[must_use]
    pub(super) fn try_mint(&self, authorized: bool) -> Option<HyperlinkCapability> {
        let _ = self;
        if authorized {
            Some(HyperlinkCapability { _seal: () })
        } else {
            None
        }
    }
}

// ---------------------------------------------------------------------------
// Host-minted extra schemes (orca deep-links §7 — the extension the module
// doc promised: a capability carrying an explicit scheme set).
// ---------------------------------------------------------------------------

/// Hard cap on host-minted extra schemes. A host mints one or two app schemes
/// (`orca`); a large set would recreate the pre-#7919 open-ended surface.
/// Abstract twin: `Cap` in `aterm_spec::derive::hyperlink_scheme_cap_model`.
pub(crate) const MAX_EXTRA_SCHEMES: usize = 4;

/// Hard cap on one extra scheme's length. Real schemes are short; this bounds
/// the per-URI `eq_ignore_ascii_case` comparisons in the OSC-8 hot path.
pub(crate) const MAX_EXTRA_SCHEME_LEN: usize = 32;

/// Schemes refused EVEN WHEN THE HOST ASKS — execution/exfiltration primitives
/// no terminal hyperlink should ever carry. Matching is case-insensitive and
/// also refuses `+`/`-`/`.`-suffixed spellings (`javascript.` etc.), so the
/// never-allow decision cannot be evaded by RFC 3986 scheme-charset suffixes
/// that some URL parsers normalize away.
const NEVER_ALLOW_SCHEMES: &[&str] = &["javascript", "data", "file", "vbscript", "about", "blob"];

/// Whether `scheme` (WITHOUT the trailing `:`) has the RFC 3986 §3.1 shape:
/// `ALPHA *( ALPHA / DIGIT / "+" / "-" / "." )`. Shared by the OSC-8 gate's
/// URI parse and [`HyperlinkAuth::authorize_scheme`]'s mint validation so the
/// two can never diverge (a minted scheme unreachable by the gate, or vice
/// versa — the smuggling seam).
#[must_use]
pub(crate) fn is_rfc3986_scheme_shape(scheme: &str) -> bool {
    let mut chars = scheme.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_alphabetic() {
        return false;
    }
    chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
}

/// Whether `lowered` (already ASCII-lowercased) hits the never-allow set,
/// including trailing scheme-charset padding (`javascript.`, `data+`).
fn is_never_allowed(lowered: &str) -> bool {
    let trimmed = lowered.trim_end_matches(['+', '-', '.']);
    NEVER_ALLOW_SCHEMES.contains(&trimmed)
}

// ---------------------------------------------------------------------------
// Authorization state.
// ---------------------------------------------------------------------------

/// Host-side authorization state for OSC 8 hyperlink acceptance
/// (#8009 CF-014) plus the host-minted extra-scheme set (deep-links §7).
///
/// Lives on [`super::Terminal`]. Forwarded through
/// [`super::TerminalHandler`] so parser handlers can call
/// [`Self::try_mint_capability`], but the
/// [`authorize`][Self::authorize] / [`revoke`][Self::revoke] methods
/// are `pub(crate)` — handlers cannot escalate their own authorization.
#[derive(Debug)]
pub(crate) struct HyperlinkAuth {
    authorized: bool,
    /// Host-minted extra schemes accepted IN ADDITION to the hardcoded safe
    /// allowlist. Stored ASCII-lowercased; bounded by [`MAX_EXTRA_SCHEMES`].
    /// Mutations only via [`authorize_scheme`][Self::authorize_scheme] /
    /// [`revoke_scheme`][Self::revoke_scheme] (host API — handlers cannot
    /// mint schemes for themselves). Abstract twin:
    /// `hyperlink_scheme_cap_model` (registered in aterm-spec).
    extra_schemes: Vec<Box<str>>,
}

impl Default for HyperlinkAuth {
    fn default() -> Self {
        Self::new()
    }
}

impl HyperlinkAuth {
    /// Construct an authorization state with the capability **granted**.
    ///
    /// Hyperlinks default to authorized (matches the pre-refactor
    /// behavior — OSC 8 has been a universally supported terminal
    /// feature since xterm's 2017 patch). Hosts shipping a hardened
    /// profile can call [`revoke`][Self::revoke] (or the
    /// `Terminal::revoke_hyperlinks` wrapper) after construction.
    #[inline]
    #[must_use]
    pub(crate) const fn new() -> Self {
        Self {
            authorized: true,
            extra_schemes: Vec::new(),
        }
    }

    /// Explicit alias for [`new`][Self::new] used where the reading
    /// call site benefits from the audit-visible name.
    #[inline]
    #[must_use]
    #[allow(
        dead_code,
        reason = "audit-visible constructor used by tests and reserved for host-config callers"
    )]
    pub(crate) const fn new_authorized() -> Self {
        Self::new()
    }

    /// Construct an authorization state with the capability **not
    /// granted**.
    ///
    /// Kept as an explicit constructor so tests and hardened-profile
    /// paths that want deny-by-default can name that choice in one
    /// line.
    #[inline]
    #[must_use]
    #[allow(
        dead_code,
        reason = "audit-visible constructor used by tests and reserved for hardened-mode callers"
    )]
    pub(crate) const fn new_revoked() -> Self {
        Self {
            authorized: false,
            extra_schemes: Vec::new(),
        }
    }

    /// Authorize OSC 8 hyperlink acceptance.
    ///
    /// After this call, a PTY-origin `OSC 8 ; params ; URI ST` sequence
    /// that passes the scheme allowlist (`is_safe_scheme`) will mint a
    /// [`HyperlinkCapability`] and set `transient.current_hyperlink`.
    #[allow(
        dead_code,
        reason = "authorize API reserved for hosts that revoke-then-reauthorize at runtime"
    )]
    pub(crate) fn authorize(&mut self) {
        self.authorized = true;
    }

    /// Revoke OSC 8 hyperlink acceptance.
    ///
    /// After this call, subsequent PTY-origin OSC 8 sequences are
    /// dropped silently at the capability gate — `current_hyperlink`
    /// is not mutated. [`authorize`][Self::authorize] restores normal
    /// behavior.
    pub(crate) fn revoke(&mut self) {
        self.authorized = false;
    }

    /// Whether OSC 8 hyperlink acceptance is currently authorized.
    #[inline]
    #[must_use]
    pub(crate) const fn is_authorized(&self) -> bool {
        self.authorized
    }

    /// Mint `scheme` into the extra allowlist (deep-links §7). Returns `false`
    /// — refusing, state unchanged — when the scheme is over-long, not RFC 3986
    /// scheme-shaped, in the never-allow set, or the set is full; `true` when
    /// stored (idempotently: re-minting a live scheme is `true` and does not
    /// consume a slot). Stored ASCII-lowercased; the OSC-8 gate compares
    /// case-insensitively, so acceptance is case-blind like the safe list.
    pub(crate) fn authorize_scheme(&mut self, scheme: &str) -> bool {
        if scheme.len() > MAX_EXTRA_SCHEME_LEN || !is_rfc3986_scheme_shape(scheme) {
            return false;
        }
        let lowered = scheme.to_ascii_lowercase();
        if is_never_allowed(&lowered) {
            return false;
        }
        if self.extra_schemes.iter().any(|s| **s == *lowered) {
            return true;
        }
        if self.extra_schemes.len() >= MAX_EXTRA_SCHEMES {
            return false;
        }
        self.extra_schemes.push(lowered.into_boxed_str());
        true
    }

    /// Remove `scheme` from the extra allowlist (case-insensitive), restoring
    /// the default posture for it. Removing an absent scheme is a no-op.
    pub(crate) fn revoke_scheme(&mut self, scheme: &str) {
        self.extra_schemes
            .retain(|s| !s.eq_ignore_ascii_case(scheme));
    }

    /// The live host-minted extra schemes (lowercased), for the OSC-8 gate and
    /// host introspection.
    #[inline]
    #[must_use]
    pub(crate) fn extra_schemes(&self) -> &[Box<str>] {
        &self.extra_schemes
    }

    /// Attempt to mint a [`HyperlinkCapability`]. Returns `Some` iff
    /// [`authorize`][Self::authorize] has been called (or `new()`
    /// defaulted authorized) and not since revoked.
    #[inline]
    #[must_use]
    #[allow(
        dead_code,
        reason = "post-migration entry point; handlers currently use try_mint_with_policy"
    )]
    pub(super) fn try_mint_capability(&self) -> Option<HyperlinkCapability> {
        HyperlinkMintAuthority::new().try_mint(self.authorized)
    }

    /// Attempt to mint a [`HyperlinkCapability`] using the caller-
    /// provided policy bit. Kept for future migration symmetry with
    /// [`super::session_memory_auth::SessionMemoryAuth::try_mint_with_policy`];
    /// hyperlinks do not currently have a `modes.allow_hyperlinks`
    /// mirror bool, so callers typically pass `self.is_authorized()`.
    #[inline]
    #[must_use]
    #[allow(
        dead_code,
        reason = "reserved for future bool-mirror migration if we introduce modes.allow_hyperlinks"
    )]
    pub(super) fn try_mint_with_policy(&self, policy: bool) -> Option<HyperlinkCapability> {
        let _ = self.authorized;
        HyperlinkMintAuthority::new().try_mint(policy)
    }
}

// ---------------------------------------------------------------------------
// Sink invoker — the only way to reach `transient.current_hyperlink` from a
// handler once the refactor completes.
// ---------------------------------------------------------------------------

/// Invoke the hyperlink-setting side effect with the validated URI and
/// optional id.
///
/// The capability token is consumed by value. A clear consequence of
/// the `_token` parameter is that static analysis of the call-graph can
/// prove the setter is reached iff a capability was minted, which is
/// iff [`HyperlinkAuth::authorize`] was called through the host API
/// (or `new()` defaulted authorized).
///
/// The URI is expected to have already cleared the scheme allowlist,
/// URL validity, BiDi-override filter, and length cap — this function
/// is the structural sink, not a second validator.
pub(super) fn invoke_set_hyperlink(
    transient: &mut super::transient_state::TransientState,
    _token: HyperlinkCapability,
    uri: &str,
    id: Option<&str>,
) {
    transient.current_hyperlink = Some(std::sync::Arc::from(uri));
    transient.current_hyperlink_id = id.map(std::sync::Arc::from);
    transient.update_has_transient_extras();
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_new_is_authorized() {
        // Hyperlinks default to authorized (backward-compat posture).
        let auth = HyperlinkAuth::new();
        assert!(auth.is_authorized());
        assert!(auth.try_mint_capability().is_some());
    }

    #[test]
    fn default_trait_impl_is_authorized() {
        let auth = HyperlinkAuth::default();
        assert!(auth.is_authorized());
    }

    #[test]
    fn new_revoked_denies_mint() {
        let auth = HyperlinkAuth::new_revoked();
        assert!(!auth.is_authorized());
        assert!(auth.try_mint_capability().is_none());
    }

    #[test]
    fn revoke_disables_future_mint() {
        let mut auth = HyperlinkAuth::new();
        auth.revoke();
        assert!(!auth.is_authorized());
        assert!(auth.try_mint_capability().is_none());
    }

    #[test]
    fn re_authorize_after_revoke_restores_capability() {
        let mut auth = HyperlinkAuth::new();
        auth.revoke();
        assert!(auth.try_mint_capability().is_none());
        auth.authorize();
        assert!(auth.try_mint_capability().is_some());
    }

    #[test]
    fn revoke_on_unauthorized_is_idempotent() {
        let mut auth = HyperlinkAuth::new_revoked();
        auth.revoke();
        assert!(auth.try_mint_capability().is_none());
        auth.revoke();
        assert!(auth.try_mint_capability().is_none());
    }

    #[test]
    fn mint_authority_reflects_policy_bit() {
        let authority = HyperlinkMintAuthority::new();
        assert!(authority.try_mint(false).is_none());
        assert!(authority.try_mint(true).is_some());
    }

    #[test]
    fn capability_and_authority_are_zero_sized() {
        assert_eq!(std::mem::size_of::<HyperlinkCapability>(), 0);
        assert_eq!(std::mem::size_of::<HyperlinkMintAuthority>(), 0);
    }

    #[test]
    fn try_mint_with_policy_tracks_policy_argument() {
        let auth = HyperlinkAuth::new();
        assert!(auth.try_mint_with_policy(false).is_none());
        assert!(auth.try_mint_with_policy(true).is_some());
    }

    // ---- host-minted extra schemes (deep-links §7) ----------------------

    #[test]
    fn authorize_scheme_stores_lowercased_and_is_idempotent() {
        let mut auth = HyperlinkAuth::new();
        assert!(auth.authorize_scheme("Orca"));
        assert_eq!(auth.extra_schemes().len(), 1);
        assert_eq!(&*auth.extra_schemes()[0], "orca");
        // Re-minting (any case) succeeds without consuming a slot.
        assert!(auth.authorize_scheme("ORCA"));
        assert_eq!(auth.extra_schemes().len(), 1);
    }

    #[test]
    fn authorize_scheme_bounded_at_max_extra_schemes() {
        let mut auth = HyperlinkAuth::new();
        for s in ["a1", "b2", "c3", "d4"] {
            assert!(auth.authorize_scheme(s));
        }
        assert!(
            !auth.authorize_scheme("e5"),
            "slot {MAX_EXTRA_SCHEMES}+1 must be refused"
        );
        assert_eq!(auth.extra_schemes().len(), MAX_EXTRA_SCHEMES);
        // A refused mint changes nothing; revoking frees the slot again.
        auth.revoke_scheme("a1");
        assert!(auth.authorize_scheme("e5"));
    }

    #[test]
    fn authorize_scheme_refuses_never_allow_incl_case_and_suffix_evasion() {
        let mut auth = HyperlinkAuth::new();
        for s in [
            "javascript",
            "JavaScript",
            "data",
            "file",
            "vbscript",
            "about",
            "blob",
            "javascript.",
            "javascript+",
            "DATA-",
        ] {
            assert!(!auth.authorize_scheme(s), "never-allow must refuse {s:?}");
        }
        assert!(auth.extra_schemes().is_empty());
        // A genuinely different scheme sharing the prefix is not blocked.
        assert!(auth.authorize_scheme("javascriptx"));
    }

    #[test]
    fn authorize_scheme_refuses_malformed_shapes_and_overlength() {
        let mut auth = HyperlinkAuth::new();
        for s in ["", "1orca", "+orca", "or ca", "orca\t", "or%3aca", "orc:a"] {
            assert!(!auth.authorize_scheme(s), "malformed shape must refuse {s:?}");
        }
        assert!(!auth.authorize_scheme(&"a".repeat(MAX_EXTRA_SCHEME_LEN + 1)));
        assert!(auth.extra_schemes().is_empty());
        // `+`/`-`/`.` are legal NON-LEADING scheme chars (RFC 3986).
        assert!(auth.authorize_scheme("web+orca"));
    }

    #[test]
    fn revoke_scheme_is_case_insensitive_and_noop_when_absent() {
        let mut auth = HyperlinkAuth::new();
        assert!(auth.authorize_scheme("orca"));
        auth.revoke_scheme("nonexistent");
        assert_eq!(auth.extra_schemes().len(), 1);
        auth.revoke_scheme("ORCA");
        assert!(auth.extra_schemes().is_empty());
    }

    /// #8001 ceremony: a minted `HyperlinkCapability` lifts
    /// `Provenance<_, Pty>` to `Provenance<_, Host>`.
    #[test]
    fn as_host_auth_token_lifts_pty_to_host() {
        use aterm_provenance::{OriginTag, Provenance, authorize_pty_to_host};

        let auth = HyperlinkAuth::new();
        let cap = auth.try_mint_capability().expect("default authorized");
        let tok = cap.as_host_auth_token();
        let pty: Provenance<String, aterm_provenance::Pty> =
            Provenance::from_pty("https://example.com".to_string());
        let host = authorize_pty_to_host(pty, tok);
        assert_eq!(host.tag(), OriginTag::Host);
        assert_eq!(host.as_ref(), "https://example.com");
    }
}
