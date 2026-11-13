// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::ledger::RunOut;
use std::sync::Mutex;

struct FakeGit {
    answers: Mutex<Vec<(&'static str, RunOut)>>,
    calls: Mutex<Vec<String>>,
}

fn out(status: i32, stdout: &str) -> RunOut {
    RunOut {
        status,
        stdout: stdout.as_bytes().to_vec(),
        stderr: vec![],
    }
}

impl FakeGit {
    fn new(answers: Vec<(&'static str, RunOut)>) -> Self {
        Self {
            answers: Mutex::new(answers),
            calls: Mutex::new(Vec::new()),
        }
    }
}

impl GitRunner for FakeGit {
    fn git(&self, args: &[&str]) -> Result<RunOut> {
        let line = args.join(" ");
        self.calls.lock().expect("calls").push(line.clone());
        let mut answers = self.answers.lock().expect("answers");
        match answers.iter().position(|(needle, _)| line.contains(needle)) {
            Some(i) => Ok(answers.remove(i).1),
            None if args.contains(&"rev-parse") => Ok(out(0, &"1".repeat(40))),
            None => Ok(out(0, "")),
        }
    }
}

#[test]
fn a_submodule_off_its_gitlink_is_refused_and_named() {
    let git = FakeGit::new(vec![(
        "submodule status --recursive",
        out(
            0,
            " 1111111111111111111111111111111111111111 vendor/ok (v0)\n\
             -2222222222222222222222222222222222222222 vendor/astream\n\
             +3333333333333333333333333333333333333333 vendor/moved (v1-2-g3333333)\n",
        ),
    )]);
    let err = submodules_at_gitlinks(&git).expect_err("refused");
    let msg = err.to_string();
    assert!(
        msg.contains("-2222") && msg.contains("vendor/astream"),
        "{msg}"
    );
    assert!(
        msg.contains("+3333") && msg.contains("vendor/moved"),
        "{msg}"
    );
    assert!(!msg.contains("vendor/ok"), "{msg}");
}

#[test]
fn a_submodule_status_line_is_off_its_gitlink_unless_it_opens_with_a_space() {
    assert!(submodules_off_gitlink("").is_empty());
    assert!(submodules_off_gitlink(" abc vendor/astream (v0)\n\n").is_empty());
    assert_eq!(
        submodules_off_gitlink("-abc a\n+def b (x)\nUghi c\n jkl d\n"),
        vec!["-abc a", "+def b (x)", "Ughi c"]
    );
}

#[test]
fn aligning_updates_every_submodule_and_then_checks_the_result() {
    let git = FakeGit::new(vec![]);
    align_submodules(&git).expect("aligned");
    let calls = git.calls.lock().expect("calls").clone();
    let at = |needle: &str| {
        calls
            .iter()
            .position(|c| c.contains(needle))
            .unwrap_or_else(|| panic!("never asked {needle:?}: {calls:?}"))
    };
    assert!(
        at("submodule update --init --recursive --checkout") < at("submodule status --recursive"),
        "the update is checked, not trusted: {calls:?}"
    );
}

#[test]
fn a_submodule_the_cut_tree_cannot_check_out_fails_loudly() {
    let git = FakeGit::new(vec![(
        "submodule update",
        RunOut {
            status: 1,
            stdout: vec![],
            stderr: b"fatal: could not read Username for 'https://github.com'".to_vec(),
        },
    )]);
    align_submodules(&git).expect_err("a stale astream must not be built");
}

#[test]
fn only_a_pin_its_declared_branch_carries_is_released() {
    use crate::ledger::testing::{self, Git};
    let dir = aterm_tempfile::tempdir().unwrap();
    let ident = [
        ("user.name", "Test"),
        ("user.email", "test@example.invalid"),
        ("commit.gpgsign", "false"),
    ];
    let git = |at: &Path, args: &[&str]| {
        let out = testing::git()
            .arg("-C")
            .arg(at)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    let bus = dir.path().join("bus");
    testing::init(&bus, &ident);
    git(&bus, &["commit", "-q", "--allow-empty", "-m", "on main"]);
    git(&bus, &["checkout", "-q", "-b", "side"]);
    git(
        &bus,
        &["commit", "-q", "--allow-empty", "-m", "only on side"],
    );
    let side = git(&bus, &["rev-parse", "HEAD"]);
    git(&bus, &["checkout", "-q", "main"]);
    let top = dir.path().join("top");
    testing::init(&top, &ident);
    git(
        &top,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            "-q",
            "-b",
            "main",
            bus.to_str().unwrap(),
            "vendor/bus",
        ],
    );
    git(&top, &["commit", "-q", "-m", "pin the bus's main"]);
    let runner = Git::new(&top);
    submodules_on_declared_branch(&runner, &fixture_origin::FixtureOrigin(&runner))
        .expect("main carries the pin");

    git(&top.join("vendor/bus"), &["checkout", "-q", &side]);
    git(&top, &["commit", "-q", "-am", "pin the side branch"]);
    let off = submodules_on_declared_branch(&runner, &fixture_origin::FixtureOrigin(&runner))
        .expect_err("a side-branch pin is refused");
    assert!(matches!(off, PinRefusal::Off(_)), "{off}");

    git(&bus, &["tag", "main", &side]);
    git(&bus, &["checkout", "-q", "side"]);
    git(&bus, &["branch", "-D", "main"]);
    let unread = submodules_on_declared_branch(&runner, &fixture_origin::FixtureOrigin(&runner))
        .expect_err("a same-named tag cannot stand in for a missing branch");
    assert!(matches!(unread, PinRefusal::Unread(_)), "{unread}");
}

#[test]
fn an_unreadable_branch_refuses_and_an_undeclared_one_is_the_remotes_head() {
    let pin = "1".repeat(40);
    let listing = format!(
        "160000 commit {pin}\tvendor/astream\n100644 blob {}\tREADME.md\n",
        "2".repeat(40)
    );
    let fake = |extra: Vec<(&'static str, RunOut)>| {
        let mut answers = vec![
            ("ls-tree -r HEAD", out(0, &listing)),
            (
                "--get-regexp",
                out(0, "submodule.vendor/astream.path vendor/astream\n"),
            ),
            ("--get submodule.vendor/astream.branch", out(1, "")),
        ];
        answers.extend(extra);
        FakeGit::new(answers)
    };

    let git = fake(vec![]);
    submodules_on_declared_branch(&git, &fixture_origin::FixtureOrigin(&git))
        .expect("on the remote's HEAD");
    let calls = git.calls.lock().expect("calls").clone();
    assert!(
        calls.contains(&format!(
            "-C vendor/astream merge-base --is-ancestor {pin} {pin}"
        )),
        "{calls:?}"
    );

    let failed = || {
        (
            " fetch ",
            RunOut {
                status: 128,
                stdout: vec![],
                stderr: b"error: RPC failed; HTTP 504 curl 22".to_vec(),
            },
        )
    };
    let unfetched = fake(vec![failed()]);
    let refused =
        submodules_on_declared_branch(&unfetched, &fixture_origin::FixtureOrigin(&unfetched))
            .expect_err("no fetch, no verdict");
    assert!(matches!(refused, PinRefusal::Unread(_)), "{refused}");
    let fetches = unfetched
        .calls
        .lock()
        .expect("calls")
        .iter()
        .filter(|call| call.contains(" fetch "))
        .count();
    assert_eq!(
        fetches, 1,
        "the gate does not wrap its origin transport in another retry budget"
    );

    let absent = || {
        (
            " fetch ",
            RunOut {
                status: 128,
                stdout: vec![],
                stderr: b"fatal: unable to access 'https://github.com/o/r.git/': The requested URL returned error: 404".to_vec(),
            },
        )
    };
    let missing = fake(vec![absent()]);
    let refused = submodules_on_declared_branch(&missing, &fixture_origin::FixtureOrigin(&missing))
        .expect_err("404 is final");
    assert!(matches!(refused, PinRefusal::Unread(_)), "{refused}");
    let fetches = missing
        .calls
        .lock()
        .expect("calls")
        .iter()
        .filter(|c| c.contains(" fetch "))
        .count();
    assert_eq!(fetches, 1);

    let undecided = fake(vec![("merge-base", out(128, ""))]);
    let refused =
        submodules_on_declared_branch(&undecided, &fixture_origin::FixtureOrigin(&undecided))
            .expect_err("no ancestry, no verdict");
    assert!(matches!(refused, PinRefusal::Unread(_)), "{refused}");
}
