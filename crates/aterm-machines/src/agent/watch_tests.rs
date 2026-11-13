// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::port::{Fake, Stat};
use std::sync::Mutex;

const ID: &str = "1800000000-4242";
const DIR: &str = "/A/state/screen-watch";

fn layout() -> Layout {
    Layout { root: "/A".into() }
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

/// A root fake that knows the admin (uid 501) as `dscl` printed it in the VM, with
/// `state/screen-watch/` root's alone.
fn host() -> Fake {
    let fake = Fake::root();
    fake.on(
        "/usr/bin/dscl . -read /Groups/admin GroupMembership",
        Ran::ok("GroupMembership: root _mbsetupuser admin\n"),
    );
    fake.on(
        "/usr/bin/dscl . -read /Users//admin UniqueID PrimaryGroupID NFSHomeDirectory UserShell",
        Ran::ok(
            "NFSHomeDirectory: /Users//admin\nPrimaryGroupID: 20\nUniqueID: 501\nUserShell: /bin/zsh\n",
        ),
    );
    fake.stat_of(
        DIR,
        Stat {
            uid: 0,
            gid: 0,
            mode: 0o700,
            dir: true,
            symlink: false,
            len: 0,
        },
    );
    fake
}

fn path(id: &str, ext: &str) -> String {
    format!("{DIR}/{id}.{ext}")
}

/// Seed `path` 0600, as the agent writes every file of a watch.
fn put(fake: &Fake, path: &str, bytes: &[u8]) {
    fake.write_file(Path::new(path), bytes, 0o600).unwrap();
}

/// A clock that moves only when paused, and runs `each` with the new time after every
/// pause: what the host does meanwhile (a signed run, a disarm, the watcher starting).
struct Script<'a> {
    at: Mutex<Duration>,
    each: Box<dyn Fn(Duration) + 'a>,
}

impl<'a> Script<'a> {
    fn new(each: impl Fn(Duration) + 'a) -> Self {
        Self {
            at: Mutex::new(Duration::ZERO),
            each: Box::new(each),
        }
    }
}

impl Clock for Script<'_> {
    fn elapsed(&self) -> Duration {
        *self.at.lock().unwrap()
    }

    fn pause(&self, d: Duration) {
        let now = {
            let mut at = self.at.lock().unwrap();
            *at += d;
            *at
        };
        assert!(now <= LIFE * 2, "still waiting after {}s", now.as_secs());
        (self.each)(now);
    }
}

fn arm_as(fake: &Fake, uid: &str, user: &str, clock: &dyn Clock) -> (ExitCode, String) {
    let mut out = Vec::new();
    let code = arm(
        fake,
        &layout(),
        Path::new("/x"),
        (uid, user),
        4242,
        clock,
        &mut out,
    );
    (code, String::from_utf8_lossy(&out).into_owned())
}

/// The watcher writing `<id>.watching` at its first pause, as a live one does once its
/// checks pass.
fn starts(fake: &Fake) -> Script<'_> {
    Script::new(move |_| {
        fake.file(&path(ID, "watching"), b"777\n");
    })
}

#[test]
fn arm_checks_the_account_records_the_arm_and_starts_the_watcher_detached() {
    let fake = host();
    let (code, out) = arm_as(&fake, "501", "admin", &starts(&fake));
    assert_eq!(out, format!("armed {ID}\n"));
    assert_eq!(code, ExitCode::SUCCESS);
    let calls: Vec<String> = fake
        .calls()
        .into_iter()
        .filter(|c| !c.contains("dscl"))
        .collect();
    assert_eq!(
        calls,
        [
            format!("mkdir {DIR} 700"),
            format!("chown 0:0 {DIR}"),
            format!("write {} 600", path(ID, "armed")),
            format!(
                "detach /x machines agent screen-watch run {ID} >> {}",
                path(ID, "log")
            ),
        ]
    );
    let armed: Armed = aterm_toml::from_str(&String::from_utf8_lossy(
        &fake.written(&path(ID, "armed")).unwrap(),
    ))
    .unwrap();
    assert_eq!(
        armed,
        Armed {
            uid: 501,
            user: "admin".into(),
            armed: 1_800_000_000,
        }
    );
    assert_eq!(fake.mode_of(&path(ID, "armed")), Some(0o600));
}

