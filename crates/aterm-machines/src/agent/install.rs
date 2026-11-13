// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Enrollment `agent install` and `uninstall` envelope execution.

use super::state::{self, Managed};
use super::{Layout, chain};
use crate::envelope::Opened;
use crate::model::{Effect, HostReport, ItemReport, Mode, Outcome, User};
use crate::port::{Cmd, System};
use crate::protocol::Line;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// `install`'s arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Args {
    /// The admin account the fleet key is authorized for, and the sudoers rule names.
    pub admin: String,
    /// The fleet key's public line (`ssh-ed25519 <base64> <comment>`).
    pub fleet_key: String,
    /// The `allowed_signers` document to install.
    pub allowed_signers: PathBuf,
    /// SHA-256 of the agent archive.
    pub payload_sha: String,
    /// The expanded archive.
    pub stage: PathBuf,
}

/// Parse `install`'s arguments.
///
/// # Errors
/// One line naming what is missing or unknown.
pub fn parse(args: &[&str]) -> Result<Args, String> {
    let mut admin = None;
    let mut fleet_key = None;
    let mut signers = None;
    let mut sha = None;
    let mut stage = None;
    let mut it = args.iter();
    while let Some(flag) = it.next() {
        let slot = match *flag {
            "--admin" => &mut admin,
            "--fleet-key" => &mut fleet_key,
            "--allowed-signers" => &mut signers,
            "--payload-sha" => &mut sha,
            "--stage" => &mut stage,
            other => return Err(format!("install: unknown argument {other:?}")),
        };
        let Some(value) = it.next() else {
            return Err(format!("install: {flag} needs a value"));
        };
        if slot.replace((*value).to_owned()).is_some() {
            return Err(format!("install: {flag} given twice"));
        }
    }
    let need =
        |v: Option<String>, flag: &str| v.ok_or_else(|| format!("install: {flag} is required"));
    Ok(Args {
        admin: need(admin, "--admin")?,
        fleet_key: need(fleet_key, "--fleet-key")?,
        allowed_signers: PathBuf::from(need(signers, "--allowed-signers")?),
        payload_sha: need(sha, "--payload-sha")?,
        stage: PathBuf::from(need(stage, "--stage")?),
    })
}

/// Whether `name` is an account name sudoers and `sudo -u` take as one plain word.
#[must_use]
pub fn account_ok(name: &str) -> bool {
    let mut bytes = name.bytes();
    matches!(bytes.next(), Some(b'a'..=b'z' | b'_'))
        && name.len() <= 32
        && bytes.all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-' | b'.'))
}

/// The sudoers drop-in for the NOPASSWD apply command pinned by digest.
#[must_use]
pub fn sudoers_text(admin: &str, sha256: &str) -> String {
    let exe = crate::protocol::AGENT_EXE;
    format!(
        "# aterm machines: the one rule (docs/DESIGN-machines-2026-10-16.md §3.2).\n\
         # Written by `aterm machines agent install`; rewritten by an agent upgrade.\n\
         Defaults!{exe} always_set_home, env_reset, secure_path=\"/usr/bin:/bin:/usr/sbin:/sbin\"\n\
         Defaults!{exe} !env_keep\n\
         {admin} ALL=(root) NOPASSWD: sha256:{sha256} {exe} machines agent apply\n"
    )
}

/// Render the drop-in for `admin` and `sha256` and have `visudo -cf` check it.
///
/// # Errors
/// One line with visudo's complaint.
pub fn check_sudoers(
    sys: &dyn System,
    layout: &Layout,
    admin: &str,
    sha256: &str,
) -> Result<String, String> {
    if !account_ok(admin) {
        return Err(format!("{admin:?} is not a plain account name"));
    }
    if !super::is_sha256(sha256) {
        return Err(format!("{sha256:?} is not a SHA-256 digest"));
    }
    let text = sudoers_text(admin, sha256);
    let tmp = layout.stage().join("sudoers.check");
    sys.write_file(&tmp, text.as_bytes(), 0o440)
        .map_err(|e| format!("{}: {e}", tmp.display()))?;
    let ran = sys.run(&Cmd::new("/usr/sbin/visudo").arg("-cf").arg(tmp.as_os_str()));
    let _ = sys.remove(&tmp);
    match ran {
        Ok(r) if r.success() => Ok(text),
        Ok(r) => Err(format!(
            "visudo refused the drop-in: {}",
            r.out()
                .lines()
                .chain(r.err().lines())
                .collect::<Vec<_>>()
                .join(" / ")
        )),
        Err(e) => Err(format!("visudo: {e}")),
    }
}

