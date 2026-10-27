// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A join's fetches and machine key, the verdict an admin Mac posts on a mistyped join
//! password, and the subscriber's starts of the converge pass, against a server on loopback.

use super::*;
use std::cell::RefCell;
use std::sync::mpsc;

const HANG_DETECTOR: Duration = Duration::from_secs(60);

fn serve_holding(store: &Path, hold: Duration) -> crate::server::Running {
    crate::server::start_with_join_change_gap(
        std::net::TcpListener::bind("127.0.0.1:0").expect("bind"),
        &crate::server::Options {
            listen: "127.0.0.1:0".parse().expect("addr"),
            store: store.to_path_buf(),
            advertise: None,
            pins: Vec::new(),
            hold,
            keep_awake: false,
        },
        Duration::ZERO,
    )
    .expect("server starts")
}

/// The team vault `ma` makes at `server`: its admin, its pinned URL and its join password.
fn team(root: &Path, server: &crate::server::Running) -> (Paths, String, SecretBytes) {
    let admin = Paths::for_home(&root.join("ma"));
    let url = format!("http://{}/v1/team", server.addr);
    let created = test_create_with_password(&admin, &url, None, Some("ma")).expect("create");
    let password = admin_open(&admin)
        .expect("admin opens")
        .doc
        .join_password()
        .expect("create makes a join password");
    (admin, format!("{url}#{}", created.signer), password)
}

fn options<'a>(
    source: &str,
    password: JoinPassword<'a>,
    fetched: Option<&'a [u8]>,
    wait: Duration,
) -> JoinOptions<'a> {
    JoinOptions {
        source: source.to_string(),
        host: Some("mb".to_string()),
        interactive: false,
        wait,
        confirm: &|_| false,
        password,
        fetched,
        requested: &|| {},
        say: &|_| {},
    }
}

/// Remove the admin Mac's approver record, as on a Mac whose approver never kept one.
fn forget_record(admin: &Paths) {
    std::fs::remove_file(admin.answered_file()).expect("CONTROL: the approver kept a record");
    for path in [admin.answered_queue_file(), admin.answered_approved_file()] {
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) => assert_eq!(error.kind(), std::io::ErrorKind::NotFound, "{error}"),
        }
    }
}

/// Wait, at most the hang detector, until `ready`.
fn until(what: &str, ready: impl Fn() -> bool) {
    let deadline = Instant::now() + HANG_DETECTOR;
    while !ready() {
        assert!(Instant::now() < deadline, "never: {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn a_join_fetches_the_vault_once_and_a_probed_one_not_again() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let server = serve_holding(&root.path().join("store"), Duration::from_millis(200));
    let (_admin, source, password) = team(root.path(), &server);
    let before = server.envelope_gets();
    let joined = join(
        &Paths::for_home(&root.path().join("mb")),
        &options(
            &source,
            JoinPassword::Given(Some(password.clone())),
            None,
            Duration::ZERO,
        ),
    );
    assert!(
        matches!(joined, Ok(JoinOutcome::Pending { .. })),
        "{joined:?}"
    );
    assert_eq!(server.envelope_gets() - before, 1, "one GET for a join");

    // A join after a probe applies what the probe fetched: the probe's GET is the only one.
    let before = server.envelope_gets();
    let Ok(ProbeOutcome::Found(probed)) = probe_within(&source, PROBE_DEADLINE) else {
        panic!("the vault answers its probe");
    };
    let joined = join(
        &Paths::for_home(&root.path().join("mc")),
        &options(
            &source,
            JoinPassword::Given(Some(password)),
            Some(&probed.envelope),
            Duration::ZERO,
        ),
    );
    assert!(
        matches!(joined, Ok(JoinOutcome::Pending { .. })),
        "{joined:?}"
    );
    assert_eq!(
        server.envelope_gets() - before,
        1,
        "the probe's GET serves the join"
    );
    server.stop();
}

#[test]
fn a_join_that_reaches_no_vault_leaves_no_machine_key() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    // A port nothing listens on: bound, then released.
    let addr = std::net::TcpListener::bind("127.0.0.1:0")
        .expect("bind")
        .local_addr()
        .expect("addr");
    let mb = Paths::for_home(&root.path().join("mb"));
    let joined = join(
        &mb,
        &options(
            &format!("http://{addr}/v1/team"),
            JoinPassword::Given(None),
            None,
            Duration::ZERO,
        ),
    );
    assert!(joined.is_err(), "{joined:?}");
    assert!(
        read_identity(&mb).expect("readable").is_none(),
        "no key made"
    );
    assert!(config(&mb).expect("readable").is_none(), "no source saved");
}

#[test]
fn a_mistyped_join_password_is_asked_again_as_soon_as_the_approver_refuses_it() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    // Only the verdict can end the joiner's long-poll within the hang detector.
    let server = serve_holding(&root.path().join("store"), Duration::from_secs(600));
    let (admin, source, password) = team(root.path(), &server);
    let mb = Paths::for_home(&root.path().join("mb"));
    let (asked, asks) = mpsc::channel();
    let (mb_at, admin_at) = (&mb, &admin);
    let listed = |with: &str| {
        let proof = join::Key::derive("team", with.as_bytes());
        move || {
            let recipient = read_identity(mb_at)
                .ok()
                .flatten()
                .map(|identity| identity.recipient().to_bech32());
            recipient.is_some_and(|recipient| {
                let wanted = proof.prove("team", &recipient, "mb");
                requests(admin_at, None)
                    .is_ok_and(|queued| queued.iter().any(|r| r.proof.as_ref() == Some(&wanted)))
            })
        }
    };
    let mistyped = "a-mistyped-join-password";
    let right = std::str::from_utf8(password.as_slice())
        .expect("utf-8")
        .to_owned();
    std::thread::scope(|scope| {
        let joiner = scope.spawn(|| {
            let typed = RefCell::new(vec![
                password.clone(),
                SecretBytes::from(mistyped.as_bytes()),
            ]);
            let ask = || {
                asked.send(()).expect("the test listens");
                typed.borrow_mut().pop()
            };
            join(
                &mb,
                &options(&source, JoinPassword::Ask(&ask), None, HANG_DETECTOR),
            )
        });
        asks.recv_timeout(HANG_DETECTOR)
            .expect("asked for the password");
        until("the mistyped request is queued", listed(mistyped));
        // CONTROL: no verdict yet, so nothing says the password was refused.
        assert!(!read_verdict(&mb).expect("asked"));
        assert!(!password_refused(&mb));

        let logged = RefCell::new(Vec::new());
        let mut approver = Approver::default();
        let judged = Instant::now();
        approver_pass(
            &admin,
            &|line| logged.borrow_mut().push(line.to_string()),
            &mut approver,
        );
        let passed = judged.elapsed();
        let refused = Instant::now();
        assert!(
            logged
                .borrow()
                .iter()
                .any(|line| line == "refused mb by the join password"),
            "{logged:?}"
        );
        // One approver pass, and the joiner asks again.
        asks.recv_timeout(HANG_DETECTOR)
            .expect("asked again after the verdict");
        eprintln!(
            "the approver's pass took {passed:?}; asked again {:?} after it",
            refused.elapsed()
        );
        until("the retyped request is queued", listed(&right));
        approver_pass(
            &admin,
            &|line| logged.borrow_mut().push(line.to_string()),
            &mut approver,
        );
        let joined = joiner.join().expect("the joiner ends");
        assert!(
            matches!(joined, Ok(JoinOutcome::Joined { .. })),
            "{joined:?}"
        );
    });
    server.stop();
}

#[test]
fn a_forged_verdict_only_asks_for_the_password_again() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let server = serve_holding(&root.path().join("store"), Duration::from_millis(200));
    let (admin, source, password) = team(root.path(), &server);
    let mb = Paths::for_home(&root.path().join("mb"));
    let asked = join(
        &mb,
        &options(
            &source,
            JoinPassword::Given(Some(password)),
            None,
            Duration::ZERO,
        ),
    );
    assert!(
        matches!(asked, Ok(JoinOutcome::Pending { .. })),
        "{asked:?}"
    );
    let url = format!("http://{}/v1/team", server.addr);
    let recipient = read_identity(&mb)
        .expect("readable")
        .expect("a key")
        .recipient()
        .to_bech32();
    let proof = load_state(&mb).expect("state").proof.expect("a proof");
    // A verdict that names no proof the request carries is not kept.
    let other = join::proof_tag("another proof");
    assert!(transport::post_verdict(&url, &recipient, &other).is_err());
    assert!(!read_verdict(&mb).expect("asked"));

    // Anyone on the LAN can post one that does: the Mac then asks its person again ...
    transport::post_verdict(&url, &recipient, &join::proof_tag(&proof)).expect("kept");
    assert!(read_verdict(&mb).expect("asked"));
    assert!(password_refused(&mb));
    // ... and nothing else changes: the request is still queued, and an admin Mac lets it in.
    assert_eq!(auto_approve(&admin).expect("approves").approved, ["mb"]);
    assert!(matches!(
        sync(&mb, None).expect("sync"),
        SyncOutcome::Applied { .. }
    ));
    assert!(!password_refused(&mb), "a member is not refused");
    server.stop();
}

