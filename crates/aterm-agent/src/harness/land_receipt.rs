// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE LANDING'S RECEIPT (`docs/DESIGN-harness-autopilot-2026-10-15.md` §14.9, slice 9b): what
//! aterm itself checked at a landing, apart from what the agent said. The probe's reading at the
//! point that landed names the repository, its `HEAD` and the remote main it was counted against
//! ([`RepoRead::Checked`]); the agent's defense line and the lines after it say what it claimed;
//! and it says whether the agent answered aterm's land check (`land-check@v1`, §14.17 item 5,
//! slice 13a) `LANDED`, the fact the words of what the agent said are chosen by
//! (`crate::supervise::mode_words::said_words`).
//!
//! The loop tells the receipt to its host before the landed face (`IdleHost::land_receipt`) and
//! journals it as a `RECEIPT` line right after the landing's `DONE` note ([`LandReceipt::line`]),
//! which a restarted or successor loop reads back with the `DONE` note's seq
//! ([`LandReceipt::of_line`]). Nothing the loop decides reads it. Its words stay in-process
//! (limit 5): the journal, and, from slice 9c, the window's surfaces
//! (`crate::supervise::mode_words::checked_words`), `status` and the task record's `checked=`
//! ([`LandReceipt::checked_word`]).

use super::acts::FACT_SEPARATOR;
use super::repo_work::RepoRead;
use crate::supervise::policy::land_check::RULE_LAND_CHECK;
use crate::supervise::policy::turn_end::RULE_LAND;

/// How many hex digits of `HEAD` a receipt names, on every surface.
pub const HEAD_DIGITS: usize = 12;

/// The longest receipt `checked=` token: a head, `@`, and the probe's printable ref,
/// with every byte of each maximum-width Unicode scalar expanded to `%XX`.
pub const CHECKED_WORD_BYTES: usize =
    HEAD_DIGITS + 1 + super::repo_work::PATH_CHARS * char::MAX.len_utf8() * 3;

/// Whether a task/result `checked=` token is one bounded canonical receipt word.
/// The ref uses the producer's percent encoding, preserving valid Unicode without
/// accepting malformed escapes, invalid UTF-8 or encoded control characters.
#[must_use]
pub fn valid_checked_word(word: &str) -> bool {
    if word.len() > CHECKED_WORD_BYTES || !word.bytes().all(|byte| byte.is_ascii_graphic()) {
        return false;
    }
    if ReceiptLeg::of_word(word).is_some_and(|leg| leg != ReceiptLeg::OnMain) {
        return true;
    }
    let Some((head, encoded)) = word.split_once('@') else {
        return false;
    };
    if head.len() != HEAD_DIGITS || !is_head(head) || encoded.is_empty() {
        return false;
    }
    let decoded = aterm_codec::pct::decode(encoded);
    !decoded.chars().any(char::is_control) && aterm_codec::pct::encode(&decoded) == encoded
}

/// The most bytes of the agent's words a receipt keeps ([`LandReceipt::said`]).
pub const SAID_BYTES: usize = 240;

/// How a receipt's journal line begins (`journal::Noted::Receipt`).
pub const RECEIPT: &str = "RECEIPT";

/// What the probe's reading at the landing could say of where the work is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ReceiptLeg {
    /// `HEAD` is on the remote main the probe counted against (`origin/main`): nothing
    /// uncommitted, untracked, stashed on its branch, unpushed or off that ref.
    OnMain,
    /// Pushed (a remote-tracking ref holds `HEAD`), with no remote main to check it against.
    Pushed,
    /// Committed in a repository with no remote: nothing was pushed anywhere.
    NoRemote,
    /// No repository checked: the probe read nothing to name (not a repository, no commit yet, a
    /// folder macOS guards, a network volume, no `git`, a read git refused; every
    /// [`RepoRead::Clean`]). Files land where they are.
    #[default]
    NoRepo,
    /// The session's folder is gone ([`RepoRead::Gone`], §14.17 item 4, slice 9c): nothing was
    /// there to check.
    Gone,
}

impl ReceiptLeg {
    /// Every leg, in the order a receipt says them.
    pub const ALL: [Self; 5] = [
        Self::OnMain,
        Self::Pushed,
        Self::NoRemote,
        Self::NoRepo,
        Self::Gone,
    ];

    /// Its word in the journal's `leg=`, `status`'s `landed_leg=` and, but for
    /// [`Self::OnMain`], the task record's `checked=` ([`LandReceipt::checked_word`]).
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::OnMain => "on-main",
            Self::Pushed => "pushed",
            Self::NoRemote => "no-remote",
            Self::NoRepo => "no-repo",
            Self::Gone => "gone",
        }
    }

    /// The leg whose [`Self::word`] `word` is.
    #[must_use]
    pub fn of_word(word: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|leg| leg.word() == word)
    }
}

