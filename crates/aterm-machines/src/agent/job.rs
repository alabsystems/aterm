// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `agent run-job <run>`: one run, detached from the SSH session that asked for it
//! (design §4.4–§4.6).
//!
//! `apply` starts it after verifying the envelope and writing `state/runs/<run>/`. The job
//! accepts only a run directory the agent wrote (root's, 0600 envelope, the run lock naming
//! it) and re-reads the envelope from there, so nothing it acts on came from anyone but a
//! verified signer. Each item's row goes to `progress.ndjson` as it finishes, then the
//! whole report to `report.json`.

use super::apply::Signer;
use super::{Layout, chain, state};
use crate::engine::{self, Journal};
use crate::envelope::{self, Action, Disrupt, Envelope};
use crate::items::{self, Ctx};
use crate::model::{Effect, HostFacts, HostReport, ItemReport, Mode, Outcome, User};
use crate::port::{Cmd, Ran, System};
use crate::protocol::Line;
use aterm_converge::Remedy;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

/// The Developer ID team an `app-zip` upgrade must be signed by (design §4.5, §8).
pub const ATERM_TEAM: &str = "A66A9P66Z7";

/// The journal is rotated past this size (one previous generation is kept).
const JOURNAL_ROTATE: usize = 1 << 20;

/// The longest an `exec` may run: a day.
const EXEC_BUDGET_MAX: u64 = 24 * 3600;

/// `agent run-job <run>`.
pub fn run_job(sys: &dyn System, layout: &Layout, exe: &Path, run: &str) -> ExitCode {
    run_job_at(sys, layout, exe, run, std::process::id())
}

// The supplied PID is the current process in shipping code; the pure fixture uses a
// virtual child identity. This does not bypass either the gate or holder admission.
fn run_job_at(sys: &dyn System, layout: &Layout, exe: &Path, run: &str, pid: u32) -> ExitCode {
    if sys.euid() != 0 {
        eprintln!("refused: run-job runs as root, started by apply");
        return ExitCode::from(super::REFUSED);
    }
    if !super::is_run_id(run) {
        eprintln!("refused: {run:?} is not a run id");
        return ExitCode::from(super::REFUSED);
    }
    if let Err(why) = sys.await_start(crate::port::START_LIMIT) {
        eprintln!("refused: run-job was not committed: {why}");
        return ExitCode::from(super::REFUSED);
    }
    let holder = match state::job_holder(sys, layout, run, pid) {
        Ok(holder) => holder,
        Err(why) => {
            eprintln!("refused: {why}");
            return ExitCode::from(super::REFUSED);
        }
    };
    let started = sys.now();
    let checked = chain::check(sys, exe)
        .map_err(|why| format!("the agent's own path is not root's alone: {why}"))
        .and_then(|()| accept(sys, layout, run));
    let (envelope, signer) = match checked {
        Ok(v) => v,
        Err(why) => {
            // Nothing is written into a run directory the job does not trust: the refusal
            // goes to the job's log, and `apply`'s stream reports a job without a report.
            eprintln!("refused: {why}");
            let _ = state::release_holder(sys, layout, &holder);
            return ExitCode::from(super::REFUSED);
        }
    };
    let mut progress = Progress::resume(sys, &layout.run_dir(run));
    // Once per run, with this job's own Full Disk Access (`HostFacts::full_disk_access`:
    // root's open of the system TCC database, never aterm's grant), which decides whether
    // an item may touch another app's container (`items::defaults::withheld`).
    let mut facts = crate::facts::gather(sys);
    facts.cached = super::hello::cached(sys, layout);
    let mut report = HostReport {
        schema: crate::SCHEMA,
        mode: Mode::Apply,
        serial: envelope.serial,
        key_id: signer.key_id.clone(),
        run_id: run.to_owned(),
        started,
        finished: started,
        facts: facts.clone(),
        items: Vec::new(),
        refused: None,
    };
    let outcome = match envelope.action {
        Action::Plan | Action::Apply => {
            report.mode = envelope.action.mode().unwrap_or(Mode::Plan);
            baseline_run(sys, layout, &envelope, facts, run, &mut progress)
        }
        Action::Facts => {
            report.mode = Mode::Plan;
            Ok(Vec::new())
        }
        Action::Exec => exec(sys, &envelope).map(|r| vec![r]),
        Action::Rekey => rekey(sys, layout, &envelope).map(|r| vec![r]),
        Action::Upgrade => upgrade(sys, layout, &envelope, run).map(|r| vec![r]),
        Action::Uninstall => Err("uninstall runs in the foreground of apply".into()),
    };
    match outcome {
        Ok(rows) => {
            for row in &rows {
                if !matches!(envelope.action, Action::Plan | Action::Apply) {
                    progress.push(&Line::Item(Box::new(row.clone())));
                }
            }
            report.items = rows;
        }
        Err(why) => report.refused = Some(why),
    }
    if matches!(envelope.action, Action::Apply) && report.refused.is_none() {
        record_managed(sys, layout, &report.items);
    }
    report.finished = sys.now();
    let failed = report.failed();
    finish(sys, layout, run, &holder, &report);
    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

/// The checks before the job trusts its run directory: root's, not writable by anyone
/// else, the envelope 0600 root-owned, and the run lock naming this run.
fn accept(sys: &dyn System, layout: &Layout, run: &str) -> Result<(Envelope, Signer), String> {
    let dir = layout.run_dir(run);
    for (path, want_dir) in [(dir.clone(), true), (dir.join("envelope"), false)] {
        let st = sys
            .stat(&path)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        if st.symlink || st.dir != want_dir || st.uid != 0 || st.mode & 0o022 != 0 {
            return Err(format!(
                "{} is not the agent's own (uid {}, mode {:o})",
                path.display(),
                st.uid,
                st.mode
            ));
        }
        if !want_dir && st.mode & 0o077 != 0 {
            return Err(format!("{} is readable by others", path.display()));
        }
    }
    match state::holder(sys, layout) {
        Some(h) if h.run == run => {}
        _ => return Err(format!("the run lock does not name run {run}")),
    }
    let wire = sys
        .read_file(&dir.join("envelope"))
        .map_err(|e| format!("envelope: {e}"))?;
    let (_, body) = envelope::split(&wire).map_err(|r| r.to_string())?;
    let envelope = envelope::parse_body(body).map_err(|r| r.to_string())?;
    let signer = sys
        .read_file(&dir.join("signer.toml"))
        .ok()
        .and_then(|b| aterm_toml::from_str::<Signer>(&String::from_utf8_lossy(&b)).ok())
        .unwrap_or(Signer {
            principal: String::new(),
            key_id: String::new(),
        });
    Ok((envelope, signer))
}

/// Write the report, record the run as the newest, and release the lock.
fn finish(
    sys: &dyn System,
    layout: &Layout,
    run: &str,
    holder: &state::Holder,
    report: &HostReport,
) {
    let dir = layout.run_dir(run);
    if let Ok(json) = aterm_json::to_string(report) {
        let _ = sys.write_file(&dir.join("report.json"), json.as_bytes(), 0o644);
    }
    state::set_last_run(sys, layout, run);
    let _ = state::release_holder(sys, layout, holder);
}

/// `progress.ndjson`, rewritten whole (atomically) after each line, so a reader never sees
/// half a line.
pub struct Progress<'a> {
    sys: &'a dyn System,
    path: PathBuf,
    text: String,
}

