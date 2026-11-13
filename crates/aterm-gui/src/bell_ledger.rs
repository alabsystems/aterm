// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! WHICH TAB RANG: each session's own record of the bells its programs rang, so "why do I keep
//! hearing a ping?" has an answer after the fact.

use std::sync::Mutex;

/// How many bells [`BellLedger`] keeps, the newest first: enough to tell a tab whose one program
/// rings from one where two take turns, and few enough for one `status` line.
pub(crate) const KEPT: usize = 4;

/// What became of one bell's sound: it played, or the gate that kept it quiet, in `App::on_bell`'s
/// order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BellSound {
    /// The Mac's alert sound played.
    Sounded,
    /// A headless instance: no window, so no sound.
    Headless,
    /// Serious mode silences the terminal's sounds.
    Serious,
    /// `bell_sound = false` in aterm.toml.
    Off,
    /// The alert sound had played within the rate limit's interval (`BELL_BEEP_INTERVAL`, a
    /// second): this bell went unheard.
    Limited,
}

impl BellSound {
    /// The wire word (`last_bell=<ms>/<word>/<program>`).
    pub(crate) const fn word(self) -> &'static str {
        match self {
            Self::Sounded => "sounded",
            Self::Headless => "headless",
            Self::Serious => "serious",
            Self::Off => "off",
            Self::Limited => "limited",
        }
    }
}

/// One bell as [`BellLedger`] keeps it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Rang {
    /// When it arrived (unix ms).
    pub(crate) at_unix_ms: u64,
    /// The program in front of the session then, as `status program=` names it, or the process name
    /// of a foreground group the sweep had not yet named (`crate::bell_program`).
    pub(crate) program: Option<String>,
    /// What became of its sound.
    pub(crate) sound: BellSound,
}

/// The program in front of a session as its bell rang ([`Rang::program`]): the name the status
/// sweep published (`status program=`, read off the leader's argv, so Claude Code is `claude`)
/// while the PTY's foreground group, `live_pgid`, is the group it named; else the process name of
/// the group in front now (`name_of`), a program the sweep has not read yet; else the published
/// name.
pub(crate) fn program_in_front(
    published: Option<String>,
    published_pgid: i32,
    live_pgid: i32,
    name_of: &dyn Fn(i32) -> Option<String>,
) -> Option<String> {
    if live_pgid > 0 && live_pgid != published_pgid {
        name_of(live_pgid).or(published)
    } else {
        published
    }
}

/// What a read of a [`BellLedger`] holds.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct BellRead {
    /// Every bell the session rang since this process began serving it.
    pub(crate) total: u64,
    /// How many of those played the alert sound.
    pub(crate) sounded: u64,
    /// The last [`KEPT`], the newest first.
    pub(crate) recent: Vec<Rang>,
}

/// A program name for one `last_bell=` entry: its `/` and `,` — which
/// separate the entry's parts and the entries, and which a reader that
/// decodes the whole value first (`aterm ctl survey`) would see as such —
/// read as `_`, then pct-encoded as every free word on the wire is. `-` for
/// none. A program name has neither most of the time: it is a file name.
fn program_token(program: Option<&str>) -> String {
    match program.filter(|p| !p.is_empty()) {
        Some(p) => crate::control::pct_encode(&p.replace(['/', ','], "_")),
        None => "-".to_string(),
    }
}

impl BellRead {
    /// ` bells=<n> bells_sounded=<n> last_bell=<ms>/<sound>/<program>[,…]|-` as `status` and the
    /// survey print it, ages counted to `now_unix_ms`: each kept bell as how long ago it rang, what
    /// became of its sound ([`BellSound::word`]) and the program in front, the newest first.
    pub(crate) fn wire(&self, now_unix_ms: u64) -> String {
        let last = if self.recent.is_empty() {
            "-".to_string()
        } else {
            self.recent
                .iter()
                .map(|r| {
                    format!(
                        "{}/{}/{}",
                        now_unix_ms.saturating_sub(r.at_unix_ms),
                        r.sound.word(),
                        program_token(r.program.as_deref())
                    )
                })
                .collect::<Vec<_>>()
                .join(",")
        };
        format!(
            "bells={} bells_sounded={} last_bell={last}",
            self.total, self.sounded
        )
    }
}

/// A session's bells ([the module](self)): a leaf lock on its context, held only to copy a few
/// words in or out.
#[derive(Debug, Default)]
pub(crate) struct BellLedger(Mutex<BellRead>);

impl BellLedger {
    /// Record one bell, `rang`.
    pub(crate) fn ring(&self, rang: Rang) {
        let mut book = self.0.lock().unwrap_or_else(|p| p.into_inner());
        book.total = book.total.saturating_add(1);
        if rang.sound == BellSound::Sounded {
            book.sounded = book.sounded.saturating_add(1);
        }
        book.recent.insert(0, rang);
        book.recent.truncate(KEPT);
    }

    /// What it holds now.
    pub(crate) fn read(&self) -> BellRead {
        self.0.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    /// [`BellRead::wire`] of what it holds now, at `now_unix_ms`.
    pub(crate) fn wire(&self, now_unix_ms: u64) -> String {
        self.read().wire(now_unix_ms)
    }
}

#[cfg(test)]
#[path = "bell_ledger_tests.rs"]
mod tests;
