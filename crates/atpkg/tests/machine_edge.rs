// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The `[machine]` settings at the REAL process edge: an update pass
//! applies them only when the `[machine]` table changed since the last apply, and never
//! under a private state root (ruling 409) nor when the window that spawned it says it runs
//! under one (ruling 410's lead), and their own verb applies them every time: a host
//! preference written at the dispatch edge of every update/seed/install pass is not that
//! edge's work (and the $HOME walk that once rode with it cost seconds).
//!
//! The observable is the synthetic-home refusal line: this child's `HOME` is a temp
//! directory, so an apply that is ATTEMPTED prints `atpkg: machine settings not applied —
//! …` (and touches nothing on the real machine), and one that is not attempted prints
//! nothing about the machine at all.
//! macOS only — elsewhere there is nothing to apply and nothing is ever printed.
//!
//! NEVER THE REAL STORE: `HOME` is a temp directory, `XDG_CONFIG_HOME` an absent one, and
//! the registry an empty local directory, so the pass finds an empty store and exits 0.

#![cfg(target_os = "macos")]

use aterm_uds::spawnfd::Stdio;
use std::path::PathBuf;

struct Fixture {
    root: aterm_tempfile::TempDir,
    prefix: PathBuf,
}

impl Fixture {
    fn new(case: &str) -> Self {
        let root = aterm_tempfile::Builder::new()
            .prefix(&format!("atpkg-machine-edge-{case}-"))
            .tempdir()
            .unwrap();
        std::fs::create_dir_all(root.path().join("home")).unwrap();
        std::fs::create_dir_all(root.path().join("registry")).unwrap();
        let prefix = atpkg::store::default_prefix(&root.path().join("home"));
        assert!(prefix.starts_with(root.path()), "{}", prefix.display());
        let layout = atpkg::store::Layout {
            prefix: prefix.clone(),
        };
        layout.ensure_dir(&prefix).unwrap();
        Self { root, prefix }
    }

    /// The owner's process: no private state root, whatever the test runner inherited.
    fn stdout(&self, args: &[&str]) -> Vec<String> {
        self.stdout_under(args, None)
    }

