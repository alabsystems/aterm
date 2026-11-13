// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! The account's durable record of setup's administrator-password attempt (setup's one early
//! elevation: the Command Line Tools, developer mode, the Mac's name rule and the LAN broker).
//! Window and terminal askers share one record and one lock, and the record says how the last
//! attempt ended, so the next automatic ask follows from that end alone, whatever build, launch,
//! update or reboot reads it:
//!
//! - `asking`: an asker claimed the episode and its process ([`Asker`]) still runs, or the
//!   attempt its line replaced still asks: no other automatic asker asks beside it. Once that
//!   process is gone (aterm quit, crashed or was killed, the Mac logged out or rebooted) with no
//!   end written, the attempt was cut off: it reads as [`Asked::Unfinished`], due again at once
//!   for a first try (else after the pause of the tries it carries, from its claim or the last
//!   unfinished end it carries), and the cut-off adds no try of its own. A window close that
//!   quits aterm with the Mac Setup tab's program asking writes no end for what the quit's
//!   hangup ends the program with ([`tab_end`]), so it is such a cut-off, never a decline.
//! - `unfinished`: the attempt ended without finishing (a password refused or never typed, a
//!   download, catalog or installer failure, Apple's installer window closed without the tools,
//!   which says nothing of which button closed it, an end the tab decided before `sudo`): the next
//!   automatic ask is due after [`ASK_AGAIN`]'s pause for that many tries in a row.
//! - `done`: the attempt finished; an automatic ask is due again only where a later reading finds
//!   a step owed, and no sooner than [`ASK_AGAIN`]'s first pause.
//! - `declined`: a person said no (a Ctrl-C at the password prompt, System State's Stop, the Mac
//!   Setup tab closed while aterm runs on): the next automatic ask holds for [`DECLINE_AGAIN`]'s
//!   long pause for that many declines in a row, as the owner's own choice holds elsewhere, so it
//!   comes back by itself without nagging; the person's own yes needs no wait: their retry
//!   (Install…, a typed `aterm init`) or System State's Set up now ([`lift`]), whose end is
//!   recorded the same way.
//!
//! Every line also says when setup's elevation last finished on this account
//! ([`Record::finished`]), whatever came after: a step that finished once and reads undone later
//! was undone by someone, which the automatic askers leave as it is.
//!
//! An `asking` line keeps the line it replaced, one deep, so the record holds two attempts at
//! once: a person's retry and the attempt it took the episode over from, which may still ask.
//! Each writes its own end in its own place: the replaced attempt's end takes the kept line's
//! place under the retry ([`hand`] names that attempt's program there too), and the retry's
//! end, where the attempt it replaced still asks, leaves that one asking over it. So a late end
//! of an older attempt never overwrites a newer retry, nothing asks under a line nobody ends,
//! and an attempt that asked nothing gives the record back as it was ([`End::Released`]):
//! exactly, or, where a retry over it gave its line back without that one, as the unfinished
//! tries' schedule and the finish its claim carried. An automatic claim succeeds only where the
//! record lets an automatic ask go now, under the lock, so no automatic asker asks beside an
//! attempt the record names. A third attempt at once (a retry over a retry over a running ask)
//! drops the oldest from the record: that one's end then writes only a finish, and once the
//! other two end, an automatic claim can go beside it while it still asks. Historical `opened`
//! records (whose end nobody wrote) read as one unfinished try, except where an update carried
//! the live tab of the build that wrote one into it ([`carry`]), and a `declined` line with no
//! count of declines (written before the count) as one decline. Unreadable or malformed records
//! hold automatic asks and are never rewritten. Every due is read at the reader's clock
//! ([`Backoff::due_by`]): a line dated after it (the clock set back since) is due at once.

use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::retry_schedule::Backoff;

/// The record's file name, in this account's aterm state ([`crate::dirs::account_state_dir`]).
pub const FILE: &str = "command-line-tools-tab";

/// The most lines a record holds: a historical record's `opened` lines; a record this code
/// writes is one line.
pub const LINES: usize = 8;

/// The longest build id a line names; aterm's are far shorter (a version, `+g` and a commit).
pub const BUILD_BYTES: usize = 96;

/// The longest record read or written: [`LINES`] lines of two builds and their numbers each (an
/// `asking` line and the one it replaced).
pub const BYTES: usize = LINES * 2 * ("unfinished ".len() + BUILD_BYTES + 9 * 21);

/// When an automatic ask comes again after an attempt that did not finish (by its tries in a
/// row), or after one that finished and a later reading finds a step owed (one try's pause):
/// ten minutes after the first, doubling, never more than twelve hours apart.
pub const ASK_AGAIN: Backoff = Backoff::new(
    Duration::from_secs(10 * 60),
    Duration::from_secs(12 * 60 * 60),
);

/// When an automatic ask comes again after a person declined (by their declines in a row): thirty
/// days after the first, the hold of the owner's own choice elsewhere (the message center's
/// `Resurface::CHOICE`, `atpkg::devtools::DECLINE_HOLD_S`), doubling, never more than ninety
/// days apart (a repeat decline's, `atpkg::devtools::DECLINE_REPEAT_HOLD_S`). So a decline is
/// neither asked again soon (a nag) nor never (a setup left undone for good with nobody told).
pub const DECLINE_AGAIN: Backoff = Backoff::new(
    Duration::from_secs(30 * 86_400),
    Duration::from_secs(90 * 86_400),
);

/// Hang detector while a change of the record waits for another asker's durable write
/// ([`Live`]'s lock), for a caller with nothing else to do meanwhile (a terminal's `aterm init`,
/// the Mac Setup tab's own program): scheduling delay alone must not fail its claim.
#[cfg(not(target_family = "wasm"))]
pub const LOCK_WAIT: std::time::Duration = std::time::Duration::from_secs(60);

/// How long a change made on a window's event loop waits for the record's lock ([`OnLoop`]): a
/// change is a short read and one durable write, so a holder past this is stalled (stopped,
/// swapped out, on a stuck disk), and the change fails rather than freeze every window's typing
/// and drawing behind it. The window tries a claim that failed again on its own schedule, and
/// nothing a failed change leaves says more than the record did.
#[cfg(not(target_family = "wasm"))]
pub const LOOP_LOCK_WAIT: std::time::Duration = std::time::Duration::from_secs(2);

/// This account's record: [`FILE`] in its aterm state root ([`crate::dirs::account_state_dir`]:
/// `~/Library/Application Support/aterm` on macOS, which `ATERM_STATE_HOME` isolates for a lab
/// instance). It reads no `XDG_CONFIG_HOME`, which a shell may export while the window, opened by
/// launchd, has none, so the window and the follow-up `aterm init` of a person's own package
/// install or update, each in its own environment, claim in one file under one lock. `None` where
/// the state root has no place (no home).
#[must_use]
pub fn path() -> Option<PathBuf> {
    crate::dirs::account_state_dir().map(|dir| dir.join(FILE))
}

/// An exclusive hold on the lock beside a record ([`Files::lock`]), let go when it drops.
pub struct Held {
    _guard: Box<dyn Send>,
}

impl Held {
    /// A hold that `guard` keeps until it drops.
    #[must_use]
    pub fn new(guard: impl Send + 'static) -> Self {
        Self {
            _guard: Box::new(guard),
        }
    }
}

/// The record's file I/O and the one process fact it reads: this Mac's ([`Live`]), or a test
/// double's own.
pub trait Files {
    /// The text of the regular file at `path`, at most `max` bytes, read without following a link
    /// or waiting on a pipe; `None` where there is none.
    ///
    /// # Errors
    /// Anything else at `path` (a link, a folder, a FIFO), a file longer than `max` bytes or not
    /// UTF-8, and the read's own error.
    fn read(&self, path: &Path, max: usize) -> io::Result<Option<String>>;