#[test]
fn an_admin_mac_records_each_mac_left_to_a_person_and_approves_exactly_the_one_named() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let server = serve_holding(&root.path().join("store"), Duration::from_millis(200));
    let (admin, source, password) = team(root.path(), &server);
    let ask = |host: &str, password: Option<SecretBytes>| {
        let machine = Paths::for_home(&root.path().join(host));
        let asked = join(
            &machine,
            &JoinOptions {
                host: Some(host.to_string()),
                ..options(&source, JoinPassword::Given(password), None, Duration::ZERO)
            },
        );
        assert!(
            matches!(asked, Ok(JoinOutcome::Pending { .. })),
            "{asked:?}"
        );
        let recipient = read_identity(&machine)
            .expect("readable")
            .expect("a key")
            .recipient()
            .to_bech32();
        (machine, recipient)
    };
    let (mb, mb_key) = ask("mb", None);
    let (mc, mc_key) = ask("mc", Some(SecretBytes::from("not-the-password".as_bytes())));
    let (md, _) = ask("md", Some(password));
    // CONTROL: nothing is recorded before the approver reads the queue.
    assert_eq!(asking(&admin), []);

    let mut approver = Approver::default();
    approver_pass(&admin, &|_| {}, &mut approver);
    let hosts = |admin: &Paths| {
        let mut asked: Vec<(String, String)> = asking(admin)
            .into_iter()
            .map(|asking| (asking.host, asking.recipient))
            .collect();
        asked.sort();
        asked
    };
    // The Mac with no password and the one with a wrong one wait for a person; the right
    // password let `md` in.
    assert_eq!(
        hosts(&admin),
        [
            ("mb".to_string(), mb_key.clone()),
            ("mc".to_string(), mc_key.clone())
        ]
    );
    assert!(matches!(
        sync(&md, None).expect("sync"),
        SyncOutcome::Applied { .. }
    ));

    // Approve names one key: that Mac is in, and the other still waits.
    assert_eq!(approve_request(&admin, &mb_key).expect("approves"), "mb");
    assert!(matches!(
        sync(&mb, None).expect("sync"),
        SyncOutcome::Applied { .. }
    ));
    assert!(matches!(sync(&mc, None).expect("sync"), SyncOutcome::NotIn));
    approver_pass(&admin, &|_| {}, &mut approver);
    assert_eq!(hosts(&admin), [("mc".to_string(), mc_key.clone())]);
    // A key that does not ask is never let in.
    assert!(approve_request(&admin, &mb_key).is_err());
    // Anyone on the LAN can queue a waiting key under a name of its choosing: Approve then lets
    // it in under neither, and names both, as `aterm secrets approve <code>` does. Negative
    // control: approving by the key alone takes the first name queued and lets `mc` in.
    let code = join_code(
        &read_identity(&mc)
            .expect("readable")
            .expect("a key")
            .recipient(),
    );
    let forged = format!("{mc_key} evil {code}");
    let peer = "192.0.2.66".parse().expect("an address");
    assert_eq!(
        crate::server::post_join_as(&server, "team", &forged, peer),
        204
    );
    let refused = approve_request(&admin, &mc_key).expect_err("two names");
    assert!(
        refused.to_string().contains("under 2 names (evil, mc)"),
        "{refused}"
    );
    assert!(matches!(sync(&mc, None).expect("sync"), SyncOutcome::NotIn));
    server.stop();
}

#[test]
fn a_vault_approved_only_by_code_records_every_asking_mac_until_it_is_in() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let server = serve_holding(&root.path().join("store"), Duration::from_millis(200));
    let (admin, source, _) = team(root.path(), &server);
    let opened = admin_open(&admin).expect("admin opens");
    let mut doc = opened.doc.clone();
    doc.set_join_password(None).expect("no join password");
    admin_publish(&admin, &opened, &doc).expect("published");
    let mb = Paths::for_home(&root.path().join("mb"));
    let asked = join(
        &mb,
        &options(&source, JoinPassword::Given(None), None, Duration::ZERO),
    );
    assert!(
        matches!(asked, Ok(JoinOutcome::Pending { .. })),
        "{asked:?}"
    );
    let key = read_identity(&mb)
        .expect("readable")
        .expect("a key")
        .recipient()
        .to_bech32();

    let mut approver = Approver::default();
    let pause = approver_pass(&admin, &|_| {}, &mut approver);
    assert_eq!(
        pause,
        Duration::ZERO,
        "by code, the next read is a long-poll"
    );
    let recorded: Vec<String> = asking(&admin).into_iter().map(|a| a.recipient).collect();
    assert_eq!(recorded, std::slice::from_ref(&key));
    // That read is held until the queue or the vault moves (here: the server's 200 ms hold),
    // so asking again at once is no loop.
    let started = std::time::Instant::now();
    assert_eq!(
        approver_pass(&admin, &|_| {}, &mut approver),
        Duration::ZERO
    );
    assert!(
        started.elapsed() >= Duration::from_millis(200),
        "the queue was read without the server holding it"
    );
    // The queue still holds the request once the Mac is in: the new version, not the queue,
    // takes it off the record.
    approve_request(&admin, &key).expect("approves");
    approver_pass(&admin, &|_| {}, &mut approver);
    assert_eq!(asking(&admin), []);
    assert!(
        requests_for(&config(&admin).expect("config").expect("one"), None)
            .expect("listed")
            .iter()
            .any(|request| request.recipient == key),
        "CONTROL: the server still lists it"
    );
    server.stop();
}

/// The record of the Macs asking names the approver that wrote it: once that process is gone
/// (the subscriber turned off, stopped or killed), nobody is shown asking, as nothing keeps the
/// record current. A record that names no process (an approver before the head line) is read
/// as it stands.
#[test]
fn a_record_of_macs_asking_ends_with_the_approver_that_wrote_it() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let paths = Paths::for_home(root.path());
    let one = Asking {
        code: "ABCD-EFGH".to_string(),
        recipient: "age1inert".to_string(),
        host: "mb".to_string(),
    };
    write_asking(&paths, std::slice::from_ref(&one)).expect("written");
    assert_eq!(
        asking(&paths),
        std::slice::from_ref(&one),
        "its approver runs"
    );
    assert_eq!(asking_with(&paths, &|_| false), [], "its approver gone");
    let row = format!("{}\t{}\t{}\n", one.code, one.recipient, one.host);
    let named = |head: &str| {
        fsx::write_atomic(
            &paths.asking_file(),
            format!("{ASKING_BY}{head}\n{row}").as_bytes(),
            0o600,
        )
        .expect("written");
        asking(&paths)
    };
    let pid = std::process::id();
    let birth = aterm_uds::process::read_birth(i32::try_from(pid).expect("a pid"))
        .expect("this process's start")
        .0;
    assert_eq!(named(&format!("{pid}@{birth}")), std::slice::from_ref(&one));
    assert_eq!(
        named(&format!("{pid}@{}", birth + 1)),
        [],
        "another process under the same pid"
    );
    assert_eq!(
        named(&pid.to_string()),
        std::slice::from_ref(&one),
        "a pid alone"
    );
    assert_eq!(named("nobody"), [], "no process named");
    let legacy = row;
    fsx::write_atomic(&paths.asking_file(), legacy.as_bytes(), 0o600).expect("written");
    assert_eq!(
        asking_with(&paths, &|_| false),
        std::slice::from_ref(&one),
        "no approver named"
    );
    write_asking(&paths, &[]).expect("cleared");
    assert!(!paths.asking_file().exists());
}

#[test]
fn a_mac_let_go_is_no_mac_asking_to_join_until_it_asks_again() {
    #[derive(Debug, Clone, Copy)]
    enum Mode {
        Password,
        Open,
        Code,
    }
    // The server's clock, moved on to move a re-asked request's time.
    static AHEAD: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    fn clock() -> u64 {
        aterm_time::unix_secs().unwrap_or(0) + AHEAD.load(std::sync::atomic::Ordering::SeqCst)
    }
    for mode in [Mode::Password, Mode::Open, Mode::Code] {
        AHEAD.store(0, std::sync::atomic::Ordering::SeqCst);
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let server = crate::server::start_with_clock(
            std::net::TcpListener::bind("127.0.0.1:0").expect("bind"),
            &crate::server::Options {
                listen: "127.0.0.1:0".parse().expect("addr"),
                store: root.path().join("store"),
                advertise: None,
                pins: Vec::new(),
                hold: Duration::from_millis(200),
                keep_awake: false,
            },
            clock,
        )
        .expect("serving");
        let (admin, source, password) = team(root.path(), &server);
        let opened = admin_open(&admin).expect("admin opens");
        let mut doc = opened.doc.clone();
        match mode {
            Mode::Password => {}
            Mode::Open => doc.set_open(true).expect("open"),
            Mode::Code => doc.set_join_password(None).expect("no join password"),
        }
        if !matches!(mode, Mode::Password) {
            admin_publish(&admin, &opened, &doc).expect("published");
        }
        let mb = Paths::for_home(&root.path().join("mb"));
        let given = match mode {
            Mode::Password => JoinPassword::Given(Some(password)),
            Mode::Open | Mode::Code => JoinPassword::Given(None),
        };
        let asked = join(&mb, &options(&source, given, None, Duration::ZERO));
        assert!(
            matches!(asked, Ok(JoinOutcome::Pending { .. })),
            "{mode:?}: {asked:?}"
        );
        let key = read_identity(&mb)
            .expect("readable")
            .expect("a key")
            .recipient()
            .to_bech32();
        let mut approver = Approver::default();
        approver_pass(&admin, &|_| {}, &mut approver);
        if matches!(mode, Mode::Code) {
            approve_request(&admin, &key).expect("approves");
        }
        // The version that let it in is read with its request still queued.
        approver_pass(&admin, &|_| {}, &mut approver);
        assert!(
            matches!(sync(&mb, None).expect("sync"), SyncOutcome::Applied { .. }),
            "{mode:?}"
        );
        assert_eq!(asking(&admin), [], "{mode:?}");

        let opened = admin_open(&admin).expect("admin opens");
        admin_revoke(&admin, &opened, "mb", false).expect("revoked");
        let queued = requests_for(&config(&admin).expect("config").expect("one"), None)
            .expect("listed")
            .into_iter()
            .find(|request| request.recipient == key)
            .expect("CONTROL: the server still lists the request that let it in");
        approver_pass(&admin, &|_| {}, &mut approver);
        assert_eq!(asking(&admin), [], "{mode:?}: the Mac let go asked nothing");
        // An approver started again knows it too.
        let mut again = Approver::resumed(&admin);
        approver_pass(&admin, &|_| {}, &mut again);
        assert_eq!(asking(&admin), [], "{mode:?}: after a restart");

        // The Mac asks again as it asked before: within the hour the server keeps its request as
        // it stands, and once its time moves, a person approves it by hand.
        let line = format!(
            "{} {} {}{}",
            queued.recipient,
            queued.host,
            queued.code,
            queued
                .proof
                .as_ref()
                .map(|proof| format!(" proof={proof}"))
                .unwrap_or_default()
        );
        let url = format!("http://{}/v1/team", server.addr);
        transport::post_join(&url, &line).expect("asks again");
        approver_pass(&admin, &|_| {}, &mut again);
        assert_eq!(asking(&admin), [], "{mode:?}: the request as it stood");
        AHEAD.store(
            crate::server::JOIN_REFRESH_EVERY_SECS + 1,
            std::sync::atomic::Ordering::SeqCst,
        );
        transport::post_join(&url, &line).expect("asks again, an hour on");
        approver_pass(&admin, &|_| {}, &mut again);
        let recorded: Vec<String> = asking(&admin).into_iter().map(|a| a.recipient).collect();
        assert_eq!(
            recorded,
            std::slice::from_ref(&key),
            "{mode:?}: asked again"
        );
        server.stop();
    }
}