#[test]
fn arm_waits_for_a_slow_watcher_to_start() {
    let fake = host();
    let clock = Script::new(|at| {
        if at >= Duration::from_secs(11) {
            fake.file(&path(ID, "watching"), b"777\n");
        }
    });
    let (code, out) = arm_as(&fake, "501", "admin", &clock);
    assert_eq!(code, ExitCode::SUCCESS, "{out}");
    assert!(fake.written(&path(ID, "armed")).is_some());
}

#[test]
fn arm_refuses_an_account_whose_uid_differs_or_that_does_not_exist_and_arms_nothing() {
    let fake = host();
    let (code, out) = arm_as(&fake, "502", "admin", &starts(&fake));
    assert_eq!(out, "not armed: account admin is uid 501, not 502\n");
    assert_eq!(code, ExitCode::FAILURE);
    let fake = host();
    fake.on(
        "/usr/bin/dscl . -read /Users//bob UniqueID PrimaryGroupID NFSHomeDirectory UserShell",
        Ran::exit(56, "<dscl_cmd> DS Error: -14136 (eDSRecordNotFound)\n"),
    );
    let (code, out) = arm_as(&fake, "502", "bob", &starts(&fake));
    assert_eq!(out, "not armed: no account bob\n");
    assert_eq!(code, ExitCode::FAILURE);
    let fake = host();
    let (code, out) = arm_as(&fake, "admin", "admin", &starts(&fake));
    assert!(out.starts_with("refused: "), "{out}");
    assert_eq!(code, ExitCode::from(super::super::REFUSED));
    let mut fake = host();
    fake.euid = 501;
    let (code, _) = arm_as(&fake, "501", "admin", &starts(&fake));
    assert_eq!(code, ExitCode::from(super::super::REFUSED));
    for fake in [host(), fake] {
        assert!(
            !fake
                .calls()
                .iter()
                .any(|c| c.starts_with("write") || c.starts_with("detach")),
            "{:?}",
            fake.calls()
        );
    }
}

#[test]
fn a_watcher_that_never_starts_watching_leaves_no_arm_behind() {
    let fake = host();
    fake.file(&path(ID, "log"), b"refused: the account changed\n");
    let clock = Script::new(|_| {});
    let (code, _) = arm_as(&fake, "501", "admin", &clock);
    assert_eq!(code, ExitCode::FAILURE);
    assert!(clock.elapsed() >= START_HANG);
    assert!(fake.written(&path(ID, "armed")).is_none());
    assert!(
        fake.calls()
            .contains(&format!("remove {}", path(ID, "armed")))
    );
}

#[test]
fn a_second_arm_for_the_same_account_replaces_the_first_and_keeps_another_accounts() {
    let fake = host();
    let old = "1799999000-7";
    let other = "1799999000-8";
    let ended = "1799990000-9";
    let unknown = "1799990000-10";
    let missing = "1799990000-11";
    fake.file(
        &path(old, "armed"),
        b"uid = 501\nuser = \"admin\"\narmed = 1799999000\n",
    )
    .file(&path(old, "watching"), b"7\n")
    .file(
        &path(other, "armed"),
        b"uid = 502\nuser = \"student\"\narmed = 1799999000\n",
    )
    .file(&path(other, "watching"), b"8\n")
    .file(&path(ended, "ended"), b"locked admin's session: ...\n")
    .file(&path(unknown, "armed"), b"unreadable ownership = [")
    .file(&path(unknown, "watching"), b"10\n")
    .file(&path(missing, "watching"), b"11\n")
    .alive(7)
    .alive(10)
    .alive(11);
    let (code, out) = arm_as(&fake, "501", "admin", &starts(&fake));
    assert_eq!(out, format!("armed {ID} (replaced {old})\n"));
    assert_eq!(code, ExitCode::SUCCESS);
    assert!(fake.written(&path(old, "armed")).is_none());
    assert!(fake.written(&path(other, "armed")).is_some());
    let removes: Vec<String> = fake
        .calls()
        .into_iter()
        .filter(|c| c.starts_with("remove"))
        .collect();
    assert_eq!(removes[0], format!("remove {}", path(old, "armed")));
    assert!(removes.contains(&format!("remove {}", path(old, "armed"))));
    assert!(!removes.iter().any(|c| c.contains(other)), "{removes:?}");
    assert!(fake.written(&path(ended, "ended")).is_some());
    assert!(fake.written(&path(unknown, "armed")).is_some());
    assert!(fake.written(&path(missing, "watching")).is_some());
    for retained in [other, ended, unknown, missing] {
        assert!(
            !removes.iter().any(|c| c.contains(retained)),
            "arming one account must preserve unknown and different owners: {retained}: {removes:?}"
        );
    }
}