/// Point `bin/aterm` at `target`: a new link beside it, renamed over the old one.
///
/// # Errors
/// One line.
pub fn link_exe(sys: &dyn System, layout: &Layout, target: &Path) -> Result<(), String> {
    let link = layout.exe_link();
    let tmp = layout.bin().join(".aterm.new");
    let _ = sys.remove(&tmp);
    sys.symlink(target, &tmp)
        .map_err(|e| format!("{}: {e}", tmp.display()))?;
    sys.rename(&tmp, &link)
        .map_err(|e| format!("{}: {e}", link.display()))
}

/// The fleet key's `authorized_keys` line, from its public line.
///
/// # Errors
/// One line when the key is not a plain public key.
pub fn fleet_line(fleet_key: &str) -> Result<String, String> {
    let (kind, blob) = crate::envelope::public_key_parts(fleet_key)
        .map_err(|why| format!("--fleet-key: {why}"))?;
    let comment = fleet_key.split_whitespace().nth(2).unwrap_or("fleet");
    if !comment
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'@' | b'.' | b'_' | b'-' | b':'))
    {
        return Err("--fleet-key: the comment is not a plain word".into());
    }
    Ok(crate::protocol::authorized_keys_line(
        &format!("{kind} {blob}"),
        &format!("aterm-machines:{comment}"),
    ))
}

/// `authorized_keys` with `line` in it once, replacing any line holding the same key (`blob`).
#[must_use]
pub fn merge_authorized_keys(current: &str, line: &str, blob: &str) -> String {
    let mut out = String::new();
    for l in current.lines() {
        if !blob.is_empty() && l.split_whitespace().any(|w| w == blob) {
            continue;
        }
        out.push_str(l);
        out.push('\n');
    }
    out.push_str(line);
    out.push('\n');
    out
}

/// `authorized_keys` without any line forcing the agent's command.
#[must_use]
pub fn strip_authorized_keys(current: &str) -> String {
    let forced = format!(
        "command=\"{} machines agent ssh\"",
        crate::protocol::AGENT_EXE
    );
    current
        .lines()
        .filter(|l| !l.contains(&forced))
        .map(|l| format!("{l}\n"))
        .collect()
}

fn read_as(sys: &dyn System, user: &User, path: &Path) -> Result<String, String> {
    let ran = sys
        .run(&Cmd::new("/bin/cat").arg(path.as_os_str()).as_user(user))
        .map_err(|e| format!("reading {} as {}: {e}", path.display(), user.name))?;
    if ran.success() {
        return Ok(ran.out());
    }
    if ran.err().contains("No such file") {
        return Ok(String::new());
    }
    Err(format!(
        "reading {} as {}: {}",
        path.display(),
        user.name,
        ran.describe()
    ))
}

fn put_as(
    sys: &dyn System,
    layout: &Layout,
    exe: &Path,
    user: &User,
    path: &Path,
    bytes: &[u8],
    mode: u32,
) -> Result<(), String> {
    let staged = layout
        .stage()
        .join(format!("put-file.{}", std::process::id()));
    sys.write_file(&staged, bytes, 0o600)
        .map_err(|e| format!("{}: {e}", staged.display()))?;
    let ran = sys.run(
        &Cmd::new(exe)
            .args(["machines", "agent", "put-file"])
            .arg(path.as_os_str())
            .arg(format!("{mode:o}"))
            .as_user(user)
            .stdin_file(&staged),
    );
    let _ = sys.remove(&staged);
    match ran {
        Ok(r) if r.success() => Ok(()),
        Ok(r) => Err(format!(
            "put-file as {}: {}",
            user.name,
            r.out().trim().lines().last().unwrap_or(&r.describe())
        )),
        Err(e) => Err(format!("put-file as {}: {e}", user.name)),
    }
}