impl<'a> Progress<'a> {
    /// The run's progress, keeping what `apply` already wrote (its `started` line).
    #[must_use]
    pub fn resume(sys: &'a dyn System, dir: &Path) -> Self {
        let path = dir.join("progress.ndjson");
        let text = sys
            .read_file(&path)
            .map(|b| String::from_utf8_lossy(&b).into_owned())
            .unwrap_or_default();
        Self { sys, path, text }
    }

    /// Append one line.
    pub fn push(&mut self, line: &Line) {
        if let Ok(json) = aterm_json::to_string(line) {
            self.text.push_str(&json);
            self.text.push('\n');
            let _ = self.sys.write_file(&self.path, self.text.as_bytes(), 0o644);
        }
    }
}

/// `state/journal.ndjson`: one line per change, before the change is applied.
pub struct FileJournal<'a> {
    sys: &'a dyn System,
    path: PathBuf,
    run: String,
}

impl<'a> FileJournal<'a> {
    /// The journal under `layout`, for `run`.
    #[must_use]
    pub fn new(sys: &'a dyn System, layout: &Layout, run: &str) -> Self {
        Self {
            sys,
            path: layout.journal(),
            run: run.to_owned(),
        }
    }
}

impl Journal for FileJournal<'_> {
    fn before_change(
        &mut self,
        id: &str,
        from: &str,
        to: &str,
        _revert: Option<&str>,
    ) -> Result<(), String> {
        let mut text = match self.sys.read_file(&self.path) {
            Ok(b) => String::from_utf8_lossy(&b).into_owned(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(format!("{}: {e}", self.path.display())),
        };
        if text.len() > JOURNAL_ROTATE {
            let old = self.path.with_extension("1.ndjson");
            self.sys
                .rename(&self.path, &old)
                .map_err(|e| format!("{}: {e}", old.display()))?;
            text.clear();
        }
        let line = aterm_json::json!({
            "time": self.sys.now(), "run": self.run, "id": id, "from": from, "to": to,
        });
        text.push_str(&aterm_json::to_string(&line).map_err(|e| e.to_string())?);
        text.push('\n');
        self.sys
            .write_file(&self.path, text.as_bytes(), 0o600)
            .map_err(|e| format!("{}: {e}", self.path.display()))
    }
}

/// Whether a change may disrupt someone at the Mac now (design §5.3).
#[must_use]
pub fn may_disrupt(e: &Envelope, facts: &HostFacts) -> bool {
    match e.disrupt {
        Disrupt::Now => true,
        Disrupt::WhenIdle => {
            facts.console_user.is_none() || facts.idle_secs.is_some_and(|s| s >= e.idle_secs)
        }
    }
}

/// A plan or apply run of the envelope's baseline.
fn baseline_run(
    sys: &dyn System,
    layout: &Layout,
    e: &Envelope,
    facts: HostFacts,
    run: &str,
    progress: &mut Progress<'_>,
) -> Result<Vec<ItemReport>, String> {
    let text = e
        .baseline
        .as_deref()
        .ok_or_else(|| "the envelope carries no baseline".to_owned())?;
    let baseline = crate::baseline::parse(text)?;
    let mut users: Vec<User> = Vec::with_capacity(baseline.users.len());
    for name in &baseline.users {
        users.push(crate::facts::user(sys, name)?);
    }
    let console = facts
        .console_user
        .as_ref()
        .and_then(|c| crate::facts::user(sys, c).ok());
    let ctx = Ctx {
        disrupt: may_disrupt(e, &facts),
        facts,
        users,
        console,
        host_name: e.host_name.clone(),
        cache_dir: layout.cache(),
    };
    let list = items::expand(&baseline, &ctx)?;
    let mode = e.action.mode().unwrap_or(Mode::Plan);
    let mut journal = FileJournal::new(sys, layout, run);
    let remedy = Remedy::Command(format!("aterm machines apply {}", e.host_name));
    engine::run_with(&list, mode, sys, &ctx, &remedy, &mut journal, &mut |row| {
        progress.push(&Line::Item(Box::new(row.clone())))
    })
    .map_err(|refused| refused.to_string())
}

