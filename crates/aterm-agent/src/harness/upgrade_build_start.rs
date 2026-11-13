// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Startup failures and later evidence that a build starts.

use std::path::Path;
use std::time::Duration;

use aterm_json::{Map, Value};

use super::{
    FailedStart, Opts, Report, St, VERSION_WAIT, check_upgrade_tree, ensure_upgrade_tree, ledger,
    said,
};
use crate::harness::upgrade::{Agent, Candidate, Phase, SessionFile};

/// The wait of a move onto a build that failed to start and is not proved since ([`crate::harness::upgrade::build_hold`]).
pub use crate::harness::upgrade::BUILD_FAILS;

/// How long after a failed probe the same build is probed again: the build does not change, so this covers a failure of the moment (a probe out of time on a loaded machine).
pub const PROBE_GAP_S: u64 = 15 * 60;

/// How many failures one note remembers by their record ([`Note::stops`]),
/// newest last: enough to tell a record saved again from a new failure.
const STOPS_KEPT: usize = 16;

/// What loads the build's program without starting a session.
const PROBE_ARG: &str = "--help";

/// Startup failures and the later probes or confirmed session starts covering them.
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
    /// The failures noted, as `<conversation> <its stop's failed_at>`, newest last: a record saved again is no new failure.
    pub stops: Vec<String>,
    /// [`Note::failures`] when a probe last ran (`0`: none).
    pub probed_for: u32,
    /// When the last probe ran (unix seconds; `0`: none).
    pub probed_at: u64,
    /// What it found ([`Probe::word`]).
    pub probe: String,
    /// [`Note::failures`] when a probe last passed (`0`: none passed).
    pub proved_for: u32,
    /// When a probe last passed (unix seconds; `0`: never).
    pub proved_at: u64,
    /// Failure generation covered by a confirmed session's kernel start.
    pub started_for: u32,
    /// That process's birth (unix seconds), strictly after the latest failure.
    pub started_at: u64,
    /// A move failed after earlier startup evidence: a help probe cannot release this hold.
    /// A confirmed newer session start or withdrawal of its failure can release it.
    pub blind: bool,
    /// The failure that made it [`Note::blind`] (one of [`Note::stops`]): a stop taken back takes the blindness it caused with it.
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
        o.insert("startedFor".into(), Value::from(self.started_for));
        o.insert("startedAt".into(), Value::from(self.started_at));
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
            started_for: count("startedFor"),
            started_at: n("startedAt"),
            blind: v.get("blind").and_then(Value::as_bool).unwrap_or(false),
            blinded_by: s("blindedBy").unwrap_or_default(),
        })
    }

    /// Whether a probe passed or a session started since the latest failure.
    pub const fn proved(&self) -> bool {
        self.proof_at() != 0
    }

    const fn proof_at(&self) -> u64 {
        let probe = if self.proved_for == self.failures {
            self.proved_at
        } else {
            0
        };
        let session = if self.started_for == self.failures {
            self.started_at
        } else {
            0
        };
        if session > probe { session } else { probe }
    }

    /// Whether a probe or real session start covered a recorded failure: a move failure
    /// after that evidence makes the note [`Note::blind`] ([`note_failure`]).
    const fn proved_since_a_failure(&self) -> bool {
        self.failures != 0 && self.proved()
    }

    /// Whether a probe since the latest failure ran the build and it exited
    /// with a failure — the one finding of a crash's probe that holds the
    /// upgrade's own moves off a build no move has failed on ([`gate`]):
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
    const fn refused_lately(&self, now: u64) -> bool {
        self.probed_for == self.failures
            && self.probed_at != 0
            && now < self.probed_at.saturating_add(PROBE_GAP_S)
    }
}

/// Where the note of build `version` in install `source` lives; `None` for a
/// word that is no plain name (nothing outside the directory is named).
fn note_file_name(source: &str, version: &str) -> Option<String> {
    let plain = |w: &str| {
        !w.is_empty()
            && w.bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'+' | b'_'))
            && !w.starts_with('.')
    };
    (plain(source) && plain(version)).then(|| format!("{source}-{version}.json"))
}

