// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! A BUILD THAT FAILED TO START, AS A FACT ABOUT THE BUILD (handoff §4, round
//! eighteen's upgradebuild lane). A Claude Code relaunch onto a build that
//! never registered the conversation (`no-resume`, [`St::failed_start`]) was
//! the CONVERSATION's failure alone ([`St::keep_stop`]): an agent of any
//! other conversation — a person's hand resume as ANOTHER conversation
//! (`claude` with no `--resume`, a new session id), any other tab's agent —
//! was moved onto that very build at its next quiet moment, ended, and left
//! stopped; and the conversation's own round, its rest run out, was moved
//! onto it again: "a build that still crashes is tried once per rest and
//! each try stops again". Each such move kills a working agent.
//!
//! So the failure is noted against the BUILD too — its version, in its
//! install — by the one writer of every record ([`save_at`]: every stop of
//! either lane is saved through it, so no step can forget it), in
//! `<state>/upgrade/build-starts/<install>-<version>.json` ([`Note`]). Before
//! any agent is moved onto a build with a failure noted ([`starts`]: the
//! upgrade's own move, at its act — announce, restart, fresh restart — and a
//! relaunch on exit, as a target it may take), the build is PROVED TO START
//! WITHOUT AN AGENT: `<build> --help` ([`probe`]), bounded as a
//! `--version` read is ([`VERSION_WAIT`]) and run through
//! `aterm_uds::spawnfd::command`. `--version` is no proof: Claude Code
//! answers it before it loads its program (the lab's crashing stand-in, and
//! a build broken past its entry point, answer it and exit at any other
//! launch), while `--help` builds its whole command line; MEASURED on 2.1.300
//! (2026-10-15): exit 0 in 0.09 s, no file written in a scratch `HOME` or
//! the directory it ran in, with the network denied. Until a probe run since
//! the build's latest failure passes, no agent is moved onto it (the move
//! waits `wait:build-fails`, [`BUILD_FAILS`]); a failing probe is run again
//! no sooner than [`PROBE_GAP_S`] after the last.
//!
//! A PROBE THAT PASSED AND A MOVE THAT STILL FAILED: the probe does not see
//! what fails (a crash in the interface, a resume the build refuses), so it
//! proves nothing more of that build ([`Note::blind`]) and no agent is moved
//! onto it again — a newer build is the next target — save on the owner's
//! own word for one agent (`--now`), which probes it once more for that move
//! ([`starts_with`]). The owner is told ONCE per build (`upgrade_status`'s
//! `build-fails` fault, keyed by the build), never by a stop per agent the
//! build was retried into.
//!
//! A STOP TAKEN BACK IS NO FAILURE ([`note`]): a relaunch stopped
//! `no-resume` whose agent came up after all (a start slower than the stale
//! bound, a start-up screen a person answered) is reopened as landed
//! ([`St::reopen_landed`]), and its failure is taken back from its build —
//! the blindness it caused with it.
//!
//! WHAT AN AGENT THE BUILD'S HOLD MEETS IS TOLD (`upgrade::build_hold`): a
//! round that asked its agent is stopped and the agent released at once; a
//! round that asked nothing waits [`BUILD_FAILS`], a release it owes typed
//! there. Nothing waits on a restart the hold keeps from coming.

use std::path::{Path, PathBuf};
use std::time::Duration;

use aterm_json::{Map, Value};

use super::{FailedStart, Opts, Report, St, VERSION_WAIT, ledger, said, state_dir};
use crate::harness::upgrade::{Candidate, Phase};

/// The wait a move onto a build that failed to start, not proved to start
/// since, records (`wait:build-fails`): the reducer's own word
/// ([`crate::harness::upgrade::build_hold`]).
pub use crate::harness::upgrade::BUILD_FAILS;

/// How long after a probe that did not pass the same build is probed again.
/// The build is a file that does not change, so this is for a failure of the
/// moment (a probe out of time on a loaded machine), never a retry into an
/// agent.
pub const PROBE_GAP_S: u64 = 15 * 60;