/// A replacement that never starts must leave the existing watch in charge.
#[test]
fn an_arm_that_cannot_start_keeps_the_watch_it_would_replace() {
    let old = "1799999000-7";
    let seed_old = |fake: &Fake| {
        fake.file(
            &path(old, "armed"),
            b"uid = 501\nuser = \"admin\"\narmed = 1799999000\n",
        )
        .file(&path(old, "watching"), b"7\n")
        .alive(7);
    };
    let good = host();
    seed_old(&good);
    let (code, _) = arm_as(&good, "501", "admin", &starts(&good));
    assert_eq!(code, ExitCode::SUCCESS);
    assert!(good.written(&path(ID, "armed")).is_some());
    assert!(good.written(&path(old, "armed")).is_none());

    let failed = host();
    seed_old(&failed);
    failed.on(&session_cmd(&admin()).display(), Ran::ok(""));
    failed.on(
        &lock_cmd(&layout(), &admin()).display(),
        Ran::ok("locked\n"),
    );
    let clock = Script::new(|_| {});
    let (code, _) = arm_as(&failed, "501", "admin", &clock);
    assert_eq!(code, ExitCode::FAILURE);
    assert!(clock.elapsed() >= START_HANG);
    assert!(failed.written(&path(ID, "armed")).is_none());
    let ended = watch(&failed, &layout(), old, &admin(), &clock, TEST);
    assert!(
        matches!(ended, Ended::Done(ref line) if line.starts_with("locked admin's session")),
        "a failed replacement must leave the old watcher able to lock: {ended:?}"
    );
    assert_eq!(locks(&failed), 1);
}

/// The heartbeat files as the agent writes them: a run in progress, or the newest
/// finished run.
fn runs(fake: &Fake, lock: Option<&str>, last: Option<&str>) {
    match lock {
        Some(run) => {
            fake.file(
                "/A/state/run.lock",
                format!("pid=10 run={run}\n").as_bytes(),
            );
        }
        None => {
            let _ = fake.remove(Path::new("/A/state/run.lock"));
        }
    }
    if let Some(run) = last {
        fake.file("/A/state/last_run", format!("{run}\n").as_bytes());
    }
}

/// A fake with arm [`ID`] for the admin in place, and the lock and session readings
/// scripted as given.
fn armed_host(session: Option<Ran>, lock: Option<Ran>) -> Fake {
    let fake = host();
    put(
        &fake,
        &path(ID, "armed"),
        b"uid = 501\nuser = \"admin\"\narmed = 1800000000\n",
    );
    if let Some(ran) = session {
        fake.on(&session_cmd(&admin()).display(), ran);
    }
    if let Some(ran) = lock {
        fake.on(&lock_cmd(&layout(), &admin()).display(), ran);
    }
    fake
}

const TEST: Bounds = Bounds {
    quiet: QUIET,
    life: LIFE,
    poll: POLL,
};

fn locks(fake: &Fake) -> usize {
    let lock = lock_cmd(&layout(), &admin()).display();
    fake.calls().iter().filter(|c| **c == lock).count()
}

#[test]
fn the_watcher_waits_while_signed_runs_come_and_locks_once_none_has_for_the_quiet_time() {
    let fake = armed_host(Some(Ran::ok("")), Some(Ran::ok("locked\n")));
    let clock = Script::new(|t| {
        let s = t.as_secs();
        if s <= 100 && s % 4 == 0 {
            let run = format!("{}-{s}", 1_800_000_000 + s);
            if s % 8 == 0 {
                runs(&fake, Some(&run), None);
            } else {
                runs(&fake, None, Some(&run));
            }
        }
    });
    let ended = watch(&fake, &layout(), ID, &admin(), &clock, TEST);
    assert_eq!(
        ended,
        Ended::Done("locked admin's session: no signed request reached the host for 60s".into())
    );
    assert_eq!(locks(&fake), 1);
    assert_eq!(clock.elapsed(), Duration::from_secs(160));
    assert_eq!(
        lock_cmd(&layout(), &admin()).display(),
        "admin@gui$ /A/bin/aterm machines agent lock-screen"
    );
    assert!(lock_cmd(&layout(), &admin()).session);
    assert!(
        lock_cmd(&layout(), &admin()).budget
            >= super::super::lock::LOCK_HANG + Duration::from_secs(60),
        "the enclosing command must cover startup and the complete console readback"
    );
}

