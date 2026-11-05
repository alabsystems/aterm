// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE AGENT'S LEFT ROWS AT A YIELD (`docs/DESIGN-leftovers-by-default-2026-10-30.md` §2): the
//! processes this session's agent started that still run, as the host read and re-proved them
//! where the repository is read ([`crate::supervise::IdleHost::leftover`]), and the word a
//! landing's done line says of them ([`LeftWord`]). Under Land a `Nothing left:` defense lands
//! only once each of them was ended (`aterm ctl end <token>`) or kept (a `keep <token>` line under
//! the defense); a view that cannot say holds nothing.

use aterm_uds::process::Identity;

/// WHAT THE HOST READ of this session's agent's left rows at one yield.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum LeftRead {
    /// Nothing was asked: no host, a host or a tape without the door, or a point where nothing
    /// is read.
    #[default]
    NotAsked,
    /// The view could not say: none was read, or a partial one (its sweep stopped at its cap, or
    /// its reading at its bound of new candidates). It holds nothing.
    Unchecked,
    /// The agent's left rows, each re-proved at the yield: the identity `sys left` prints as a
    /// row's `end=` token.
    Rows(Vec<Identity>),
}

impl LeftRead {
    /// The rows read, none where none were.
    #[must_use]
    pub fn rows(&self) -> &[Identity] {
        match self {
            Self::Rows(rows) => rows,
            Self::NotAsked | Self::Unchecked => &[],
        }
    }

    /// The rows no token `kept` keeps.
    pub fn unkept<'a>(
        &'a self,
        kept: impl Fn(&Identity) -> bool + 'a,
    ) -> impl Iterator<Item = &'a Identity> + 'a {
        self.rows().iter().filter(move |row| !kept(row))
    }
}

/// The answer of a read whose thread could not start, or that panicked: the view could not say.
impl crate::harness::repo_work::ProbeAnswer for LeftRead {
    fn unread() -> Self {
        Self::Unchecked
    }
}

/// A token as `sys left` prints it, in either kernel's shape: `<pid>@<birth>:<idversion>` or
/// `<pid>@<birth>` ([`Identity::parse_shaped`]); `None` for any other word.
#[must_use]
pub fn token_of(word: &str) -> Option<Identity> {
    Identity::parse_shaped(word, word.contains(':'))
}

/// THE DONE LINE'S `left=` WORD: what the landing's yield read of the agent's left rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeftWord {
    /// None was left (`left=0`).
    None,
    /// None was left but those the defenses kept (`left=kept:<n>`).
    Kept(u16),
    /// This many still ran, unkept, once the leg had held its most (`left=<n>`).
    Left(u16),
    /// The view could not say (`left=unchecked`).
    Unchecked,
}

impl LeftWord {
    /// The word of `read` at a landing, `kept` the tokens the episode's defenses kept: `None`
    /// where nothing was asked.
    #[must_use]
    pub fn of(read: &LeftRead, kept: impl Fn(&Identity) -> bool) -> Option<Self> {
        let count = |n: usize| u16::try_from(n).unwrap_or(u16::MAX);
        match read {
            LeftRead::NotAsked => None,
            LeftRead::Unchecked => Some(Self::Unchecked),
            LeftRead::Rows(rows) => {
                let held = rows.iter().filter(|row| kept(row)).count();
                Some(match (rows.len() - held, held) {
                    (0, 0) => Self::None,
                    (0, held) => Self::Kept(count(held)),
                    (left, _) => Self::Left(count(left)),
                })
            }
        }
    }

    /// Its word on the done line (`0`, `kept:<n>`, `<n>`, `unchecked`).
    #[must_use]
    pub fn word(self) -> String {
        match self {
            Self::None => "0".to_owned(),
            Self::Kept(n) => format!("{KEPT}{n}"),
            Self::Left(n) => n.to_string(),
            Self::Unchecked => UNCHECKED.to_owned(),
        }
    }

    /// The word `word` is ([`Self::word`]'s inverse): `None` for any other, a count with a sign,
    /// a leading zero or past `u16`, and `kept:0`.
    #[must_use]
    pub fn of_word(word: &str) -> Option<Self> {
        let count = |text: &str| {
            (!text.is_empty()
                && text.bytes().all(|b| b.is_ascii_digit())
                && (text == "0" || !text.starts_with('0')))
            .then(|| text.parse::<u16>().ok())
            .flatten()
        };
        if word == UNCHECKED {
            return Some(Self::Unchecked);
        }
        if let Some(n) = word.strip_prefix(KEPT) {
            return count(n).filter(|&n| n > 0).map(Self::Kept);
        }
        count(word).map(|n| if n == 0 { Self::None } else { Self::Left(n) })
    }
}

const KEPT: &str = "kept:";

const UNCHECKED: &str = "unchecked";

#[cfg(test)]
#[path = "leftover_tests.rs"]
mod tests;
