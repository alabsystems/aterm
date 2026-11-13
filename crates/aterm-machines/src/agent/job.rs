// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Detached `agent run-job` execution, progress streaming, and report generation.

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

/// Developer ID team an `app-zip` upgrade must be signed by.
pub const ATERM_TEAM: &str = "A66A9P66Z7";

/// The journal is rotated past this size (one previous generation is kept).
const JOURNAL_ROTATE: usize = 1 << 20;

/// The longest an `exec` may run.
const EXEC_BUDGET_MAX: u64 = 24 * 3600;

/// `agent run-job <run>`.
pub fn run_job(sys: &dyn System, layout: &Layout, exe: &Path, run: &str) -> ExitCode {
    run_job_at(sys, layout, exe, run, std::process::id())
}

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
            eprintln!("refused: {why}");
            let _ = state::release_holder(sys, layout, &holder);
            return ExitCode::from(super::REFUSED);
        }
    };
    let mut progress = Progress::resume(sys, &layout.run_dir(run));
    let mut facts = if matches!(envelope.action, Action::Facts) {
        crate::facts::gather(sys)
    } else {
        crate::facts::gather_without_keyboards(sys)
    };
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
            let rows = baseline_run(sys, layout, &envelope, &mut facts, run, &mut progress);
            report.facts = facts.clone();
            rows
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

/// The checks before the job trusts its run directory.
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

/// `progress.ndjson`.
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

/// `state/journal.ndjson`.
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

impl FileJournal<'_> {
    /// Append `line` as one JSON line, moving a journal past [`JOURNAL_ROTATE`] aside first.
    fn append(&mut self, line: &aterm_json::Value) -> Result<(), String> {
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
        text.push_str(&aterm_json::to_string(line).map_err(|e| e.to_string())?);
        text.push('\n');
        self.sys
            .write_file(&self.path, text.as_bytes(), 0o600)
            .map_err(|e| format!("{}: {e}", self.path.display()))
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
        let line = aterm_json::json!({
            "time": self.sys.now(), "run": self.run, "id": id, "from": from, "to": to,
        });
        self.append(&line)
    }

    fn failed(&mut self, id: &str, why: &str) -> Result<(), String> {
        let line = aterm_json::json!({
            "time": self.sys.now(), "run": self.run, "id": id, "failed": why,
        });
        self.append(&line)
    }
}

/// Whether a change may disrupt someone at the Mac now.
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
    facts: &mut HostFacts,
    run: &str,
    progress: &mut Progress<'_>,
) -> Result<Vec<ItemReport>, String> {
    let text = e
        .baseline
        .as_deref()
        .ok_or_else(|| "the envelope carries no baseline".to_owned())?;
    let baseline = crate::baseline::parse(text)?;
    if baseline
        .keyboard
        .as_ref()
        .is_some_and(|table| table.caps_lock.is_some())
    {
        facts.keyboards = crate::facts::inventory_keyboards(sys);
    }
    let mut users: Vec<User> = Vec::with_capacity(baseline.users.len());
    for name in &baseline.users {
        users.push(crate::facts::user(sys, name)?);
    }
    let console = facts
        .console_user
        .as_ref()
        .and_then(|c| crate::facts::user(sys, c).ok());
    let ctx = Ctx {
        disrupt: may_disrupt(e, facts),
        facts: facts.clone(),
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

/// Records every item id an apply run touched in `managed.toml`.
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
        waits_on: None,
        essential: false,
        elapsed_ms: None,
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

/// Runs an `exec` job ([`exec_ran`]).
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

/// The run an [`exec`] row reports, rebuilt from it.
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
                .and_then(crate::parse_int::<i32>)
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

/// Replaces `allowed_signers` with a checked document.
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
/// top (a dev binary).
/// # Errors
/// One line when `dir` cannot be listed or holds neither one `.app` with `Contents/MacOS/aterm` nor
/// exactly one regular file.
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

/// The name of each upgrade's work folder under `stage/` starts with this (`upgrade-<run>`).
const UPGRADE_WORK: &str = "upgrade-";

/// Remove each upgrade's work folder under `stage/` that holds nothing back: nothing was
/// started from it, its program showed it ran, or its hold is spent
/// ([`crate::first_exec::hold_left`]). One still held stays for a later sweep.
pub(super) fn sweep_upgrades(sys: &dyn System, layout: &Layout) {
    let stage = layout.stage();
    for name in sys.list_dir(&stage).unwrap_or_default() {
        let work = stage.join(&name);
        if name.starts_with(UPGRADE_WORK)
            && sys.stat(&work).is_ok_and(|s| s.dir && !s.symlink)
            && crate::first_exec::hold_left(sys, &work).is_none()
        {
            let _ = super::remove_tree(sys, &work);
        }
    }
}

/// Installs an `upgrade` archive from the root cache.
fn upgrade(
    sys: &dyn System,
    layout: &Layout,
    e: &Envelope,
    run: &str,
) -> Result<ItemReport, String> {
    sweep_upgrades(sys, layout);
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
    let work = layout.stage().join(format!("{UPGRADE_WORK}{run}"));
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
    // Its first exec: `work` stays (uninstall too keeps it) until it shows it ran.
    crate::first_exec::stamp(sys, &work)
        .map_err(|err| format!("upgrade: {}: {err}", work.display()))?;
    let said = sys
        .run(
            &Cmd::new(&new_exe)
                .args(["machines", "agent", "version"])
                .budget(aterm_uds::deadline::FIRST_EXEC_HANG),
        )
        .map_err(|err| format!("upgrade: the new agent did not run: {err}"))?;
    if crate::first_exec::showed_it_ran(&said) {
        crate::first_exec::mark_ran(sys, &work);
    }
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
        let _ = sys.rename(&layout.agent(), &work.join("failed"));
        let _ = sys.rename(&layout.previous(), &layout.agent());
        if let Some(target) = &old_target {
            let _ = super::install::link_exe(sys, layout, target);
        }
        return Err(format!("{why}; the previous agent was put back"));
    }
    let _ = super::remove_tree(sys, &work);
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
    let build = |version: &str, digest: &str| match digest.get(..12) {
        Some(short) => format!("{version} ({short})"),
        None => version.to_owned(),
    };
    r.from = Some(build(&from, &old_digest));
    r.to = Some(build(&version, &digest));
    r.revert = Some(format!(
        "`aterm machines apply <host>` from the older aterm puts its agent back; the previous \
         agent stays in {} until the next upgrade",
        layout.previous().display()
    ));
    Ok(r)
}

/// Make an expanded archive root's.
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
#[path = "job_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "job_start_tests.rs"]
mod job_start_tests;
