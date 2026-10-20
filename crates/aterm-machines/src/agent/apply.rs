// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `agent apply` (root, through the one sudoers rule) and `agent attach` (the admin).
//!
//! `apply`, in order (design §3.2, §3.3, §4.4):
//!
//! 1. refuses unless it is root and its own path chain is root's alone;
//! 2. reads the envelope from standard input (at most 8 MiB) and verifies it with
//!    `ssh-keygen` against `allowed_signers`, this Mac's UUID, the clock and the floors.
//!    Before the signature is accepted it parses nothing and acts on nothing;
//! 3. takes `state/run.lock` (a live run answers `busy run=<id>`, recorded in
//!    `state/last_busy` for the screen watch, `super::watch`), checks the floor again
//!    under the lock, and RAISES the signer's floor before it acts, so a run that crashes
//!    cannot be replayed;
//! 4. copies each artifact the envelope names from the admin's inbox into the root cache,
//!    checking the hash of the root-owned copy, then removes the inbox copy as the admin;
//!    an `apply` then removes every cached artifact it does not name;
//! 5. writes `state/runs/<id>/`, starts `agent run-job <id>` detached (`setsid`, its output
//!    to a log) and `caffeinate -i -s -w <job>` beside it;
//! 6. streams the run's `progress.ndjson` until `report.json` appears ([`follow`]). If the
//!    connection drops, the job goes on; `attach` picks the stream up again.
//!
//! An `uninstall` envelope is the one action carried out in the foreground (the job could
//! not report from a directory it removes).

use super::{Layout, chain, state};
use crate::envelope::{self, Action, Opened};
use crate::model::{HostReport, User};
use crate::port::{Cmd, StagedChild, System};
use crate::protocol::Line;
use std::io::{Read, Write};
use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

/// How often [`follow`] looks for new progress.
const POLL: Duration = Duration::from_millis(250);

/// macOS's `admin` group (`dscl . -read /Groups/admin PrimaryGroupID`: 80, measured in the
/// macOS 27 VM). The enrolled admin is in it; other local accounts are not.
pub const ADMIN_GID: u32 = 80;

/// Print one stream line.
fn emit(out: &mut dyn Write, line: &Line) -> bool {
    match aterm_json::to_string(line) {
        Ok(text) => writeln!(out, "{text}").and_then(|()| out.flush()).is_ok(),
        Err(_) => false,
    }
}

/// Refuse as a stream line (what the controller's reader expects) and answer status 2.
fn refuse_line(out: &mut dyn Write, why: &str) -> ExitCode {
    emit(
        out,
        &Line::Refused {
            why: why.to_owned(),
        },
    );
    ExitCode::from(super::REFUSED)
}

/// `agent apply`.
pub fn apply(
    sys: &dyn System,
    layout: &Layout,
    exe: &Path,
    stdin: &mut dyn Read,
    out: &mut dyn Write,
) -> ExitCode {
    if sys.euid() != 0 {
        return refuse_line(
            out,
            "apply runs as root, through `sudo -n /Library/Management/aterm/bin/aterm machines \
             agent apply`",
        );
    }
    if let Err(why) = chain::check(sys, exe) {
        return refuse_line(
            out,
            &format!("the agent's own path is not root's alone: {why}"),
        );
    }
    let mut wire = Vec::new();
    if let Err(e) = stdin
        .take(envelope::MAX_WIRE as u64 + 1)
        .read_to_end(&mut wire)
    {
        return refuse_line(out, &format!("standard input: {e}"));
    }
    let opened = match verify(sys, layout, &wire) {
        Ok(o) => o,
        Err(why) => return refuse_line(out, &why),
    };
    let e = &opened.envelope;
    let run = format!("{}-{}", sys.now(), e.serial);
    match state::take_lock(sys, layout, &run, std::process::id()) {
        Ok(state::Lock::Taken) => {}
        Ok(state::Lock::Busy { run: other }) => {
            // Signed and fresh, so a controller is there: the screen watch's heartbeat.
            state::set_last_busy(sys, layout, &run);
            return refuse_line(out, &format!("busy run={other}"));
        }
        Err(why) => return refuse_line(out, &why),
    }
    let mut starting = match StartingRun::claim(sys, layout, &run, std::process::id()) {
        Ok(owner) => owner,
        // An unrecognizable holder is never removed as a guessed ownership claim.
        Err(why) => return refuse_line(out, &why),
    };
    // Under the lock: the floor again (another apply may have consumed a later serial),
    // then raise it before anything is acted on.
    let raised = state::floors_or_refuse(sys, layout).and_then(|floors| {
        let floor = state::floor_of(&floors, &opened.principal);
        if e.serial <= floor {
            return Err(envelope::Refusal::Replay {
                principal: opened.principal.clone(),
                serial: e.serial,
                floor,
            }
            .to_string());
        }
        state::raise_floor(sys, layout, &opened.principal, e.serial)
    });
    if let Err(why) = raised {
        return refuse_line(out, &why);
    }
    if e.action == Action::Uninstall {
        let code = super::install::uninstall(sys, layout, exe, &opened, &run, out);
        // The foreground uninstall owns/removes the persistent agent tree itself.
        starting.active = false;
        return code;
    }
    let notes = match take_artifacts(sys, layout, &opened) {
        Ok(notes) => notes,
        Err(why) => {
            return refuse_line(out, &why);
        }
    };
    if let Err(why) = start(
        sys,
        layout,
        exe,
        &wire,
        &opened,
        &run,
        &notes,
        &mut starting,
    ) {
        return refuse_line(out, &why);
    }
    follow(sys, layout, &run, out, POLL)
}