/// An approver that never read a request while its key was in the vault (a new admin Mac's, or
/// one started on a build before its record of answered requests) takes the request of a key an
/// open vault let go, still queued, for no ask: the `code-only` line says a version let it go.
/// Once the Mac asks again and the server moves its request's time, a person is asked.
#[test]
fn a_fresh_approver_shows_no_mac_let_go_from_an_open_vault_until_it_asks_again() {
    static AHEAD: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    fn clock() -> u64 {
        aterm_time::unix_secs().unwrap_or(0) + AHEAD.load(std::sync::atomic::Ordering::SeqCst)
    }
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let server = crate::server::start_with_clock(
        std::net::TcpListener::bind("127.0.0.1:0").expect("bind"),
        &crate::server::Options {
            listen: "127.0.0.1:0".parse().expect("addr"),
            store: root.path().join("store"),
            advertise: None,
            pins: Vec::new(),
            hold: Duration::from_millis(200),
            keep_awake: false,
        },
        clock,
    )
    .expect("serving");
    let admin = Paths::for_home(&root.path().join("ma"));
    let url = format!("http://{}/v1/team", server.addr);
    let created = create(&admin, &url, None, Some("ma")).expect("create");
    let source = format!("{url}#{}", created.signer);
    let mb = Paths::for_home(&root.path().join("mb"));
    let asked = join(
        &mb,
        &options(&source, JoinPassword::Given(None), None, Duration::ZERO),
    );
    assert!(
        matches!(asked, Ok(JoinOutcome::Pending { .. })),
        "{asked:?}"
    );
    let key = read_identity(&mb)
        .expect("readable")
        .expect("a key")
        .recipient()
        .to_bech32();
    approver_pass(&admin, &|_| {}, &mut Approver::default());
    assert!(
        matches!(sync(&mb, None).expect("sync"), SyncOutcome::Applied { .. }),
        "the open vault let it in"
    );
    let opened = admin_open(&admin).expect("admin opens");
    admin_revoke(&admin, &opened, "mb", false).expect("revoked");
    assert_eq!(
        admin_open(&admin).expect("admin opens").doc.code_only(),
        std::slice::from_ref(&key),
        "CONTROL: the open vault names the key it let go"
    );
    let queued = requests_for(&config(&admin).expect("config").expect("one"), None)
        .expect("listed")
        .into_iter()
        .find(|request| request.recipient == key)
        .expect("CONTROL: the server still lists the request that let it in");

    // An approver with no record of the requests it read while the key was in the vault.
    forget_record(&admin);
    let mut fresh = Approver::resumed(&admin);
    approver_pass(&admin, &|_| {}, &mut fresh);
    assert_eq!(asking(&admin), [], "the Mac let go asked nothing");
    approver_pass(&admin, &|_| {}, &mut Approver::resumed(&admin));
    assert_eq!(asking(&admin), [], "an approver started again knows it too");

    let line = format!("{} {} {}", queued.recipient, queued.host, queued.code);
    AHEAD.store(
        crate::server::JOIN_REFRESH_EVERY_SECS + 1,
        std::sync::atomic::Ordering::SeqCst,
    );
    transport::post_join(&url, &line).expect("asks again, an hour on");
    approver_pass(&admin, &|_| {}, &mut fresh);
    let recorded: Vec<String> = asking(&admin).into_iter().map(|a| a.recipient).collect();
    assert_eq!(recorded, std::slice::from_ref(&key), "asked again");
    server.stop();
}

/// A Mac waiting as an admin opens the vault is held on a `code-only` line for a person to
/// approve by code, and stays shown across restarts: the approver showed it as the line appeared,
/// and its record keeps the keys it knew on such lines, so it never takes the request for
/// answered.
#[test]
fn a_mac_held_as_the_vault_opens_stays_shown_after_the_approver_restarts() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let server = serve_holding(&root.path().join("store"), Duration::from_millis(200));
    let admin = Paths::for_home(&root.path().join("ma"));
    let url = format!("http://{}/v1/team", server.addr);
    let created = test_create_with_password(&admin, &url, None, Some("ma")).expect("create");
    let source = format!("{url}#{}", created.signer);
    let mb = Paths::for_home(&root.path().join("mb"));
    let asked = join(
        &mb,
        &options(&source, JoinPassword::Given(None), None, Duration::ZERO),
    );
    assert!(
        matches!(asked, Ok(JoinOutcome::Pending { .. })),
        "{asked:?}"
    );
    let key = read_identity(&mb)
        .expect("readable")
        .expect("a key")
        .recipient()
        .to_bech32();
    let shown =
        |admin: &Paths| -> Vec<String> { asking(admin).into_iter().map(|a| a.recipient).collect() };
    let mut approver = Approver::resumed(&admin);
    approver_pass(&admin, &|_| {}, &mut approver);
    assert_eq!(
        shown(&admin),
        std::slice::from_ref(&key),
        "no proof: asking"
    );

    let opened = admin_open(&admin).expect("admin opens");
    let mut doc = opened.doc.clone();
    doc.set_open(true).expect("open");
    admin_publish(&admin, &opened, &doc).expect("published");
    assert_eq!(
        admin_open(&admin).expect("admin opens").doc.code_only(),
        std::slice::from_ref(&key),
        "CONTROL: opening holds the Mac waiting"
    );
    approver_pass(&admin, &|_| {}, &mut approver);
    assert_eq!(shown(&admin), std::slice::from_ref(&key), "held: asking");
    approver_pass(&admin, &|_| {}, &mut Approver::resumed(&admin));
    assert_eq!(
        shown(&admin),
        std::slice::from_ref(&key),
        "an approver started again still shows it"
    );
    server.stop();
}

/// An admin Mac whose approver did not read the vault while a Mac came and went (off, asleep, or
/// no admin then) shows no ghost when it reads the vault again: the key is new on a `code-only`
/// line to its record, so the request that let the Mac in is taken for answered.
#[test]
fn an_approver_away_while_a_mac_came_and_went_shows_no_ghost() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let server = serve_holding(&root.path().join("store"), Duration::from_millis(200));
    let admin = Paths::for_home(&root.path().join("ma"));
    let url = format!("http://{}/v1/team", server.addr);
    let created = create(&admin, &url, None, Some("ma")).expect("create");
    let source = format!("{url}#{}", created.signer);
    approver_pass(&admin, &|_| {}, &mut Approver::resumed(&admin));
    assert!(
        read_answered(&admin).begun.is_some(),
        "CONTROL: the approver's record is begun on the vault"
    );

    // Away: a Mac asks, is let in and is let go, with no pass of this approver.
    let mb = Paths::for_home(&root.path().join("mb"));
    let asked = join(
        &mb,
        &options(&source, JoinPassword::Given(None), None, Duration::ZERO),
    );
    assert!(
        matches!(asked, Ok(JoinOutcome::Pending { .. })),
        "{asked:?}"
    );
    let key = read_identity(&mb)
        .expect("readable")
        .expect("a key")
        .recipient()
        .to_bech32();
    assert_eq!(
        auto_approve(&admin).expect("approved").approved,
        ["mb"],
        "CONTROL: let in"
    );
    let opened = admin_open(&admin).expect("admin opens");
    admin_revoke(&admin, &opened, "mb", false).expect("revoked");
    assert!(
        requests_for(&config(&admin).expect("config").expect("one"), None)
            .expect("listed")
            .iter()
            .any(|request| request.recipient == key),
        "CONTROL: the server still lists the request that let it in"
    );

    approver_pass(&admin, &|_| {}, &mut Approver::resumed(&admin));
    assert_eq!(asking(&admin), [], "the Mac let go asked nothing");
    server.stop();
}

