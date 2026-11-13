// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

struct Leader {
    master: i32,
    child: std::process::Child,
}

impl Drop for Leader {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        // SAFETY: the master fd this test opened and alone owns.
        unsafe { libc::close(self.master) };
    }
}

fn leader() -> Leader {
    use std::os::fd::IntoRawFd as _;
    let (master, slave, child) = aterm_pty::spawn_session_leader_on_pty(|slave| {
        let mut cmd = std::process::Command::new("/bin/sleep");
        cmd.arg("30")
            .stdout(std::process::Stdio::from(slave.try_clone()?))
            .stderr(std::process::Stdio::from(slave.try_clone()?));
        Ok(cmd)
    })
    .expect("spawn sleep");
    drop(slave);
    let led = Leader {
        master: master.into_raw_fd(),
        child,
    };
    let pid = libc::pid_t::try_from(led.child.id()).expect("pid");
    aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "the child leads the pty's foreground",
        // SAFETY: `tcgetpgrp` only reads the live master's foreground group.
        || (unsafe { libc::tcgetpgrp(led.master) } == pid).then_some(()),
    );
    led
}

#[test]
fn rekey_is_owner_class_and_names_its_tab() {
    let store = crate::session_store::new_store();
    let tab = Some(Selector::parse("s-00000000000000000001"));
    assert_eq!(
        dispatch_rekey_verb(
            "shell=4242",
            tab.as_ref(),
            Scope::Edge(aterm_session::EdgeToken::generate()),
            &store
        ),
        "ERR denied\n"
    );
    assert_eq!(
        dispatch_rekey_verb("shell=4242", None, Scope::Owner, &store),
        REKEY_USAGE,
        "the bare form names no tab to heal"
    );
    assert_eq!(
        dispatch_rekey_verb("shell=4242", tab.as_ref(), Scope::Owner, &store),
        "ERR no such session\n"
    );
}

#[test]
fn rekey_is_issued_only_while_the_named_shell_leads_the_foreground() {
    let led = leader();
    let pid = led.child.id();
    let sid = aterm_session::SessionId::new("s-00000000000000000002");
    let mut healthy = Terminal::new(24, 80);
    healthy.set_require_shell_integration_nonce(true);
    healthy.authorize_shell_integration([3; 32]);
    healthy.process(
        format!(
            "\x1b]633;P;AtermIntegration={};id={}\x07",
            crate::shell_body::own_rev(),
            aterm_core::shell_integration::hex_encode(&[3; 32])
        )
        .as_bytes(),
    );
    let healthy = Arc::new(Mutex::new(healthy));
    let off = Arc::new(Mutex::new(Terminal::new(24, 80)));
    for bad in [
        "",
        "shell=",
        "shell=abc",
        "shell=1",
        "shell=5 more",
        "withdraw now",
        "4242",
    ] {
        assert_eq!(
            cmd_rekey(&healthy, led.master, &sid, bad),
            REKEY_USAGE,
            "{bad:?}"
        );
    }
    let other = pid + 1;
    assert_eq!(
        cmd_rekey(&healthy, led.master, &sid, &format!("shell={other}")),
        format!("ERR pid {other} is not the foreground process group ({pid})\n"),
    );
    assert_eq!(
        cmd_rekey(&healthy, -1, &sid, &format!("shell={pid}")),
        "ERR no foreground process group\n",
    );
    assert!(
        cmd_rekey(&healthy, led.master, &sid, &format!("shell={pid}"))
            .starts_with("ERR rekey integration=on"),
    );
    assert!(
        cmd_rekey(&off, led.master, &sid, &format!("shell={pid}"))
            .starts_with("ERR rekey integration=off"),
    );
    assert_eq!(
        cmd_rekey(&healthy, led.master, &sid, "withdraw"),
        "OK rekey none\n"
    );
}
