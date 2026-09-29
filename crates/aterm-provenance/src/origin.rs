// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The six origins: [`Origin`] sealed trait, [`OriginTag`] runtime mirror, and
//! the 6 marker types.

mod sealed {
    /// Sealing trait for [`super::Origin`]. Prevents downstream crates from
    /// adding origin variants; adding one means a new [`super::OriginTag`]
    /// discriminant.
    pub trait Sealed {}
}

/// Compile-time origin tag. `Origin` is sealed; adding a variant is a
/// framework-level action.
///
/// Every valid origin marker type exposes its runtime tag via
/// [`Origin::TAG`], which [`crate::Provenance::tag`] reads.
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

/// Runtime-shaped mirror of [`Origin`], as [`crate::Provenance::tag`] answers it.
///
/// The discriminants are pinned (`tests/unit.rs`,
/// `origin_tag_discriminants_are_stable`); nothing persists them today.
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