/// An approver that showed a Mac as asking, then read nothing (asleep, or off the LAN) while
/// another admin let it in by code and let it go, shows no ghost when it reads the vault again:
/// what it showed at a version before the one just before says nothing of that Mac now.
#[test]
fn an_approver_that_showed_a_mac_then_slept_through_its_stay_shows_no_ghost() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let server = serve_holding(&root.path().join("store"), Duration::from_millis(200));
    let (admin, source, _password) = team(root.path(), &server);
    let mb = Paths::for_home(&root.path().join("mb"));
    let asked = join(
        &mb,
        &options(&source, JoinPassword::Given(None), None, Duration::ZERO),
    );
    assert!(
        matches!(asked, Ok(JoinOutcome::Pending { .. })),
        "{asked:?}"
    );
    let key = read_identity(&mb)
        .expect("readable")
        .expect("a key")
        .recipient()
        .to_bech32();
    let mut approver = Approver::resumed(&admin);
    approver_pass(&admin, &|_| {}, &mut approver);
    let shown: Vec<String> = asking(&admin).into_iter().map(|a| a.recipient).collect();
    assert_eq!(
        shown,
        std::slice::from_ref(&key),
        "CONTROL: no proof: asking"
    );

    // Asleep: another admin lets it in by its code and lets it go. That admin records the
    // request on its own Mac, not on this one.
    let opened = admin_open(&admin).expect("admin opens");
    let mut doc = opened.doc.clone();
    doc.add_machine("mb", &key).expect("added");
    admin_publish_approved(&admin, &opened, &doc, std::slice::from_ref(&key))
        .expect("approved by code");
    assert!(
        approved_recipients(&admin).is_empty(),
        "CONTROL: this Mac recorded no approval"
    );
    let opened = admin_open(&admin).expect("admin opens");
    admin_revoke(&admin, &opened, "mb", false).expect("revoked");

    approver_pass(&admin, &|_| {}, &mut approver);
    assert_eq!(asking(&admin), [], "the Mac let go asked nothing");
    server.stop();
}

/// A vault with a join password names a Mac it lets go on a `code-only` line too, so an approver
/// with no record of it shows no ghost, and opening the vault later lets the key in only by its
/// code. Its person is told of the new password, not of an open vault's line.
#[test]
fn a_mac_let_go_from_a_closed_vault_is_named_code_only() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let server = serve_holding(&root.path().join("store"), Duration::from_millis(200));
    let (admin, source, password) = team(root.path(), &server);
    let mb = Paths::for_home(&root.path().join("mb"));
    let asked = join(
        &mb,
        &options(
            &source,
            JoinPassword::Given(Some(password)),
            None,
            Duration::ZERO,
        ),
    );
    assert!(
        matches!(asked, Ok(JoinOutcome::Pending { .. })),
        "{asked:?}"
    );
    let key = read_identity(&mb)
        .expect("readable")
        .expect("a key")
        .recipient()
        .to_bech32();
    approver_pass(&admin, &|_| {}, &mut Approver::resumed(&admin));
    assert!(
        admin_open(&admin)
            .expect("admin opens")
            .doc
            .machines()
            .iter()
            .any(|machine| machine.recipient == key),
        "CONTROL: the password let it in"
    );

    let opened = admin_open(&admin).expect("admin opens");
    let done = admin_revoke(&admin, &opened, "mb", false)
        .expect("revoked")
        .published;
    assert!(done.password_renewed && !done.kept_out, "{done:?}");
    let after = admin_open(&admin).expect("admin opens");
    assert!(!after.doc.open(), "the vault stays closed");
    assert_eq!(after.doc.code_only(), std::slice::from_ref(&key));

    forget_record(&admin);
    approver_pass(&admin, &|_| {}, &mut Approver::resumed(&admin));
    assert_eq!(asking(&admin), [], "the Mac let go asked nothing");
    server.stop();
}

/// A Mac let go that asks again with the renewed join password before the approver reads the
/// version that let it go is held as the request it had standing then, and let in once the
/// server moves its time, within the hour: never shown as asking, never left out for good.
#[test]
fn a_mac_let_go_that_proves_the_renewed_password_is_let_in_within_the_hour() {
    static AHEAD: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    fn clock() -> u64 {
        aterm_time::unix_secs().unwrap_or(0) + AHEAD.load(std::sync::atomic::Ordering::SeqCst)
    }
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let server = crate::server::start_with_clock(
        std::net::TcpListener::bind("127.0.0.1:0").expect("bind"),
        &crate::server::Options {
            listen: "127.0.0.1:0".parse().expect("addr"),
            store: root.path().join("store"),
            advertise: None,
            pins: Vec::new(),
            hold: Duration::from_millis(200),
            keep_awake: false,
        },
        clock,
    )
    .expect("serving");
    let (admin, source, password) = team(root.path(), &server);
    let mb = Paths::for_home(&root.path().join("mb"));
    let asked = join(
        &mb,
        &options(
            &source,
            JoinPassword::Given(Some(password)),
            None,
            Duration::ZERO,
        ),
    );
    assert!(
        matches!(asked, Ok(JoinOutcome::Pending { .. })),
        "{asked:?}"
    );
    let key = read_identity(&mb)
        .expect("readable")
        .expect("a key")
        .recipient()
        .to_bech32();
    let member = |admin: &Paths| {
        admin_open(admin)
            .expect("admin opens")
            .doc
            .machines()
            .iter()
            .any(|machine| machine.recipient == key)
    };
    let mut approver = Approver::resumed(&admin);
    approver_pass(&admin, &|_| {}, &mut approver);
    assert!(member(&admin), "CONTROL: the password let it in");

    // Away: the Mac is let go, and asks again with the password that version made, past the
    // server's 10 s between a key's changes.
    let opened = admin_open(&admin).expect("admin opens");
    admin_revoke(&admin, &opened, "mb", false).expect("revoked");
    let renewed = admin_open(&admin)
        .expect("admin opens")
        .doc
        .join_password()
        .expect("the vault keeps a join password");
    AHEAD.store(11, std::sync::atomic::Ordering::SeqCst);
    let asked = join(
        &mb,
        &options(
            &source,
            JoinPassword::Given(Some(renewed)),
            None,
            Duration::ZERO,
        ),
    );
    assert!(
        matches!(asked, Ok(JoinOutcome::Pending { .. })),
        "{asked:?}"
    );
    approver_pass(&admin, &|_| {}, &mut approver);
    assert!(!member(&admin), "held as the request it had standing");
    assert_eq!(asking(&admin), [], "and not shown");

    let queued = requests_for(&config(&admin).expect("config").expect("one"), None)
        .expect("listed")
        .into_iter()
        .find(|request| request.recipient == key)
        .expect("CONTROL: the server lists its new request");
    let line = format!(
        "{} {} {} proof={}",
        queued.recipient,
        queued.host,
        queued.code,
        queued.proof.expect("CONTROL: a proof")
    );
    AHEAD.store(
        11 + crate::server::JOIN_REFRESH_EVERY_SECS + 1,
        std::sync::atomic::Ordering::SeqCst,
    );
    let url = format!("http://{}/v1/team", server.addr);
    transport::post_join(&url, &line).expect("asks again, an hour on");
    approver_pass(&admin, &|_| {}, &mut approver);
    assert!(member(&admin), "let in once its time moved");
    server.stop();
}

/// A request a Mac joined with stays held once the vault lets the Mac go, even after an admin
/// sets the password it proved again: a proof says nothing of when it was made.
#[test]
fn a_mac_let_go_is_not_let_back_in_when_its_old_password_is_set_again() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let server = serve_holding(&root.path().join("store"), Duration::from_millis(200));
    let (admin, source, password) = team(root.path(), &server);
    let mb = Paths::for_home(&root.path().join("mb"));
    let asked = join(
        &mb,
        &options(
            &source,
            JoinPassword::Given(Some(password.clone())),
            None,
            Duration::ZERO,
        ),
    );
    assert!(
        matches!(asked, Ok(JoinOutcome::Pending { .. })),
        "{asked:?}"
    );
    let key = read_identity(&mb)
        .expect("readable")
        .expect("a key")
        .recipient()
        .to_bech32();
    let member = |admin: &Paths| {
        admin_open(admin)
            .expect("admin opens")
            .doc
            .machines()
            .iter()
            .any(|machine| machine.recipient == key)
    };
    let mut approver = Approver::resumed(&admin);
    approver_pass(&admin, &|_| {}, &mut approver);
    assert!(member(&admin), "CONTROL: the password let it in");
    let opened = admin_open(&admin).expect("admin opens");
    admin_revoke(&admin, &opened, "mb", false).expect("revoked");
    approver_pass(&admin, &|_| {}, &mut approver);
    assert!(!member(&admin), "CONTROL: let go");

    // The team's usual password, set again.
    let opened = admin_open(&admin).expect("admin opens");
    let mut doc = opened.doc.clone();
    doc.set_join_password(Some(password.as_slice()))
        .expect("the old password");
    admin_publish(&admin, &opened, &doc).expect("published");
    approver_pass(&admin, &|_| {}, &mut approver);
    assert!(!member(&admin), "the request it joined with is no ask");
    assert_eq!(asking(&admin), []);
    approver_pass(&admin, &|_| {}, &mut Approver::resumed(&admin));
    assert!(!member(&admin), "nor after a restart");
    server.stop();
}

