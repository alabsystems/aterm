// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What a Mac's own join of ALab says, and how long it asks the members by itself, on every
//! platform: the `members` module runs the join where it is built (unix), and System State's
//! page, built everywhere, words its Secrets line and its card by these.
//!
//! What lets an asking Mac in follows where it is ([`JoinPlace`], from its network place): the
//! members' automatic admission lets a Mac in only from the home ALab LAN, so there the words say
//! automatic; a Mac that holds the fleet's book proves itself to each member it reaches, which
//! lets an ALab Mac back in by itself anywhere; and away from that LAN the words say what does let
//! it in (a member's Allow and `aterm members join` on this Mac, or its return to that LAN). Being
//! away is words only: no alert and nothing for the person to answer.

/// Where a Mac that asks to join ALab is, for its join's words ([`Self::of`] its network place).
/// It words a report and decides nothing: the members' admission reads its own proof.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JoinPlace {
    /// On the ALab LAN: the members' automatic admission can let this Mac in.
    Home,
    /// Off the ALab LAN, or where the place cannot tell, holding the fleet's book (its members
    /// known to the place: this Mac was in the fleet): it proves itself to each member it reaches,
    /// over the fleet or on a member's own port, which lets an ALab Mac back in by itself
    /// (re-admission by proof, `crate::client::readmit`), wherever it is.
    Proves,
    /// Where that admission never lets a Mac in and it holds no fleet book: a network the place
    /// knows is not the ALab LAN, no network, a Personal Hotspot or a cellular link.
    Away,
    /// Not known: a plain LAN or a tunnel with no ALab LAN to compare it with (a Mac that was
    /// never a member has no fleet book), a network the place is still checking, a remembered
    /// ALab LAN whose members fell silent, or no place read at all.
    Unknown,
}

impl JoinPlace {
    /// Where `place` puts an asking Mac.
    #[must_use]
    pub const fn of(place: &aterm_netplace::Place) -> Self {
        use aterm_netplace::{Kind, Where};
        let proves = matches!(place.known, Some(known) if known > 0);
        match place.at {
            Where::AlabLan => Self::Home,
            _ if proves => Self::Proves,
            Where::Remote(_) if place.once_alab => Self::Unknown,
            Where::Remote(_)
            | Where::Offline
            | Where::Network(Kind::Hotspot { .. } | Kind::Cellular) => Self::Away,
            Where::Network(Kind::Lan | Kind::Vpn) | Where::Checking(_) => Self::Unknown,
        }
    }

    /// Where this Mac is now ([`Self::of`] its network place under this process's state root,
    /// which reads the route with no packet); [`Self::Unknown`] with no state root, and off unix,
    /// where no network place is kept.
    #[must_use]
    pub fn here() -> Self {
        #[cfg(unix)]
        {
            aterm_types::dirs::state_dir().map_or(Self::Unknown, |state| {
                Self::of(&aterm_netplace::read(&state))
            })
        }
        #[cfg(not(unix))]
        {
            Self::Unknown
        }
    }
}

/// Status while this Mac, on the ALab LAN, waits for a member there to let it in on its proof
/// ([`let_in`]).
pub const LET_IN: &str = "waiting for automatic ALab admission; aterm keeps retrying";

/// Status while the join is pending off the ALab LAN where this Mac holds the fleet's book
/// ([`let_in`]).
pub const LET_IN_PROVES: &str = "this Mac proves itself to each ALab member it reaches, which lets \
                                 an ALab Mac back in by itself, and aterm keeps retrying; failing \
                                 that, a member's Allow and `aterm members join` here do, or its \
                                 return to the ALab LAN";

/// Status while the join is pending away from the ALab LAN ([`let_in`]).
pub const LET_IN_AWAY: &str = "off the ALab LAN nothing lets this Mac in by itself: a member's \
                               Allow and `aterm members join` here do, or its return to that LAN";

/// Status while the join is pending where the place cannot say whether this is the ALab LAN
/// ([`let_in`]).
pub const LET_IN_UNKNOWN: &str = "automatic ALab admission lets this Mac in on the ALab LAN, \
                                  and aterm keeps retrying; elsewhere a member's Allow and \
                                  `aterm members join` here do";

/// What lets a Mac that asked in, where it is (`at`): `aterm members status`, `join --no-wait`,
/// `aterm init` and `sync.peers` say it.
#[must_use]
pub const fn let_in(at: JoinPlace) -> &'static str {
    match at {
        JoinPlace::Home => LET_IN,
        JoinPlace::Proves => LET_IN_PROVES,
        JoinPlace::Away => LET_IN_AWAY,
        JoinPlace::Unknown => LET_IN_UNKNOWN,
    }
}

/// System State's Secrets row note while the join is pending off the ALab LAN where this Mac
/// holds the fleet's book ([`asking_note`]).
pub const ASKING_PROVES_NOTE: &str = "This Mac proves itself to each ALab member it reaches, \
                                      which lets an ALab Mac back in by itself; failing that, a \
                                      member's Allow and aterm members join here do, or its \
                                      return to the ALab LAN";

/// System State's Secrets row note while the join is pending away from the ALab LAN
/// ([`asking_note`]).
pub const ASKING_AWAY_NOTE: &str = "Off the ALab LAN nothing lets this Mac in by itself: a \
                                    member's Allow and aterm members join here do, or its \
                                    return to that LAN";

/// System State's Secrets row note while the join is pending where the place cannot say
/// whether this is the ALab LAN ([`asking_note`]).
pub const ASKING_UNKNOWN_NOTE: &str = "Joining ALab automatically on the ALab LAN, and aterm \
                                       keeps retrying; elsewhere a member's Allow and aterm \
                                       members join here let this Mac in";

/// System State's Secrets row note (and `aterm doctor`'s) while this Mac's join is pending,
/// where it is (`at`): on the ALab LAN [`aterm_types::macos_setup::VAULT_ASKING_NOTE`].
#[must_use]
pub const fn asking_note(at: JoinPlace) -> &'static str {
    match at {
        JoinPlace::Home => aterm_types::macos_setup::VAULT_ASKING_NOTE,
        JoinPlace::Proves => ASKING_PROVES_NOTE,
        JoinPlace::Away => ASKING_AWAY_NOTE,
        JoinPlace::Unknown => ASKING_UNKNOWN_NOTE,
    }
}

/// The initial observation interval for an automatic ALab join. Passing it changes
/// diagnostic state, not the admission policy: the join continues without an Allow prompt.
pub const JOIN_QUIET: std::time::Duration = std::time::Duration::from_secs(30);

/// What a join that asked the members says while it asks by itself ([`JOIN_QUIET`]).
pub const JOINING: &str = "Joining ALab…";

#[cfg(test)]
#[path = "alab_join_tests.rs"]
mod tests;
