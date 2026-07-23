// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! DCS passthrough capability authority — structural gate on raw DCS
//! payload delivery to host callbacks (#8009 CF-013).
//!
//! # The privilege-conflation problem
//!
//! DCS sequences (`ESC P … ST`) deliver arbitrary byte payloads to the
//! host via [`super::TerminalDcs::callback`]. Before this module, every
//! DCS callback invocation handed the payload to the host untagged — the
//! host saw `&[u8]` and had no structural way to know the bytes came
//! from an untrusted PTY stream rather than from host-synthesized test
//! fixtures. Per `reports/2026-04-18-privilege-conflation-audit.md`,
//! CF-013 marks this as a **trust-origin conflation**: PTY-origin DCS
//! payload is narrated / forwarded with the same authority as host-
//! generated payload.
//!
//! # The structural fix — parallel to [`super::hyperlink_auth`]
//!
//! The DCS unhook handler no longer reaches for `self.dcs.callback`
//! directly. Instead, [`DcsAuth::try_mint_capability`] returns
//! `Option<DcsEmitCapability>`; on `Some`, the handler calls
//! [`invoke_dcs_callback`] with the token, which wraps the raw payload
//! in a [`aterm_provenance::Provenance<&[u8], aterm_provenance::Pty>`]
//! and deliberately *erases* the provenance (audit-surfaced via the
//! `// PROVENANCE-ERASE: CF-013 DCS FFI boundary` comment) before
//! handing to the FFI-stable `FnMut(&[u8], u8)` callback. On `None`
//! (authorization revoked) the callback is never invoked — the DCS
//! payload is silently dropped.
//!
//! The capability is **zero-sized with a private `_seal: ()`**. No code
//! outside this module can construct one. Authorization transitions
//! ([`DcsAuth::authorize`] / [`revoke`]) are `pub(crate)` and reachable
//! only via the host-facing `Terminal` API
//! ([`super::Terminal::authorize_dcs`] /
//! [`super::Terminal::revoke_dcs`]).
//!
//! # What the capability does not do
//!
//! This is an **opt-out** gate for the raw DCS callback pipeline. It
//! does not police the *contents* of the DCS payload (that's the job of
//! the per-DCS-type handlers: DECRQSS, Sixel, DECDLD, tmux/conductor
//! tokens, XTGETTCAP, etc.). Those handlers already run *before* the
//! callback — and the tmux/conductor activation paths already have their
//! own mint-token gates (`modal_protocol_auth::try_mint_tmux_token`). The
//! DCS capability covers only the raw-bytes callback, which is the one
//! path that delivers PTY payload to host-registered FnMut handlers.
//!
//! # Default posture
//!
//! The capability defaults to **authorized**. The pre-refactor behavior
//! was that every registered DCS callback received every completed DCS
//! sequence; hardened hosts can revoke via
//! [`super::Terminal::revoke_dcs`].

// ---------------------------------------------------------------------------
// Capability token — zero-sized, unforgeable outside this module.
// ---------------------------------------------------------------------------

/// Capability for raw DCS callback emission (#8009 CF-013).
///
/// Zero-sized. Its constructor is private to this module, so the only
/// way to obtain one is through [`DcsAuth::try_mint_capability`], which
/// requires the host to have previously called [`DcsAuth::authorize`]
/// (itself reachable only through [`super::Terminal::authorize_dcs`]).
///
/// Consumed by value when passed to [`invoke_dcs_callback`]; Rust move
/// semantics prevent a stashed token from silently replaying a single
/// authorization across future DCS sequences.
#[must_use = "DcsEmitCapability has no effect unless passed to invoke_dcs_callback"]
pub(super) struct DcsEmitCapability {
    _seal: (),
}

impl DcsEmitCapability {
    /// Provenance ceremony: lift this DCS capability into a
    /// [`aterm_provenance::HostAuthorizationToken`] borrowed for the
    /// capability's lifetime.
    ///
    /// Holding a `DcsEmitCapability` proves the host authorized raw DCS
    /// callback delivery; the returned token lets downstream
    /// `authorize_pty_to_host` consumers lift Pty-origin DCS byte slices
    /// into `Host`-origin values at the FFI boundary.
    #[must_use]
    #[allow(
        dead_code,
        reason = "ceremony surface required by #8001 audit test; first external caller still pending"
    )]
    pub(crate) fn as_host_auth_token(&self) -> aterm_provenance::HostAuthorizationToken<'_> {
        let _ = self;
        aterm_provenance::HostAuthorizationToken::__new_for_capability_only()
    }
}

