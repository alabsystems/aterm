// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `init.fseventsd`: brings macOS's file-change service back under the memory any service needs
//! ([`aterm_vitals::services`]) by restarting it as root, once a person types their password.
//!
//! fseventsd's launchd job budgets it 30 MB, and a fresh one holds 3.5 to 3.9 MB. On 2026-10-26
//! one up 35 days held 152 GB with swap at 59.4 of 60 GiB. After its SIGTERM it took 14.7 s to
//! end (most of what it held was compressed or swapped out); launchd, which keeps the job alive,
//! spawned the new one within milliseconds of that end, and it held 3.7 MB. Nothing unprivileged
//! ends it. This item repairs; prevention is placement: aterm's build cache, verify's side lanes,
//! release targets and test scratch sit in `.ubd` folders, whose file events fseventsd drops
//! before it delivers or keeps them (`aterm_tempfile::QUIET`, observed on macOS 27.0), so
//! aterm's own churn reaches none of its clients or history. Another program's churn still can,
//! and so can a macOS that stops honouring the name, which is what this repair is for.
//!
//! Only `aterm init --only init.fseventsd` restarts it, from the margin under the large bar
//! the window's watch holds a service by ([`followed_from`]): the restart is destructive and
//! takes the person's password, so a plain `aterm init` (which may run in a tab with `sudo`'s
//! ticket still cached, beside other lanes' rows) only names that command. The restart takes the
//! password with `sudo -v` and a prompt that names fseventsd and the cost, reads fseventsd
//! again, and sends SIGTERM to the very process it measured, by pid and start time (never by
//! name). Once the signal went it records the restart ([`Sent`]), so that a second run within
//! [`RECENT`] restarts nothing again, and waits, bounded, for that one to end and launchd's new
//! one to run. A fseventsd that ended on its own between the reading and the signal is no
//! failure: the wait sees launchd's new one.
//!
//! An account-shared lock covers the recent-restart check, authorization, the fresh identity
//! check, the signal and its record. A competing run fails before opening another terminal
//! command rather than waiting behind the first run's password prompt. The lock is released
//! before the read-only exit wait, and its rendezvous stays in place for later runs.

use std::path::PathBuf;
use std::time::Duration;

use crate::items::{Ctx, Item};
use crate::model::Probe;
use crate::port::{Cmd, System};
use aterm_uds::services::{Look, Service};
use aterm_vitals::services::{FSEVENTSD, Size, followed_from, large_bar, size};
use aterm_vitals::words::mem;

/// The item's id, as `aterm init --only` and every remedy that names it spell it.
pub const ID: &str = FSEVENTSD.init;

/// How long the restart waits for the old fseventsd to end and launchd's new one to run: the
/// 152 GB one took 14.7 s to end, so four times that and more.
pub const RESTART_WITHIN: Duration = Duration::from_secs(90);

/// How often that wait looks.
pub const POLL: Duration = Duration::from_millis(250);

/// How long after a restart was sent no run restarts fseventsd again.
pub const RECENT: Duration = Duration::from_mins(5);

/// The file under the account's state root that records the last restart sent ([`Sent`]).
pub const STAMP: &str = "fseventsd-restart";

/// `sudo`'s path.
const SUDO: &str = "/usr/bin/sudo";

/// Why the item cannot restart it from here.
pub const NO_TTY: &str = "restarting it asks for your password, and this run has no terminal \
                          to ask it in: run `aterm init --only init.fseventsd` in a terminal";

/// Why the item cannot restart it for this account.
pub const NOT_ADMIN: &str = "restarting it takes an administrator's password, and this account \
                             is not an administrator of this Mac: an administrator's account \
                             restarts it, as does a restart of the Mac";

/// One restart sent: the process signalled, by pid and start second, and when, unix seconds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sent {
    /// The pid signalled.
    pub pid: u32,
    /// When that process started, unix seconds.
    pub start_s: u64,
    /// When the signal was sent, unix seconds.
    pub at_s: u64,
}

