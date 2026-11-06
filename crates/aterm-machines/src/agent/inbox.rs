// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Unprivileged `put` and `put-file` agent verbs for writing into user directories.

use crate::port::System;
use std::io::{Read, Write};
use std::path::{Component, Path};
use std::process::ExitCode;

/// Maximum payload size accepted by `put-file`.
const MAX_PUT_FILE: u64 = 16 << 20;

/// `agent put <sha256>`.
pub fn put(sys: &dyn System, sha: &str, stdin: &mut dyn Read, out: &mut dyn Write) -> ExitCode {
    if !super::is_sha256(sha) {
        return super::refuse(out, "put takes a lowercase SHA-256 hex digest");
    }
    if sys.euid() == 0 {
        return super::refuse(out, "put runs as the admin, never as root");
    }
    let me = match super::me(sys) {
        Ok(u) => u,
        Err(why) => return super::refuse(out, &format!("put: {why}")),
    };
    put_as(sys, &me, sha, stdin, out)
}

/// Ensure `home` is a non-root, dot-free absolute directory and neither `home` nor any existing
/// component along `target` inside `home` is a symbolic link or non-directory ancestor.
fn check_home_and_walk(
    sys: &dyn System,
    home: &Path,
    target: &Path,
    target_is_dir: bool,
) -> Result<(), String> {
    let valid_home = home.is_absolute()
        && home.parent().is_some()
        && !home
            .components()
            .any(|c| matches!(c, Component::ParentDir | Component::CurDir));
    if !valid_home {
        return Err(format!(
            "{} is not a non-root absolute home directory",
            home.display()
        ));
    }
    if !target.starts_with(home) || (!target_is_dir && target == home) {
        return Err(format!(
            "{} is outside {}",
            target.display(),
            home.display()
        ));
    }
    let rel = target
        .strip_prefix(home)
        .map_err(|_| format!("{} is outside {}", target.display(), home.display()))?;
    let check_component = |p: &Path, must_be_dir: bool| -> Result<(), String> {
        match sys.stat(p) {
            Ok(st) if st.symlink => Err(format!("{} is a symbolic link", p.display())),
            Ok(st) if must_be_dir && !st.dir => Err(format!("{} is not a directory", p.display())),
            Ok(_) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(format!("{}: {e}", p.display())),
        }
    };
    let mut cur = home.to_path_buf();
    check_component(&cur, true)?;
    let parts: Vec<Component<'_>> = rel.components().collect();
    for (idx, comp) in parts.iter().enumerate() {
        cur.push(comp.as_os_str());
        let is_last = idx + 1 == parts.len();
        check_component(&cur, target_is_dir || !is_last)?;
    }
    Ok(())
}

/// [`put`] for the resolved caller `me`: at most
/// [`MAX_ARTIFACT_BYTES`](crate::controller::artifacts::MAX_ARTIFACT_BYTES), the most the
/// controller downloads of one artifact.
pub fn put_as(
    sys: &dyn System,
    me: &crate::model::User,
    sha: &str,
    stdin: &mut dyn Read,
    out: &mut dyn Write,
) -> ExitCode {
    put_at_most(
        sys,
        me,
        sha,
        crate::controller::artifacts::MAX_ARTIFACT_BYTES,
        stdin,
        out,
    )
}