#[test]
fn a_run_still_in_progress_is_one_that_started_and_a_long_one_does_not_hold_off_the_lock() {
    let fake = armed_host(Some(Ran::ok("")), Some(Ran::ok("locked\n")));
    let clock = Script::new(|t| {
        if t == Duration::from_secs(10) {
            runs(&fake, Some("1800000010-3"), None);
        }
    });
    let ended = watch(&fake, &layout(), ID, &admin(), &clock, TEST);
    assert!(matches!(ended, Ended::Done(l) if l.starts_with("locked")));
    assert_eq!(clock.elapsed(), Duration::from_secs(70));
}

#[test]
fn requests_refused_busy_while_another_run_holds_the_host_keep_the_watch_waiting() {
    let fake = armed_host(Some(Ran::ok("")), Some(Ran::ok("locked\n")));
    let clock = Script::new(|t| {
        let s = t.as_secs();
        if s == 10 {
            runs(&fake, Some("1800000010-3"), None);
        }
        if (12..=148).contains(&s) && s % 4 == 0 {
            fake.file(
                "/A/state/last_busy",
                format!("{}-{s}\n", 1_800_000_000 + s).as_bytes(),
            );
        }
    });
    let ended = watch(&fake, &layout(), ID, &admin(), &clock, TEST);
    assert_eq!(
        ended,
        Ended::Done("locked admin's session: no signed request reached the host for 60s".into())
    );
    assert_eq!(locks(&fake), 1);
    assert_eq!(clock.elapsed(), Duration::from_secs(208));
}

#[test]
fn an_unreadable_session_reading_still_attempts_the_lock_and_keeps_why() {
    let present = armed_host(Some(Ran::ok("")), Some(Ran::ok("locked\n")));
    let ended = watch(
        &present,
        &layout(),
        ID,
        &admin(),
        &Script::new(|_| {}),
        TEST,
    );
    assert!(matches!(ended, Ended::Done(ref line) if line.starts_with("locked")));
    assert_eq!(locks(&present), 1);
    let missing = armed_host(None, Some(Ran::ok("locked\n")));
    let ended = watch(
        &missing,
        &layout(),
        ID,
        &admin(),
        &Script::new(|_| {}),
        TEST,
    );
    assert!(matches!(ended, Ended::Done(ref line) if line.contains("could not be read")));
    assert_eq!(locks(&missing), 1);
    for reading in [
        Ran::exit(125, "Domain does not support specified action"),
        Ran::exit(13, "Permission denied"),
        Ran {
            status: None,
            stdout: Vec::new(),
            stderr: Vec::new(),
        },
    ] {
        let diagnostic = reading.describe();
        let fake = armed_host(Some(reading), Some(Ran::ok("locked\n")));
        let ended = watch(&fake, &layout(), ID, &admin(), &Script::new(|_| {}), TEST);
        assert_eq!(
            locks(&fake),
            1,
            "an unreadable session is not positive absence: {ended:?}"
        );
        assert!(
            matches!(ended, Ended::Done(ref line) if line.starts_with("locked") && line.contains("could not be read") && line.contains(&diagnostic)),
            "the lock outcome must retain the failed reading: {ended:?}"
        );
    }
    let reading = Ran::exit(125, "Domain does not support specified action");
    let diagnostic = reading.describe();
    let reason = "no login session";
    let absent = armed_host(
        Some(reading),
        Some(Ran {
            status: Some(1),
            stdout: format!("not locked: {reason}\n").into_bytes(),
            stderr: Vec::new(),
        }),
    );
    let ended = watch(&absent, &layout(), ID, &admin(), &Script::new(|_| {}), TEST);
    assert_eq!(locks(&absent), 1);
    assert!(
        matches!(ended, Ended::Done(ref line) if line.contains(reason) && line.contains(&diagnostic)),
        "{ended:?}"
    );
}