/// Zero-sized minting authority for [`DcsEmitCapability`].
///
/// Held implicitly by the terminal module — no field is required
/// because the authority has no state. Named ZST for parity with
/// [`super::hyperlink_auth::HyperlinkMintAuthority`] etc.
#[derive(Debug, Default)]
pub(super) struct DcsEmitMintAuthority {
    _seal: (),
}

impl DcsEmitMintAuthority {
    /// Construct the authority.
    #[inline]
    #[must_use]
    pub(super) const fn new() -> Self {
        Self { _seal: () }
    }

    /// Attempt to mint a [`DcsEmitCapability`] given a boolean policy
    /// bit.
    ///
    /// Returns `Some` iff `authorized` is `true`. This is the only
    /// public constructor of [`DcsEmitCapability`]; adding a new
    /// DCS-callback-shaped handler that forgets to consult the auth
    /// state produces a compile error at the `invoke_dcs_callback` call
    /// site rather than silently reopening the hole.
    #[inline]
    #[must_use]
    pub(super) fn try_mint(&self, authorized: bool) -> Option<DcsEmitCapability> {
        let _ = self;
        if authorized {
            Some(DcsEmitCapability { _seal: () })
        } else {
            None
        }
    }
}

// ---------------------------------------------------------------------------
// Authorization state.
// ---------------------------------------------------------------------------

/// Host-side authorization state for raw DCS callback emission
/// (#8009 CF-013).
///
/// Lives on [`super::Terminal`]. Forwarded through
/// [`super::TerminalHandler`] so parser handlers can call
/// [`Self::try_mint_capability`], but the
/// [`authorize`][Self::authorize] / [`revoke`][Self::revoke] methods
/// are `pub(crate)` — handlers cannot escalate their own authorization.
#[derive(Debug)]
pub(crate) struct DcsAuth {
    authorized: bool,
}

impl Default for DcsAuth {
    fn default() -> Self {
        Self::new()
    }
}

impl DcsAuth {
    /// Construct an authorization state with the capability **granted**.
    ///
    /// DCS callbacks default to authorized (matches the pre-refactor
    /// behavior — every registered DCS callback received every completed
    /// DCS sequence). Hosts shipping a hardened profile can call
    /// [`revoke`][Self::revoke] (or the `Terminal::revoke_dcs` wrapper)
    /// after construction.
    #[inline]
    #[must_use]
    pub(crate) const fn new() -> Self {
        Self { authorized: true }
    }

    /// Authorize raw DCS callback delivery.
    ///
    /// After this call, a completed PTY-origin DCS sequence drives the
    /// registered `DcsCallback` with the payload bytes.
    #[allow(
        dead_code,
        reason = "authorize API reserved for hosts that revoke-then-reauthorize at runtime"
    )]
    pub(crate) fn authorize(&mut self) {
        self.authorized = true;
    }

    /// Revoke raw DCS callback delivery.
    ///
    /// After this call, subsequent PTY-origin DCS payloads are dropped
    /// silently at the capability gate — the callback is not invoked.
    /// [`authorize`][Self::authorize] restores normal behavior.
    ///
    /// **Note:** per-type DCS handlers (DECRQSS, Sixel, tmux/conductor,
    /// XTGETTCAP) still run — they are governed by their own capability
    /// gates. This revoke only blocks the raw `FnMut(&[u8], u8)`
    /// callback.
    pub(crate) fn revoke(&mut self) {
        self.authorized = false;
    }

    /// Whether raw DCS callback delivery is currently authorized.
    #[inline]
    #[must_use]
    pub(crate) const fn is_authorized(&self) -> bool {
        self.authorized
    }

    /// Attempt to mint a [`DcsEmitCapability`]. Returns `Some` iff
    /// [`authorize`][Self::authorize] has been called (or `new()`
    /// defaulted authorized) and not since revoked.
    #[inline]
    #[must_use]
    pub(super) fn try_mint_capability(&self) -> Option<DcsEmitCapability> {
        DcsEmitMintAuthority::new().try_mint(self.authorized)
    }
}

// ---------------------------------------------------------------------------
// Sink invoker — the only way to reach `self.dcs.callback` from a handler.
// ---------------------------------------------------------------------------

