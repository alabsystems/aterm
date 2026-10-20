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
//! build was retried into. The one agent moved onto it first is not left
//! down (LC-5 of ATERM-KI-1016-1): a Claude Code restart plans, beside its
//! line onto the build, the line back onto the program its process ran
//! ([`St::roll_back`]), and a relaunch that ends without registering the
//! conversation is stopped `no-resume` (the failure noted as above) and
//! relaunched once on that line, in the same shell, resuming the same
//! conversation on the build it ran.
//!
//! A STOP TAKEN BACK IS NO FAILURE ([`note`]): a relaunch stopped
//! `no-resume` whose agent came up after all (a start slower than the stale
//! bound, a start-up screen a person answered) is reopened as landed
//! ([`St::reopen_landed`]), and its failure is taken back from its build —
//! the blindness it caused with it.
//!
//! A CRASHED AGENT IS MOVED ONLY ONTO A BUILD PROVED TO START ([`proved`],
//! LC-5 of ATERM-KI-1015-1): the relaunch on exit — a crash is also an
//! upgrade (`relaunch::relaunch_target`) — took the newest build the session
//! may move to whether or not anything had ever started on it, and a build
//! presented seconds before the crash, broken at its entry point, left the
//! agent down until its relaunch went stale (`no-resume`, five minutes) and
//! its back-off ran out (MEASURED in the real-Claude lab, 2026-10-15: down
//! 15 minutes). A crash's relaunch is the agent's way back, and is never
//! staked on a build nothing has proved. So the same probe runs before it
//! for a build no probe has passed yet, failure noted or not, and its answer
//! is the build's note like any other ([`Note::proved`]; one that does not
//! pass is not run again within [`PROBE_GAP_S`]). What it changes of the
//! upgrade's own moves is only what it found failing (the review of the r20
//! relaunch lane): a build it found exiting with a failure holds every move
//! off it, as a noted failure does — one it could not run (a build presented
//! mid-install) or that did not end in time (a loaded machine) holds none,
//! and only the crash's relaunch waits out the gap; and a probe that passed
//! before any failure blinds nothing: the build's first move failure is met
//! by the probe after a first failure, as round eighteen meets it
//! ([`Note::blind`] comes only of a failure after a probe passed since a
//! failure).
//!
//! THE UPGRADE'S OWN FIRST MOVE ONTO A BUILD IS PROBED TOO (ATERM-KI-1016-1's
//! LC-5): the move ends a working agent at a quiet moment, and a build broken
//! at its entry point ended that agent before its noted failure held the
//! rest. So a build no probe has run on is probed once before the first move
//! onto it, with the crash's probe's rule for what holds: found exiting, no
//! agent is moved onto it; could not run or did not end, the move goes (that
//! says nothing of the build), and the next move past [`PROBE_GAP_S`] probes
//! again; passed, it is proved, and a later crash's relaunch takes it too.
//! What the probe cannot see (a crash in the interface, a resume the build
//! refuses) is still met by the move, as round eighteen meets it.
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

    /// Whether a probe run since the latest failure passed — with no
    /// failure noted, whether one passed at all ([`proved`]).
    pub fn proved(&self) -> bool {
        self.proved_at != 0 && self.proved_for == self.failures
    }

    /// Whether a probe run since a failure noted passed: a move failure
    /// after it makes the note [`Note::blind`] ([`note_failure`]). A probe
    /// that passed before any failure (a crash's relaunch's, [`proved`])
    /// blinds nothing: the build's first failure is met by the probe after
    /// it, as round eighteen meets it.
    fn proved_since_a_failure(&self) -> bool {
        self.failures != 0 && self.proved()
    }

    /// Whether a probe since the latest failure ran the build and it exited
    /// with a failure — the one finding of a crash's probe that holds the
    /// upgrade's own moves off a build no move has failed on ([`starts_with`]):
    /// one that could not run it, or did not see it end in time, says
    /// nothing of the build.
    fn found_exiting(&self) -> bool {
        self.probed_for == self.failures
            && self.probed_at != 0
            && !self.proved()
            && self.probe == Probe::Exited.word()
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
    let blinded = note.proved_since_a_failure() && moved_at >= note.proved_at;
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
/// no failure to start noted that a probe has not found exiting — probed
/// first where none has run on it (the module's "The upgrade's own first
/// move"), and held only where that probe sees it exit with a failure — and
/// for one a probe run since its latest failure proved to start; else the
/// probe is run now (no sooner than
/// [`PROBE_GAP_S`] after one that did not pass), its answer recorded and
/// said once on the ledger (`build-probe:<what>`, as row `r`), and
/// `Err(`[`BUILD_FAILS`]`)` where it did not pass — or where a move after a
/// passing probe failed all the same ([`Note::blind`]), which no probe
/// proves again for any agent but one whose move the owner asked for
/// (`owner_now`, their `--now`): that move is probed once more, and taken
/// if the probe passes. A dry run runs no build and records nothing: what
/// is not proved is a wait. `moving`: the step asked for moves now (an
/// announcement, a restart, a fresh restart); asked of a round that waits,
/// a build no probe has run on is not probed yet — its first probe comes
/// right before the move it would stake an agent on.
pub fn starts(
    opts: &Opts,
    r: &Report,
    to: &Candidate,
    now: u64,
    owner_now: bool,
    moving: bool,
) -> Result<(), &'static str> {
    gate(
        opts,
        r,
        &Build::of(to),
        now,
        owner_now,
        false,
        moving,
        run_probe,
    )
}

