// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A machine's fleet access level: the ceiling on what another member of its fleet may do to
//! its sessions over aterm's fleet port (`docs/DESIGN-fleet-access-lock-2026-11-10.md`).
//!
//! A level is never a grant: `[fleet] allow` and its drive grants say who is a trusted driver,
//! and the level says what a trusted driver may do now. Each remote act has a [`DriveKind`], each
//! kind [`DriveKind::needs`] a level, and an act is admitted where the machine's level is at
//! least that ([`FleetAccess::admits`]).

/// The state folder holding one file per worker another Mac started here, named by the worker's
/// sid and holding that Mac's node: the driven window writes it, and `aterm fleet access` reads
/// it to refuse a raise asked from inside such a worker.
pub const DRIVE_WORKERS: &str = "drive-workers";

/// How long a level rests before what cannot be taken back follows it. A raise the menu-bar
/// lock's click asked for is written once the icon has rested on it this long, and the holds
/// other Macs placed here are lifted once a drop below Read and Write has. A refusal never
/// waits: a lower level binds at once.
pub const SETTLE: std::time::Duration = std::time::Duration::from_secs(2);

/// The state file holding this machine's level (`aterm_agent::fleet_access` reads and writes
/// it). It is named here because the fleet's key writer removes it: a level is chosen for one
/// fleet, and a machine that comes to hold another fleet's key starts that fleet at Read Only
/// (`aterm_net::fleet_member::HeldBook::write_key`).
pub const FILE: &str = "fleet-access";

/// Write an enum and its `ALL` from one list of variants, so a variant is in both or in
/// neither: a sweep over `ALL` misses none, with no test to police a second list.
macro_rules! listed {
    (
        $(#[$meta:meta])*
        pub enum $name:ident {
            $($(#[$doc:meta])* $variant:ident,)+
        }
    ) => {
        $(#[$meta])*
        pub enum $name {
            $($(#[$doc])* $variant,)+
        }

        impl $name {
            /// Every one, in the order the enum names them.
            pub const ALL: [Self; [$($name::$variant),+].len()] = [$(Self::$variant),+];
        }
    };
}

listed! {
    /// What the other machines of a fleet may do to this one, in rising order.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
    pub enum FleetAccess {
        /// Other Macs read this one; none starts a job, sends a message or types into a session.
        #[default]
        ReadOnly,
        /// Other Macs also message, type into, take over and close the sessions already here;
        /// none starts a new one.
        ReadWrite,
        /// Other Macs also start new jobs and tabs here.
        Full,
    }
}

impl FleetAccess {
    /// The level's word on the command line, in its state file and on the wire.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::ReadOnly => "read",
            Self::ReadWrite => "write",
            Self::Full => "full",
        }
    }

    /// The level [`Self::word`] names: exactly the three lowercase words.
    #[must_use]
    pub fn from_word(word: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|level| level.word() == word)
    }

    /// The level a survey header's `access=` token says another Mac is at. A header without the
    /// token is an aterm from before the lock, which enforces none: full. A word this build does
    /// not know reads as the lowest level, so nothing is offered that the Mac may refuse.
    #[must_use]
    pub fn of_header(token: Option<&str>) -> Self {
        token.map_or(Self::Full, |word| {
            Self::from_word(word).unwrap_or(Self::ReadOnly)
        })
    }

    /// The level's name in a menu.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::ReadOnly => "Read Only",
            Self::ReadWrite => "Read and Write, No New Tabs",
            Self::Full => "Full Access",
        }
    }

    /// The level's short name, for a title flash and a tooltip.
    #[must_use]
    pub const fn short(self) -> &'static str {
        match self {
            Self::ReadOnly => "Read Only",
            Self::ReadWrite => "Read and Write",
            Self::Full => "Full Access",
        }
    }

    /// The level one click on: Read Only, Read and Write, Full Access, and round to Read Only.
    #[must_use]
    pub const fn rotated(self) -> Self {
        match self {
            Self::ReadOnly => Self::ReadWrite,
            Self::ReadWrite => Self::Full,
            Self::Full => Self::ReadOnly,
        }
    }

    /// Whether a machine at this level admits an act of `kind` from a driver its grants trust.
    #[must_use]
    pub fn admits(self, kind: DriveKind) -> bool {
        self >= kind.needs()
    }

    /// The refusal a machine at this level answers an act of `kind` with, or `None` where the
    /// level admits it: [`crate::control_verbs::DRIVE_LOCKED`], the level in force, `needs=` the
    /// level the act needs, and why in words. It names no word of the refused line and ends in no
    /// newline; its caller adds both.
    #[must_use]
    pub fn refusal(self, kind: DriveKind) -> Option<String> {
        if self.admits(kind) {
            return None;
        }
        let needs = kind.needs();
        let what = match self {
            Self::ReadOnly => {
                "another Mac reads it, and starts no job, sends no message and types into no \
                 session here"
            }
            Self::ReadWrite | Self::Full => {
                "another Mac drives the sessions already here and starts none"
            }
        };
        Some(format!(
            "{head}{level} needs={needs}: this Mac's fleet access is {label}: {what}, until its \
             person raises it (the lock in its menu bar, or `aterm fleet access {needs}` there)",
            head = crate::control_verbs::DRIVE_LOCKED,
            level = self.word(),
            needs = needs.word(),
            label = self.label(),
        ))
    }

    /// The level in force and the level needed that a [`Self::refusal`] in `reply` names, or
    /// `None` where `reply` is no such refusal.
    #[must_use]
    pub fn refused_at(reply: &str) -> Option<(Self, Self)> {
        let rest = reply.strip_prefix(crate::control_verbs::DRIVE_LOCKED)?;
        let (level, rest) = rest.split_once(' ')?;
        let needs = rest.strip_prefix("needs=")?;
        let needs = needs
            .split(|c: char| c == ':' || c.is_whitespace())
            .next()?;
        Some((Self::from_word(level)?, Self::from_word(needs)?))
    }
}