    /// Replace the file at `path` whole with `text`, durably, its directory entry included, making
    /// its folder where there is none.
    ///
    /// # Errors
    /// A path held by anything but a regular file, which is left as it is, and the write's own
    /// error, which leaves the file as it was.
    fn write(&self, path: &Path, text: &str) -> io::Result<()>;

    /// Remove the file at `path`; none there is no error.
    ///
    /// # Errors
    /// The removal's own.
    fn remove(&self, path: &Path) -> io::Result<()>;

    /// Hold the lock that every change of the record at `path` is made under ([`claim`],
    /// [`end`]), waiting while another asker, in this process or another, holds it, until the
    /// hold drops. Its folder is made where there is none.
    ///
    /// # Errors
    /// No hold within the wait, and the lock's own error.
    fn lock(&self, path: &Path) -> io::Result<Held>;

    /// Whether the process `pid` born `birth` ([`Asker::birth`]) runs now: an `asking` line whose
    /// asker no longer runs was cut off.
    fn runs(&self, pid: i32, birth: u64) -> bool;
}

/// The process that asks for the password: a terminal `aterm init` itself, or the Mac Setup tab's
/// own program (whose window claims the episode, then hands it to that program, [`hand`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Asker {
    /// The build that asks.
    pub build: String,
    /// Its pid.
    pub pid: i32,
    /// Its birth, as [`Files::runs`] reads it (the kernel's start record: whole microseconds on
    /// Apple, [`aterm_uds::process::read_birth`]).
    pub birth: u64,
}

impl Asker {
    /// This process, asking as `build`; `None` where its birth cannot be read.
    #[cfg(not(target_family = "wasm"))]
    #[must_use]
    pub fn this_process(build: &str) -> Option<Self> {
        let pid = i32::try_from(std::process::id()).ok()?;
        Self::process(build, pid)
    }

    /// The process `pid`, asking as `build`; `None` where its birth cannot be read.
    #[cfg(not(target_family = "wasm"))]
    #[must_use]
    pub fn process(build: &str, pid: i32) -> Option<Self> {
        let birth = aterm_uds::process::read_birth(pid)?.0;
        Some(Self {
            build: build.to_owned(),
            pid,
            birth,
        })
    }
}

/// An attempt's hold on the episode: what [`claim`] or [`explicit`] wrote, which only that
/// attempt's [`end`] and [`hand`] match.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Claim {
    /// The asker the record names now.
    pub asker: Asker,
    /// When the attempt claimed the episode (unix seconds).
    pub at: u64,
    /// The attempt itself ([`fresh`]): two claims one asker makes in one second are still two
    /// attempts, so an older one's late end writes only into its own place ([`end`]).
    pub n: u64,
}

/// A number no other claim this process makes holds, and no claim another running process makes:
/// the process id above a count of the claims made here.
fn fresh() -> u64 {
    static MADE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let made = MADE.fetch_add(1, std::sync::atomic::Ordering::Relaxed) & u64::from(u32::MAX);
    (process_id() << 32) | made
}

/// This process's id; none on wasm, which runs one process.
#[cfg(not(target_family = "wasm"))]
fn process_id() -> u64 {
    u64::from(std::process::id())
}

/// This process's id; none on wasm, which runs one process.
#[cfg(target_family = "wasm")]
const fn process_id() -> u64 {
    0
}

/// What the account's record says about automatic password requests.
#[derive(Debug)]
pub enum Asked {
    /// No record: an automatic ask may go now.
    Available,
    /// An asker's process runs: no other automatic ask goes beside it.
    Asking,
    /// The last attempt did not finish, or was cut off: the next automatic ask is due at `due`
    /// (unix seconds), after `tries` tries in a row that did not finish.
    Unfinished {
        /// When the next automatic ask is due (unix seconds).
        due: u64,
        /// Tries in a row that did not finish.
        tries: u32,
    },
    /// The last attempt finished: an automatic ask is due again at `due` only where a step reads
    /// owed then.
    Done {
        /// The earliest the next automatic ask may come (unix seconds).
        due: u64,
    },
    /// A person declined, `declines` times in a row: the next automatic ask holds until `due`
    /// (unix seconds, [`DECLINE_AGAIN`]). A decline whose hold has passed reads as the record
    /// would without it ([`lift`]).
    Declined {
        /// When the next automatic ask is due (unix seconds).
        due: u64,
        /// Declines in a row.
        declines: u32,
    },
    /// The record cannot be read safely; automatic requests remain held.
    Unread(io::Error),
}

impl Asked {
    /// When an automatic ask may go (unix seconds): at once where there is no record, at its due
    /// after an attempt that ended or once a decline's hold passes, and never while an asker runs
    /// or where the record cannot be read.
    #[must_use]
    pub const fn due(&self) -> Option<u64> {
        match self {
            Self::Available => Some(0),
            Self::Unfinished { due, .. } | Self::Done { due } | Self::Declined { due, .. } => {
                Some(*due)
            }
            Self::Asking | Self::Unread(_) => None,
        }
    }

    /// Whether an automatic ask may go at `now` (unix seconds).
    #[must_use]
    pub fn may_ask(&self, now: u64) -> bool {
        self.due().is_some_and(|due| due <= now)
    }

    /// Whether an automatic ask is still coming: one runs now, or one may go now or at its next
    /// try. Not while a person's decline holds (it comes back only after [`DECLINE_AGAIN`]'s long
    /// pause, so a person's step until then), nor where the record cannot be read.
    #[must_use]
    pub const fn coming(&self) -> bool {
        !matches!(self, Self::Declined { .. } | Self::Unread(_))
    }
}

/// The account's record as one read finds it ([`record`]).
#[derive(Debug)]
pub struct Record {
    /// What it says of automatic asks.
    pub asked: Asked,
    /// Setup's elevation finished on this account at least once ([`End::Done`]), whatever came
    /// after: a root step it did that reads undone now (developer mode turned off) was undone
    /// by someone since, so an automatic ask is never owed for it and never re-applies it.
    /// `false` where the record cannot be read.
    pub finished: bool,
}

/// How an attempt ended ([`end`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum End {
    /// It finished.
    Done,
    /// It did not finish: tried again by itself on [`ASK_AGAIN`]'s schedule.
    Unfinished,
    /// A person declined it: no automatic ask comes until [`DECLINE_AGAIN`]'s long pause passes
    /// or their own yes ([`lift`]).
    Declined,
    /// It provably asked nothing (no `sudo` ever started, or it stood aside before asking): the
    /// record goes back to exactly the line it held before the claim, or to none; where a
    /// person's retry over the claim already gave it back without that line, to the unfinished
    /// tries' schedule or the finish the claim carried of it.
    Released,
}

/// Where the record held the attempt whose end it took ([`end`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Took {
    /// As its line: the attempt was the record's own, and its end is the line's (carried by the
    /// attempt it replaced where that one still asks).
    Line,
    /// As the line its line keeps: a person's retry replaced the attempt, and its end went into
    /// that place, under the retry.
    Replaced,
    /// In neither place (three attempts asked at once, the oldest given up): only a finish is
    /// written, as the account's.
    Nowhere,
}

impl Took {
    /// Whether the record held the attempt, in either place, and now says how it ended.
    #[must_use]
    pub const fn held(self) -> bool {
        !matches!(self, Self::Nowhere)
    }
}

/// One line of a record.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Line {
    /// `asking <build> <pid> <birth> <at> <n> <tries> <since> <finished>`, then ` after ` and the
    /// line it replaced where there was one: claimed at `at` as attempt `n`, after `tries`
    /// unfinished tries in a row whose last ended at `since`, the elevation having last finished
    /// at `finished` (0: never). `prior` is kept one deep: a replaced `asking` line keeps no
    /// prior of its own.
    Asking {
        claim: Claim,
        tries: u32,
        since: u64,
        finished: u64,
        prior: Option<Box<Line>>,
    },
    /// `unfinished <build> <at> <tries> <finished>`.
    Unfinished {
        build: String,
        at: u64,
        tries: u32,
        finished: u64,
    },
    /// `done <build> <at>`: it finished at `at`.
    Done { build: String, at: u64 },
    /// `declined <build> <at> <finished> <declines>`: the `declines`-th decline in a row, at `at`
    /// (a line written before the count read as the first).
    Declined {
        build: String,
        at: u64,
        finished: u64,
        declines: u32,
    },
    /// A historical `opened <build>` or `opened` line: claimed, its end never written.
    Opened,
}