/// [`put_as`], refusing standard input longer than `cap` bytes with nothing kept.
fn put_at_most(
    sys: &dyn System,
    me: &crate::model::User,
    sha: &str,
    cap: u64,
    stdin: &mut dyn Read,
    out: &mut dyn Write,
) -> ExitCode {
    if !super::is_sha256(sha) {
        return super::refuse(out, "put takes a lowercase SHA-256 hex digest");
    }
    let home = Path::new(&me.home);
    let dir = super::user_inbox(&me.home);
    if let Err(why) = check_home_and_walk(sys, home, &dir, true) {
        return super::refuse(out, &format!("put: {why}"));
    }
    if let Err(e) = sys.create_dir_all(&dir, 0o700) {
        return super::refuse(out, &format!("put: {}: {e}", dir.display()));
    }
    let partial = dir.join(format!(".{sha}.partial"));
    let target = dir.join(sha);
    let mut capped = stdin.take(cap.saturating_add(1));
    match sys.write_stream(&partial, &mut capped, 0o600) {
        Ok(written) if written > cap => {
            let _ = sys.remove(&partial);
            return super::refuse(
                out,
                &format!("put: more than {cap} bytes on standard input"),
            );
        }
        Ok(_) => {}
        Err(e) => {
            let _ = sys.remove(&partial);
            return super::refuse(out, &format!("put: {}: {e}", partial.display()));
        }
    }
    match sys.sha256(&partial) {
        Ok(got) if got == sha => {}
        Ok(got) => {
            let _ = sys.remove(&partial);
            return super::refuse(out, &format!("put: the bytes hash to {got}, not {sha}"));
        }
        Err(e) => {
            let _ = sys.remove(&partial);
            return super::refuse(out, &format!("put: {e}"));
        }
    }
    if let Err(e) = sys.rename(&partial, &target) {
        let _ = sys.remove(&partial);
        return super::refuse(out, &format!("put: {}: {e}", target.display()));
    }
    let _ = writeln!(out, "ok {sha}");
    ExitCode::SUCCESS
}

/// `agent put-file <path> <mode-octal>`: standard input, atomically, into `path` inside
/// the caller's own home. Missing parent directories are made 0700.
pub fn put_file(
    sys: &dyn System,
    path: &Path,
    mode: &str,
    stdin: &mut dyn Read,
    out: &mut dyn Write,
) -> ExitCode {
    if sys.euid() == 0 {
        return super::refuse(
            out,
            "put-file writes as the user whose home it is, never as root",
        );
    }
    let Some(mode) = (!mode.is_empty() && mode.bytes().all(|b| matches!(b, b'0'..=b'7')))
        .then(|| u32::from_str_radix(mode, 8).ok())
        .flatten()
    else {
        return super::refuse(out, "put-file: the mode is not octal");
    };
    if mode & !0o777 != 0 {
        return super::refuse(out, "put-file: only permission bits (0777) may be set");
    }
    let plain = path.is_absolute()
        && !path
            .components()
            .any(|c| matches!(c, Component::ParentDir | Component::CurDir));
    if !plain {
        return super::refuse(out, "put-file: the path must be absolute, with no . or ..");
    }
    let me = match super::me(sys) {
        Ok(u) => u,
        Err(why) => return super::refuse(out, &format!("put-file: {why}")),
    };
    put_file_as(sys, &me, path, mode, stdin, out)
}

