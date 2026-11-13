// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Command Line Tools install, an early step of a Mac's first launch. A launch that finds
//! Apple's Command Line Tools (git, cc, the macOS SDK) missing, which only a root install with the
//! person's password adds, asks for them at once, with no setup pass and no page to wait for: the
//! window opens a terminal tab titled `Mac Setup` ([`TAB_TITLE`]) in front of its current
//! tab, whose own program is `aterm init --only base.clt`, in plain words
//! ([`aterm_types::macos_setup::COMMAND_LINE_TOOLS_TAB`]): its one Mac password prompt is the
//! person's, in the tab, and a run parked on the tools wakes once they are there (System State
//! design §3.3). Until they exist the shim keeps Apple's own dialog away from everything aterm
//! starts ([`crate::clt_shim`]).
//!
//! aterm opens the tab by itself where the account's record of setup's administrator ask lets an
//! automatic ask go now ([`Mark`], which the package follow-up's `aterm init` shares): no other
//! asker's attempt runs, no person declined the last ask (a Stop, a Ctrl-C at the prompt, the tab
//! closed while it asked: until their Set up now, Install… or typed `aterm init`), and the last
//! attempt's next try is due: an attempt cut off (aterm quit or crashed with no keeper to carry
//! its program, the Mac restarted) is
//! due again at once, one that did not finish (Apple's window closed without the tools included)
//! after a pause that doubles from ten minutes up to twelve hours, whatever build or launch reads
//! it, in a tab behind the window's active one; once no macOS dialog is up, no installer is up,
//! no tab an earlier aterm opened runs on with no custody here, the keyboard was quiet for 2.5 s,
//! Low Data Mode is not holding the download back (a tab that found it on spends no opening, and
//! the next opens once it is off), and the account is one of the Mac's administrators, whom alone
//! macOS lets install them, while the tools are still missing: their arrival by any route ends
//! the request. Which window has focus, and what it shows, holds nothing back. A pass that finds
//! the tools missing later (they were there at launch) asks
//! too, under its run's admission. The GitHub repositories row's Install… opens it on a person's
//! press at any time ([`App::press_developer_tools_install`]); a press while a macOS dialog aterm
//! opened is up is kept, and the tab opens once that dialog closes ([`Install::PressedQueued`]).
//! The page reads where the install stands
//! ([`App::developer_tools_install_step`]): the tab asking for the password, the installer
//! running, Apple's own installer window waiting for the person, a press waiting, or why the
//! last press opened nothing.
//! Keys typed while it spawns stay where they were ([`crate::tab_spawn::SpawnAsk::unasked`]).
//! The same checks run before landing: a busy or typed-into destination defers the request
//! without opening an empty tab or consuming its attempt. Stop withdraws an unopened request
//! and interrupts only the exact receipt-bound installer child already running.
//! The tab's program waits for the window's go line, typed once it landed, before it does
//! anything. Its exact child holds off other setup prompts while it may still ask
//! ([`App::developer_tools_install_may_ask`]): until the root run its own program started is seen
//! alive, past the Mac password, while a quit could carry it, and again whenever Apple's installer
//! window is up or an installer runs that is not that run's. aterm's quit leaves such a program
//! running in the keeper's custody, for the next launch to adopt with its receipt
//! ([`App::developer_tools_custody_at_exit`]); a manifest a later launch restores from leaves the
//! tab out, which only an update's handoff describes ([`App::is_developer_tools_session`]). The
//! finished terminal keeps the result visible, and a program that ended without saying how gets
//! the window's end line.

use crate::App;
use crate::Wake;
use crate::system_state::{NotOpened, ToolsInstall};
use aterm_types::macos_setup::Observation;

#[cfg(all(target_os = "macos", debug_assertions))]
mod lab_fixture;
mod probe;
mod receipt;
mod stop;
pub(crate) use stop::Reach as StopReach;

/// The "revision" of the want a launch that found the tools missing makes itself: newer than any
/// pass, so only a person's Stop of a run or the tools' arrival ([`App::developer_tools_watch`])
/// withdraws it, as a pass that no longer waits on the tools for its own Repositories (packaged git
/// serves them) says nothing of the tools themselves. A Stop of a choice waiting for another setup
/// leaves it, though a durable Stop that lands on the last run still holds it
/// ([`App::developer_tools_install_authorized`]).
pub(crate) const LAUNCH_WANT: u64 = u64::MAX;

/// Where this launch's install stands.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Install {
    /// Nothing waits on the tools.
    #[default]
    Unasked,
    /// This launch ([`LAUNCH_WANT`]), or this exact run's last pass, waits on them: its tab opens
    /// once it is due ([`Install::due`]).
    Wanted(u64),
    /// A shell is starting, with the latest request retained until it can land.
    /// Stop or a pass that no longer needs the tools can withdraw it meanwhile.
    Opening { wanted: Option<u64> },
    /// A person pressed Install… while a macOS permission gesture held the dialog slot: queued
    /// so the row explains why and the tab opens once the slot frees and the keyboard has been
    /// quiet for 2.5 s.
    PressedQueued { wanted: Option<u64> },
    /// A person's press of Install… started a shell, which lands once its own check passes
    /// ([`App::press_developer_tools_install`]), whatever run is admitted.
    Pressed,
    /// The tab was opened (or its opening failed): no other opens beside it. Its end decides
    /// what comes next ([`App::developer_tools_install_ended`]): an attempt that did not finish
    /// stands as a want again, held by the account's record until its next try, and a decline
    /// leaves it here.
    Opened,
}

impl Install {
    /// A fixture injects a revision's want, unless its tab already opened.
    #[cfg(test)]
    #[must_use]
    pub(crate) const fn asked(self, revision: u64) -> Self {
        match self {
            Self::Unasked => Self::Wanted(revision),
            Self::Wanted(previous) if revision >= previous => Self::Wanted(revision),
            Self::Opening { wanted } => Self::Opening {
                wanted: Some(match wanted {
                    Some(previous) if previous > revision => previous,
                    _ => revision,
                }),
            },
            Self::PressedQueued { wanted } => Self::PressedQueued {
                wanted: Some(match wanted {
                    Some(previous) if previous > revision => previous,
                    _ => revision,
                }),
            },
            other => other,
        }
    }

    /// A fixture withdraws a revision's want without withdrawing a newer request.
    #[cfg(test)]
    #[must_use]
    pub(crate) const fn withdrawn_by(self, revision: u64) -> Self {
        match self {
            Self::Wanted(previous) if revision >= previous => Self::Unasked,
            Self::Opening {
                wanted: Some(previous),
            } if revision >= previous => Self::Opening { wanted: None },
            Self::PressedQueued {
                wanted: Some(previous),
            } if revision >= previous => Self::Unasked,
            other => other,
        }
    }

    /// A pass ended without waiting on the tools, or a person stopped the run: a want not yet
    /// opened is dropped.
    #[must_use]
    pub(crate) const fn withdrawn(self) -> Self {
        match self {
            Self::Wanted(_) | Self::PressedQueued { .. } => Self::Unasked,
            Self::Opening { .. } => Self::Opening { wanted: None },
            other => other,
        }
    }

    /// Whether the tab opens now: wanted, with no macOS dialog this process started up
    /// (`dialog_up`).
    #[must_use]
    pub(crate) const fn due(self, dialog_up: bool) -> bool {
        matches!(self, Self::Wanted(_)) && !dialog_up
    }
}

/// What the tab's program gives `aterm`: the install item alone, with its one password prompt.
pub(crate) const ARGS: [&str; 3] = aterm_types::macos_setup::COMMAND_LINE_TOOLS_ARGS;

/// The tab's fixed title, which the page names ([`crate::system_state::TOOLS_PASSWORD_HEADLINE`]).
pub(crate) const TAB_TITLE: &str = "Mac Setup";

/// What holds the automatic tab while another macOS dialog this process started is up
/// ([`App::developer_tools_install_held`]): that dialog's end wakes the window
/// ([`App::wake_when_no_access_dialog`]).
const DIALOG_UP: &str = "a macOS dialog this process started is up";

/// What holds the automatic tab while a network watch just started has not read Low Data Mode
/// ([`App::developer_tools_wait_network`]).
const FIRST_NETWORK_READING: &str = "the network watch's first reading";

/// What holds the automatic tab while nobody wants it: a launch on a Mac with the tools wants it
/// once the elevation's reading says a root step is owed ([`App::root_steps_owed`]).
const NO_WANT: &str = "no want";

/// What holds the automatic tab while the tools are there and no root step reads owed.
const NOTHING_OWED: &str = "nothing owed";

/// How long the page keeps the GitHub repositories row calm after the tools were found installed
/// (the tab's program said so, or the watch found them), while the row's next reading has not
/// landed ([`Settling`]): a reading that says the same as before past it is the truth again.
const SETTLE_BOUND: std::time::Duration = std::time::Duration::from_secs(120);

/// The durable account-wide record of setup's administrator ask, shared with terminal init
/// ([`aterm_types::clt_mark`]), as this window reads and changes it: the window claims an
/// automatic ask only where the record lets one go now, hands its claim to the tab's own program
/// ([`Self::hand`]), so the attempt reads cut off (and is asked again at once) when that program
/// ends with no end written, and records how the attempt ended ([`Self::end`]), which decides the
/// next automatic ask: unfinished on a doubling schedule, declined never by itself, done only
/// where a step is owed again. A person's Install… claims whatever the record says, and their
/// Set up now lifts a decline ([`Self::lift`]). A record another asker holds, or one that cannot
/// be read, is read again on [`LOOK_AGAIN`] while a want waits on it, and a claim that failed is
/// not tried again before [`aterm_types::clt_mark::ASK_AGAIN`]'s pause for the claims that failed
/// in a row ([`Self::held`], [`Self::due`]). Every change is made on the window's loop, so it
/// waits for the record's lock no longer than [`aterm_types::clt_mark::LOOP_LOCK_WAIT`]
/// ([`aterm_types::clt_mark::OnLoop`]): an asker stalled while it holds the lock fails the
/// change rather than freeze every window.
#[derive(Debug)]
pub(crate) struct Mark {
    path: Option<std::path::PathBuf>,
    /// What the record said at its last read.
    said: Said,
    /// The record said setup's elevation finished once on this account
    /// ([`aterm_types::clt_mark::Record::finished`]): a root step that reads undone since was
    /// undone by someone, and the automatic tab leaves it as it reads ([`App::elevation_owed`]).
    finished: bool,
    /// The record's last attempt did not finish: an automatic ask now is a try again, whose tab
    /// lands behind the window's active one.
    retry: bool,
    /// When the record was last read (unix seconds), and how many reads in a row found it held by
    /// another asker or unreadable: the next look is due on [`LOOK_AGAIN`] after them.
    read_at: u64,
    looks: u32,
    /// Claims that failed in a row (the record could not be written or locked), and when the last
    /// did (unix seconds): none is tried again before [`aterm_types::clt_mark::ASK_AGAIN`]'s pause
    /// for them.
    missed: u32,
    missed_at: u64,
    /// This window's claim, not yet handed to a tab's program or ended.
    claim: Option<aterm_types::clt_mark::Claim>,
}

/// When the window reads the account's record again while another asker's attempt holds it or
/// it cannot be read, and a want waits on it ([`Mark::look_again`]): a minute after the first
/// such read, doubling, never more than [`aterm_types::clt_mark::ASK_AGAIN`]'s first pause. The
/// record is the durable schedule; this only says when the window looks at it.
const LOOK_AGAIN: aterm_types::retry_schedule::Backoff = aterm_types::retry_schedule::Backoff::new(
    std::time::Duration::from_secs(60),
    std::time::Duration::from_secs(10 * 60),
);

/// What the record said of the next automatic ask at its last read ([`Mark`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Said {
    /// It may go at this unix second, or at once where that has passed.
    Due(u64),
    /// Another asker's attempt runs.
    Asking,
    /// A person declined the last ask: no automatic ask before this unix second, its long pause
    /// ([`aterm_types::clt_mark::DECLINE_AGAIN`]), when the record is read again.
    Declined(u64),
    /// The record has no place or cannot be read.
    Unread,
}

impl Default for Mark {
    /// A record with no place that lets every ask go: a fixture's isolated stand-in.
    fn default() -> Self {
        Self::blank(None, Said::Due(0))
    }
}

/// This Mac's clock, in unix seconds, as the record's times are.
fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

impl Mark {
    /// Read the durable account episode at launch. Unreadable records hold automatic asks;
    /// fixtures use an isolated in-memory default.
    pub(crate) fn for_launch(successor: bool) -> Self {
        #[cfg(test)]
        {
            let _ = successor;
            Self::default()
        }
        #[cfg(not(test))]
        {
            aterm_types::clt_mark::path().map_or(Self::blank(None, Said::Unread), |path| {
                Self::launched(path, successor, receipt::alive)
            })
        }
    }

    /// The record at `path` as a launch reads it, the receipts of tabs whose program no longer
    /// runs (`alive`) taken away: such a tab's attempt reads cut off in the record, which its
    /// program held ([`Self::hand`]), so the record alone decides when it is asked again. An
    /// update's `successor` takes none away, since its parent still runs: the tab of a program
    /// that ended while a descendant holds its PTY is that parent's to read, and taking its
    /// receipt would hold the parent's handoff for as long as that descendant lives. Its adoption
    /// takes the tab, or the next launch that is no successor reads it.
    #[cfg(any(not(test), unix))]
    fn launched(
        path: std::path::PathBuf,
        successor: bool,
        alive: impl Fn(i32, u64) -> bool,
    ) -> Self {
        let orphans = if successor {
            Vec::new()
        } else {
            receipt::Receipt::orphans(&path, alive)
        };
        for orphan in orphans {
            orphan.remove_if_matching();
        }
        Self::at(path)
    }

    /// The record kept at `path`, read now.
    #[cfg(any(not(test), unix))]
    pub(crate) fn at(path: std::path::PathBuf) -> Self {
        let mut mark = Self::blank(Some(path), Said::Unread);
        mark.read();
        mark
    }

    /// A record at `path` that says `said`, read at no time yet.
    const fn blank(path: Option<std::path::PathBuf>, said: Said) -> Self {
        Self {
            path,
            said,
            finished: false,
            retry: false,
            read_at: 0,
            looks: 0,
            missed: 0,
            missed_at: 0,
            claim: None,
        }
    }

    /// Read the record again.
    fn read(&mut self) {
        use aterm_types::clt_mark::Asked;
        let record = self.path.as_deref().map(|path| {
            aterm_types::clt_mark::record(&aterm_types::clt_mark::OnLoop, path, unix_now())
        });
        self.finished = record.as_ref().is_some_and(|record| record.finished);
        self.retry = matches!(
            record.as_ref().map(|record| &record.asked),
            Some(Asked::Unfinished { .. })
        );
        self.said = match record.map(|record| record.asked) {
            None | Some(Asked::Unread(_)) => Said::Unread,
            Some(Asked::Asking) => Said::Asking,
            Some(Asked::Declined { due, .. }) => Said::Declined(due),
            Some(asked) => asked.due().map_or(Said::Unread, Said::Due),
        };
        self.read_at = unix_now();
        self.looks = if matches!(self.said, Said::Asking | Said::Unread) {
            self.looks.saturating_add(1)
        } else {
            0
        };
    }

    /// Read the record again where another asker's attempt held it or it could not be read at
    /// the last read, once [`LOOK_AGAIN`] says the next look is due: that asker may have ended
    /// without finishing, or been cut off, and a read error may have passed; and where a person's
    /// decline held it, once its pause passed, so the ask comes again by itself.
    fn look_again(&mut self) {
        let now = unix_now();
        let due = match self.said {
            Said::Asking | Said::Unread => LOOK_AGAIN.due(self.read_at, self.looks) <= now,
            Said::Declined(due) => due <= now,
            Said::Due(_) => false,
        };
        if self.path.is_some() && due {
            self.read();
        }
    }

    /// When the claims that failed in a row may be tried again (unix seconds); none after a
    /// claim that held or none that failed.
    fn missed_due(&self) -> Option<u64> {
        (self.missed > 0).then(|| aterm_types::clt_mark::ASK_AGAIN.due(self.missed_at, self.missed))
    }

