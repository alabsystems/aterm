// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::model::{Effect, HostFacts, ItemReport, Mode, Outcome};
use crate::port::{Fake, Ran, Stat};

fn layout() -> Layout {
    Layout { root: "/A".into() }
}

fn row(id: &str) -> ItemReport {
    ItemReport {
        id: id.into(),
        family: "power".into(),
        title: id.into(),
        outcome: Outcome::Ok,
        from: None,
        to: Some("0".into()),
        why: None,
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
    }
}

fn report(items: Vec<ItemReport>) -> HostReport {
    HostReport {
        schema: 1,
        mode: Mode::Apply,
        serial: 5,
        key_id: String::new(),
        run_id: "1-5".into(),
        started: 1,
        finished: 2,
        facts: HostFacts::default(),
        items,
        refused: None,
    }
}

fn lines(out: &[u8]) -> Vec<Line> {
    String::from_utf8_lossy(out)
        .lines()
        .map(|l| aterm_json::from_str(l).unwrap())
        .collect()
}

#[test]
fn follow_replays_the_progress_from_the_start_then_the_report() {
    let fake = Fake::root();
    let mut progress = String::new();
    for l in [
        Line::Started { run: "1-5".into() },
        Line::Item(Box::new(row("power.ac.sleep"))),
    ] {
        progress.push_str(&aterm_json::to_string(&l).unwrap());
        progress.push('\n');
    }
    progress.push_str("{\"item\":");
    fake.file("/A/state/runs/1-5/progress.ndjson", progress.as_bytes());
    fake.file(
        "/A/state/runs/1-5/report.json",
        aterm_json::to_string(&report(vec![row("power.ac.sleep")]))
            .unwrap()
            .as_bytes(),
    );
    let mut out = Vec::new();
    let code = follow(&fake, &layout(), "1-5", &mut out, Duration::ZERO);
    assert_eq!(code, ExitCode::SUCCESS);
    let got = lines(&out);
    assert_eq!(got.len(), 3, "{got:?}");
    assert!(matches!(&got[0], Line::Started { run } if run == "1-5"));
    assert!(matches!(&got[1], Line::Item(r) if r.id == "power.ac.sleep"));
    assert!(matches!(&got[2], Line::Report(r) if r.items.len() == 1));
}

#[test]
fn a_job_that_died_without_a_report_ends_the_stream_with_a_refusal() {
    let fake = Fake::root();
    fake.file(
        "/A/state/runs/1-5/progress.ndjson",
        b"{\"started\":{\"run\":\"1-5\"}}\n",
    );
    fake.file("/A/state/runs/1-5/job.pid", b"4242\n");
    let mut out = Vec::new();
    let code = follow(&fake, &layout(), "1-5", &mut out, Duration::ZERO);
    assert_eq!(code, ExitCode::from(super::super::REFUSED));
    let got = lines(&out);
    assert!(
        matches!(got.last(), Some(Line::Refused { why }) if why.contains("without a report")),
        "{got:?}"
    );
}

#[test]
fn a_crashed_jobs_reused_pid_does_not_keep_the_stream_open() {
    let fake = Fake::root();
    fake.file(
        "/A/state/runs/1-5/progress.ndjson",
        b"{\"started\":{\"run\":\"1-5\"}}\n",
    );
    fake.file(
        "/A/state/run.lock",
        b"pid=4242 run=1-5 start=Fri_Oct_16_22:36:10_2026\n",
    );
    fake.alive(4242);
    fake.born(4242, 1_792_224_062);
    let mut out = Vec::new();
    let code = follow(&fake, &layout(), "1-5", &mut out, Duration::ZERO);
    assert_eq!(code, ExitCode::from(super::super::REFUSED));
    assert!(
        matches!(lines(&out).last(), Some(Line::Refused { why }) if why.contains("without a report"))
    );
}

fn opened(serial: u64) -> Opened {
    Opened {
        envelope: crate::envelope::parse_body(
            format!(
                "schema = 1\nhost = \"{UUID}\"\nhost_name = \"h\"\nserial = {serial}\nissued = 1\nexpires = 2\naction = \"facts\"\n"
            )
            .as_bytes(),
        )
        .unwrap(),
        principal: "signing@ctl".into(),
        key_id: String::new(),
    }
}

#[test]
fn a_stale_report_does_not_authorize_a_missing_start_holder() {
    let fake = Fake::root();
    fake.file("/A/state/runs/1-5/report.json", b"{}");
    let error = match StartingRun::claim(&fake, &layout(), "1-5", 17) {
        Ok(_) => panic!("a stale report is not lock ownership"),
        Err(error) => error,
    };
    assert!(error.contains("no longer names this apply"), "{error}");
    assert!(!fake.calls().iter().any(|call| call.contains("run-job")));
}