    /// `state_home`: the development seam `ATERM_STATE_HOME` a private instance runs under
    /// (ruling 409), set — never inherited.
    fn stdout_under(&self, args: &[&str], state_home: Option<&std::path::Path>) -> Vec<String> {
        let out = self.run(args, state_home);
        assert!(
            out.status.success(),
            "{args:?}: {}\n{}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
        Self::lines(&out.stdout)
    }

    /// The dev `atpkg` over this fixture, however it ends. Its working directory is the
    /// fixture's, so a relative seam names nothing outside it.
    fn run(&self, args: &[&str], state_home: Option<&std::path::Path>) -> std::process::Output {
        let mut cmd = aterm_uds::spawnfd::command(env!("CARGO_BIN_EXE_atpkg"));
        cmd.args(args)
            .current_dir(self.root.path())
            .env("HOME", self.root.path().join("home"))
            .env("XDG_CONFIG_HOME", self.root.path().join("config"))
            .env(
                "ATPKG_REGISTRY",
                format!("dir:{}", self.root.path().join("registry").display()),
            )
            .env_remove("ATERM_STATE_HOME")
            .stdin(Stdio::null());
        if let Some(state_home) = state_home {
            cmd.env("ATERM_STATE_HOME", state_home);
        }
        cmd.output().expect("run the dev atpkg")
    }

    fn lines(stdout: &[u8]) -> Vec<String> {
        String::from_utf8_lossy(stdout)
            .lines()
            .map(str::to_string)
            .collect()
    }

    /// Whether `lines` say anything about the machine settings.
    fn says_machine(lines: &[String]) -> bool {
        lines
            .iter()
            .any(|l| l.starts_with("atpkg: machine") || l.starts_with("atpkg machine:"))
    }

    fn attempted(lines: &[String]) -> bool {
        lines
            .iter()
            .any(|l| l.starts_with(atpkg::cli::MACHINE_NOT_APPLIED_PREFIX))
    }
}

/// Never applied: the pass carries the table (the first pass on a machine configures it).
/// Applied and unchanged: the pass says nothing about the machine. The verb: every time.
#[test]
fn an_update_pass_applies_the_machine_settings_only_when_the_table_changed() {
    let fx = Fixture::new("changed");
    assert!(
        Fixture::attempted(&fx.stdout(&["update"])),
        "never applied on this machine: the pass carries the table"
    );
    std::fs::write(
        fx.prefix.join("machine.applied"),
        atpkg::machine::applied_fingerprint(&atpkg::config::MachineConfig::default()),
    )
    .unwrap();
    let idle = fx.stdout(&["update"]);
    assert!(
        !idle
            .iter()
            .any(|l| l.starts_with("atpkg: machine") || l.starts_with("atpkg machine:")),
        "an unchanged table rides no pass: {idle:?}"
    );
    assert!(
        Fixture::attempted(&fx.stdout(&["machine", "apply"])),
        "the verb applies every time"
    );
}

/// A PASS UNDER A PRIVATE STATE ROOT CARRIES NO `[machine]` EDIT (ruling 409). The prefix
/// is shared with the owner, and `machine.applied` there is the owner's record, so a
/// development build given its own `ATERM_STATE_HOME` would read its own table as an edit
/// and apply it to the owner's Mac. Here the table was never applied — the case that makes
/// the owner's pass attempt it (the test above) — and the private pass attempts nothing,
/// records nothing, and leaves the edit for the owner's next pass. The typed verb still
/// applies (here, to the synthetic home's refusal: nothing on the real machine).
///
/// A RELATIVE OR EMPTY SEAM COUNTS (ruling 410): it resolves no root, but the process asked
/// for one of its own. Each value is driven here, so a helper that let only an absolute
/// path count — the natural "consistency with `aterm_data_dir`" edit — fails this test.
#[test]
fn a_pass_under_a_private_state_root_carries_no_machine_edit() {
    let fx = Fixture::new("private");
    let absolute = fx.root.path().join("aterm-state");
    for state in [
        absolute.as_path(),
        std::path::Path::new(""),
        std::path::Path::new("rel/state"),
    ] {
        let private = fx.stdout_under(&["update"], Some(state));
        assert!(
            !Fixture::says_machine(&private),
            "a private pass ({state:?}) says nothing about the machine: {private:?}"
        );
        assert!(
            !fx.prefix.join("machine.applied").exists(),
            "and ({state:?}) records nothing in the owner's prefix"
        );
    }
    assert!(
        Fixture::attempted(&fx.stdout_under(&["machine", "apply"], Some(&absolute))),
        "the typed verb still applies under a private state root"
    );
    assert!(
        Fixture::attempted(&fx.stdout(&["update"])),
        "the owner's next pass still carries the table"
    );
}

/// A PASS THE PRIVATE WINDOW LEADS WITH ITS FLAG CARRIES NO `[machine]` EDIT, WHATEVER THE
/// CHILD'S BUILD (ruling 410). The window's passes run the `atpkg` beside it, which in a
/// development tree is a separate build that may compile no seam: there the inherited
/// `ATERM_STATE_HOME` reads as unset. So the window puts
/// [`atpkg::cli::PRIVATE_STATE_ROOT_FLAG`] ahead of the verb, and the child honours it with
/// the seam unset — the seam-less child's view — over a never-applied table, the case that
/// makes the owner's pass attempt the apply. The flag is stripped before the verb reads its
/// operands (the pass exits 0), and the owner's next pass still carries the table.
#[test]
fn a_pass_led_by_the_private_flag_carries_no_machine_edit_with_the_seam_unset() {
    let fx = Fixture::new("lead");
    for args in [
        &[atpkg::cli::PRIVATE_STATE_ROOT_FLAG, "update"][..],
        &[
            atpkg::cli::PRIVATE_STATE_ROOT_FLAG,
            "update",
            "--wait-lock",
            "5",
        ][..],
    ] {
        let led = fx.stdout(args);
        assert!(
            !Fixture::says_machine(&led),
            "{args:?} says nothing about the machine: {led:?}"
        );
        assert!(
            !fx.prefix.join("machine.applied").exists(),
            "{args:?} records nothing in the owner's prefix"
        );
    }
    assert!(
        Fixture::attempted(&fx.stdout(&["update"])),
        "the owner's next pass still carries the table"
    );
}

/// AN `atpkg` THAT DOES NOT KNOW THE LEAD REFUSES IT BEFORE ITS MACHINE EDGE (ruling 410).
/// Every `atpkg` reads its first word as the verb, and the `[machine]` edge is asked only
/// for a pass verb, so a word it does not know ahead of the verb is an unknown verb: exit 2,
/// nothing on stdout, no store lock, no `[machine]` apply. That is what an `atpkg` built
/// before the flag does with the private window's lead — it fails the pass loudly instead
/// of applying the instance's table to the owner's Mac. Driven here with a word this build
/// does not know either, over the never-applied table.
#[test]
fn an_unknown_word_ahead_of_the_verb_is_refused_before_the_machine_edge() {
    let fx = Fixture::new("unknown-lead");
    let out = fx.run(&["--a-lead-this-build-does-not-know", "update"], None);
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    assert!(out.stdout.is_empty(), "{out:?}");
    assert!(
        !fx.prefix.join("machine.applied").exists(),
        "nothing was applied or recorded"
    );
}

/// A PASS THE STORE LOCK TURNS AWAY STILL CARRIES THE EDIT, AND HAS SAID SO WHEN IT EXITS.
/// The edge's apply needs no store lock, so it runs BESIDE the pass, on a thread started
/// before the lock is asked for, and the pass joins it before it returns — a refused pass
/// too. Here the test holds the store lock, so the `update` is refused at once (exit 75),
/// and the never-applied table's apply was still attempted: its synthetic-home refusal is
/// on stdout.
#[test]
fn a_pass_the_store_lock_refuses_still_carries_the_machine_edit() {
    let fx = Fixture::new("contended");
    let layout = atpkg::store::Layout {
        prefix: fx.prefix.clone(),
    };
    let held = atpkg::lock::try_lock_store(&layout).expect("the test holds the store lock");
    let out = fx.run(&["update"], None);
    drop(held);
    assert_eq!(
        out.status.code(),
        Some(i32::from(atpkg::lock::CONTENDED_EXIT)),
        "{out:?}"
    );
    assert!(
        Fixture::attempted(&Fixture::lines(&out.stdout)),
        "the refused pass still carried the never-applied table: {out:?}"
    );
}

/// A PASS EDGE DOES NOT WAIT OUT ANOTHER `[machine]` APPLY. On a first launch the window's
/// own `machine apply` holds `machine.lock` while the seed's edge would carry the same
/// never-applied table: the edge leaves it to that apply — and an edit it read too early to
/// the next pass's edge — so the seed exits while the queue is still held. An edge that
/// queued behind it held the seed, and the install the window starts behind it, until the
/// window's `defaults` writes were done. Here the test holds the queue for the seed's whole
/// life: the seed exits under the hang detector, and attempted nothing.
#[test]
fn a_pass_edge_does_not_wait_out_another_machine_apply() {
    let fx = Fixture::new("queue-held");
    let queue = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(fx.prefix.join("machine.lock"))
        .unwrap();
    #[expect(
        clippy::disallowed_methods,
        reason = "the test holds the [machine] queue for one child's life; closing it lets go"
    )]
    queue
        .try_lock()
        .expect("the test holds the [machine] queue");
    let mut child = aterm_uds::spawnfd::command(env!("CARGO_BIN_EXE_atpkg"))
        .arg("seed")
        .current_dir(fx.root.path())
        .env("HOME", fx.root.path().join("home"))
        .env("XDG_CONFIG_HOME", fx.root.path().join("config"))
        .env(
            "ATPKG_REGISTRY",
            format!("dir:{}", fx.root.path().join("registry").display()),
        )
        .env_remove("ATERM_STATE_HOME")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("run the dev atpkg");
    let until = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while child.try_wait().unwrap().is_none() {
        if std::time::Instant::now() >= until {
            let _ = child.kill();
            let _ = child.wait();
            panic!("hang detector: the seed waited for the [machine] queue another apply holds");
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let out = child.wait_with_output().unwrap();
    drop(queue);
    assert!(out.status.success(), "{out:?}");
    assert!(
        !Fixture::attempted(&Fixture::lines(&out.stdout)),
        "the apply that holds the queue carries the table: {out:?}"
    );
}