/// How many failures one note remembers by their record ([`Note::stops`]),
/// newest last: enough to tell a record saved again from a new failure.
const STOPS_KEPT: usize = 16;

/// The argument a start probe passes: what loads the build's program
/// without starting a session (the module's docs).
const PROBE_ARG: &str = "--help";

/// WHAT IS KNOWN OF ONE BUILD'S START: the failures noted against it, the
/// probes run since, and whether a probe can still prove anything of it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Note {
    /// The build's version, as its stop's record names it.
    pub version: String,
    /// Its install ([`crate::harness::upgrade::Source`]'s word).
    pub source: String,
    /// How many failures to start were noted.
    pub failures: u32,
    /// When the latest was noted (unix seconds).
    pub failed_at: u64,
    /// The failures noted, as `<conversation> <its stop's failed_at>`,
    /// newest last: a record saved again is no new failure.
    pub stops: Vec<String>,
    /// [`Note::failures`] when a probe last ran, and when (`0`: none).
    pub probed_for: u32,
    /// When the last probe ran (unix seconds; `0`: none).
    pub probed_at: u64,
    /// What it found ([`Probe::word`]).
    pub probe: String,
    /// [`Note::failures`] when a probe last passed (`0`: none passed), and
    /// when.
    pub proved_for: u32,
    /// When a probe last passed (unix seconds; `0`: never).
    pub proved_at: u64,
    /// A move onto the build failed after a probe had proved it to start: no
    /// probe proves anything of it any more.
    pub blind: bool,
    /// The failure that made it [`Note::blind`] (one of [`Note::stops`]): a
    /// stop taken back takes the blindness it caused with it.
    pub blinded_by: String,
}

impl Note {
    fn to_json(&self) -> String {
        let mut o = Map::new();
        o.insert("version".into(), Value::from(self.version.as_str()));
        o.insert("source".into(), Value::from(self.source.as_str()));
        o.insert("failures".into(), Value::from(self.failures));
        o.insert("failedAt".into(), Value::from(self.failed_at));
        o.insert(
            "stops".into(),
            Value::Array(self.stops.iter().map(|s| Value::from(s.as_str())).collect()),
        );
        o.insert("probedFor".into(), Value::from(self.probed_for));
        o.insert("probedAt".into(), Value::from(self.probed_at));
        o.insert("probe".into(), Value::from(self.probe.as_str()));
        o.insert("provedFor".into(), Value::from(self.proved_for));
        o.insert("provedAt".into(), Value::from(self.proved_at));
        o.insert("blind".into(), Value::from(self.blind));
        o.insert("blindedBy".into(), Value::from(self.blinded_by.as_str()));
        aterm_json::to_string(&Value::Object(o)).unwrap_or_default()
    }

    fn from_json(text: &str) -> Option<Note> {
        let v: Value = aterm_json::from_str(text).ok()?;
        let s = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_owned);
        let n = |k: &str| v.get(k).and_then(Value::as_u64).unwrap_or(0);
        let count = |k: &str| u32::try_from(n(k)).unwrap_or(u32::MAX);
        Some(Note {
            version: s("version")?,
            source: s("source")?,
            failures: count("failures"),
            failed_at: n("failedAt"),
            stops: v
                .get("stops")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default(),
            probed_for: count("probedFor"),
            probed_at: n("probedAt"),
            probe: s("probe").unwrap_or_default(),
            proved_for: count("provedFor"),
            proved_at: n("provedAt"),
            blind: v.get("blind").and_then(Value::as_bool).unwrap_or(false),
            blinded_by: s("blindedBy").unwrap_or_default(),
        })
    }

    /// Whether a probe run since the latest failure passed.
    fn proved(&self) -> bool {
        self.proved_for != 0 && self.proved_for == self.failures
    }

    /// Whether a probe run since the latest failure did not pass, within
    /// [`PROBE_GAP_S`] of `now`.
    fn refused_lately(&self, now: u64) -> bool {
        self.probed_for == self.failures
            && self.probed_at != 0
            && now < self.probed_at.saturating_add(PROBE_GAP_S)
    }
}