#[test]
fn a_lock_that_fails_says_why() {
    let fake = armed_host(
        Some(Ran::ok("")),
        Some(Ran {
            status: Some(1),
            stdout: b"not locked: SACLockScreenImmediate answered 22\n".to_vec(),
            stderr: Vec::new(),
        }),
    );
    let ended = watch(&fake, &layout(), ID, &admin(), &Script::new(|_| {}), TEST);
    assert_eq!(
        ended,
        Ended::Done(
            "not locked: no signed request reached the host for 60s; lock-screen: \
             SACLockScreenImmediate answered 22"
                .into()
        )
    );
}

#[test]
fn a_watch_disarmed_or_replaced_ends_without_reading_or_locking() {
    let fake = armed_host(Some(Ran::ok("")), Some(Ran::ok("locked\n")));
    let clock = Script::new(|t| {
        if t == Duration::from_secs(30) {
            let _ = forget(&fake, &layout(), ID);
        }
    });
    assert_eq!(
        watch(&fake, &layout(), ID, &admin(), &clock, TEST),
        Ended::Disarmed
    );
    assert_eq!(clock.elapsed(), Duration::from_secs(30));
    assert_eq!(locks(&fake), 0);
    assert!(
        !fake.calls().contains(&session_cmd(&admin()).display()),
        "{:?}",
        fake.calls()
    );
}

#[test]
fn a_watch_that_reaches_its_longest_life_locks_even_while_runs_still_come() {
    let fake = armed_host(Some(Ran::ok("")), Some(Ran::ok("locked\n")));
    let clock = Script::new(|t| {
        let s = t.as_secs();
        if s % 10 == 0 {
            runs(&fake, None, Some(&format!("{}-{s}", 1_800_000_000 + s)));
        }
    });
    let short = Bounds {
        life: Duration::from_secs(300),
        ..TEST
    };
    let ended = watch(&fake, &layout(), ID, &admin(), &clock, short);
    assert_eq!(
        ended,
        Ended::Done(
            "locked admin's session: it had watched for 300s, the longest a watch lasts".into()
        )
    );
    assert_eq!(locks(&fake), 1);
}

fn run_watcher(fake: &Fake, clock: &dyn Clock) -> (ExitCode, String) {
    let mut out = Vec::new();
    let code = run(fake, &layout(), ID, 777, clock, TEST, &mut out);
    (code, String::from_utf8_lossy(&out).into_owned())
}

#[test]
fn the_watcher_records_what_it_did_and_leaves_no_arm() {
    let fake = armed_host(Some(Ran::ok("")), Some(Ran::ok("locked\n")));
    let (code, out) = run_watcher(&fake, &Script::new(|_| {}));
    assert_eq!(code, ExitCode::SUCCESS);
    assert_eq!(
        out,
        "watching 1800000000-4242 for admin (uid 501)\nlocked admin's session: no signed \
         request reached the host for 60s\n"
    );
    assert_eq!(
        fake.written(&path(ID, "ended")).as_deref(),
        Some(&b"locked admin's session: no signed request reached the host for 60s\n"[..])
    );
    assert_eq!(fake.mode_of(&path(ID, "ended")), Some(0o600));
    assert!(fake.written(&path(ID, "armed")).is_none());
    assert!(fake.written(&path(ID, "watching")).is_none());
    let calls = fake.calls();
    let at = |c: &str| calls.iter().position(|x| x == c).unwrap();
    assert!(at("ignore hangups") < at(&format!("write {} 600", path(ID, "watching"))));
    let fake = armed_host(Some(Ran::ok("")), Some(Ran::ok("locked\n")));
    let clock = Script::new(|_| {
        let _ = forget(&fake, &layout(), ID);
    });
    let (code, out) = run_watcher(&fake, &clock);
    assert_eq!(code, ExitCode::SUCCESS);
    assert!(out.ends_with(&format!("disarmed {ID}\n")), "{out}");
    assert!(fake.written(&path(ID, "ended")).is_none());
    assert_eq!(locks(&fake), 0);
}

