// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Owns-correctness capabilities (ATERM_DESIGN WS-G).
//!
//! A [`Cap<E>`] authorizes effect `E` at a trust [`Tier`]. `Cap` carries a
//! private `_seal` field, so no `Cap { .. }` literal exists outside this crate;
//! the only way to obtain one is [`Authority::grant`]. An [`Authority`] is the
//! single minting authority and is itself minted by exactly one entry point,
//! [`Authority::root_authority`], which is `unsafe`.
//!
//! ## What is guaranteed
//!
//! TWO distinct properties, both delivered:
//!
//! 1. **No struct-literal forgery (Rust privacy).** Because `Cap`'s and
//!    `Authority`'s fields are private, no code outside this crate can write
//!    `Cap { .. }` / `Authority { .. }`. Capabilities flow only from a held
//!    `Authority`.
//! 2. **No-mint-reachability (feature-sealed mint + machine-checked property;
//!    ATERM_DESIGN §5.4).** [`Authority::root_authority`] is sealed behind the
//!    `launcher-mint` cargo feature (or a crate's own `test` cfg): the symbol
//!    exists ONLY for the trusted launcher binaries that enable the feature, so no
//!    engine crate can even *name* the mint — a parser / handler / extension path
//!    cannot reach a mint site because it is absent from that crate's compiled
//!    surface. The property is proven abstractly by
//!    [`aterm_spec::derive::mint_reachability_model`] (invariant `NoUntrustedTop`,
//!    model-checked by `ty`) and bound to the real tree by the
//!    `mint_sites_are_launcher_only` source-scan gate
//!    (`crates/aterm-cap/tests/mint_reachability.rs`): exactly the two launchers
//!    enable the feature, and no production code outside a launcher reaches the
//!    mint. The `unsafe` keyword is retained as an audit marker carrying the
//!    "call once, from the trusted launcher" contract.
//!
//! The **effect layer**: a function that performs a privileged effect takes a
//! `&Cap<ThatEffect>`, so it is a type error to call it without first having been
//! granted the capability. [`require`] is the canonical gate.
//!
//! STATUS: no-struct-forgery is sound-by-construction (privacy); the grant/tier/
//! effect-gate behaviour is tested below; no-mint-reachability (§5.4) is delivered
//! via the `launcher-mint` feature seal + `mint_reachability_model` (`ty`) +
//! `mint_sites_are_launcher_only` (source-scan).

use std::marker::PhantomData;

/// Trust tier carried by a capability — the effect layer's policy axis.
///
/// Ordered: `Untrusted < Trusted < Certified`. A sink can require a minimum tier
/// (e.g. only `Certified` capabilities may elide a runtime safety check).
#[derive(Clone, Copy, PartialEq, Eq, Debug, PartialOrd, Ord, Hash)]
pub enum Tier {
    /// From an unauthenticated / third-party source.
    Untrusted,
    /// From the trusted in-process core.
    Trusted,
    /// Backed by a machine-checked certificate (the only tier that may elide a
    /// runtime check; mirrors Trust's `Certified` assurance tier).
    Certified,
}

impl Tier {
    /// The variant's canonical name — identical to its `Debug` rendering. Used
    /// by the hand-written `Debug`/`Display` impls so they can format through
    /// [`std::fmt::Formatter::write_str`] (a single static-string write) rather
    /// than the `write!`/`format_args!` machinery.
    #[must_use]
    const fn name(self) -> &'static str {
        match self {
            Tier::Untrusted => "Untrusted",
            Tier::Trusted => "Trusted",
            Tier::Certified => "Certified",
        }
    }
}

/// A capability authorizing effect marker `E`, at a trust [`Tier`].
///
/// The `_seal` private field prevents struct-literal forgery: outside this crate
/// there is no way to name it, so no `Cap { .. }` literal can construct a `Cap`.
/// The only source is [`Authority::grant`], which requires holding an
/// [`Authority`]. (This is the no-struct-forgery property; the stronger
/// no-mint-reachability property is ROADMAP §5.4 — see the crate docs.)
pub struct Cap<E> {
    tier: Tier,
    _effect: PhantomData<fn() -> E>,
    _seal: Seal,
}