    /// Why no automatic ask goes now, as the record read last, or `None` where one may.
    pub(crate) fn held(&self) -> Option<&'static str> {
        if self.missed_due().is_some_and(|due| due > unix_now()) {
            return Some("the last claim of the ask failed; it is tried again on its schedule");
        }
        match self.said {
            Said::Due(due) if due <= unix_now() => None,
            Said::Due(_) => Some("the last ask's next try is not due yet (the account's record)"),
            Said::Asking => Some("another aterm is asking for the Mac password now"),
            Said::Declined(_) => Some(
                "a person declined the last ask; it comes again by itself after a long pause (the \
                 account's record)",
            ),
            Said::Unread => Some("the account's record of the ask cannot be read"),
        }
    }

    /// Whether an automatic ask is still coming, now or at its due: not while a person's decline
    /// holds (a person's step until its long pause passes), nor where the record cannot be read.
    pub(crate) const fn coming(&self) -> bool {
        matches!(self.said, Said::Due(_) | Said::Asking)
    }

    /// Whether an automatic ask may still come: [`Self::coming`], or a record kept in a place
    /// that cannot be read now, which is read again ([`Self::look_again`]) and asks by itself
    /// once it reads.
    pub(crate) const fn may_come(&self) -> bool {
        self.coming() || (matches!(self.said, Said::Unread) && self.path.is_some())
    }

    /// Whether the record says setup's elevation finished once on this account ([`Self::finished`]).
    pub(crate) const fn finished(&self) -> bool {
        self.finished
    }

    /// Whether the next automatic ask tries again after an attempt that did not finish.
    const fn retrying(&self) -> bool {
        self.retry
    }

    /// When the window next looks (unix seconds): the next automatic ask's due, the end of a
    /// person's decline's pause, or the next read of a record another asker holds or that cannot
    /// be read ([`Self::look_again`]), never before a claim that failed may be tried again.
    pub(crate) fn due(&self) -> Option<u64> {
        let said = match self.said {
            Said::Due(due) | Said::Declined(due) => Some(due),
            Said::Asking | Said::Unread if self.path.is_some() => {
                Some(LOOK_AGAIN.due(self.read_at, self.looks))
            }
            Said::Asking | Said::Unread => None,
        };
        said.map(|due| due.max(self.missed_due().unwrap_or(0)))
    }

    /// A person's own yes to setup (their Set up now): a decline the record holds is lifted
    /// ([`aterm_types::clt_mark::lift`]), so the automatic ask comes again by itself, and a claim
    /// that failed may be tried again at once. `true` where a decline was lifted.
    pub(crate) fn lift(&mut self) -> bool {
        self.missed = 0;
        let Some(path) = self.path.clone() else {
            return false;
        };
        let lifted = aterm_types::clt_mark::lift(&aterm_types::clt_mark::OnLoop, &path)
            .unwrap_or_else(|error| {
                aterm_log::warn!(
                    "System State: the account's declined ask was not lifted: {error}"
                );
                false
            });
        self.read();
        lifted
    }

    /// Read the record again before booking a shell: `true` where another asker's attempt or end
    /// holds this window's automatic ask now.
    fn asked_since(&mut self) -> bool {
        if self.path.is_some() {
            self.read();
        }
        self.held().is_some()
    }

    /// Claim the episode as the tab lands, as this window's process, under the record's lock:
    /// `true` only where the record let an automatic ask go and this claim holds it.
    fn claim(&mut self) -> bool {
        let Some(path) = self.path.clone() else {
            return self.held().is_none();
        };
        let claimed = aterm_types::clt_mark::Asker::this_process(&aterm_stamp::build_id())
            .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::NotFound))
            .and_then(|asker| {
                aterm_types::clt_mark::claim(
                    &aterm_types::clt_mark::OnLoop,
                    &path,
                    &asker,
                    unix_now(),
                )
            });
        let held = match claimed {
            Ok(claim) => {
                self.claim = claim;
                self.missed = 0;
                self.claim.is_some()
            }
            Err(error) => {
                // Held on the record's own schedule, so a record that cannot be written books no
                // shell after shell ([`Self::held`]).
                self.missed = self.missed.saturating_add(1);
                self.missed_at = unix_now();
                aterm_log::warn!("System State: the Mac Setup tab's ask was not claimed: {error}");
                false
            }
        };
        self.read();
        held
    }

    /// A person's own Install…: claim the episode whatever the record says, so an older
    /// attempt's late end writes only into its own place under it. `true` where the claim was
    /// recorded.
    fn explicit(&mut self) -> bool {
        let Some(path) = self.path.clone() else {
            return true;
        };
        let claimed = aterm_types::clt_mark::Asker::this_process(&aterm_stamp::build_id())
            .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::NotFound))
            .and_then(|asker| {
                aterm_types::clt_mark::explicit(
                    &aterm_types::clt_mark::OnLoop,
                    &path,
                    &asker,
                    unix_now(),
                )
            });
        let held = match claimed {
            Ok(claim) => {
                self.claim = Some(claim);
                true
            }
            Err(error) => {
                aterm_log::warn!(
                    "System State: the explicit installer ask was not recorded: {error}"
                );
                false
            }
        };
        self.read();
        held
    }

    /// Hand this window's claim to the tab's program `pid`, which asks for the password, so the
    /// attempt reads cut off once that program ends with no end written, whatever becomes of
    /// this window, and stays this window's to end: where a person's retry took the episode over
    /// first, the line that retry keeps names the program ([`aterm_types::clt_mark::hand`]). The
    /// claim the record holds then; this window's own where `pid`'s birth cannot be read; `None`
    /// where the record holds it nowhere (two retries at once gave it up) or there is none.
    fn hand(&mut self, pid: i32) -> Option<aterm_types::clt_mark::Claim> {
        let claim = self.claim.take()?;
        let Some(path) = self.path.as_deref() else {
            return Some(claim);
        };
        let Some(birth) = aterm_uds::process::read_birth(pid) else {
            return Some(claim);
        };
        match aterm_types::clt_mark::hand(
            &aterm_types::clt_mark::OnLoop,
            path,
            &claim,
            pid,
            birth.0,
        ) {
            Ok(handed) => handed,
            Err(error) => {
                aterm_log::warn!("System State: the ask stays this window's: {error}");
                Some(claim)
            }
        }
    }

    /// Carry the attempt of an earlier aterm's live tab this launch adopted into the record as
    /// its program's (`pid`, asking as `build`, the build its receipt says claimed, else this
    /// one), or as this window's process where `pid`'s birth cannot be read
    /// ([`aterm_types::clt_mark::carry`]): the record reads it asking while it runs, and its end
    /// is recorded against the claim this gives. `None` where the record holds another attempt
    /// or how one ended, or has no place.
    ///
    /// # Errors
    /// The record's own ([`aterm_types::clt_mark::carry`]), and no asker to name.
    fn carry(
        &mut self,
        pid: i32,
        build: Option<&str>,
    ) -> std::io::Result<Option<aterm_types::clt_mark::Claim>> {
        let Some(path) = self.path.clone() else {
            return Ok(None);
        };
        let this = aterm_stamp::build_id();
        let carried = aterm_types::clt_mark::Asker::process(build.unwrap_or(&this), pid)
            .or_else(|| aterm_types::clt_mark::Asker::this_process(&this))
            .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::NotFound))
            .and_then(|asker| {
                aterm_types::clt_mark::carry(
                    &aterm_types::clt_mark::OnLoop,
                    &path,
                    &asker,
                    unix_now(),
                )
            });
        self.read();
        carried
    }

    /// Give this window's claim back unused: no tab's program started, so nothing asked.
    fn release(&mut self) {
        if let Some(claim) = self.claim.take() {
            let _ = self.end(&claim, aterm_types::clt_mark::End::Released);
        }
    }

    /// Record how the attempt `claim` ended, in its own place: as the record's line, or under a
    /// person's retry that took it over and asks on. Where the record holds it nowhere (two
    /// retries at once gave it up), only a finish is written, as the account's. Says where the
    /// record held `claim` ([`aterm_types::clt_mark::Took`]): as its line where it has no place,
    /// this window's own attempt, and nowhere where the end could not be recorded.
    fn end(
        &mut self,
        claim: &aterm_types::clt_mark::Claim,
        how: aterm_types::clt_mark::End,
    ) -> aterm_types::clt_mark::Took {
        let took = self
            .path
            .as_deref()
            .map_or(aterm_types::clt_mark::Took::Line, |path| {
                aterm_types::clt_mark::end(
                    &aterm_types::clt_mark::OnLoop,
                    path,
                    claim,
                    how,
                    unix_now(),
                )
                .unwrap_or_else(|error| {
                    aterm_log::warn!(
                        "System State: how the installer ask ended was not recorded: {error}"
                    );
                    aterm_types::clt_mark::Took::Nowhere
                })
            });
        self.read();
        took
    }
}

/// A watch on whether this Mac's network is in Low Data Mode (Network.framework's path monitor),
/// which wakes the window at each reading ([`Wake::DeveloperToolsLowData`]) until it is dropped.
pub(crate) struct NetworkWatch {
    /// The monitor (none in a test's stand-in).
    #[cfg(target_os = "macos")]
    _monitor: Option<aterm_objc::PathMonitor>,
    /// When it started: the automatic tab waits briefly for its first reading.
    since: std::time::Instant,
}

/// How long the automatic tab waits for a new [`NetworkWatch`]'s first reading before it opens
/// without one; the monitor hands over the path in use at once on a Mac with a network.
const FIRST_READING: std::time::Duration = std::time::Duration::from_secs(3);

impl NetworkWatch {
    /// Start one; `None` where the monitor cannot be had (and off macOS), where the tab's program
    /// reads Low Data Mode itself.
    fn start() -> Option<Self> {
        #[cfg(target_os = "macos")]
        {
            let monitor = aterm_objc::PathMonitor::start(c"aterm.clt.low-data", |reading| {
                let _ = crate::workstation_setup::send_setup_wake(Wake::DeveloperToolsLowData(
                    reading.constrained,
                ));
            })?;
            Some(Self {
                _monitor: Some(monitor),
                since: std::time::Instant::now(),
            })
        }
        #[cfg(not(target_os = "macos"))]
        {
            None
        }
    }

    /// A watch that reads nothing, whose readings a test hands the window itself.
    #[cfg(all(test, unix))]
    pub(crate) fn stand_in() -> Self {
        Self {
            #[cfg(target_os = "macos")]
            _monitor: None,
            since: std::time::Instant::now(),
        }
    }
}

/// What this Mac shows of an install of Apple's Command Line Tools now ([`probe`]): the on-demand
/// placeholder, a live `softwareupdate` or `installer`, Apple's own installer app (at any age),
/// and the stamp of a live run of aterm's root half in the placeholder.
pub(crate) fn installing_now() -> Reading {
    probe::installing_now()
}

/// Whether the compiler is installed, using atpkg's read-only presence check. This runs no
/// compiler or installer: repositories can work through packaged git while no compiler exists.
pub(crate) fn c_toolchain_installed() -> Option<bool> {
    atpkg::prereq::installed_with(
        std::env::var_os("PATH").as_deref(),
        &atpkg::prereq::CcHost::this_machine(),
    )
}

/// Whether setup's developer tools have arrived: Git and a compiler behind the system
/// driver. Match init's standard PATH so an unrelated compiler cannot end this watch.
/// Ignore a project's `DEVELOPER_DIR`, as init's machine-toolchain probe does.
/// Discovery only reads files; it never invokes Apple's stubs or a compiler.
pub(crate) fn developer_tools_ready() -> bool {
    developer_tools_ready_on(
        aterm_types::clt_shim::developer_git(),
        std::ffi::OsStr::new("/usr/bin:/bin:/usr/sbin:/sbin"),
        atpkg::prereq::CcHost::this_machine(),
    )
}

fn developer_tools_ready_on(
    git: bool,
    path: &std::ffi::OsStr,
    host: atpkg::prereq::CcHost,
) -> bool {
    let host = atpkg::prereq::CcHost {
        developer_dir_override: None,
        ..host
    };
    git && atpkg::prereq::installed_with(Some(path), &host) == Some(true)
}

/// What this Mac shows of an install of Apple's Command Line Tools under way, by itself
/// ([`activity`]): the page reads it beside the tab aterm opened.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Activity {
    /// Nothing installs them: a placeholder left behind by an install that ended is no install.
    #[default]
    Idle,
    /// Installer evidence could not be read, so another installer must not start yet.
    Unread,
    /// Apple's own installer window (`Install Command Line Developer Tools`, which
    /// `xcode-select --install` and the `git` and `cc` shims open) is open, at any age: waiting
    /// for the person's own Install or Agree press, or running the install a press started
    /// there, which nothing this Mac shows tells apart. The person finishes there.
    AppleWindow,
    /// An install runs: the on-demand placeholder is there and something holds it,
    /// `softwareupdate` or `installer` (aterm's own root install, Homebrew's,
    /// [`aterm_types::macos_setup::tools_held_by`], as `aterm init` reads it), or a live root
    /// run.
    Installing,
}

/// How many of a live root run's processes a [`Reading`] names ([`Reading::run_chain`]): the run,
/// `sudo` (and the monitor its `use_pty` forks), and the tab's program that ran `sudo`, with room
/// to spare.
pub(crate) const RUN_CHAIN: usize = 6;

/// What [`activity`] reads of this Mac at one moment.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Reading {
    /// Installer evidence could not be read, so absence of an installer is unproved.
    pub(crate) unread: bool,
    /// The on-demand placeholder ([`aterm_types::macos_setup::COMMAND_LINE_TOOLS_INSTALLING`]) is
    /// there.
    pub(crate) placeholder: bool,
    /// Apple's `softwareupdate` or `installer` runs, by its exact path.
    pub(crate) install_process: bool,
    /// Apple's `Install Command Line Developer Tools` app runs.
    pub(crate) apple_window: bool,
    /// The placeholder carries the stamp of a run of aterm's root half that is alive now
    /// ([`aterm_types::macos_setup::placeholder_held_by_a_live_run`], the reader `aterm init`
    /// shares): between two steps, or waiting on Apple's installer window, with no installer
    /// process running this moment.
    pub(crate) live_run: bool,
    /// That live run's process and its parents, nearest first, as the same look at the process
    /// table read them, `0` past the last (and throughout with no live run): whose program
    /// started it, which `sudo` let run only past the Mac password
    /// ([`App::developer_tools_watch`] marks that tab's [`Terminal::past_password`]).
    pub(crate) run_chain: [u32; RUN_CHAIN],
}

/// [`Activity`] for `reading`: the placeholder counts as an install only while an installer
/// holds it or a live run of aterm's root half stamped it, so a stale one nothing holds reads as
/// no install; Apple's window, beside the placeholder it made as it opened or with none (it
/// opened beside another install's, which has ended since; its own request ends only as it
/// quits), is where the person finishes, at any age. `aterm init` and the root half read the
/// same ([`aterm_types::macos_setup::tools_held_by`], and a live run's stamp ahead of the
/// window): `aterm init` defers to the first as an install under way and to the window naming
/// the person's step there, and the root half waits on that window, never taking or making a
/// placeholder beside it.
#[must_use]
pub(crate) const fn activity(reading: Reading) -> Activity {
    if reading.placeholder && (reading.install_process || reading.live_run) {
        Activity::Installing
    } else if reading.apple_window {
        Activity::AppleWindow
    } else if reading.unread {
        Activity::Unread
    } else {
        Activity::Idle
    }
}