#[test]
fn apply_refuses_without_root_and_reads_nothing() {
    let mut fake = Fake::root();
    fake.euid = 501;
    let mut out = Vec::new();
    let mut stdin: &[u8] = b"anything";
    let code = apply(&fake, &layout(), Path::new("/x"), &mut stdin, &mut out);
    assert_eq!(code, ExitCode::from(super::super::REFUSED));
    assert_eq!(stdin, b"anything", "standard input was read");
    assert!(fake.calls().is_empty());
}

#[test]
fn apply_refuses_an_agent_whose_path_chain_is_not_roots() {
    let fake = Fake::root();
    let mut out = Vec::new();
    let mut stdin: &[u8] = b"anything";
    let code = apply(
        &fake,
        &layout(),
        Path::new("/Users//admin/aterm"),
        &mut stdin,
        &mut out,
    );
    assert_eq!(code, ExitCode::from(super::super::REFUSED));
    let got = lines(&out);
    assert!(
        matches!(&got[0], Line::Refused { why } if why.contains("path is not root's")),
        "{got:?}"
    );
    assert_eq!(stdin, b"anything");
}

/// The answers `apply` needs on a host where the envelope verifies.
fn verifying_host(principal: &str) -> Fake {
    let mut fake = Fake::root();
    fake.next_pid = 4242;
    fake.born(4242, 1_792_190_170);
    let dir = Stat {
        uid: 0,
        gid: 0,
        mode: 0o755,
        dir: true,
        symlink: false,
        len: 0,
    };
    for d in ["/", "/A", "/A/agent", "/A/stage"] {
        fake.stat_of(d, dir);
    }
    fake.stat_of("/A/agent/aterm", Stat { dir: false, ..dir });
    fake.on(
        "/usr/sbin/ioreg -rd1 -c IOPlatformExpertDevice",
        Ran::ok(crate::agent::platform::tests::IOREG_VM),
    );
    let pid = std::process::id();
    fake.on(
        &format!("/usr/bin/ssh-keygen -Y find-principals -s /A/stage/aterm-open-{pid}.sig -f /A/allowed_signers"),
        Ran::ok(&format!("{principal}\n")),
    );
    fake.on(
        &format!("/usr/bin/ssh-keygen -Y verify -f /A/allowed_signers -I {principal} -n aterm-machines -s /A/stage/aterm-open-{pid}.sig"),
        Ran::ok(&format!("Good \"aterm-machines\" signature for {principal} with ED25519 key SHA256:P6HGW+cv8zgw7CrKPm2sOl5fu7RG4dXNK0SYwxiFmiw\n")), // gitleaks:allow
    );
    fake
}

const UUID: &str = "C3396674-FCD3-52E5-A1C1-BD5B9E8F5C43";

/// An armored signature's shape (the fake's ssh-keygen answers for it).
const SIG: &str = "-----BEGIN SSH SIGNATURE-----\nU1NIU0lHAAAAAQ==\n-----END SSH SIGNATURE-----\n";

fn wire(fake: &Fake, serial: u64, action: crate::envelope::Action) -> Vec<u8> {
    let e = crate::envelope::Envelope {
        schema: 1,
        host: UUID.into(),
        host_name: "lab-07".into(),
        serial,
        issued: fake.now,
        expires: fake.now + 86_400,
        action,
        disrupt: crate::envelope::Disrupt::WhenIdle,
        idle_secs: 900,
        baseline: Some("schema = 1\n".into()),
        artifacts: Vec::new(),
        exec: None,
        upgrade: None,
        rekey: None,
    };
    format!("{SIG}{}", crate::envelope::body_text(&e).unwrap()).into_bytes()
}