/// The note of build `version` in install `source`, if one was written.
pub fn load(opts: &Opts, source: &str, version: &str) -> Option<Note> {
    let name = note_file_name(source, version)?;
    let dir = check_upgrade_tree(&opts.state, Some("build-starts")).ok()??;
    let text = crate::harness::accounts::read_bounded(&dir.join(name), 256 * 1024)
        .ok()
        .flatten()?;
    Note::from_json(&text)
}

/// Writes `note`, returning whether it was persisted. A dry run writes nothing.
fn write(opts: &Opts, note: &Note) -> bool {
    if opts.dry_run {
        return false;
    }
    let Some(name) = note_file_name(&note.source, &note.version) else {
        return false;
    };
    let Ok(dir) = ensure_upgrade_tree(&opts.state, Some("build-starts")) else {
        return false;
    };
    crate::harness::atomic_write::write_atomic(&dir.join(name), &note.to_json()).is_ok()
}

/// What `session`'s record, saved at `now` over `before` (the record on disk), says of its build's start: a failure it carries ([`note_failure`]) or a stop it takes back ([`taken_back`]).
pub fn note(opts: &Opts, session: &str, before: Option<&St>, st: &St, now: u64) {
    if let Some(failed) = before.and_then(|before| taken_back(before, st)) {
        retract(opts, session, &failed, st);
    }
    note_failure(opts, session, st, now);
}

/// The failure `before` noted that `st` takes back: its own `no-resume` stop
/// ([`St::failed_start`], not a kept one) reopened as a relaunch that landed after all
/// ([`St::reopen_landed`], to [`Phase::Relaunched`]), so the build did start.
fn taken_back(before: &St, st: &St) -> Option<FailedStart> {
    let failed = before.failed_start()?;
    let own = matches!(&before.phase, Phase::Failed(why) if why == "no-resume");
    (own && matches!(st.phase, Phase::Relaunched { .. }) && st.to == failed.to).then_some(failed)
}