impl Line {
    /// The line's text, newline included.
    fn text(&self) -> String {
        format!("{}\n", self.words())
    }

    /// The line's words, with no newline.
    fn words(&self) -> String {
        match self {
            Self::Asking {
                claim,
                tries,
                since,
                finished,
                prior,
            } => {
                let head = format!(
                    "asking {} {} {} {} {} {tries} {since} {finished}",
                    claim.asker.build, claim.asker.pid, claim.asker.birth, claim.at, claim.n
                );
                match prior {
                    Some(prior) => format!("{head} after {}", prior.words()),
                    None => head,
                }
            }
            Self::Unfinished {
                build,
                at,
                tries,
                finished,
            } => format!("unfinished {build} {at} {tries} {finished}"),
            Self::Done { build, at } => format!("done {build} {at}"),
            Self::Declined {
                build,
                at,
                finished,
                declines,
            } => format!("declined {build} {at} {finished} {declines}"),
            Self::Opened => "opened".to_owned(),
        }
    }

    /// This line as an `asking` line keeps it ([`Line::Asking::prior`]): one deep, so a replaced
    /// `asking` line keeps no prior of its own.
    fn as_prior(&self) -> Self {
        match self {
            Self::Asking {
                claim,
                tries,
                since,
                finished,
                ..
            } => Self::Asking {
                claim: claim.clone(),
                tries: *tries,
                since: *since,
                finished: *finished,
                prior: None,
            },
            other => other.clone(),
        }
    }

    /// This `asking` line with `claim` in place of its own (the same attempt, handed to the
    /// process that asks); any other line as it is.
    fn handed(&self, claim: Claim) -> Self {
        match self {
            Self::Asking {
                tries,
                since,
                finished,
                prior,
                ..
            } => Self::Asking {
                claim,
                tries: *tries,
                since: *since,
                finished: *finished,
                prior: prior.clone(),
            },
            other => other.clone(),
        }
    }

    /// This line, also saying setup's elevation finished at `at` (an attempt the line no longer
    /// holds finished, [`end`]): an asking line and the line it keeps carry the finish, a done
    /// line becomes that finish, and a historical line reads on as the unfinished try it was,
    /// asked as `build`.
    fn finished_at(&self, build: &str, at: u64) -> Self {
        match self {
            Self::Asking {
                claim,
                tries,
                since,
                prior,
                ..
            } => Self::Asking {
                claim: claim.clone(),
                tries: *tries,
                since: *since,
                finished: at,
                prior: prior
                    .as_deref()
                    .map(|prior| Box::new(prior.finished_at(build, at))),
            },
            Self::Unfinished {
                build: asked,
                at: when,
                tries,
                ..
            } => Self::Unfinished {
                build: asked.clone(),
                at: *when,
                tries: *tries,
                finished: at,
            },
            Self::Done { build: asked, .. } => Self::Done {
                build: asked.clone(),
                at,
            },
            Self::Declined {
                build: asked,
                at: when,
                declines,
                ..
            } => Self::Declined {
                build: asked.clone(),
                at: *when,
                finished: at,
                declines: *declines,
            },
            Self::Opened => Self::Unfinished {
                build: build.to_owned(),
                at: 0,
                tries: 1,
                finished: at,
            },
        }
    }

    /// The line an asking line's claim carried of the line it replaced ([`carried`]), written as
    /// `build`: its unfinished tries in a row, the last ended at `since`, or the elevation's
    /// finish at `finished`; `None` where it carried neither (it replaced no record).
    fn from_carried(build: String, tries: u32, since: u64, finished: u64) -> Option<Self> {
        if tries > 0 {
            Some(Self::Unfinished {
                build,
                at: since,
                tries,
                finished,
            })
        } else if finished > 0 {
            Some(Self::Done {
                build,
                at: finished,
            })
        } else {
            None
        }
    }

    /// When setup's elevation last finished, as this line says (0: never).
    const fn finished(&self) -> u64 {
        match self {
            Self::Asking { finished, .. }
            | Self::Unfinished { finished, .. }
            | Self::Declined { finished, .. } => *finished,
            Self::Done { at, .. } => *at,
            Self::Opened => 0,
        }
    }

    /// The line `text` is, or `None`.
    fn parse(text: &str) -> Option<Self> {
        let mut words = text.split(' ');
        let kind = words.next()?;
        let line = match kind {
            // `opened` alone, or naming one build.
            "opened" => {
                if build(&mut words).is_none() && text != "opened" {
                    return None;
                }
                Self::Opened
            }
            "asking" => {
                let build = build(&mut words)?;
                let pid = number(&mut words)?;
                let birth = number(&mut words)?;
                let at = number(&mut words)?;
                let n = number(&mut words)?;
                let tries = number(&mut words)?;
                let since = number(&mut words)?;
                let finished = number(&mut words)?;
                let prior = match words.next() {
                    None => None,
                    Some("after") => {
                        let rest: Vec<&str> = words.by_ref().collect();
                        match Self::parse(&rest.join(" "))? {
                            Self::Asking { prior: Some(_), .. } => return None,
                            prior => Some(Box::new(prior)),
                        }
                    }
                    Some(_) => return None,
                };
                Self::Asking {
                    claim: Claim {
                        asker: Asker { build, pid, birth },
                        at,
                        n,
                    },
                    tries,
                    since,
                    finished,
                    prior,
                }
            }
            "unfinished" => Self::Unfinished {
                build: build(&mut words)?,
                at: number(&mut words)?,
                tries: number(&mut words)?,
                finished: number(&mut words)?,
            },
            "done" => Self::Done {
                build: build(&mut words)?,
                at: number(&mut words)?,
            },
            "declined" => Self::Declined {
                build: build(&mut words)?,
                at: number(&mut words)?,
                finished: number(&mut words)?,
                // Written before the count: one decline.
                declines: match words.next() {
                    None => 1,
                    Some(declines) => declines.parse().ok().filter(|declines| *declines > 0)?,
                },
            },
            _ => return None,
        };
        words.next().is_none().then_some(line)
    }
}

/// The next of `words` where it is a build a line can name ([`nameable`]).
fn build<'a>(words: &mut impl Iterator<Item = &'a str>) -> Option<String> {
    words
        .next()
        .filter(|build| nameable(build))
        .map(str::to_owned)
}

/// The next of `words` where it is a number.
fn number<'a, N: std::str::FromStr>(words: &mut impl Iterator<Item = &'a str>) -> Option<N> {
    words.next()?.parse().ok()
}

/// The state a record's `text` holds: its one line, or a historical record's `opened` lines (read
/// as one cut-off attempt). `None` for any other text, an empty one included, and for more lines
/// than a record keeps.
fn state(text: &str) -> Option<Line> {
    let lines: Vec<Line> = text.lines().map(Line::parse).collect::<Option<_>>()?;
    match lines.as_slice() {
        [one] => Some(one.clone()),
        many if (2..=LINES).contains(&many.len())
            && many.iter().all(|line| *line == Line::Opened) =>
        {
            Some(Line::Opened)
        }
        _ => None,
    }
}

