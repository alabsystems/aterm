// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

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