impl Sent {
    /// The record's one line: `<pid> <start second> <sent second>`.
    #[must_use]
    pub fn line(&self) -> String {
        format!("{} {} {}\n", self.pid, self.start_s, self.at_s)
    }

    /// The record `bytes` hold; `None` for anything but one such line.
    #[must_use]
    pub fn parse(bytes: &[u8]) -> Option<Self> {
        let text = std::str::from_utf8(bytes).ok()?;
        let mut words = text.split_whitespace();
        let sent = Self {
            pid: crate::parse_uint(words.next()?)?,
            start_s: crate::parse_uint(words.next()?)?,
            at_s: crate::parse_uint(words.next()?)?,
        };
        words.next().is_none().then_some(sent)
    }
}

/// Where restarts are recorded: [`STAMP`] under this account's state root.
#[must_use]
pub fn stamp_path() -> Option<PathBuf> {
    aterm_types::dirs::account_state_dir().map(|root| root.join(STAMP))
}

/// `init.fseventsd`: restarts fseventsd as root while it holds past its bar, as the port reads
/// it ([`System::service`]), in a run whose `--only` names the item.
pub struct Fseventsd {
    /// Whether this run's `--only` names the item: only such a run restarts fseventsd; a plain
    /// run names the command that does.
    pub named: bool,
    /// Where restarts are recorded ([`stamp_path`]); with none, nothing is restarted.
    pub stamp: Option<PathBuf>,
}

impl Fseventsd {
    /// The item for a run whose `--only` names it (`named`) or not, recording restarts at
    /// [`stamp_path`].
    #[must_use]
    pub fn new(named: bool) -> Self {
        Self {
            named,
            stamp: stamp_path(),
        }
    }

    /// The prompt `sudo` asks the password with: what it restarts, how much it holds, and what
    /// the restart costs (`%u` is sudo's own: the account's name).
    #[must_use]
    pub fn prompt(service: &Service) -> String {
        format!(
            "aterm restarts macOS's fseventsd (pid {}), which holds {}; {}. Password for %u: ",
            service.pid,
            mem(service.footprint),
            FSEVENTSD.cost
        )
    }

    /// `sudo -v` with [`Self::prompt`]: the password, asked once, before anything runs as root.
    #[must_use]
    pub fn validate(service: &Service) -> Cmd {
        Cmd::new(SUDO).args(["-v", "-p", &Self::prompt(service)])
    }

    /// The signal to the one process measured, as root.
    #[must_use]
    pub fn kill(service: &Service) -> Cmd {
        Cmd::new(SUDO).args([
            "-p",
            &Self::prompt(service),
            "/bin/kill",
            "-TERM",
            &service.pid.to_string(),
        ])
    }

    /// Whether this run restarts `service` on a Mac of `total` bytes: `--only` names the item,
    /// the service holds from the margin under the large bar ([`followed_from`]), as the
    /// window's watch keeps a service large, and it is alone in its coalition.
    fn wants(&self, service: &Service, total: u64) -> bool {
        self.named && service.live == 1 && service.footprint >= followed_from(total)
    }

    /// The restart sent within [`RECENT`], as [`Self::stamp`] records it.
    fn recent(&self, sys: &dyn System) -> Option<Sent> {
        let sent = Sent::parse(&sys.read_file(self.stamp.as_deref()?).ok()?)?;
        sys.now()
            .checked_sub(sent.at_s)
            .is_some_and(|age| age < RECENT.as_secs())
            .then_some(sent)
    }

    /// Whether the process `sent` names still runs: the same pid, started at the same second.
    fn still_runs(sys: &dyn System, sent: Sent) -> bool {
        sys.pid_alive(sent.pid) && sys.process_started(sent.pid) == Some(sent.start_s)
    }