/// A Mac this admin Mac lets in by its code is recorded as it is let in, so an approver that
/// read no version while it was in (the subscriber off, or stuck) shows no ghost once another
/// version lets it go, even for a key a `code-only` line named before: it took the request it
/// let in for answered.
#[test]
fn a_mac_let_in_by_code_while_the_approver_was_away_is_no_ghost_once_let_go() {
    static AHEAD: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    fn clock() -> u64 {
        aterm_time::unix_secs().unwrap_or(0) + AHEAD.load(std::sync::atomic::Ordering::SeqCst)
    }
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let server = crate::server::start_with_clock(
        std::net::TcpListener::bind("127.0.0.1:0").expect("bind"),
        &crate::server::Options {
            listen: "127.0.0.1:0".parse().expect("addr"),
            store: root.path().join("store"),
            advertise: None,
            pins: Vec::new(),
            hold: Duration::from_millis(200),
            keep_awake: false,
        },
        clock,
    )
    .expect("serving");
    let (admin, source, _password) = team(root.path(), &server);
    let mb = Paths::for_home(&root.path().join("mb"));
    let asked = join(
        &mb,
        &options(&source, JoinPassword::Given(None), None, Duration::ZERO),
    );
    assert!(
        matches!(asked, Ok(JoinOutcome::Pending { .. })),
        "{asked:?}"
    );
    let key = read_identity(&mb)
        .expect("readable")
        .expect("a key")
        .recipient()
        .to_bech32();
    let mut approver = Approver::resumed(&admin);
    approver_pass(&admin, &|_| {}, &mut approver);
    approve_request(&admin, &key).expect("approved by code");
    approver_pass(&admin, &|_| {}, &mut approver);
    let opened = admin_open(&admin).expect("admin opens");
    admin_revoke(&admin, &opened, "mb", false).expect("revoked");
    approver_pass(&admin, &|_| {}, &mut approver);
    assert_eq!(asking(&admin), [], "CONTROL: the Mac let go asked nothing");

    // It asks again an hour on, and is shown: its key is on a `code-only` line the approver knew.
    let queued = requests_for(&config(&admin).expect("config").expect("one"), None)
        .expect("listed")
        .into_iter()
        .find(|request| request.recipient == key)
        .expect("CONTROL: the server still lists its request");
    AHEAD.store(
        crate::server::JOIN_REFRESH_EVERY_SECS + 1,
        std::sync::atomic::Ordering::SeqCst,
    );
    let url = format!("http://{}/v1/team", server.addr);
    let line = format!("{} {} {}", queued.recipient, queued.host, queued.code);
    transport::post_join(&url, &line).expect("asks again, an hour on");
    approver_pass(&admin, &|_| {}, &mut approver);
    let shown: Vec<String> = asking(&admin).into_iter().map(|a| a.recipient).collect();
    assert_eq!(shown, std::slice::from_ref(&key), "CONTROL: asked again");

    // Away: this Mac lets it in by its code, and a version lets it go.
    approve_request(&admin, &key).expect("approved by code again");
    assert_eq!(
        approved_recipients(&admin),
        std::slice::from_ref(&key),
        "the record keeps the request let in, not the one the server no longer lists"
    );
    let opened = admin_open(&admin).expect("admin opens");
    admin_revoke(&admin, &opened, "mb", false).expect("revoked again");

    approver_pass(&admin, &|_| {}, &mut approver);
    assert_eq!(asking(&admin), [], "the running approver shows no ghost");
    approver_pass(&admin, &|_| {}, &mut Approver::resumed(&admin));
    assert_eq!(asking(&admin), [], "nor does one started again");
    server.stop();
}

/// The approver record keeps the requests it answered in `answered`, which a build before its
/// queue file reads under 64 KiB, and the queue and its `code-only` keys in that file, however
/// many: a large vault's record still reads whole there. A record of 2026-10-26, with those
/// lines at the head of `answered`, still reads, and its next write moves them.
#[test]
fn an_approver_record_keeps_its_requests_where_a_build_before_reads_them() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let admin = Paths::for_home(&root.path().join("ma"));
    let seen: std::collections::BTreeSet<String> =
        (0..4000).map(|n| format!("age1{n:058}")).collect();
    let answered: std::collections::BTreeSet<Answered> = (0..ANSWERED_MOST as u64)
        .map(|n| Answered {
            recipient: format!("age1{n:058}"),
            host: format!("mac-{n}"),
            at: 1_790_000_000 + n,
        })
        .collect();
    write_answered(&admin, &answered, Some("team http://x/v1/team"), &seen).expect("written");
    assert!(
        fsx::read_private(&admin.answered_file(), 64 << 10)
            .expect("readable")
            .is_some(),
        "a build before reads its requests"
    );
    let record = read_answered(&admin);
    assert_eq!(record.answered, answered);
    assert_eq!(record.begun.as_deref(), Some("team http://x/v1/team"));
    assert_eq!(record.seen, seen);

    // A record a build of 2026-10-26 wrote over this one (a rollback): its queue and keys at the
    // head of `answered`, newer than the queue file left beside it.
    let key = format!("age1{:058}", 7);
    let text = format!(
        "{ANSWERED_BEGUN}team http://y/v1/team\n{ANSWERED_SEEN}{key}\n{key}\t1790000001\tmb\n"
    );
    fsx::write_atomic(&admin.answered_file(), text.as_bytes(), 0o600).expect("written");
    assert!(
        admin.answered_queue_file().exists(),
        "CONTROL: the queue file stays"
    );
    let record = read_answered(&admin);
    assert_eq!(record.begun.as_deref(), Some("team http://y/v1/team"));
    assert_eq!(record.seen, std::collections::BTreeSet::from([key.clone()]));
    let requests: Vec<&str> = record.answered.iter().map(|a| a.host.as_str()).collect();
    assert_eq!(requests, ["mb"]);
    write_answered(
        &admin,
        &record.answered,
        record.begun.as_deref(),
        &record.seen,
    )
    .expect("written");
    let requests = std::fs::read_to_string(admin.answered_file()).expect("requests");
    assert!(!requests.contains(ANSWERED_BEGUN), "{requests}");
    assert_eq!(read_answered(&admin), record);
}

#[test]
fn an_approver_rests_once_a_member_has_two_requests_under_one_name() {
    static AHEAD: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    fn clock() -> u64 {
        aterm_time::unix_secs().unwrap_or(0) + AHEAD.load(std::sync::atomic::Ordering::SeqCst)
    }
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let server = crate::server::start_with_clock(
        std::net::TcpListener::bind("127.0.0.1:0").expect("bind"),
        &crate::server::Options {
            listen: "127.0.0.1:0".parse().expect("addr"),
            store: root.path().join("store"),
            advertise: None,
            pins: Vec::new(),
            hold: Duration::from_millis(200),
            keep_awake: false,
        },
        clock,
    )
    .expect("serving");
    let (admin, source, password) = team(root.path(), &server);
    let mb = Paths::for_home(&root.path().join("mb"));
    let asked = join(
        &mb,
        &options(
            &source,
            JoinPassword::Given(Some(password)),
            None,
            Duration::ZERO,
        ),
    );
    assert!(
        matches!(asked, Ok(JoinOutcome::Pending { .. })),
        "{asked:?}"
    );
    let key = read_identity(&mb)
        .expect("readable")
        .expect("a key")
        .recipient();
    // The same key asks under the same name from another address, a while on, with a wrong
    // proof (a mistyped password, or a peer that read the listing): a second request with its
    // own time. Another Mac's wrong proof is refused on every pass that reads the queue.
    let wrong = crate::b64::encode_padded(&[7u8; 32]);
    AHEAD.store(10, std::sync::atomic::Ordering::SeqCst);
    let other = |recipient: &Recipient, host: &str, from: &str| {
        let line = format!(
            "{} {host} {} proof={wrong}",
            recipient.to_bech32(),
            join_code(recipient)
        );
        let status =
            crate::server::post_join_as(&server, "team", &line, from.parse().expect("an address"));
        assert_eq!(status, 204, "{host} queued");
    };
    other(&key, "mb", "192.0.2.66");
    let stranger = machine_identity(&Paths::for_home(&root.path().join("ml")))
        .expect("a key")
        .recipient();
    other(&stranger, "ml", "192.0.2.77");

    let logged = RefCell::new(Vec::<String>::new());
    let log = |line: &str| logged.borrow_mut().push(line.to_string());
    let mut approver = Approver::default();
    approver_pass(&admin, &log, &mut approver);
    approver_pass(&admin, &log, &mut approver);
    assert!(
        logged
            .borrow()
            .iter()
            .any(|line| line.starts_with("approved mb")),
        "{:?}",
        logged.borrow()
    );
    assert!(
        logged
            .borrow()
            .iter()
            .any(|line| line.starts_with("refused ml")),
        "CONTROL: a pass that reads the queue says so: {:?}",
        logged.borrow()
    );
    logged.borrow_mut().clear();
    for _ in 0..3 {
        approver_pass(&admin, &log, &mut approver);
    }
    assert_eq!(*logged.borrow(), Vec::<String>::new(), "the approver rests");
    server.stop();
}

#[test]
fn an_approver_rests_while_a_mac_let_go_asks_again_under_another_proof() {
    static AHEAD: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    fn clock() -> u64 {
        aterm_time::unix_secs().unwrap_or(0) + AHEAD.load(std::sync::atomic::Ordering::SeqCst)
    }
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let server = crate::server::start_with_clock(
        std::net::TcpListener::bind("127.0.0.1:0").expect("bind"),
        &crate::server::Options {
            listen: "127.0.0.1:0".parse().expect("addr"),
            store: root.path().join("store"),
            advertise: None,
            pins: Vec::new(),
            hold: Duration::from_millis(200),
            keep_awake: false,
        },
        clock,
    )
    .expect("serving");
    let (admin, source, password) = team(root.path(), &server);
    let mb = Paths::for_home(&root.path().join("mb"));
    let asked = join(
        &mb,
        &options(
            &source,
            JoinPassword::Given(Some(password)),
            None,
            Duration::ZERO,
        ),
    );
    assert!(
        matches!(asked, Ok(JoinOutcome::Pending { .. })),
        "{asked:?}"
    );
    let key = read_identity(&mb)
        .expect("readable")
        .expect("a key")
        .recipient();
    let logged = RefCell::new(Vec::<String>::new());
    let log = |line: &str| logged.borrow_mut().push(line.to_string());
    let mut approver = Approver::default();
    approver_pass(&admin, &log, &mut approver);
    approver_pass(&admin, &log, &mut approver);
    let opened = admin_open(&admin).expect("admin opens");
    admin_revoke(&admin, &opened, "mb", false).expect("revoked");
    approver_pass(&admin, &log, &mut approver);
    // The Mac let go asks again from another address with a password that no longer holds: its
    // new request stands beside the one that let it in, and is refused.
    let line = format!(
        "{} mb {} proof={}",
        key.to_bech32(),
        join_code(&key),
        crate::b64::encode_padded(&[7u8; 32])
    );
    let ask = || {
        let from = "192.0.2.66".parse().expect("an address");
        let status = crate::server::post_join_as(&server, "team", &line, from);
        assert_eq!(status, 204, "queued");
    };
    ask();
    logged.borrow_mut().clear();
    approver_pass(&admin, &log, &mut approver);
    assert!(
        logged
            .borrow()
            .iter()
            .any(|line| line.starts_with("refused mb")),
        "{:?}",
        logged.borrow()
    );
    // An hour on it asks again as before, and the server moves its time: judged already, it
    // starts no pass.
    logged.borrow_mut().clear();
    AHEAD.store(
        crate::server::JOIN_REFRESH_EVERY_SECS + 1,
        std::sync::atomic::Ordering::SeqCst,
    );
    ask();
    for _ in 0..3 {
        approver_pass(&admin, &log, &mut approver);
    }
    assert_eq!(*logged.borrow(), Vec::<String>::new(), "the approver rests");
    server.stop();
}

