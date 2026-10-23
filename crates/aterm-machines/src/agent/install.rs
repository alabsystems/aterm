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
    let how = format!(
        "to enroll it again, run `aterm machines unenroll <host>` from its controller, or at \
         this Mac `sudo rm -rf {} {}` and remove the fleet key's line (its comment starts \
         `aterm-machines:`) from {}/.ssh/authorized_keys, then enroll again",
        layout.root.display(),
        crate::SUDOERS_FILE,
        admin.home
    );
    match sys.stat(&signers) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Ok(_) => Err(format!(
            "this Mac is already enrolled ({} exists) and only a signed rekey changes who it \
             trusts; {how}",
            signers.display()
        )),
        Err(e) => Err(format!(
            "{}: {e}; whether this Mac is already enrolled cannot be told, so nothing was \
             changed; {how}",
            signers.display()
        )),
    }
}

/// Perform the agent installation for the resolved `admin`.
///
/// # Errors
/// One line naming why installation failed.
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
        agent_version: env!("CARGO_PKG_VERSION").to_owned(),
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
        env!("CARGO_PKG_VERSION")
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

/// Execute the `uninstall` envelope and stream the host report before removing the agent root.
pub fn uninstall(
    sys: &dyn System,
    layout: &Layout,
    exe: &Path,
    opened: &Opened,
    run: &str,
    out: &mut dyn Write,
) -> ExitCode {
    let started = sys.now();
    let facts = crate::facts::gather(sys);
    let mut rows: Vec<ItemReport> = Vec::new();
    let mut step = |id: &str, title: &str, done: Result<bool, String>| {
        let row = ItemReport {
            id: id.to_owned(),
            family: "agent".to_owned(),
            title: title.to_owned(),
            outcome: match done {
                Ok(true) => Outcome::Changed,
                Ok(false) => Outcome::Ok,
                Err(_) => Outcome::Failed,
            },
            from: None,
            to: None,
            why: done.err(),
            revert: None,
            effect: Effect::Now,
            detail: None,
            layer: None,
            class: None,
            explain: None,
            stood_aside: None,
            essential: false,
        };
        let _ = aterm_json::to_string(&Line::Item(Box::new(row.clone())))
            .map(|l| writeln!(out, "{l}").and_then(|()| out.flush()));
        rows.push(row);
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
    let _ = super::remove_tree(sys, &layout.root);
    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::port::{Fake, Ran, Stat};

    const BLOB: &str = "AAAAC3NzaC1lZDI1NTE5AAAAIEsLWe7+eYemQuyP3+onlaAlCHNVLq1yojB4CPyPW4yR";

    #[test]
    fn the_sudoers_rule_names_exactly_apply_pinned_by_digest() {
        let text = sudoers_text("admin", &"ab".repeat(32));
        let rule = text.lines().last().unwrap();
        assert_eq!(
            rule,
            format!(
                "admin ALL=(root) NOPASSWD: sha256:{} /Library/Management/aterm/bin/aterm machines agent apply",
                "ab".repeat(32)
            )
        );
        assert!(!text.contains('*'), "a wildcard would admit install");
        assert!(text.contains("Defaults!/Library/Management/aterm/bin/aterm !env_keep"));
    }

    #[test]
    fn visudo_must_accept_the_drop_in_before_it_is_used() {
        let fake = Fake::root();
        let layout = Layout { root: "/A".into() };
        fake.on(
            "/usr/sbin/visudo -cf /A/stage/sudoers.check",
            Ran {
                status: Some(1),
                stdout: b"/A/stage/sudoers.check:4:58: empty string\n".to_vec(),
                stderr: Vec::new(),
            },
        );
        let err = check_sudoers(&fake, &layout, "admin", &"ab".repeat(32)).unwrap_err();
        assert!(err.contains("empty string"), "{err}");
        assert!(fake.written("/A/stage/sudoers.check").is_none());
        assert!(check_sudoers(&fake, &layout, "ad min", &"ab".repeat(32)).is_err());
        assert!(check_sudoers(&fake, &layout, "admin", "*").is_err());
    }

    #[test]
    fn install_arguments_are_all_required_and_never_repeated() {
        let ok = [
            "--admin",
            "admin",
            "--fleet-key",
            "ssh-ed25519 AAAA fleet@ctl",
            "--allowed-signers",
            "/A/.staging/allowed_signers",
            "--payload-sha",
            "ab",
            "--stage",
            "/A/.staging/payload",
        ];
        let a = parse(&ok).unwrap();
        assert_eq!(a.admin, "admin");
        assert!(parse(&ok[..8]).is_err());
        let mut twice = ok.to_vec();
        twice.extend(["--admin", "root"]);
        assert!(parse(&twice).is_err());
        assert!(parse(&["--shell", "x"]).is_err());
    }

    #[test]
    fn the_fleet_key_line_is_restricted_and_replaces_its_own_earlier_line() {
        let line = fleet_line(&format!("ssh-ed25519 {BLOB} aterm-machines-fleet@ctl")).unwrap();
        assert!(line.starts_with("restrict,command=\"/Library/Management/aterm/bin/aterm machines agent ssh\" ssh-ed25519 "));
        assert!(line.ends_with(" aterm-machines:aterm-machines-fleet@ctl"));
        assert!(fleet_line(&format!("ssh-ed25519 {BLOB} a\"b")).is_err());
        assert!(fleet_line("command=\"/bin/sh\" ssh-ed25519 AAAA").is_err());
        let current = format!(
            "ssh-ed25519 OWNKEYAAAAAAAAAAAAAAAA admin@laptop\nrestrict,command=\"old\" ssh-ed25519 {BLOB} old\n"
        );
        let merged = merge_authorized_keys(&current, &line, BLOB);
        assert_eq!(merged.lines().count(), 2, "{merged}");
        assert!(merged.starts_with("ssh-ed25519 OWNKEY"));
        assert_eq!(merge_authorized_keys(&merged, &line, BLOB), merged);
        assert_eq!(
            strip_authorized_keys(&merged),
            "ssh-ed25519 OWNKEYAAAAAAAAAAAAAAAA admin@laptop\n"
        );
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

    fn staged() -> (Fake, Layout, Args) {
        let fake = Fake::root();
        let layout = Layout {
            root: "/Library/Management/aterm".into(),
        };
        for d in [
            "/",
            "/Library",
            "/Library/Management",
            "/Library/Management/aterm",
            "/Library/Management/aterm/.staging",
            "/Library/Management/aterm/.staging/payload",
            "/Library/Management/aterm/bin",
            "/Library/Management/aterm/state",
            "/Library/Management/aterm/state/runs",
            "/Library/Management/aterm/cache",
            "/Library/Management/aterm/stage",
            "/Library/Management/aterm/inbox",
            "/Library/Management/aterm/agent",
        ] {
            fake.stat_of(d, root_dir());
        }
        fake.file(
            "/Library/Management/aterm/.staging/payload/aterm-machines-dev",
            b"agent",
        );
        fake.stat_of(
            "/Library/Management/aterm/.staging/payload/aterm-machines-dev",
            Stat {
                dir: false,
                ..root_dir()
            },
        );
        fake.stat_of(
            "/Library/Management/aterm/agent/aterm-machines-dev",
            Stat {
                dir: false,
                ..root_dir()
            },
        );
        fake.file(
            "/Library/Management/aterm/.staging/allowed_signers",
            format!("signing@ctl namespaces=\"aterm-machines\" ssh-ed25519 {BLOB}\n").as_bytes(),
        );
        fake.file(
            "/Library/Management/aterm/.staging/payload.zip",
            b"the archive",
        );
        let payload_sha = fake
            .sha256(Path::new("/Library/Management/aterm/.staging/payload.zip"))
            .unwrap();
        let args = Args {
            admin: "admin".into(),
            fleet_key: format!("ssh-ed25519 {BLOB} aterm-machines-fleet@ctl"),
            allowed_signers: "/Library/Management/aterm/.staging/allowed_signers".into(),
            payload_sha,
            stage: "/Library/Management/aterm/.staging/payload".into(),
        };
        (fake, layout, args)
    }

    fn script_install(fake: &Fake) {
        for c in [
            "/usr/sbin/chown -R root:wheel /Library/Management/aterm/.staging/payload",
            "/bin/chmod -R go-w /Library/Management/aterm/.staging/payload",
            "/bin/chmod 0755 /Library/Management/aterm/.staging/payload",
        ] {
            fake.on(c, Ran::ok(""));
        }
        fake.on(
            "/usr/sbin/visudo -cf /Library/Management/aterm/stage/sudoers.check",
            Ran::ok("/Library/Management/aterm/stage/sudoers.check: parsed OK\n"),
        );
        fake.on(
            "admin$ /bin/cat /Users//admin/.ssh/authorized_keys",
            Ran::exit(
                1,
                "cat: /Users//admin/.ssh/authorized_keys: No such file or directory",
            ),
        );
        fake.on(
            "admin$ /Library/Management/aterm/bin/aterm machines agent put-file /Users//admin/.ssh/authorized_keys 600",
            Ran::ok("ok /Users//admin/.ssh/authorized_keys\n"),
        );
    }

    const STAGED_EXE: &str = "/Library/Management/aterm/.staging/payload/aterm-machines-dev";

    #[test]
    fn install_refuses_an_archive_it_cannot_hash_or_that_hashes_wrong() {
        let (fake, layout, args) = staged();
        script_install(&fake);
        let mut wrong = args.clone();
        wrong.payload_sha = "ab".repeat(32);
        let mut out = Vec::new();
        let err = install(
            &fake,
            &layout,
            Path::new(STAGED_EXE),
            &wrong,
            &admin(),
            &mut out,
        )
        .unwrap_err();
        assert!(err.contains("hashes to"), "{err}");
        let _ = fake.remove(Path::new("/Library/Management/aterm/.staging/payload.zip"));
        let err = install(
            &fake,
            &layout,
            Path::new(STAGED_EXE),
            &args,
            &admin(),
            &mut out,
        )
        .unwrap_err();
        assert!(err.contains("payload.zip"), "{err}");
        assert!(!fake.calls().iter().any(|c| c.starts_with("rename")));
    }

    #[test]
    fn a_reinstall_starts_a_renamed_principal_at_the_highest_floor() {
        let (fake, layout, args) = staged();
        script_install(&fake);
        fake.file(
            "/Library/Management/aterm/state/floors.toml",
            b"[[floor]]\nprincipal = \"old@ctl\"\nserial = 40\n",
        );
        let mut out = Vec::new();
        install(
            &fake,
            &layout,
            Path::new(STAGED_EXE),
            &args,
            &admin(),
            &mut out,
        )
        .unwrap_or_else(|e| panic!("{e}\n{}", String::from_utf8_lossy(&out)));
        let floors = state::floors_or_refuse(&fake, &layout).unwrap();
        assert_eq!(state::floor_of(&floors, "signing@ctl"), 40);
    }

    #[test]
    fn install_puts_the_staged_agent_in_place_and_writes_only_root_files_as_root() {
        let (fake, layout, args) = staged();
        for c in [
            "/usr/sbin/chown -R root:wheel /Library/Management/aterm/.staging/payload",
            "/bin/chmod -R go-w /Library/Management/aterm/.staging/payload",
            "/bin/chmod 0755 /Library/Management/aterm/.staging/payload",
        ] {
            fake.on(c, Ran::ok(""));
        }
        fake.on(
            "/usr/sbin/visudo -cf /Library/Management/aterm/stage/sudoers.check",
            Ran::ok("/Library/Management/aterm/stage/sudoers.check: parsed OK\n"),
        );
        fake.on(
            "admin$ /bin/cat /Users//admin/.ssh/authorized_keys",
            Ran::exit(
                1,
                "cat: /Users//admin/.ssh/authorized_keys: No such file or directory",
            ),
        );
        let pid = std::process::id();
        fake.on(
            "admin$ /Library/Management/aterm/bin/aterm machines agent put-file /Users//admin/.ssh/authorized_keys 600",
            Ran::ok("ok /Users//admin/.ssh/authorized_keys\n"),
        );
        let exe = Path::new("/Library/Management/aterm/.staging/payload/aterm-machines-dev");
        let mut out = Vec::new();
        install(&fake, &layout, exe, &args, &admin(), &mut out)
            .unwrap_or_else(|e| panic!("{e}\n{}", String::from_utf8_lossy(&out)));
        let calls = fake.calls();
        assert!(calls.contains(&"rename /Library/Management/aterm/.staging/payload /Library/Management/aterm/agent".to_owned()), "{calls:#?}");
        assert!(calls.iter().any(|c| c == "symlink /Library/Management/aterm/bin/.aterm.new -> /Library/Management/aterm/agent/aterm-machines-dev"), "{calls:#?}");
        let check = calls
            .iter()
            .position(|c| c.starts_with("/usr/sbin/visudo -cf"))
            .unwrap();
        let write = calls
            .iter()
            .position(|c| c == "write /etc/sudoers.d/aterm-machines 440")
            .unwrap();
        assert!(check < write);
        let sudoers =
            String::from_utf8(fake.written("/etc/sudoers.d/aterm-machines").unwrap()).unwrap();
        let sha = fake
            .sha256(Path::new(
                "/Library/Management/aterm/agent/aterm-machines-dev",
            ))
            .unwrap();
        assert!(
            sudoers.contains(&format!(
                "NOPASSWD: sha256:{sha} /Library/Management/aterm/bin/aterm machines agent apply"
            )),
            "{sudoers}"
        );
        assert_eq!(
            fake.mode_of("/Library/Management/aterm/allowed_signers"),
            Some(0o644)
        );
        assert!(fake.written("/Users//admin/.ssh/authorized_keys").is_none());
        assert!(
            calls.iter().all(|c| !c.starts_with("write /Users/")),
            "{calls:#?}"
        );
        assert!(
            fake.written(&format!("/Library/Management/aterm/stage/put-file.{pid}"))
                .is_none()
        );
        let managed = state::managed(&fake, &layout).unwrap();
        assert_eq!(managed.admin, "admin");
        assert_eq!(managed.executable, "agent/aterm-machines-dev");
    }

    #[test]
    fn uninstall_reports_every_step_before_it_removes_the_agent_itself() {
        let fake = Fake::root();
        let layout = Layout {
            root: "/Library/Management/aterm".into(),
        };
        fake.file("/Library/Management/aterm/managed.toml", b"schema = 1\nadmin = \"admin\"\npayload_sha256 = \"\"\nagent_version = \"\"\nexecutable = \"agent/x\"\ninstalled = 0\n");
        fake.file("/etc/sudoers.d/aterm-machines", b"rule");
        fake.stat_of("/Library/Management/aterm", root_dir());
        let opened = Opened {
            envelope: crate::envelope::parse_body(
                b"schema = 1\nhost = \"U\"\nhost_name = \"h\"\nserial = 9\nissued = 1\nexpires = 2\naction = \"uninstall\"\n",
            )
            .unwrap(),
            principal: "signing@ctl".into(),
            key_id: String::new(),
        };
        let mut out = Vec::new();
        let _ = uninstall(
            &fake,
            &layout,
            Path::new("/Library/Management/aterm/bin/aterm"),
            &opened,
            "1-9",
            &mut out,
        );
        let calls = fake.calls();
        assert!(
            calls.contains(&"remove /etc/sudoers.d/aterm-machines".to_owned()),
            "{calls:#?}"
        );
        assert_eq!(
            calls.last().map(String::as_str),
            Some("remove /Library/Management/aterm")
        );
        let text = String::from_utf8(out).unwrap();
        let last: Line = aterm_json::from_str(text.lines().last().unwrap()).unwrap();
        let Line::Report(report) = last else {
            panic!("{text}");
        };
        let ids: Vec<&str> = report.items.iter().map(|r| r.id.as_str()).collect();
        assert!(ids.contains(&"uninstall.sudoers"), "{ids:?}");
        assert!(ids.contains(&"uninstall.authorized-keys"), "{ids:?}");
        assert!(ids.contains(&"uninstall.inbox"), "{ids:?}");
        assert_eq!(report.serial, 9);
    }

    #[test]
    fn uninstall_says_ok_for_what_was_never_there_and_changed_only_for_what_it_removed() {
        let fake = Fake::root();
        let layout = Layout {
            root: "/Library/Management/aterm".into(),
        };
        fake.file("/Library/Management/aterm/managed.toml", b"schema = 1\nadmin = \"admin\"\npayload_sha256 = \"\"\nagent_version = \"\"\nexecutable = \"agent/x\"\ninstalled = 0\n");
        fake.file("/etc/sudoers.d/aterm-machines", b"rule");
        let admin = admin();
        fake.on(
            "admin$ /bin/rm -fv /Users//admin/.ssh/aterm_github_ed25519 /Users//admin/.ssh/aterm_github_ed25519.pub",
            Ran::ok(""),
        )
        .on(
            "admin$ /bin/rm -rfv /Users//admin/Library/Caches/aterm-machines",
            Ran::ok("/Users//admin/Library/Caches/aterm-machines\n"),
        )
        .on(
            "admin$ /bin/cat /Users//admin/.ssh/authorized_keys",
            Ran::ok("ssh-ed25519 OWNKEY admin@laptop\n"),
        );
        fake.on(
            "/usr/bin/dscl . -list /Users UniqueID",
            Ran::ok("admin                    501\nroot                     0\n"),
        )
        .on(
            "/usr/bin/dscl . -read /Groups/admin GroupMembership",
            Ran::ok("GroupMembership: root admin\n"),
        )
        .on(
            "/usr/bin/dscl . -read /Users//admin UniqueID PrimaryGroupID NFSHomeDirectory UserShell",
            Ran::ok(&format!(
                "NFSHomeDirectory: {}\nPrimaryGroupID: 20\nUniqueID: 501\nUserShell: /bin/zsh\n",
                admin.home
            )),
        );
        let opened = Opened {
            envelope: crate::envelope::parse_body(
                b"schema = 1\nhost = \"U\"\nhost_name = \"h\"\nserial = 9\nissued = 1\nexpires = 2\naction = \"uninstall\"\n",
            )
            .unwrap(),
            principal: "signing@ctl".into(),
            key_id: String::new(),
        };
        let mut out = Vec::new();
        let _ = uninstall(
            &fake,
            &layout,
            Path::new("/Library/Management/aterm/bin/aterm"),
            &opened,
            "1-9",
            &mut out,
        );
        let text = String::from_utf8(out).unwrap();
        let Line::Report(report) = aterm_json::from_str(text.lines().last().unwrap()).unwrap()
        else {
            panic!("{text}");
        };
        let outcome = |id: &str| {
            report
                .items
                .iter()
                .find(|r| r.id == id)
                .map(|r| r.outcome)
                .unwrap_or_else(|| panic!("{id}: {:#?}", report.items))
        };
        assert_eq!(outcome("uninstall.github-key@admin"), Outcome::Ok);
        assert_eq!(
            outcome("uninstall.authorized-keys"),
            Outcome::Ok,
            "no line of ours"
        );
        assert_eq!(outcome("uninstall.inbox"), Outcome::Changed);
        assert_eq!(outcome("uninstall.sudoers"), Outcome::Changed);
    }

    #[test]
    fn install_refuses_an_enrolled_mac_before_it_changes_anything() {
        let (fake, layout, args) = staged();
        script_install(&fake);
        fake.file(
            "/Library/Management/aterm/allowed_signers",
            format!("old@ctl namespaces=\"aterm-machines\" ssh-ed25519 {BLOB}\n").as_bytes(),
        );
        let mut out = Vec::new();
        let err = install(
            &fake,
            &layout,
            Path::new(STAGED_EXE),
            &args,
            &admin(),
            &mut out,
        )
        .unwrap_err();
        assert!(
            err.contains("/Library/Management/aterm/allowed_signers"),
            "{err}"
        );
        assert!(err.contains("`aterm machines unenroll <host>`"), "{err}");
        assert!(
            err.contains("`sudo rm -rf /Library/Management/aterm /etc/sudoers.d/aterm-machines`"),
            "{err}"
        );
        assert!(err.contains("/Users//admin/.ssh/authorized_keys"), "{err}");
        assert!(!err.contains('\n'), "one line: {err}");
        assert_eq!(fake.calls(), Vec::<String>::new(), "nothing ran or changed");
        assert!(out.is_empty());
        assert_eq!(
            fake.written("/Library/Management/aterm/allowed_signers")
                .unwrap(),
            format!("old@ctl namespaces=\"aterm-machines\" ssh-ed25519 {BLOB}\n").as_bytes()
        );
        let (fake, layout, args) = staged();
        script_install(&fake);
        fake.stat_fails(
            "/Library/Management/aterm/allowed_signers",
            std::io::ErrorKind::PermissionDenied,
        );
        let err = install(
            &fake,
            &layout,
            Path::new(STAGED_EXE),
            &args,
            &admin(),
            &mut out,
        )
        .unwrap_err();
        assert!(
            err.starts_with("/Library/Management/aterm/allowed_signers: "),
            "{err}"
        );
        assert_eq!(fake.calls(), Vec::<String>::new());
    }

    #[test]
    fn an_install_that_failed_before_the_end_can_be_run_again() {
        let (fake, layout, args) = staged();
        for c in [
            "/usr/sbin/chown -R root:wheel /Library/Management/aterm/.staging/payload",
            "/bin/chmod -R go-w /Library/Management/aterm/.staging/payload",
            "/bin/chmod 0755 /Library/Management/aterm/.staging/payload",
        ] {
            fake.on(c, Ran::ok(""));
        }
        fake.on(
            "/usr/sbin/visudo -cf /Library/Management/aterm/stage/sudoers.check",
            Ran::ok("/Library/Management/aterm/stage/sudoers.check: parsed OK\n"),
        );
        fake.on(
            "admin$ /bin/cat /Users//admin/.ssh/authorized_keys",
            Ran::exit(
                1,
                "cat: /Users//admin/.ssh/authorized_keys: Permission denied",
            ),
        );
        let mut out = Vec::new();
        let err = install(
            &fake,
            &layout,
            Path::new(STAGED_EXE),
            &args,
            &admin(),
            &mut out,
        )
        .unwrap_err();
        assert!(err.contains("Permission denied"), "{err}");
        assert!(
            fake.written("/Library/Management/aterm/allowed_signers")
                .is_none(),
            "a failed install left the Mac looking enrolled"
        );
        assert!(refuse_enrolled(&fake, &layout, &admin()).is_ok());
    }

    #[test]
    fn install_refuses_to_run_from_anywhere_but_the_stage() {
        let (fake, layout, args) = staged();
        let mut out = Vec::new();
        let err = install(
            &fake,
            &layout,
            Path::new("/Users//admin/aterm"),
            &args,
            &admin(),
            &mut out,
        )
        .unwrap_err();
        assert!(err.contains("runs from the staged agent"), "{err}");
        let mut other = args.clone();
        other.stage = "/tmp/payload".into();
        let exe = Path::new("/Library/Management/aterm/.staging/payload/aterm-machines-dev");
        assert!(install(&fake, &layout, exe, &other, &admin(), &mut out).is_err());
        let mut other = args;
        other.allowed_signers = "/Users//admin/allowed_signers".into();
        assert!(install(&fake, &layout, exe, &other, &admin(), &mut out).is_err());
        assert!(
            !fake
                .calls()
                .iter()
                .any(|c| c.starts_with("rename") || c.starts_with("write"))
        );
    }

    #[test]
    fn install_refuses_an_executable_an_upgrade_would_not_find_in_the_archive() {
        let (fake, layout, args) = staged();
        script_install(&fake);
        let _ = fake.remove(Path::new(STAGED_EXE));
        let file = Stat {
            dir: false,
            ..root_dir()
        };
        for root in [
            "/Library/Management/aterm/.staging/payload",
            "/Library/Management/aterm/agent",
        ] {
            for d in [
                "aterm.app",
                "aterm.app/Contents",
                "aterm.app/Contents/MacOS",
            ] {
                fake.file(&format!("{root}/{d}"), b"")
                    .stat_of(&format!("{root}/{d}"), root_dir());
            }
            for exe in ["aterm", "aterm-ctl"] {
                let path = format!("{root}/aterm.app/Contents/MacOS/{exe}");
                fake.file(&path, b"agent").stat_of(&path, file);
            }
        }
        let staged_exe = |name: &str| {
            PathBuf::from(format!(
                "/Library/Management/aterm/.staging/payload/aterm.app/Contents/MacOS/{name}"
            ))
        };
        let mut out = Vec::new();
        install(
            &fake,
            &layout,
            &staged_exe("aterm-ctl"),
            &args,
            &admin(),
            &mut out,
        )
        .unwrap_err();
        assert!(
            !fake.calls().iter().any(|c| c.starts_with("rename")
                || c.starts_with("mkdir")
                || c.starts_with("write")),
            "{:#?}",
            fake.calls()
        );
        install(
            &fake,
            &layout,
            &staged_exe("aterm"),
            &args,
            &admin(),
            &mut out,
        )
        .unwrap_or_else(|e| panic!("{e}\n{}", String::from_utf8_lossy(&out)));
        assert_eq!(
            state::managed(&fake, &layout).unwrap().executable,
            "agent/aterm.app/Contents/MacOS/aterm"
        );
    }
}