/// The owned installer child, including its retained result after an exit notification.
pub(crate) struct Terminal {
    session: u64,
    purpose: TerminalPurpose,
    finished: bool,
    recovered: bool,
    receipt: Option<receipt::Receipt>,
    /// The installer was seen running while this tab's program lived: the program outliving it
    /// (reading the tools back, saying so) is no wait on the person.
    installer_seen: bool,
    /// How the program ended was read and acted on ([`App::developer_tools_install_ended`]).
    ended: bool,
    /// The window already wrote the tab's last line (its install never started), so its end
    /// adds none.
    told: bool,
    /// The claim of the account's ask record its program holds, which its end records
    /// ([`Mark::end`]); none where its opening claimed none. A newer attempt's claim is never
    /// this one, so an older tab's late end writes only into its own place in the record.
    claim: Option<aterm_types::clt_mark::Claim>,
    /// This exact child has already received setup's cancellation signal.
    stop_sent: bool,
    /// Its tab was closed while its program ran and aterm runs on (its last view went before its
    /// end was seen, [`App::retire_developer_tools_install`]): someone said no to its ask, which
    /// the record keeps as a decline.
    closed: bool,
    /// Its tab goes with this aterm while its program runs: the close of aterm's last window,
    /// which quits it ([`App::note_developer_tools_tabs_quit`]). Nobody said no to its ask, so
    /// what the quit's hangup ends its program with writes no end
    /// ([`aterm_types::clt_mark::tab_end`]): the record's `asking` line outlives its asker. A
    /// program the quit carries ([`App::developer_tools_custody_at_exit`]) runs on, and the launch
    /// that adopts it records its end with the claim its receipt names; one the quit hangs up goes
    /// with the tab, and the next read takes the attempt as cut off, asked again by itself
    /// ([`aterm_types::clt_mark::Asked::Unfinished`]), never as a decline. A status of the
    /// program's own read after the quit was noted (a finish just before it) is still its end.
    quit: bool,
    /// Adopted from an earlier aterm whose receipt named no claim of this build's record
    /// ([`receipt::Receipt::earlier`]): its attempt is still to be carried into the record
    /// ([`App::carry_earlier_developer_tools_tabs`]), once this launch's handoff is in.
    carry: Option<receipt::Earlier>,
    /// The root run its program started was seen alive ([`Reading::run_chain`], as the watch
    /// reads it): `sudo` took the Mac password, and the program asks nothing more in the tab,
    /// which then holds setup's other prompts back only while something else might still ask
    /// ([`App::developer_tools_install_carried`]).
    past_password: bool,
    /// aterm's quit would leave its program running, as the watch's last reading found it
    /// ([`App::developer_tools_custody_at_exit`], whose disk and keeper reads the watch makes
    /// once a second): what [`App::developer_tools_install_carried`] reads, from memory.
    carried: bool,
}

#[derive(Clone, Copy)]
enum TerminalPurpose {
    Tools,
    #[cfg(target_os = "macos")]
    NameRule {
        generation: u64,
        origin: crate::tab_spawn::MenuOrigin,
    },
}

impl Terminal {
    /// The tab `session`, its program running (`recovered`: adopted from an earlier aterm, with
    /// its `receipt`).
    const fn running(session: u64, recovered: bool, receipt: Option<receipt::Receipt>) -> Self {
        Self {
            session,
            purpose: TerminalPurpose::Tools,
            finished: false,
            recovered,
            receipt,
            installer_seen: false,
            ended: false,
            told: false,
            claim: None,
            stop_sent: false,
            closed: false,
            quit: false,
            carry: None,
            past_password: false,
            carried: false,
        }
    }

    fn tools(&self) -> bool {
        matches!(self.purpose, TerminalPurpose::Tools)
    }
}

/// What the page says of the install beyond its tab and this Mac's installers.
#[derive(Debug, Default)]
pub(crate) struct Aftermath {
    /// The tools were found installed, by the tab's program or the watch ([`Settling`]).
    settling: Option<Settling>,
    /// Why the tab a person's Install… asked for did not open, until their next press.
    refused: Option<NotOpened>,
}

impl Aftermath {
    /// The GitHub repositories row read `reading` at `now`: a reading other than the one the
    /// page had as the tools were found installed, or [`SETTLE_BOUND`] passed, ends the calm
    /// [`Settling`] kept.
    fn settle(&mut self, reading: Option<&Observation>, now: std::time::Instant) {
        if self
            .settling
            .as_ref()
            .is_some_and(|settling| reading != settling.reading.as_ref() || now >= settling.until)
        {
            self.settling = None;
        }
    }
}

/// The tools were found installed (the tab's program said so, or the watch found them): the
/// GitHub repositories row stays calm until its next reading lands (one that differs from
/// `reading`, the one the page had then), or [`SETTLE_BOUND`] passes.
#[derive(Debug)]
struct Settling {
    reading: Option<Observation>,
    until: std::time::Instant,
}

/// The tab's program for `exe`, this program's `aterm`: the install item alone ([`ARGS`]),
/// told to leave developer mode as it reads where that is the person's
/// (`leave_developer_mode`, [`App::developer_mode_is_theirs`]).
fn tab_command(exe: &std::path::Path, leave_developer_mode: bool) -> Vec<std::ffi::OsString> {
    let mut command = tab_command_running(
        std::iter::once(exe.as_os_str().to_owned()).chain(ARGS.map(std::ffi::OsString::from)),
    );
    if leave_developer_mode {
        command.insert(
            2,
            format!(
                "{}=1",
                aterm_types::macos_setup::COMMAND_LINE_TOOLS_TAB_LEAVE_DEVELOPER_MODE
            )
            .into(),
        );
    }
    command
}

/// `program` as the tab runs it: under `/usr/bin/env`, which adds the tab's variable, with no
/// shell between them.
fn tab_command_running(
    program: impl IntoIterator<Item = std::ffi::OsString>,
) -> Vec<std::ffi::OsString> {
    let mut command = vec![
        std::ffi::OsString::from("/usr/bin/env"),
        format!("{}=1", aterm_types::macos_setup::COMMAND_LINE_TOOLS_TAB).into(),
    ];
    command.extend(program);
    command
}

/// This program's own `aterm`, which the tab runs; none off a Unix front door.
fn own_aterm() -> Option<std::path::PathBuf> {
    #[cfg(not(windows))]
    {
        crate::ledger_key::own_front_door()
    }
    #[cfg(windows)]
    {
        None
    }
}

/// The line typed into the tab in place of its go line when its install must not start: any
/// line but [`aterm_types::macos_setup::COMMAND_LINE_TOOLS_TAB_GO`] ends the program at once,
/// having started and said nothing.
const TAB_STOP: &str = "stop";

/// The tab's last line when aterm could not save what it needs to follow the install, which
/// therefore never started.
fn not_started_line() -> String {
    format!(
        "{}: aterm could not save what it needs to keep track of it.",
        aterm_types::macos_setup::COMMAND_LINE_TOOLS_NOT_FINISHED
    )
}

/// Whether `session`, adopted from an earlier aterm, runs a Mac Setup tab's program that has not
/// ended ([`child_ended`]): aterm's pin of [`TAB_TITLE`], which an adoption carries with the tab
/// ([`App::developer_tools_tab_adopted`]).
fn runs_adopted_mac_setup(session: &crate::Session) -> bool {
    session.handoff_local_id.is_some() && !child_ended(session) && pins_tab_title(session)
}

/// Whether `session`'s title is [`TAB_TITLE`], aterm's pin of the Mac Setup tab's.
fn pins_tab_title(session: &crate::Session) -> bool {
    session
        .ctx
        .meta
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get("title")
        == Some(TAB_TITLE)
}

/// Whether `child`'s program ended as far as this window can tell: its status collected, its
/// process reaped, or its terminal closed.
fn child_ended(child: &crate::Session) -> bool {
    child.child_exit.get().is_some()
        || child
            .child_reaped
            .load(std::sync::atomic::Ordering::Acquire)
        || child.pty_closed_at.get().is_some()
}

/// What a launch starts of the install ([`launch_install`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LaunchInstall {
    /// The watch for the tools' arrival ([`watch_for_tools`]).
    pub(crate) watch: bool,
    /// The want of the tools, whose tab opens by itself.
    pub(crate) want: bool,
}

impl LaunchInstall {
    /// Where the launch's install stands from the start: wanted as [`LAUNCH_WANT`], or unasked.
    pub(crate) const fn install(self) -> Install {
        if self.want {
            Install::Wanted(LAUNCH_WANT)
        } else {
            Install::Unasked
        }
    }

    /// The launch's want of the tools
    /// ([`crate::workstation_setup::Host::developer_tools_want`]).
    pub(crate) const fn want(self) -> Option<u64> {
        if self.want { Some(LAUNCH_WANT) } else { None }
    }
}

/// [`LaunchInstall`] for a launch with a `window` (not a headless instance) that reads this Mac
/// (`reads_mac`: not a test), with the tools `missing` and the account's [`Mark`] `opened` or not.
pub(crate) const fn launch_install(
    window: bool,
    reads_mac: bool,
    missing: bool,
    opened: bool,
) -> LaunchInstall {
    LaunchInstall {
        watch: reads_mac && missing,
        want: window && missing && !opened,
    }
}

/// Wake the window every second until the developer tools exist, with what this Mac shows of an
/// installer then ([`installing_now`], a look at every process), read here on the watch's own
/// thread so the window's loop scans no process table each second
/// ([`App::developer_tools_watch`]): their arrival by any route takes the shim off new shells,
/// and a launch's want gets its looks with no status poll to ride. Only a Mac that lacks them runs
/// it; whether they exist reads with a stat and a readlink, and the second that finds them sends
/// no reading and ends the watch.
pub(crate) fn watch_for_tools() {
    let watch = std::thread::Builder::new()
        .name("aterm-clt-watch".to_owned())
        .spawn(|| {
            loop {
                std::thread::sleep(std::time::Duration::from_secs(1));
                let present = developer_tools_ready();
                let reading = (!present).then(installing_now);
                let _ =
                    crate::workstation_setup::send_setup_wake(Wake::DeveloperToolsWatch(reading));
                if present {
                    return;
                }
            }
        });
    if let Err(error) = watch {
        aterm_log::warn!("System State: the Command Line Tools watch did not start: {error}");
    }
}

/// What setup's one early elevation owes this Mac beside the tools ([`root_steps_owed_by`]):
/// developer mode, apart from the rest, since a person may have turned it off after setup turned
/// it on, which the automatic tab then leaves as it reads ([`App::elevation_owed`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct RootSteps {
    /// Developer mode is off, or on and this console account may not use it.
    pub(crate) debugger: bool,
    /// The rule that names the Mac after admission, or the LAN broker.
    pub(crate) other: bool,
}

impl RootSteps {
    /// Nothing owed.
    #[cfg(all(test, unix))]
    pub(crate) const NONE: Self = Self {
        debugger: false,
        other: false,
    };
    /// A step beside developer mode owed (the Mac's name rule, the LAN broker).
    #[cfg(test)]
    pub(crate) const OTHER: Self = Self {
        debugger: false,
        other: true,
    };

    /// Whether anything is owed.
    #[cfg(all(test, unix))]
    pub(crate) const fn any(self) -> bool {
        self.debugger || self.other
    }
}

/// What setup's one early elevation owes this Mac beside the tools, from what
/// this account reads with no prompt (`aterm_machines::root_steps::owed` reads the same for
/// `aterm init`): developer `mode` (`DevToolsSecurity -status`), or an enabled mode this
/// console account is not authorized to use (`member`), or `rule`, the stat of the
/// rule that names the Mac after admission ([`aterm_types::macos_setup::ADMITTED_NAME_RULE`]),
/// reading no root-owned 0440 file while no refused rule stands
/// ([`aterm_types::macos_setup::ADMITTED_NAME_RULE_REFUSED`], `refused`), or `broker`, the LAN
/// broker's install owed as both read it ([`aterm_uds::lan::install::Reading::owed_with_running`]:
/// its image or plist missing or not this build's, or its service stopped; a running image of
/// the source's own signing team adopts with no password). A step that cannot be read is not owed: no password is
/// asked for want of a reading. Only macOS reads it.
#[cfg(any(target_os = "macos", all(test, unix)))]
pub(crate) fn root_steps_owed_by(
    mode: aterm_containment::consent::DebuggerMode,
    member: aterm_containment::consent::DebuggerMember,
    rule: std::io::Result<(u32, u32, bool)>,
    refused: bool,
    broker: bool,
) -> RootSteps {
    use aterm_containment::consent::DebuggerMode;
    let off = match mode {
        DebuggerMode::Disabled => Some(true),
        DebuggerMode::Enabled => Some(false),
        // Apple's recognized unknown-rule answer establishes no missing root step, just as
        // the CLI batch's `root_steps::owed` reads it. Keep the diagnostic mode, not a tab
        // whose own program has nothing it may apply.
        DebuggerMode::Unrecognized | DebuggerMode::Unknown => None,
    };
    let rule_owed = match rule {
        Ok((uid, mode, file)) => !(file && uid == 0 && mode == 0o440),
        Err(error) => error.kind() == std::io::ErrorKind::NotFound && !refused,
    };
    RootSteps {
        debugger: off == Some(true)
            || (off == Some(false)
                && member == aterm_containment::consent::DebuggerMember::NotMember),
        other: rule_owed || broker,
    }
}

/// Read, on a thread of its own, whether setup's one early elevation owes this Mac anything
/// beside the tools ([`root_steps_owed_by`]), and wake the window with it
/// ([`App::root_steps_owed`]). `false` when the thread could not start: no answer comes.
pub(crate) fn read_root_steps() -> bool {
    let read = std::thread::Builder::new()
        .name("aterm-root-steps".to_owned())
        .spawn(|| {
            #[cfg(target_os = "macos")]
            {
                use aterm_containment::consent::{DebuggerMember, ProbeGate, probe_debugger};
                use std::os::unix::fs::MetadataExt as _;
                let debugger = probe_debugger(ProbeGate::on());
                // SAFETY: `geteuid` reads this process's effective uid and cannot fail.
                let uid = unsafe { libc::geteuid() };
                let member = if uid != 0
                    && std::fs::metadata("/dev/console").is_ok_and(|meta| meta.uid() == uid)
                {
                    debugger.member
                } else {
                    DebuggerMember::Unknown
                };
                let rule = std::fs::symlink_metadata(aterm_types::macos_setup::ADMITTED_NAME_RULE)
                    .map(|meta| (meta.uid(), meta.mode() & 0o7777, meta.file_type().is_file()));
                let refused =
                    std::fs::symlink_metadata(aterm_types::macos_setup::ADMITTED_NAME_RULE_REFUSED)
                        .is_ok();
                let broker = aterm_uds::lan::install::owed_here(|| {
                    aterm_secrets::service::system_job_running(aterm_uds::lan::LABEL)
                });
                let owed = root_steps_owed_by(debugger.mode, member, rule, refused, broker);
                aterm_log::info!("System State: setup's one Mac password owes this Mac: {owed:?}");
                let _ = crate::workstation_setup::send_setup_wake(Wake::RootStepsOwed(owed));
            }
        });
    match read {
        Ok(_) => true,
        Err(error) => {
            aterm_log::warn!("System State: the elevation's steps were not read: {error}");
            false
        }
    }
}

/// [`crate::tab_spawn::TabSpawnThen`] for the automatic tab of `revision`'s want: its landing
/// check ([`App::prepare_developer_tools_install_landing`]) claims the account's ask last, so a
/// tab lands only where that claim holds it, which a landing that fails after the check gives
/// back ([`Mark::release`]): it asked nothing.
fn install_tab_then(revision: u64) -> crate::tab_spawn::TabSpawnThen {
    let claimed = std::rc::Rc::new(std::cell::Cell::new(false));
    crate::tab_spawn::TabSpawnThen::Before {
        check: Box::new({
            let claimed = std::rc::Rc::clone(&claimed);
            move |app, _window| {
                let lands = app.prepare_developer_tools_install_landing(revision);
                claimed.set(lands);
                lands
            }
        }),
        then: Box::new(move |app, landed| {
            // A quiet refusal restored Wanted/Unasked before this callback. Only a real
            // opening (or its failure) consumes the launch's attempt.
            if matches!(
                app.workstation_setup.developer_tools,
                Install::Opening { .. }
            ) {
                app.workstation_setup.developer_tools = Install::Opened;
                match landed {
                    Ok(sid) => {
                        let spent = claimed.get();
                        app.dispatch_developer_tools_install(revision, |app| {
                            app.start_developer_tools_install(&sid, spent);
                        });
                        // Only a real landing changes frontness. A refused spawn during
                        // shutdown must not republish the still-retained setup window.
                        app.publish_system_state_front();
                    }
                    Err(why) => {
                        if claimed.get() {
                            app.workstation_setup.developer_tools_mark.release();
                        }
                        aterm_log::warn!(
                            "System State: the Command Line Tools install tab did not open: {why}"
                        );
                    }
                }
            }
            app.release_unused_access_dialog(std::time::Instant::now());
        }),
    }
}