    /// What runs since the restart `sent`: the old fseventsd still ending, or the new one.
    fn since(sys: &dyn System, sent: Sent) -> String {
        let age = sys.now().saturating_sub(sent.at_s);
        if Self::still_runs(sys, sent) {
            return format!(
                "restart in progress: the old fseventsd (pid {}) is still ending {age} s after \
                 its TERM, and launchd starts the new one once it has",
                sent.pid
            );
        }
        match sys.service(&FSEVENTSD) {
            Look::Running(s, _) => format!(
                "restarted {age} s ago: the new fseventsd (pid {}) holds {}",
                s.pid,
                mem(s.footprint)
            ),
            Look::NoneRunning => {
                format!("restarted {age} s ago; launchd starts the new one at its next client")
            }
            Look::Unread => format!("restarted {age} s ago"),
        }
    }
}

/// Where `aterm init` places the item: the person's own Mac, as root once.
#[must_use]
pub fn place() -> crate::engine::Place {
    crate::engine::Place {
        layer: aterm_converge::Layer::Workstation,
        class: aterm_converge::Class::Elevate,
        requires: aterm_converge::Requires::Console,
        remedy: aterm_converge::Remedy::Init {
            only: ID.to_owned(),
        },
    }
}

/// What a fseventsd of `footprint` bytes holds on a Mac of `total`, against the bar.
fn holds(footprint: u64, total: u64) -> String {
    let bar = mem(large_bar(total));
    match size(footprint, total) {
        Size::Usual if footprint >= followed_from(total) => format!(
            "fseventsd holds {}, within an eighth under the {bar} bar",
            mem(footprint)
        ),
        Size::Usual => format!("fseventsd holds {}, under the {bar} bar", mem(footprint)),
        Size::Large | Size::Huge => {
            format!("fseventsd holds {}, past the {bar} bar", mem(footprint))
        }
    }
}

impl Item for Fseventsd {
    fn id(&self) -> String {
        ID.to_owned()
    }