/// WHETHER A CRASHED AGENT'S RELAUNCH MAY MOVE ONTO `to` AT `now`
/// ([`starts`] where the build must be PROVED to start, failure noted or
/// not: the module's "A crashed agent"). `Ok` once a probe since its latest
/// failure — or, none noted, any probe — passed; else the probe runs now
/// (no sooner than [`PROBE_GAP_S`] after one that did not pass), and
/// `Err(`[`BUILD_FAILS`]`)` where it does not pass. A dry run runs nothing:
/// what is not proved is a wait.
pub fn proved(opts: &Opts, r: &Report, to: &Candidate, now: u64) -> Result<(), &'static str> {
    gate(opts, r, &Build::of(to), now, false, true, true, run_probe)
}

/// [`proved`] for a build named by its program `exe` (what the move types),
/// its `version` as its store record spells it and its install `source` —
/// a build whose version the Claude lane's [`Candidate`] cannot hold: Muse
/// Code's revision form (`1.4.11-R6500.1`), which its live upgrade moves onto
/// only once proved (`upgrade_drive`'s `muse` module). Its note is filed
/// under that install and version, as every build's is; a probe that does
/// not end in time (`--help` waits on Muse's auth lock while another process
/// holds it, measured) is no proof: `Err(`[`BUILD_FAILS`]`)`, and nothing is
/// moved onto it until a probe passes, past [`PROBE_GAP_S`].
pub fn proved_named(
    opts: &Opts,
    r: &Report,
    exe: &Path,
    version: &str,
    source: &str,
    now: u64,
) -> Result<(), &'static str> {
    let build = Build {
        exe,
        version: version.to_string(),
        source,
    };
    gate(opts, r, &build, now, false, true, true, run_probe)
}

/// The build a start probe runs and its note is filed under: its program,
/// its version as its record spells it, its install's word.
struct Build<'a> {
    exe: &'a Path,
    version: String,
    source: &'a str,
}