#[test]
fn apply_raises_the_floor_takes_the_lock_and_starts_a_detached_job_before_streaming() {
    let fake = verifying_host("signing@ctl");
    let run = format!("{}-5", fake.now);
    fake.file(
        &format!("/A/state/runs/{run}/report.json"),
        aterm_json::to_string(&report(Vec::new()))
            .unwrap()
            .as_bytes(),
    );
    let mut out = Vec::new();
    let mut stdin: &[u8] = &wire(&fake, 5, crate::envelope::Action::Apply);
    let code = apply(
        &fake,
        &layout(),
        Path::new("/A/agent/aterm"),
        &mut stdin,
        &mut out,
    );
    assert_eq!(code, ExitCode::SUCCESS, "{}", String::from_utf8_lossy(&out));
    let floors = state::floors_or_refuse(&fake, &layout()).unwrap();
    assert_eq!(state::floor_of(&floors, "signing@ctl"), 5);
    assert_eq!(
        state::holder(&fake, &layout()),
        Some(state::Holder {
            pid: 4242,
            run: run.clone(),
            start: Some("Fri_Oct_16_22:36:10_2026".into()),
        })
    );
    let calls = fake.calls();
    assert!(
        calls.contains(&format!("mkdir /A/state/runs/{run} 750"))
            && calls.contains(&format!("chown 0:80 /A/state/runs/{run}")),
        "{calls:#?}"
    );
    assert_eq!(
        fake.mode_of(&format!("/A/state/runs/{run}/envelope")),
        Some(0o600)
    );
    let calls = fake.calls();
    let floor = calls
        .iter()
        .position(|c| c == "write /A/state/floors.toml 644")
        .unwrap();
    let job = calls
        .iter()
        .position(|c| {
            c == &format!(
                "stage /A/agent/aterm machines agent run-job {run} >> /A/state/runs/{run}/job.log"
            )
        })
        .unwrap_or_else(|| panic!("{calls:#?}"));
    assert!(floor < job, "the floor rises before anything is acted on");
    assert!(calls.iter().any(|c| c == "stage committed 4242"));
    assert!(calls.contains(&format!(
        "detach /usr/bin/caffeinate -i -w 4242 >> /A/state/runs/{run}/caffeinate.log"
    )));
    let got = lines(&out);
    assert!(
        matches!(&got[0], Line::Started { run: r } if *r == run),
        "{got:?}"
    );
    assert!(matches!(got.last(), Some(Line::Report(_))), "{got:?}");
}