/// A defect `CltEpisode`'s conformance plants in this code, one at a time and on its own
/// thread, to show its walk or a law of the model catches it (`model_tests`): each is one of the
/// model's `Buggy=1` mutants. Nothing plants one outside a test.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Defect {
    /// A cut-off attempt reads as a person's decline (`CutOffReadsDeclined`).
    CutOffReadsDeclined,
    /// An automatic claim goes beside a running asker (`ClaimBesideLive`).
    ClaimBesideLive,
    /// An automatic claim goes through a held decline (`ClaimThroughDecline`).
    ClaimThroughDecline,
    /// A hand that writes nothing, so the record names the window, not its tab's program
    /// (`HandLeavesWindow`).
    HandLeavesWindow,
    /// A hand of a claim the line keeps refused, so the window holds no claim while the record
    /// names it (`HandDropsClaim`: the stall the review found, as the code had it).
    HandDropsClaim,
    /// An end of the attempt the line replaced that writes over the line's own
    /// (`StaleEndWrites`).
    StaleEndWrites,
    /// A retry's end that drops the running ask it replaced, so the record no longer names it
    /// (`RetryEndDropsAsk`: as the code had it).
    EndDropsRunningAsk,
    /// A release that counts a try (`ReleaseCountsATry`).
    ReleaseCountsATry,
    /// An unfinished end that forgets the elevation finished (`EndForgetsFinish`).
    EndForgetsFinish,
    /// The window's decision reads aterm's quit hanging its tab's program up as a person's
    /// decline (`InterruptionDeclines`).
    InterruptionDeclines,
    /// The window's decision writes nothing for any end at a quit, the program's own finish
    /// included (`QuitDropsFinish`: as the code had it).
    QuitDropsOwnEnd,
    /// A terminal's decision reads its hangup or a `SIGTERM` as the person's decline
    /// (`HangupDeclinesT`: any stop it caught, as `init_clt::episode_end` had it).
    HangupDeclines,
}

#[cfg(test)]
thread_local! {
    /// The defect planted on this thread ([`Defect`]).
    static PLANTED: std::cell::Cell<Option<Defect>> = const { std::cell::Cell::new(None) };
}

/// Whether `defect` is planted on this thread: never outside a test.
fn planted(defect: Defect) -> bool {
    #[cfg(test)]
    {
        PLANTED.with(|planted| planted.get() == Some(defect))
    }
    #[cfg(not(test))]
    {
        let _ = defect;
        false
    }
}

/// What `line` says of automatic asks at `now` (unix seconds), its asker's process read through
/// `files`: each due read at that clock ([`Backoff::due_by`]), so a line dated after it is due
/// at once, never at a time the clock may take far longer than the schedule's longest pause to
/// reach.
fn asked(files: &dyn Files, line: &Line, now: u64) -> Asked {
    match line {
        // Its asker runs, or the attempt it replaced still asks (a person's retry over a running
        // ask, cut off while that one asks on).
        Line::Asking { prior, .. }
            if asker_runs(files, line)
                || prior
                    .as_deref()
                    .is_some_and(|prior| asker_runs(files, prior)) =>
        {
            Asked::Asking
        }
        Line::Asking { claim, .. } if planted(Defect::CutOffReadsDeclined) => Asked::Declined {
            due: DECLINE_AGAIN.due_by(claim.at, 1, now),
            declines: 1,
        },
        // Cut off: due again as the try it was, from its claim or the unfinished end it carries,
        // whichever came last, which adds no try of its own.
        Line::Asking {
            claim,
            tries,
            since,
            ..
        } => Asked::Unfinished {
            due: ASK_AGAIN.due_by(claim.at.max(*since), *tries, now),
            tries: *tries,
        },
        Line::Unfinished { at, tries, .. } => Asked::Unfinished {
            due: ASK_AGAIN.due_by(*at, *tries, now),
            tries: *tries,
        },
        Line::Done { at, .. } => Asked::Done {
            due: ASK_AGAIN.due_by(*at, 1, now),
        },
        Line::Declined {
            build,
            at,
            finished,
            declines,
        } => match DECLINE_AGAIN.due_by(*at, *declines, now) {
            due if due > now => Asked::Declined {
                due,
                declines: *declines,
            },
            // Its hold passed: what the record would say had nobody declined, as a lift writes.
            _ if *finished > 0 => asked(
                files,
                &Line::Done {
                    build: build.clone(),
                    at: *finished,
                },
                now,
            ),
            _ => Asked::Available,
        },
        // Its end was never written: one cut-off try, long since due.
        Line::Opened => Asked::Unfinished {
            due: ASK_AGAIN.due(0, 1),
            tries: 1,
        },
    }
}

/// Read the durable episode at `now` (unix seconds), independently of the running build.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "CltEpisode",
        action = "Read",
        project = "aterm_types::clt_mark::model_tests::project"
    )
)]
#[must_use]
pub fn reading(files: &dyn Files, path: &Path, now: u64) -> Asked {
    record(files, path, now).asked
}

/// Read the durable episode at `now` (unix seconds), and whether setup's elevation ever finished
/// on this account ([`Record::finished`]), in one read.
#[must_use]
pub fn record(files: &dyn Files, path: &Path, now: u64) -> Record {
    match files.read(path, BYTES) {
        Ok(None) => Record {
            asked: Asked::Available,
            finished: false,
        },
        Ok(Some(text)) => match state(&text) {
            Some(line) => Record {
                asked: asked(files, &line, now),
                finished: line.finished() > 0,
            },
            None => Record {
                asked: Asked::Unread(malformed()),
                finished: false,
            },
        },
        Err(error) => Record {
            asked: Asked::Unread(error),
            finished: false,
        },
    }
}

/// The tries in a row, the last unfinished end and the elevation's last finish a claim over
/// `line` carries on.
fn carried(line: Option<&Line>) -> (u32, u64, u64) {
    let finished = line.map_or(0, Line::finished);
    match line {
        None | Some(Line::Done { .. } | Line::Declined { .. }) => (0, 0, finished),
        Some(Line::Unfinished { at, tries, .. }) => (*tries, *at, finished),
        // A cut-off attempt carries its tries on and adds none, its pause counted from its claim
        // or the unfinished end it carries, as it reads ([`asked`]); a live one is replaced only by
        // a person's retry, which carries them on too.
        Some(Line::Asking {
            claim,
            tries,
            since,
            ..
        }) => (*tries, claim.at.max(*since), finished),
        Some(Line::Opened) => (1, 0, finished),
    }
}

/// Whether `line` is an `asking` line whose asker runs now ([`Files::runs`]).
fn asker_runs(files: &dyn Files, line: &Line) -> bool {
    matches!(line, Line::Asking { claim, .. } if files.runs(claim.asker.pid, claim.asker.birth))
}

/// What a person's retry claims over, of the record's `line`: that line, or, where its asker is
/// gone while the `asking` line it replaced still asks (an earlier retry over a running ask, cut
/// off), that line, so the retry keeps the attempt that still asks and its end. The gone one
/// adds nothing to it, as a cut-off adds no try, and says the same finish: a claim carries its
/// replaced line's finish, and a later finish is written into both ([`Line::finished_at`]).
fn standing<'a>(files: &dyn Files, line: Option<&'a Line>) -> Option<&'a Line> {
    if let Some(
        held @ Line::Asking {
            prior: Some(prior), ..
        },
    ) = line
        && !asker_runs(files, held)
        && asker_runs(files, prior)
    {
        return Some(prior);
    }
    line
}

/// The `asking` line of `claim` over the record's `line` (none where there was no record): it
/// carries that line's tries and last finish on, and keeps the line itself for a release.
fn asking_over(claim: Claim, line: Option<&Line>) -> Line {
    let (tries, since, finished) = carried(line);
    Line::Asking {
        claim,
        tries,
        since,
        finished,
        prior: line.map(|line| Box::new(line.as_prior())),
    }
}