impl<'a> Build<'a> {
    /// The Claude lane's candidate.
    fn of(to: &'a Candidate) -> Build<'a> {
        Build {
            exe: &to.exe,
            version: to.version.to_string(),
            source: to.source.as_str(),
        }
    }
}

/// The probe [`starts`] and [`proved`] run: [`probe`] itself.
#[cfg(not(test))]
fn run_probe(exe: &Path) -> Probe {
    probe(exe)
}

/// A probe a test names in place of [`probe`] ([`PROBE`]).
#[cfg(test)]
pub(crate) type NamedProbe = std::rc::Rc<dyn Fn(&Path) -> Probe>;

#[cfg(test)]
thread_local! {
    /// The probe a test's [`starts`] and [`proved`] run on its thread, where
    /// it names one; unnamed, the real [`probe`]. A test that moves an agent
    /// onto a build that is no program (`/nonexistent/claude`) names what
    /// that build's probe would find.
    pub(crate) static PROBE: std::cell::RefCell<Option<NamedProbe>> =
        const { std::cell::RefCell::new(None) };
}

/// [`run_probe`] in a test: the probe its thread names ([`PROBE`]), else
/// the real one.
#[cfg(test)]
fn run_probe(exe: &Path) -> Probe {
    match PROBE.with(|named| named.borrow().clone()) {
        Some(run) => run(exe),
        None => probe(exe),
    }
}

/// [`starts`] over the probe `run` for a step that moves — with `require`,
/// [`proved`]. The tests' seam: production probes through [`gate`].
#[cfg(all(test, unix))]
pub fn starts_with(
    opts: &Opts,
    r: &Report,
    to: &Candidate,
    now: u64,
    owner_now: bool,
    require: bool,
    run: impl FnOnce(&Path) -> Probe,
) -> Result<(), &'static str> {
    gate(opts, r, &Build::of(to), now, owner_now, require, true, run)
}

/// [`starts_with`], `moving` as [`starts`] takes it.
#[allow(clippy::too_many_arguments)]
fn gate(
    opts: &Opts,
    r: &Report,
    to: &Build<'_>,
    now: u64,
    owner_now: bool,
    require: bool,
    moving: bool,
    run: impl FnOnce(&Path) -> Probe,
) -> Result<(), &'static str> {
    let version = to.version.clone();
    let mut note = load(opts, to.source, &version).unwrap_or_else(|| Note {
        version: version.clone(),
        source: to.source.to_string(),
        ..Note::default()
    });
    // No failure noted (or every one taken back): the upgrade's move needs
    // no proof, but the build is PROBED ONCE before the first agent is staked
    // on it (ATERM-KI-1016-1's LC-5: a build broken at its entry point ended
    // the one working agent its first move ended, before its noted failure
    // held the rest). What that probe finds holds the move only where it ran
    // the build and saw it exit with a failure — the build's fact, for every
    // move, as a crash's probe that found it so; one that could not run it or
    // did not see it end says nothing of the build, holds none, and is run
    // again past [`PROBE_GAP_S`]. A build proved, a dry read (which runs no
    // build), a probe of no finding lately and a round that does not move
    // now (its probe is the move's): the move goes. A crash's relaunch needs
    // the proof.
    let first_probe = note.failures == 0 && !require && !note.found_exiting();
    if first_probe && (!moving || note.proved() || note.refused_lately(now) || opts.dry_run) {
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
    let found = run(to.exe);
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
        to: format!("{version}({})", to.source),
        ..said(r.clone(), format!("build-probe:{}", found.word()))
    };
    let failed = match note.failures {
        1 => "once".to_string(),
        n => format!("{n} times"),
    };
    // The upgrade's own first probe holds its move only by a build it saw
    // exit with a failure.
    let goes = passed || (first_probe && found != Probe::Exited);
    let detail = if note.failures == 0 && passed {
        format!(
            "`{} {PROBE_ARG}` ran and exited 0: the build no agent has started on yet is proved \
             to start without one, so an agent may move onto it",
            to.exe.display()
        )
    } else if first_probe && goes {
        format!(
            "`{} {PROBE_ARG}` {}: that says nothing of the build no agent has started on yet, so \
             the upgrade's move onto it goes ahead",
            to.exe.display(),
            found.words()
        )
    } else if first_probe {
        format!(
            "`{} {PROBE_ARG}` {}: the build no agent has started on yet does not start, so no \
             agent is moved onto it (probed again in {} at the earliest)",
            to.exe.display(),
            found.words(),
            crate::harness::upgrade::span(PROBE_GAP_S)
        )
    } else if note.failures == 0 {
        format!(
            "`{} {PROBE_ARG}` {}: the build no agent has started on yet is not proved to start, \
             so a crashed agent comes back on the build it ran (probed again in {} at the \
             earliest)",
            to.exe.display(),
            found.words(),
            crate::harness::upgrade::span(PROBE_GAP_S)
        )
    } else if passed && note.blind {
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
    if goes { Ok(()) } else { Err(BUILD_FAILS) }
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