#[test]
fn a_replayed_or_busy_apply_is_refused_and_changes_nothing() {
    let fake = verifying_host("signing@ctl");
    fake.file(
        "/A/state/floors.toml",
        b"[[floor]]\nprincipal = \"signing@ctl\"\nserial = 5\n",
    );
    let mut out = Vec::new();
    let mut stdin: &[u8] = &wire(&fake, 5, crate::envelope::Action::Apply);
    let code = apply(
        &fake,
        &layout(),
        Path::new("/A/agent/aterm"),
        &mut stdin,
        &mut out,
    );
    assert_eq!(code, ExitCode::from(super::super::REFUSED));
    assert!(
        matches!(&lines(&out)[0], Line::Refused { why } if why == "replay principal=signing@ctl serial=5 floor=5"),
        "{}",
        String::from_utf8_lossy(&out)
    );
    let fake = verifying_host("signing@ctl");
    fake.file("/A/state/run.lock", b"pid=77 run=1799999999-4\n");
    fake.alive(77);
    let mut out = Vec::new();
    let mut stdin: &[u8] = &wire(&fake, 6, crate::envelope::Action::Apply);
    let code = apply(
        &fake,
        &layout(),
        Path::new("/A/agent/aterm"),
        &mut stdin,
        &mut out,
    );
    assert_eq!(code, ExitCode::from(super::super::REFUSED));
    assert!(
        matches!(&lines(&out)[0], Line::Refused { why } if why == "busy run=1799999999-4"),
        "{}",
        String::from_utf8_lossy(&out)
    );
    assert!(
        state::floors_or_refuse(&fake, &layout())
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        state::last_busy(&fake, &layout()).as_deref(),
        Some("1800000000-6")
    );
    assert!(
        !fake
            .calls()
            .iter()
            .any(|c| c.starts_with("detach") || c.starts_with("stage "))
    );
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

/// SHA-256 of "hello".
const HELLO: &str = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";
const INBOX: &str = "/Users//admin/Library/Caches/aterm-machines/inbox";

#[test]
fn an_artifact_is_cached_only_from_a_root_owned_copy_that_hashes_to_its_name() {
    let fake = Fake::root();
    let sha = HELLO.to_owned();
    fake.file(&format!("{INBOX}/{sha}"), b"hello");
    fake.on(&format!("admin$ /bin/rm -f -- {INBOX}/{sha}"), Ran::ok(""));
    let want = (sha.clone(), "app.Emacs".to_owned());
    let notes = copy_artifacts(&fake, &layout(), &[&want], &admin()).unwrap();
    assert_eq!(notes, Vec::<String>::new());
    assert_eq!(fake.written(&format!("/A/cache/{sha}")).unwrap(), b"hello");
    assert!(fake.written(&format!("/A/inbox/{sha}")).is_none());
    let fake = Fake::root();
    fake.file(&format!("{INBOX}/{sha}"), b"evil");
    let err = copy_artifacts(&fake, &layout(), &[&want], &admin()).unwrap_err();
    assert!(err.contains("hashes to"), "{err}");
    assert!(fake.written(&format!("/A/cache/{sha}")).is_none());
    assert!(fake.written(&format!("/A/inbox/{sha}")).is_none());
    assert!(!fake.calls().iter().any(|c| c.contains("/bin/rm")));
    let fake = Fake::root();
    let err = copy_artifacts(&fake, &layout(), &[&want], &admin()).unwrap_err();
    assert!(err.contains("upload it with `put`"), "{err}");
}

#[test]
fn the_inbox_copy_goes_once_it_is_cached_and_by_the_admin_not_by_root() {
    let fake = Fake::root();
    fake.file(&format!("{INBOX}/{HELLO}"), b"hello");
    fake.on(
        &format!("admin$ /bin/rm -f -- {INBOX}/{HELLO}"),
        Ran::ok(""),
    );
    let want = (HELLO.to_owned(), "app.Emacs".to_owned());
    copy_artifacts(&fake, &layout(), &[&want], &admin()).unwrap();
    let calls = fake.calls();
    let cached = calls
        .iter()
        .position(|c| c == &format!("rename /A/inbox/{HELLO} /A/cache/{HELLO}"))
        .unwrap_or_else(|| panic!("{calls:#?}"));
    let removed = calls
        .iter()
        .position(|c| c == &format!("admin$ /bin/rm -f -- {INBOX}/{HELLO}"))
        .unwrap_or_else(|| panic!("{calls:#?}"));
    assert!(cached < removed, "{calls:#?}");
    assert!(
        !calls
            .iter()
            .any(|c| c.starts_with(&format!("remove {INBOX}"))),
        "root unlinked in the admin's inbox: {calls:#?}"
    );
    let fake = Fake::root();
    fake.file(&format!("{INBOX}/{HELLO}"), b"hello");
    fake.on(
        &format!("admin$ /bin/rm -f -- {INBOX}/{HELLO}"),
        Ran::exit(1, &format!("rm: {INBOX}/{HELLO}: Permission denied\n")),
    );
    let notes = copy_artifacts(&fake, &layout(), &[&want], &admin()).unwrap();
    assert_eq!(
        fake.written(&format!("/A/cache/{HELLO}")).unwrap(),
        b"hello"
    );
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert!(
        notes[0].contains(&format!("{INBOX}/{HELLO}")) && notes[0].contains("Permission denied"),
        "{notes:?}"
    );
}

#[test]
fn an_artifact_named_twice_is_taken_once() {
    let fake = Fake::root();
    fake.file(&format!("{INBOX}/{HELLO}"), b"hello");
    fake.on(
        &format!("admin$ /bin/rm -f -- {INBOX}/{HELLO}"),
        Ran::ok(""),
    );
    let app = (HELLO.to_owned(), "app.Emacs".to_owned());
    let upgrade = (HELLO.to_owned(), "aterm 0.104.0".to_owned());
    let notes = copy_artifacts(&fake, &layout(), &[&app, &upgrade], &admin()).unwrap();
    assert_eq!(notes, Vec::<String>::new());
    let calls = fake.calls();
    let count = |p: &str| calls.iter().filter(|c| c.starts_with(p)).count();
    assert_eq!(count(&format!("copy {INBOX}/{HELLO} ")), 1, "{calls:#?}");
    assert_eq!(count("admin$ /bin/rm"), 1, "{calls:#?}");
}

#[test]
fn an_apply_prunes_the_cached_artifacts_its_baseline_no_longer_names() {
    let old = "1".repeat(64);
    let seeded = || {
        let fake = Fake::root();
        fake.file(&format!("/A/cache/{HELLO}"), b"hello");
        fake.file(&format!("/A/cache/{old}"), b"old image");
        fake.file("/A/cache/README", b"not an artifact");
        fake
    };
    let mut e = opened(5);
    e.envelope.artifacts = vec![crate::envelope::ArtifactRef {
        sha256: HELLO.into(),
        name: "app.Emacs".into(),
    }];
    for action in [Action::Plan, Action::Exec, Action::Upgrade] {
        let fake = seeded();
        e.envelope.action = action;
        assert_eq!(take_artifacts(&fake, &layout(), &e), Ok(Vec::new()));
        assert!(
            fake.written(&format!("/A/cache/{old}")).is_some(),
            "{action:?}"
        );
    }
    let fake = seeded();
    e.envelope.action = Action::Apply;
    let notes = take_artifacts(&fake, &layout(), &e).unwrap();
    assert!(fake.written(&format!("/A/cache/{old}")).is_none());
    assert_eq!(
        fake.written(&format!("/A/cache/{HELLO}")).unwrap(),
        b"hello"
    );
    assert!(fake.written("/A/cache/README").is_some());
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert!(notes[0].contains(&old), "{notes:?}");
}

#[test]
fn a_note_opens_the_run_log_before_the_job_starts() {
    let mut fake = Fake::root();
    fake.next_pid = 4242;
    fake.file("/A/state/run.lock", b"pid=17 run=1-5 start=parent\n");
    fake.born(4242, 1_792_190_170);
    let layout = layout();
    let mut owner = StartingRun::claim(&fake, &layout, "1-5", 17).unwrap();
    start(
        &fake,
        &layout,
        Path::new("/A/agent/aterm"),
        SIG.as_bytes(),
        &opened(5),
        "1-5",
        &["the inbox copy stays".to_owned()],
        &mut owner,
    )
    .unwrap();
    assert_eq!(
        fake.written("/A/state/runs/1-5/job.log").unwrap(),
        b"note: the inbox copy stays\n"
    );
    assert_eq!(fake.mode_of("/A/state/runs/1-5/job.log"), Some(0o600));
    let calls = fake.calls();
    let log = calls
        .iter()
        .position(|c| c == "write /A/state/runs/1-5/job.log 600")
        .unwrap();
    let job = calls.iter().position(|c| c.starts_with("stage ")).unwrap();
    assert!(log < job, "{calls:#?}");
}

/// A digest that is no SHA-256 never becomes a cache path, even one a file answers at.
/// NEGATIVE CONTROL: a directory or a link at a real digest's cache path is no cached
/// artifact, so it is taken again (here refused for want of the managed state).
#[test]
fn an_artifact_digest_is_checked_before_its_cache_path_is_and_only_a_file_is_cached() {
    let file = Stat {
        uid: 0,
        gid: 0,
        mode: 0o644,
        dir: false,
        symlink: false,
        len: 5,
    };
    let mut e = opened(5);
    e.envelope.action = Action::Plan;
    e.envelope.artifacts = vec![crate::envelope::ArtifactRef {
        sha256: "../x".into(),
        name: "app.Evil".into(),
    }];
    let fake = Fake::root();
    fake.stat_of("/A/cache/../x", file);
    let err = take_artifacts(&fake, &layout(), &e).unwrap_err();
    assert!(err.contains("\"../x\" is not a SHA-256 digest"), "{err}");
    assert!(fake.calls().is_empty(), "{:?}", fake.calls());

    e.envelope.artifacts = vec![crate::envelope::ArtifactRef {
        sha256: HELLO.into(),
        name: "app.Emacs".into(),
    }];
    let fake = Fake::root();
    fake.stat_of(&format!("/A/cache/{HELLO}"), file);
    assert_eq!(take_artifacts(&fake, &layout(), &e), Ok(Vec::new()));
    for not_a_file in [
        Stat { dir: true, ..file },
        Stat {
            symlink: true,
            ..file
        },
    ] {
        let fake = Fake::root();
        fake.stat_of(&format!("/A/cache/{HELLO}"), not_a_file);
        let err = take_artifacts(&fake, &layout(), &e).unwrap_err();
        assert!(err.contains("managed"), "{not_a_file:?}: {err}");
    }
}

/// A copy that cannot be renamed into the cache leaves no landing file behind.
#[test]
fn a_cache_rename_that_fails_removes_its_landing_copy() {
    let fake = Fake::root();
    fake.file(&format!("{INBOX}/{HELLO}"), b"hello");
    fake.rename_fails(
        &format!("/A/cache/{HELLO}"),
        std::io::ErrorKind::IsADirectory,
    );
    let want = (HELLO.to_owned(), "app.Emacs".to_owned());
    let err = copy_artifacts(&fake, &layout(), &[&want], &admin()).unwrap_err();
    assert!(err.starts_with("artifact app.Emacs: "), "{err}");
    assert!(fake.written(&format!("/A/inbox/{HELLO}")).is_none());
    assert!(fake.written(&format!("/A/cache/{HELLO}")).is_none());
    assert!(!fake.calls().iter().any(|c| c.contains("/bin/rm")));
}

#[test]
fn attach_refuses_a_run_id_that_is_not_one() {
    let fake = Fake::root();
    let mut out = Vec::new();
    let code = attach(&fake, &layout(), "../x", &mut out);
    assert_eq!(code, ExitCode::from(super::super::REFUSED));
    assert!(fake.calls().is_empty());
}