/// [`install_tab_then`] for a person's press of Install…: its own landing check, and no run's
/// revision to answer to, since the press is the person's yes. A tab that did not start tells
/// them, as any tab they open does; one its check refused says why on the row
/// ([`App::prepare_pressed_install_landing`]).
fn pressed_tab_then() -> crate::tab_spawn::TabSpawnThen {
    crate::tab_spawn::TabSpawnThen::Before {
        check: Box::new(move |app, window| app.prepare_pressed_install_landing(window)),
        then: Box::new(move |app, landed| {
            if app.workstation_setup.developer_tools == Install::Pressed {
                app.workstation_setup.developer_tools = Install::Opened;
                match landed {
                    Ok(sid) => {
                        // The person's press goes whatever the record says; a claim it did not
                        // win takes back nothing later.
                        let spent = app.workstation_setup.developer_tools_mark.explicit();
                        app.start_developer_tools_install(&sid, spent);
                        app.publish_system_state_front();
                    }
                    // Keys typed meanwhile stayed in System State (the tab spawns unasked, so
                    // none reaches its password prompt), which also keeps the spawn's own
                    // notice back: the press is the person's, so they hear it here.
                    Err(why) => {
                        aterm_log::warn!(
                            "System State: the Command Line Tools install tab did not open: {why}"
                        );
                        app.post_message(crate::message_reporters::new_tab_failed(&why));
                    }
                }
            }
            app.release_unused_access_dialog(std::time::Instant::now());
            app.republish_system_state_developer_tools();
        }),
    }
}

#[cfg(target_os = "macos")]
fn name_rule_tab_then(
    generation: u64,
    origin: crate::tab_spawn::MenuOrigin,
) -> crate::tab_spawn::TabSpawnThen {
    crate::tab_spawn::TabSpawnThen::Before {
        check: Box::new(move |app, window| {
            window == origin.window()
                && app.machine_sets == generation
                && origin.is_current(app)
                && !app.headless
                && !app.developer_tools_install_active()
                && !app.access_dialog_up()
                && app.reserve_access_dialog()
        }),
        then: Box::new(move |app, landed| {
            match landed {
                Ok(sid) => app.start_machine_name_authorization(&sid, generation, origin),
                Err(why) => app.machine_name_authorization_finished(generation, Err(why)),
            }
            app.release_unused_access_dialog(std::time::Instant::now());
        }),
    }
}

impl App {
    /// One explicit naming permission repair, in the real tab that owns its prompt.
    /// The fixed nonroot command waits for GO before it may launch sudo. Spawn
    /// success alone supplies no permission or naming publication authority.
    #[cfg(target_os = "macos")]
    pub(crate) fn open_machine_name_authorization(&mut self, generation: u64) {
        let origin = self
            .frontmost_window
            .and_then(|window| self.palette_menu_origin(window));
        let Some(origin) = origin.filter(|origin| {
            !self.headless
                && self.gesture_unvouched().is_none()
                && self.machine_sets == generation
                && origin.is_current(self)
                && !self.developer_tools_install_active()
                && !self.access_dialog_up()
        }) else {
            self.machine_name_authorization_finished(generation,
                Err("Name permission repair needs the window that requested it, with no other permission operation running".into()));
            return;
        };
        let Some(exe) = own_aterm() else {
            self.machine_name_authorization_finished(
                generation,
                Err("this application cannot name its own permission-repair program".into()),
            );
            return;
        };
        let args = ["machines", "here", "apply", "--plain", "--name-rule-only"];
        let ask = crate::tab_spawn::SpawnAsk {
            unasked: true,
            exec_command: Some(tab_command_running(
                std::iter::once(exe.into_os_string()).chain(args.map(std::ffi::OsString::from)),
            )),
            ..crate::tab_spawn::SpawnAsk::default()
        };
        let then = name_rule_tab_then(generation, origin);
        let _act = crate::tab_marks::ActScope::gesture("name-permission");
        self.open_off_loop(
            origin.window(),
            crate::tab_spawn::SpawnPlace::Tab,
            ask,
            then,
        );
    }

    #[cfg(target_os = "macos")]
    fn start_machine_name_authorization(
        &mut self,
        sid: &str,
        generation: u64,
        origin: crate::tab_spawn::MenuOrigin,
    ) {
        let session = self
            .store
            .read()
            .unwrap_or_else(|why| why.into_inner())
            .by_sid(&aterm_session::SessionId::new(sid))
            .map(|handle| handle.local_id);
        let session = session.filter(|session| {
            self.pool.get(*session).is_some_and(|child| {
                child.pid == child.shell_identity.pid && child.ctx.self_id.as_str() == sid
            })
        });
        let Some(session) = session else {
            self.type_into_new_session(sid, TAB_STOP);
            self.machine_name_authorization_finished(
                generation,
                Err("the permission tab's exact child could not be retained".into()),
            );
            return;
        };
        let mut terminal = Terminal::running(session, false, None);
        terminal.purpose = TerminalPurpose::NameRule { generation, origin };
        self.workstation_setup
            .developer_tools_session
            .push(terminal);
        self.type_into_new_session(sid, aterm_types::macos_setup::COMMAND_LINE_TOOLS_TAB_GO);
    }