/// Steps 1–2 of `apply`: the signature, the host, the clock and the floors.
fn verify(sys: &dyn System, layout: &Layout, wire: &[u8]) -> Result<Opened, String> {
    if wire.len() > envelope::MAX_WIRE {
        return Err(format!(
            "malformed envelope: more than {} bytes",
            envelope::MAX_WIRE
        ));
    }
    let uuid = super::platform::platform_uuid(sys)
        .map_err(|why| format!("this Mac's UUID cannot be read, so no envelope is: {why}"))?;
    let floors = state::floors_or_refuse(sys, layout)?;
    if let Err(e) = sys.create_dir_all(&layout.stage(), 0o700) {
        return Err(format!("{}: {e}", layout.stage().display()));
    }
    envelope::open_in(
        sys,
        wire,
        &layout.allowed_signers(),
        &uuid,
        &|p| state::floor_of(&floors, p),
        &layout.stage(),
    )
    .map_err(|r| r.to_string())
}

/// Step 4: every artifact the envelope names (and an upgrade's archive) into the root
/// cache, from the admin's inbox, hash-checked on the root-owned copy; for an `apply`, the
/// rest of the cache pruned ([`prune_cache`]). Returns the notes for the run's log (an
/// inbox copy that could not be removed, what the pruning removed).
fn take_artifacts(
    sys: &dyn System,
    layout: &Layout,
    opened: &Opened,
) -> Result<Vec<String>, String> {
    let e = &opened.envelope;
    let mut wanted: Vec<(String, String)> = e
        .artifacts
        .iter()
        .map(|a| (a.sha256.to_ascii_lowercase(), a.name.clone()))
        .collect();
    if let Some(u) = &e.upgrade {
        wanted.push((
            u.sha256.to_ascii_lowercase(),
            format!("aterm {}", u.version),
        ));
    }
    let missing: Vec<&(String, String)> = wanted
        .iter()
        .filter(|(sha, _)| sys.stat(&layout.cached(sha)).is_err())
        .collect();
    let mut notes = if missing.is_empty() {
        Vec::new()
    } else {
        let managed = state::managed(sys, layout)?;
        let admin = crate::facts::user(sys, &managed.admin)?;
        copy_artifacts(sys, layout, &missing, &admin)?
    };
    if e.action == Action::Apply {
        notes.extend(prune_cache(sys, layout, &wanted));
    }
    Ok(notes)
}

/// After an `apply` envelope's artifacts are cached, every other cached artifact leaves the
/// cache. An apply names all that its baseline needs (a plan, `exec` or `upgrade` names
/// only some, and prunes nothing), so one it leaves out is a superseded pin, an app image
/// of hundreds of megabytes that would otherwise stay until uninstall; should a later
/// baseline name it again, the controller uploads it again from that run's `hello`. Root
/// unlinks in `cache/`, which only root writes. Returns a note for each one removed or
/// left.
fn prune_cache(sys: &dyn System, layout: &Layout, wanted: &[(String, String)]) -> Vec<String> {
    let cache = layout.cache();
    let names = match sys.list_dir(&cache) {
        Ok(names) => names,
        Err(e) => {
            return vec![format!(
                "cache: {}: {e}; nothing was pruned",
                cache.display()
            )];
        }
    };
    let mut notes = Vec::new();
    for name in names {
        if !super::is_sha256(&name) || wanted.iter().any(|(sha, _)| *sha == name) {
            continue;
        }
        let path = layout.cached(&name);
        notes.push(match sys.remove(&path) {
            Ok(()) => format!("cache: removed {name}, which this baseline does not name"),
            Err(e) => format!("cache: {name}, which this baseline does not name, stays ({e})"),
        });
    }
    notes
}

/// Copy each `(sha256, name)` from the admin's inbox into the root cache: first to a
/// root-owned landing file, whose hash is checked, then renamed into `cache/`. The inbox
/// copy is then removed, as the admin: root never unlinks by path in a directory the admin
/// can write. One that stays (hundreds of megabytes, until uninstall) is a note, not a
/// failure.
fn copy_artifacts(
    sys: &dyn System,
    layout: &Layout,
    missing: &[&(String, String)],
    admin: &User,
) -> Result<Vec<String>, String> {
    let inbox = super::user_inbox(&admin.home);
    let mut notes = Vec::new();
    for (at, (sha, name)) in missing.iter().map(|w| (&w.0, &w.1)).enumerate() {
        // Named twice (an artifact that is also the upgrade's archive): the first one took
        // it, and its inbox copy is gone.
        if missing[..at].iter().any(|w| w.0 == *sha) {
            continue;
        }
        if !super::is_sha256(sha) {
            return Err(format!("artifact {name}: {sha:?} is not a SHA-256 digest"));
        }
        let from = inbox.join(sha);
        let landing = layout.inbox().join(sha);
        sys.copy_file(&from, &landing, 0o644).map_err(|e| {
            format!(
                "artifact {name} ({sha}) is not in {}'s inbox ({e}); upload it with `put`",
                admin.name
            )
        })?;
        match sys.sha256(&landing) {
            Ok(got) if got == *sha => {}
            Ok(got) => {
                let _ = sys.remove(&landing);
                return Err(format!(
                    "artifact {name}: the inbox copy hashes to {got}, not {sha}"
                ));
            }
            Err(e) => {
                let _ = sys.remove(&landing);
                return Err(format!("artifact {name}: {e}"));
            }
        }
        sys.rename(&landing, &layout.cached(sha))
            .map_err(|e| format!("artifact {name}: {e}"))?;
        let removed = sys.run(
            &Cmd::new("/bin/rm")
                .args(["-f", "--"])
                .arg(from.as_os_str())
                .as_user(admin),
        );
        let why = match removed {
            Ok(ran) if ran.success() => continue,
            Ok(ran) => ran.describe(),
            Err(e) => e.to_string(),
        };
        notes.push(format!(
            "artifact {name}: cached, but {}'s inbox copy {} stays ({why})",
            admin.name,
            from.display()
        ));
    }
    Ok(notes)
}