/// Where the note of build `version` in install `source` lives; `None` for a
/// word that is no plain name (nothing outside the directory is named).
fn path(opts: &Opts, source: &str, version: &str) -> Option<PathBuf> {
    let plain = |w: &str| {
        !w.is_empty()
            && w.bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'+' | b'_'))
            && !w.starts_with('.')
    };
    (plain(source) && plain(version)).then(|| {
        state_dir(opts)
            .join("build-starts")
            .join(format!("{source}-{version}.json"))
    })
}

/// The note of build `version` in install `source`, if one was written.
pub fn load(opts: &Opts, source: &str, version: &str) -> Option<Note> {
    Note::from_json(&std::fs::read_to_string(path(opts, source, version)?).ok()?)
}

/// Writes `note` (a dry run writes nothing).
fn write(opts: &Opts, note: &Note) {
    if opts.dry_run {
        return;
    }
    let Some(path) = path(opts, &note.source, &note.version) else {
        return;
    };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let tmp = path.with_extension("json.tmp");
    if std::fs::write(&tmp, note.to_json()).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
}

/// WHAT `session`'s RECORD, SAVED AT `now` OVER `before` (the record on
/// disk), SAYS OF ITS BUILD'S START — by [`save_at`], the one writer every
/// stop and every reopening is saved through: the failure it carries
/// ([`note_failure`]), or a stop it takes back ([`taken_back`]).
pub fn note(opts: &Opts, session: &str, before: Option<&St>, st: &St, now: u64) {
    if let Some(failed) = before.and_then(|before| taken_back(before, st)) {
        retract(opts, session, &failed, st);
    }
    note_failure(opts, session, st, now);
}

/// The failure `before` noted that `st` TAKES BACK: a stop `no-resume` of
/// its own ([`St::failed_start`], not one it keeps) reopened as the relaunch
/// that landed after all ([`St::reopen_landed`]: the one step from that stop
/// to [`Phase::Relaunched`]) — the new process held the conversation, so the
/// build did start. A re-armed round (pending) still carries the failure:
/// it rested on it.
fn taken_back(before: &St, st: &St) -> Option<FailedStart> {
    let failed = before.failed_start()?;
    let own = matches!(&before.phase, Phase::Failed(why) if why == "no-resume");
    (own && matches!(st.phase, Phase::Relaunched { .. }) && st.to == failed.to).then_some(failed)
}

/// [`taken_back`]'s failure, `session`'s, retracted from its build's note
/// as `st` saves it: no longer counted, and the blindness it caused lifted
/// ([`Note::blinded_by`]); a probe that proved the build since every failure
/// noted proves it since every one left. Said once on the ledger
/// (`build-probe:taken-back`).
fn retract(opts: &Opts, session: &str, failed: &FailedStart, st: &St) {
    let Some(mut note) = load(opts, &failed.source, &failed.to) else {
        return;
    };
    let stop = format!("{session} {}", failed.failed_at);
    let Some(at) = note.stops.iter().position(|s| *s == stop) else {
        return;
    };
    note.stops.remove(at);
    let was = note.failures;
    note.failures = was.saturating_sub(1);
    for count in [&mut note.probed_for, &mut note.proved_for] {
        if *count >= was {
            *count = note.failures;
        }
    }
    let unblinded = note.blind && note.blinded_by == stop;
    if unblinded {
        note.blind = false;
        note.blinded_by.clear();
    }
    write(opts, &note);
    let row = Report {
        pid: st.pid,
        tab: st.tab.clone(),
        session: session.to_string(),
        from: st.from.clone(),
        to: format!("{}({})", note.version, note.source),
        step: "build-probe:taken-back".to_string(),
    };
    ledger(
        opts,
        &row,
        &format!(
            "the relaunch stopped as never picked up came up after all: its stop is no failure \
             of the build to start, and is taken back{}",
            if unblinded {
                " — with the blindness it caused: a probe proves the build again"
            } else {
                ""
            }
        ),
    );
}