/// Claim the episode for an automatic ask by `asker` at `now` (unix seconds), under the shared
/// lock: only where the record lets one go now ([`Asked::may_ask`]). `None`, rewriting nothing,
/// where another asker runs, the next try is not due yet, or a person's decline holds.
///
/// # Errors
/// An unsafe or malformed record, an asker no line can name, or a lock/write error.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "CltEpisode",
        action = "ClaimW",
        project = "aterm_types::clt_mark::model_tests::project"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "CltEpisode",
        action = "ClaimT",
        project = "aterm_types::clt_mark::model_tests::project"
    )
)]
pub fn claim(files: &dyn Files, path: &Path, asker: &Asker, now: u64) -> io::Result<Option<Claim>> {
    change(files, path, Some(&asker.build), |files, line| {
        let may = line.map_or(Asked::Available, |line| asked(files, line, now));
        let defect = match may {
            Asked::Asking => planted(Defect::ClaimBesideLive),
            Asked::Declined { .. } => planted(Defect::ClaimThroughDecline),
            _ => false,
        };
        if !may.may_ask(now) && !defect {
            return Ok((None, None));
        }
        let claim = Claim {
            asker: asker.clone(),
            at: now,
            n: fresh(),
        };
        Ok((
            Some(Write::Line(asking_over(claim.clone(), line))),
            Some(claim),
        ))
    })
}

/// Claim the episode for a person's own retry by `asker` at `now`, whatever the record says
/// (a decline, a try not yet due, another attempt asking): their press or typed `aterm init` is
/// their yes. The line keeps the attempt it replaced, one deep, whose late end then writes only
/// into its own place under the retry ([`end`]); over a cut-off retry whose replaced attempt
/// still asks, it keeps that attempt instead.
///
/// # Errors
/// As [`claim`]. Explicit callers keep their person-granted authority on recording failure.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "CltEpisode",
        action = "ExplicitO",
        project = "aterm_types::clt_mark::model_tests::project"
    )
)]
pub fn explicit(files: &dyn Files, path: &Path, asker: &Asker, now: u64) -> io::Result<Claim> {
    change(files, path, Some(&asker.build), |files, line| {
        let claim = Claim {
            asker: asker.clone(),
            at: now,
            n: fresh(),
        };
        Ok((
            Some(Write::Line(asking_over(
                claim.clone(),
                standing(files, line),
            ))),
            claim,
        ))
    })
}

/// A person's yes to setup (System State's Set up now) lifts their earlier decline: a
/// `declined` record goes back to what it would say had nobody declined (the elevation's last
/// finish, or no record), so an automatic ask may go again. `true` where it lifted one; any
/// other record is left as it is.
///
/// # Errors
/// An unsafe or malformed record, or a lock/write/removal error.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "CltEpisode",
        action = "Lift",
        project = "aterm_types::clt_mark::model_tests::project"
    )
)]
pub fn lift(files: &dyn Files, path: &Path) -> io::Result<bool> {
    change(files, path, None, |_, line| match line {
        Some(Line::Declined {
            build, finished, ..
        }) => Ok((
            Some(if *finished > 0 {
                Write::Line(Line::Done {
                    build: build.clone(),
                    at: *finished,
                })
            } else {
                Write::Remove
            }),
            true,
        )),
        _ => Ok((None, false)),
    })
}

/// Carry the attempt of an earlier build's Mac Setup tab, whose live program an update handed to
/// this build, into the record as `asker`'s (that program) at `now`. Such a build wrote no line
/// this one reads the end of (a historical `opened` record, whose end nobody writes), so where
/// the record holds that, or no record at all, it becomes the `asking` line a claim over it
/// would write: no other automatic asker asks beside the tab while its program runs, the window
/// that adopted the tab records its end against the claim this returns, and once the program
/// is gone with no end written the attempt reads cut off. Where the record already names
/// `asker` asking, in its line or the one that line keeps (this tab carried before, and a
/// person's retry over it since), that claim, rewriting nothing: the attempt stays the adopting
/// window's to end, in its own place. `None`, rewriting nothing, where it holds anything else:
/// an attempt of this format another asker made, or how one ended.
///
/// # Errors
/// An unsafe or malformed record, an asker no line can name, or a lock/write error.
pub fn carry(files: &dyn Files, path: &Path, asker: &Asker, now: u64) -> io::Result<Option<Claim>> {
    change(files, path, Some(&asker.build), |_, line| match line {
        None | Some(Line::Opened) => {
            let claim = Claim {
                asker: asker.clone(),
                at: now,
                n: fresh(),
            };
            Ok((
                Some(Write::Line(asking_over(claim.clone(), line))),
                Some(claim),
            ))
        }
        Some(Line::Asking { claim, .. }) if claim.asker == *asker => {
            Ok((None, Some(claim.clone())))
        }
        Some(Line::Asking {
            prior: Some(prior), ..
        }) => match prior.as_ref() {
            Line::Asking { claim, .. } if claim.asker == *asker => Ok((None, Some(claim.clone()))),
            _ => Ok((None, None)),
        },
        Some(_) => Ok((None, None)),
    })
}

/// Hand `claim`'s attempt to the process `pid` born `birth` (the Mac Setup tab's own program,
/// which asks for the password): the record's asker becomes that process, so the attempt is
/// cut off when it ends, whatever becomes of the window that claimed it. Where a person's retry
/// took the episode over first, the line it keeps names the program instead, so the attempt
/// stays the window's to end: back on the record once that retry gives it back or ends, never
/// a line naming the window that nobody ends. `None`, rewriting nothing, where the record holds
/// `claim` in neither place (three attempts asked at once, the oldest given up).
///
/// # Errors
/// An unsafe or malformed record, or a lock/write error.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "CltEpisode",
        action = "Hand",
        project = "aterm_types::clt_mark::model_tests::project"
    )
)]
pub fn hand(
    files: &dyn Files,
    path: &Path,
    claim: &Claim,
    pid: i32,
    birth: u64,
) -> io::Result<Option<Claim>> {
    let handed = Claim {
        asker: Asker {
            build: claim.asker.build.clone(),
            pid,
            birth,
        },
        at: claim.at,
        n: claim.n,
    };
    change(
        files,
        path,
        Some(&claim.asker.build),
        |_, line| match line {
            Some(Line::Asking { claim: held, .. })
                if held == claim && planted(Defect::HandLeavesWindow) =>
            {
                Ok((None, Some(held.clone())))
            }
            Some(line @ Line::Asking { claim: held, .. }) if held == claim => {
                Ok((Some(Write::Line(line.handed(handed.clone()))), Some(handed)))
            }
            Some(Line::Asking {
                claim: holder,
                tries,
                since,
                finished,
                prior: Some(replaced),
            }) if matches!(replaced.as_ref(), Line::Asking { claim: held, .. } if held == claim)
                && !planted(Defect::HandDropsClaim) =>
            {
                Ok((
                    Some(Write::Line(Line::Asking {
                        claim: holder.clone(),
                        tries: *tries,
                        since: *since,
                        finished: *finished,
                        prior: Some(Box::new(replaced.handed(handed.clone()))),
                    })),
                    Some(handed),
                ))
            }
            _ => Ok((None, None)),
        },
    )
}