/// `agent install …`.
pub fn main(
    sys: &dyn System,
    layout: &Layout,
    exe: &Path,
    args: &[&str],
    out: &mut dyn Write,
) -> ExitCode {
    // This staged agent's first exec is behind it: the next enrollment's root script may
    // clear the stage ([`crate::controller::enroll::root_script`]).
    if exe.starts_with(layout.staging()) {
        crate::first_exec::mark_ran(sys, &layout.staging());
    }
    let args = match parse(args) {
        Ok(a) => a,
        Err(why) => return super::refuse(out, &why),
    };
    if sys.euid() != 0 {
        return super::refuse(out, "install runs as root (enrollment runs it with sudo)");
    }
    let admin = match crate::facts::user(sys, &args.admin) {
        Ok(u) => u,
        Err(why) => return super::refuse(out, &format!("install: {why}")),
    };
    match install(sys, layout, exe, &args, &admin, out) {
        Ok(()) => ExitCode::SUCCESS,
        Err(why) => super::refuse(out, &format!("install: {why}")),
    }
}

fn refuse_enrolled(sys: &dyn System, layout: &Layout, admin: &User) -> Result<(), String> {
    let signers = layout.allowed_signers();
    let how = || {
        // An inventory that cannot be read holds as long as a stamp that cannot be.
        let wait = unproven(sys, layout)
            .map_or(Some(aterm_uds::deadline::FIRST_EXEC_HANG), |held| {
                held.iter().map(|(_, left)| *left).max()
            })
            .map(|left| {
                format!(
                    " once {} more minute(s) of awake time have passed, or after a restart (a \
                     program it holds has not shown it ran)",
                    crate::first_exec::minutes(left)
                )
            })
            .unwrap_or_default();
        format!(
            "to enroll it again, run `aterm machines unenroll <host>` from its controller, or at \
             this Mac `sudo rm -rf {} {}`{wait} and remove the fleet key's line (its comment \
             starts `aterm-machines:`) from {}/.ssh/authorized_keys, then enroll again",
            layout.root.display(),
            crate::SUDOERS_FILE,
            admin.home
        )
    };
    match sys.stat(&signers) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Ok(_) => Err(format!(
            "this Mac is already enrolled ({} exists) and only a signed rekey changes who it \
             trusts; {}",
            signers.display(),
            how()
        )),
        Err(e) => Err(format!(
            "{}: {e}; whether this Mac is already enrolled cannot be told, so nothing was \
             changed; {}",
            signers.display(),
            how()
        )),
    }
}

