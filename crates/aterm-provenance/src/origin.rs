// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The sealed [`Origin`] trait and its one marker, [`Pty`].

mod sealed {
    /// Sealing trait for [`super::Origin`]: downstream crates cannot add origins.
    pub trait Sealed {}
}

/// Compile-time origin tag. Sealed; adding an origin is a framework-level action.
///
/// [`Pty`] is the only origin, so a tainted value has no other label to be
/// relabelled to, and no crate outside this one can add one
/// (`docs/HARDCORE_BACKLOG.md` P4):
///
/// ```
/// use aterm_provenance::{Provenance, Pty};
/// // Control: the one origin, constructed the one way.
/// let pty = Provenance::<_, Pty>::from_pty(b"rm -rf /".to_vec());
/// # let _ = pty;
/// ```
///
/// ```compile_fail,E0277
/// use aterm_provenance::Origin;
/// #[derive(Clone, Copy)]
/// struct Host;
/// impl Origin for Host {}
/// ```
/// use aterm_provenance::{Provenance, Pty};
/// // Control: the one origin, constructed the one way.
pub trait Origin: sealed::Sealed + 'static + Copy {}

/// PTY origin marker. Bytes from the shell — the primary adversary surface.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Pty;

impl sealed::Sealed for Pty {}
impl Origin for Pty {}