/// A private, un-nameable witness. It is `Copy` (so a granted [`Cap`] can be
/// copied) but still unconstructable outside this crate — copying one requires
/// already holding it, which requires the [`Authority`]. This blocks struct
/// literals only; it does NOT by itself prove no code path reaches the mint
/// (that is the §5.4 no-mint-reachability obligation, not yet implemented).
#[derive(Clone, Copy)]
struct Seal;

impl<E> Cap<E> {
    /// The trust tier this capability was granted at.
    #[must_use]
    pub fn tier(&self) -> Tier {
        self.tier
    }

    /// Whether this capability meets a minimum required tier.
    #[must_use]
    pub fn satisfies(&self, min: Tier) -> bool {
        self.tier >= min
    }
}

// Capabilities are copyable witnesses (passing one around does not consume it).
// Hand-written (not derived) so the impls do NOT add a spurious `E: Clone`/
// `E: Copy` bound — `E` is a phantom effect marker, never a value. The `Copy`
// type's canonical `clone` is `*self` (identical bytes: same tier, same seal).
impl<E> Clone for Cap<E> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<E> Copy for Cap<E> {}

impl<E> std::fmt::Debug for Cap<E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Cap<")?;
        f.write_str(std::any::type_name::<E>())?;
        f.write_str(">(")?;
        f.write_str(self.tier.name())?;
        f.write_str(")")
    }
}

/// The single minting authority. Holding one is the right to grant capabilities;
/// it is created once at process start by the trusted launcher and handed only to
/// the trusted core. It is not `Clone`, so it cannot be duplicated into untrusted
/// hands.
pub struct Authority {
    _seal: Seal,
}

impl Authority {
    /// Mint the process root authority. **The sealed root-authority mint** (§5.4):
    /// whoever obtains the returned value can grant ANY capability at ANY tier, so
    /// this is the most powerful operation in the process.
    ///
    /// It is `unsafe` deliberately — not because it can trigger memory unsafety,
    /// but to make every mint site a greppable, audited, opt-in `unsafe` block,
    /// and to make the safety contract a *requirement of the caller* rather than
    /// ambient authority any code could exercise. There is intentionally **no
    /// safe public path** to an `Authority`.
    ///
    /// Beyond `unsafe`, the mint is **sealed behind the `launcher-mint` feature**
    /// (see `# Sealing` below), so untrusted engine crates cannot even name it —
    /// the machine-checked no-mint-reachability boundary (§5.4), not merely
    /// trusted-launcher discipline.
    ///
    /// # Safety
    ///
    /// The caller MUST be the trusted launcher / process entry point, and MUST
    /// call this AT MOST ONCE, before any untrusted input is processed. Granting
    /// or leaking the returned `Authority` (or caps minted from it) to untrusted
    /// code defeats the entire capability model.
    ///
    /// # Sealing — no-mint-reachability (ATERM_DESIGN §5.4)
    ///
    /// This constructor is **sealed behind the `launcher-mint` cargo feature** (or
    /// the crate's own `test` cfg): only the trusted launcher binaries (`aterm-gui`,
    /// `aterm-cli`) enable that feature, so for every other crate the symbol does not
    /// exist and **cannot be named — even inside an `unsafe` block**. That is the
    /// no-mint-reachability seal: no parser / control handler / extension path can
    /// reach a mint, because the mint is not in its compiled surface. It stays
    /// `unsafe` (the audit marker) and `#[doc(hidden)]` (an unwelcoming, undiscoverable
    /// shape), mirroring `aterm_provenance`'s `__new_for_capability_only` seal.
    ///
    /// The property is proven abstractly by
    /// [`aterm_spec::derive::mint_reachability_model`] (invariant `NoUntrustedTop`,
    /// model-checked by `ty`) and bound to the real tree by the
    /// `mint_sites_are_launcher_only` source-scan conformance gate: exactly the two
    /// launcher binaries enable `launcher-mint`, and no library/engine crate reaches
    /// the mint in production code.
    #[cfg(any(test, feature = "launcher-mint"))]
    #[doc(hidden)]
    #[must_use]
    pub unsafe fn root_authority() -> Authority {
        Authority { _seal: Seal }
    }