/// THE FAILURE A RECORD SAVED AT `now` CARRIES, NOTED AGAINST ITS BUILD
/// ([`St::failed_start`]): `session`'s record, saved by [`save_at`], the one
/// writer. Once per failure ([`Note::stops`]): a record saved again, a new
/// process of the conversation that keeps the stop ([`St::keep_stop`]) and a
/// restart's record that carries it ([`St::kept_by_restart`]) are the same
/// failure. A failure of a move made after a probe had proved the build to
/// start ([`St::relaunched_at`] at or past [`Note::proved_at`]) makes the
/// note [`Note::blind`].
fn note_failure(opts: &Opts, session: &str, st: &St, now: u64) {
    let Some(failed) = st.failed_start() else {
        return;
    };
    let (version, source) = (failed.to.as_str(), failed.source.as_str());
    let stop = format!("{session} {}", failed.failed_at);
    let mut note = load(opts, source, version).unwrap_or_else(|| Note {
        version: version.to_string(),
        source: source.to_string(),
        ..Note::default()
    });
    if note.stops.contains(&stop) {
        return;
    }
    let moved_at = if st.relaunched_at == 0 {
        now
    } else {
        st.relaunched_at
    };
    let blinded = note.proved() && moved_at >= note.proved_at;
    if blinded {
        note.blind = true;
        note.blinded_by.clone_from(&stop);
        let row = Report {
            pid: st.pid,
            tab: st.tab.clone(),
            session: session.to_string(),
            from: st.from.clone(),
            to: format!("{version}({source})"),
            step: "build-probe:blind".to_string(),
        };
        ledger(
            opts,
            &row,
            "a move onto the build failed to start after its probe had passed: the probe does \
             not see what fails, so no agent is moved onto this build again (a newer build is \
             the next target)",
        );
    }
    note.failures = note.failures.saturating_add(1);
    note.failed_at = now;
    note.stops.push(stop);
    let over = note.stops.len().saturating_sub(STOPS_KEPT);
    note.stops.drain(..over);
    write(opts, &note);
}

/// WHETHER AN AGENT MAY BE MOVED ONTO `to` AT `now`: `Ok` for a build with
/// no failure to start noted, and for one a probe run since its latest
/// failure proved to start; else the probe is run now (no sooner than
/// [`PROBE_GAP_S`] after one that did not pass), its answer recorded and
/// said once on the ledger (`build-probe:<what>`, as row `r`), and
/// `Err(`[`BUILD_FAILS`]`)` where it did not pass — or where a move after a
/// passing probe failed all the same ([`Note::blind`]), which no probe
/// proves again for any agent but one whose move the owner asked for
/// (`owner_now`, their `--now`): that move is probed once more, and taken
/// if the probe passes. A dry run runs no build and records nothing: what
/// is not proved is a wait.
pub fn starts(
    opts: &Opts,
    r: &Report,
    to: &Candidate,
    now: u64,
    owner_now: bool,
) -> Result<(), &'static str> {
    starts_with(opts, r, to, now, owner_now, probe)
}