    fn family(&self) -> &'static str {
        "machine"
    }

    fn title(&self) -> String {
        "macOS's file-change service (fseventsd) under the memory any service needs".to_owned()
    }

    fn probe(&self, sys: &dyn System, _ctx: &Ctx) -> Probe {
        if !sys.macos() {
            return Probe::NotApplicable {
                why: "fseventsd is macOS's".to_owned(),
            };
        }
        if let Some(sent) = self.recent(sys) {
            return Probe::Ok {
                now: Self::since(sys, sent),
            };
        }
        let (s, total) = match sys.service(&FSEVENTSD) {
            Look::Unread => {
                return Probe::Unusable {
                    why: "fseventsd's footprint could not be read (the process table, its image \
                          or its coalition gave no reading)"
                        .to_owned(),
                };
            }
            Look::NoneRunning => {
                return Probe::Ok {
                    now: "no fseventsd runs (launchd starts one at its next client)".to_owned(),
                };
            }
            Look::Running(s, total) => (s, total),
        };
        let held = holds(s.footprint, total);
        let within = s.footprint >= followed_from(total);
        if size(s.footprint, total) == Size::Usual && !(self.named && within) {
            return Probe::Ok { now: held };
        }
        if s.live != 1 {
            return Probe::Unusable {
                why: format!(
                    "{held}, but its coalition holds {} processes, so that is not fseventsd's \
                     alone",
                    s.live
                ),
            };
        }
        if !self.wants(&s, total) {
            return Probe::Deferred {
                why: format!(
                    "{held}; `aterm init --only {ID}` restarts it now (one password prompt; {}), \
                     which a plain `aterm init` never does",
                    FSEVENTSD.cost
                ),
            };
        }
        if !sys.has_tty() {
            return Probe::Blocked {
                why: format!("{held}; {NO_TTY}"),
            };
        }
        if sys.administrator() == Some(false) {
            return Probe::Blocked {
                why: format!("{held}; {NOT_ADMIN}"),
            };
        }
        Probe::Change {
            from: held,
            to: format!(
                "restarted as root once `sudo` takes your password: `sudo kill -TERM {}`; {}",
                s.pid, FSEVENTSD.cost
            ),
        }
    }

    fn apply(&self, sys: &dyn System, _ctx: &Ctx) -> Result<(), String> {
        if !sys.has_tty() {
            return Err(NO_TTY.to_owned());
        }
        let Some(stamp) = self.stamp.as_deref() else {
            return Err(
                "this account has no state directory to record the restart in, so \
                        nothing was restarted"
                    .to_owned(),
            );
        };
        if let Some(parent) = stamp.parent().filter(|p| !p.as_os_str().is_empty()) {
            sys.create_dir_all(parent, 0o700)
                .map_err(|error| format!("prepare the restart lock: {error}; nothing restarted"))?;
        }
        let restart = sys
            .hold_service_restart(&stamp.with_extension("lock"))
            .map_err(|error| {
                if error.kind() == std::io::ErrorKind::TimedOut {
                    "another fseventsd restart is in progress; this run restarted nothing".into()
                } else {
                    format!("hold the restart lock: {error}; nothing restarted")
                }
            })?;
        let Look::Running(measured, _) = sys.service(&FSEVENTSD) else {
            return Ok(());
        };
        if self.recent(sys).is_some() {
            return Ok(());
        }
        let validate = Self::validate(&measured);
        match sys.run_tty(&validate, &mut |_| Ok(())) {
            Ok(Some(0)) => {}
            Ok(status) => {
                return Err(format!(
                    "`sudo -v` exited {}: it took no password, so nothing was restarted",
                    status.map_or_else(|| "on a signal".to_owned(), |code| code.to_string())
                ));
            }
            Err(e) => return Err(format!("`sudo -v`: {e}; nothing was restarted")),
        }
        // The password took as long as the person did: read again, and signal only the process
        // measured, still past its bar, that no other run restarted meanwhile.
        if self.recent(sys).is_some() {
            return Ok(());
        }
        let Look::Running(now, total) = sys.service(&FSEVENTSD) else {
            return Ok(());
        };
        if (now.pid, now.start_s) != (measured.pid, measured.start_s)
            || !self.wants(&now, total)
            || sys.process_started(now.pid) != Some(now.start_s)
        {
            return Ok(());
        }
        let sent = Sent {
            pid: now.pid,
            start_s: now.start_s,
            at_s: sys.now(),
        };
        let kill = Self::kill(&now);
        let killed = match sys.run_tty(&kill, &mut |_| Ok(())) {
            Ok(Some(0)) => Ok(true),
            // It may have ended on its own after the reading (`kill` finds no such process).
            Ok(_) | Err(_) if !Self::still_runs(sys, sent) => Ok(false),
            Ok(status) => Err(format!(
                "`sudo kill -TERM {}` exited {}",
                now.pid,
                status.map_or_else(|| "on a signal".to_owned(), |code| code.to_string())
            )),
            Err(e) => Err(format!("`sudo kill -TERM {}`: {e}", now.pid)),
        };
        // Recorded only once the signal went, so a run stopped before it leaves no record of a
        // restart that never happened.
        if killed? {
            let recorded = stamp
                .parent()
                .map_or(Ok(()), |dir| sys.create_dir_all(dir, 0o700))
                .and_then(|()| sys.write_file(stamp, sent.line().as_bytes(), 0o600));
            if let Err(e) = recorded {
                aterm_log::warn!(
                    "init.fseventsd: the restart sent to pid {} could not be recorded in {}: {e}",
                    sent.pid,
                    stamp.display()
                );
            }
        }
        drop(restart);
        // The old one ends in its own time (the 152 GB one took 14.7 s), and launchd starts the
        // new one after: wait for both, bounded. The read-back says which it saw ([`Self::since`]).
        let mut waited = Duration::ZERO;
        while waited < RESTART_WITHIN {
            let gone = !Self::still_runs(sys, sent);
            if gone
                && matches!(
                    sys.service(&FSEVENTSD),
                    Look::Running(s, _) if (s.pid, s.start_s) != (sent.pid, sent.start_s)
                )
            {
                break;
            }
            sys.sleep(POLL);
            waited += POLL;
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "init_fseventsd_tests.rs"]
mod tests;