#[test]
fn an_approver_that_stays_stuck_says_why_once_and_says_when_it_approves_again() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let server = serve_holding(&root.path().join("store"), Duration::from_millis(200));
    let (admin, _, _) = team(root.path(), &server);
    let logged = RefCell::new(Vec::<String>::new());
    let log = |line: &str| logged.borrow_mut().push(line.to_string());
    let mut approver = Approver::default();
    approver_pass(&admin, &log, &mut approver);
    assert_eq!(
        *logged.borrow(),
        Vec::<String>::new(),
        "CONTROL: a working approver says nothing"
    );

    // Another process holds the vault state, so every pass fails the same way.
    let held = StateLock::take(&admin).expect("the state lock");
    approver_pass(&admin, &log, &mut approver);
    assert_eq!(
        *logged.borrow(),
        Vec::<String>::new(),
        "one failing pass is a collision"
    );
    assert_eq!(approver_stuck(&admin, std::process::id()), None);
    approver_pass(&admin, &log, &mut approver);
    approver_pass(&admin, &log, &mut approver);
    let why = "vault state is being changed; check approval again";
    assert_eq!(*logged.borrow(), [format!("not approving: {why}")]);
    assert_eq!(
        approver_stuck(&admin, std::process::id()),
        Some(why.to_string())
    );

    drop(held);
    approver_pass(&admin, &log, &mut approver);
    assert_eq!(
        logged.borrow().last().map(String::as_str),
        Some("approving again")
    );
    assert_eq!(approver_stuck(&admin, std::process::id()), None);
    server.stop();
}

#[test]
fn a_stuck_approver_reports_again_only_for_a_failure_that_differs_in_more_than_numbers() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let paths = Paths::for_home(root.path());
    let logged = RefCell::new(Vec::<String>::new());
    let log = |line: &str| logged.borrow_mut().push(line.to_string());
    let mut approver = Approver::default();
    for text in [
        "refused at seq 7",
        "refused at seq 8",
        "refused at seq 9",
        "the server is gone",
    ] {
        approver.trouble(&paths, &log, text.to_string());
    }
    assert_eq!(
        *logged.borrow(),
        [
            "not approving: refused at seq 8",
            "not approving: the server is gone"
        ]
    );
    assert_eq!(
        approver_stuck(&paths, std::process::id()),
        Some("the server is gone".to_string())
    );
    // A record is one line, whatever the failure's text holds.
    approver.trouble(&paths, &log, "a\nb\tc".to_string());
    assert_eq!(
        approver_stuck(&paths, std::process::id()),
        Some("a b c".to_string())
    );
    approver.untroubled(&paths, &log, "approving again");
    assert_eq!(approver_stuck(&paths, std::process::id()), None);
    approver.untroubled(&paths, &log, "approving again");
    assert_eq!(
        logged
            .borrow()
            .iter()
            .filter(|line| *line == "approving again")
            .count(),
        1
    );
}

#[test]
fn an_approver_record_is_its_own_process_s_and_a_failed_removal_is_tried_again() {
    use std::os::unix::fs::PermissionsExt;
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let paths = Paths::for_home(root.path());
    let logged = RefCell::new(Vec::<String>::new());
    let log = |line: &str| logged.borrow_mut().push(line.to_string());
    fsx::ensure_private_dir(&paths.dir).expect("dir");
    // Another live approver's record stays; a gone one's goes.
    let live = std::os::unix::process::parent_id();
    let gone = 999_999_999;
    for pid in [live, gone] {
        std::fs::write(paths.stuck_file(pid), "stuck\n").expect("record");
        std::fs::set_permissions(
            paths.stuck_file(pid),
            std::fs::Permissions::from_mode(0o600),
        )
        .expect("mode");
    }
    clear_dead_stuck(&paths).expect("cleared");
    assert_eq!(approver_stuck(&paths, live), Some("stuck".to_string()));
    assert_eq!(approver_stuck(&paths, gone), None);

    let mut approver = Approver {
        failing: true,
        ..Approver::default()
    };
    approver.trouble(&paths, &log, "the server is gone".to_string());
    let own = std::process::id();
    assert_eq!(
        approver_stuck(&paths, own),
        Some("the server is gone".to_string())
    );
    let mode = |mode| {
        std::fs::set_permissions(&paths.dir, std::fs::Permissions::from_mode(mode)).expect("mode");
    };
    mode(0o500);
    approver.untroubled(&paths, &log, "approving again");
    approver.untroubled(&paths, &log, "approving again");
    mode(0o700);
    let failed = logged
        .borrow()
        .iter()
        .filter(|line| line.starts_with("could not clear the approver's record"))
        .count();
    assert_eq!(failed, 1, "{logged:?}");
    assert_eq!(
        approver_stuck(&paths, own),
        Some("the server is gone".to_string())
    );
    approver.untroubled(&paths, &log, "approving again");
    assert_eq!(
        approver_stuck(&paths, own),
        None,
        "the removal is tried again"
    );
    assert_eq!(approver_stuck(&paths, live), Some("stuck".to_string()));
}

#[test]
fn a_failed_approval_publish_is_reported_on_its_first_pass_unless_the_vault_kept_nothing() {
    use std::os::unix::fs::PermissionsExt;
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let store = root.path().join("store");
    let server = serve_holding(&store, Duration::from_millis(200));
    let (admin, source, password) = team(root.path(), &server);
    let ask = |host: &str| {
        let machine = Paths::for_home(&root.path().join(host));
        let asked = join(
            &machine,
            &JoinOptions {
                host: Some(host.to_string()),
                ..options(
                    &source,
                    JoinPassword::Given(Some(password.clone())),
                    None,
                    Duration::ZERO,
                )
            },
        );
        assert!(
            matches!(asked, Ok(JoinOutcome::Pending { .. })),
            "{asked:?}"
        );
    };
    ask("mb");
    let logged = RefCell::new(Vec::<String>::new());
    let log = |line: &str| logged.borrow_mut().push(line.to_string());
    let mut approver = Approver::default();
    let own = std::process::id();

    // Another publish holds the store, so the server refuses the approval's version before
    // storing it (409), as when another admin's approver let the Mac in first: that publish
    // cannot have landed, so the first pass says nothing and reads again soon. Negative control:
    // reporting every failed publish at once logs here and records the approver stuck.
    let held = crate::transport::PublishLock::take_clearing_dead(&store.join("team.vault"))
        .unwrap_or_else(|_| panic!("the store's publish lock"));
    assert_eq!(
        approver_pass(&admin, &log, &mut approver),
        Duration::from_secs(2)
    );
    assert_eq!(*logged.borrow(), Vec::<String>::new());
    assert_eq!(approver_stuck(&admin, own), None);
    // One that keeps being refused is reported on its second pass.
    assert_eq!(
        approver_pass(&admin, &log, &mut approver),
        Duration::from_secs(30)
    );
    let second = logged.borrow().clone();
    assert_eq!(second.len(), 1, "{second:?}");
    assert!(
        second[0].starts_with("not approving: approving by the join password failed:"),
        "{second:?}"
    );
    assert!(approver_stuck(&admin, own).is_some());
    drop(held);
    approver_pass(&admin, &log, &mut approver);
    assert!(
        logged
            .borrow()
            .iter()
            .any(|line| line == "approved mb by the join password"),
        "{logged:?}"
    );
    assert_eq!(
        logged.borrow().last().map(String::as_str),
        Some("approving again")
    );
    assert_eq!(approver_stuck(&admin, own), None);

    // A refusal said nothing and left the approver as it was, so a pass after it that fails
    // another way (here this Mac's state is busy) is that failure's first, and says nothing
    // either; refused again after it, two passes in a row failed and the second is reported.
    // Negative control: a silent refusal that counts as a failing pass reports the busy state.
    ask("md");
    logged.borrow_mut().clear();
    let held = crate::transport::PublishLock::take_clearing_dead(&store.join("team.vault"))
        .unwrap_or_else(|_| panic!("the store's publish lock"));
    assert_eq!(
        approver_pass(&admin, &log, &mut approver),
        Duration::from_secs(2)
    );
    let busy = StateLock::take(&admin).expect("this Mac's state lock");
    approver_pass(&admin, &log, &mut approver);
    drop(busy);
    assert_eq!(*logged.borrow(), Vec::<String>::new());
    assert_eq!(approver_stuck(&admin, own), None);
    assert_eq!(
        approver_pass(&admin, &log, &mut approver),
        Duration::from_secs(30)
    );
    let again = logged.borrow().clone();
    assert_eq!(again.len(), 1, "{again:?}");
    assert!(
        again[0].starts_with("not approving: approving by the join password failed:"),
        "{again:?}"
    );
    drop(held);
    approver_pass(&admin, &log, &mut approver);
    assert!(
        logged
            .borrow()
            .iter()
            .any(|line| line == "approved md by the join password"),
        "{logged:?}"
    );
    assert_eq!(approver_stuck(&admin, own), None);

    // A store the server cannot write fails the publish another way (500), and a failed publish
    // other than a refusal may have landed (a reply lost after the server took it): the first
    // pass says so.
    ask("mc");
    logged.borrow_mut().clear();
    let mode = |mode: u32| {
        std::fs::set_permissions(&store, std::fs::Permissions::from_mode(mode))
            .expect("store mode");
    };
    mode(0o500);
    let pause = approver_pass(&admin, &log, &mut approver);
    mode(0o700);
    assert_eq!(pause, Duration::from_secs(30));
    let first = logged.borrow().clone();
    assert_eq!(first.len(), 1, "{first:?}");
    assert!(
        first[0].starts_with("not approving: approving by the join password failed:"),
        "{first:?}"
    );
    assert!(approver_stuck(&admin, own).is_some());
    approver_pass(&admin, &log, &mut approver);
    assert!(
        logged
            .borrow()
            .iter()
            .any(|line| line == "approved mc by the join password"),
        "{logged:?}"
    );
    assert_eq!(approver_stuck(&admin, own), None);
    server.stop();
}

