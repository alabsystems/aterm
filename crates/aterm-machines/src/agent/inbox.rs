// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The two verbs that write as an ordinary account, never as root:
//!
//! * `put <sha256>`: the controller uploads an artifact into the admin's inbox
//!   (`~/Library/Caches/aterm-machines/inbox/<sha256>`); the bytes must hash to the name.
//!   The root agent copies it out and hashes its own copy again (design §8).
//! * `put-file <path> <mode>`: the root agent re-runs itself as a user to write a file in
//!   that user's home, so a link the user planted can never redirect a root write
//!   (design §3.5).

use crate::port::System;
use std::io::{Read, Write};
use std::path::{Component, Path};
use std::process::ExitCode;

/// The largest file `put-file` writes (keys, ssh config, authorized_keys: small text).
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

/// [`put`] for the resolved caller `me`.
pub fn put_as(
    sys: &dyn System,
    me: &crate::model::User,
    sha: &str,
    stdin: &mut dyn Read,
    out: &mut dyn Write,
) -> ExitCode {
    if !super::is_sha256(sha) {
        return super::refuse(out, "put takes a lowercase SHA-256 hex digest");
    }
    let dir = super::user_inbox(&me.home);
    if let Err(e) = sys.create_dir_all(&dir, 0o700) {
        return super::refuse(out, &format!("put: {}: {e}", dir.display()));
    }
    let partial = dir.join(format!(".{sha}.partial"));
    let target = dir.join(sha);
    if let Err(e) = sys.write_stream(&partial, stdin, 0o600) {
        let _ = sys.remove(&partial);
        return super::refuse(out, &format!("put: {}: {e}", partial.display()));
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
    let Ok(mode) = u32::from_str_radix(mode, 8) else {
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
    if !path.starts_with(home) || path == home {
        return super::refuse(
            out,
            &format!("put-file: {} is outside {}", path.display(), home.display()),
        );
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
    }

    #[test]
    fn put_keeps_bytes_only_when_they_hash_to_their_name() {
        let fake = as_admin();
        // SHA-256 of "hello".
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
}