/// Perform the agent installation for the resolved `admin`.
///
/// # Errors
/// One line naming why installation failed; whatever was done before it stays (re-running
/// install finishes it, because `allowed_signers`, which makes the Mac enrolled, is written
/// last).
pub fn install(
    sys: &dyn System,
    layout: &Layout,
    exe: &Path,
    args: &Args,
    admin: &User,
    out: &mut dyn Write,
) -> Result<(), String> {
    refuse_enrolled(sys, layout, admin)?;
    if !account_ok(&args.admin) || admin.name != args.admin {
        return Err(format!("{:?} is not a plain account name", args.admin));
    }
    if !super::is_sha256(&args.payload_sha) {
        return Err("--payload-sha is not a lowercase SHA-256 digest".into());
    }
    let line = fleet_line(&args.fleet_key)?;
    let payload = layout.staging().join("payload");
    if args.stage != payload {
        return Err(format!(
            "--stage must be {} (where enrollment expanded the archive)",
            payload.display()
        ));
    }
    if !exe.starts_with(&payload) {
        return Err(format!(
            "install runs from the staged agent under {}, not {}",
            payload.display(),
            exe.display()
        ));
    }
    if !args.allowed_signers.starts_with(layout.staging()) {
        return Err(format!(
            "--allowed-signers must be the root-owned copy under {}",
            layout.staging().display()
        ));
    }
    let io = |p: &Path, e: std::io::Error| format!("{}: {e}", p.display());
    super::job::own_tree(sys, &payload)?;
    chain::check(sys, exe)?;
    let signers = sys
        .read_file(&args.allowed_signers)
        .map_err(|e| io(&args.allowed_signers, e))?;
    let signers = String::from_utf8(signers).map_err(|_| "allowed_signers is not UTF-8")?;
    let principals = crate::envelope::check_signers(&signers)?;
    let archive = layout.staging().join("payload.zip");
    match sys.sha256(&archive) {
        Ok(got) if got == args.payload_sha => {}
        Ok(got) => {
            return Err(format!(
                "{} hashes to {got}, not {}",
                archive.display(),
                args.payload_sha
            ));
        }
        Err(e) => return Err(io(&archive, e)),
    }
    let rel = exe
        .strip_prefix(&payload)
        .map_err(|_| "the staged executable is outside the stage")?
        .to_path_buf();
    let (found, _) = super::job::find_executable(sys, &payload)?;
    if found != rel {
        return Err(format!(
            "enrollment ran {}, but an upgrade would take {} from this archive",
            rel.display(),
            found.display()
        ));
    }
    for (dir, open) in [
        (layout.root.clone(), true),
        (layout.bin(), true),
        (layout.state(), true),
        (layout.runs(), true),
        (layout.cache(), true),
        (layout.stage(), false),
        (layout.inbox(), false),
    ] {
        sys.create_dir_all(&dir, if open { 0o755 } else { 0o700 })
            .map_err(|e| io(&dir, e))?;
        sys.chown(&dir, 0, 0).map_err(|e| io(&dir, e))?;
        let st = sys.stat(&dir).map_err(|e| io(&dir, e))?;
        if st.symlink || !st.dir || st.mode & 0o022 != 0 {
            return Err(format!(
                "{} is not a private directory (mode {:o})",
                dir.display(),
                st.mode
            ));
        }
        if open && st.mode & 0o005 != 0o005 {
            return Err(format!(
                "{} is mode {:o}; the admin's forced command needs it 0755",
                dir.display(),
                st.mode
            ));
        }
    }
    for dir in layout.root.ancestors().skip(1) {
        let st = sys.stat(dir).map_err(|e| io(dir, e))?;
        if st.mode & 0o001 == 0 {
            return Err(format!(
                "{} is mode {:o}; the admin's forced command cannot reach the agent through it",
                dir.display(),
                st.mode
            ));
        }
    }
    super::job::sweep_upgrades(sys, layout);
    if sys.stat(&layout.agent()).is_ok() {
        super::remove_tree(sys, &layout.previous()).map_err(|e| io(&layout.previous(), e))?;
        sys.rename(&layout.agent(), &layout.previous())
            .map_err(|e| io(&layout.agent(), e))?;
    }
    sys.rename(&payload, &layout.agent())
        .map_err(|e| io(&payload, e))?;
    let installed = layout.agent().join(&rel);
    link_exe(sys, layout, &installed)?;
    chain::check(sys, &layout.exe_link())?;
    let digest = sys.sha256(&installed).map_err(|e| io(&installed, e))?;
    let sudoers = check_sudoers(sys, layout, &args.admin, &digest)?;
    sys.write_file(Path::new(crate::SUDOERS_FILE), sudoers.as_bytes(), 0o440)
        .map_err(|e| io(Path::new(crate::SUDOERS_FILE), e))?;
    let _ = writeln!(
        out,
        "allowed aterm's helper, and only it, to make admin changes without a password"
    );
    let previous = state::managed(sys, layout).ok();
    let managed = Managed {
        schema: crate::SCHEMA,
        admin: args.admin.clone(),
        payload_sha256: args.payload_sha.clone(),
        agent_version: aterm_stamp::app_version().to_owned(),
        executable: Path::new("agent").join(&rel).to_string_lossy().into_owned(),
        installed: sys.now(),
        upgraded: 0,
        items: previous.map(|m| m.items).unwrap_or_default(),
    };
    state::save_managed(sys, layout, &managed)?;
    sys.write_file(&layout.never_index(), b"", 0o644)
        .map_err(|e| io(&layout.never_index(), e))?;
    let keys = Path::new(&admin.home).join(".ssh/authorized_keys");
    let current = read_as(sys, admin, &keys)?;
    let (_, blob) = crate::envelope::public_key_parts(&args.fleet_key)?;
    let merged = merge_authorized_keys(&current, &line, blob);
    put_as(
        sys,
        layout,
        &layout.exe_link(),
        admin,
        &keys,
        merged.as_bytes(),
        0o600,
    )?;
    let _ = writeln!(
        out,
        "this Mac now lets the controller in with its own key, which can only talk to \
         aterm's helper"
    );
    state::inherit_floors(sys, layout, &principals)?;
    sys.write_file(&layout.allowed_signers(), signers.as_bytes(), 0o644)
        .map_err(|e| io(&layout.allowed_signers(), e))?;
    let _ = writeln!(
        out,
        "this Mac now takes changes only from {}",
        signed_by(&principals)
    );
    let _ = super::remove_tree(sys, &layout.staging());
    let _ = writeln!(
        out,
        "installed aterm's helper {}",
        aterm_stamp::app_version()
    );
    Ok(())
}

