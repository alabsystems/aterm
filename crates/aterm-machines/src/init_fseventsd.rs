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
            pid: words.next()?.parse().ok()?,
            start_s: words.next()?.parse().ok()?,
            at_s: words.next()?.parse().ok()?,
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
        (sys.now().saturating_sub(sent.at_s) < RECENT.as_secs()).then_some(sent)
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
        match sys.run_tty(&validate) {
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
        let killed = match sys.run_tty(&kill) {
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
mod tests {
    use super::*;
    use crate::engine::bind;
    use crate::items::defaults::tests::ctx;
    use crate::model::{Mode, Outcome};
    use crate::port::{Fake, Ran};
    use aterm_converge::{Class, Layer, NoJournal, Scope};

    const GIB: u64 = 1 << 30;
    const M3: u64 = 128 * GIB;
    const STARTED: u64 = 1_790_000_000;
    const NOW: u64 = 1_790_400_000;
    const STAMPED: &str = "/state/fseventsd-restart";

    fn service(pid: u32, footprint: u64) -> Service {
        Service {
            coalition: 7,
            pid,
            start_s: STARTED + u64::from(pid),
            start_us: 0,
            uid: 0,
            name: "fseventsd".into(),
            path: Some(FSEVENTSD.path.into()),
            live: 1,
            footprint,
        }
    }

    fn running(pid: u32, footprint: u64) -> Look {
        Look::Running(service(pid, footprint), M3)
    }

    fn item(named: bool) -> Fseventsd {
        Fseventsd {
            named,
            stamp: Some(STAMPED.into()),
        }
    }

    fn validate(pid: u32, footprint: u64) -> String {
        format!(
            "tty {}",
            Fseventsd::validate(&service(pid, footprint)).display()
        )
    }

    fn kill(pid: u32, footprint: u64) -> String {
        format!(
            "tty {}",
            Fseventsd::kill(&service(pid, footprint)).display()
        )
    }

    /// A Mac with a person at its terminal, at [`NOW`], where fseventsd 537 runs.
    fn mac() -> Fake {
        let mut sys = Fake::default();
        sys.tty = true;
        sys.now = NOW;
        sys.alive(537).born(537, STARTED + 537);
        sys
    }

    /// One run of `item` on `sys`, whose looks answer `looks` in turn.
    fn run(
        item: &Fseventsd,
        sys: &Fake,
        looks: Vec<Look>,
        mode: Mode,
    ) -> aterm_converge::ItemReport {
        sys.services(looks);
        let ctx = ctx(&[]);
        let bound = bind([(item as &dyn Item, place())], sys, &ctx);
        let mut rows =
            aterm_converge::run_scoped(&bound, mode, Scope::EVERY, &mut NoJournal, &mut |_| {})
                .unwrap();
        rows.remove(0)
    }

    /// The commands a run sent to the terminal, its record's writes and removals.
    fn acts(sys: &Fake) -> Vec<String> {
        sys.calls()
            .into_iter()
            .filter(|c| {
                c.starts_with("tty ") || c.starts_with("write ") || c.starts_with("remove ")
            })
            .collect()
    }

    /// Named by `--only`, a plan names the one root command and runs nothing; the apply asks
    /// the password with a prompt naming fseventsd and its cost, reads it again, signals the
    /// very process measured and only then records the restart, and the new one reads back.
    #[test]
    fn past_the_bar_the_restart_asks_once_and_signals_the_process_measured() {
        let sys = mac();
        let row = run(&item(true), &sys, vec![running(537, 141 * GIB)], Mode::Plan);
        assert_eq!(row.outcome, Outcome::Planned, "{row:?}");
        assert_eq!(
            row.from.as_deref(),
            Some("fseventsd holds 141.0 GiB, past the 8.0 GiB bar")
        );
        assert!(
            row.to.as_deref().is_some_and(|to| to.starts_with(
                "restarted as root once `sudo` takes your password: `sudo kill -TERM 537`; it \
                 discards the disk's file-change history"
            )),
            "{row:?}"
        );
        assert_eq!(row.layer, Some(Layer::Workstation));
        assert_eq!(row.class, Some(Class::Elevate));
        assert!(
            acts(&sys).is_empty(),
            "a plan runs nothing: {:?}",
            sys.calls()
        );

        let prompt = Fseventsd::prompt(&service(537, 141 * GIB));
        assert!(
            prompt.starts_with(
                "aterm restarts macOS's fseventsd (pid 537), which holds 141.0 GiB; it discards"
            ) && prompt.ends_with("Password for %u: "),
            "{prompt}"
        );
        sys.on(&validate(537, 141 * GIB), Ran::ok(""));
        sys.on(&kill(537, 141 * GIB), Ran::ok(""));
        let slept = std::sync::atomic::AtomicUsize::new(0);
        sys.on_sleep(move |sys, _| {
            if slept.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 3 {
                sys.exited(537);
                sys.services(vec![running(90_001, 3_700_000)]);
            }
        });
        let row = run(
            &item(true),
            &sys,
            vec![running(537, 141 * GIB)],
            Mode::Apply,
        );
        assert_eq!(row.outcome, Outcome::Changed, "{row:?} {:?}", sys.calls());
        assert_eq!(
            row.to.as_deref(),
            Some("restarted 0 s ago: the new fseventsd (pid 90001) holds 4 MiB")
        );
        assert_eq!(
            sys.calls()
                .iter()
                .filter(|c| c.starts_with("sleep "))
                .count(),
            4,
            "the wait ends once the old one has gone and the new one runs"
        );
        assert_eq!(
            acts(&sys),
            [
                validate(537, 141 * GIB),
                kill(537, 141 * GIB),
                format!("write {STAMPED} 600"),
            ],
            "the password once, the signal to pid 537 alone, then its record"
        );
        assert_eq!(
            Sent::parse(&sys.read_file(std::path::Path::new(STAMPED)).unwrap()),
            Some(Sent {
                pid: 537,
                start_s: STARTED + 537,
                at_s: NOW
            })
        );
    }

    /// A 152 GB fseventsd takes its time to end (14.7 s on 2026-10-26): the wait is bounded at
    /// a minute and more, and one still ending after it reads back as a restart in progress,
    /// never a failure. A second run within minutes of it restarts nothing and asks nothing.
    /// NEGATIVE CONTROL: once [`RECENT`] has passed, a run past the bar asks again.
    #[test]
    fn a_slow_exit_is_a_restart_in_progress_and_no_second_restart() {
        assert!(RESTART_WITHIN >= Duration::from_mins(1));
        let sys = mac();
        sys.on(&validate(537, 141 * GIB), Ran::ok(""));
        sys.on(&kill(537, 141 * GIB), Ran::ok(""));
        let row = run(
            &item(true),
            &sys,
            vec![running(537, 141 * GIB)],
            Mode::Apply,
        );
        assert_eq!(row.outcome, Outcome::Changed, "{row:?}");
        assert!(
            row.to.as_deref().is_some_and(|to| to
                .starts_with("restart in progress: the old fseventsd (pid 537) is still ending")),
            "{row:?}"
        );
        let sleeps = sys
            .calls()
            .iter()
            .filter(|c| c.starts_with("sleep "))
            .count();
        let bound = (RESTART_WITHIN.as_millis() / POLL.as_millis()) as usize;
        assert_eq!(sleeps, bound, "the whole bound, then no more");

        let before = acts(&sys).len();
        for named in [false, true] {
            let row = run(
                &item(named),
                &sys,
                vec![running(537, 141 * GIB)],
                Mode::Apply,
            );
            assert_eq!(row.outcome, Outcome::Ok, "{row:?}");
        }
        assert_eq!(
            acts(&sys).len(),
            before,
            "no second password, no second signal"
        );

        let mut later = mac();
        later.now = NOW + RECENT.as_secs();
        later.file(
            STAMPED,
            Sent {
                pid: 537,
                start_s: STARTED + 537,
                at_s: NOW,
            }
            .line()
            .as_bytes(),
        );
        let row = run(
            &item(true),
            &later,
            vec![running(537, 141 * GIB)],
            Mode::Plan,
        );
        assert_eq!(row.outcome, Outcome::Planned, "{row:?}");
    }

    /// The password can take any time: a fseventsd replaced meanwhile (someone else restarted
    /// it) is not signalled, and nothing is recorded.
    #[test]
    fn a_fseventsd_replaced_while_the_password_was_typed_is_left_alone() {
        let sys = mac();
        sys.on(&validate(537, 141 * GIB), Ran::ok(""));
        let looks = vec![
            running(537, 141 * GIB),
            running(537, 141 * GIB),
            running(90_001, 3_700_000),
        ];
        let row = run(&item(true), &sys, looks, Mode::Apply);
        assert_eq!(row.outcome, Outcome::Changed, "{row:?}");
        assert_eq!(acts(&sys), [validate(537, 141 * GIB)], "no signal");
    }

    /// A fseventsd that ended on its own between the reading and the signal (`kill` finds no
    /// such process) is no failure: the wait sees launchd's new one, and nothing is recorded,
    /// since this run sent nothing. NEGATIVE CONTROL: a `kill` that fails while the process
    /// still runs fails the row, with nothing recorded.
    #[test]
    fn a_fseventsd_that_ended_before_its_signal_is_no_failure() {
        let sys = mac();
        sys.on(&validate(537, 141 * GIB), Ran::ok(""));
        sys.on(
            &kill(537, 141 * GIB),
            Ran::exit(1, "kill: 537: No such process"),
        );
        sys.exited(537);
        let looks = vec![
            running(537, 141 * GIB),
            running(537, 141 * GIB),
            running(537, 141 * GIB),
            running(90_001, 3_700_000),
        ];
        let row = run(&item(true), &sys, looks, Mode::Apply);
        assert_eq!(row.outcome, Outcome::Changed, "{row:?} {:?}", sys.calls());
        assert_eq!(
            acts(&sys),
            [validate(537, 141 * GIB), kill(537, 141 * GIB)],
            "nothing recorded"
        );

        let sys = mac();
        sys.on(&validate(537, 141 * GIB), Ran::ok(""));
        sys.on(
            &kill(537, 141 * GIB),
            Ran::exit(1, "Operation not permitted"),
        );
        let row = run(
            &item(true),
            &sys,
            vec![running(537, 141 * GIB)],
            Mode::Apply,
        );
        assert_eq!(row.outcome, Outcome::Failed, "{row:?}");
        assert_eq!(acts(&sys), [validate(537, 141 * GIB), kill(537, 141 * GIB)]);
    }

    /// A refused password restarts nothing and records nothing.
    #[test]
    fn a_refused_password_restarts_nothing() {
        let sys = mac();
        sys.on(&validate(537, 141 * GIB), Ran::exit(1, "Sorry, try again."));
        let row = run(
            &item(true),
            &sys,
            vec![running(537, 141 * GIB)],
            Mode::Apply,
        );
        assert_eq!(row.outcome, Outcome::Failed, "{row:?}");
        assert!(
            row.why
                .as_deref()
                .is_some_and(|w| w.contains("nothing was restarted")),
            "{row:?}"
        );
        assert_eq!(acts(&sys), [validate(537, 141 * GIB)]);
    }

    /// A plain `aterm init` never asks for a password, past either bar: it names the command
    /// that restarts it now. Named by `--only`, the same run asks from the margin under the
    /// large bar, where the window's watch still holds the service large. NEGATIVE CONTROL:
    /// under that margin a named run asks nothing.
    #[test]
    fn only_a_run_that_names_the_item_asks() {
        let sys = mac();
        for footprint in [20 * GIB, 141 * GIB] {
            let row = run(
                &item(false),
                &sys,
                vec![running(537, footprint)],
                Mode::Apply,
            );
            assert_eq!(row.outcome, Outcome::Deferred, "{row:?}");
            assert!(
                row.explain.as_ref().is_some_and(|e| e
                    .why
                    .contains("`aterm init --only init.fseventsd` restarts it now")),
                "{row:?}"
            );
        }
        assert!(acts(&sys).is_empty(), "{:?}", sys.calls());
        let row = run(&item(true), &sys, vec![running(537, 20 * GIB)], Mode::Plan);
        assert_eq!(row.outcome, Outcome::Planned, "{row:?}");
        let row = run(
            &item(true),
            &sys,
            vec![running(537, 7 * GIB + (512 << 20))],
            Mode::Plan,
        );
        assert_eq!(row.outcome, Outcome::Planned, "{row:?}");
        assert_eq!(
            row.from.as_deref(),
            Some("fseventsd holds 7.5 GiB, within an eighth under the 8.0 GiB bar")
        );
        let row = run(
            &item(true),
            &sys,
            vec![running(537, 7 * GIB - (1 << 20))],
            Mode::Plan,
        );
        assert_eq!(row.outcome, Outcome::Ok, "{row:?}");
    }

    /// Under the bar, with none running, unread, off macOS, for an account that is not an
    /// administrator, or with fseventsd sharing its coalition (its footprint not its own),
    /// nothing runs and nothing asks.
    #[test]
    fn under_the_bar_or_unsure_nothing_asks() {
        let mut sys = mac();
        let mut shared = service(537, 141 * GIB);
        shared.live = 3;
        for (look, outcome) in [
            (running(537, 3_700_000), Outcome::Ok),
            (running(537, 6 * GIB), Outcome::Ok),
            (Look::NoneRunning, Outcome::Ok),
            (Look::Unread, Outcome::Unknown),
            (Look::Running(shared, M3), Outcome::Unknown),
        ] {
            let row = run(&item(true), &sys, vec![look.clone()], Mode::Apply);
            assert_eq!(row.outcome, outcome, "{look:?}: {row:?}");
        }
        sys.admin = Some(false);
        let row = run(
            &item(true),
            &sys,
            vec![running(537, 141 * GIB)],
            Mode::Apply,
        );
        assert_eq!(row.outcome, Outcome::Blocked, "{row:?}");
        sys.admin = None;
        sys.linux = true;
        let row = run(
            &item(true),
            &sys,
            vec![running(537, 141 * GIB)],
            Mode::Apply,
        );
        assert_eq!(row.outcome, Outcome::NotApplicable, "{row:?}");
        assert!(acts(&sys).is_empty(), "{:?}", sys.calls());
    }

    #[test]
    fn without_a_terminal_the_item_is_blocked_on_init_only() {
        let mut sys = mac();
        sys.tty = false;
        for mode in [Mode::Plan, Mode::Apply] {
            let row = run(&item(true), &sys, vec![running(537, 141 * GIB)], mode);
            assert_eq!(row.outcome, Outcome::Blocked, "{mode:?}: {row:?}");
            assert!(
                row.why
                    .as_deref()
                    .is_some_and(|why| why.contains("`aterm init --only init.fseventsd`")),
                "{row:?}"
            );
        }
        assert!(acts(&sys).is_empty(), "{:?}", sys.calls());
    }

    #[test]
    fn competing_restarts_cannot_authorize_or_signal_beside_the_first() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::{Arc, Mutex, mpsc};
        let dir = aterm_tempfile::tempdir().unwrap();
        let mut sys = mac();
        sys.service_restart_lock = Some(dir.path().join("restart.lock"));
        sys.services(vec![running(537, 141 * GIB)]);
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let release_rx = Arc::new(Mutex::new(release_rx));
        let first = AtomicBool::new(true);
        sys.tty_model(move |sys, cmd| {
            if cmd.args.iter().any(|arg| arg == "-TERM") {
                if first.swap(false, Ordering::SeqCst) {
                    entered_tx.send(()).unwrap();
                    release_rx.lock().unwrap().recv().unwrap();
                }
                sys.exited(537);
                sys.services(vec![running(90_001, 3_700_000)]);
            }
            Some(Ran::ok(""))
        });
        let restart = item(true);
        std::thread::scope(|scope| {
            let sys = &sys;
            let restart = &restart;
            let first = scope.spawn(move || restart.apply(sys, &ctx(&[])));
            let entered = entered_rx.recv_timeout(Duration::from_secs(60));
            let (done_tx, done_rx) = mpsc::channel();
            let second = scope.spawn(move || {
                let result = restart.apply(sys, &ctx(&[]));
                done_tx.send(()).unwrap();
                result
            });
            let done = done_rx.recv_timeout(Duration::from_secs(60));
            // Always release the first run before judging either result, including a mutant
            // that blocks behind its password/signal transaction.
            let _ = release_tx.send(());
            let first = first.join().unwrap();
            let second = second.join().unwrap();
            entered.unwrap();
            done.unwrap();
            first.unwrap();
            assert!(
                second.is_err(),
                "the competing transaction must stand aside"
            );
        });
        let transcript = acts(&sys);
        let commands: Vec<_> = transcript
            .iter()
            .filter(|call| call.starts_with("tty "))
            .cloned()
            .collect();
        assert_eq!(commands, [validate(537, 141 * GIB), kill(537, 141 * GIB)]);
        assert_eq!(
            Sent::parse(&sys.read_file(std::path::Path::new(STAMPED)).unwrap()),
            Some(Sent {
                pid: 537,
                start_s: STARTED + 537,
                at_s: NOW,
            })
        );
        restart.apply(&sys, &ctx(&[])).unwrap();
        assert_eq!(acts(&sys), transcript);
    }

    #[test]
    fn a_lock_failure_opens_no_terminal_and_failed_auth_releases_the_lock() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let mut sys = mac();
        sys.service_restart_lock = Some(dir.path().to_path_buf());
        sys.services(vec![running(537, 141 * GIB)]);
        sys.tty_model(|_, _| Some(Ran::ok("")));
        assert!(item(true).apply(&sys, &ctx(&[])).is_err());
        assert!(acts(&sys).is_empty());
        assert!(sys.read_file(std::path::Path::new(STAMPED)).is_err());

        let lock = dir.path().join("restart.lock");
        let mut refused = mac();
        refused.service_restart_lock = Some(lock.clone());
        refused.services(vec![running(537, 141 * GIB)]);
        refused.tty_model(|_, _| Some(Ran::exit(1, "")));
        assert!(item(true).apply(&refused, &ctx(&[])).is_err());
        assert_eq!(acts(&refused), [validate(537, 141 * GIB)]);
        assert!(refused.read_file(std::path::Path::new(STAMPED)).is_err());
        aterm_update_core::FileLock::acquire_within(&lock, Duration::ZERO).unwrap();
    }

    #[test]
    fn a_record_is_one_line_of_three_numbers() {
        let sent = Sent {
            pid: 537,
            start_s: 1,
            at_s: 2,
        };
        assert_eq!(Sent::parse(sent.line().as_bytes()), Some(sent));
        for junk in ["", "537 1", "537 1 2 3", "x 1 2"] {
            assert_eq!(Sent::parse(junk.as_bytes()), None, "{junk:?}");
        }
    }
}