/// What aterm checked at one landing, and what the agent said of it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct LandReceipt {
    /// `HEAD`'s commit as [`HEAD_DIGITS`] lowercase hex digits; empty on [`ReceiptLeg::NoRepo`].
    pub head: String,
    /// The remote main `HEAD` was found on (`origin/main`); empty where none was read.
    pub main_ref: String,
    /// The repository's top-level directory, as the probe names it ([`RepoWork::repo`]'s form:
    /// a control character `?`, a longer path's end kept behind `…`); on [`ReceiptLeg::Gone`]
    /// the folder that is gone, in that form; empty on [`ReceiptLeg::NoRepo`].
    ///
    /// [`RepoWork::repo`]: super::repo_work::RepoWork::repo
    pub repo: String,
    /// `HEAD`'s branch, whose stashes the probe counted, on [`ReceiptLeg::OnMain`] (slice 9c:
    /// `no stash on main`; [`DETACHED`] on a detached `HEAD`); empty on every other leg and on a
    /// receipt journaled before the branch was read (slice 9b's line has no `branch=`).
    ///
    /// [`DETACHED`]: super::repo_work::DETACHED
    pub branch: String,
    /// What the reading says of where the work is.
    pub leg: ReceiptLeg,
    /// The agent answered aterm's land check (`land-check@v1`, typed after its `Nothing left:`
    /// defense) `LANDED`, and the landing stood on that answer: [`Self::said`] is then the
    /// defense the check audited, not the word. A landing a build before the check noted, and a
    /// landing by the agent's own check, answered none (`mode_words::said_words` says which).
    pub check: bool,
    /// The agent's defense line and the lines after it, as one line: hidden and control
    /// characters dropped, runs of space one, title-safe (no fact separator, no spaced dash), at
    /// most [`SAID_BYTES`] bytes (a cut ends in `…`); empty where the reply held no defense.
    pub said: String,
    /// When the landing was read, unix ms: the wall its `DONE` note was made at (a landing a
    /// loop before this one noted keeps that one's first time).
    pub at_ms: i64,
}

impl LandReceipt {
    /// The receipt of a landing read as `read` (the probe's reading at the point that landed)
    /// over the agent's last words `tail`, at `at_ms`, answering no land check (the loop sets
    /// [`Self::check`] where its landing stood on one). `None` where the reading is not clean (a
    /// finding, or a repository it could not read): no landing stands on one.
    #[must_use]
    pub fn of(read: &RepoRead, tail: Option<&str>, at_ms: i64) -> Option<Self> {
        let said = tail.map(said_of).unwrap_or_default();
        match read {
            RepoRead::Checked(at) => {
                let leg = if !at.remote {
                    ReceiptLeg::NoRemote
                } else if at.main_ref.is_empty() {
                    ReceiptLeg::Pushed
                } else {
                    ReceiptLeg::OnMain
                };
                let head = at.head.get(..HEAD_DIGITS)?;
                let on_main = |s: &String| {
                    if leg == ReceiptLeg::OnMain {
                        s.clone()
                    } else {
                        String::new()
                    }
                };
                is_head(head).then(|| Self {
                    head: head.to_owned(),
                    main_ref: on_main(&at.main_ref),
                    repo: at.repo.clone(),
                    branch: on_main(&at.branch),
                    leg,
                    check: false,
                    said,
                    at_ms,
                })
            }
            RepoRead::Clean => Some(Self {
                said,
                at_ms,
                ..Self::default()
            }),
            RepoRead::Gone(folder) => Some(Self {
                repo: folder.clone(),
                leg: ReceiptLeg::Gone,
                said,
                at_ms,
                ..Self::default()
            }),
            RepoRead::Found(_) | RepoRead::Unread => None,
        }
    }

    /// What the task record's `checked=` says of it (slice 9c): `<head>@<main ref>` on
    /// [`ReceiptLeg::OnMain`] (`3f9a1c2e4b7d@origin/main`, the ref percent-encoded), else its
    /// leg's word (`pushed`, `no-remote`, `no-repo`, `gone`).
    #[must_use]
    pub fn checked_word(&self) -> String {
        match self.leg {
            ReceiptLeg::OnMain => {
                format!("{}@{}", self.head, aterm_codec::pct::encode(&self.main_ref))
            }
            leg => leg.word().to_owned(),
        }
    }