#[test]
fn a_member_s_failures_to_read_are_not_an_approver_s_and_one_that_stops_being_admin_says_so() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let server = serve_holding(&root.path().join("store"), Duration::from_millis(200));
    let (admin, _, _) = team(root.path(), &server);
    let logged = RefCell::new(Vec::<String>::new());
    let log = |line: &str| logged.borrow_mut().push(line.to_string());
    let held = StateLock::take(&admin).expect("the state lock");
    let mut member = Approver {
        member: true,
        ..Approver::default()
    };
    for _ in 0..3 {
        approver_pass(&admin, &log, &mut member);
    }
    assert_eq!(*logged.borrow(), Vec::<String>::new());
    assert_eq!(approver_stuck(&admin, std::process::id()), None);
    // CONTROL: the same failures from an approver last read as an admin are reported.
    let mut approver = Approver::default();
    for _ in 0..3 {
        approver_pass(&admin, &log, &mut approver);
    }
    assert_eq!(logged.borrow().len(), 1, "{logged:?}");
    drop(held);

    // A home with no vault: the reported failure is withdrawn without claiming approval.
    let nowhere = Paths::for_home(&root.path().join("nowhere"));
    approver_pass(&nowhere, &log, &mut approver);
    assert_eq!(
        logged.borrow().last().map(String::as_str),
        Some("not approving: this Mac is no admin of a vault now")
    );
    assert!(approver.member);
    server.stop();
}

#[test]
fn an_approver_starting_clears_its_own_pid_s_old_record_and_gone_ones() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let paths = Paths::for_home(root.path());
    fsx::ensure_private_dir(&paths.dir).expect("dir");
    let own = std::process::id();
    for pid in [own, 999_999_999] {
        fsx::write_atomic(&paths.stuck_file(pid), b"old\n", 0o600).expect("record");
    }
    let stop = AtomicBool::new(true);
    approver_loop(&paths, &|_| {}, &stop);
    assert_eq!(approver_stuck(&paths, own), None);
    assert_eq!(approver_stuck(&paths, 999_999_999), None);
}

#[test]
fn a_record_that_failed_to_be_written_is_written_on_a_later_failing_pass() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let paths = Paths::for_home(root.path());
    fsx::ensure_private_dir(&paths.dir).expect("dir");
    let logged = RefCell::new(Vec::<String>::new());
    let log = |line: &str| logged.borrow_mut().push(line.to_string());
    let mut approver = Approver {
        failing: true,
        ..Approver::default()
    };
    // A folder where the record goes: the write cannot rename over it.
    let own = std::process::id();
    std::fs::create_dir(paths.stuck_file(own)).expect("in the way");
    approver.trouble(&paths, &log, "the server is gone".to_string());
    approver.trouble(&paths, &log, "the server is gone".to_string());
    std::fs::remove_dir(paths.stuck_file(own)).expect("out of the way");
    assert_eq!(approver_stuck(&paths, own), None);
    let failed = logged
        .borrow()
        .iter()
        .filter(|line| line.starts_with("could not record why the approver is stuck"))
        .count();
    assert_eq!(failed, 1, "{logged:?}");
    approver.trouble(&paths, &log, "the server is gone".to_string());
    assert_eq!(
        approver_stuck(&paths, own),
        Some("the server is gone".to_string())
    );
    assert_eq!(
        logged
            .borrow()
            .iter()
            .filter(|line| line.starts_with("not approving"))
            .count(),
        1
    );
}

#[test]
fn a_join_whose_request_is_not_heard_keeps_its_proof_for_the_next_ask() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let server = serve_holding(&root.path().join("store"), Duration::from_millis(200));
    let (_admin, source, password) = team(root.path(), &server);
    let Ok(ProbeOutcome::Found(probed)) = probe_within(&source, PROBE_DEADLINE) else {
        panic!("the vault answers its probe");
    };
    // The vault goes away between the look and the request: the join applies what it saw,
    // and its request is not heard.
    server.stop();
    let mb = Paths::for_home(&root.path().join("mb"));
    let joined = join(
        &mb,
        &options(
            &source,
            JoinPassword::Given(Some(password.clone())),
            Some(&probed.envelope),
            Duration::ZERO,
        ),
    );
    assert!(joined.is_err(), "{joined:?}");
    let state = load_state(&mb).expect("state");
    let recipient = read_identity(&mb)
        .expect("readable")
        .expect("a key")
        .recipient()
        .to_bech32();
    let wanted = join::Key::derive("team", password.as_slice()).prove("team", &recipient, "mb");
    assert_eq!(state.proof.as_deref(), Some(wanted.as_str()));
    assert_eq!(state.requested, 0, "the next pass asks again");
    assert_eq!(state.membership, Stored::Pending);
}

#[test]
fn a_verdict_kept_before_the_subscribers_long_poll_is_read_before_it() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    // Only the server's stop ends the subscriber's long-poll within the hang detector.
    let server = serve_holding(&root.path().join("store"), Duration::from_secs(600));
    let (admin, source, _) = team(root.path(), &server);
    let mb = Paths::for_home(&root.path().join("mb"));
    let mistyped = SecretBytes::from("a-mistyped-join-password".as_bytes());
    let asked = join(
        &mb,
        &options(
            &source,
            JoinPassword::Given(Some(mistyped)),
            None,
            Duration::ZERO,
        ),
    );
    assert!(
        matches!(asked, Ok(JoinOutcome::Pending { .. })),
        "{asked:?}"
    );
    // The approver refuses it before mb's subscriber runs (one starting, or pausing between
    // long-polls): no long-poll is held for the verdict to end.
    approver_pass(&admin, &|_| {}, &mut Approver::default());
    assert_eq!(server.waiting(), 0);
    let stop = AtomicBool::new(false);
    let logged = std::sync::Mutex::new(Vec::<String>::new());
    let read_before_the_long_poll = std::thread::scope(|scope| {
        let subscriber = scope.spawn(|| {
            subscriber_loop_until(
                &mb,
                &|line| logged.lock().expect("log").push(line.to_owned()),
                &|| stop.load(Ordering::SeqCst),
                &Converging {
                    start: &|_| None,
                    routes: &crate::server::route_sources,
                },
            )
        });
        until("the subscriber's long-poll is held", || {
            server.waiting() == 1
        });
        let refused = password_refused(&mb);
        stop.store(true, Ordering::SeqCst);
        server.stop();
        subscriber
            .join()
            .expect("the subscriber ends")
            .expect("cleanly");
        refused
    });
    // CONTROL: a verdict read only once a long-poll ends is not read while this one is held.
    assert!(read_before_the_long_poll);
    let refusals = logged
        .lock()
        .expect("log")
        .iter()
        .filter(|line| line.contains("refused this Mac's join password"))
        .count();
    assert_eq!(refusals, 1, "said once");
}

#[test]
fn a_join_keeps_its_new_proof_before_its_request_is_answered() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let server = serve_holding(&root.path().join("store"), Duration::from_millis(200));
    let (_admin, source, password) = team(root.path(), &server);
    let mb = Paths::for_home(&root.path().join("mb"));
    let asked = join(
        &mb,
        &options(
            &source,
            JoinPassword::Given(Some(password)),
            None,
            Duration::ZERO,
        ),
    );
    assert!(
        matches!(asked, Ok(JoinOutcome::Pending { .. })),
        "{asked:?}"
    );
    server.stop();
    let first = load_state(&mb).expect("state").proof.expect("a proof");
    // The vault's address now takes a request and never answers it.
    let silent = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let mut config = config(&mb).expect("readable").expect("joined");
    config.source = Source::Url(format!(
        "http://{}/v1/team",
        silent.local_addr().expect("addr")
    ));
    save_config(&mb, &config).expect("saved");
    let recipient = read_identity(&mb)
        .expect("readable")
        .expect("a key")
        .recipient()
        .to_bech32();
    let retyped =
        join::Key::derive("team", b"the-password-typed-again").prove("team", &recipient, "mb");
    assert_ne!(retyped, first);
    let held = std::thread::scope(|scope| {
        let asking = scope.spawn(|| request_join(&mb, &config, Some(retyped.clone()), Asker::Join));
        let (stream, _) = silent.accept().expect("the request arrives");
        // While its POST is unanswered, an interrupt would leave this proof.
        let held = load_state(&mb).expect("state").proof;
        drop(stream);
        drop(silent);
        assert!(asking.join().expect("ends").is_err(), "not heard");
        held
    });
    // CONTROL: a proof saved only once the POST is answered is the earlier one here.
    assert_eq!(held.as_deref(), Some(retyped.as_str()));
    let state = load_state(&mb).expect("state");
    assert_eq!(state.proof.as_deref(), Some(retyped.as_str()));
    assert_eq!(state.requested, 0, "the next pass asks again");
}