/// [`starts`] over the probe `run`.
pub fn starts_with(
    opts: &Opts,
    r: &Report,
    to: &Candidate,
    now: u64,
    owner_now: bool,
    run: impl FnOnce(&Path) -> Probe,
) -> Result<(), &'static str> {
    let version = to.version.to_string();
    let Some(mut note) = load(opts, to.source.as_str(), &version) else {
        return Ok(());
    };
    // Every failure noted was taken back.
    if note.failures == 0 {
        return Ok(());
    }
    if note.blind && !owner_now {
        return Err(BUILD_FAILS);
    }
    if note.proved() {
        return Ok(());
    }
    if note.refused_lately(now) || opts.dry_run {
        return Err(BUILD_FAILS);
    }
    let found = run(&to.exe);
    note.probed_for = note.failures;
    note.probed_at = now;
    found.word().clone_into(&mut note.probe);
    let passed = found == Probe::Starts;
    if passed {
        note.proved_for = note.failures;
        note.proved_at = now;
    }
    write(opts, &note);
    let row = Report {
        to: format!("{version}({})", to.source.as_str()),
        ..said(r.clone(), format!("build-probe:{}", found.word()))
    };
    let failed = match note.failures {
        1 => "once".to_string(),
        n => format!("{n} times"),
    };
    let detail = if passed && note.blind {
        format!(
            "`{} {PROBE_ARG}` ran and exited 0 on the owner's --now: the build a move failed on \
             after its probe had passed ({failed} in all) is moved onto for this agent alone; \
             for every other it stays one no probe proves",
            to.exe.display()
        )
    } else if passed {
        format!(
            "`{} {PROBE_ARG}` ran and exited 0: the build that failed to start ({failed}) is \
             proved to start without an agent, so agents move onto it again",
            to.exe.display()
        )
    } else {
        format!(
            "`{} {PROBE_ARG}` {}: the build failed to start ({failed}) and is not proved to \
             start, so no agent is moved onto it (probed again in {} at the earliest)",
            to.exe.display(),
            found.words(),
            crate::harness::upgrade::span(PROBE_GAP_S)
        )
    };
    ledger(opts, &row, &detail);
    if passed { Ok(()) } else { Err(BUILD_FAILS) }
}

/// What one start probe found ([`probe`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Probe {
    /// It ran and exited 0.
    Starts,
    /// It ran and exited otherwise (or by a signal).
    Exited,
    /// It could not be run at all.
    NoExec,
    /// It did not end within the bound; its process group was killed.
    TimedOut,
}

impl Probe {
    /// One word for the note and the ledger.
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            Probe::Starts => "starts",
            Probe::Exited => "exited",
            Probe::NoExec => "no-exec",
            Probe::TimedOut => "timed-out",
        }
    }

    /// What it found, in a sentence's words.
    fn words(self) -> &'static str {
        match self {
            Probe::Starts => "ran and exited 0",
            Probe::Exited => "exited with a failure",
            Probe::NoExec => "could not be run",
            Probe::TimedOut => "did not end in time",
        }
    }
}

/// `<exe> --help` ([`PROBE_ARG`]), bounded by [`VERSION_WAIT`] — the child
/// in its own process group, killed whole at the bound, its output
/// discarded — through `aterm_uds::spawnfd::command`, which strips inherited
/// descriptors and keeps a console window off screen on Windows.
#[must_use]
pub fn probe(exe: &Path) -> Probe {
    probe_within(exe, VERSION_WAIT)
}

/// [`probe`] with an explicit bound.
pub(super) fn probe_within(exe: &Path, limit: Duration) -> Probe {
    let deadline = std::time::Instant::now() + limit;
    let mut cmd = aterm_uds::spawnfd::command(exe);
    cmd.arg(PROBE_ARG)
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .stdout(aterm_uds::spawnfd::Stdio::null())
        .stderr(aterm_uds::spawnfd::Stdio::null());
    #[cfg(unix)]
    cmd.process_group(0);
    let Ok(mut child) = cmd.spawn() else {
        return Probe::NoExec;
    };
    #[cfg(unix)]
    let group = libc::pid_t::try_from(child.id()).ok();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                return if status.success() {
                    Probe::Starts
                } else {
                    Probe::Exited
                };
            }
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            _ => {
                #[cfg(unix)]
                if let Some(group) = group {
                    // SAFETY: a signal to the process group this spawn made
                    // (`process_group(0)`: its pgid is the child's pid), sent
                    // before the child is reaped, so the id is still its.
                    unsafe { libc::kill(-group, libc::SIGKILL) };
                }
                let _ = child.kill();
                let _ = child.wait();
                return Probe::TimedOut;
            }
        }
    }
}