/// Record how `claim`'s attempt ended at `now`, and say where the record held that claim
/// ([`Took`]): as its line, or as the line that line keeps, and it now says so; or in neither
/// (three attempts asked at once, the oldest given up), where a finish is still the account's,
/// so the record then says the elevation finished ([`Record::finished`]), and any other end
/// rewrites nothing.
///
/// # Errors
/// An unsafe or malformed record, or a lock/write/removal error.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "CltEpisode",
        action = "DoneW",
        project = "aterm_types::clt_mark::model_tests::project"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "CltEpisode",
        action = "UnfinishedW",
        project = "aterm_types::clt_mark::model_tests::project"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "CltEpisode",
        action = "DeclinedW",
        project = "aterm_types::clt_mark::model_tests::project"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "CltEpisode",
        action = "ClosedW",
        project = "aterm_types::clt_mark::model_tests::project"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "CltEpisode",
        action = "ReleasedW",
        project = "aterm_types::clt_mark::model_tests::project"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "CltEpisode",
        action = "DoneT",
        project = "aterm_types::clt_mark::model_tests::project"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "CltEpisode",
        action = "UnfinishedT",
        project = "aterm_types::clt_mark::model_tests::project"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "CltEpisode",
        action = "DeclinedT",
        project = "aterm_types::clt_mark::model_tests::project"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "CltEpisode",
        action = "ReleasedT",
        project = "aterm_types::clt_mark::model_tests::project"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "CltEpisode",
        action = "InterruptedT",
        project = "aterm_types::clt_mark::model_tests::project"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "CltEpisode",
        action = "DoneO",
        project = "aterm_types::clt_mark::model_tests::project"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "CltEpisode",
        action = "UnfinishedO",
        project = "aterm_types::clt_mark::model_tests::project"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "CltEpisode",
        action = "DeclinedO",
        project = "aterm_types::clt_mark::model_tests::project"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "CltEpisode",
        action = "ReleasedO",
        project = "aterm_types::clt_mark::model_tests::project"
    )
)]
pub fn end(files: &dyn Files, path: &Path, claim: &Claim, how: End, now: u64) -> io::Result<Took> {
    change(files, path, Some(&claim.asker.build), |files, line| {
        let build = claim.asker.build.clone();
        // The attempt the line holds ends: the line says how, or, where the attempt it replaced
        // still asks (a person's retry over a running ask), that attempt asks on over how this
        // one ended, as a claim over that end would, so no automatic ask goes beside it and its
        // end stays its own to write. A release gives back exactly the line it replaced.
        if let Some(Line::Asking {
            claim: held,
            tries,
            since,
            finished,
            prior,
        }) = line
            && held == claim
        {
            let ended = ending(
                build,
                (*tries, *since, *finished),
                prior.as_deref(),
                how,
                now,
            );
            let write = match prior.as_deref() {
                Some(running @ Line::Asking { claim: on, .. })
                    if how != End::Released
                        && asker_runs(files, running)
                        && !planted(Defect::EndDropsRunningAsk) =>
                {
                    Write::Line(asking_over(on.clone(), ended.as_ref()))
                }
                _ => ended.map_or(Write::Remove, Write::Line),
            };
            return Ok((Some(write), Took::Line));
        }
        // The attempt the line replaced ends while the line's own goes on (the window's tab,
        // still asking when a person's retry took the episode over): how it ended takes the
        // replaced line's place, under the line's own attempt, which carries it on as a claim
        // over it would. Where the line's own asker is gone too, that end is the line: a cut-off
        // adds nothing.
        if let Some(
            holding @ Line::Asking {
                claim: holder,
                finished,
                prior: Some(replaced),
                ..
            },
        ) = line
            && let Line::Asking {
                claim: held,
                tries,
                since,
                finished: theirs,
                ..
            } = replaced.as_ref()
            && held == claim
        {
            let ended = ending(build, (*tries, *since, *theirs), None, how, now);
            let write = if asker_runs(files, holding) && !planted(Defect::StaleEndWrites) {
                let (tries, since, carried_finish) = carried(ended.as_ref());
                Write::Line(Line::Asking {
                    claim: holder.clone(),
                    tries,
                    since,
                    finished: carried_finish.max(*finished),
                    prior: ended.map(Box::new),
                })
            } else {
                ended.map_or(Write::Remove, Write::Line)
            };
            return Ok((Some(write), Took::Replaced));
        }
        // An attempt the record holds in neither place (three asked at once, and it the oldest,
        // given up one deep): how it ended is not the record's to say, but a finish is the
        // account's whoever holds it: the line, and the line it keeps, say so, and no line
        // becomes the finish.
        if how == End::Done {
            let finish = line.map_or_else(
                || Line::Done {
                    build: build.clone(),
                    at: now,
                },
                |line| line.finished_at(&build, now),
            );
            return Ok((Some(Write::Line(finish)), Took::Nowhere));
        }
        Ok((None, Took::Nowhere))
    })
}

/// The line the end `how` at `now` of an attempt asked as `build` writes: its claim carried
/// `carried`'s unfinished tries in a row, the last ended at its `since`, and the elevation's
/// finish at its `finished` ([`carried`]), over the line `prior` it replaced (`None` where it
/// replaced none, or where that one was dropped, kept one deep). `None` for no line.
fn ending(
    build: String,
    (tries, since, finished): (u32, u64, u64),
    prior: Option<&Line>,
    how: End,
    now: u64,
) -> Option<Line> {
    match how {
        End::Done => Some(Line::Done { build, at: now }),
        // One more decline in a row where the claim replaced one (a person's own retry, or an
        // automatic ask once its hold passed); else the first.
        End::Declined => Some(Line::Declined {
            build,
            at: now,
            finished,
            declines: match prior {
                Some(Line::Declined { declines, .. }) => declines.saturating_add(1),
                _ => 1,
            },
        }),
        End::Unfinished => Some(Line::Unfinished {
            build,
            at: now,
            tries: tries.saturating_add(1),
            finished: if planted(Defect::EndForgetsFinish) {
                0
            } else {
                finished
            },
        }),
        End::Released if planted(Defect::ReleaseCountsATry) => Some(Line::Unfinished {
            build,
            at: now,
            tries: tries.saturating_add(1),
            finished,
        }),
        // Exactly what the record held before the claim: a decline, a finish, another asker's
        // live attempt or an unfinished try's schedule, or nothing. Where that line was dropped
        // (a person's retry over this claim gave the record back without it, kept one deep),
        // what the claim carried of it: its unfinished tries' schedule, or its finish.
        End::Released => match prior {
            Some(prior) => Some(prior.clone()),
            None => Line::from_carried(build, tries, since, finished),
        },
    }
}

/// How the Mac Setup tab's program ended, as the window that opened its tab reads it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TabExit {
    /// It exited with this status of its own.
    Code(i32),
    /// A signal ended it.
    Signal,
}

/// How the window records the end of its Mac Setup tab's attempt ([`end`]), from the program's
/// `exit` (`None` where the window saw no status: its end-of-file came first, or the launch that
/// adopted the tab found the program gone), whether its tab was `closed` while the program ran
/// and aterm ran on, and whether it goes with aterm's `quit` (the close of the last window,
/// noted before its end was read). `None`: no end is written, so once its asker is gone the
/// record reads the attempt cut off, an unfinished try asked again by itself, never a decline.
///
/// - At a quit, what its hangup ends the program with writes nothing: a signal, no status, or
///   the stop's code ([`crate::macos_setup::COMMAND_LINE_TOOLS_TAB_STOPPED`]), which the program
///   exits with for any stop it caught, a hangup included, so it cannot tell a person's Ctrl-C
///   from the quit's. Any other status of the program's own is its end, quit or not: a finish
///   is never lost to a quit that came just after it.
/// - 0 is done; the stop's code a person's decline (a Ctrl-C at the password prompt or later,
///   System State's Stop, the tab closed while aterm ran on); Low Data Mode's, a deferral before
///   the password ask (another login holds the console, or an installer not the tab's runs), and
///   the codes of a program that never ran ([`never_ran`]) give the claim back, as nothing was
///   asked; any other status, and a signal, unfinished (Apple's window closed without the tools,
///   whose button nothing tells, an account no administrator's, a refused or untyped password, a
///   failure).
/// - With no status seen: a decline where its tab was closed while it ran (someone said no), else
///   unfinished.
#[must_use]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "CltEpisode",
        action = "DoneW",
        project = "aterm_types::clt_mark::model_tests::project"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "CltEpisode",
        action = "UnfinishedW",
        project = "aterm_types::clt_mark::model_tests::project"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "CltEpisode",
        action = "DeclinedW",
        project = "aterm_types::clt_mark::model_tests::project"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "CltEpisode",
        action = "ClosedW",
        project = "aterm_types::clt_mark::model_tests::project"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "CltEpisode",
        action = "ReleasedW",
        project = "aterm_types::clt_mark::model_tests::project"
    )
)]
pub fn tab_end(exit: Option<TabExit>, closed: bool, quit: bool) -> Option<End> {
    use crate::macos_setup::{
        COMMAND_LINE_TOOLS_TAB_INSTALLER_HELD, COMMAND_LINE_TOOLS_TAB_LOW_DATA,
        COMMAND_LINE_TOOLS_TAB_NOT_OWN_LOGIN, COMMAND_LINE_TOOLS_TAB_STOPPED,
    };
    let stopped = |code: i32| code == i32::from(COMMAND_LINE_TOOLS_TAB_STOPPED);
    // Codes the program exits with only before its password ask.
    let before_the_ask = |code: i32| {
        [
            COMMAND_LINE_TOOLS_TAB_LOW_DATA,
            COMMAND_LINE_TOOLS_TAB_NOT_OWN_LOGIN,
            COMMAND_LINE_TOOLS_TAB_INSTALLER_HELD,
        ]
        .into_iter()
        .any(|before| code == i32::from(before))
    };
    if quit {
        if !matches!(exit, Some(TabExit::Code(code)) if !stopped(code)) {
            return planted(Defect::InterruptionDeclines).then_some(End::Declined);
        }
        if planted(Defect::QuitDropsOwnEnd) {
            return None;
        }
    }
    Some(match exit {
        Some(TabExit::Code(0)) => End::Done,
        Some(TabExit::Code(code)) if stopped(code) => End::Declined,
        Some(TabExit::Code(code)) if before_the_ask(code) || never_ran(code) => End::Released,
        Some(TabExit::Code(_) | TabExit::Signal) => End::Unfinished,
        None if closed => End::Declined,
        None => End::Unfinished,
    })
}

