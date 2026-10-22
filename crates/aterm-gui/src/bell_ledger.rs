// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! WHICH TAB RANG (2026-10-22): each session's own record of the bells its
//! programs rang, so "why do I keep hearing a ping?" has an answer after the
//! fact. A BEL reaches `App::on_bell`, which decides whether the Mac's alert
//! sound plays and records the bell here: when, the program in front, and
//! what the sound gates made of it ([`BellSound`]). `status` and the survey
//! print it (`bells= bells_sounded= last_bell=`, [`BellRead::wire`]).
//!
//! A FACT FOR LATER, NEVER AN ALERT: nothing here raises attention, posts a
//! message or moves a need, and recording changes nothing the bell does —
//! the beep, the flash and the attention request are `on_bell`'s, decided
//! before the record is written.
//!
//! The counts are this aterm process's: a self-update starts them over, as
//! it does `human_ms=` (the session log's `bell` marks are what crosses one).

use std::sync::Mutex;

/// How many bells [`BellLedger`] keeps, the newest first: enough to tell a
/// tab whose one program rings from one where two take turns, and few enough
/// for one `status` line.
pub(crate) const KEPT: usize = 4;

/// What became of one bell's sound: it played, or the gate that kept it
/// quiet, in `App::on_bell`'s order.
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
    /// The alert sound had played within the rate limit's interval
    /// (`BELL_BEEP_INTERVAL`, a second): this bell went unheard.
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
    /// The program in front of the session then, as `status program=` names
    /// it, or the process name of a foreground group the sweep had not yet
    /// named (`App::bell_program`). `None` when nothing could say.
    pub(crate) program: Option<String>,
    /// What became of its sound.
    pub(crate) sound: BellSound,
}

/// The program in front of a session as its bell rang ([`Rang::program`]):
/// the name the status sweep published (`status program=`, read off the
/// leader's argv, so Claude Code is `claude`) while the PTY's foreground
/// group, `live_pgid`, is the group it named; else the process name of the
/// group in front now (`name_of`), a program the sweep has not read yet;
/// else the published name. `live_pgid <= 0` is no reading (a ConPTY, a
/// session with no PTY).
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
    /// ` bells=<n> bells_sounded=<n> last_bell=<ms>/<sound>/<program>[,…]|-`
    /// as `status` and the survey print it, ages counted to `now_unix_ms`:
    /// each kept bell as how long ago it rang, what became of its sound
    /// ([`BellSound::word`]) and the program in front, the newest first.
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

/// A session's bells ([the module](self)): a leaf lock on its context, held
/// only to copy a few words in or out.
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
mod tests {
    use super::*;

    fn rang(at: u64, program: Option<&str>, sound: BellSound) -> Rang {
        Rang {
            at_unix_ms: at,
            program: program.map(str::to_owned),
            sound,
        }
    }

    /// A session that never rang says so in every field; one that rang
    /// counts each bell, the sounded ones apart, and keeps the newest
    /// [`KEPT`] first. NEGATIVE CONTROL: the quiet ledger's fields.
    #[test]
    fn a_ledger_counts_every_bell_and_keeps_the_newest_few() {
        let quiet = BellLedger::default();
        assert_eq!(
            quiet.wire(10_000),
            "bells=0 bells_sounded=0 last_bell=-",
            "a session that never rang"
        );

        let ledger = BellLedger::default();
        ledger.ring(rang(1_000, Some("zsh"), BellSound::Sounded));
        ledger.ring(rang(2_000, Some("zsh"), BellSound::Limited));
        ledger.ring(rang(3_000, Some("vim"), BellSound::Off));
        ledger.ring(rang(4_000, None, BellSound::Serious));
        ledger.ring(rang(5_000, Some("claude"), BellSound::Sounded));
        assert_eq!(
            ledger.wire(10_000),
            "bells=5 bells_sounded=2 \
             last_bell=5000/sounded/claude,6000/serious/-,7000/off/vim,8000/limited/zsh",
            "five rang, two sounded; the oldest of five is the one let go"
        );
        assert_eq!(ledger.read().recent.len(), KEPT);
    }

    /// The sweep's name while the group in front is the one it named; the
    /// process in front by its own name when the sweep has not caught up;
    /// the sweep's name when nothing in front can be read or named.
    #[test]
    fn the_program_in_front_is_the_sweeps_name_unless_another_group_holds_it() {
        let name_of = |pgid: i32| (pgid == 77).then(|| "vim".to_owned());
        let claude = || Some("claude".to_owned());
        assert_eq!(program_in_front(claude(), 42, 42, &name_of), claude());
        assert_eq!(
            program_in_front(claude(), 42, 77, &name_of).as_deref(),
            Some("vim"),
            "another group in front: its own name"
        );
        assert_eq!(program_in_front(claude(), 42, 99, &name_of), claude());
        assert_eq!(program_in_front(claude(), 42, -1, &name_of), claude());
        assert_eq!(program_in_front(None, 0, -1, &name_of), None);
    }

    /// A program name cannot split its entry, even for a reader that
    /// decodes the whole value before it splits it: its `/` and `,` read as
    /// `_`, and the rest decodes back. The value stays one token.
    #[test]
    fn a_program_name_never_splits_its_entry() {
        let ledger = BellLedger::default();
        ledger.ring(rang(0, Some("a/b,c d"), BellSound::Headless));
        let wire = ledger.wire(0);
        let last = wire.rsplit_once("last_bell=").expect("the field").1;
        assert!(!last.contains(' '), "one token: {wire}");
        let decoded = aterm_codec::pct::decode(last);
        let parts: Vec<&str> = decoded.split('/').collect();
        assert_eq!(parts, ["0", "headless", "a_b_c d"], "{wire}");
        assert!(!decoded.contains(','), "{wire}");
    }
}