    /// Mint a capability for effect `E` at `tier`.
    #[must_use]
    pub fn grant<E>(&self, tier: Tier) -> Cap<E> {
        Cap {
            tier,
            _effect: PhantomData,
            _seal: Seal,
        }
    }
}

/// The effect-gate. A privileged operation calls `require(cap, min)`; it returns
/// `Ok(())` only if `cap` meets the minimum tier, so the operation cannot proceed
/// without an adequately-tiered capability (and, by the type, without a capability
/// *for that effect* at all). Returns the shortfall on denial.
///
/// # Errors
/// Returns [`Denied`] when `cap.tier() < min`.
pub fn require<E>(cap: &Cap<E>, min: Tier) -> Result<(), Denied> {
    if cap.satisfies(min) {
        Ok(())
    } else {
        Err(Denied {
            have: cap.tier(),
            need: min,
        })
    }
}

/// An effect was denied because the presented capability's tier was too low.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Denied {
    pub have: Tier,
    pub need: Tier,
}

impl std::fmt::Display for Denied {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("capability denied: have ")?;
        f.write_str(self.have.name())?;
        f.write_str(", need at least ")?;
        f.write_str(self.need.name())
    }
}
impl std::error::Error for Denied {}

/// Zero-size effect markers naming a privileged effect a [`Cap`] authorizes. A
/// function performing the effect takes `&Cap<ThisEffect>`.
pub mod effects {
    /// Authorizes spawning a child process (the PTY shell). The single spawn seam
    /// (`aterm-pty`) should require `Cap<Spawn>`.
    pub enum Spawn {}
    /// Authorizes writing the filesystem (e.g. scrollback persistence).
    pub enum FsWrite {}
    /// Authorizes touching the system clipboard (OSC 52).
    pub enum Clipboard {}
    /// Authorizes opening a network socket.
    pub enum Network {}

    // --- hierarchical-session edges (design docs/design/HIERARCHICAL_SESSIONS.md
    // §7.1, A.6). These are the COARSE compile-time class gate; the fine, per-edge
    // object identity (which src→dst) is the runtime `aterm-session` EdgeToken. The
    // intended minimum tier is enforced by `require(cap, min)` at the call site
    // (as `Spawn` is), not baked into the marker. Cross-session `WriteInput` from
    // untrusted in-process code stays compile-gated OFF until ROADMAP §5.4 is GREEN
    // (§7.7); same-uid cross-process driving rides the runtime EdgeToken, sound
    // independent of §5.4.

    /// Authorizes READING another session's rendered surface (screen/cells/blocks/
    /// scrollback/search/image/timeline). Intended minimum tier: `Untrusted`.
    pub enum ReadScreen {}
    /// Authorizes INJECTING input into another session — the full human vocabulary
    /// (keys, mouse click/drag/wheel, selection, paste, resize, focus), all of which
    /// converge on the one `App::input` seam (Addendum A.2). Intended minimum tier:
    /// `Trusted`.
    pub enum WriteInput {}
    /// Authorizes sending a SIGNAL to another session's foreground process group
    /// (a separate scope from `WriteInput`: a human's Ctrl-C is a byte, not an
    /// out-of-band signal — §7.2). Intended minimum tier: `Trusted`.
    pub enum SignalEdge {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use effects::{Clipboard, Spawn};

    // Test helper: mint the root authority. SAFETY: in a single-threaded test
    // process with no untrusted input, the trusted-launcher contract of
    // `root_authority` is trivially satisfied.
    fn test_authority() -> Authority {
        unsafe { Authority::root_authority() }
    }

    #[test]
    fn granted_cap_carries_its_tier() {
        let authority = test_authority();
        let spawn: Cap<Spawn> = authority.grant(Tier::Trusted);
        assert_eq!(spawn.tier(), Tier::Trusted);
        assert!(spawn.satisfies(Tier::Untrusted));
        assert!(spawn.satisfies(Tier::Trusted));
        assert!(!spawn.satisfies(Tier::Certified));
    }