/// Withdraws [`taken_back`]'s failure from its build's note: no longer counted, the blindness it
/// caused lifted ([`Note::blinded_by`]), and a probe that covered every failure still covers every
/// one left.
fn retract(opts: &Opts, session: &str, failed: &FailedStart, st: &St) {
    let Some(mut note) = load(opts, &failed.source, &failed.to) else {
        return;
    };
    let stop = format!("{session} {}", failed.failed_at);
    let Some(at) = note.stops.iter().position(|s| *s == stop) else {
        return;
    };
    // A proof covers a prefix of the failure sequence. Removing an earlier failure
    // shifts that prefix too, even when later failures were never covered by it.
    let after = u32::try_from(note.stops.len() - at - 1).unwrap_or(u32::MAX);
    let removed_for = note.failures.saturating_sub(after);
    note.stops.remove(at);
    let was = note.failures;
    note.failures = was.saturating_sub(1);
    for count in [
        &mut note.probed_for,
        &mut note.proved_for,
        &mut note.started_for,
    ] {
        if *count >= removed_for {
            *count = count.saturating_sub(1);
        }
    }
    let unblinded = note.blind && note.blinded_by == stop;
    if unblinded {
        note.blind = false;
        note.blinded_by.clear();
    }
    write(opts, &note);
    let row = Report {
        rung: None,
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

/// Notes the failure `session`'s record carries ([`St::failed_start`]) against its build, once
/// per failure ([`Note::stops`]): a record saved again, a new process that keeps the stop
/// ([`St::keep_stop`]) and a restart's record carrying it ([`St::kept_by_restart`]) are the same
/// failure.
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
    let blinded = note.proved_since_a_failure() && moved_at >= note.proof_at();
    if blinded {
        note.blind = true;
        note.blinded_by.clone_from(&stop);
        let row = Report {
            rung: None,
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
            "a move onto the build failed after earlier startup evidence: automatic moves are \
             held until this build is confirmed starting after that failure or a newer build \
             is selected",
        );
    }
    note.failures = note.failures.saturating_add(1);
    note.failed_at = now;
    note.stops.push(stop);
    let over = note.stops.len().saturating_sub(STOPS_KEPT);
    note.stops.drain(..over);
    write(opts, &note);
}

/// A real upgrade relaunch can disprove an older startup ban. The caller supplies fresh
/// kernel identity, unique conversation ownership and tab ownership checks under the sweep
/// lock; historical records and a successful help probe supply none of that evidence.
pub(super) fn observed_start(
    opts: &Opts,
    r: &Report,
    st: &St,
    sf: &SessionFile,
    now: u64,
    confirm: impl FnOnce() -> bool,
) {
    match st.agent {
        Agent::Claude => {}
        Agent::Codex | Agent::Agy | Agent::Muse => return,
    }
    if opts.dry_run
        || !st.is_upgrade()
        || !matches!(st.phase, Phase::Relaunched { .. })
        || st.exited_at == 0
        || !matches!(st.source.as_str(), "managed" | "native")
        || sf.version != st.to
        || sf.pid == 0
        || sf.session_id != r.session
        || sf.status != "idle"
    {
        return;
    }
    let Some(started) = crate::harness::unix_time::parse_lstart(&sf.proc_start) else {
        return;
    };
    let Some(mut note) = load(opts, &st.source, &st.to) else {
        return;
    };
    if note.failures == 0
        || note.failed_at == 0
        || started <= note.failed_at
        || started > now
        || (note.proved() && !note.blind)
        || !confirm()
    {
        return;
    }
    note.started_for = note.failures;
    note.started_at = started;
    note.blind = false;
    note.blinded_by.clear();
    if !write(opts, &note) {
        return;
    }
    ledger(
        opts,
        &said(r.clone(), "build-start:observed".to_string()),
        "the target build started after its latest recorded startup failure and now holds its \
         conversation in its tab by kernel identity; that real start releases the older build hold",
    );
}

/// WHETHER AN AGENT MAY BE MOVED ONTO `to` AT `now`: `Ok` for a build with no failure to start noted that a probe has not found exiting — probed first where none has run on it (the module's "The upgrade's own first move"), and held only where that probe sees it exit with a failure — and for one proved since its latest failure by a probe or confirmed session start; else the probe is run now (no sooner than [`PROBE_GAP_S`] after one that did not pass), its answer recorded and said once on the ledger (`build-probe:<what>`, as row `r`), and `Err(`[`BUILD_FAILS`]`)` where it did not pass — or while a move failed after earlier startup evidence ([`Note::blind`]). A help probe cannot release that hold for any agent but one whose move the owner asked for (`owner_now`, their `--now`): that move is probed once more, and taken if the probe passes. [`observed_start`] releases an older hold only after a new target process is confirmed.
pub fn starts(
    opts: &Opts,
    r: &Report,
    to: &Candidate,
    now: u64,
    owner_now: bool,
    moving: bool,
) -> Result<(), &'static str> {
    let holds = crate::harness::first_exec::holds_dir(&opts.state);
    gate(
        opts,
        r,
        &Build::of(to),
        now,
        owner_now,
        false,
        moving,
        |exe| run_probe(exe, &holds),
    )
}

/// WHETHER A CRASHED AGENT'S RELAUNCH MAY MOVE ONTO `to` AT `now` ([`starts`] where the build must be PROVED to start, failure noted or not: the module's "A crashed agent").
pub fn proved(opts: &Opts, r: &Report, to: &Candidate, now: u64) -> Result<(), &'static str> {
    let holds = crate::harness::first_exec::holds_dir(&opts.state);
    gate(opts, r, &Build::of(to), now, false, true, true, |exe| {
        run_probe(exe, &holds)
    })
}