#[test]
fn the_watcher_trusts_only_an_arm_that_is_roots_alone_for_an_account_with_that_uid() {
    let fake = armed_host(Some(Ran::ok("")), Some(Ran::ok("locked\n")));
    put(
        &fake,
        &path(ID, "armed"),
        b"uid = 502\nuser = \"admin\"\narmed = 1800000000\n",
    );
    let (code, out) = run_watcher(&fake, &Script::new(|_| {}));
    assert_eq!(code, ExitCode::from(super::super::REFUSED));
    assert!(
        out.contains("account admin is uid 501 now, not 502"),
        "{out}"
    );
    let fake = armed_host(Some(Ran::ok("")), Some(Ran::ok("locked\n")));
    fake.stat_of(
        DIR,
        Stat {
            uid: 0,
            gid: 0,
            mode: 0o755,
            dir: true,
            symlink: false,
            len: 0,
        },
    );
    let (code, out) = run_watcher(&fake, &Script::new(|_| {}));
    assert_eq!(code, ExitCode::from(super::super::REFUSED));
    assert!(out.contains("is not root's alone"), "{out}");
    let mut fake = armed_host(Some(Ran::ok("")), Some(Ran::ok("locked\n")));
    fake.euid = 501;
    let (code, _) = run_watcher(&fake, &Script::new(|_| {}));
    assert_eq!(code, ExitCode::from(super::super::REFUSED));
    assert_eq!(locks(&fake), 0);
    assert!(!fake.calls().contains(&"ignore hangups".to_owned()));
}

fn disarm_it(fake: &Fake, id: &str) -> (ExitCode, String) {
    let mut out = Vec::new();
    let code = disarm(fake, &layout(), id, &mut out);
    (code, String::from_utf8_lossy(&out).into_owned())
}

#[test]
fn disarm_removes_the_arm_first_and_says_what_an_ended_watcher_did() {
    let fake = armed_host(None, None);
    fake.file(&path(ID, "watching"), b"777\n");
    let (code, out) = disarm_it(&fake, ID);
    assert_eq!(
        (code, out.as_str()),
        (ExitCode::SUCCESS, "disarmed 1800000000-4242\n")
    );
    let removes: Vec<String> = fake
        .calls()
        .into_iter()
        .filter(|c| c.starts_with("remove"))
        .collect();
    assert_eq!(
        removes,
        ["armed", "watching", "ended", "log"].map(|ext| format!("remove {}", path(ID, ext)))
    );
    assert!(fake.written(&path(ID, "armed")).is_none());
    let fake = host();
    fake.file(
        &path(ID, "ended"),
        b"locked admin's session: no signed request reached the host for 61s\n",
    );
    let (_, out) = disarm_it(&fake, ID);
    assert_eq!(
        out,
        "disarmed 1800000000-4242 (it had ended: locked admin's session: no signed \
         request reached the host for 61s)\n"
    );
    let (code, out) = disarm_it(&host(), ID);
    assert_eq!(
        (code, out.as_str()),
        (
            ExitCode::SUCCESS,
            "disarmed 1800000000-4242 (nothing was armed)\n"
        )
    );
    let fake = host();
    let (code, _) = disarm_it(&fake, "../../etc/passwd");
    assert_eq!(code, ExitCode::from(super::super::REFUSED));
    let mut fake = host();
    fake.euid = 501;
    let (code, _) = disarm_it(&fake, ID);
    assert_eq!(code, ExitCode::from(super::super::REFUSED));
    assert!(!fake.calls().iter().any(|c| c.starts_with("remove")));
}

#[test]
fn screen_watch_is_an_agent_verb_and_its_unknown_forms_are_refused() {
    let fake = host();
    for args in [
        &["screen-watch"][..],
        &["screen-watch", "arm", "--user", "admin", "--uid", "501"],
        &["screen-watch", "lock"],
    ] {
        let mut out = Vec::new();
        let code = super::super::run(
            &fake,
            &layout(),
            Path::new("/x"),
            args,
            &mut std::io::empty(),
            &mut out,
        );
        assert_eq!(code, ExitCode::from(super::super::REFUSED), "{args:?}");
        assert!(String::from_utf8_lossy(&out).starts_with("refused: "));
    }
    let mut out = Vec::new();
    let code = super::super::run(
        &fake,
        &layout(),
        Path::new("/x"),
        &["screen-watch", "disarm", ID],
        &mut std::io::empty(),
        &mut out,
    );
    assert_eq!(code, ExitCode::SUCCESS);
    assert_eq!(fake.calls().len(), 4, "the disarm's removals only");
}