    #[test]
    fn require_gates_on_tier() {
        let authority = test_authority();
        let clip: Cap<Clipboard> = authority.grant(Tier::Untrusted);
        // An effect needing Trusted is denied to an Untrusted cap.
        assert_eq!(
            require(&clip, Tier::Trusted),
            Err(Denied {
                have: Tier::Untrusted,
                need: Tier::Trusted
            })
        );
        // An effect needing Untrusted is allowed.
        assert_eq!(require(&clip, Tier::Untrusted), Ok(()));

        let certified: Cap<Clipboard> = authority.grant(Tier::Certified);
        assert_eq!(require(&certified, Tier::Trusted), Ok(()));
    }

    // Demonstrates the effect layer: a privileged op that is impossible to call
    // without a Cap<Spawn> of sufficient tier. (The no-STRUCT-FORGERY property is
    // a compile-time fact — a `Cap { .. }` literal does not compile outside the
    // crate because the `_seal` field is private. This is NOT the stronger
    // no-mint-reachability claim, which is ROADMAP §5.4 and not yet delivered;
    // root authority is still reachable via the `unsafe` trusted-launcher mint.)
    #[test]
    fn effect_gated_operation() {
        fn privileged_spawn(cap: &Cap<Spawn>) -> Result<&'static str, Denied> {
            require(cap, Tier::Trusted)?;
            Ok("spawned")
        }
        let authority = test_authority();
        let ok: Cap<Spawn> = authority.grant(Tier::Trusted);
        let weak: Cap<Spawn> = authority.grant(Tier::Untrusted);
        assert_eq!(privileged_spawn(&ok), Ok("spawned"));
        assert!(privileged_spawn(&weak).is_err());
    }

    #[test]
    fn caps_are_copyable_witnesses() {
        let authority = test_authority();
        let c: Cap<Spawn> = authority.grant(Tier::Trusted);
        let d = c; // Copy, not move
        assert_eq!(c.tier(), d.tier());
    }

    // Acceptance (CAP-1): minting the root authority is ONLY reachable through
    // the `unsafe` audited entry. The mint compiles and works inside an `unsafe`
    // block; there is no safe public path. A SAFE call —
    //   `let _ = Authority::root_authority();`
    // — does NOT compile (it requires `unsafe`), which is the whole point; we
    // assert the unsafe path here and document that the safe path is a type error.
    // The hierarchical-session edge markers gate exactly like `Spawn`: the marker
    // is the effect class, `require(cap, min)` is the tier floor. WriteInput/Signal
    // floor at Trusted; ReadScreen floors at Untrusted (a read is the least power).
    #[test]
    fn session_edge_effects_gate_on_their_intended_tier() {
        use effects::{ReadScreen, SignalEdge, WriteInput};
        let authority = test_authority();

        let untrusted_write: Cap<WriteInput> = authority.grant(Tier::Untrusted);
        assert!(
            require(&untrusted_write, Tier::Trusted).is_err(),
            "WriteInput floors at Trusted"
        );
        let trusted_write: Cap<WriteInput> = authority.grant(Tier::Trusted);
        assert_eq!(require(&trusted_write, Tier::Trusted), Ok(()));

        let untrusted_signal: Cap<SignalEdge> = authority.grant(Tier::Untrusted);
        assert!(
            require(&untrusted_signal, Tier::Trusted).is_err(),
            "SignalEdge floors at Trusted"
        );

        // A read is the least-power edge: an Untrusted cap suffices.
        let read: Cap<ReadScreen> = authority.grant(Tier::Untrusted);
        assert_eq!(require(&read, Tier::Untrusted), Ok(()));
    }

    #[test]
    fn root_authority_requires_unsafe_and_then_grants() {
        // SAFETY: single-threaded test, no untrusted input — trusted-launcher
        // contract trivially holds.
        let authority = unsafe { Authority::root_authority() };
        let spawn: Cap<Spawn> = authority.grant(Tier::Certified);
        assert_eq!(spawn.tier(), Tier::Certified);
    }
}