fn signed_by(principals: &[String]) -> String {
    let mut hosts: Vec<&str> = Vec::new();
    for p in principals {
        let host = p.split_once('@').map_or(p.as_str(), |(_, h)| h);
        if !hosts.contains(&host) {
            hosts.push(host);
        }
    }
    match hosts.as_slice() {
        [one] => format!("the controller {one}"),
        _ => format!("the controllers {}", hosts.join(", ")),
    }
}

/// The folders under the agent root that hold a program that has not shown it ran
/// ([`crate::first_exec::hold_left`]): enrollment's stage and each upgrade's work folder, which
/// a program was started from, and the record that holds `/Applications/aterm.app` in place
/// ([`Layout::app_first_run`]), so an enrollment after this uninstall still waits it out. A
/// file in `stage/` (an envelope's or a sudoers check's scratch) is no such folder.
fn unproven(
    sys: &dyn System,
    layout: &Layout,
) -> Result<Vec<(PathBuf, std::time::Duration)>, String> {
    let stage = layout.stage();
    let io = |path: &Path, e: std::io::Error| format!("{}: {e}", path.display());
    let names = match sys.stat(&stage) {
        Ok(st) if st.dir && !st.symlink => sys.list_dir(&stage).map_err(|e| io(&stage, e))?,
        Ok(_) => Vec::new(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(io(&stage, e)),
    };
    let mut held = Vec::new();
    for dir in [layout.staging(), layout.app_first_run()]
        .into_iter()
        .chain(names.iter().map(|name| stage.join(name)))
    {
        let st = match sys.stat(&dir) {
            Ok(st) => st,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(io(&dir, e)),
        };
        if st.dir
            && !st.symlink
            && let Some(left) = crate::first_exec::hold_left(sys, &dir)
        {
            held.push((dir, left));
        }
    }
    Ok(held)
}

/// Execute the `uninstall` envelope and stream the host report before removing the agent
/// root, all but the folders [`unproven`] keeps, which its report names and a one-time
/// launchd job removes later ([`super::reclaim`]).
pub fn uninstall(
    sys: &dyn System,
    layout: &Layout,
    exe: &Path,
    opened: &Opened,
    run: &str,
    out: &mut dyn Write,
) -> ExitCode {
    let started = sys.now();
    let facts = crate::facts::gather_without_keyboards(sys);
    let mut rows: Vec<ItemReport> = Vec::new();
    let mut push = |id: &str, title: &str, outcome: Outcome, why: Option<String>| {
        let row = ItemReport {
            id: id.to_owned(),
            family: "agent".to_owned(),
            title: title.to_owned(),
            outcome,
            from: None,
            to: None,
            why,
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
        };
        let _ = aterm_json::to_string(&Line::Item(Box::new(row.clone())))
            .map(|l| writeln!(out, "{l}").and_then(|()| out.flush()));
        rows.push(row);
    };
    let mut step = |id: &str, title: &str, done: Result<bool, String>| {
        let outcome = match done {
            Ok(true) => Outcome::Changed,
            Ok(false) => Outcome::Ok,
            Err(_) => Outcome::Failed,
        };
        push(id, title, outcome, done.err());
    };
    for user in &facts.users {
        let key = Path::new(&user.home).join(".ssh/aterm_github_ed25519");
        let mut public = key.clone().into_os_string();
        public.push(".pub");
        let ran = sys.run(
            &Cmd::new("/bin/rm")
                .arg("-fv")
                .arg(key.as_os_str())
                .arg(public)
                .as_user(user),
        );
        let done = match ran {
            Ok(r) if r.success() => Ok(!r.out().trim().is_empty()),
            Ok(r) => Err(r.describe()),
            Err(e) => Err(e.to_string()),
        };
        step(
            &format!("uninstall.github-key@{}", user.name),
            &format!("aterm's GitHub key for {}", user.name),
            done,
        );
    }
    let admin = state::managed(sys, layout).and_then(|m| crate::facts::user(sys, &m.admin));
    let keys_done = admin.as_ref().map_err(Clone::clone).and_then(|admin| {
        let keys = Path::new(&admin.home).join(".ssh/authorized_keys");
        let current = read_as(sys, admin, &keys)?;
        let stripped = strip_authorized_keys(&current);
        if stripped != current {
            put_as(sys, layout, exe, admin, &keys, stripped.as_bytes(), 0o600)?;
        }
        Ok(stripped != current)
    });
    step(
        "uninstall.authorized-keys",
        "the controller's key, which let it in without a password",
        keys_done,
    );
    let inbox_done = admin.as_ref().map_err(Clone::clone).and_then(|admin| {
        let dir = Path::new(&admin.home).join("Library/Caches/aterm-machines");
        let ran = sys
            .run(
                &Cmd::new("/bin/rm")
                    .arg("-rfv")
                    .arg(dir.as_os_str())
                    .as_user(admin),
            )
            .map_err(|e| e.to_string())?;
        if ran.success() {
            Ok(!ran.out().trim().is_empty())
        } else {
            Err(ran.describe())
        }
    });
    step("uninstall.inbox", "aterm's upload folder", inbox_done);
    let rule = Path::new(crate::SUDOERS_FILE);
    let sudoers = match sys.stat(rule) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        _ => sys.remove(rule).map(|()| true).map_err(|e| e.to_string()),
    };
    step(
        "uninstall.sudoers",
        "the rule that let aterm's helper make admin changes",
        sudoers,
    );
    let mut kept = Vec::new();
    let held = match unproven(sys, layout) {
        Ok(held) => held,
        Err(why) => {
            kept.push(layout.root.clone());
            push(
                "uninstall.unproven",
                "folders of programs that have not shown they ran",
                Outcome::Failed,
                Some(format!(
                    "could not determine which folders are held ({why}); {} stays",
                    layout.root.display()
                )),
            );
            Vec::new()
        }
    };
    kept.extend(held.iter().map(|(dir, _)| dir.clone()));
    if let Some(left) = held.iter().map(|(_, left)| *left).max() {
        let list: Vec<String> = kept.iter().map(|dir| dir.display().to_string()).collect();
        let list = list.join(" ");
        let minutes = crate::first_exec::minutes(left);
        let (outcome, then) = match super::reclaim::schedule(sys, layout, run, &kept, left) {
            Ok(()) => (
                Outcome::Changed,
                format!(
                    "a one-time launchd job removes them, and then itself, after {minutes} \
                     more minute(s) of awake time or at the next restart"
                ),
            ),
            Err(why) => (
                Outcome::Person,
                format!(
                    "`sudo rm -rf {}` once {minutes} more minute(s) of awake time have passed, \
                     or after a restart; the job that removes them did not start ({why})",
                    layout.root.display()
                ),
            ),
        };
        push(
            "uninstall.unproven",
            "folders of programs that have not shown they ran",
            outcome,
            Some(format!(
                "{list} stay: a program they hold has not shown it ran, and macOS may \
                 still be checking it; {then}"
            )),
        );
    }
    let report = HostReport {
        schema: crate::SCHEMA,
        mode: Mode::Apply,
        serial: opened.envelope.serial,
        key_id: opened.key_id.clone(),
        run_id: run.to_owned(),
        started,
        finished: sys.now(),
        facts,
        items: rows,
        refused: None,
    };
    let failed = report.failed();
    let _ = aterm_json::to_string(&Line::Report(Box::new(report)))
        .map(|l| writeln!(out, "{l}").and_then(|()| out.flush()));
    let _ = super::remove_tree_keeping(sys, &layout.root, &kept);
    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

#[cfg(test)]
#[path = "install_tests.rs"]
mod tests;
