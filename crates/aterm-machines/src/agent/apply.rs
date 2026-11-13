// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `agent apply` root execution and `agent attach` progress streaming ([`follow`]).

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

/// macOS's `admin` group.
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
            state::set_last_busy(sys, layout, &run);
            return refuse_line(out, &format!("busy run={other}"));
        }
        Err(why) => return refuse_line(out, &why),
    }
    let mut starting = match StartingRun::claim(sys, layout, &run, std::process::id()) {
        Ok(owner) => owner,
        Err(why) => return refuse_line(out, &why),
    };
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

/// Verify the envelope: its signature, this Mac's UUID, the clock and the floors.
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

/// Cache every artifact the envelope names (and an upgrade's archive), copying the ones not yet
/// cached from the admin's inbox; for an `apply`, prune the rest ([`prune_cache`]).
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
    if let Some((sha, name)) = wanted.iter().find(|(sha, _)| !super::is_sha256(sha)) {
        return Err(format!("artifact {name}: {sha:?} is not a SHA-256 digest"));
    }
    let missing: Vec<&(String, String)> = wanted
        .iter()
        .filter(|(sha, _)| {
            !sys.stat(&layout.cached(sha))
                .is_ok_and(|st| !st.dir && !st.symlink)
        })
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

/// After an `apply` envelope's artifacts are cached, every other cached artifact leaves the cache.
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

/// Copy each `(sha256, name)` (a digest [`take_artifacts`] checked) from the admin's inbox into
/// the root cache.
fn copy_artifacts(
    sys: &dyn System,
    layout: &Layout,
    missing: &[&(String, String)],
    admin: &User,
) -> Result<Vec<String>, String> {
    let inbox = super::user_inbox(&admin.home);
    let mut notes = Vec::new();
    for (at, (sha, name)) in missing.iter().map(|w| (&w.0, &w.1)).enumerate() {
        if missing[..at].iter().any(|w| w.0 == *sha) {
            continue;
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
        if let Err(e) = sys.rename(&landing, &layout.cached(sha)) {
            let _ = sys.remove(&landing);
            return Err(format!("artifact {name}: {e}"));
        }
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
            let _ = std::io::stderr().write_all(
                format!("uncommitted run cleanup unproven: {why}; run lock retained\n").as_bytes(),
            );
        }
    }
}

/// Start the run: its directory, the detached job and its keep-awake.
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
    starting
        .child
        .as_mut()
        .expect("the staged child is retained")
        .commit()
        .map_err(|e| format!("the run job was not committed: {e}"))?;
    starting.active = false;
    drop(starting.child.take());
    // Idle sleep only (`-i`): a run never keeps a MacBook running with its lid closed, which
    // `-s` (PreventSystemSleep) would on AC power; a person who wants that opts in through
    // acaffeine's Keep Awake with Lid Closed.
    let _ = sys.spawn_detached(
        &Cmd::new("/usr/bin/caffeinate").args(["-i", "-w", &pid.to_string()]),
        &dir.join("caffeinate.log"),
    );
    Ok(())
}

/// `agent attach <run>`.
pub fn attach(sys: &dyn System, layout: &Layout, run: &str, out: &mut dyn Write) -> ExitCode {
    if !super::is_run_id(run) {
        return refuse_line(out, &format!("{run:?} is not a run id"));
    }
    if sys.stat(&layout.run_dir(run)).is_err() {
        return refuse_line(out, &format!("no run {run} on this host"));
    }
    follow(sys, layout, run, out, POLL)
}

/// Stream `state/runs/<run>/progress.ndjson` from its start, then the report, polling every `poll`.
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
        if !state::running(sys, layout, run) {
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
#[path = "apply_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "start_tests.rs"]
mod start_tests;