/// What stopped a terminal `aterm init` while its claimed attempt ran, as its own process caught
/// it ([`TermExit::Unfinished`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stop {
    /// A key the person typed at that terminal, which it sends the run's whole foreground group,
    /// `sudo` and the root half with it: Ctrl-C (`SIGINT`) or Ctrl-\ (`SIGQUIT`). Their no.
    Person,
    /// What nobody at that terminal sent: its hangup (`SIGHUP`: it closed, the aterm it ran in
    /// quit, a logout ended its login), a `SIGTERM` (a logout, launchd, an update's handoff, a
    /// kill), or a root half that says it stopped while the run itself caught no stop.
    Interruption,
}

/// How a terminal `aterm init`'s claimed attempt ended, as its run reads it ([`term_end`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TermExit {
    /// `sudo` provably never started: nothing was asked.
    NotStarted,
    /// The root half finished, or the tools arrived anyway.
    Finished {
        /// The read-back after it finds the tools in place where they were to be installed, and
        /// no root step owed.
        settled: bool,
    },
    /// It did not finish: a refused or untyped password, a download, catalog or installer
    /// failure, Apple's window closed without the tools, a lost wait, or a stop.
    Unfinished {
        /// What stopped it, where the run caught a stop or the root half says it stopped.
        stop: Option<Stop>,
    },
}

/// How a terminal `aterm init` records its claimed attempt's end ([`end`]), from how its run
/// ended (`exit`): only a run that proves `sudo` never started gives the claim back; a finish is
/// done only as read back after it; a person's key at that terminal is their decline; anything
/// else is unfinished, asked again by itself on [`ASK_AGAIN`]'s schedule, an interruption nobody
/// at that terminal sent included (a logout's `SIGTERM`, the terminal's hangup), never a decline.
/// The run knows which stop it caught and writes the end it can; the window's quit cannot tell its
/// hangup from a person's Ctrl-C, so it writes none ([`tab_end`]).
#[must_use]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "CltEpisode",
        action = "DoneT",
        project = "aterm_types::clt_mark::model_tests::project"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "CltEpisode",
        action = "UnfinishedT",
        project = "aterm_types::clt_mark::model_tests::project"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "CltEpisode",
        action = "DeclinedT",
        project = "aterm_types::clt_mark::model_tests::project"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "CltEpisode",
        action = "ReleasedT",
        project = "aterm_types::clt_mark::model_tests::project"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "CltEpisode",
        action = "InterruptedT",
        project = "aterm_types::clt_mark::model_tests::project"
    )
)]
pub fn term_end(exit: TermExit) -> End {
    match exit {
        TermExit::NotStarted => End::Released,
        TermExit::Finished { settled: true } => End::Done,
        TermExit::Finished { settled: false } => End::Unfinished,
        TermExit::Unfinished {
            stop: Some(Stop::Person),
        } => End::Declined,
        TermExit::Unfinished {
            stop: Some(Stop::Interruption),
        } if planted(Defect::HangupDeclines) => End::Declined,
        TermExit::Unfinished { .. } => End::Unfinished,
    }
}

/// Whether the tab's exit `code` says its program never started the install: its own word for a
/// go line that never came or came behind other typing
/// ([`crate::macos_setup::COMMAND_LINE_TOOLS_TAB_NOT_STARTED`]), and `/usr/bin/env`'s own when
/// it cannot run the tab's `aterm` (126: found but not runnable, 127: not found), which
/// `aterm init` never exits with.
#[must_use]
pub fn never_ran(code: i32) -> bool {
    code == i32::from(crate::macos_setup::COMMAND_LINE_TOOLS_TAB_NOT_STARTED)
        || matches!(code, 126 | 127)
}

/// The `CltEpisode` actions the record does not perform, named where the whole-source closure
/// (aterm-models' `spec_xref_closure`) collects them: the askers' processes ending, aterm's quit
/// and time passing, which the conformance's rig does, and the model's `Buggy=1` mutants, which
/// its negative control plants in this code (`Defect`).
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "CltEpisode",
        action = "WindowGoes",
        reason = "The environment: the window's process ends (an update's handoff, a crash) and \
                  its tab's program, which the next window adopts, asks on; a claim naming the \
                  window reads cut off."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "CltEpisode",
        action = "QuitNoted",
        reason = "The environment: the close of aterm's last window notes its quit while the Mac \
                  Setup tab's program asks, before the program's end is read."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "CltEpisode",
        action = "AtermQuits",
        reason = "The environment: aterm ends (a quit, a crash, a logout, the Mac's restart) \
                  with the window and its tab's program; what the quit's hangup ends the program \
                  with writes no end (`tab_end`, driven outside a step)."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "CltEpisode",
        action = "ProgramGoes",
        reason = "The environment: the Mac Setup tab's program is killed, writing no end."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "CltEpisode",
        action = "TermGoes",
        reason = "The environment: a terminal `aterm init` is killed, writing no end."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "CltEpisode",
        action = "OwnGoes",
        reason = "The environment: the process of a person's own retry is killed, writing no \
                  end."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "CltEpisode",
        action = "Tick",
        reason = "The environment: the clock passes every pause, a decline's long hold \
                  included; every reading and change takes the clock as an argument."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "CltEpisode",
        action = "CutOffReadsDeclined",
        reason = "The Buggy=1 mutant: a cut-off attempt read as a decline; planted in `asked` \
                  by the negative control."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "CltEpisode",
        action = "ClaimBesideLive",
        reason = "The Buggy=1 mutant: an automatic claim beside a running asker; planted in \
                  `claim` by the negative control."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "CltEpisode",
        action = "ClaimThroughDecline",
        reason = "The Buggy=1 mutant: an automatic claim through a held decline; planted in \
                  `claim` by the negative control."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "CltEpisode",
        action = "HandLeavesWindow",
        reason = "The Buggy=1 mutant: a hand that leaves the record naming the window; \
                  planted in `hand` by the negative control."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "CltEpisode",
        action = "HandDropsClaim",
        reason = "The Buggy=1 mutant: a hand of a claim the line keeps refused, the window left \
                  holding none; planted in `hand` by the negative control."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "CltEpisode",
        action = "StaleEndWrites",
        reason = "The Buggy=1 mutant: an end of the attempt the line replaced that writes over \
                  the line's own; planted in `end` by the negative control."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "CltEpisode",
        action = "RetryEndDropsAsk",
        reason = "The Buggy=1 mutant: a retry's end that drops the running ask it replaced; \
                  planted in `end` by the negative control."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "CltEpisode",
        action = "ReleaseCountsATry",
        reason = "The Buggy=1 mutant: a release that counts a try; planted in `end` by the \
                  negative control."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "CltEpisode",
        action = "EndForgetsFinish",
        reason = "The Buggy=1 mutant: an unfinished end that forgets the elevation finished; \
                  planted in `end` by the negative control."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "CltEpisode",
        action = "InterruptionDeclines",
        reason = "The Buggy=1 mutant: aterm's quit hanging the tab's program up read as a \
                  decline; planted in `tab_end` by the negative control."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "CltEpisode",
        action = "QuitDropsFinish",
        reason = "The Buggy=1 mutant: the program's own finish dropped at a quit; planted in \
                  `tab_end` by the negative control."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "CltEpisode",
        action = "HangupDeclinesT",
        reason = "The Buggy=1 mutant: a terminal's hangup or a SIGTERM read as the person's \
                  decline; planted in `term_end` by the negative control."
    )
)]
#[doc(hidden)]
pub fn clt_episode_environment() {}

