// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn history_keeps_cancelled_attempts_replaced_by_a_verdict() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let root = tmp.path();
    git(root, &["init", "--quiet"]).unwrap();
    git(root, &["config", "user.name", "Andrew Yates"]).unwrap();
    git(root, &["config", "user.email", "test@example.invalid"]).unwrap();
    git(root, &["remote", "add", "origin", "."]).unwrap();
    let now = aterm_time::unix_secs().unwrap();
    let mut row = Record {
        commit: "head".into(),
        tree: "tree".into(),
        host: "host".into(),
        verifier: String::new(),
        controller: String::new(),
        inputs: String::new(),
        token: "first".into(),
        state: "judging".into(),
        cause: String::new(),
        started: now,
        updated: now,
        lease_until: now + LEASE,
        rows: vec![],
    };
    let mut parent = None;
    for (token, state, delta) in [
        ("first", "judging", 0),
        ("first", "cancelled", 1),
        ("second", "judging", 2),
        ("second", "judging", 3),
        ("second", "pass", 4),
    ] {
        row.token = token.into();
        row.state = state.into();
        row.updated = now + delta;
        assert!(append(root, "origin", parent.as_deref(), &row, deadline()).unwrap());
        parent = head(root, "origin", deadline()).unwrap();
    }
    let head = parent.unwrap();
    let attempts = history(root, &head, now).unwrap();
    assert_eq!(
        attempts.len(),
        2,
        "heartbeats are not attempts; a retry does not erase cancellation"
    );
    assert!(
        attempts
            .iter()
            .any(|r| r.token == "first" && r.state == "cancelled")
    );
    assert!(
        attempts
            .iter()
            .any(|r| r.token == "second" && r.state == "pass")
    );
    assert_eq!(
        records(root, Some(&head), deadline()).unwrap().len(),
        1,
        "negative control: the tree snapshot really has overwritten the cancellation"
    );
    assert_eq!(history(root, &head, now + 2).unwrap().len(), 1);
}

#[cfg(unix)]
#[test]
fn renewal_interrupts_abort_only_scoped_journal_work() {
    const CHILD: &str = "ATERM_VERIFY_RENEWAL_INTERRUPT_CHILD";
    const TEST: &str = "judged::tests::renewal_interrupts_abort_only_scoped_journal_work";
    if std::env::var_os(CHILD).is_some() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let root = dir.path();
        git(root, &["init", "--quiet"]).unwrap();
        git(root, &["init", "--bare", "--quiet", "remote"]).unwrap();
        git(root, &["remote", "add", "origin", "remote"]).unwrap();
        let watch = aterm_toolchain::interrupt::Watch::enter();
        // SAFETY: raise sends this valid signal to this isolated child test process;
        // its live Watch owns the disposition and retains it for the controller.
        #[expect(
            clippy::disallowed_methods,
            reason = "this isolated child catches non-core SIGTERM in its live Watch to test journal cancellation; it must not die by SIGKILL"
        )]
        let raised = unsafe { libc::raise(libc::SIGTERM) };
        assert_eq!(raised, 0);
        let stopped = interruptible(|| output(&mut command(root, &["--version"]), None));
        assert!(stopped.unwrap_err().contains("interrupted"));
        let stopped_remote = interruptible(|| head(root, "origin", deadline()));
        assert!(stopped_remote.unwrap_err().contains("interrupted"));
        assert_eq!(watch.caught(), Some(libc::SIGTERM));
        // The terminal append's scope must work even with the same signal pending.
        assert!(
            output(&mut command(root, &["--version"]), None)
                .unwrap()
                .contains("git version")
        );
        assert_eq!(watch.take(), Some(libc::SIGTERM));
        return;
    }
    let mut cmd = aterm_uds::spawnfd::command(std::env::current_exe().unwrap());
    cmd.args(["--exact", TEST, "--nocapture"]).env(CHILD, "1");
    let out =
        aterm_uds::deadline::output_within(&mut cmd, std::time::Duration::from_secs(60)).unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn only_decided_verdicts_or_unexpired_claims_exclude_another_sample() {
    let mut r = Record {
        commit: "c".into(),
        tree: "t".into(),
        host: "h".into(),
        verifier: "v".into(),
        controller: String::new(),
        inputs: "i".into(),
        token: "one".into(),
        state: "judging".into(),
        cause: String::new(),
        started: 0,
        updated: 0,
        lease_until: 120,
        rows: vec![],
    };
    assert!(r.occupied(119));
    assert!(!r.occupied(120));
    for state in ["cancelled", "could-not-run"] {
        r.state = state.into();
        assert!(!r.occupied(1));
    }
    for state in ["pass", "red"] {
        r.state = state.into();
        assert!(r.occupied(1000));
    }
}

#[test]
fn competing_append_cannot_replace_a_claim_and_pruning_keeps_recent_rows() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let root = tmp.path();
    git(root, &["init", "--bare", "remote"]).unwrap();
    git(root, &["clone", "remote", "one"]).unwrap();
    let one = root.join("one");
    git(&one, &["config", "user.name", "Andrew Yates"]).unwrap();
    git(&one, &["config", "user.email", "test@example.invalid"]).unwrap();
    let mut r = Record {
        commit: "c".into(),
        tree: "t".into(),
        host: "h".into(),
        verifier: "v".into(),
        controller: String::new(),
        inputs: "i".into(),
        token: "one".into(),
        state: "judging".into(),
        cause: String::new(),
        started: 0,
        updated: 1,
        lease_until: 120,
        rows: vec![],
    };
    assert!(append(&one, "origin", None, &r, deadline()).unwrap());
    let first = head(&one, "origin", deadline()).unwrap().unwrap();
    assert!(
        append(
            &one,
            "origin",
            Some(&first),
            &r,
            std::time::Instant::now() - std::time::Duration::from_secs(1)
        )
        .is_err()
    );
    assert_eq!(
        head(&one, "origin", deadline()).unwrap().as_deref(),
        Some(first.as_str())
    );
    let blob = git(&one, &["rev-parse", &format!("{first}:verdicts/t.toml")]).unwrap();
    let input = format!("{blob}\n").repeat(3000);
    let objects = output(
        &mut command(&one, &["cat-file", "--batch"]),
        Some(input.as_bytes()),
    )
    .unwrap();
    assert_eq!(
        decode_objects(objects.as_bytes()).unwrap().len(),
        3000,
        "large journals must drain output while feeding more object requests"
    );
    r.token = "two".into();
    assert!(!append(&one, "origin", None, &r, deadline()).unwrap());
    assert_eq!(
        records(&one, Some(&first), deadline()).unwrap()[0].token,
        "one"
    );
    r.tree = "new".into();
    r.updated = RETENTION + 2;
    assert!(append(&one, "origin", Some(&first), &r, deadline()).unwrap());
    let last = head(&one, "origin", deadline()).unwrap().unwrap();
    assert_eq!(records(&one, Some(&last), deadline()).unwrap(), vec![r]);
}