/// Invoke the DCS callback with the raw payload and final byte.
///
/// The capability token is consumed by value, so a minted capability
/// cannot be reused across dispatches. The payload is deliberately
/// wrapped in a `Provenance<&[u8], Pty>` inside this function so that
/// the trust boundary is structurally visible to every reader of the
/// emission site: the data crossing into the FFI-stable
/// `FnMut(&[u8], u8)` callback is PTY-origin.
///
/// Provenance is *erased* before the callback is invoked because the
/// public callback signature is a long-standing FFI contract (see
/// `docs/CALLBACKS.md`). The erasure is audit-surfaced via the
/// `// PROVENANCE-ERASE: CF-013 DCS FFI boundary` comment below; CI
/// greps for it. Future work may introduce a parallel
/// `DcsCallbackWithOrigin` setter that preserves the provenance wrapper
/// across the FFI boundary, at which point this erasure site is the
/// single lever that needs changing.
pub(super) fn invoke_dcs_callback(
    callback: &mut aterm_types::DcsCallback,
    _token: DcsEmitCapability,
    data: &[u8],
    final_byte: u8,
) {
    // PTY-origin byte slice. The type-level marker proves the trust
    // level at the emission site.
    let payload: aterm_provenance::Provenance<&[u8], aterm_provenance::Pty> =
        aterm_provenance::Provenance::from_pty(data);
    // PROVENANCE-ERASE: CF-013 DCS FFI boundary — public callback
    // signature `FnMut(&[u8], u8)` is FFI-stable; erasure is intentional
    // and audit-surfaced.
    let data_ref: &[u8] = payload.as_ref();
    callback(data_ref, final_byte);
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_new_is_authorized() {
        let auth = DcsAuth::new();
        assert!(auth.is_authorized());
        assert!(auth.try_mint_capability().is_some());
    }

    #[test]
    fn default_trait_impl_is_authorized() {
        let auth = DcsAuth::default();
        assert!(auth.is_authorized());
    }

    #[test]
    fn revoke_disables_future_mint() {
        let mut auth = DcsAuth::new();
        auth.revoke();
        assert!(!auth.is_authorized());
        assert!(auth.try_mint_capability().is_none());
    }

    #[test]
    fn re_authorize_after_revoke_restores_capability() {
        let mut auth = DcsAuth::new();
        auth.revoke();
        assert!(auth.try_mint_capability().is_none());
        auth.authorize();
        assert!(auth.try_mint_capability().is_some());
    }

    #[test]
    fn revoke_on_unauthorized_is_idempotent() {
        let mut auth = DcsAuth::new();
        auth.revoke();
        auth.revoke();
        assert!(!auth.is_authorized());
        assert!(auth.try_mint_capability().is_none());
    }

    #[test]
    fn mint_authority_reflects_policy_bit() {
        let authority = DcsEmitMintAuthority::new();
        assert!(authority.try_mint(false).is_none());
        assert!(authority.try_mint(true).is_some());
    }

    #[test]
    fn capability_and_authority_are_zero_sized() {
        assert_eq!(std::mem::size_of::<DcsEmitCapability>(), 0);
        assert_eq!(std::mem::size_of::<DcsEmitMintAuthority>(), 0);
    }

    /// #8001 ceremony: a minted `DcsEmitCapability` lifts
    /// `Provenance<_, Pty>` to `Provenance<_, Host>`.
    #[test]
    fn as_host_auth_token_lifts_pty_to_host() {
        use aterm_provenance::{OriginTag, Provenance, authorize_pty_to_host};

        let auth = DcsAuth::new();
        let cap = auth.try_mint_capability().expect("default authorized");
        let tok = cap.as_host_auth_token();
        let pty: Provenance<Vec<u8>, aterm_provenance::Pty> =
            Provenance::from_pty(b"\x1bP$qm\x1b\\".to_vec());
        let host = authorize_pty_to_host(pty, tok);
        assert_eq!(host.tag(), OriginTag::Host);
        assert_eq!(host.as_ref(), b"\x1bP$qm\x1b\\");
    }

    /// The sink invoker consumes the capability by value and forwards
    /// the bytes to the provided callback.
    #[test]
    fn invoke_dcs_callback_delivers_payload_when_authorized() {
        use std::sync::{Arc, Mutex};

        let auth = DcsAuth::new();
        let token = auth.try_mint_capability().expect("default authorized");
        let received: Arc<Mutex<Vec<(Vec<u8>, u8)>>> = Arc::new(Mutex::new(Vec::new()));
        let received_clone = Arc::clone(&received);
        let mut callback: aterm_types::DcsCallback =
            Box::new(move |data: &[u8], final_byte: u8| {
                received_clone
                    .lock()
                    .unwrap()
                    .push((data.to_vec(), final_byte));
            });
        invoke_dcs_callback(&mut callback, token, b"test-payload", b'q');
        let guard = received.lock().unwrap();
        assert_eq!(guard.len(), 1);
        assert_eq!(guard[0].0, b"test-payload");
        assert_eq!(guard[0].1, b'q');
    }
}