    /// Start the install in its landed tab `sid`, recording the tab first: even an immediate
    /// failure's exit must release the slot and leave its explanation in the tab. The tab takes
    /// its fixed title, which the page names, and its program its go line, once the tab's
    /// recovery record is saved; without one the install never starts, the tab says why, and the
    /// opening's claim is given back at once ([`Mark::release`]): it installed nothing, and no
    /// person chose that. `spent`: its landing (or the person's press) claimed the account's
    /// [`Mark`], whose claim goes to the tab's program ([`Mark::hand`]) and into the recovery
    /// record, so a launch that adopts the live tab records its end as this one would.
    fn start_developer_tools_install(&mut self, sid: &str, spent: bool) {
        let session = self
            .store
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .by_sid(&aterm_session::SessionId::new(sid))
            .map(|handle| handle.local_id);
        let Some(session) = session else {
            return;
        };
        let Some(child) = self.pool.get(session) else {
            return;
        };
        let claim = if spent {
            self.workstation_setup.developer_tools_mark.hand(child.pid)
        } else {
            None
        };
        let recovery = self
            .workstation_setup
            .developer_tools_mark
            .path
            .as_deref()
            .and_then(|mark| {
                (child.pid == child.shell_identity.pid && child.ctx.self_id.as_str() == sid)
                    .then(|| {
                        receipt::Receipt::for_child(
                            mark,
                            sid,
                            child.pid,
                            child.shell_identity.birth,
                            claim.as_ref(),
                        )
                    })
                    .flatten()
            });
        let recovery = match recovery {
            Some(receipt) => receipt.write_exclusive().map(|()| Some(receipt)),
            None if crate::keeper_link::on() => Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "the installer child has no verified recovery identity or state directory",
            )),
            None => Ok(None),
        };
        let (receipt, go) = match recovery {
            Ok(receipt) => (receipt, true),
            Err(error) => {
                aterm_log::warn!(
                    "System State: installer not started: could not save its recovery state: {error}"
                );
                (None, false)
            }
        };
        if !go && let Some(claim) = &claim {
            self.workstation_setup
                .developer_tools_mark
                .end(claim, aterm_types::clt_mark::End::Released);
        }
        let mut terminal = Terminal::running(session, false, receipt);
        terminal.told = !go;
        terminal.claim = claim.filter(|_| go);
        self.close_finished_developer_tools_tabs();
        self.workstation_setup.developer_tools_attempt = Some(session);
        self.workstation_setup
            .developer_tools_session
            .push(terminal);
        if go {
            // An install starts: the shim's line says the tools are on their way again.
            self.workstation_setup.developer_tools_unfinished = false;
            self.settle_developer_tools_shim();
        }
        if let Err(error) = self.pin_developer_tools_title(session) {
            aterm_log::info!("System State: the Mac Setup tab kept its own title: {error:?}");
        }
        if go {
            self.type_into_new_session(sid, aterm_types::macos_setup::COMMAND_LINE_TOOLS_TAB_GO);
        } else {
            self.write_developer_tools_line(session, &not_started_line());
            self.type_into_new_session(sid, TAB_STOP);
        }
    }

    /// A new attempt's tab landed: the tabs of earlier attempts whose programs ended (each said
    /// how, and the new one says the rest) go, so tries again never pile up tabs nobody waits on.
    /// The last tab of its window stays, since closing that would end the window. A naming
    /// permission's tab is no attempt of the tools: its result stays in it.
    fn close_finished_developer_tools_tabs(&mut self) {
        let ended: Vec<u64> = self
            .workstation_setup
            .developer_tools_session
            .iter()
            .filter(|terminal| terminal.tools() && terminal.ended)
            .map(|terminal| terminal.session)
            .collect();
        for session in ended {
            if self.close_ends_window(session) {
                continue;
            }
            if let Err(why) = self.close_session_by_id(session) {
                aterm_log::info!("System State: an earlier Mac Setup tab stays open: {why}");
            }
        }
    }

    /// The tab's fixed title, [`TAB_TITLE`], as aterm's own pin.
    fn pin_developer_tools_title(
        &mut self,
        session: u64,
    ) -> Result<bool, crate::session_timeline::MetaWriteError> {
        let Some(ctx) = self.pool.get(session).map(|s| s.ctx.clone()) else {
            return Ok(false);
        };
        let changed = crate::session_timeline::write_session_meta(
            &ctx,
            crate::session_timeline::MetaField::Title,
            crate::session_timeline::MetaEdit::Set(TAB_TITLE),
            aterm_slog::Actor::Aterm,
        )?;
        if changed {
            self.refresh_meta_dependent_chrome(session);
        }
        Ok(changed)
    }

    /// Recover only exact child custody, with the claim its receipt names across updates.
    /// Adoption never types installer bytes, claims nothing, and grants no setup authority.
    pub(crate) fn recover_developer_tools_install(&mut self, local: u64) {
        if self
            .workstation_setup
            .developer_tools_session
            .iter()
            .any(|terminal| terminal.session == local)
        {
            return;
        }
        let Some(child) = self.pool.get(local) else {
            return;
        };
        if child.handoff_local_id.is_none() || child.pid != child.shell_identity.pid {
            return;
        }
        let Some(receipt) = self
            .workstation_setup
            .developer_tools_mark
            .path
            .as_deref()
            .and_then(|mark| {
                receipt::Receipt::adopt(
                    mark,
                    child.ctx.self_id.as_str(),
                    child.pid,
                    child.shell_identity.birth,
                )
            })
        else {
            return;
        };
        let finished = child_ended(child);
        // The record names this tab's program as the asker where its opening claimed: adoption
        // changes nothing in it, and records the end it reads as the opening window would. An
        // earlier aterm's receipt names no claim of this record: its attempt is carried into it
        // once the handoff is in ([`Self::carry_earlier_developer_tools_tabs`]), never before,
        // as the outgoing aterm still reads the receipt as it wrote it until then.
        let claim = receipt.claimed().cloned();
        let carry = receipt.earlier().cloned();
        let mut terminal = Terminal::running(local, true, Some(receipt));
        terminal.finished = finished;
        terminal.claim = claim;
        terminal.carry = carry;
        if !finished || self.workstation_setup.developer_tools_attempt.is_none() {
            self.workstation_setup.developer_tools_attempt = Some(local);
        }
        self.workstation_setup
            .developer_tools_session
            .push(terminal);
        self.workstation_setup.developer_tools = Install::Opened;
        if !finished {
            self.recover_access_dialog();
            // The earlier aterm's install runs on in its tab: its want comes with it (as the
            // oldest pass's, so any pass of this launch that no longer waits on the tools ends
            // it), and the shim's line says the tools are on their way, which this launch's
            // reconcile, with nothing installing them yet, had taken back.
            let setup = &mut self.workstation_setup;
            setup.developer_tools_unfinished = false;
            setup.developer_tools_want.get_or_insert(0);
            self.settle_developer_tools_want();
        }
    }

    /// Carry the attempts of the live tabs this launch adopted from an earlier aterm whose
    /// receipts named no claim of this build's record into that record, once, as their
    /// programs' ([`Mark::carry`]), and rewrite each receipt as this build's, naming the claim
    /// ([`receipt::Receipt::carry`]): the record then holds every other automatic asker while
    /// such a tab's program runs (its `opened` line alone read as a try long since due), and the
    /// tab's end (a decline included) is recorded as this build's own tab's would be. Run only
    /// once the handoff that brought them is in (setup starts no sooner,
    /// [`App::tick_workstation_setup`]), so the outgoing aterm reads its receipt unchanged until
    /// it commits. A tab whose end was read already stays as adopted. Where the record could not
    /// be changed (its lock held past the window's wait, an error), the tab keeps its earlier
    /// receipt and is carried again at `now`'s next pass after [`LOOK_AGAIN`]'s pause for the
    /// carries that failed in a row ([`crate::workstation_setup::Host::deadline`] wakes it).
    pub(crate) fn carry_earlier_developer_tools_tabs(&mut self, now: std::time::Instant) {
        let setup = &mut self.workstation_setup;
        if setup
            .developer_tools_carry_retry
            .is_some_and(|retry| retry > now)
        {
            return;
        }
        let due: Vec<(u64, receipt::Earlier)> = setup
            .developer_tools_session
            .iter()
            .filter(|terminal| !terminal.ended)
            .filter_map(|terminal| Some((terminal.session, terminal.carry.clone()?)))
            .collect();
        let mut missed = false;
        for (session, earlier) in due {
            let Some(pid) = self.pool.get(session).map(|child| child.pid) else {
                continue;
            };
            let carried = self
                .workstation_setup
                .developer_tools_mark
                .carry(pid, earlier.build.as_deref());
            let Some(terminal) = self
                .workstation_setup
                .developer_tools_session
                .iter_mut()
                .find(|terminal| terminal.session == session)
            else {
                continue;
            };
            let claim = match carried {
                Ok(claim) => claim,
                Err(error) => {
                    missed = true;
                    aterm_log::warn!(
                        "System State: an earlier aterm's Mac Setup tab was not carried into the \
                         account's record; it is tried again on its schedule: {error}"
                    );
                    continue;
                }
            };
            terminal.carry = None;
            if let Some(receipt) = terminal.receipt.as_mut()
                && let Err(error) = receipt.carry(claim.as_ref())
            {
                aterm_log::warn!(
                    "System State: an earlier aterm's installer receipt stays as it wrote it: \
                     {error}"
                );
            }
            terminal.claim = claim;
        }
        let setup = &mut self.workstation_setup;
        if missed {
            setup.developer_tools_carry_misses =
                setup.developer_tools_carry_misses.saturating_add(1);
            setup.developer_tools_carry_retry =
                Some(now + LOOK_AGAIN.after(setup.developer_tools_carry_misses));
        } else {
            setup.developer_tools_carry_misses = 0;
            setup.developer_tools_carry_retry = None;
        }
    }

    /// An update's Commit: this build has taken over, so it stops holding back what waited for
    /// that. The Mac Setup tabs it adopted from an earlier aterm are carried into the account's
    /// record no sooner than this, at setup's next tick
    /// ([`Self::carry_earlier_developer_tools_tabs`]), so a successor that stands down leaves the
    /// record and its parent's receipts as they were.
    #[cfg(unix)]
    pub(crate) fn note_incoming_handoff_committed(&mut self) {
        self.incoming_handoff_pending = false;
    }

    /// Where an install of Apple's Command Line Tools stands for System State's page (design
    /// §3.3), with `status` the reading the page is about to show: an install running (or this
    /// tab's program outliving one it saw, or the tab saying the tools are installed while the
    /// row's next reading is still to come), else the tab asking for the password (or, restored
    /// from an earlier aterm, for whatever it asks), else Apple's own installer window waiting
    /// for the person, else a person's press queued behind a macOS dialog, else an account that
    /// may not install them, else Low Data Mode holding the download back, else why their last
    /// press opened nothing, else neither.
    pub(crate) fn developer_tools_install_step(
        &mut self,
        status: &crate::SetupStatus,
    ) -> Option<ToolsInstall> {
        self.read_developer_tools_exit();
        let reading = self.developer_tools_known();
        let activity = activity(reading);
        let setup = &mut self.workstation_setup;
        setup
            .developer_tools_after
            .settle(status.repositories_reading(), std::time::Instant::now());
        // A program whose end is not read yet still counts as running: its exit status comes a
        // moment after its terminal closed. While it runs, Apple's own installer window is where
        // the person finishes (the program opens it where Apple's catalog lists no tools, or
        // waits on the one already open, and asks them to press Install there), whatever
        // placeholder is held.
        let mut tab = None;
        for terminal in setup
            .developer_tools_session
            .iter_mut()
            .filter(|terminal| terminal.tools() && !terminal.ended)
        {
            terminal.installer_seen |= activity == Activity::Installing;
            tab = tab.or(Some(if reading.apple_window {
                ToolsInstall::AppleWindow
            } else if terminal.installer_seen {
                ToolsInstall::Installing
            } else if terminal.recovered {
                ToolsInstall::Recovered
            } else {
                ToolsInstall::Password
            }));
        }
        if tab.is_some() {
            return tab;
        }
        if activity == Activity::Installing || setup.developer_tools_after.settling.is_some() {
            return Some(ToolsInstall::Installing);
        }
        if activity == Activity::AppleWindow {
            return Some(ToolsInstall::AppleWindow);
        }
        if matches!(setup.developer_tools, Install::PressedQueued { .. }) {
            return Some(ToolsInstall::Queued);
        }
        if setup.developer_tools_not_admin {
            return Some(ToolsInstall::NotAdmin);
        }
        if setup.developer_tools_low_data == Some(true) {
            return Some(ToolsInstall::LowData);
        }
        if let Some(why) = setup.developer_tools_after.refused {
            return Some(ToolsInstall::NotOpened(why));
        }
        self.developer_tools_ask_coming()
            .then_some(ToolsInstall::Coming)
    }

    /// Whether aterm's own ask for setup's Mac password comes by itself: a want stands that may
    /// open the tab ([`Self::developer_tools_install_authorized`]), or its shell is booked, and the
    /// account's record lets an automatic ask come, now or at its next try ([`Mark::coming`]),
    /// outside a scratch world. Then nobody needs to press anything, and the page says so calmly.
    fn developer_tools_ask_coming(&self) -> bool {
        let setup = &self.workstation_setup;
        !setup.developer_tools_scratch_world
            && setup.developer_tools_mark.coming()
            && match setup.developer_tools {
                Install::Opening { .. } => true,
                Install::Wanted(revision) => self.developer_tools_install_authorized(revision),
                _ => false,
            }
    }

    /// A compiler installed outside this window ends its missing-tools exception without
    /// waiting for another package pass. A dismissed entry still needs that observation; one
    /// already resolved needs no more filesystem reads. Repository readiness is not compiler
    /// readiness: packaged git works without Apple's tools, and Linux packages them separately.
    ///
    /// A `missing` verdict held while an install was on its way
    /// ([`crate::workstation_setup::Host::c_toolchain_missing_held`]) is dropped once the tools
    /// are there, and filed once absence is confirmed and nothing installs them any more
    /// (the tab stopped, failed or closed, an account that may not install them). An unread
    /// presence keeps the verdict pending until a later observation decides it.
    pub(crate) fn settle_missing_tools_exception(&mut self) {
        use crate::toolchain_words::{CToolchain, c_toolchain_entry};
        let installed = || {
            self.workstation_setup
                .developer_tools_installed
                .and_then(|read| read())
        };
        if self.workstation_setup.c_toolchain_missing_held {
            match installed() {
                Some(true) => self.workstation_setup.c_toolchain_missing_held = false,
                Some(false) if !self.developer_tools_on_their_way() => {
                    self.workstation_setup.c_toolchain_missing_held = false;
                    self.except(&c_toolchain_entry(CToolchain::Missing));
                }
                Some(false) | None => {}
            }
            return;
        }
        let missing = c_toolchain_entry(CToolchain::Missing).key();
        if !self.has_unresolved_exception(&missing) {
            return;
        }
        if installed() == Some(true) {
            self.except_clear(&missing);
        }
    }

    /// What this Mac shows of an installer now ([`Reading`]), probed live on the window's loop: the
    /// dispatch boundaries read it (a landing's check, a person's press), so no tab lands beside
    /// an installer that started since the last look. While the watch runs, its reading is brought
    /// up to date with this one ([`Self::developer_tools_known`]).
    fn developer_tools_reading(&mut self) -> Reading {
        let reading = self
            .workstation_setup
            .developer_tools_probe
            .map_or_else(Reading::default, |read| read());
        if let Some(seen) = &mut self.workstation_setup.developer_tools_seen {
            *seen = reading;
        }
        reading
    }

    /// Whether installer custody blocks another opening at this dispatch boundary: a known
    /// installer or unread evidence ([`Self::developer_tools_reading`]).
    fn developer_tools_install_blocked(&mut self) -> bool {
        activity(self.developer_tools_reading()) != Activity::Idle
    }

    /// What the window knows of this Mac's installers for its looks that are no dispatch (the
    /// page's step, the shim's word, the automatic tab's due check, a queued press's wait): the
    /// reading the watch took on its own thread within the last second, or the newer one a
    /// boundary read since, while the watch runs ([`watch_for_tools`]); a live probe where no watch
    /// runs (the tools were there at launch, or arrived since), whose looks come only with events.
    fn developer_tools_known(&self) -> Reading {
        self.workstation_setup
            .developer_tools_seen
            .unwrap_or_else(|| {
                self.workstation_setup
                    .developer_tools_probe
                    .map_or_else(Reading::default, |read| read())
            })
    }

    /// Whether an installer is up on this Mac, as the window knows it
    /// ([`Self::developer_tools_known`]).
    fn developer_tools_installing_known(&self) -> bool {
        matches!(
            activity(self.developer_tools_known()),
            Activity::Installing | Activity::AppleWindow
        )
    }

    /// A person's press of the GitHub repositories row's Install… (design §3.3): the tab opens
    /// now, whatever run is admitted and whether or not it opened on this Mac before, landing
    /// once System State is still the front view of its window and no macOS dialog this process
    /// started is up. When a macOS permission gesture holds the dialog slot, queues the press so
    /// the row explains why and opens the tab once the slot frees; a helper panel out of sight
    /// holds no press, giving way first (`App::press_meets_unseen_panel`). `true` when its shell
    /// started or queued, or the guard that panel freed started the run's own install tab;
    /// `false` starts nothing: a headless instance, an installer already running, a tab open,
    /// opening or queued, an account that may not install them (the row offers no Install…
    /// then), or no front page.
    pub(crate) fn press_developer_tools_install(&mut self) -> bool {
        if self.headless
            || self.developer_tools_install_active()
            || self.workstation_setup.developer_tools_not_admin
        {
            return false;
        }
        let wanted = matches!(self.workstation_setup.developer_tools, Install::Wanted(_));
        // A helper panel out of sight never holds the press: it gives way first. The guard it
        // frees can start the install the run wanted (`Self::tick_developer_tools_install`),
        // whose tab is then this press's.
        let _ = self.press_meets_unseen_panel(None, std::time::Instant::now());
        match self.workstation_setup.developer_tools {
            Install::Opening { .. } if wanted => return true,
            Install::Opening { .. } | Install::Pressed | Install::PressedQueued { .. } => {
                return false;
            }
            _ => {}
        }
        let Some(window) = self
            .windows
            .keys()
            .find(|id| self.system_state_frontmost_in(**id))
            .copied()
        else {
            return false;
        };
        if self.developer_tools_install_blocked() {
            return false;
        }
        // A new press: the reason the last one opened nothing is spent.
        self.workstation_setup.developer_tools_after.refused = None;
        if self.access_dialog_up() {
            let wanted = match self.workstation_setup.developer_tools {
                Install::Wanted(revision) => Some(revision),
                _ => None,
            };
            self.workstation_setup.developer_tools = Install::PressedQueued { wanted };
            aterm_log::info!(
                "System State: Install… queued while a macOS permission gesture holds the dialog slot"
            );
            self.republish_system_state_developer_tools();
            return true;
        }
        let Some((ask, then)) = self.begin_pressed_install(own_aterm) else {
            return false;
        };
        let _act = crate::tab_marks::ActScope::gesture("install");
        self.open_off_loop(window, crate::tab_spawn::SpawnPlace::Tab, ask, then);
        true
    }

    /// How a person's pressed tab spawns and what it does once it lands, running this program's
    /// `aterm` (read only then), with the request now Pressed. Keys typed while it spawns stay in
    /// System State ([`crate::tab_spawn::SpawnAsk::unasked`]), so none reaches its password
    /// prompt; its landing tells the person if it did not start ([`pressed_tab_then`]). `None`
    /// starts nothing: a program that cannot name its `aterm` says so in the log.
    fn begin_pressed_install(
        &mut self,
        aterm: impl FnOnce() -> Option<std::path::PathBuf>,
    ) -> Option<(crate::tab_spawn::SpawnAsk, crate::tab_spawn::TabSpawnThen)> {
        let Some(exe) = aterm() else {
            aterm_log::warn!(
                "System State: this program cannot name itself; Install… opens no tab"
            );
            return None;
        };
        self.workstation_setup.developer_tools = Install::Pressed;
        aterm_log::info!(
            "System State: Install… starts the Mac Setup tab (`aterm {}`)",
            ARGS.join(" ")
        );
        let ask = crate::tab_spawn::SpawnAsk {
            unasked: true,
            // The program waits for its go line, which only the landing types.
            exec_command: Some(tab_command(&exe, self.developer_mode_is_theirs())),
            ..crate::tab_spawn::SpawnAsk::default()
        };
        Some((ask, pressed_tab_then()))
    }

    /// The landing check of a pressed tab: System State still the front view of `window`, no
    /// installer up and the dialog slot free, which the child holds through its lifetime. A tab
    /// that cannot land opens nothing: a macOS permission gesture that took the dialog slot since
    /// the press keeps it queued; System State left behind says why on the row, with Install…
    /// there for another press; an installer that started meanwhile is the row's wait.
    fn prepare_pressed_install_landing(&mut self, window: crate::WindowId) -> bool {
        if self.workstation_setup.developer_tools != Install::Pressed {
            return false;
        }
        let front = self.system_state_frontmost_in(window);
        let installer = front.then(|| activity(self.developer_tools_reading()));
        if installer == Some(Activity::Unread) {
            self.workstation_setup.developer_tools = Install::PressedQueued { wanted: None };
            aterm_log::info!(
                "System State: the Mac Setup tab Install… asked for waits for readable installer state"
            );
            self.republish_system_state_developer_tools();
            return false;
        }
        if installer == Some(Activity::Idle) {
            if !self.access_dialog_up() && self.reserve_access_dialog() {
                return true;
            }
            self.workstation_setup.developer_tools = Install::PressedQueued { wanted: None };
            self.wake_when_no_access_dialog();
            aterm_log::info!(
                "System State: the Mac Setup tab Install… asked for waits for the dialog slot"
            );
            self.republish_system_state_developer_tools();
            return false;
        }
        self.workstation_setup.developer_tools = Install::Opened;
        if !front {
            self.workstation_setup.developer_tools_after.refused = Some(NotOpened::LeftPage);
        }
        aterm_log::info!(
            "System State: the Mac Setup tab Install… asked for did not land{}",
            if front {
                ": an installer is up"
            } else {
                ": System State was no longer in front"
            }
        );
        false
    }

    pub(crate) fn developer_tools_install_active(&self) -> bool {
        self.workstation_setup
            .developer_tools_session
            .iter()
            .any(|terminal| !terminal.finished)
    }

    /// A live installer travels with its PTY when its durable receipt lets the successor
    /// recover the exact child's prompt ownership without submitting another command
    /// ([`Self::developer_tools_terminal_transferable`]).
    pub(crate) fn developer_tools_install_holds_handoff(&self) -> bool {
        self.workstation_setup
            .developer_tools_session
            .iter()
            .filter(|terminal| !terminal.finished)
            .any(|terminal| !self.developer_tools_terminal_transferable(terminal))
    }

    /// Whether `terminal`'s program may outlive this process with its prompt ownership: it is
    /// its tab session's own child, and its durable receipt beside the account's record lets the
    /// next process to hold its PTY (an update's successor, or the launch a keeper offers it to)
    /// recover that exact child's custody without submitting another command.
    fn developer_tools_terminal_transferable(&self, terminal: &Terminal) -> bool {
        self.pool.get(terminal.session).is_some_and(|child| {
            child.pid == child.shell_identity.pid
                && terminal.receipt.as_ref().is_some_and(|receipt| {
                    self.workstation_setup
                        .developer_tools_mark
                        .path
                        .as_deref()
                        .is_some_and(|mark| {
                            receipt.transferable(
                                mark,
                                child.ctx.self_id.as_str(),
                                child.pid,
                                child.shell_identity.birth,
                            )
                        })
                })
        })
    }

    /// The Mac Setup tabs whose programs aterm's quit leaves running (`App::end_run`,
    /// `App::close_window_logical`): each unfinished one whose exact child is transferable
    /// ([`Self::developer_tools_terminal_transferable`]) and whose master this process's keeper
    /// holds and keeps past a quit whose BYE names it
    /// ([`crate::workstation_setup::Host::developer_tools_keeper_carries`]). The quit closes none
    /// of their tabs and hangs none up, and its BYE names each ([`crate::keeper_link::bye`]): the
    /// keeper holds its PTY with no window, as it holds a crashed window's, until the next launch
    /// takes it at its HELLO and adopts its program with the receipt
    /// ([`Self::recover_developer_tools_install`]), so the install goes on and its one password
    /// is asked once. With no such keeper, none: the quit hangs each up, its attempt cut off and
    /// asked again by itself ([`Terminal::quit`]). Reads each receipt on disk and the keeper
    /// link's state.
    pub(crate) fn developer_tools_custody_at_exit(&self) -> Vec<u64> {
        let Some(carries) = self.workstation_setup.developer_tools_keeper_carries else {
            return Vec::new();
        };
        self.workstation_setup
            .developer_tools_session
            .iter()
            .filter(|terminal| {
                !terminal.finished
                    && carries(terminal.session)
                    && self.developer_tools_terminal_transferable(terminal)
            })
            .map(|terminal| terminal.session)
            .collect()
    }

    /// Leave the Mac Setup tabs aterm's quit carries ([`Self::developer_tools_custody_at_exit`])
    /// to the keeper, and say which: no teardown of this process's hangs their programs up, and
    /// the caller keeps their tabs out of the quit's closing and names them in its BYE. Once the
    /// quit is decided, which nothing then takes back.
    pub(crate) fn carry_developer_tools_install_at_exit(&self) -> Vec<u64> {
        let carried = self.developer_tools_custody_at_exit();
        for session in &carried {
            if let Some(session) = self.pool.get(*session) {
                session.leave_to_keeper();
            }
        }
        carried
    }

    /// Whether this launch's Mac Setup tabs run past their password where nothing could ask the
    /// person anything in them (H25): each unfinished one's own root run was seen alive
    /// ([`Terminal::past_password`]), and aterm's quit leaves it running ([`Terminal::carried`],
    /// as the watch last read [`Self::developer_tools_custody_at_exit`]), so neither System
    /// Settings coming forward nor its Quit & Reopen costs the install; Apple's installer window
    /// is not up, where the person finishes; this Mac's installers were read, and an install it
    /// reads is that run's own ([`Reading::run_chain`]); and no Mac Setup tab adopted from an
    /// earlier aterm runs outside this window's custody ([`Self::developer_tools_tab_adopted`]).
    /// Reads memory, save the window's last reading of the installer.
    pub(crate) fn developer_tools_install_carried(&self) -> bool {
        let running: Vec<&Terminal> = self
            .workstation_setup
            .developer_tools_session
            .iter()
            .filter(|terminal| !terminal.finished)
            .collect();
        if running.is_empty()
            || !running
                .iter()
                .all(|terminal| terminal.past_password && terminal.carried)
        {
            return false;
        }
        let known = self.developer_tools_known();
        let ours = known.live_run
            && running.iter().any(|terminal| {
                self.pool.get(terminal.session).is_some_and(|child| {
                    u32::try_from(child.pid)
                        .is_ok_and(|pid| pid > 1 && known.run_chain[1..].contains(&pid))
                })
            });
        let installers = match activity(known) {
            Activity::Idle => true,
            Activity::Installing => ours,
            Activity::Unread | Activity::AppleWindow => false,
        };
        installers
            && !known.apple_window
            && !self.pool.iter().any(|session| {
                runs_adopted_mac_setup(session)
                    && !self
                        .workstation_setup
                        .developer_tools_session
                        .iter()
                        .any(|terminal| terminal.session == session.id)
            })
    }

    /// Whether this launch's Mac Setup tab may still ask the person something, which holds
    /// setup's other prompts back (`App::access_gesture_up_beside_walk`): its program runs
    /// ([`Self::developer_tools_install_active`]) and is not carried past its password
    /// ([`Self::developer_tools_install_carried`]).
    pub(crate) fn developer_tools_install_may_ask(&self) -> bool {
        self.developer_tools_install_active() && !self.developer_tools_install_carried()
    }

    /// Mark each running Mac Setup tab whose program started the live root run `reading` names
    /// ([`Reading::run_chain`], past its first: the run itself) as past its password
    /// ([`Terminal::past_password`]), a mark that stays (`sudo` asks once), and note of each
    /// whether aterm's quit would carry it now ([`Terminal::carried`]).
    fn note_developer_tools_custody(&mut self, reading: &Reading) {
        let custody = self.developer_tools_custody_at_exit();
        let pool = &self.pool;
        for terminal in self
            .workstation_setup
            .developer_tools_session
            .iter_mut()
            .filter(|terminal| !terminal.finished)
        {
            terminal.carried = custody.contains(&terminal.session);
            if reading.live_run
                && pool.get(terminal.session).is_some_and(|child| {
                    u32::try_from(child.pid)
                        .is_ok_and(|pid| pid > 1 && reading.run_chain[1..].contains(&pid))
                })
            {
                terminal.past_password = true;
            }
        }
    }

    /// Whether `view` is a Mac Setup tab's terminal ([`Self::is_developer_tools_session`]).
    pub(crate) fn view_is_developer_tools_session(&self, view: crate::tab_model::ViewId) -> bool {
        self.view_store
            .get(view)
            .copied()
            .and_then(crate::tab_model::View::terminal_session)
            .is_some_and(|session| self.is_developer_tools_session(session))
    }

    /// Whether `session` is a Mac Setup tab's terminal: one this window opened or recovered,
    /// finished or not, or one adopted from an earlier aterm under aterm's pin of [`TAB_TITLE`].
    /// Its program is one install attempt, so only an update's handoff, whose successor fills its
    /// leaf with the same PTY, describes it (`App::capture_manifest`): a restore from any other
    /// manifest (a quit's, the crash journal's, the cold lane's, a closed tab's) would bring it
    /// back as a plain shell under that title, one more after each attempt. A program a quit or
    /// a crash leaves running comes back through the keeper, as a tab of its own.
    pub(crate) fn is_developer_tools_session(&self, session: u64) -> bool {
        self.workstation_setup
            .developer_tools_session
            .iter()
            .any(|terminal| terminal.session == session)
            || self
                .pool
                .get(session)
                .is_some_and(|child| child.handoff_local_id.is_some() && pins_tab_title(child))
    }

    /// The installer terminal `session` landed, as a pressed tab's landing leaves it, with no
    /// recovery receipt: it holds off other prompts until
    /// [`Self::finish_developer_tools_install`].
    #[cfg(all(test, target_os = "macos"))]
    pub(crate) fn land_install_session_for_test(&mut self, session: u64) {
        self.workstation_setup.developer_tools_attempt = Some(session);
        self.workstation_setup
            .developer_tools_session
            .push(Terminal::running(session, false, None));
    }

    /// The tab's program ended, or its final view detached: only that ends its prompt
    /// ownership. Returning true also keeps its completed terminal available for reading the
    /// result, and how the program ended is acted on once it is read
    /// ([`Self::developer_tools_install_ended`]).
    pub(crate) fn finish_developer_tools_install(&mut self, session: u64) -> bool {
        let Some(terminal) = self
            .workstation_setup
            .developer_tools_session
            .iter_mut()
            .find(|terminal| terminal.session == session)
        else {
            return false;
        };
        terminal.finished = true;
        if let Some(receipt) = terminal.receipt.take() {
            receipt.remove_if_matching();
        }
        self.release_unused_access_dialog(std::time::Instant::now());
        self.read_developer_tools_exit();
        self.republish_system_state_developer_tools();
        true
    }

    /// Read how each finished tab's program ended, once: an exit seen before its status was
    /// collected is read again at the next step ([`Self::developer_tools_install_step`]). A tab
    /// restored from an earlier aterm whose program is not this one's child may never give its
    /// status: its end is read as unknown.
    fn read_developer_tools_exit(&mut self) {
        let unread: Vec<(u64, bool)> = self
            .workstation_setup
            .developer_tools_session
            .iter()
            .filter(|terminal| terminal.finished && !terminal.ended)
            .map(|terminal| (terminal.session, terminal.recovered))
            .collect();
        for (session, recovered) in unread {
            if self.pool.get(session).is_none() {
                // Its tab is gone with it: nothing to read, and nothing to write into.
                self.developer_tools_install_ended(session, None);
                continue;
            }
            match self.exit_status(session, crate::app_tabs::ExitLook::First) {
                Some(exit) => self.developer_tools_install_ended(session, Some(exit)),
                None if recovered => self.developer_tools_install_ended(session, None),
                None => {}
            }
        }
    }

    /// Record how the tab's attempt ended in the account's record, which decides the next automatic
    /// ask, as [`aterm_types::clt_mark::tab_end`] decides it: done; declined for a stop (a Ctrl-C at
    /// the password prompt, System State's Stop) or a tab closed while its program ran and aterm
    /// runs on; nothing for what aterm's quit hung the program up with, so the record reads it cut
    /// off ([`Terminal::quit`]); given back for an end before anything was asked (Low Data Mode, a
    /// program that never ran); unfinished for anything else, an unknown end included. A delayed
    /// result never replaces a newer attempt's state. An unfinished end wants
    /// the tab again by itself on the record's schedule where the tools are missing; with them
    /// there, the elevation's other steps are read again first, and only a fresh reading that
    /// still owes one wants it ([`App::root_steps_owed`]), so a tab with nothing left to do never
    /// opens again. Actual installed tools refresh ordinary repository observation; no exit alone
    /// grants repository readiness.
    pub(crate) fn developer_tools_install_ended(
        &mut self,
        session: u64,
        exit: Option<aterm_pty::ChildExit>,
    ) {
        let Some(terminal) = self
            .workstation_setup
            .developer_tools_session
            .iter_mut()
            .find(|terminal| terminal.session == session && !terminal.ended)
        else {
            return;
        };
        terminal.finished = true;
        terminal.ended = true;
        #[cfg(target_os = "macos")]
        if let TerminalPurpose::NameRule { generation, origin } = terminal.purpose {
            let retained = origin.is_retained(self)
                && self
                    .windows_with_focused_session(session)
                    .contains(&origin.window());
            let result = if !retained {
                Err("the window or content that requested name permission repair was closed or replaced".into())
            } else {
                match exit {
                    Some(aterm_pty::ChildExit::Code(0)) => Ok(()),
                    Some(status) => Err(format!(
                        "name permission repair did not succeed ({status:?})"
                    )),
                    None => {
                        Err("the name permission repair's terminal result is unavailable".into())
                    }
                }
            };
            self.machine_name_authorization_finished(generation, result);
            return;
        }
        let told = terminal.told;
        let closed = terminal.closed;
        let quit = terminal.quit;
        let claim = terminal.claim.take();
        let recovered = terminal.recovered;
        // EOF can precede the child's status. A person may start a retry meanwhile: retain the
        // old terminal's result, but only the current attempt changes the retry's state or shim.
        let current = self.workstation_setup.developer_tools_attempt == Some(session);
        // This window's own child said Low Data Mode, which it says only once it read no root
        // step owed beside the tools, before its password ask: a fresh reading, where its claim
        // still held the record as its own line ([`Mark::end`]).
        let mut read_nothing_owed = false;
        // What the exit says, for the page and this launch's gates; how the attempt ends in the
        // account's record, which decides the next automatic ask, is the record's own decision
        // ([`aterm_types::clt_mark::tab_end`]).
        match exit {
            Some(aterm_pty::ChildExit::Code(0)) => {
                aterm_log::info!("System State: the Mac Setup tab installed them");
            }
            Some(aterm_pty::ChildExit::Code(1)) => {
                aterm_log::info!("System State: the Mac Setup tab said it did not finish");
                if current {
                    self.workstation_setup.developer_tools_unfinished = true;
                }
            }
            Some(aterm_pty::ChildExit::Code(code))
                if code == i32::from(aterm_types::macos_setup::COMMAND_LINE_TOOLS_TAB_STOPPED) =>
            {
                // A person declined (a Ctrl-C at the password prompt or later), unless aterm's
                // quit hung the program up, which it cannot tell from a Ctrl-C: then the record
                // reads the attempt cut off. Until a decline's long pause passes, the way on is
                // the person's own `aterm init` (or Install…, while the tools are missing).
                aterm_log::info!("System State: the Mac Setup tab was stopped");
                if current {
                    self.workstation_setup.developer_tools_unfinished = true;
                }
            }
            Some(aterm_pty::ChildExit::Code(code))
                if code
                    == i32::from(
                        aterm_types::macos_setup::COMMAND_LINE_TOOLS_TAB_WINDOW_CLOSED,
                    ) =>
            {
                // Nothing says which button closed Apple's window: Not Now and Cancel, but also
                // Done on Apple's own failure and a quit at logout. Unfinished, asked again by
                // itself on the record's schedule, never a decline.
                aterm_log::info!(
                    "System State: the Mac Setup tab said Apple's window closed without them"
                );
                if current {
                    self.workstation_setup.developer_tools_unfinished = true;
                }
            }
            Some(aterm_pty::ChildExit::Code(code))
                if code == i32::from(aterm_types::macos_setup::COMMAND_LINE_TOOLS_TAB_LOW_DATA) =>
            {
                // The tab emits this code only before entering sudo, so its claim goes back; a
                // later diagnosis of Low Data Mode is a generic failure, which is unfinished.
                aterm_log::info!("System State: the Mac Setup tab waits for Low Data Mode");
                if current {
                    let setup = &mut self.workstation_setup;
                    if setup.developer_tools == Install::Opened {
                        setup.developer_tools = setup
                            .developer_tools_want
                            .map_or(Install::Unasked, Install::Wanted);
                    }
                    setup.developer_tools_low_data = Some(true);
                    if setup.developer_tools_reads_mac && setup.developer_tools_network.is_none() {
                        setup.developer_tools_network = NetworkWatch::start();
                    }
                    read_nothing_owed = !recovered && claim.is_some();
                }
            }
            Some(aterm_pty::ChildExit::Code(code))
                if code
                    == i32::from(aterm_types::macos_setup::COMMAND_LINE_TOOLS_TAB_NOT_ADMIN) =>
            {
                // Asked again by itself on the record's schedule, and this launch's gate holds
                // it while the account stays no administrator's.
                aterm_log::info!(
                    "System State: the Mac Setup tab found an account that may not install them"
                );
                if current {
                    self.workstation_setup.developer_tools_not_admin = true;
                }
            }
            Some(aterm_pty::ChildExit::Code(code))
                if code
                    == i32::from(
                        aterm_types::macos_setup::COMMAND_LINE_TOOLS_TAB_NOT_OWN_LOGIN,
                    )
                    || code
                        == i32::from(
                            aterm_types::macos_setup::COMMAND_LINE_TOOLS_TAB_INSTALLER_HELD,
                        ) =>
            {
                aterm_log::info!(
                    "System State: the Mac Setup tab deferred before its password ask ({code})"
                );
                // Nothing was asked (another login holds the console, or an installer that is
                // not this tab's runs): the claim goes back, and the want stands again behind
                // the window's own gates (its own console, a live installer) and the record's.
                if current {
                    if code
                        == i32::from(
                            aterm_types::macos_setup::COMMAND_LINE_TOOLS_TAB_INSTALLER_HELD,
                        )
                    {
                        // Read the real custody once; the combined exit code cannot supply an
                        // installer observation for the ordinary gate.
                        let _ = self.developer_tools_reading();
                    }
                    let setup = &mut self.workstation_setup;
                    setup.developer_tools_unfinished = true;
                    if setup.developer_tools == Install::Opened {
                        setup.developer_tools = setup
                            .developer_tools_want
                            .map_or(Install::Unasked, Install::Wanted);
                    }
                }
            }
            Some(other) => {
                aterm_log::info!("System State: the Mac Setup tab ended: {other:?}");
                if current {
                    self.workstation_setup.developer_tools_unfinished = true;
                }
                if !told {
                    let line = format!(
                        "{}.",
                        aterm_types::macos_setup::COMMAND_LINE_TOOLS_NOT_FINISHED
                    );
                    self.write_developer_tools_line(session, &line);
                }
            }
            None => {
                if current {
                    self.workstation_setup.developer_tools_unfinished = true;
                }
                if closed {
                    aterm_log::info!("System State: the Mac Setup tab was closed while it ran");
                }
            }
        }
        let how = aterm_types::clt_mark::tab_end(
            exit.map(|exit| match exit {
                aterm_pty::ChildExit::Code(code) => aterm_types::clt_mark::TabExit::Code(code),
                aterm_pty::ChildExit::Signal(_) => aterm_types::clt_mark::TabExit::Signal,
            }),
            closed,
            quit,
        );
        match (how, &claim) {
            (Some(how), Some(claim)) => {
                if self.workstation_setup.developer_tools_mark.end(claim, how)
                    == aterm_types::clt_mark::Took::Line
                    && read_nothing_owed
                {
                    // Its read is the fresh one, so the cached hint of root work goes, and the
                    // retained want waits for the download instead of opening another tab. An
                    // adopted child's, or one whose claim a newer attempt replaced (its end
                    // written under that attempt, [`aterm_types::clt_mark::Took::Replaced`]) or
                    // the record no longer holds, reads for no one here.
                    self.workstation_setup.developer_tools_root_owed = RootSteps::default();
                }
            }
            // What aterm's quit hung its program up with: no end is written, so once its asker
            // is gone the record reads the attempt cut off, asked again by itself, never
            // declined.
            (None, _) => aterm_log::info!(
                "System State: the Mac Setup tab went with aterm's quit; its ask comes again by \
                 itself"
            ),
            (Some(_), None) => {}
        }
        // An attempt that did not finish is asked again by itself once the record's next try is
        // due ([`Mark::held`]): the want stands again, as after Low Data Mode, and the window's
        // deadline wakes it then ([`crate::workstation_setup::Host::deadline`]). With the tools
        // there, the attempt may have done the steps it was for: what is owed is read again, and
        // only a fresh reading that still owes a step wants the tab ([`Self::root_steps_owed`]).
        if current
            && how == Some(aterm_types::clt_mark::End::Unfinished)
            && self.workstation_setup.developer_tools == Install::Opened
        {
            let setup = &mut self.workstation_setup;
            setup.developer_tools = if setup.developer_tools_shim.tools_present() {
                setup.read_root_steps_again();
                Install::Unasked
            } else {
                setup
                    .developer_tools_want
                    .map_or(Install::Unasked, Install::Wanted)
            };
        }
        // The tools are there: exit 0 said its program installed them, or they came another way
        // while it ran (Apple's dialog from another app, a package follow-up's install), whatever
        // this end said. The watch left the row to this end ([`Self::developer_tools_watch`]),
        // and a finished run polls no status, so it is read again here, once, and the shim comes
        // off new shells now, ahead of the next pass's word.
        if current
            && (matches!(exit, Some(aterm_pty::ChildExit::Code(0)))
                || self.workstation_setup.developer_tools_shim.tools_present())
        {
            self.read_repositories_again();
            self.settle_developer_tools_want();
        }
        if current {
            self.settle_developer_tools_shim();
            self.settle_missing_tools_exception();
        }
        // Its work is done and nothing in it waits on anyone, so the current attempt's tab goes by
        // itself and no tab is left for a person to close (nux round 9, item 12); an older one's
        // late end keeps its tab and its result. The last tab of its window stays, since closing
        // that would end the window.
        if current
            && matches!(exit, Some(aterm_pty::ChildExit::Code(0)))
            && !self.close_ends_window(session)
            && let Err(why) = self.close_session_by_id(session)
        {
            aterm_log::info!("System State: the Mac Setup tab stays open: {why}");
        }
    }

    /// Write `line` into the tab `session` as its last line, as a shell that failed at start
    /// keeps its own.
    fn write_developer_tools_line(&mut self, session: u64, line: &str) {
        if let Some(pooled) = self.pool.get(session) {
            let mut term = crate::term_lock(&pooled.term);
            let lead = if term.cursor().col == 0 { "" } else { "\r\n" };
            term.process(format!("{lead}{line}\r\n").as_bytes());
        }
        let _ = self.admit_output_redraws(session, std::time::Instant::now());
    }

    /// The tab `session`'s last view went: its end is read now ([`Self::finish_developer_tools_install`]),
    /// as a tab closed while its program ran where that end was not seen first
    /// ([`Terminal::closed`]), unless it goes with this aterm's quit ([`Terminal::quit`]), and the
    /// tab leaves the window's custody.
    pub(crate) fn retire_developer_tools_install(&mut self, session: u64) {
        if let Some(terminal) = self
            .workstation_setup
            .developer_tools_session
            .iter_mut()
            .find(|terminal| terminal.session == session && !terminal.finished)
        {
            terminal.closed = !terminal.quit;
        }
        if self.finish_developer_tools_install(session) {
            #[cfg(target_os = "macos")]
            if let Some(TerminalPurpose::NameRule { generation, .. }) = self
                .workstation_setup
                .developer_tools_session
                .iter()
                .find(|terminal| terminal.session == session && !terminal.ended)
                .map(|terminal| terminal.purpose)
            {
                self.machine_name_authorization_finished(generation,
                    Err("the name permission repair tab closed before a successful result was collected".into()));
            }
            self.workstation_setup
                .developer_tools_session
                .retain(|terminal| terminal.session != session);
        }
    }

    /// The close of aterm's last window quits it (`App::close_window_logical`): each Mac Setup
    /// tab whose program still runs goes with the process, its attempt never declined
    /// ([`Terminal::quit`]), as on any other quit, which closes no tab before the process ends;
    /// one the quit carries ([`Self::developer_tools_custody_at_exit`]) stays in the pool to the
    /// end, its program running on.
    pub(crate) fn note_developer_tools_tabs_quit(&mut self) {
        for terminal in self
            .workstation_setup
            .developer_tools_session
            .iter_mut()
            .filter(|terminal| !terminal.finished)
        {
            terminal.quit = true;
        }
    }

    /// The window and tab of the Mac Setup tab, while it is open (its program still
    /// running first): what the headline's Go to tab selects.
    pub(crate) fn developer_tools_tab(&self) -> Option<(crate::WindowId, usize)> {
        let mut sessions: Vec<&Terminal> = self
            .workstation_setup
            .developer_tools_session
            .iter()
            .filter(|terminal| terminal.tools())
            .collect();
        sessions.sort_by_key(|terminal| terminal.finished);
        sessions.into_iter().find_map(|terminal| {
            self.windows.iter().find_map(|(&window, state)| {
                state
                    .tab_set
                    .tabs()
                    .iter()
                    .position(|tab| {
                        tab.root.any_leaf(&mut |view| {
                            self.view_store
                                .get(*view)
                                .copied()
                                .and_then(crate::tab_model::View::terminal_session)
                                == Some(terminal.session)
                        })
                    })
                    .map(|index| (window, index))
            })
        })
    }

    /// The headline's Go to tab: the Mac Setup tab comes to the front of its window,
    /// and that window to the front. `false` when it is not open.
    pub(crate) fn go_to_developer_tools_tab(&mut self) -> bool {
        let Some((window, index)) = self.developer_tools_tab() else {
            return false;
        };
        self.switch_tab_in(window, index);
        self.raise_window(window);
        true
    }

    /// The Apple-window card's Go to installer: Apple's own Command Line Tools installer window,
    /// already open, comes to the front ([`probe::apple_installer_pid`], read at the press). It
    /// only goes there: it starts no install and asks for no permission. `false` when that window
    /// is no longer open, or macOS did not bring it forward.
    pub(crate) fn go_to_developer_tools_installer(&self) -> bool {
        #[cfg(target_os = "macos")]
        {
            use crate::platform::AppRt as _;
            probe::apple_installer_pid().is_some_and(|pid| self.apprt.activate_instance(pid))
        }
        #[cfg(not(target_os = "macos"))]
        {
            false
        }
    }

    /// The launch's first look: what the want keeps ([`Self::settle_developer_tools_want`]: the
    /// account's reading, the Low Data Mode watch, the shim), and a want that is due opens its
    /// tab. A launch that found the tools missing wants them from its setup host's start
    /// ([`crate::workstation_setup::Host::for_launch_marked`]).
    pub(crate) fn developer_tools_first_look(&mut self) {
        self.settle_developer_tools_want();
        self.tick_developer_tools_install();
    }

    /// The watch's second ([`watch_for_tools`]), with what it read of this Mac's installers on its
    /// own thread (`None`: the tools exist now, and the watch has ended): the window's looks read
    /// that reading until the next second ([`Self::developer_tools_known`]), so none scans the
    /// process table on the loop. The want is settled again ([`Self::settle_developer_tools_want`])
    /// only where the shim's word moves (an installer started or ended, a run's admission
    /// changed) or the tools arrived, which takes the shim off new shells however they came; a
    /// second that moves nothing writes nothing. Their arrival also ends the automatic opening:
    /// a request not yet opened goes ([`Install::Unasked`]), a person's press queued behind a
    /// macOS dialog with it, and so does the launch's want ([`LAUNCH_WANT`]), which stood for the
    /// tools alone; a pass's want stays until a later pass says it no longer waits on them. With
    /// no install tab of this window's running, whose own end reads it however it ends
    /// ([`Self::developer_tools_install_ended`]), the GitHub repositories row is read again at
    /// once ([`Self::read_repositories_again`]), so a page that showed
    /// Install… for tools that came another way (Apple's dialog from another app, a package
    /// follow-up's install) moves on, as no status poll of a finished run would read it. A want
    /// waiting on a quiet keyboard or a free dialog slot looks again. A reading whose live root
    /// run a tab's program started marks that tab past its password
    /// ([`Terminal::past_password`]), and each reading notes whether a quit would carry each tab
    /// ([`Terminal::carried`]).
    pub(crate) fn developer_tools_watch(&mut self, reading: Option<Reading>) {
        self.workstation_setup.developer_tools_seen = reading;
        if let Some(reading) = &reading {
            self.note_developer_tools_custody(reading);
        }
        if reading.is_none() {
            // The elevation's other steps still owed keep the launch's want: its one password
            // does them on a Mac that has the tools too ([`Self::root_steps_owed`]).
            let owed = self.root_steps_still_owed();
            let setup = &mut self.workstation_setup;
            if !owed {
                if matches!(
                    setup.developer_tools,
                    Install::Wanted(_) | Install::PressedQueued { .. }
                ) {
                    setup.developer_tools = Install::Unasked;
                }
                if setup.developer_tools_want == Some(LAUNCH_WANT) {
                    setup.developer_tools_want = None;
                }
            }
            if !self.developer_tools_install_active() {
                self.read_repositories_again();
            }
            self.republish_system_state_developer_tools();
        }
        let said = self.workstation_setup.developer_tools_shim_said.get();
        if reading.is_none() || said != Some(self.developer_tools_on_their_way()) {
            self.settle_developer_tools_want();
        }
        self.tick_developer_tools_install();
    }

    /// Whether the GitHub repositories row, reading `reading`, still waits on Apple's Command
    /// Line Tools ([`crate::SetupStatus::tools_owed`]): the shim reads them missing, or they were
    /// found installed after the reading the page had then and no other has landed since
    /// ([`Settling`], which a newer reading or [`SETTLE_BOUND`] ends here).
    pub(crate) fn developer_tools_owed(&mut self, reading: Option<&Observation>) -> bool {
        let setup = &mut self.workstation_setup;
        setup
            .developer_tools_after
            .settle(reading, std::time::Instant::now());
        setup.developer_tools_shim.tools_missing() || setup.developer_tools_after.settling.is_some()
    }

    /// The tools are installed: the GitHub repositories row is read again at once and the run
    /// waiting on them wakes (`InitialSetupHandlers::tools_installed`), the row staying calm until
    /// that reading lands ([`Settling`]).
    fn read_repositories_again(&mut self) {
        self.workstation_setup.developer_tools_after.settling = Some(Settling {
            reading: self
                .system_state_host
                .latest_status()
                .repositories_reading()
                .cloned(),
            until: std::time::Instant::now() + SETTLE_BOUND,
        });
        if let Some(handlers) = self.system_state_handlers() {
            (handlers.tools_installed)();
        }
    }

    /// The launch's reading of setup's one early elevation ([`read_root_steps`]): `owed`, a root
    /// step beside the tools is still to do (developer mode, the rule that names the Mac after
    /// admission, the LAN broker). Owed, the launch wants the tab as it wants it for missing tools
    /// ([`LAUNCH_WANT`]), on a Mac that has them too, so its one password does every root step
    /// setup has, asked by aterm itself with no press; the same holds and gates keep it (no
    /// scratch world, the account's [`Mark`], a quiet keyboard, no macOS dialog up, an
    /// administrator's account), and nothing else waits on it. Developer mode that is the
    /// person's ([`Self::developer_mode_is_theirs`]) owes nothing.
    pub(crate) fn root_steps_owed(&mut self, owed: RootSteps) {
        self.workstation_setup.developer_tools_root_owed = owed;
        self.workstation_setup.root_steps_awaited = false;
        if self.root_steps_still_owed() {
            let setup = &mut self.workstation_setup;
            let want = *setup.developer_tools_want.get_or_insert(LAUNCH_WANT);
            if setup.developer_tools == Install::Unasked {
                setup.developer_tools = Install::Wanted(want);
            }
        }
        self.settle_developer_tools_want();
        self.tick_developer_tools_install();
    }

    /// Whether developer mode is the person's to leave as it reads: setup's elevation finished
    /// once on this account ([`Mark::finished`]), so developer mode off now was turned off since
    /// (aterm's Settings ▸ Security, `DevToolsSecurity -disable`), or they opted the Debugger row
    /// out ([`Self::access_opt_outs`]). The automatic tab then owes nothing for it, and its root
    /// half leaves it as it reads where it asks for another step.
    pub(crate) fn developer_mode_is_theirs(&self) -> bool {
        self.workstation_setup.developer_tools_mark.finished() || self.debugger_opted_out()
    }

    /// Whether the elevation owes a root step beside the tools, as the last reading said
    /// ([`Self::root_steps_owed`]), developer mode the person's owing nothing.
    fn root_steps_still_owed(&self) -> bool {
        let owed = self.workstation_setup.developer_tools_root_owed;
        owed.other || (owed.debugger && !self.developer_mode_is_theirs())
    }

    /// Whether the elevation's tab has anything to do: the tools are missing, or its other root
    /// steps are owed ([`Self::root_steps_still_owed`]).
    fn elevation_owed(&self) -> bool {
        !self.workstation_setup.developer_tools_shim.tools_present() || self.root_steps_still_owed()
    }

    /// Inject a revision-bound want for installer custody and Stop fixtures. Live automatic
    /// wants come from the launch's tools reading or the independent root-step probe.
    #[cfg(test)]
    pub(crate) fn want_developer_tools_install(&mut self, revision: u64, wanted: bool) {
        let install = &mut self.workstation_setup.developer_tools;
        *install = if wanted {
            install.asked(revision)
        } else {
            install.withdrawn_by(revision)
        };
        // The want outlives the tab's opening ([`Install::Opened`]): only a later pass that no
        // longer waits on the tools ends it, never an older one.
        let want = &mut self.workstation_setup.developer_tools_want;
        *want = if wanted {
            Some(want.map_or(revision, |previous| previous.max(revision)))
        } else {
            want.filter(|previous| *previous > revision)
        };
        self.settle_developer_tools_want();
        self.tick_developer_tools_install();
    }

    /// No pass wants the Command Line Tools any more: a person's Stop, or a plan that leaves the
    /// GitHub repositories out (the Developer group turned off).
    pub(crate) fn withdraw_developer_tools_want(&mut self) {
        self.workstation_setup.developer_tools_want = None;
        self.settle_developer_tools_want();
    }

    /// Settle what a pass's want of the Command Line Tools keeps. While a pass wants them the
    /// window reads, once, whether this account may install them, and watches for Low Data Mode,
    /// which holds the download back; the watch ends with the want, unless the mode still holds
    /// the row's wait. Then the shim follows ([`Self::settle_developer_tools_shim`]).
    pub(crate) fn settle_developer_tools_want(&mut self) {
        let setup = &mut self.workstation_setup;
        let wanted = setup.developer_tools_want.is_some();
        if setup.developer_tools_reads_mac {
            if wanted {
                if setup.developer_tools_network.is_none() {
                    setup.developer_tools_network = NetworkWatch::start();
                }
                // The account's groups read only on a Unix host; elsewhere no Mac's tools are due.
                #[cfg(unix)]
                let admin = aterm_uds::account::administrator();
                #[cfg(not(unix))]
                let admin = None;
                if admin == Some(false) {
                    setup.developer_tools_not_admin = true;
                }
            } else if setup.developer_tools_low_data != Some(true) {
                setup.developer_tools_network = None;
                setup.developer_tools_low_data = None;
            }
        }
        self.settle_developer_tools_shim();
    }

    /// The git-and-friends shim stands in front of `/usr/bin` for the shells aterm spawns from the
    /// first launch until the Command Line Tools exist, whatever becomes of an install (design
    /// §6.3), and this settles it: it comes off new shells once they exist, and its line says they
    /// are on their way ([`Self::developer_tools_on_their_way`]). Its directory stays for the
    /// shells that have it.
    pub(crate) fn settle_developer_tools_shim(&self) {
        let setup = &self.workstation_setup;
        let says = setup
            .developer_tools_shim
            .settle(|| self.developer_tools_on_their_way());
        setup.developer_tools_shim_said.set(Some(says));
    }

    /// Whether an install of the tools is on its way: this account may install them, and either
    /// no install of them ended unfinished since one last started
    /// ([`crate::workstation_setup::Host::developer_tools_unfinished`]) while an install tab's
    /// program runs or a want stands that may open one ([`Self::developer_tools_install_authorized`]:
    /// a launch's want under a hold or a durable Stop, or a pass's its run no longer admits,
    /// installs nothing), or an install of them is under way on this Mac whoever started it, as the
    /// window knows it ([`Self::developer_tools_installing_known`]: the package follow-up's root
    /// half, Apple's installer window, another installer).
    pub(crate) fn developer_tools_on_their_way(&self) -> bool {
        let setup = &self.workstation_setup;
        !setup.developer_tools_not_admin
            && ((!setup.developer_tools_unfinished
                && (self.developer_tools_install_active()
                    || setup.developer_tools_want.is_some_and(|revision| {
                        self.developer_tools_install_authorized(revision)
                    })))
                || self.developer_tools_installing_known())
    }

    /// The network watch read Low Data Mode `on` or off. Off, the automatic tab may open (its
    /// opening unspent), and the watch ends unless a pass still wants the tools.
    pub(crate) fn developer_tools_low_data(&mut self, on: bool) {
        let setup = &mut self.workstation_setup;
        // A reading the monitor submitted before its watch ended is moot.
        if setup.developer_tools_network.is_none() {
            return;
        }
        let was = setup.developer_tools_low_data.replace(on);
        if !on && setup.developer_tools_want.is_none() {
            setup.developer_tools_network = None;
            setup.developer_tools_low_data = None;
        }
        if was != Some(on) {
            aterm_log::info!(
                "System State: Low Data Mode is {} for the Command Line Tools",
                if on { "on" } else { "off" }
            );
            self.republish_system_state_developer_tools();
        }
        self.tick_developer_tools_install();
    }

    /// Whether the automatic tab waits on the network: Low Data Mode holds the download back, or
    /// a watch just started has not read it yet ([`FIRST_READING`]). With the tools there is no
    /// download, so the elevation's other steps never wait on it. A cached independent root
    /// need also admits the same transaction; its child checks that need again before asking.
    fn developer_tools_wait_network(&self) -> bool {
        let setup = &self.workstation_setup;
        if setup.developer_tools_shim.tools_present() || self.root_steps_still_owed() {
            return false;
        }
        match setup.developer_tools_low_data {
            Some(on) => on,
            None => setup
                .developer_tools_network
                .as_ref()
                .is_some_and(|watch| watch.since.elapsed() < FIRST_READING),
        }
    }

    /// [`Install::due`] for a want still authorized, once the keyboard was quiet for 2.5 s
    /// ([`crate::hands_off_keys`]): the landed tab takes the window's keys. Never while the
    /// account's record holds the automatic ask ([`Mark::held`]: another asker's attempt, a try not
    /// due yet, a person's decline, a record that cannot be read, a claim that failed); nor once
    /// the tools exist, however they came (the shim's reading,
    /// [`crate::clt_shim::Shim::tools_present`]), and the elevation owes nothing else
    /// ([`Self::elevation_owed`]). A live
    /// installer, as the window knows it ([`Self::developer_tools_installing_known`]; the
    /// landing's check reads it live), an earlier aterm's tab running on with no custody here
    /// ([`Self::developer_tools_tab_adopted`]), Low Data Mode
    /// ([`Self::developer_tools_wait_network`]) and an account that is none of the Mac's
    /// administrators hold the request, without consuming the automatic attempt. The window reads
    /// [`Self::developer_tools_install_held`], which says which of these holds it.
    #[cfg(test)]
    fn developer_tools_install_due(&self, dialog_up: bool) -> bool {
        self.developer_tools_install_held(dialog_up).is_none()
    }

    /// What holds the automatic tab ([`Self::developer_tools_install_due`]), in the order the
    /// gates are read: `None` when it is due ([`DIALOG_UP`] while another macOS dialog of this
    /// process is up, whose end wakes the window, [`Self::begin_developer_tools_install`]). The
    /// window logs each new answer while a want stands ([`Self::begin_developer_tools_install`]),
    /// so a tab that never opens says why.
    fn developer_tools_install_held(&self, dialog_up: bool) -> Option<&'static str> {
        self.developer_tools_install_gate(dialog_up, true)
    }

    /// Whether the automatic Mac Setup tab is, or may yet be, this window's next macOS ask: its
    /// shell is booked ([`Install::Opening`] for a want), or nothing holds it but what passes by
    /// itself ([`Self::developer_tools_install_held`]): a key typed in the last 2.5 s, another
    /// macOS dialog of this process, the network watch's first reading, or, while the launch's
    /// reading of the elevation's other steps is out
    /// ([`crate::workstation_setup::Host::root_steps_awaited`]), no want yet, nothing yet owed,
    /// or Low Data Mode: independent root work may still precede its constrained download.
    /// A walk that would start by itself yields to it (`App::look_for_walk_start`), and so do
    /// System State's access steps and a control verb's page (`App::no_gesture_up`): a helper
    /// panel, alert or page nobody answers would hold setup's one password for as long as it
    /// stands. Reads memory alone. Only a window that opens tabs ever wants the tab (the
    /// launch's want comes from its reading of this Mac), so a headless one never yields.
    pub(crate) fn mac_setup_tab_comes_next(&self) -> bool {
        if matches!(
            self.workstation_setup.developer_tools,
            Install::Opening { wanted: Some(_) }
        ) {
            return true;
        }
        match self.developer_tools_install_gate(false, false) {
            None | Some(FIRST_NETWORK_READING) => true,
            Some(NO_WANT | NOTHING_OWED | "Low Data Mode") => {
                self.workstation_setup.root_steps_awaited
            }
            Some(_) => false,
        }
    }

    /// Whether an install of Apple's tools runs on this Mac outside this launch's own Mac Setup
    /// tab (whose program [`Self::developer_tools_install_active`] covers): an earlier aterm's tab
    /// this window adopted runs on ([`Self::developer_tools_tab_adopted`]), or an installer is up
    /// ([`Self::developer_tools_installing_known`]: aterm's root half, Apple's installer window,
    /// another installer). Setup's one Mac password prompt may be asking in that tab, or the
    /// install going. A walk that would start by itself waits for its end
    /// (`App::look_for_walk_start`): Full Disk Access switched on brings macOS's Quit & Reopen,
    /// whose quit hangs up a tab no keeper carries mid-install, so its next try would ask for the
    /// password again and start the install over; and System Settings coming forward would take
    /// the keyboard from the prompt or the installer's window. None of that holds while this
    /// window's own tabs are carried past their password
    /// ([`Self::developer_tools_install_carried`]): the install it reads is their run's, which the
    /// quit carries, and nothing asks in them. Reads memory, save the window's last reading of
    /// the installer.
    pub(crate) fn developer_tools_install_elsewhere(&self) -> bool {
        (self.developer_tools_tab_adopted() || self.developer_tools_installing_known())
            && !self.developer_tools_install_carried()
    }

    /// [`Self::developer_tools_install_held`]'s gates in order, reading the keyboard's rest only
    /// when `keys` says so.
    fn developer_tools_install_gate(&self, dialog_up: bool, keys: bool) -> Option<&'static str> {
        let setup = &self.workstation_setup;
        if setup.developer_tools_scratch_world {
            Some("a scratch world (a check's bundle asks for no password)")
        } else if let Some(why) = setup.developer_tools_mark.held() {
            Some(why)
        } else if setup.developer_tools_not_admin {
            Some("this account is no administrator")
        } else if self.developer_tools_wait_network() {
            Some(if setup.developer_tools_low_data == Some(true) {
                "Low Data Mode"
            } else {
                FIRST_NETWORK_READING
            })
        } else if keys
            && !crate::hands_off_keys(true, self.last_keystroke_at, std::time::Instant::now())
        {
            Some("a key typed in the last 2.5 s")
        } else if !matches!(setup.developer_tools, Install::Wanted(_)) {
            Some(NO_WANT)
        } else if !matches!(setup.developer_tools, Install::Wanted(revision)
            if self.developer_tools_install_authorized(revision))
        {
            Some("no admitted run, or setup held")
        } else if !setup.developer_tools.due(dialog_up) {
            Some(DIALOG_UP)
        } else if !self.elevation_owed() {
            Some(NOTHING_OWED)
        } else if self.developer_tools_tab_adopted() {
            Some("an earlier aterm's tab runs on")
        } else {
            match activity(self.developer_tools_known()) {
                Activity::Idle => None,
                Activity::Unread => Some("the installer state could not be read"),
                Activity::Installing | Activity::AppleWindow => Some("an installer runs"),
            }
        }
    }

    /// Whether a session adopted from an earlier aterm runs the Mac Setup tab's program:
    /// aterm's pin of [`TAB_TITLE`], which an adoption carries with the tab, over a child that has
    /// not ended ([`child_ended`]). Its own `sudo` may be asking for the password now, with no
    /// placeholder, stamp or installer yet for a probe to see, and a tab whose recovery record this
    /// launch could not read (none, a malformed one, another place) gives this one no custody,
    /// so the automatic tab waits for it to end rather than ask beside it. A tab this launch has
    /// custody of holds the automatic tab already ([`Install::Opened`]).
    fn developer_tools_tab_adopted(&self) -> bool {
        self.pool.iter().any(runs_adopted_mac_setup)
    }

    /// A launch's want stands on nothing but no hold and no Stop kept in the run's records: it is
    /// the owner's standing wish on a Mac without the tools. A pass's stands on its run's admission.
    fn developer_tools_install_authorized(&self, revision: u64) -> bool {
        let status = self.system_state_host.latest_status();
        if revision == LAUNCH_WANT {
            let stopped = status.finished && status.run.is_some_and(|run| !run.allowed);
            return self.workstation_setup.setup_unheld() && !stopped;
        }
        self.permission_steps_admitted()
            && self.workstation_setup.setup_unheld()
            && !status.changing
            && status
                .run
                .is_some_and(|run| run.allowed && run.revision == revision)
    }

    /// Recheck before registration changes the active tab, reserving the worker's dialog
    /// slot through the installer child's lifetime. A key typed while the shell started
    /// defers it too, as does an earlier aterm's tab running on
    /// ([`Self::developer_tools_tab_adopted`]); a newer want survives a refused shell. Tools that
    /// arrived since the shell was booked, with nothing else owed, land nothing, and the request
    /// goes ([`Install::Unasked`]). Last of all the landing claims the ask in the account's record
    /// ([`Mark::claim`]): one another asker won since the shell was booked (the follow-up of a
    /// package install or update, whose `sudo` may be asking now), or a claim that failed, lands
    /// nothing, and the want waits on the record ([`Self::developer_tools_asked_elsewhere`]):
    /// that asker's end, read on [`LOOK_AGAIN`], or the failed claim's pause.
    fn prepare_developer_tools_install_landing(&mut self, revision: u64) -> bool {
        let Install::Opening { wanted } = self.workstation_setup.developer_tools else {
            return false;
        };
        let missing = self.elevation_owed();
        if wanted == Some(revision)
            && missing
            && crate::hands_off_keys(true, self.last_keystroke_at, std::time::Instant::now())
            && self.developer_tools_install_authorized(revision)
            && self
                .system_state_handlers()
                .is_some_and(|handlers| (handlers.own_console)())
            && !self.developer_tools_tab_adopted()
            && !self.access_dialog_up()
            && !self.developer_tools_install_blocked()
            && self.reserve_access_dialog()
        {
            if self.workstation_setup.developer_tools_mark.claim() {
                return true;
            }
            self.workstation_setup.developer_tools = Install::Wanted(revision);
            self.developer_tools_asked_elsewhere();
            return false;
        }
        let kept = wanted.filter(|_| missing);
        self.workstation_setup.developer_tools = kept.map_or(Install::Unasked, Install::Wanted);
        if kept.is_some() && self.access_dialog_up() {
            // Deferred behind another macOS dialog: its end wakes the window to look again.
            self.wake_when_no_access_dialog();
        }
        aterm_log::info!(
            "System State: Command Line Tools install {}",
            if kept.is_some() {
                "deferred"
            } else if missing {
                "withdrawn"
            } else {
                "withdrawn: the tools are installed, and nothing else is owed"
            }
        );
        false
    }

    /// The account's record holds the automatic ask though this launch's reading did not:
    /// another asker asked meanwhile (the follow-up of a package install or update, or another
    /// window) or ended an attempt, or the landing's claim failed. No tab opens by itself until
    /// the record lets one go ([`Mark::held`]), and Install… is the way on meanwhile: the shim's
    /// line says the tools are on their way only while an install of them is under way
    /// ([`Self::developer_tools_on_their_way`]).
    fn developer_tools_asked_elsewhere(&mut self) {
        aterm_log::info!(
            "System State: the account's record holds the Command Line Tools ask now (another \
             asker, an attempt's end, or a claim that failed); no tab opens by itself until it \
             lets one go"
        );
        self.workstation_setup.developer_tools_unfinished = true;
        self.settle_developer_tools_shim();
    }

    /// Whether the automatic tab now tries again after an attempt that did not finish: the
    /// account's record says so ([`Mark::retrying`]), or this launch's own tab already ran.
    fn developer_tools_tries_again(&self) -> bool {
        self.workstation_setup.developer_tools_mark.retrying()
            || self.workstation_setup.developer_tools_attempt.is_some()
    }

    /// A tab may land after Stop or a newer choice. Its old command then stays untyped.
    fn dispatch_developer_tools_install(&mut self, revision: u64, start: impl FnOnce(&mut Self)) {
        if self.developer_tools_install_authorized(revision) {
            start(self);
        }
    }

    /// Open the install's tab if it is due ([`Install::due`]), or a person's queued press once
    /// it may go ([`Install::PressedQueued`]); else it waits for the next status, which comes
    /// each second while a window is up.
    pub(crate) fn tick_developer_tools_install(&mut self) {
        if self.headless {
            return;
        }
        self.open_developer_tools_install(own_aterm);
    }

    /// [`Self::tick_developer_tools_install`]'s opening, `aterm` naming this program's `aterm`:
    /// a queued press's tab is the person's (`by=human`, the press that asked for it), the
    /// automatic one aterm's own (`by=harness`).
    pub(crate) fn open_developer_tools_install(
        &mut self,
        aterm: impl FnOnce() -> Option<std::path::PathBuf>,
    ) {
        let by = if matches!(
            self.workstation_setup.developer_tools,
            Install::PressedQueued { .. }
        ) {
            crate::session_store::ExitActor::Human
        } else {
            crate::session_store::ExitActor::Harness
        };
        let Some((window, ask, then)) = self.begin_developer_tools_install(aterm) else {
            return;
        };
        aterm_log::info!(
            "System State: starting the Command Line Tools install tab (`aterm {}`)",
            ARGS.join(" ")
        );
        let _act = crate::tab_marks::ActScope::enter(by, "install");
        self.open_off_loop(window, crate::tab_spawn::SpawnPlace::Tab, ask, then);
    }

    /// When the install is due, the window its tab opens in, how it spawns and what it does once
    /// it lands (running this program's `aterm`, read only then), with the request now Opening;
    /// none once the account's [`Mark`], read again then, says another asker asked or ended an
    /// attempt meanwhile ([`Mark::asked_since`]; the landing claims the ask, [`install_tab_then`]).
    /// A program that cannot name its `aterm` spends the launch's attempt and opens nothing. A
    /// person's queued press goes first, once nothing holds it, as [`Self::begin_pressed_install`]
    /// starts it; tools that arrived meanwhile let it go.
    fn begin_developer_tools_install(
        &mut self,
        aterm: impl FnOnce() -> Option<std::path::PathBuf>,
    ) -> Option<(
        crate::WindowId,
        crate::tab_spawn::SpawnAsk,
        crate::tab_spawn::TabSpawnThen,
    )> {
        if let Install::PressedQueued { wanted } = self.workstation_setup.developer_tools {
            // The tools arrived while the press waited: the GitHub repositories row reads again
            // and no longer needs them. A page with no reading of that row yet keeps the press.
            let latest = self.system_state_host.latest_status();
            if latest.repositories_reading().is_some() && latest.repositories_tools_wait().is_none()
            {
                self.workstation_setup.developer_tools =
                    wanted.map_or(Install::Unasked, Install::Wanted);
                self.republish_system_state_developer_tools();
                return None;
            }
            // A queued press is no longer this moment's, and its tab takes the window's keys:
            // it also waits for 2.5 s of quiet keyboard, as the automatic tab does. One behind
            // another macOS dialog is woken at that dialog's end.
            let dialog_up = self.access_dialog_up();
            if dialog_up {
                self.wake_when_no_access_dialog();
            }
            if self.developer_tools_install_active()
                || dialog_up
                || activity(self.developer_tools_known()) != Activity::Idle
                || !crate::hands_off_keys(true, self.last_keystroke_at, std::time::Instant::now())
            {
                return None;
            }
            let window = self
                .windows
                .keys()
                .find(|id| self.system_state_frontmost_in(**id))
                .copied()?;
            self.workstation_setup.developer_tools = Install::Opened;
            let started = self.begin_pressed_install(aterm);
            self.republish_system_state_developer_tools();
            let (ask, then) = started?;
            return Some((window, ask, then));
        }
        // A record another asker held, or that could not be read, is read again once its look is
        // due: that attempt may have ended, or the read error passed.
        self.workstation_setup.developer_tools_mark.look_again();
        let held = self.developer_tools_install_held(self.access_dialog_up());
        if held == Some(DIALOG_UP) {
            // The dialog's end publishes nothing the window reads, and the setup run may be
            // parked on this tab, so no status read comes either: be woken then.
            self.wake_when_no_access_dialog();
        }
        if matches!(self.workstation_setup.developer_tools, Install::Wanted(_))
            && self.workstation_setup.developer_tools_held_said.get() != held
        {
            self.workstation_setup.developer_tools_held_said.set(held);
            if let Some(why) = held {
                aterm_log::info!("System State: the Mac Setup tab waits: {why}");
            }
        }
        if held.is_some() {
            return None;
        }
        // The app has no controlling tty. Its own local graphic console is checked here;
        // the child's full person/tty admission is checked again under the terminal lease.
        if !self
            .system_state_handlers()
            .is_some_and(|handlers| (handlers.own_console)())
        {
            return None;
        }
        // The account's record holds the ask now: another asker (the follow-up of a package
        // install or update) asked or ended meanwhile, so no shell is booked. One that asks after
        // this look loses the landing's claim instead.
        if self.workstation_setup.developer_tools_mark.asked_since() {
            self.developer_tools_asked_elsewhere();
            return None;
        }
        // Whichever window is frontmost, focused or not, whatever it shows.
        let window = self
            .frontmost_window
            .filter(|id| self.windows.contains_key(id))
            .or_else(|| self.windows.keys().next().copied())?;
        let Install::Wanted(revision) = self.workstation_setup.developer_tools else {
            return None;
        };
        self.workstation_setup.developer_tools = Install::Opened;
        let Some(exe) = aterm() else {
            aterm_log::warn!(
                "System State: this program cannot name itself; the Command Line Tools install \
                 opens no tab"
            );
            return None;
        };
        self.workstation_setup.developer_tools = Install::Opening {
            wanted: Some(revision),
        };
        let ask = crate::tab_spawn::SpawnAsk {
            unasked: true,
            // A try again after an attempt that did not finish, by the record or this launch,
            // lands behind the window's active tab: it takes no keys from whatever the person
            // does, and System State's row says where it waits.
            background: self.developer_tools_tries_again(),
            // The program waits for its go line, which only the landing check sends: nothing
            // installs before it, and no shell, rc hook or global `-e` runs in between.
            exec_command: Some(tab_command(&exe, self.developer_mode_is_theirs())),
            ..crate::tab_spawn::SpawnAsk::default()
        };
        Some((window, ask, install_tab_then(revision)))
    }
}

#[cfg(all(test, unix))]
#[path = "clt_install/landing_tests.rs"]
mod landing_tests;

#[cfg(all(test, unix))]
#[path = "clt_install/child_tests.rs"]
mod child_tests;

#[cfg(test)]
#[path = "clt_install_tests.rs"]
mod tests;
