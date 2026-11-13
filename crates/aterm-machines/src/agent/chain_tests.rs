// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::port::{Fake, Stat};

fn dir(uid: u32, mode: u32) -> Stat {
    Stat {
        uid,
        gid: 0,
        mode,
        dir: true,
        symlink: false,
        len: 0,
    }
}

fn file(uid: u32, mode: u32) -> Stat {
    Stat {
        uid,
        gid: 0,
        mode,
        dir: false,
        symlink: false,
        len: 1,
    }
}

fn installed() -> Fake {
    let fake = Fake::root();
    for d in [
        "/",
        "/Library",
        "/Library/Management",
        "/Library/Management/aterm",
        "/Library/Management/aterm/bin",
        "/Library/Management/aterm/agent",
    ] {
        fake.stat_of(d, dir(0, 0o755));
    }
    fake.stat_of(
        "/Library/Management/aterm/agent/aterm-machines-dev",
        file(0, 0o755),
    );
    fake.link(
        "/Library/Management/aterm/bin/aterm",
        "/Library/Management/aterm/agent/aterm-machines-dev",
    );
    fake
}

const EXE: &str = "/Library/Management/aterm/bin/aterm";

#[test]
fn the_installed_agent_through_its_root_link_passes() {
    check(&installed(), Path::new(EXE)).unwrap();
    check(
        &installed(),
        Path::new("/Library/Management/aterm/agent/aterm-machines-dev"),
    )
    .unwrap();
}

#[test]
fn a_directory_another_account_may_write_fails_the_chain() {
    let fake = installed();
    fake.stat_of("/Library/Management", dir(0, 0o775));
    let err = check(&fake, Path::new(EXE)).unwrap_err();
    assert!(err.contains("/Library/Management is writable"), "{err}");
    let fake = installed();
    fake.stat_of("/Library/Management/aterm/agent", dir(501, 0o755));
    let err = check(&fake, Path::new(EXE)).unwrap_err();
    assert!(err.contains("owned by uid 501"), "{err}");
}

#[test]
fn an_executable_others_could_replace_fails_the_chain() {
    let fake = installed();
    fake.stat_of(
        "/Library/Management/aterm/agent/aterm-machines-dev",
        file(0, 0o775),
    );
    assert!(check(&fake, Path::new(EXE)).is_err());
    let fake = installed();
    fake.stat_of(
        "/Library/Management/aterm/agent/aterm-machines-dev",
        file(501, 0o755),
    );
    assert!(check(&fake, Path::new(EXE)).is_err());
}

#[test]
fn a_link_anywhere_but_bin_aterm_fails_the_chain() {
    let fake = installed();
    fake.stat_of(
        "/Library/Management/aterm/agent",
        Stat {
            symlink: true,
            dir: false,
            ..dir(0, 0o755)
        },
    );
    let err = check(&fake, Path::new(EXE)).unwrap_err();
    assert!(err.contains("symbolic link"), "{err}");
    let fake = installed();
    fake.stat_of(
        EXE,
        Stat {
            uid: 501,
            gid: 20,
            mode: 0o755,
            dir: false,
            symlink: true,
            len: 0,
        },
    );
    let err = check(&fake, Path::new(EXE)).unwrap_err();
    assert!(err.contains("link owned by uid 501"), "{err}");
}

#[test]
fn a_link_through_dot_dot_fails_the_chain() {
    let fake = installed();
    fake.link(EXE, "../../../../Users//admin/aterm");
    assert!(check(&fake, Path::new(EXE)).is_err());
}