    /// Its line in the journal, under the landing's `DONE` note of point `seq`:
    /// `RECEIPT seq=<n> rule=land@v1 head=<12 hex|-> on=<ref|-> leg=<word>
    /// check=<land-check@v1|-> repo=<pct|-> [branch=<pct>] <said>`, `branch=` only where a branch
    /// is named (slice 9c; a line written before it has none, and reads back with none).
    #[must_use]
    pub fn line(&self, seq: u64) -> String {
        let or_dash = |s: &str| {
            if s.is_empty() {
                "-".to_owned()
            } else {
                aterm_codec::pct::encode(s)
            }
        };
        let line = format!(
            "{RECEIPT} seq={seq} rule={RULE_LAND} head={} on={} leg={} check={} repo={}",
            or_dash(&self.head),
            or_dash(&self.main_ref),
            self.leg.word(),
            if self.check { RULE_LAND_CHECK } else { "-" },
            or_dash(&self.repo),
        );
        let line = if self.branch.is_empty() {
            line
        } else {
            format!("{line} branch={}", aterm_codec::pct::encode(&self.branch))
        };
        if self.said.is_empty() {
            line
        } else {
            format!("{line} {}", self.said)
        }
    }

    /// The point's seq and the receipt a journal `line` says ([`Self::line`]), the landing read
    /// at `at_ms`; `None` for any line this build did not write. A line a build before the land
    /// check wrote has no `check=` and reads as answering none.
    #[must_use]
    pub fn of_line(line: &str, at_ms: i64) -> Option<(u64, Self)> {
        let mut rest = line.strip_prefix(RECEIPT)?.strip_prefix(' ')?;
        let mut field = |key: &str| {
            let (word, after) = rest.split_once(' ').unwrap_or((rest, ""));
            let value = word.strip_prefix(key)?.strip_prefix('=')?;
            rest = after;
            Some(value)
        };
        let seq = field("seq")?.parse::<u64>().ok()?;
        if field("rule")? != RULE_LAND {
            return None;
        }
        let undash = |s: &str| {
            if s == "-" {
                String::new()
            } else {
                aterm_codec::pct::decode(s)
            }
        };
        let head = undash(field("head")?);
        let main_ref = undash(field("on")?);
        let leg = ReceiptLeg::of_word(field("leg")?)?;
        let check = match field("check") {
            Some(RULE_LAND_CHECK) => true,
            Some("-") | None => false,
            Some(_) => return None,
        };
        let repo = undash(field("repo")?);
        let (branch, said) = match rest.strip_prefix("branch=") {
            Some(named) => {
                let (branch, said) = named.split_once(' ').unwrap_or((named, ""));
                (aterm_codec::pct::decode(branch), said.to_owned())
            }
            None => (String::new(), rest.to_owned()),
        };
        let named = !head.is_empty() && !repo.is_empty();
        let shaped = match leg {
            ReceiptLeg::NoRepo => head.is_empty() && main_ref.is_empty() && repo.is_empty(),
            ReceiptLeg::OnMain => named && !main_ref.is_empty(),
            ReceiptLeg::Pushed | ReceiptLeg::NoRemote => named && main_ref.is_empty(),
            ReceiptLeg::Gone => head.is_empty() && main_ref.is_empty() && !repo.is_empty(),
        } && (branch.is_empty() || leg == ReceiptLeg::OnMain);
        let head_ok = matches!(leg, ReceiptLeg::NoRepo | ReceiptLeg::Gone)
            || (head.len() == HEAD_DIGITS && is_head(&head));
        let kept = said_of_line(&said) == said;
        (shaped && head_ok && kept).then_some((
            seq,
            Self {
                head,
                main_ref,
                repo,
                branch,
                leg,
                check,
                said,
                at_ms,
            },
        ))
    }
}

/// Whether `hex` is lowercase hex digits alone.
fn is_head(hex: &str) -> bool {
    !hex.is_empty()
        && hex
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// The receipt's words of the agent's last words `tail` ([`LandReceipt::said`]): its defense
/// line and every line after it, as one line.
#[must_use]
pub fn said_of(tail: &str) -> String {
    crate::supervise::policy::needs::defense_and_after(tail)
        .map(|lines| said_of_line(&lines.join(" ")))
        .unwrap_or_default()
}

/// `words` as a receipt keeps them: hidden characters dropped, control characters and line
/// breaks a space, runs of space one, the fact separator and a spaced dash a comma, cut to
/// [`SAID_BYTES`] with `…`.
fn said_of_line(words: &str) -> String {
    let shown: String = crate::supervise::limit::one_line(words)
        .chars()
        .filter(|c| !aterm_messages::text::is_hidden_char(*c))
        .collect();
    let one = crate::supervise::limit::one_line(&shown)
        .replace(FACT_SEPARATOR, ", ")
        .replace(" \u{2014} ", ", ");
    let one = one.trim();
    if one.len() <= SAID_BYTES {
        return one.to_owned();
    }
    let mut cut = super::truncate_bytes(one, SAID_BYTES - '…'.len_utf8())
        .trim_end()
        .to_owned();
    cut.push('…');
    cut
}

#[cfg(test)]
#[path = "land_receipt_tests.rs"]
mod tests;