impl std::fmt::Display for FleetAccess {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.word())
    }
}

listed! {
    /// What one act of another fleet Mac does here: the class the lock judges it by.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    pub enum DriveKind {
        /// A read: a survey, a board, a session's status, text, screen, result or inbox, a wait.
        Read,
        /// A bounded read of the plain shell the driver itself opened and nobody touched since.
        ShellRead,
        /// This Mac's vault admin service judging a request (a fleet service).
        Vault,
        /// One fixed service-repair operation (a fleet service).
        ServiceRepair,
        /// This Mac's bounded account renewal pass (a fleet service).
        AccountsRenew,
        /// Mail to a session here.
        Post,
        /// Marking a reply acknowledged in this Mac's mail store.
        MailSeen,
        /// A prompt typed into a session and submitted.
        Turn,
        /// A key pressed in a session.
        Key,
        /// Text typed into a session.
        Send,
        /// Taking a session over, or handing it back.
        Hold,
        /// Closing a session.
        Close,
        /// Claiming a folder for one of the driver's requests (`request claim path=…`): it
        /// widens what a later cleanup may delete, and opens nothing.
        Claim,
        /// The mesh's `bus.log` stream, its cross-Mac message path.
        BusLog,
        /// Starting a worker or a plain shell: a new tab, and a window where none is open.
        Spawn,
    }
}

impl DriveKind {
    /// The lowest level that admits this kind. Every kind is named: a new one does not compile
    /// until it is given a level.
    #[must_use]
    pub const fn needs(self) -> FleetAccess {
        match self {
            Self::Read | Self::ShellRead => FleetAccess::ReadOnly,
            // The three fleet services run fixed implementations under their own authorization
            // and first-run setup needs them unattended: the lock does not govern them.
            Self::Vault | Self::ServiceRepair | Self::AccountsRenew => FleetAccess::ReadOnly,
            Self::Post
            | Self::MailSeen
            | Self::Turn
            | Self::Key
            | Self::Send
            | Self::Hold
            | Self::Close
            | Self::Claim
            | Self::BusLog => FleetAccess::ReadWrite,
            Self::Spawn => FleetAccess::Full,
        }
    }

    /// The kind's word in a refusal and a record: the verb of the line that carries it.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::ShellRead => "shell-read",
            Self::Vault => "vault",
            Self::ServiceRepair => "service-repair",
            Self::AccountsRenew => "accounts-renew",
            Self::Post => "post",
            Self::MailSeen => "inbox seen",
            Self::Turn => "turn",
            Self::Key => "key",
            Self::Send => "send",
            Self::Hold => "hold",
            Self::Close => "close",
            Self::Claim => "request claim",
            Self::BusLog => "bus.log",
            Self::Spawn => "spawn",
        }
    }
}

#[cfg(test)]
#[path = "fleet_access_tests.rs"]
mod tests;