/// [`proved`] for a build named by its program `exe` (what the move types), its `version` as its store record spells it and its install `source` — a build whose version the Claude lane's [`Candidate`] cannot hold: Muse Code's revision form (`1.4.11-R6500.1`), which its live upgrade moves onto only once proved (`upgrade_drive`'s `muse` module).
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
    let holds = crate::harness::first_exec::holds_dir(&opts.state);
    gate(opts, r, &build, now, false, true, true, |exe| {
        run_probe(exe, &holds)
    })
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
fn run_probe(exe: &Path, holds: &Path) -> Probe {
    probe(exe, holds)
}

/// A probe a test names in place of [`probe`] ([`PROBE`]).
#[cfg(test)]
pub(crate) type NamedProbe = std::rc::Rc<dyn Fn(&Path) -> Probe>;

#[cfg(test)]
thread_local! {
    /// The probe a test's [`starts`] and [`proved`] run on its thread, where it names one; unnamed, the real [`probe`].
    pub(crate) static PROBE: std::cell::RefCell<Option<NamedProbe>> =
        const { std::cell::RefCell::new(None) };
}

/// [`run_probe`] in a test: the probe its thread names ([`PROBE`]), else
/// the real one.
#[cfg(test)]
fn run_probe(exe: &Path, holds: &Path) -> Probe {
    match PROBE.with(|named| named.borrow().clone()) {
        Some(run) => run(exe),
        None => probe(exe, holds),
    }
}

/// [`starts`] over the probe `run` for a step that moves — with `require`, [`proved`].
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

/// `starts_with`, `moving` as [`starts`] takes it.
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
    pub const fn word(self) -> &'static str {
        match self {
            Probe::Starts => "starts",
            Probe::Exited => "exited",
            Probe::NoExec => "no-exec",
            Probe::TimedOut => "timed-out",
        }
    }

    /// What it found, in a sentence's words.
    const fn words(self) -> &'static str {
        match self {
            Probe::Starts => "ran and exited 0",
            Probe::Exited => "exited with a failure",
            Probe::NoExec => "could not be run",
            Probe::TimedOut => "did not end in time",
        }
    }
}

/// `<exe> --help` ([`PROBE_ARG`]), bounded by [`VERSION_WAIT`], output discarded, in its own process group, killed whole at the bound; the program is held from before its start until it exits with a code of its own or never started ([`crate::harness::first_exec::hold`], under `holds` unless atpkg's store holds it).
#[must_use]
pub fn probe(exe: &Path, holds: &Path) -> Probe {
    probe_within(exe, VERSION_WAIT, Some(holds))
}

/// [`probe`] with an explicit bound; with no `holds`, the program is not held.
pub(super) fn probe_within(exe: &Path, limit: Duration, holds: Option<&Path>) -> Probe {
    let Ok(crate::harness::first_exec::Prepared {
        command: mut cmd,
        mut hold,
    }) = crate::harness::first_exec::prepare(
        exe,
        holds,
        crate::harness::first_exec::ProbeWord::Help,
    )
    else {
        return Probe::NoExec;
    };
    cmd.stdin(aterm_uds::spawnfd::Stdio::null())
        .stdout(aterm_uds::spawnfd::Stdio::null())
        .stderr(aterm_uds::spawnfd::Stdio::null());
    #[cfg(unix)]
    cmd.process_group(0);
    let Ok(mut child) = cmd.spawn() else {
        crate::harness::first_exec::never_started(&mut hold);
        return Probe::NoExec;
    };
    // Image custody and immutable wrapper capture precede execution. Their cost
    // cannot consume the build's run allowance before its child has even started.
    let now = std::time::Instant::now();
    let deadline = now
        .checked_add(limit)
        .unwrap_or_else(|| now + Duration::from_secs(3600));
    #[cfg(unix)]
    let group = libc::pid_t::try_from(child.id()).ok();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                // a signal's end shows nothing of a run: the hold stands
                if status.code().is_some() {
                    crate::harness::first_exec::ran(&mut hold);
                }
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
                    // The group this spawn made (`process_group(0)`: pgid is the child's pid), signalled before the child is reaped, so the id is still its.
                    let _ = aterm_uds::process::kill_group_until_empty(group);
                }
                let _ = child.kill();
                let _ = child.wait();
                return Probe::TimedOut;
            }
        }
    }
}