/// Remember every item id an apply run touched (design §4.2 step 5's `managed.toml`).
fn record_managed(sys: &dyn System, layout: &Layout, rows: &[ItemReport]) {
    let Ok(mut m) = state::managed(sys, layout) else {
        return;
    };
    let before = m.items.len();
    for r in rows {
        if !m.items.contains(&r.id) {
            m.items.push(r.id.clone());
        }
    }
    if m.items.len() != before {
        let _ = state::save_managed(sys, layout, &m);
    }
}

fn row(id: &str, family: &str, title: String) -> ItemReport {
    ItemReport {
        id: id.to_owned(),
        family: family.to_owned(),
        title,
        outcome: Outcome::Changed,
        from: None,
        to: None,
        why: None,
        revert: None,
        effect: Effect::Now,
        detail: None,
        layer: None,
        class: None,
        explain: None,
        stood_aside: None,
        essential: false,
    }
}

/// The last `max` bytes of `text`, on a character boundary.
fn tail(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut start = text.len() - max;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    &text[start..]
}

/// The most of each output stream an `exec` row keeps (the end of it).
const EXEC_KEEP: usize = 64 << 10;

/// `exec` (design §4.6): one argv as root or as a named user, no shell.
///
/// The row is how the run travels back, and [`exec_ran`] reads it back: `detail` is
/// standard output and `why` standard error (each its last 64 KiB, trailing whitespace
/// trimmed, absent when empty); `to` is `exit <n>`, or `killed` when a signal ended it.
/// A failed run that printed nothing on standard error says so in `why` (`exit 1`,
/// `killed by a signal`). A program that never ran (it could not start, it outran its
/// budget, the port refused it) has no `to`, and `why` says what happened. The outcome
/// is `changed` on exit 0 and `failed` otherwise.
fn exec(sys: &dyn System, e: &Envelope) -> Result<ItemReport, String> {
    let x = e
        .exec
        .as_ref()
        .ok_or_else(|| "an exec envelope carries no exec payload".to_owned())?;
    let Some((program, args)) = x.argv.split_first() else {
        return Err("exec: the argv is empty".into());
    };
    if !program.starts_with('/') {
        return Err(format!("exec: {program:?} is not an absolute path"));
    }
    // Bounded: `Instant + Duration` panics past the clock's range, which would kill the
    // job without a report.
    let mut cmd = Cmd::new(program)
        .args(args)
        .budget(Duration::from_secs(x.budget_secs.clamp(1, EXEC_BUDGET_MAX)));
    if let Some(name) = &x.user {
        cmd = cmd.as_user(&crate::facts::user(sys, name)?);
    }
    let mut r = row("exec", "exec", cmd.display());
    match sys.run(&cmd) {
        Ok(ran) => {
            let kept = |bytes: &[u8]| {
                let text = String::from_utf8_lossy(bytes);
                let text = tail(text.trim_end(), EXEC_KEEP);
                (!text.is_empty()).then(|| text.to_owned())
            };
            r.detail = kept(&ran.stdout);
            r.to = Some(
                ran.status
                    .map_or_else(|| "killed".to_owned(), |s| format!("exit {s}")),
            );
            r.why = kept(&ran.stderr);
            if !ran.success() {
                r.outcome = Outcome::Failed;
                r.why = r.why.or_else(|| Some(ran.describe()));
            }
        }
        Err(e) => {
            r.outcome = Outcome::Failed;
            r.why = Some(e.to_string());
        }
    }
    Ok(r)
}

/// The run an [`exec`] row reports, rebuilt from it: the inverse the controller uses to
/// read a program it ran on a host as if it had run it itself (a person step's probe).
/// Standard output and error come back as the row kept them: their last 64 KiB, without
/// trailing whitespace. A failed run whose `why` is only how it ended (`exit 1`, `killed
/// by a signal`) printed nothing on standard error.
///
/// # Errors
/// One line when the row says the program never ran (its `why`), or is not an exec row.
pub fn exec_ran(row: &ItemReport) -> Result<Ran, String> {
    if row.family != "exec" {
        return Err(format!("{} is not an exec row", row.id));
    }
    let status = match row.to.as_deref() {
        None => {
            return Err(row
                .why
                .clone()
                .unwrap_or_else(|| "the program never ran".to_owned()));
        }
        Some("killed") => None,
        Some(to) => Some(
            to.strip_prefix("exit ")
                .and_then(|n| n.parse::<i32>().ok())
                .ok_or_else(|| format!("exec: {to:?} is not an exit status"))?,
        ),
    };
    let quiet = Ran {
        status,
        ..Ran::default()
    };
    let stderr = match &row.why {
        Some(why) if status != Some(0) && *why == quiet.describe() => Vec::new(),
        Some(why) => why.clone().into_bytes(),
        None => Vec::new(),
    };
    Ok(Ran {
        status,
        stdout: row.detail.clone().unwrap_or_default().into_bytes(),
        stderr,
    })
}

/// `rekey`: replace `allowed_signers` with a checked document (design §3.1). Floors stay,
/// so a principal that returns keeps its floor.
fn rekey(sys: &dyn System, layout: &Layout, e: &Envelope) -> Result<ItemReport, String> {
    let text = e
        .rekey
        .as_deref()
        .ok_or_else(|| "a rekey envelope carries no allowed_signers".to_owned())?;
    let principals = envelope::check_signers(text)?;
    let before = sys
        .read_file(&layout.allowed_signers())
        .ok()
        .and_then(|b| envelope::check_signers(&String::from_utf8_lossy(&b)).ok())
        .unwrap_or_default();
    // Before the new list can verify anything: a key it names under a new principal must
    // not start that principal at floor 0.
    state::inherit_floors(sys, layout, &principals)?;
    sys.write_file(&layout.allowed_signers(), text.as_bytes(), 0o644)
        .map_err(|e| format!("{}: {e}", layout.allowed_signers().display()))?;
    let mut r = row("agent.rekey", "agent", "allowed_signers".into());
    r.from = Some(before.join(", "));
    r.to = Some(principals.join(", "));
    Ok(r)
}

