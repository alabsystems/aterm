// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The 6-element origin lattice: [`Origin`] sealed trait, [`OriginTag`]
//! runtime mirror, and the 6 marker types. See
//! `designs/2026-04-19-provenance-framework.md` §3.

mod sealed {
    /// Sealing trait for [`super::Origin`]. Prevents downstream crates from
    /// adding origin variants; adding an origin is a framework-level ceremony
    /// (update §3 Hasse diagram, update join table in `build.rs`, update the
    /// TLA+ spec, bump checkpoint schema version).
    pub trait Sealed {}
}

/// Compile-time origin tag. `Origin` is sealed; adding a variant is a
/// framework-level action (see §3 Hasse diagram).
///
/// Every valid origin marker type exposes its runtime tag via
/// [`Origin::TAG`], which is used when projecting to a
/// [`crate::DynProvenance`].
pub trait Origin: sealed::Sealed + 'static + Copy {
    /// Runtime representation of this origin.
    const TAG: OriginTag;

    /// Returns [`Origin::TAG`] by value.
    ///
    /// Every sealed impl returns its own tag as a monomorphic constant, so
    /// this is behaviorally identical to reading `Self::TAG`. It exists so
    /// generic code (e.g. [`crate::Provenance::tag`]) carries a call in its
    /// polymorphic MIR instead of an unevaluated `OriginTag`-typed constant,
    /// which the Trust L0 verifier refuses to model pre-monomorphization.
    #[must_use]
    fn runtime_tag() -> OriginTag;
}

/// Runtime-shaped mirror of [`Origin`]. Stored in [`crate::DynProvenance`]
/// and in per-row grid metadata (Phase 2).
///
/// Discriminants are stable at-rest: checkpoint v4 uses these byte values
/// directly (see design §5.1).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum OriginTag {
    /// Bytes minted by the host app (aTerm.app / aterm-alacritty). Max trust.
    Host = 0,
    /// Bytes from aterm.toml / profile JSON / MCP server allowlist.
    ConfigFile = 1,
    /// Bytes typed live by the user through the input controller.
    User = 2,
    /// Bytes from the local AI predictor, MCP tools, or voice narration.
    Ai = 3,
    /// Bytes over out-of-band network channels (mosh UDP, ssh agent metadata,
    /// LSP responses).
    NetworkUntrusted = 4,
    /// Bytes from the shell or any program within it — the primary adversary
    /// surface.
    Pty = 5,
}

impl OriginTag {
    /// Enumerates every valid origin tag. Useful for exhaustive tests.
    #[must_use]
    pub const fn all() -> [OriginTag; 6] {
        [
            OriginTag::Host,
            OriginTag::ConfigFile,
            OriginTag::User,
            OriginTag::Ai,
            OriginTag::NetworkUntrusted,
            OriginTag::Pty,
        ]
    }

    /// Returns the byte representation of this tag.
    ///
    /// Used by the FFI bridge (`aterm_grid_cell_origin` in Phase 2) and by
    /// checkpoint serialization (§5.1). Guaranteed stable across versions.
    #[must_use]
    pub const fn as_u8(self) -> u8 {
        self as u8
    }
}

// --- origin marker types ---------------------------------------------------

/// Host-origin marker. Bytes minted by the host app itself.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Host;

/// Config-file origin marker. Bytes read from on-disk configuration.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct ConfigFile;

/// User origin marker. Bytes typed live by the user.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct User;

/// AI origin marker. Bytes from the AI predictor, MCP tools, or voice.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Ai;

/// Network-untrusted origin marker. Bytes from out-of-band network channels.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct NetworkUntrusted;

/// PTY origin marker. Bytes from the shell — the primary adversary surface.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Pty;

impl sealed::Sealed for Host {}
impl Origin for Host {
    const TAG: OriginTag = OriginTag::Host;
    fn runtime_tag() -> OriginTag {
        OriginTag::Host
    }
}
impl sealed::Sealed for ConfigFile {}
impl Origin for ConfigFile {
    const TAG: OriginTag = OriginTag::ConfigFile;
    fn runtime_tag() -> OriginTag {
        OriginTag::ConfigFile
    }
}
impl sealed::Sealed for User {}
impl Origin for User {
    const TAG: OriginTag = OriginTag::User;
    fn runtime_tag() -> OriginTag {
        OriginTag::User
    }
}
impl sealed::Sealed for Ai {}
impl Origin for Ai {
    const TAG: OriginTag = OriginTag::Ai;
    fn runtime_tag() -> OriginTag {
        OriginTag::Ai
    }
}
impl sealed::Sealed for NetworkUntrusted {}
impl Origin for NetworkUntrusted {
    const TAG: OriginTag = OriginTag::NetworkUntrusted;
    fn runtime_tag() -> OriginTag {
        OriginTag::NetworkUntrusted
    }
}
impl sealed::Sealed for Pty {}
impl Origin for Pty {
    const TAG: OriginTag = OriginTag::Pty;
    fn runtime_tag() -> OriginTag {
        OriginTag::Pty
    }
}

/// Synthetic `Top` sentinel type. Deliberately does **not** implement
/// [`Origin`] — `Provenance<T, Top>` must not compile. (Asserted by the
/// `top_is_not_origin` compile-fail test.)
///
/// `Top` exists only so that diagnostic / TLA+-facing code can name the
/// element; its runtime analogue is [`crate::TOP_TAG_U8`] in
/// [`crate::DynProvenance`].
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Top;

/// Synthetic tag value representing the lattice's `Top` element (mixed
/// incomparable; cannot be lifted). See §3.2.
///
/// This is not a variant of [`OriginTag`] because `Top` is never a valid
/// static origin — it appears only in `DynProvenance` diagnostics and in the
/// TLA+ spec.
pub const TOP_TAG_U8: u8 = 0xFF;