/// [`put_file`] for the resolved caller `me`, after the root and argument checks.
fn put_file_as(
    sys: &dyn System,
    me: &crate::model::User,
    path: &Path,
    mode: u32,
    stdin: &mut dyn Read,
    out: &mut dyn Write,
) -> ExitCode {
    let home = Path::new(&me.home);
    if let Err(why) = check_home_and_walk(sys, home, path, false) {
        return super::refuse(out, &format!("put-file: {why}"));
    }
    let mut bytes = Vec::new();
    if let Err(e) = stdin.take(MAX_PUT_FILE + 1).read_to_end(&mut bytes) {
        return super::refuse(out, &format!("put-file: standard input: {e}"));
    }
    if bytes.len() as u64 > MAX_PUT_FILE {
        return super::refuse(out, "put-file: more than 16 MiB on standard input");
    }
    if let Some(parent) = path.parent()
        && sys.stat(parent).is_err()
        && let Err(e) = sys.create_dir_all(parent, 0o700)
    {
        return super::refuse(out, &format!("put-file: {}: {e}", parent.display()));
    }
    match sys.write_file(path, &bytes, mode) {
        Ok(()) => {
            let _ = writeln!(out, "ok {}", path.display());
            ExitCode::SUCCESS
        }
        Err(e) => super::refuse(out, &format!("put-file: {}: {e}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::User;
    use crate::port::{Fake, Ran};

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

    fn as_admin() -> Fake {
        let mut fake = Fake::root();
        fake.euid = 501;
        fake.on("/usr/bin/id -un", Ran::ok("admin\n"));
        fake
    }

    fn refused() -> ExitCode {
        ExitCode::from(super::super::REFUSED)
    }

    #[test]
    fn put_file_and_put_refuse_to_run_as_root() {
        let fake = Fake::root();
        let mut out = Vec::new();
        let code = put_file(
            &fake,
            Path::new("/Users//admin/.ssh/authorized_keys"),
            "600",
            &mut std::io::empty(),
            &mut out,
        );
        assert_eq!(code, refused());
        assert!(String::from_utf8_lossy(&out).contains("never as root"));
        let mut out = Vec::new();
        let code = put(&fake, &"ab".repeat(32), &mut std::io::empty(), &mut out);
        assert_eq!(code, refused());
        assert!(fake.calls().is_empty(), "{:?}", fake.calls());
    }

    #[test]
    fn put_file_refuses_a_path_that_climbs_out_or_a_mode_beyond_permissions() {
        let fake = as_admin();
        for (path, mode) in [
            ("relative/file", "600"),
            ("/Users//admin/../root/x", "600"),
            ("/Users//admin/x", "4755"),
            ("/Users//admin/x", "rw"),
        ] {
            let mut out = Vec::new();
            let code = put_file(
                &fake,
                Path::new(path),
                mode,
                &mut std::io::empty(),
                &mut out,
            );
            assert_eq!(code, refused(), "{path} {mode}");
        }
        for path in ["/Users//administrator/x", "/Users//admin", "/etc/sudoers.d/x"] {
            let mut out = Vec::new();
            let code = put_file_as(
                &fake,
                &admin(),
                Path::new(path),
                0o600,
                &mut std::io::empty(),
                &mut out,
            );
            assert_eq!(code, refused(), "{path}");
        }
        assert!(!fake.calls().iter().any(|c| c.starts_with("write")));
    }

    #[test]
    fn put_file_makes_the_missing_parent_private_and_writes_with_the_mode() {
        let fake = as_admin();
        let mut out = Vec::new();
        let code = put_file_as(
            &fake,
            &admin(),
            Path::new("/Users//admin/.ssh/authorized_keys"),
            0o600,
            &mut &b"restrict ssh-ed25519 AAAA\n"[..],
            &mut out,
        );
        assert_eq!(code, ExitCode::SUCCESS, "{}", String::from_utf8_lossy(&out));
        assert_eq!(
            fake.calls(),
            [
                "mkdir /Users//admin/.ssh 700",
                "write /Users//admin/.ssh/authorized_keys 600"
            ]
        );
        assert_eq!(
            fake.written("/Users//admin/.ssh/authorized_keys").unwrap(),
            b"restrict ssh-ed25519 AAAA\n"
        );
        let mut out = Vec::new();
        assert_eq!(
            put_file(
                &fake,
                Path::new("/Users//admin/.ssh/authorized_keys"),
                "+600",
                &mut &b"x"[..],
                &mut out,
            ),
            refused()
        );
    }

    /// A put longer than its cap keeps nothing, partial file included. NEGATIVE CONTROL:
    /// exactly the cap is taken whole.
    #[test]
    fn put_refuses_more_than_its_cap_and_takes_exactly_the_cap() {
        let sha = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";
        let inbox = "/Users//admin/Library/Caches/aterm-machines/inbox";
        let fake = as_admin();
        let mut out = Vec::new();
        assert_eq!(
            put_at_most(&fake, &admin(), sha, 4, &mut &b"hello"[..], &mut out),
            refused()
        );
        assert!(
            String::from_utf8_lossy(&out).contains("more than 4 bytes"),
            "{}",
            String::from_utf8_lossy(&out)
        );
        assert!(fake.written(&format!("{inbox}/{sha}")).is_none());
        assert!(fake.written(&format!("{inbox}/.{sha}.partial")).is_none());

        let fake = as_admin();
        let mut out = Vec::new();
        assert_eq!(
            put_at_most(&fake, &admin(), sha, 5, &mut &b"hello"[..], &mut out),
            ExitCode::SUCCESS,
            "{}",
            String::from_utf8_lossy(&out)
        );
        assert_eq!(fake.written(&format!("{inbox}/{sha}")).unwrap(), b"hello");
    }

    #[test]
    fn put_keeps_bytes_only_when_they_hash_to_their_name() {
        let fake = as_admin();
        let sha = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";
        let mut out = Vec::new();
        assert_eq!(
            put_as(&fake, &admin(), sha, &mut &b"hello"[..], &mut out),
            ExitCode::SUCCESS
        );
        assert_eq!(String::from_utf8_lossy(&out), format!("ok {sha}\n"));
        let inbox = "/Users//admin/Library/Caches/aterm-machines/inbox";
        assert_eq!(fake.written(&format!("{inbox}/{sha}")).unwrap(), b"hello");
        let wrong = "00".repeat(32);
        let mut out = Vec::new();
        assert_eq!(
            put_as(&fake, &admin(), &wrong, &mut &b"hello"[..], &mut out),
            refused()
        );
        assert!(String::from_utf8_lossy(&out).contains("hash to 2cf24dba"));
        assert!(fake.written(&format!("{inbox}/{wrong}")).is_none());
        assert!(fake.written(&format!("{inbox}/.{wrong}.partial")).is_none());
    }

    /// DEGENERATE HOME AND SYMLINK ESCAPE ARE REFUSED: `me.home = "/"` cannot turn `starts_with`
    /// into root-wide write access, and a symbolic link along `home` or `path` is refused before
    /// reading `create_dir_all` or `write_file`.
    /// NEGATIVE CONTROL: a real directory inside a valid `/Users//admin` home still writes cleanly.
    #[test]
    fn put_file_and_put_refuse_root_home_and_symlink_ancestors() {
        let fake = as_admin();
        let mut root_home = admin();
        root_home.home = "/".into();
        let mut out = Vec::new();
        assert_eq!(
            put_file_as(
                &fake,
                &root_home,
                Path::new("/etc/sudoers.d/escape"),
                0o600,
                &mut &b"ALL ALL=(ALL) NOPASSWD: ALL\n"[..],
                &mut out,
            ),
            refused()
        );
        assert!(
            String::from_utf8_lossy(&out).contains("non-root absolute home"),
            "{}",
            String::from_utf8_lossy(&out)
        );

        let sha = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";
        let mut out = Vec::new();
        assert_eq!(
            put_as(&fake, &root_home, sha, &mut &b"hello"[..], &mut out),
            refused()
        );

        fake.symlink(Path::new("/tmp/outside"), Path::new("/Users//admin/.ssh"))
            .unwrap();
        let mut out = Vec::new();
        assert_eq!(
            put_file_as(
                &fake,
                &admin(),
                Path::new("/Users//admin/.ssh/authorized_keys"),
                0o600,
                &mut &b"ssh-ed25519 AAAA\n"[..],
                &mut out,
            ),
            refused()
        );
        assert!(
            String::from_utf8_lossy(&out).contains("symbolic link"),
            "{}",
            String::from_utf8_lossy(&out)
        );
        assert!(
            fake.written("/Users//admin/.ssh/authorized_keys").is_none(),
            "must never write through a symlinked parent"
        );

        let mut out = Vec::new();
        assert_eq!(
            put_file_as(
                &fake,
                &admin(),
                Path::new("/Users//admin/.config/ok.toml"),
                0o600,
                &mut &b"ok = true\n"[..],
                &mut out,
            ),
            ExitCode::SUCCESS
        );
        assert_eq!(
            fake.written("/Users//admin/.config/ok.toml").unwrap(),
            b"ok = true\n"
        );
    }
}