/// The agent executable inside an expanded archive, relative to `dir`: for a bundle,
/// `<name>.app/Contents/MacOS/aterm`, from `aterm.app` or else the one `.app` at the top
/// (`ship dev-app`'s `aterm (dev).app`, a renamed copy); else the one regular file at the
/// top (a dev binary). Enrollment runs the executable the controller names, and its
/// install refuses one this would not find, so every enrolled agent can be upgraded.
///
/// # Errors
/// One line when the archive holds none of these.
pub fn find_executable(sys: &dyn System, dir: &Path) -> Result<(PathBuf, bool), String> {
    let regular = |rel: &Path| sys.stat(&dir.join(rel)).is_ok_and(|s| !s.dir && !s.symlink);
    let inside = |app: &str| Path::new(app).join("Contents/MacOS/aterm");
    let release = inside("aterm.app");
    if regular(&release) {
        return Ok((release, true));
    }
    if let Ok(app) = crate::items::apps::one_app_in(sys, dir) {
        let exe = inside(&app);
        if regular(&exe) {
            return Ok((exe, true));
        }
    }
    let names = sys
        .list_dir(dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?;
    let files: Vec<&String> = names
        .iter()
        .filter(|n| !n.starts_with('.'))
        .filter(|n| regular(Path::new(n)))
        .collect();
    match files.as_slice() {
        [one] => Ok((PathBuf::from(one), false)),
        _ => Err(format!(
            "{} holds neither one .app with Contents/MacOS/aterm nor exactly one binary",
            dir.display()
        )),
    }
}

/// `upgrade` (design §4.5): the archive from the root cache, expanded in the stage,
/// checked (Developer ID for `app-zip`, the signed hash alone for `dev-zip`), asked its
/// version, then swapped into `agent/` with the old one kept in `agent.prev/`, and the
/// sudoers digest rewritten; the archive then leaves the cache. A failed check changes
/// nothing.
fn upgrade(
    sys: &dyn System,
    layout: &Layout,
    e: &Envelope,
    run: &str,
) -> Result<ItemReport, String> {
    let u = e
        .upgrade
        .as_ref()
        .ok_or_else(|| "an upgrade envelope carries no upgrade payload".to_owned())?;
    let sha = u.sha256.to_ascii_lowercase();
    let archive = layout.cached(&sha);
    match sys.sha256(&archive) {
        Ok(got) if got == sha => {}
        Ok(got) => return Err(format!("upgrade: the cached archive hashes to {got}")),
        Err(err) => return Err(format!("upgrade: {}: {err}", archive.display())),
    }
    let work = layout.stage().join(format!("upgrade-{run}"));
    let _ = super::remove_tree(sys, &work);
    sys.create_dir_all(&work, 0o700)
        .map_err(|err| format!("{}: {err}", work.display()))?;
    let expanded = work.join("payload");
    let ditto = sys
        .run(
            &Cmd::new("/usr/bin/ditto")
                .args(["-x", "-k"])
                .arg(archive.as_os_str())
                .arg(expanded.as_os_str())
                .budget(Duration::from_secs(600)),
        )
        .map_err(|err| format!("upgrade: ditto: {err}"))?;
    if !ditto.success() {
        return Err(format!("upgrade: ditto -x -k: {}", ditto.describe()));
    }
    own_tree(sys, &expanded).map_err(|why| format!("upgrade: {why}"))?;
    let (rel, bundle) = find_executable(sys, &expanded)?;
    match (u.kind.as_str(), bundle) {
        ("app-zip", true) => {
            // The bundle the executable is in, whatever it is named: the seal covers it.
            let name = rel.iter().next().map(PathBuf::from).unwrap_or_default();
            let app = expanded.join(&name);
            let requirement = format!(
                "=anchor apple generic and certificate leaf[subject.OU] = \"{ATERM_TEAM}\""
            );
            let ran = sys
                .run(
                    &Cmd::new("/usr/bin/codesign")
                        .args(["--verify", "--deep", "--strict", "-R"])
                        .arg(requirement)
                        .arg(app.as_os_str())
                        .budget(Duration::from_secs(300)),
                )
                .map_err(|err| format!("upgrade: codesign: {err}"))?;
            if !ran.success() {
                return Err(format!(
                    "upgrade: {} is not signed by {ATERM_TEAM}: {}",
                    name.display(),
                    ran.describe()
                ));
            }
        }
        ("dev-zip", _) => {}
        (kind, _) => {
            return Err(format!(
                "upgrade: kind {kind:?} with this archive is not one the agent installs"
            ));
        }
    }
    let new_exe = expanded.join(&rel);
    chain::check(sys, &new_exe)
        .map_err(|why| format!("upgrade: the new agent's path is not root's alone: {why}"))?;
    let said = sys
        .run(&Cmd::new(&new_exe).args(["machines", "agent", "version"]))
        .map_err(|err| format!("upgrade: the new agent did not run: {err}"))?;
    let version = said.out().trim().to_owned();
    if !said.success() || (!u.version.is_empty() && version != u.version) {
        return Err(format!(
            "upgrade: the new agent says {version:?} ({}), not {:?}",
            said.describe(),
            u.version
        ));
    }
    let digest = sys
        .sha256(&new_exe)
        .map_err(|err| format!("upgrade: {err}"))?;
    let sudoers =
        super::install::check_sudoers(sys, layout, &managed_admin(sys, layout)?, &digest)?;
    // The swap: agent → agent.prev, payload → agent.
    let old_target = sys.read_link(&layout.exe_link()).ok();
    let old_digest = old_target
        .as_ref()
        .and_then(|t| sys.sha256(t).ok())
        .unwrap_or_default();
    let _ = super::remove_tree(sys, &layout.previous());
    sys.rename(&layout.agent(), &layout.previous())
        .map_err(|err| format!("upgrade: {err}"))?;
    if let Err(err) = sys.rename(&expanded, &layout.agent()) {
        let _ = sys.rename(&layout.previous(), &layout.agent());
        return Err(format!("upgrade: {err}"));
    }
    let landed = super::install::link_exe(sys, layout, &layout.agent().join(&rel)).and_then(|()| {
        sys.write_file(Path::new(crate::SUDOERS_FILE), sudoers.as_bytes(), 0o440)
            .map_err(|err| format!("upgrade: {}: {err}", crate::SUDOERS_FILE))
    });
    if let Err(why) = landed {
        // The sudoers rule still pins the old agent's digest: a new agent left in place
        // would be refused every later envelope. Put the old one back.
        let _ = sys.rename(&layout.agent(), &work.join("failed"));
        let _ = sys.rename(&layout.previous(), &layout.agent());
        if let Some(target) = &old_target {
            let _ = super::install::link_exe(sys, layout, target);
        }
        return Err(format!("{why}; the previous agent was put back"));
    }
    let _ = super::remove_tree(sys, &work);
    // The archive did its one job: the new agent is in agent/ and the old one in
    // agent.prev/. Each build would otherwise leave tens of megabytes in the cache.
    let _ = sys.remove(&archive);
    let mut m = state::managed(sys, layout)?;
    let from = std::mem::replace(&mut m.agent_version, version.clone());
    m.payload_sha256 = sha;
    m.executable = Path::new("agent").join(&rel).to_string_lossy().into_owned();
    m.upgraded = sys.now();
    state::save_managed(sys, layout, &m)?;
    let mut r = row(
        "agent.upgrade",
        "agent",
        "aterm's helper on this Mac, switched to the controller's build".into(),
    );
    // Two builds of one version differ only by their executables' hashes.
    let build = |version: &str, digest: &str| match digest.get(..12) {
        Some(short) => format!("{version} ({short})"),
        None => version.to_owned(),
    };
    r.from = Some(build(&from, &old_digest));
    r.to = Some(build(&version, &digest));
    // `apply` upgrades any host whose agent is not the controller's own build (design
    // §4.5), so an older aterm's apply brings its own agent back.
    r.revert = Some(format!(
        "`aterm machines apply <host>` from the older aterm puts its agent back; the previous \
         agent stays in {} until the next upgrade",
        layout.previous().display()
    ));
    Ok(r)
}

/// Make an expanded archive root's: `ditto -x` as root keeps the owners the zip recorded
/// (measured in the VM: an archive made by uid 501 on the controller expanded as
/// `admin:wheel`), and a writable bit for anyone else would fail the path chain.
pub(crate) fn own_tree(sys: &dyn System, dir: &Path) -> Result<(), String> {
    for cmd in [
        Cmd::new("/usr/sbin/chown")
            .args(["-R", "root:wheel"])
            .arg(dir.as_os_str()),
        Cmd::new("/bin/chmod")
            .args(["-R", "go-w"])
            .arg(dir.as_os_str()),
        Cmd::new("/bin/chmod").arg("0755").arg(dir.as_os_str()),
    ] {
        let cmd = cmd.budget(Duration::from_secs(300));
        let ran = sys
            .run(&cmd)
            .map_err(|e| format!("{}: {e}", cmd.display()))?;
        if !ran.success() {
            return Err(format!("{}: {}", cmd.display(), ran.describe()));
        }
    }
    Ok(())
}

fn managed_admin(sys: &dyn System, layout: &Layout) -> Result<String, String> {
    Ok(state::managed(sys, layout)?.admin)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::port::{Fake, Ran, Stat};

    fn layout() -> Layout {
        Layout { root: "/A".into() }
    }

    fn body(action: Action) -> Envelope {
        Envelope {
            schema: 1,
            host: "U".into(),
            host_name: "lab-07".into(),
            serial: 5,
            issued: 1,
            expires: 2,
            action,
            disrupt: Disrupt::WhenIdle,
            idle_secs: 900,
            baseline: None,
            artifacts: Vec::new(),
            exec: None,
            upgrade: None,
            rekey: None,
        }
    }

    #[test]
    fn disruption_waits_for_an_idle_or_empty_console_unless_told_now() {
        let mut e = body(Action::Apply);
        let mut facts = HostFacts {
            console_user: Some("alice".into()),
            idle_secs: Some(60),
            ..HostFacts::default()
        };
        assert!(!may_disrupt(&e, &facts));
        facts.idle_secs = Some(901);
        assert!(may_disrupt(&e, &facts));
        facts.idle_secs = None;
        assert!(!may_disrupt(&e, &facts), "an unknown idle time is not idle");
        facts.console_user = None;
        assert!(may_disrupt(&e, &facts));
        facts.console_user = Some("alice".into());
        e.disrupt = Disrupt::Now;
        assert!(may_disrupt(&e, &facts));
    }

    #[test]
    fn the_journal_records_each_change_as_one_line_before_it_is_made() {
        let fake = Fake::root();
        let mut j = FileJournal::new(&fake, &layout(), "1-5");
        j.before_change("power.ac.sleep", "10", "0", None).unwrap();
        j.before_change("names.computer", "\"a\"", "\"b\"", None)
            .unwrap();
        let text = String::from_utf8(fake.written("/A/state/journal.ndjson").unwrap()).unwrap();
        let lines: Vec<aterm_json::Value> = text
            .lines()
            .map(|l| aterm_json::from_str(l).unwrap())
            .collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["id"].as_str(), Some("power.ac.sleep"));
        assert_eq!(lines[1]["from"].as_str(), Some("\"a\""));
        assert_eq!(lines[1]["run"].as_str(), Some("1-5"));
        assert_eq!(fake.mode_of("/A/state/journal.ndjson"), Some(0o600));
    }

    fn root_dir() -> Stat {
        Stat {
            uid: 0,
            gid: 0,
            mode: 0o755,
            dir: true,
            symlink: false,
            len: 0,
        }
    }

    #[test]
    fn the_job_refuses_a_run_directory_someone_else_could_have_written() {
        let fake = Fake::root();
        fake.stat_of(
            "/A/state/runs/1-5",
            Stat {
                uid: 501,
                ..root_dir()
            },
        );
        let err = accept(&fake, &layout(), "1-5").unwrap_err();
        assert!(err.contains("not the agent's own"), "{err}");
        // A world-readable envelope is not one apply wrote.
        let fake = Fake::root();
        fake.stat_of("/A/state/runs/1-5", root_dir());
        fake.file("/A/state/runs/1-5/envelope", b"x");
        let err = accept(&fake, &layout(), "1-5").unwrap_err();
        assert!(err.contains("readable by others"), "{err}");
    }

    #[test]
    fn the_job_refuses_a_run_the_lock_does_not_name() {
        let fake = Fake::root();
        fake.stat_of("/A/state/runs/1-5", root_dir());
        fake.stat_of(
            "/A/state/runs/1-5/envelope",
            Stat {
                mode: 0o600,
                dir: false,
                ..root_dir()
            },
        );
        fake.file("/A/state/run.lock", b"pid=9 run=2-6\n");
        let err = accept(&fake, &layout(), "1-5").unwrap_err();
        assert!(err.contains("does not name run 1-5"), "{err}");
    }

    fn exec_of(argv: &[&str]) -> Envelope {
        let mut e = body(Action::Exec);
        e.exec = Some(envelope::Exec {
            argv: argv.iter().map(|&a| a.to_owned()).collect(),
            user: None,
            budget_secs: 30,
        });
        e
    }

    #[test]
    fn exec_runs_one_absolute_argv_and_keeps_its_output_status_and_errors_apart() {
        let fake = Fake::root();
        fake.on("/usr/bin/sw_vers -buildVersion", Ran::ok("26A428\n"));
        let r = exec(&fake, &exec_of(&["/usr/bin/sw_vers", "-buildVersion"])).unwrap();
        assert_eq!(r.outcome, Outcome::Changed);
        assert_eq!(r.detail.as_deref(), Some("26A428"));
        assert_eq!(r.to.as_deref(), Some("exit 0"));
        assert_eq!(r.why, None);
        assert!(exec(&fake, &exec_of(&["sw_vers"])).is_err());
        fake.on(
            "/usr/bin/false",
            Ran {
                status: Some(1),
                stdout: b"half\n".to_vec(),
                stderr: b"no\nreally\n".to_vec(),
            },
        );
        let r = exec(&fake, &exec_of(&["/usr/bin/false"])).unwrap();
        assert_eq!(r.outcome, Outcome::Failed);
        assert_eq!(r.detail.as_deref(), Some("half"));
        assert_eq!(r.why.as_deref(), Some("no\nreally"));
        assert_eq!(r.to.as_deref(), Some("exit 1"));
        // Never started: no status, and why says what happened.
        let r = exec(&fake, &exec_of(&["/usr/bin/missing"])).unwrap();
        assert_eq!(r.outcome, Outcome::Failed);
        assert_eq!(r.to, None);
        assert!(r.why.is_some());
    }

    #[test]
    fn an_exec_row_reads_back_as_the_run_it_reports() {
        let fake = Fake::root();
        let denied = "Error: unable to open database \"/Library/Application Support/com.apple.TCC/TCC.db\": authorization denied";
        for (argv, ran) in [
            (["/usr/bin/sw_vers", "-buildVersion"], Ran::ok("26A428")),
            (
                ["/usr/bin/sqlite3", "TCC.db"],
                Ran {
                    status: Some(1),
                    stdout: b"partial".to_vec(),
                    stderr: denied.as_bytes().to_vec(),
                },
            ),
            // `git config --get` of an unset key: exit 1 and nothing printed at all.
            (["/usr/bin/git", "config"], Ran::exit(1, "")),
            (
                ["/bin/sleep", "60"],
                Ran {
                    status: None,
                    stdout: b"so far".to_vec(),
                    stderr: Vec::new(),
                },
            ),
        ] {
            let display = argv.join(" ");
            fake.on(&display, ran.clone());
            let row = exec(&fake, &exec_of(&argv)).unwrap();
            assert_eq!(exec_ran(&row), Ok(ran), "{display}: {row:?}");
        }
        assert_eq!(
            exec(&fake, &exec_of(&["/bin/sleep", "60"]))
                .unwrap()
                .to
                .as_deref(),
            Some("killed")
        );
        // A program that never ran has no run to read back.
        let row = exec(&fake, &exec_of(&["/usr/bin/missing"])).unwrap();
        assert_eq!(exec_ran(&row), Err(row.why.clone().unwrap()));
        let mut other = row;
        other.family = "power".into();
        assert!(exec_ran(&other).is_err());
    }

    #[test]
    fn rekey_refuses_an_allowed_signers_that_would_lock_the_fleet_out() {
        let fake = Fake::root();
        let mut e = body(Action::Rekey);
        e.rekey = Some("# nobody\n".into());
        assert!(rekey(&fake, &layout(), &e).is_err());
        assert!(fake.written("/A/allowed_signers").is_none());
        let blob = "AAAAC3NzaC1lZDI1NTE5AAAAIEsLWe7+eYemQuyP3+onlaAlCHNVLq1yojB4CPyPW4yR";
        e.rekey = Some(format!(
            "new@ctl namespaces=\"aterm-machines\" ssh-ed25519 {blob}\n"
        ));
        let r = rekey(&fake, &layout(), &e).unwrap();
        assert_eq!(r.to.as_deref(), Some("new@ctl"));
        assert_eq!(fake.mode_of("/A/allowed_signers"), Some(0o644));
    }

    #[test]
    fn a_key_rekeyed_under_a_new_principal_keeps_its_floor() {
        let fake = Fake::root();
        // `signing@ctl` has consumed serials up to 7.
        fake.file(
            "/A/state/floors.toml",
            b"[[floor]]\nprincipal = \"signing@ctl\"\nserial = 7\n",
        );
        let blob = "AAAAC3NzaC1lZDI1NTE5AAAAIEsLWe7+eYemQuyP3+onlaAlCHNVLq1yojB4CPyPW4yR";
        let mut e = body(Action::Rekey);
        e.rekey = Some(format!(
            "daily@ctl namespaces=\"aterm-machines\" ssh-ed25519 {blob}\n"
        ));
        rekey(&fake, &layout(), &e).unwrap();
        // Envelopes that key signed as signing@ctl (serials 1–7) do not replay as daily@ctl.
        let floors = state::floors_or_refuse(&fake, &layout()).unwrap();
        assert_eq!(state::floor_of(&floors, "daily@ctl"), 7);
        let calls = fake.calls();
        let raised = calls
            .iter()
            .position(|c| c == "write /A/state/floors.toml 644")
            .unwrap();
        let listed = calls
            .iter()
            .position(|c| c == "write /A/allowed_signers 644")
            .unwrap();
        assert!(
            raised < listed,
            "the floor rises before the new list can verify"
        );
    }

    #[test]
    fn a_bundle_is_found_by_its_executable_and_a_dev_archive_by_its_one_binary() {
        let fake = Fake::root();
        fake.file("/P/aterm.app/Contents/MacOS/aterm", b"x");
        assert_eq!(
            find_executable(&fake, Path::new("/P")).unwrap(),
            (PathBuf::from("aterm.app/Contents/MacOS/aterm"), true)
        );
        let fake = Fake::root();
        fake.file("/P/aterm-machines-dev", b"x");
        assert_eq!(
            find_executable(&fake, Path::new("/P")).unwrap(),
            (PathBuf::from("aterm-machines-dev"), false)
        );
        let fake = Fake::root();
        fake.file("/P/a", b"x").file("/P/b", b"y");
        assert!(find_executable(&fake, Path::new("/P")).is_err());
    }

    /// Seed `dir` as a directory the fake lists in its parent.
    fn listed_dir(fake: &Fake, dir: &str) {
        fake.file(dir, b"").stat_of(dir, root_dir());
    }

    #[test]
    fn a_bundle_of_any_name_is_found_when_it_is_the_only_one_and_holds_aterm() {
        // `ship dev-app`'s bundle, as `ditto -c -k --keepParent` archives it.
        let fake = Fake::root();
        listed_dir(&fake, "/P/aterm (dev).app");
        fake.file("/P/aterm (dev).app/Contents/MacOS/aterm", b"x");
        assert_eq!(
            find_executable(&fake, Path::new("/P")).unwrap(),
            (PathBuf::from("aterm (dev).app/Contents/MacOS/aterm"), true)
        );
        // Two bundles, neither aterm.app: which one is the agent is not the agent's guess.
        listed_dir(&fake, "/P/Other.app");
        fake.file("/P/Other.app/Contents/MacOS/aterm", b"y");
        assert!(find_executable(&fake, Path::new("/P")).is_err());
        // One bundle without Contents/MacOS/aterm is not an agent.
        let fake = Fake::root();
        listed_dir(&fake, "/P/Other.app");
        fake.file("/P/Other.app/Contents/MacOS/other", b"x");
        assert!(find_executable(&fake, Path::new("/P")).is_err());
    }

    /// A host mid-upgrade: the archive cached, `ditto` expanded it into the stage with the
    /// new agent at `rel` inside it, and the expansion is root's (or not, per `exe_uid`).
    fn upgrading_at(rel: &str, exe_uid: u32) -> (Fake, Envelope) {
        let fake = Fake::root();
        let mut hasher = aterm_digest::Sha256::new();
        hasher.update(b"the archive");
        let sha = aterm_codec::hex::encode_lower(&hasher.finalize());
        fake.file(&format!("/A/cache/{sha}"), b"the archive");
        for d in [
            "/",
            "/A",
            "/A/stage",
            "/A/stage/upgrade-1-5",
            "/A/stage/upgrade-1-5/payload",
            "/A/agent",
            "/A/bin",
        ] {
            fake.stat_of(d, root_dir());
        }
        let payload = "/A/stage/upgrade-1-5/payload";
        let mut dir = PathBuf::from(payload);
        let parents: Vec<&str> = rel.split('/').collect();
        for (i, part) in parents[..parents.len() - 1].iter().enumerate() {
            dir.push(part);
            let d = dir.to_string_lossy().into_owned();
            if i == 0 {
                listed_dir(&fake, &d);
            } else {
                fake.stat_of(&d, root_dir());
            }
        }
        let exe = format!("{payload}/{rel}");
        fake.file(&exe, b"new agent");
        fake.stat_of(
            &exe,
            Stat {
                uid: exe_uid,
                dir: false,
                ..root_dir()
            },
        );
        fake.file(
            "/A/managed.toml",
            b"schema = 1\nadmin = \"admin\"\npayload_sha256 = \"\"\nagent_version = \"0.1.0\"\nexecutable = \"agent/aterm-machines-dev\"\ninstalled = 0\n",
        );
        fake.on(
            &format!("/usr/bin/ditto -x -k /A/cache/{sha} {payload}"),
            Ran::ok(""),
        );
        for c in [
            "/usr/sbin/chown -R root:wheel /A/stage/upgrade-1-5/payload",
            "/bin/chmod -R go-w /A/stage/upgrade-1-5/payload",
            "/bin/chmod 0755 /A/stage/upgrade-1-5/payload",
        ] {
            fake.on(c, Ran::ok(""));
        }
        fake.on(
            &format!("{} machines agent version", crate::port::shell_word(&exe)),
            Ran::ok(&format!("{}\n", env!("CARGO_PKG_VERSION"))),
        );
        fake.on(
            "/usr/sbin/visudo -cf /A/stage/sudoers.check",
            Ran::ok("/A/stage/sudoers.check: parsed OK\n"),
        );
        let mut e = body(Action::Upgrade);
        e.upgrade = Some(envelope::Upgrade {
            sha256: sha,
            kind: "dev-zip".into(),
            version: env!("CARGO_PKG_VERSION").into(),
        });
        (fake, e)
    }

    /// [`upgrading_at`] a dev binary.
    fn upgrading(exe_uid: u32) -> (Fake, Envelope) {
        upgrading_at("aterm-machines-dev", exe_uid)
    }

    fn cached_archive(e: &Envelope) -> String {
        format!("/A/cache/{}", e.upgrade.as_ref().unwrap().sha256)
    }

    #[test]
    fn an_upgrade_is_made_roots_and_checked_before_it_runs_and_before_the_swap() {
        let (fake, e) = upgrading(0);
        let r = upgrade(&fake, &layout(), &e, "1-5").unwrap();
        assert_eq!(r.outcome, Outcome::Changed);
        // The revert names where the previous agent is.
        let revert = r.revert.unwrap();
        assert!(revert.contains("/A/agent.prev"), "{revert}");
        let calls = fake.calls();
        let at = |c: &str| {
            calls
                .iter()
                .position(|x| x == c)
                .unwrap_or_else(|| panic!("{c}: {calls:#?}"))
        };
        let own = at("/usr/sbin/chown -R root:wheel /A/stage/upgrade-1-5/payload");
        let probe = at("/A/stage/upgrade-1-5/payload/aterm-machines-dev machines agent version");
        let check = at("/usr/sbin/visudo -cf /A/stage/sudoers.check");
        let keep = at("rename /A/agent /A/agent.prev");
        let swap = at("rename /A/stage/upgrade-1-5/payload /A/agent");
        let rule = at("write /etc/sudoers.d/aterm-machines 440");
        assert!(own < probe && probe < check && check < keep && keep < swap && swap < rule);
        let sudoers =
            String::from_utf8(fake.written("/etc/sudoers.d/aterm-machines").unwrap()).unwrap();
        let digest = fake
            .sha256(Path::new("/A/agent/aterm-machines-dev"))
            .unwrap();
        assert!(sudoers.contains(&format!("sha256:{digest} ")), "{sudoers}");
        // The archive is not kept: the cache would grow by one archive a build.
        assert!(rule < at(&format!("remove {}", cached_archive(&e))));
        assert!(fake.written(&cached_archive(&e)).is_none());
    }

    #[test]
    fn a_dev_bundle_upgrades_on_its_signed_hash_and_bin_aterm_points_into_it() {
        let rel = "aterm (dev).app/Contents/MacOS/aterm";
        let (fake, e) = upgrading_at(rel, 0);
        upgrade(&fake, &layout(), &e, "1-5").unwrap();
        let calls = fake.calls();
        assert!(
            calls.contains(&format!("symlink /A/bin/.aterm.new -> /A/agent/{rel}")),
            "{calls:#?}"
        );
        assert!(!calls.iter().any(|c| c.contains("codesign")), "{calls:#?}");
        assert_eq!(
            state::managed(&fake, &layout()).unwrap().executable,
            format!("agent/{rel}")
        );
    }

    #[test]
    fn an_app_zip_is_checked_against_the_team_on_the_bundle_it_holds() {
        let (fake, mut e) = upgrading_at("aterm (dev).app/Contents/MacOS/aterm", 0);
        e.upgrade.as_mut().unwrap().kind = "app-zip".into();
        let codesign = format!(
            "/usr/bin/codesign --verify --deep --strict -R {} {}",
            crate::port::shell_word(&format!(
                "=anchor apple generic and certificate leaf[subject.OU] = \"{ATERM_TEAM}\""
            )),
            crate::port::shell_word("/A/stage/upgrade-1-5/payload/aterm (dev).app"),
        );
        // A locally signed bundle: not the team's.
        fake.on(
            &codesign,
            Ran::exit(
                3,
                "test-requirement: code failed to satisfy specified code requirement(s)",
            ),
        );
        let err = upgrade(&fake, &layout(), &e, "1-5").unwrap_err();
        assert!(err.contains("aterm (dev).app is not signed by"), "{err}");
        let calls = fake.calls();
        assert!(calls.contains(&codesign), "{calls:#?}");
        assert!(!calls.iter().any(|c| c.starts_with("rename")), "{calls:#?}");
        // A refused upgrade keeps its archive: the next apply sends the same one.
        assert!(fake.written(&cached_archive(&e)).is_some());
    }

    #[test]
    fn an_upgrade_someone_else_owns_is_refused_and_the_old_agent_stays() {
        let (fake, e) = upgrading(501);
        let err = upgrade(&fake, &layout(), &e, "1-5").unwrap_err();
        assert!(err.contains("owned by uid 501"), "{err}");
        let calls = fake.calls();
        assert!(
            !calls.iter().any(|c| c.contains("machines agent version")),
            "{calls:#?}"
        );
        assert!(!calls.iter().any(|c| c.starts_with("rename")), "{calls:#?}");
        assert!(fake.written("/etc/sudoers.d/aterm-machines").is_none());
        assert!(fake.written(&cached_archive(&e)).is_some());
    }

    #[test]
    fn progress_keeps_the_started_line_apply_wrote() {
        let fake = Fake::root();
        fake.file(
            "/A/state/runs/1-5/progress.ndjson",
            b"{\"started\":{\"run\":\"1-5\"}}\n",
        );
        let mut p = Progress::resume(&fake, Path::new("/A/state/runs/1-5"));
        p.push(&Line::Refused { why: "x".into() });
        let text =
            String::from_utf8(fake.written("/A/state/runs/1-5/progress.ndjson").unwrap()).unwrap();
        assert_eq!(text.lines().count(), 2);
        assert!(text.starts_with("{\"started\""));
    }
}

#[cfg(test)]
#[path = "job_start_tests.rs"]
mod job_start_tests;