/// What a change writes.
enum Write {
    /// The record becomes this one line.
    Line(Line),
    /// The record goes.
    Remove,
}

/// Read the record at `path` under its lock and make the change `decide` returns for its line
/// (`None` where there is no record), with `decide`'s answer. `build`, the build a written line
/// names where the change names one, must be nameable ([`nameable`]).
fn change<T>(
    files: &dyn Files,
    path: &Path,
    build: Option<&str>,
    decide: impl FnOnce(&dyn Files, Option<&Line>) -> io::Result<(Option<Write>, T)>,
) -> io::Result<T> {
    if build.is_some_and(|build| !nameable(build)) {
        return Err(unnameable());
    }
    let _held = files.lock(path)?;
    let line = match files.read(path, BYTES)? {
        None => None,
        Some(text) => Some(state(&text).ok_or_else(malformed)?),
    };
    let (write, answer) = decide(files, line.as_ref())?;
    match write {
        Some(Write::Line(line)) => files.write(path, &line.text())?,
        Some(Write::Remove) => files.remove(path)?,
        None => {}
    }
    Ok(answer)
}

/// Why a build no line could name has no reading and no change.
fn unnameable() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "no line of the record can name this build",
    )
}

/// Why a malformed record has no reading and is left as found.
fn malformed() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "the record is malformed and is left as found",
    )
}

/// Whether a line can name `build`: one word, no longer than [`BUILD_BYTES`].
fn nameable(build: &str) -> bool {
    (1..=BUILD_BYTES).contains(&build.len()) && !build.contains(char::is_whitespace)
}

/// The lock file beside the record at `path`: its name and `.lock`.
#[cfg(not(target_family = "wasm"))]
fn lock_path(path: &Path) -> io::Result<PathBuf> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "the record has no name"))?;
    Ok(path.with_file_name(format!("{name}.lock")))
}

/// The folder the record at `path` sits in (`.` for a bare name).
#[cfg(not(target_family = "wasm"))]
fn folder(path: &Path) -> &Path {
    match path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir,
        _ => Path::new("."),
    }
}

/// This Mac's files and processes: a read bounded to a regular file, never through a link and
/// never waiting on a pipe ([`aterm_uds::runs::read_bounded_regular_text`]), a write through a
/// file of this process's own beside the record, flushed durably and renamed over it, then its
/// folder flushed, a lock that is an exclusive `flock` of a file of its own beside the record
/// (its name and `.lock`, [`aterm_uds::file_lock::LockFile`]: the record itself is replaced by
/// each write, so its own inode holds no lock), waited for up to [`LOCK_WAIT`], and a process
/// read by its pid and birth ([`aterm_uds::process::read_birth`]).
#[cfg(not(target_family = "wasm"))]
#[derive(Debug, Clone, Copy, Default)]
pub struct Live;

/// The record's lock for a change that started at `started` and waits at most `wait` from then
/// for another asker's hold to go: a change whose wait has already run out fails `TimedOut`
/// before it looks, and one still waiting on a held lock at its deadline fails `WouldBlock`.
#[cfg(not(target_family = "wasm"))]
fn lock_within(
    path: &Path,
    started: std::time::Instant,
    wait: std::time::Duration,
) -> io::Result<Held> {
    let deadline = started + wait;
    if std::time::Instant::now() >= deadline {
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "Command Line Tools record lock did not finish",
        ));
    }
    let lock = lock_path(path)?;
    std::fs::create_dir_all(folder(path))?;
    aterm_uds::file_lock::LockFile::take(
        &lock,
        deadline,
        "another aterm is changing the record of its Command Line Tools ask",
    )
    .map(Held::new)
}

#[cfg(not(target_family = "wasm"))]
impl Files for Live {
    fn read(&self, path: &Path, max: usize) -> io::Result<Option<String>> {
        match aterm_uds::runs::read_bounded_regular_text(path, max as u64) {
            Ok(text) => Ok(Some(text)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    fn write(&self, path: &Path, text: &str) -> io::Result<()> {
        use std::io::Write as _;
        let dir = folder(path);
        std::fs::create_dir_all(dir)?;
        if std::fs::symlink_metadata(path).is_ok_and(|meta| !meta.file_type().is_file()) {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "the record's path is held by something that is not a file",
            ));
        }
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "the record has no name"))?;
        let staged = path.with_file_name(format!("{name}.tmp-{}", std::process::id()));
        // One an earlier process of this pid left as it died mid-write (before its rename) would
        // hold every later change: the name is this live process's own, under the record's lock
        // ([`Files::lock`]), and a link there goes, not where it points.
        let _ = std::fs::remove_file(&staged);
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options
                .mode(0o600)
                .custom_flags(aterm_uds::atomic_file::LOCK_OPEN_FLAGS);
        }
        let mut file = options.open(&staged)?;
        let written = file
            .write_all(text.as_bytes())
            .and_then(|()| aterm_durable::durable(&file));
        drop(file);
        let written = written.and_then(|()| std::fs::rename(&staged, path));
        if written.is_err() {
            let _ = std::fs::remove_file(&staged);
        }
        written?;
        // The record is in place; a folder that cannot be flushed here (a network home that takes
        // no full flush) keeps it, as the window's other marks are kept.
        let _ = aterm_durable::durable_dir(dir);
        Ok(())
    }

    fn remove(&self, path: &Path) -> io::Result<()> {
        match std::fs::remove_file(path) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            done => done,
        }
    }

    fn lock(&self, path: &Path) -> io::Result<Held> {
        lock_within(path, std::time::Instant::now(), LOCK_WAIT)
    }

    fn runs(&self, pid: i32, birth: u64) -> bool {
        pid > 1 && aterm_uds::process::read_birth(pid).is_some_and(|read| read.0 == birth)
    }
}

/// This Mac's files and processes as a window's event loop changes the record: [`Live`]'s, its
/// lock waited for no longer than [`LOOP_LOCK_WAIT`], so a stalled holder (an `aterm init`
/// stopped or swapped out while it held the lock) fails the window's change instead of freezing
/// every window behind it.
#[cfg(not(target_family = "wasm"))]
#[derive(Debug, Clone, Copy, Default)]
pub struct OnLoop;

#[cfg(not(target_family = "wasm"))]
impl Files for OnLoop {
    fn read(&self, path: &Path, max: usize) -> io::Result<Option<String>> {
        Live.read(path, max)
    }

    fn write(&self, path: &Path, text: &str) -> io::Result<()> {
        Live.write(path, text)
    }

    fn remove(&self, path: &Path) -> io::Result<()> {
        Live.remove(path)
    }

    fn lock(&self, path: &Path) -> io::Result<Held> {
        lock_within(path, std::time::Instant::now(), LOOP_LOCK_WAIT)
    }

    fn runs(&self, pid: i32, birth: u64) -> bool {
        Live.runs(pid, birth)
    }
}

#[cfg(test)]
mod model_tests;

#[cfg(all(test, unix))]
#[path = "clt_mark_tests.rs"]
mod tests;