/// A Mac that types the join password is let in by an admin Mac's approver within one request:
/// the approver long-polls the join queue, so a request ends its wait at once and nothing naps
/// between them. Negative control: an approver that polls (a server that names no queue
/// version) naps 2 s between passes, so the request waits out a nap.
#[test]
fn a_typed_join_password_is_let_in_with_no_nap_between() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let server = serve_holding(&root.path().join("store"), Duration::from_millis(200));
    let (admin, source, password) = team(root.path(), &server);
    let mb = Paths::for_home(&root.path().join("mb"));
    let stop = AtomicBool::new(false);
    let naps = std::sync::atomic::AtomicUsize::new(0);
    std::thread::scope(|scope| {
        scope.spawn(|| {
            approver_loop_napping(&admin, &|_| {}, &stop, &mut |pause| {
                naps.fetch_add(1, Ordering::SeqCst);
                std::thread::sleep(pause);
            });
        });
        until("the approver long-polls the queue", || {
            server.join_gets() >= 2
        });
        let joined = join(
            &mb,
            &options(
                &source,
                JoinPassword::Given(Some(password.clone())),
                None,
                HANG_DETECTOR,
            ),
        );
        stop.store(true, Ordering::SeqCst);
        assert!(
            matches!(joined, Ok(JoinOutcome::Joined { .. })),
            "{joined:?}"
        );
    });
    assert_eq!(naps.load(Ordering::SeqCst), 0, "the approver never napped");
    server.stop();
}

/// An idle admin Mac's approver costs one request per hold of the server's, each a long-poll
/// the server holds: 3600 / 25 = 144 an hour at the default hold, where polling every 2 s cost
/// 1800. Negative control: a pass that pauses (a polling approver) fails the zero pauses.
#[test]
fn an_idle_approver_asks_once_per_hold() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let server = serve_holding(&root.path().join("store"), Duration::from_millis(200));
    let (admin, _, _) = team(root.path(), &server);
    let mut approver = Approver::default();
    let pauses: Vec<Duration> = (0..5)
        .map(|_| approver_pass(&admin, &|_| {}, &mut approver))
        .collect();
    assert_eq!(pauses, [Duration::ZERO; 5]);
    assert_eq!(server.join_gets(), 5, "one GET of the queue per pass");
    eprintln!(
        "idle approver: {} GETs in 5 passes, each held {:?}: {} per idle hour at the default hold",
        server.join_gets(),
        Duration::from_millis(200),
        3600 / crate::server::DEFAULT_HOLD.as_secs()
    );
    server.stop();
}

/// An approver judges at each change to the join queue, which any host on the LAN can make by
/// asking to join, and stretches the join password once for all of them while it stays: three
/// passes judging three listings stretch it once. Negative control: a key stretched in each
/// judging pass is stretched three times.
#[test]
fn an_approver_stretches_the_join_password_once_while_it_stays() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let server = serve_holding(&root.path().join("store"), Duration::from_millis(200));
    let (admin, source, _) = team(root.path(), &server);
    let mut approver = Approver::default();
    let derived = || crate::join::DERIVED.with(std::cell::Cell::get);
    let before = derived();
    approver_pass(&admin, &|_| {}, &mut approver);
    for (n, host) in [(1, "mb"), (2, "mc")] {
        let mac = Paths::for_home(&root.path().join(format!("m{n}")));
        let mut asking = options(&source, JoinPassword::Given(None), None, Duration::ZERO);
        asking.host = Some(host.to_owned());
        let asked = join(&mac, &asking);
        assert!(
            matches!(asked, Ok(JoinOutcome::Pending { .. })),
            "{asked:?}"
        );
        approver_pass(&admin, &|_| {}, &mut approver);
        assert_eq!(
            approver.heard.lines.len(),
            n,
            "the pass judged the queue with this request in it"
        );
    }
    assert_eq!(
        derived() - before,
        1,
        "stretched once for three judged listings"
    );
    server.stop();
}

/// Sets its flag as it drops, a failed check's unwinding included: the subscriber loop that
/// flag stops ends, so the scope that runs it ends and the failure is reported.
struct StopsOnDrop<'a>(&'a AtomicBool);

impl Drop for StopsOnDrop<'_> {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// A member's subscriber starts the converge pass's runner once as it starts, none for healthy
/// rounds that changed nothing on the routes it started on, one for each version it applies,
/// and one once this Mac's routes moved; the loop's long-polls go on beside each start.
/// NEGATIVE CONTROL: a loop that starts no runner after an applied version (or none as it
/// starts) never reaches the count.
#[test]
fn the_subscriber_starts_the_converge_pass_as_it_starts_per_applied_version_and_on_moved_routes() {
    use std::net::{IpAddr, Ipv4Addr};
    use std::sync::atomic::AtomicUsize;
    const HOME: crate::converge::Routes =
        [Some(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 7))), None, None];
    const AWAY: crate::converge::Routes =
        [Some(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 9))), None, None];
    let root = aterm_tempfile::tempdir().expect("tempdir");
    // A long-poll held past HELD ends a round that changed nothing, and the next begins at once.
    let server = serve_holding(
        &root.path().join("store"),
        HELD + Duration::from_millis(100),
    );
    let (admin, source, _) = team(root.path(), &server);
    let mb = Paths::for_home(&root.path().join("mb"));
    let asked = join(
        &mb,
        &options(&source, JoinPassword::Given(None), None, Duration::ZERO),
    );
    assert!(
        matches!(asked, Ok(JoinOutcome::Pending { .. })),
        "{asked:?}"
    );
    let key = read_identity(&mb)
        .expect("readable")
        .expect("a key")
        .recipient()
        .to_bech32();
    approve_request(&admin, &key).expect("approves");
    assert!(matches!(sync(&mb, None), Ok(SyncOutcome::Applied { .. })));
    let (starts, reads) = (AtomicUsize::new(0), AtomicUsize::new(0));
    let (away, stop) = (AtomicBool::new(false), AtomicBool::new(false));
    let start = |_: &Path| {
        starts.fetch_add(1, Ordering::SeqCst);
        None
    };
    let routes = || {
        reads.fetch_add(1, Ordering::SeqCst);
        if away.load(Ordering::SeqCst) {
            AWAY
        } else {
            HOME
        }
    };
    let count = || starts.load(Ordering::SeqCst);
    // A healthy round reads the routes once, after its pass: `n` more reads are `n` rounds.
    let rounds = |n: usize| {
        let read = reads.load(Ordering::SeqCst);
        until("healthy rounds", || {
            reads.load(Ordering::SeqCst) >= read + n
        });
    };
    std::thread::scope(|scope| {
        let _stops = StopsOnDrop(&stop);
        let subscriber = scope.spawn(|| {
            subscriber_loop_until(
                &mb,
                &|_| {},
                &|| stop.load(Ordering::SeqCst),
                &Converging {
                    start: &start,
                    routes: &routes,
                },
            )
        });
        until("one start as it starts", || count() == 1);
        rounds(2);
        assert_eq!(count(), 1, "none for rounds that changed nothing");
        let opened = admin_open(&admin).expect("admin opens");
        let mut doc = opened.doc.clone();
        doc.set_entry(&Audience::Everyone, "token", b"inert-token")
            .expect("an entry");
        admin_publish(&admin, &opened, &doc).expect("published");
        until("one more for the applied version", || count() == 2);
        away.store(true, Ordering::SeqCst);
        until("one more once the routes moved", || count() == 3);
        rounds(2);
        assert_eq!(count(), 3, "not again on the routes it started on");
        stop.store(true, Ordering::SeqCst);
        subscriber
            .join()
            .expect("the subscriber ends")
            .expect("cleanly");
    });
    server.stop();
}

/// A runner that ended while the vault cannot be reached is reaped by the next round, though
/// every round fails: it is no zombie until a long-poll succeeds again.
/// NEGATIVE CONTROL: a loop that reaps its runner in healthy rounds alone leaves it a zombie.
#[cfg(unix)]
#[test]
fn a_runner_that_ended_while_the_vault_cannot_be_reached_is_reaped() {
    use aterm_uds::process::{ProcessPresence, process_presence};
    use std::os::unix::fs::PermissionsExt as _;
    use std::sync::atomic::AtomicI32;
    let root = aterm_tempfile::tempdir().expect("tempdir");
    // No vault is configured here: every round fails, and naps.
    let mb = Paths::for_home(&root.path().join("mb"));
    let sh = aterm_uds::deadline::stand_in(Path::new("/bin/sh"));
    let quick = root.path().join("quick");
    std::fs::write(&quick, format!("#!{}\nexit 0\n", sh.display())).expect("script");
    std::fs::set_permissions(&quick, std::fs::Permissions::from_mode(0o700)).expect("executable");
    let pid = AtomicI32::new(0);
    // The runner is handed to the loop once it exited, and before anything reaped it.
    let start = |_: &Path| {
        let child = RefCell::new(crate::converge::start(&quick, "subscriber").expect("a runner"));
        until("the runner exits", || {
            child.borrow_mut().exited().expect("its state")
        });
        let child = child.into_inner();
        pid.store(i32::try_from(child.id()).expect("a pid"), Ordering::SeqCst);
        Some(child)
    };
    let stop = AtomicBool::new(false);
    std::thread::scope(|scope| {
        let _stops = StopsOnDrop(&stop);
        let subscriber = scope.spawn(|| {
            subscriber_loop_until(
                &mb,
                &|_| {},
                &|| stop.load(Ordering::SeqCst),
                &Converging {
                    start: &start,
                    routes: &crate::server::route_sources,
                },
            )
        });
        until("the runner started", || pid.load(Ordering::SeqCst) != 0);
        let pid = pid.load(Ordering::SeqCst);
        until("the runner that ended is reaped", || {
            process_presence(pid) == ProcessPresence::Absent
        });
        stop.store(true, Ordering::SeqCst);
        subscriber
            .join()
            .expect("the subscriber ends")
            .expect("cleanly");
    });
}