/// What the run job reads back about who signed its envelope.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Signer {
    /// The `allowed_signers` principal.
    pub principal: String,
    /// The key's fingerprint.
    pub key_id: String,
}

/// Custody between taking the run lock and the child's one-way start commit.
/// This holds no PID registry: the port retains the exact unreaped Child until commit.
struct StartingRun<'s> {
    sys: &'s dyn System,
    layout: &'s Layout,
    holder: state::Holder,
    child: Option<Box<dyn StagedChild + 's>>,
    active: bool,
}

impl<'s> StartingRun<'s> {
    fn claim(sys: &'s dyn System, layout: &'s Layout, run: &str, pid: u32) -> Result<Self, String> {
        let holder = state::holder(sys, layout)
            .filter(|holder| holder.run == run && holder.pid == pid)
            .ok_or_else(|| "the taken run lock no longer names this apply process".to_owned())?;
        Ok(Self {
            sys,
            layout,
            holder,
            child: None,
            active: true,
        })
    }

    fn cancel(&mut self) -> Result<(), String> {
        if !self.active {
            return Ok(());
        }
        // Clear before any fallible cleanup: on an unproven reap the persistent lock
        // stays. The port's final Drop closes/signals only its exact owned child and
        // transfers unreaped custody to the existing reaper; no later Drop guesses a PID.
        self.active = false;
        if let Some(child) = &mut self.child {
            child.abort().map_err(|e| e.to_string())?;
        }
        state::release_holder(self.sys, self.layout, &self.holder)
    }
}

impl Drop for StartingRun<'_> {
    fn drop(&mut self) {
        if let Err(why) = self.cancel() {
            // Reporting cleanup must not panic while unwinding an earlier failure.
            let _ = std::io::stderr().write_all(
                format!("uncommitted run cleanup unproven: {why}; run lock retained\n").as_bytes(),
            );
        }
    }
}

/// Step 5: the run directory, the detached job and its keep-awake. `notes` open the job's
/// log (and go to standard error).
#[expect(
    clippy::too_many_arguments,
    reason = "one internal run start and its custody inputs"
)]
fn start<'s>(
    sys: &'s dyn System,
    layout: &Layout,
    exe: &Path,
    wire: &[u8],
    opened: &Opened,
    run: &str,
    notes: &[String],
    starting: &mut StartingRun<'s>,
) -> Result<(), String> {
    match prepare_start(sys, layout, exe, wire, opened, run, notes, starting) {
        Ok(()) => Ok(()),
        Err(why) => match starting.cancel() {
            Ok(()) => Err(why),
            Err(cleanup) => Err(format!(
                "{why}; uncommitted job cleanup unproven: {cleanup}; run lock retained"
            )),
        },
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "one internal run start and its custody inputs"
)]
fn prepare_start<'s>(
    sys: &'s dyn System,
    layout: &Layout,
    exe: &Path,
    wire: &[u8],
    opened: &Opened,
    run: &str,
    notes: &[String],
    starting: &mut StartingRun<'s>,
) -> Result<(), String> {
    if !starting.active
        || starting.child.is_some()
        || starting.holder.run != run
        || starting.layout.root != layout.root
    {
        return Err("the run start does not own a fresh matching handoff".into());
    }
    let dir = layout.run_dir(run);
    let io = |e: std::io::Error| format!("{}: {e}", dir.display());
    // The report holds root's facts and an `exec`'s output: readable by the admin (the
    // forced command's `attach`, through the admin group) and root, not by every account.
    sys.create_dir_all(&layout.runs(), 0o755).map_err(io)?;
    sys.create_dir_all(&dir, 0o750).map_err(io)?;
    sys.chown(&dir, 0, ADMIN_GID).map_err(io)?;
    sys.write_file(&dir.join("envelope"), wire, 0o600)
        .map_err(io)?;
    let signer = Signer {
        principal: opened.principal.clone(),
        key_id: opened.key_id.clone(),
    };
    let signer = aterm_toml::to_string(&signer).map_err(|e| e.to_string())?;
    sys.write_file(&dir.join("signer.toml"), signer.as_bytes(), 0o644)
        .map_err(io)?;
    let started = aterm_json::to_string(&Line::Started {
        run: run.to_owned(),
    })
    .map_err(|e| e.to_string())?;
    sys.write_file(
        &dir.join("progress.ndjson"),
        format!("{started}\n").as_bytes(),
        0o644,
    )
    .map_err(io)?;
    if !notes.is_empty() {
        let text: String = notes.iter().map(|n| format!("note: {n}\n")).collect();
        // Not `eprint!`, which panics when the connection behind standard error is gone.
        let _ = std::io::stderr().write_all(text.as_bytes());
        sys.write_file(&dir.join("job.log"), text.as_bytes(), 0o600)
            .map_err(io)?;
    }
    let job = Cmd::new(exe).args(["machines", "agent", "run-job", run]);
    starting.child = Some(
        sys.stage_detached(&job, &dir.join("job.log"))
            .map_err(|e| format!("the run job did not start: {e}"))?,
    );
    let pid = starting
        .child
        .as_ref()
        .expect("the staged child is retained")
        .id();
    let birth = state::started(sys, pid)
        .ok_or_else(|| "the staged run job's start identity could not be read".to_owned())?;
    sys.write_file(&dir.join("job.pid"), format!("{pid}\n").as_bytes(), 0o644)
        .map_err(io)?;
    let to = state::Holder {
        pid,
        run: run.to_owned(),
        start: Some(birth),
    };
    state::hand_lock_from(sys, layout, &starting.holder, &to)?;
    starting.holder = to;
    // Final fallible start operation. Success transfers detached lifetime to the job;
    // an error delivers no commit byte and retains the exact Child for cancellation.
    starting
        .child
        .as_mut()
        .expect("the staged child is retained")
        .commit()
        .map_err(|e| format!("the run job was not committed: {e}"))?;
    starting.active = false;
    drop(starting.child.take());
    // Keep the Mac awake while the job lives (design §4.4). A Mac that sleeps anyway
    // only delays the run, so a failure here is not fatal.
    let _ = sys.spawn_detached(
        &Cmd::new("/usr/bin/caffeinate").args(["-i", "-s", "-w", &pid.to_string()]),
        &dir.join("caffeinate.log"),
    );
    Ok(())
}

/// `agent attach <run>`: the run's stream from its start.
pub fn attach(sys: &dyn System, layout: &Layout, run: &str, out: &mut dyn Write) -> ExitCode {
    if !super::is_run_id(run) {
        return refuse_line(out, &format!("{run:?} is not a run id"));
    }
    if sys.stat(&layout.run_dir(run)).is_err() {
        return refuse_line(out, &format!("no run {run} on this host"));
    }
    follow(sys, layout, run, out, POLL)
}

/// Stream `state/runs/<run>/progress.ndjson` from its start, then the report, polling every
/// `poll`. Ends with a refusal line when the job is gone and left no report. A failed
/// write (the controller went away) ends the stream; the job goes on.
pub fn follow(
    sys: &dyn System,
    layout: &Layout,
    run: &str,
    out: &mut dyn Write,
    poll: Duration,
) -> ExitCode {
    let dir = layout.run_dir(run);
    let mut sent = 0_usize;
    let mut flush_progress = |out: &mut dyn Write| -> bool {
        let Ok(bytes) = sys.read_file(&dir.join("progress.ndjson")) else {
            return true;
        };
        let Some(complete) = bytes.iter().rposition(|b| *b == b'\n') else {
            return true;
        };
        if complete < sent {
            return true;
        }
        let fresh = &bytes[sent..=complete];
        sent = complete + 1;
        out.write_all(fresh).and_then(|()| out.flush()).is_ok()
    };
    let mut gone_once = false;
    loop {
        if !flush_progress(out) {
            return ExitCode::FAILURE;
        }
        if let Ok(bytes) = sys.read_file(&dir.join("report.json")) {
            if !flush_progress(out) {
                return ExitCode::FAILURE;
            }
            return match aterm_json::from_slice::<HostReport>(&bytes) {
                Ok(report) => {
                    let failed = report.failed();
                    emit(out, &Line::Report(Box::new(report)));
                    if failed {
                        ExitCode::FAILURE
                    } else {
                        ExitCode::SUCCESS
                    }
                }
                Err(e) => refuse_line(out, &format!("run {run}: report.json does not parse: {e}")),
            };
        }
        // The job holds the run lock from its hand-off until it has written its report.
        // The lock's holder identity (pid and start) is what says it still runs; a bare
        // pid could have been reused since a crash, and the stream would never end.
        if !state::running(sys, layout, run) {
            // Once more after the job is seen gone: it may have written the report between
            // the two reads.
            if gone_once {
                return refuse_line(
                    out,
                    &format!(
                        "run {run}: the run job ended without a report (its log is \
                         {}/job.log)",
                        dir.display()
                    ),
                );
            }
            gone_once = true;
            continue;
        }
        std::thread::sleep(poll);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Effect, HostFacts, ItemReport, Mode, Outcome};
    use crate::port::{Fake, Ran, Stat};

    fn layout() -> Layout {
        Layout { root: "/A".into() }
    }

    fn row(id: &str) -> ItemReport {
        ItemReport {
            id: id.into(),
            family: "power".into(),
            title: id.into(),
            outcome: Outcome::Ok,
            from: None,
            to: Some("0".into()),
            why: None,
            revert: None,
            effect: Effect::Now,
            detail: None,
        }
    }

    fn report(items: Vec<ItemReport>) -> HostReport {
        HostReport {
            schema: 1,
            mode: Mode::Apply,
            serial: 5,
            key_id: String::new(),
            run_id: "1-5".into(),
            started: 1,
            finished: 2,
            facts: HostFacts::default(),
            items,
            refused: None,
        }
    }

    fn lines(out: &[u8]) -> Vec<Line> {
        String::from_utf8_lossy(out)
            .lines()
            .map(|l| aterm_json::from_str(l).unwrap())
            .collect()
    }

    #[test]
    fn follow_replays_the_progress_from_the_start_then_the_report() {
        let fake = Fake::root();
        let mut progress = String::new();
        for l in [
            Line::Started { run: "1-5".into() },
            Line::Item(row("power.ac.sleep")),
        ] {
            progress.push_str(&aterm_json::to_string(&l).unwrap());
            progress.push('\n');
        }
        // A half-written last line is not sent.
        progress.push_str("{\"item\":");
        fake.file("/A/state/runs/1-5/progress.ndjson", progress.as_bytes());
        fake.file(
            "/A/state/runs/1-5/report.json",
            aterm_json::to_string(&report(vec![row("power.ac.sleep")]))
                .unwrap()
                .as_bytes(),
        );
        let mut out = Vec::new();
        let code = follow(&fake, &layout(), "1-5", &mut out, Duration::ZERO);
        assert_eq!(code, ExitCode::SUCCESS);
        let got = lines(&out);
        assert_eq!(got.len(), 3, "{got:?}");
        assert!(matches!(&got[0], Line::Started { run } if run == "1-5"));
        assert!(matches!(&got[1], Line::Item(r) if r.id == "power.ac.sleep"));
        assert!(matches!(&got[2], Line::Report(r) if r.items.len() == 1));
    }

    #[test]
    fn a_job_that_died_without_a_report_ends_the_stream_with_a_refusal() {
        let fake = Fake::root();
        fake.file(
            "/A/state/runs/1-5/progress.ndjson",
            b"{\"started\":{\"run\":\"1-5\"}}\n",
        );
        fake.file("/A/state/runs/1-5/job.pid", b"4242\n");
        let mut out = Vec::new();
        let code = follow(&fake, &layout(), "1-5", &mut out, Duration::ZERO);
        assert_eq!(code, ExitCode::from(super::super::REFUSED));
        let got = lines(&out);
        assert!(
            matches!(got.last(), Some(Line::Refused { why }) if why.contains("without a report")),
            "{got:?}"
        );
    }

    #[test]
    fn a_crashed_jobs_reused_pid_does_not_keep_the_stream_open() {
        let fake = Fake::root();
        fake.file(
            "/A/state/runs/1-5/progress.ndjson",
            b"{\"started\":{\"run\":\"1-5\"}}\n",
        );
        // The lock still names the run; its pid now belongs to a process started later.
        fake.file(
            "/A/state/run.lock",
            b"pid=4242 run=1-5 start=Fri_Oct_16_22:36:10_2026\n",
        );
        fake.alive(4242);
        fake.on(
            "/usr/bin/env TZ=UTC0 /bin/ps -o lstart= -p 4242",
            Ran::ok("Sat Oct 17 08:01:02 2026\n"),
        );
        let mut out = Vec::new();
        let code = follow(&fake, &layout(), "1-5", &mut out, Duration::ZERO);
        assert_eq!(code, ExitCode::from(super::super::REFUSED));
        assert!(
            matches!(lines(&out).last(), Some(Line::Refused { why }) if why.contains("without a report"))
        );
    }

    fn opened(serial: u64) -> Opened {
        Opened {
            envelope: crate::envelope::parse_body(
                format!(
                    "schema = 1\nhost = \"{UUID}\"\nhost_name = \"h\"\nserial = {serial}\nissued = 1\nexpires = 2\naction = \"facts\"\n"
                )
                .as_bytes(),
            )
            .unwrap(),
            principal: "signing@ctl".into(),
            key_id: String::new(),
        }
    }

    #[test]
    fn a_stale_report_does_not_authorize_a_missing_start_holder() {
        let fake = Fake::root();
        fake.file("/A/state/runs/1-5/report.json", b"{}");
        let error = match StartingRun::claim(&fake, &layout(), "1-5", 17) {
            Ok(_) => panic!("a stale report is not lock ownership"),
            Err(error) => error,
        };
        assert!(error.contains("no longer names this apply"), "{error}");
        assert!(!fake.calls().iter().any(|call| call.contains("run-job")));
    }

    #[test]
    fn apply_refuses_without_root_and_reads_nothing() {
        let mut fake = Fake::root();
        fake.euid = 501;
        let mut out = Vec::new();
        let mut stdin: &[u8] = b"anything";
        let code = apply(&fake, &layout(), Path::new("/x"), &mut stdin, &mut out);
        assert_eq!(code, ExitCode::from(super::super::REFUSED));
        assert_eq!(stdin, b"anything", "standard input was read");
        assert!(fake.calls().is_empty());
    }

    #[test]
    fn apply_refuses_an_agent_whose_path_chain_is_not_roots() {
        let fake = Fake::root();
        // Nothing seeded: the executable's stat fails.
        let mut out = Vec::new();
        let mut stdin: &[u8] = b"anything";
        let code = apply(
            &fake,
            &layout(),
            Path::new("/Users//admin/aterm"),
            &mut stdin,
            &mut out,
        );
        assert_eq!(code, ExitCode::from(super::super::REFUSED));
        let got = lines(&out);
        assert!(
            matches!(&got[0], Line::Refused { why } if why.contains("path is not root's")),
            "{got:?}"
        );
        assert_eq!(stdin, b"anything");
    }

    /// The answers `apply` needs on a host where the envelope verifies: the chain, the
    /// UUID (the VM's ioreg) and ssh-keygen's two answers as measured in the VM.
    fn verifying_host(principal: &str) -> Fake {
        let mut fake = Fake::root();
        fake.next_pid = 4242;
        fake.on(
            "/usr/bin/env TZ=UTC0 /bin/ps -o lstart= -p 4242",
            Ran::ok("known child birth\n"),
        );
        let dir = Stat {
            uid: 0,
            gid: 0,
            mode: 0o755,
            dir: true,
            symlink: false,
            len: 0,
        };
        for d in ["/", "/A", "/A/agent", "/A/stage"] {
            fake.stat_of(d, dir);
        }
        fake.stat_of("/A/agent/aterm", Stat { dir: false, ..dir });
        fake.on(
            "/usr/sbin/ioreg -rd1 -c IOPlatformExpertDevice",
            Ran::ok(crate::agent::platform::tests::IOREG_VM),
        );
        let pid = std::process::id();
        fake.on(
            &format!("/usr/bin/ssh-keygen -Y find-principals -s /A/stage/aterm-open-{pid}.sig -f /A/allowed_signers"),
            Ran::ok(&format!("{principal}\n")),
        );
        fake.on(
            &format!("/usr/bin/ssh-keygen -Y verify -f /A/allowed_signers -I {principal} -n aterm-machines -s /A/stage/aterm-open-{pid}.sig"),
            Ran::ok(&format!("Good \"aterm-machines\" signature for {principal} with ED25519 key SHA256:P6HGW+cv8zgw7CrKPm2sOl5fu7RG4dXNK0SYwxiFmiw\n")), // gitleaks:allow
        );
        fake
    }

    const UUID: &str = "C3396674-FCD3-52E5-A1C1-BD5B9E8F5C43";

    /// An armored signature's shape (the fake's ssh-keygen answers for it).
    const SIG: &str =
        "-----BEGIN SSH SIGNATURE-----\nU1NIU0lHAAAAAQ==\n-----END SSH SIGNATURE-----\n";

    fn wire(fake: &Fake, serial: u64, action: crate::envelope::Action) -> Vec<u8> {
        let e = crate::envelope::Envelope {
            schema: 1,
            host: UUID.into(),
            host_name: "lab-07".into(),
            serial,
            issued: fake.now,
            expires: fake.now + 86_400,
            action,
            disrupt: crate::envelope::Disrupt::WhenIdle,
            idle_secs: 900,
            baseline: Some("schema = 1\n".into()),
            artifacts: Vec::new(),
            exec: None,
            upgrade: None,
            rekey: None,
        };
        format!("{SIG}{}", crate::envelope::body_text(&e).unwrap()).into_bytes()
    }

    #[test]
    fn apply_raises_the_floor_takes_the_lock_and_starts_a_detached_job_before_streaming() {
        let fake = verifying_host("signing@ctl");
        let run = format!("{}-5", fake.now);
        fake.file(
            &format!("/A/state/runs/{run}/report.json"),
            aterm_json::to_string(&report(Vec::new()))
                .unwrap()
                .as_bytes(),
        );
        let mut out = Vec::new();
        let mut stdin: &[u8] = &wire(&fake, 5, crate::envelope::Action::Apply);
        let code = apply(
            &fake,
            &layout(),
            Path::new("/A/agent/aterm"),
            &mut stdin,
            &mut out,
        );
        assert_eq!(code, ExitCode::SUCCESS, "{}", String::from_utf8_lossy(&out));
        // The floor is raised to the envelope's serial.
        let floors = state::floors_or_refuse(&fake, &layout()).unwrap();
        assert_eq!(state::floor_of(&floors, "signing@ctl"), 5);
        // The lock was handed to the job; the envelope kept root-only.
        assert_eq!(
            state::holder(&fake, &layout()),
            Some(state::Holder {
                pid: 4242,
                run: run.clone(),
                start: Some("known_child_birth".into()),
            })
        );
        // The run directory is root's and the admin group's, not every account's.
        let calls = fake.calls();
        assert!(
            calls.contains(&format!("mkdir /A/state/runs/{run} 750"))
                && calls.contains(&format!("chown 0:80 /A/state/runs/{run}")),
            "{calls:#?}"
        );
        assert_eq!(
            fake.mode_of(&format!("/A/state/runs/{run}/envelope")),
            Some(0o600)
        );
        let calls = fake.calls();
        let floor = calls
            .iter()
            .position(|c| c == "write /A/state/floors.toml 644")
            .unwrap();
        let job = calls
            .iter()
            .position(|c| {
                c == &format!("stage /A/agent/aterm machines agent run-job {run} >> /A/state/runs/{run}/job.log")
            })
            .unwrap_or_else(|| panic!("{calls:#?}"));
        assert!(floor < job, "the floor rises before anything is acted on");
        assert!(calls.iter().any(|c| c == "stage committed 4242"));
        assert!(calls.contains(&format!(
            "detach /usr/bin/caffeinate -i -s -w 4242 >> /A/state/runs/{run}/caffeinate.log"
        )));
        let got = lines(&out);
        assert!(
            matches!(&got[0], Line::Started { run: r } if *r == run),
            "{got:?}"
        );
        assert!(matches!(got.last(), Some(Line::Report(_))), "{got:?}");
    }

    #[test]
    fn a_replayed_or_busy_apply_is_refused_and_changes_nothing() {
        let fake = verifying_host("signing@ctl");
        fake.file(
            "/A/state/floors.toml",
            b"[[floor]]\nprincipal = \"signing@ctl\"\nserial = 5\n",
        );
        let mut out = Vec::new();
        let mut stdin: &[u8] = &wire(&fake, 5, crate::envelope::Action::Apply);
        let code = apply(
            &fake,
            &layout(),
            Path::new("/A/agent/aterm"),
            &mut stdin,
            &mut out,
        );
        assert_eq!(code, ExitCode::from(super::super::REFUSED));
        assert!(
            matches!(&lines(&out)[0], Line::Refused { why } if why == "replay principal=signing@ctl serial=5 floor=5"),
            "{}",
            String::from_utf8_lossy(&out)
        );
        // A live run holds the lock: busy, and the floor stays where it was.
        let fake = verifying_host("signing@ctl");
        fake.file("/A/state/run.lock", b"pid=77 run=1799999999-4\n");
        fake.alive(77);
        let mut out = Vec::new();
        let mut stdin: &[u8] = &wire(&fake, 6, crate::envelope::Action::Apply);
        let code = apply(
            &fake,
            &layout(),
            Path::new("/A/agent/aterm"),
            &mut stdin,
            &mut out,
        );
        assert_eq!(code, ExitCode::from(super::super::REFUSED));
        assert!(
            matches!(&lines(&out)[0], Line::Refused { why } if why == "busy run=1799999999-4"),
            "{}",
            String::from_utf8_lossy(&out)
        );
        assert!(
            state::floors_or_refuse(&fake, &layout())
                .unwrap()
                .is_empty()
        );
        // Recorded for the screen watch, by the id the run would have had.
        assert_eq!(
            state::last_busy(&fake, &layout()).as_deref(),
            Some("1800000000-6")
        );
        assert!(
            !fake
                .calls()
                .iter()
                .any(|c| c.starts_with("detach") || c.starts_with("stage "))
        );
    }

    fn admin() -> User {
        User {
            name: "admin".into(),
            uid: 501,
            gid: 20,
            home: "/Users//admin".into(),
            shell: "/bin/zsh".into(),
            admin: true,
        }
    }

    /// SHA-256 of "hello".
    const HELLO: &str = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";
    const INBOX: &str = "/Users//admin/Library/Caches/aterm-machines/inbox";

    #[test]
    fn an_artifact_is_cached_only_from_a_root_owned_copy_that_hashes_to_its_name() {
        let fake = Fake::root();
        let sha = HELLO.to_owned();
        fake.file(&format!("{INBOX}/{sha}"), b"hello");
        fake.on(&format!("admin$ /bin/rm -f -- {INBOX}/{sha}"), Ran::ok(""));
        let want = (sha.clone(), "app.Emacs".to_owned());
        let notes = copy_artifacts(&fake, &layout(), &[&want], &admin()).unwrap();
        assert_eq!(notes, Vec::<String>::new());
        assert_eq!(fake.written(&format!("/A/cache/{sha}")).unwrap(), b"hello");
        assert!(fake.written(&format!("/A/inbox/{sha}")).is_none());
        // Bytes swapped in the inbox after upload: refused, nothing cached, and the inbox
        // copy is left for the admin to see.
        let fake = Fake::root();
        fake.file(&format!("{INBOX}/{sha}"), b"evil");
        let err = copy_artifacts(&fake, &layout(), &[&want], &admin()).unwrap_err();
        assert!(err.contains("hashes to"), "{err}");
        assert!(fake.written(&format!("/A/cache/{sha}")).is_none());
        assert!(fake.written(&format!("/A/inbox/{sha}")).is_none());
        assert!(!fake.calls().iter().any(|c| c.contains("/bin/rm")));
        // Missing: the refusal says to upload it.
        let fake = Fake::root();
        let err = copy_artifacts(&fake, &layout(), &[&want], &admin()).unwrap_err();
        assert!(err.contains("upload it with `put`"), "{err}");
    }

    #[test]
    fn the_inbox_copy_goes_once_it_is_cached_and_by_the_admin_not_by_root() {
        let fake = Fake::root();
        fake.file(&format!("{INBOX}/{HELLO}"), b"hello");
        fake.on(
            &format!("admin$ /bin/rm -f -- {INBOX}/{HELLO}"),
            Ran::ok(""),
        );
        let want = (HELLO.to_owned(), "app.Emacs".to_owned());
        copy_artifacts(&fake, &layout(), &[&want], &admin()).unwrap();
        let calls = fake.calls();
        let cached = calls
            .iter()
            .position(|c| c == &format!("rename /A/inbox/{HELLO} /A/cache/{HELLO}"))
            .unwrap_or_else(|| panic!("{calls:#?}"));
        let removed = calls
            .iter()
            .position(|c| c == &format!("admin$ /bin/rm -f -- {INBOX}/{HELLO}"))
            .unwrap_or_else(|| panic!("{calls:#?}"));
        assert!(cached < removed, "{calls:#?}");
        assert!(
            !calls
                .iter()
                .any(|c| c.starts_with(&format!("remove {INBOX}"))),
            "root unlinked in the admin's inbox: {calls:#?}"
        );
        // A removal that fails is a note; the artifact is cached all the same.
        let fake = Fake::root();
        fake.file(&format!("{INBOX}/{HELLO}"), b"hello");
        fake.on(
            &format!("admin$ /bin/rm -f -- {INBOX}/{HELLO}"),
            Ran::exit(1, &format!("rm: {INBOX}/{HELLO}: Permission denied\n")),
        );
        let notes = copy_artifacts(&fake, &layout(), &[&want], &admin()).unwrap();
        assert_eq!(
            fake.written(&format!("/A/cache/{HELLO}")).unwrap(),
            b"hello"
        );
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(
            notes[0].contains(&format!("{INBOX}/{HELLO}"))
                && notes[0].contains("Permission denied"),
            "{notes:?}"
        );
    }

    #[test]
    fn an_artifact_named_twice_is_taken_once() {
        // The fake leaves the file after `rm`; the live inbox would not have it the second
        // time, and the run would be refused for an artifact it had just cached.
        let fake = Fake::root();
        fake.file(&format!("{INBOX}/{HELLO}"), b"hello");
        fake.on(
            &format!("admin$ /bin/rm -f -- {INBOX}/{HELLO}"),
            Ran::ok(""),
        );
        let app = (HELLO.to_owned(), "app.Emacs".to_owned());
        let upgrade = (HELLO.to_owned(), "aterm 0.104.0".to_owned());
        let notes = copy_artifacts(&fake, &layout(), &[&app, &upgrade], &admin()).unwrap();
        assert_eq!(notes, Vec::<String>::new());
        let calls = fake.calls();
        let count = |p: &str| calls.iter().filter(|c| c.starts_with(p)).count();
        assert_eq!(count(&format!("copy {INBOX}/{HELLO} ")), 1, "{calls:#?}");
        assert_eq!(count("admin$ /bin/rm"), 1, "{calls:#?}");
    }

    #[test]
    fn an_apply_prunes_the_cached_artifacts_its_baseline_no_longer_names() {
        // The baseline's pin moved from an old image to HELLO, both cached by earlier runs.
        let old = "1".repeat(64);
        let seeded = || {
            let fake = Fake::root();
            fake.file(&format!("/A/cache/{HELLO}"), b"hello");
            fake.file(&format!("/A/cache/{old}"), b"old image");
            fake.file("/A/cache/README", b"not an artifact");
            fake
        };
        let mut e = opened(5);
        e.envelope.artifacts = vec![crate::envelope::ArtifactRef {
            sha256: HELLO.into(),
            name: "app.Emacs".into(),
        }];
        // A plan (or exec, or upgrade) names only part of what the host needs: it keeps all.
        for action in [Action::Plan, Action::Exec, Action::Upgrade] {
            let fake = seeded();
            e.envelope.action = action;
            assert_eq!(take_artifacts(&fake, &layout(), &e), Ok(Vec::new()));
            assert!(
                fake.written(&format!("/A/cache/{old}")).is_some(),
                "{action:?}"
            );
        }
        let fake = seeded();
        e.envelope.action = Action::Apply;
        let notes = take_artifacts(&fake, &layout(), &e).unwrap();
        assert!(fake.written(&format!("/A/cache/{old}")).is_none());
        assert_eq!(
            fake.written(&format!("/A/cache/{HELLO}")).unwrap(),
            b"hello"
        );
        assert!(fake.written("/A/cache/README").is_some());
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(notes[0].contains(&old), "{notes:?}");
    }

    #[test]
    fn a_note_opens_the_run_log_before_the_job_starts() {
        let mut fake = Fake::root();
        fake.next_pid = 4242;
        fake.file("/A/state/run.lock", b"pid=17 run=1-5 start=parent\n");
        fake.on(
            "/usr/bin/env TZ=UTC0 /bin/ps -o lstart= -p 4242",
            Ran::ok("known child birth\n"),
        );
        let layout = layout();
        let mut owner = StartingRun::claim(&fake, &layout, "1-5", 17).unwrap();
        start(
            &fake,
            &layout,
            Path::new("/A/agent/aterm"),
            SIG.as_bytes(),
            &opened(5),
            "1-5",
            &["the inbox copy stays".to_owned()],
            &mut owner,
        )
        .unwrap();
        assert_eq!(
            fake.written("/A/state/runs/1-5/job.log").unwrap(),
            b"note: the inbox copy stays\n"
        );
        assert_eq!(fake.mode_of("/A/state/runs/1-5/job.log"), Some(0o600));
        let calls = fake.calls();
        let log = calls
            .iter()
            .position(|c| c == "write /A/state/runs/1-5/job.log 600")
            .unwrap();
        let job = calls.iter().position(|c| c.starts_with("stage ")).unwrap();
        assert!(log < job, "{calls:#?}");
    }

    #[test]
    fn attach_refuses_a_run_id_that_is_not_one() {
        let fake = Fake::root();
        let mut out = Vec::new();
        let code = attach(&fake, &layout(), "../x", &mut out);
        assert_eq!(code, ExitCode::from(super::super::REFUSED));
        assert!(fake.calls().is_empty());
    }
}

#[cfg(test)]
#[path = "start_tests.rs"]
mod start_tests;
