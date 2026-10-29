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

/// Let `key` in as `host` by its code as another admin Mac does: the version names it, and that
/// Mac records the request it let in on itself, not on this one.
fn let_in_by_another_admin(admin: &Paths, host: &str, key: &str) {
    let opened = admin_open(admin).expect("admin opens");
    let mut doc = opened.doc.clone();
    doc.add_machine(host, key).expect("added");
    admin_publish_approved(admin, &opened, &doc, &[key.to_owned()]).expect("approved by code");
    assert!(
        approved_recipients(admin).is_empty(),
        "CONTROL: this Mac recorded no approval"
    );
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
        until("the joiner's long-poll is held", || server.waiting() == 1);
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
        if matches!(mode, Mode::Open) {
            // An open vault admits nothing by itself: a member's Allow lets it in.
            super::handover::test_hand_allowed(&admin, &mb, "mb");
        }
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
            "{} {} {}{}{}",
            queued.recipient,
            queued.host,
            queued.code,
            queued
                .allowed
                .as_ref()
                .map(|allowed| format!(" allowed={allowed}"))
                .unwrap_or_default(),
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
    super::handover::test_hand_allowed(&admin, &mb, "mb");
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
        "a member's Allow let it in"
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
    super::handover::test_hand_allowed(&admin, &mb, "mb");
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

/// A server on loopback whose clock is `clock`.
fn serve_with_clock(store: &Path, clock: fn() -> u64) -> crate::server::Running {
    crate::server::start_with_clock(
        std::net::TcpListener::bind("127.0.0.1:0").expect("bind"),
        &crate::server::Options {
            listen: "127.0.0.1:0".parse().expect("addr"),
            store: store.to_path_buf(),
            advertise: None,
            pins: Vec::new(),
            hold: Duration::from_millis(200),
            keep_awake: false,
        },
        clock,
    )
    .expect("serving")
}

/// The keys of the Macs the approver last showed as asking.
fn shown_keys(admin: &Paths) -> Vec<String> {
    asking(admin).into_iter().map(|a| a.recipient).collect()
}

/// Publish `edit` of the vault as an admin Mac on an aterm before `[join]`'s notes does: the
/// text as edited, its notes as they were, and a `code-only` line for each key it lets go, as
/// builds since 2026-10-26 write.
fn publish_before_notes(paths: &Paths, edit: impl FnOnce(&mut Document)) {
    let admin = admin_open(paths).expect("admin opens");
    let mut doc = admin.doc.clone();
    edit(&mut doc);
    for key in let_go(&admin.published_doc, &doc) {
        doc.add_code_only(&key).expect("named code-only");
    }
    let identity = machine_identity(paths).expect("this Mac's key");
    let (bound, _) = publication_rights(&admin, &doc, &identity).expect("may publish");
    let admissions = admin.admissions.retained(&doc).expect("retained");
    let bytes = seal_with_admissions_and_parents(
        &admin.config.name,
        envelope::next_seq(admin.seq).expect("a next version"),
        &admin.seed,
        &admin.commons_seed,
        bound,
        &doc,
        &admin.handoff,
        &admin.opened_heads,
        &admissions,
    )
    .expect("sealed");
    transport::publish(&admin.config.source, &bytes).expect("published");
    sync(paths, None).expect("applied");
}

/// The team vault at `server`, where `mb` asked with no proof and the approver showed it as
/// asking: the vault's admin, its source, `mb`'s key and the approver.
fn asking_with_no_proof(
    root: &Path,
    server: &crate::server::Running,
) -> (Paths, String, String, Approver) {
    let (admin, source, _password) = team(root, server);
    let mb = Paths::for_home(&root.join("mb"));
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
    assert_eq!(
        shown_keys(&admin),
        std::slice::from_ref(&key),
        "CONTROL: no proof: asking"
    );
    (admin, source, key, approver)
}

/// [`asking_with_no_proof`], then a version of this aterm opens the vault, which holds `mb` on
/// a `code-only` line noted at its own version, and the approver still shows it: the vault's
/// admin, its source, `mb`'s key, the approver and the version it judged.
fn held_as_the_vault_opened(
    root: &Path,
    server: &crate::server::Running,
) -> (Paths, String, String, Approver, u64) {
    let (admin, source, key, mut approver) = asking_with_no_proof(root, server);
    let opened = admin_open(&admin).expect("admin opens");
    let mut doc = opened.doc.clone();
    doc.set_open(true).expect("open");
    admin_publish(&admin, &opened, &doc).expect("published");
    approver_pass(&admin, &|_| {}, &mut approver);
    assert_eq!(
        shown_keys(&admin),
        std::slice::from_ref(&key),
        "CONTROL: held: asking"
    );
    let judged = read_answered(&admin)
        .judged
        .expect("CONTROL: the record names the version judged");
    (admin, source, key, approver, judged)
}

/// The team vault at `server`, whose clock `ahead` moves on, where `mb` asked with no proof and
/// was shown, was let in by another admin Mac and let go, then asked again an hour on and was
/// shown at once: its key is on a `code-only` line the approver knew at its last judged pass,
/// and the request it asks with now is none the approver read while it was in. The vault's
/// admin, `mb`'s key, the line it asks with, and the approver.
fn shown_on_a_line_it_knew(
    root: &Path,
    server: &crate::server::Running,
    ahead: &std::sync::atomic::AtomicU64,
) -> (Paths, String, String, Approver) {
    let (admin, _source, key, mut approver) = asking_with_no_proof(root, server);
    let_in_by_another_admin(&admin, "mb", &key);
    approver_pass(&admin, &|_| {}, &mut approver);
    let opened = admin_open(&admin).expect("admin opens");
    admin_revoke(&admin, &opened, "mb", false).expect("revoked");
    approver_pass(&admin, &|_| {}, &mut approver);
    assert_eq!(asking(&admin), [], "CONTROL: the Mac let go asked nothing");

    let queued = requests_for(&config(&admin).expect("config").expect("one"), None)
        .expect("listed")
        .into_iter()
        .find(|request| request.recipient == key)
        .expect("CONTROL: the server still lists its request");
    let line = format!("{} {} {}", queued.recipient, queued.host, queued.code);
    ahead.store(
        crate::server::JOIN_REFRESH_EVERY_SECS + 1,
        std::sync::atomic::Ordering::SeqCst,
    );
    let url = format!("http://{}/v1/team", server.addr);
    transport::post_join(&url, &line).expect("asks again, an hour on");
    approver_pass(&admin, &|_| {}, &mut approver);
    assert_eq!(
        shown_keys(&admin),
        std::slice::from_ref(&key),
        "CONTROL: asked again, and shown at once"
    );
    (admin, key, line, approver)
}

/// A Mac another admin Mac lets in by its code and a version lets go, both while this approver
/// reads nothing, is no ghost, even for a key a `code-only` line named at its last pass: the
/// version that let it go named the key anew, and the request it asked with is taken for
/// answered. A Mac that asks again after it is shown.
#[test]
fn a_mac_another_admin_lets_in_by_code_and_lets_go_while_the_approver_is_away_is_no_ghost() {
    static AHEAD: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    fn clock() -> u64 {
        aterm_time::unix_secs().unwrap_or(0) + AHEAD.load(std::sync::atomic::Ordering::SeqCst)
    }
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let server = serve_with_clock(&root.path().join("store"), clock);
    let (admin, key, line, mut approver) = shown_on_a_line_it_knew(root.path(), &server, &AHEAD);

    // Away: another admin Mac lets it in by its code, and a version lets it go.
    let_in_by_another_admin(&admin, "mb", &key);
    let opened = admin_open(&admin).expect("admin opens");
    admin_revoke(&admin, &opened, "mb", false).expect("revoked again");

    let mut restarted = Approver::resumed(&admin);
    approver_pass(&admin, &|_| {}, &mut approver);
    assert_eq!(asking(&admin), [], "the running approver shows no ghost");
    approver_pass(&admin, &|_| {}, &mut restarted);
    assert_eq!(asking(&admin), [], "nor does one started again");

    // It asks again an hour after that, and is shown.
    AHEAD.store(
        2 * crate::server::JOIN_REFRESH_EVERY_SECS + 2,
        std::sync::atomic::Ordering::SeqCst,
    );
    let url = format!("http://{}/v1/team", server.addr);
    transport::post_join(&url, &line).expect("asks again, an hour on");
    approver_pass(&admin, &|_| {}, &mut approver);
    assert_eq!(
        shown_keys(&admin),
        std::slice::from_ref(&key),
        "asked again after it"
    );
    server.stop();
}

/// A Mac held as the vault opened, and shown, that another admin Mac lets in by its code and a
/// version lets go while this approver reads nothing, is no ghost: the version that let it go
/// named its key anew, so the request it was held on is taken for answered.
#[test]
fn a_mac_held_as_the_vault_opens_then_let_in_and_gone_by_another_admin_is_no_ghost() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let server = serve_holding(&root.path().join("store"), Duration::from_millis(200));
    let (admin, _source, key, mut approver, _judged) =
        held_as_the_vault_opened(root.path(), &server);

    // Away: another admin Mac lets it in by its code, and a version lets it go.
    let_in_by_another_admin(&admin, "mb", &key);
    let opened = admin_open(&admin).expect("admin opens");
    admin_revoke(&admin, &opened, "mb", false).expect("revoked");

    let mut restarted = Approver::resumed(&admin);
    approver_pass(&admin, &|_| {}, &mut approver);
    assert_eq!(asking(&admin), [], "the running approver shows no ghost");
    approver_pass(&admin, &|_| {}, &mut restarted);
    assert_eq!(asking(&admin), [], "nor does one started again");
    server.stop();
}

/// A Mac another admin Mac's approver lets in by the join password and a version lets go, while
/// this approver reads nothing, is neither shown nor refused, even for a key a `code-only` line
/// named at its last pass: the version that let it go named the key anew, so the request that
/// proved the password then is taken for answered, not checked against the password renewed
/// since, and no verdict asks its person for the password again.
#[test]
fn a_mac_another_approver_lets_in_by_the_password_and_lets_go_is_neither_shown_nor_refused() {
    static AHEAD: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    fn clock() -> u64 {
        aterm_time::unix_secs().unwrap_or(0) + AHEAD.load(std::sync::atomic::Ordering::SeqCst)
    }
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let server = serve_with_clock(&root.path().join("store"), clock);
    let (admin, source, password) = team(root.path(), &server);
    let url = format!("http://{}/v1/team", server.addr);
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
    let refused = || {
        transport::list_verdicts(&url)
            .expect("verdicts")
            .iter()
            .any(|(recipient, _)| *recipient == key)
    };
    let mut approver = Approver::resumed(&admin);
    approver_pass(&admin, &|_| {}, &mut approver);
    assert!(member(&admin), "CONTROL: the password let it in");
    let opened = admin_open(&admin).expect("admin opens");
    admin_revoke(&admin, &opened, "mb", false).expect("revoked");
    approver_pass(&admin, &|_| {}, &mut approver);
    assert_eq!(asking(&admin), [], "CONTROL: the Mac let go asked nothing");
    assert!(
        read_answered(&admin).seen.contains(&key),
        "CONTROL: the approver knows its key on a `code-only` line"
    );

    // It asks again with the password that version made, past the server's 10 s between a
    // key's changes.
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

    // Away: another admin Mac's approver, which keeps no record here, lets it in by that
    // password, and a version lets it go and renews the password.
    assert_eq!(
        auto_approve(&admin).expect("approved").approved,
        ["mb"],
        "CONTROL: let in by the password"
    );
    let opened = admin_open(&admin).expect("admin opens");
    admin_revoke(&admin, &opened, "mb", false).expect("revoked again");

    let mut restarted = Approver::resumed(&admin);
    approver_pass(&admin, &|_| {}, &mut approver);
    assert_eq!(asking(&admin), [], "the running approver shows no ghost");
    assert!(!refused(), "and refuses no password");
    approver_pass(&admin, &|_| {}, &mut restarted);
    assert_eq!(asking(&admin), [], "nor does one started again");
    assert!(!refused(), "nor refuses one");
    assert!(!member(&admin), "and neither lets it in");
    server.stop();
}

/// Admin Macs on an aterm before `[join]`'s notes that let a Mac in by its code and let it go
/// while this approver reads nothing leave the notes as they were and the run short of the
/// version this approver reads: no key it knew counts as named still, and the request the Mac
/// asked with is taken for answered.
#[test]
fn a_mac_admins_before_notes_let_in_and_let_go_is_no_ghost() {
    static AHEAD: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    fn clock() -> u64 {
        aterm_time::unix_secs().unwrap_or(0) + AHEAD.load(std::sync::atomic::Ordering::SeqCst)
    }
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let server = serve_with_clock(&root.path().join("store"), clock);
    let (admin, key, _line, mut approver) = shown_on_a_line_it_knew(root.path(), &server, &AHEAD);
    let named = admin_open(&admin)
        .expect("admin opens")
        .doc
        .code_only_named_in();
    assert!(
        named.contains_key(&key),
        "CONTROL: the version read notes its key"
    );

    // Away: admin Macs before the notes let it in by its code, and let it go.
    publish_before_notes(&admin, |doc| doc.add_machine("mb", &key).expect("added"));
    publish_before_notes(&admin, |doc| {
        doc.remove_machine("mb", false).expect("removed");
    });
    let after = admin_open(&admin).expect("admin opens");
    assert_eq!(
        after.doc.code_only_named_in(),
        named,
        "CONTROL: the notes stand as they were"
    );
    assert!(
        after
            .doc
            .code_only_noted()
            .is_some_and(|(_, through)| through < after.seq),
        "CONTROL: the run stops short of the version read"
    );

    let mut restarted = Approver::resumed(&admin);
    approver_pass(&admin, &|_| {}, &mut approver);
    assert_eq!(asking(&admin), [], "the running approver shows no ghost");
    approver_pass(&admin, &|_| {}, &mut restarted);
    assert_eq!(asking(&admin), [], "nor does one started again");
    server.stop();
}

/// A Mac held on a `code-only` line as the vault opened, and shown, stays shown to an approver
/// that read nothing while another admin Mac let another Mac in by its code and let it go and
/// published again: the notes say its key stayed named, while the key let go is named anew.
#[test]
fn a_mac_held_on_a_code_only_line_stays_shown_while_other_macs_come_and_go() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let server = serve_holding(&root.path().join("store"), Duration::from_millis(200));
    let (admin, source, key, mut approver, judged) = held_as_the_vault_opened(root.path(), &server);

    // Away: another Mac asks, another admin Mac lets it in by its code, a version lets it go,
    // and one more version.
    let mc = Paths::for_home(&root.path().join("mc"));
    let asked = join(
        &mc,
        &JoinOptions {
            host: Some("mc".to_string()),
            ..options(&source, JoinPassword::Given(None), None, Duration::ZERO)
        },
    );
    assert!(
        matches!(asked, Ok(JoinOutcome::Pending { .. })),
        "{asked:?}"
    );
    let other = read_identity(&mc)
        .expect("readable")
        .expect("a key")
        .recipient()
        .to_bech32();
    let_in_by_another_admin(&admin, "mc", &other);
    let opened = admin_open(&admin).expect("admin opens");
    admin_revoke(&admin, &opened, "mc", false).expect("revoked");
    let opened = admin_open(&admin).expect("admin opens");
    let mut doc = opened.doc.clone();
    doc.set_entry(&Audience::Everyone, "note", b"one more")
        .expect("set");
    admin_publish(&admin, &opened, &doc).expect("published");
    let after = admin_open(&admin).expect("admin opens");
    assert!(after.seq >= judged + 3, "CONTROL: {judged} {after:?}");
    assert_eq!(
        after.doc.code_only(),
        [key.clone(), other],
        "CONTROL: both are on `code-only` lines"
    );

    let mut restarted = Approver::resumed(&admin);
    approver_pass(&admin, &|_| {}, &mut approver);
    assert_eq!(
        shown_keys(&admin),
        std::slice::from_ref(&key),
        "the running approver still shows it, and only it"
    );
    approver_pass(&admin, &|_| {}, &mut restarted);
    assert_eq!(
        shown_keys(&admin),
        std::slice::from_ref(&key),
        "and so does one started again"
    );
    server.stop();
}

/// A Mac held as the vault opened, and shown, that another admin Mac lets in by its code, an
/// admin Mac on an aterm before `[join]`'s notes lets go, and a version of this aterm follows,
/// all while this approver reads nothing, is no ghost: the run begins three versions after the
/// one the approver read, so two versions it does not cover could have let the key in and let it
/// go, and the key, on its line again with no note, is named anew.
#[test]
fn a_mac_an_admin_before_notes_lets_go_between_versions_that_note_is_no_ghost() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let server = serve_holding(&root.path().join("store"), Duration::from_millis(200));
    let (admin, _source, key, mut approver, judged) =
        held_as_the_vault_opened(root.path(), &server);

    // Away: another admin Mac lets it in by its code, one on an aterm before the notes lets it
    // go, and one more version.
    let_in_by_another_admin(&admin, "mb", &key);
    publish_before_notes(&admin, |doc| {
        doc.remove_machine("mb", false).expect("removed");
    });
    let opened = admin_open(&admin).expect("admin opens");
    let mut doc = opened.doc.clone();
    doc.set_entry(&Audience::Everyone, "note", b"one more")
        .expect("set");
    admin_publish(&admin, &opened, &doc).expect("published");
    let after = admin_open(&admin).expect("admin opens");
    assert_eq!(after.seq, judged + 3, "CONTROL: three versions since");
    assert_eq!(
        after.doc.code_only(),
        std::slice::from_ref(&key),
        "CONTROL: on its `code-only` line again"
    );
    assert_eq!(
        after.doc.code_only_named_in(),
        BTreeMap::new(),
        "CONTROL: with no note"
    );
    assert_eq!(
        after.doc.code_only_noted(),
        Some((judged + 3, judged + 3)),
        "CONTROL: the run begins at the version read"
    );

    let mut restarted = Approver::resumed(&admin);
    approver_pass(&admin, &|_| {}, &mut approver);
    assert_eq!(asking(&admin), [], "the running approver shows no ghost");
    approver_pass(&admin, &|_| {}, &mut restarted);
    assert_eq!(asking(&admin), [], "nor does one started again");
    server.stop();
}

/// A Mac held on a `code-only` line that an admin Mac on an aterm before `[join]`'s notes wrote
/// as it opened the vault, and shown, stays shown to an approver started again at that version,
/// which no note covers, and to one that read nothing while versions of this aterm followed: each
/// carried the key on its line with no note, and a key with none was on its line since before
/// their run began.
#[test]
fn a_mac_held_on_a_line_an_aterm_before_notes_wrote_stays_shown_as_versions_note() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let server = serve_holding(&root.path().join("store"), Duration::from_millis(200));
    let (admin, _source, key, mut approver) = asking_with_no_proof(root.path(), &server);
    publish_before_notes(&admin, |doc| {
        doc.set_open(true).expect("open");
        doc.add_code_only(&key).expect("held");
    });
    approver_pass(&admin, &|_| {}, &mut approver);
    assert_eq!(
        shown_keys(&admin),
        std::slice::from_ref(&key),
        "CONTROL: held: asking"
    );
    let judged = read_answered(&admin)
        .judged
        .expect("CONTROL: the record names the version judged");
    assert_eq!(
        admin_open(&admin)
            .expect("admin opens")
            .doc
            .code_only_noted(),
        None,
        "CONTROL: the version read has no notes"
    );
    approver_pass(&admin, &|_| {}, &mut Approver::resumed(&admin));
    assert_eq!(
        shown_keys(&admin),
        std::slice::from_ref(&key),
        "one started again at that version still shows it"
    );

    // Away: this aterm publishes three versions.
    for name in ["one", "two", "three"] {
        let opened = admin_open(&admin).expect("admin opens");
        let mut doc = opened.doc.clone();
        doc.set_entry(&Audience::Everyone, name, b"v").expect("set");
        admin_publish(&admin, &opened, &doc).expect("published");
    }
    let after = admin_open(&admin).expect("admin opens");
    assert_eq!(after.seq, judged + 3, "CONTROL: three versions since");
    assert_eq!(
        after.doc.code_only(),
        std::slice::from_ref(&key),
        "CONTROL: on its `code-only` line"
    );
    assert_eq!(
        after.doc.code_only_named_in(),
        BTreeMap::new(),
        "CONTROL: with no note"
    );
    assert_eq!(
        after.doc.code_only_noted(),
        Some((judged + 1, judged + 3)),
        "CONTROL: the run begins just after the version judged"
    );

    let mut restarted = Approver::resumed(&admin);
    approver_pass(&admin, &|_| {}, &mut approver);
    assert_eq!(
        shown_keys(&admin),
        std::slice::from_ref(&key),
        "the running approver still shows it"
    );
    approver_pass(&admin, &|_| {}, &mut restarted);
    assert_eq!(
        shown_keys(&admin),
        std::slice::from_ref(&key),
        "and so does one started again"
    );
    server.stop();
}

/// A record whose last judged pass read a version above the one read now (a vault whose version
/// went back) vouches for no key: each it knew on a `code-only` line is named anew
/// ([`stayed_named`]). Negative control: without the check of the version judged against the one
/// read, a key it knew stays named.
#[test]
fn a_record_judged_above_the_version_read_keeps_no_key_named() {
    let key = Identity::generate().expect("a key").recipient().to_bech32();
    let doc = Document::parse(&format!("[machines]\nma = {key}\n[admins]\nma\n")).expect("vault");
    let known = std::collections::BTreeSet::from([key]);
    assert_eq!(
        stayed_named(&doc, 7, Some(7), &known),
        known,
        "CONTROL: at the version judged, a key stays named"
    );
    for judged in [8, 12] {
        assert_eq!(
            stayed_named(&doc, 7, Some(judged), &known),
            std::collections::BTreeSet::new(),
            "judged {judged}, read 7"
        );
    }
}

/// Each version notes in `[join]` comments the version that named each key on a `code-only` line
/// anew: a version that lets a key go notes it at its own version, one that keeps a key keeps
/// its note and carries the run on, an approval by code drops the note with the line, and a
/// person's edit that deletes the notes gets them back. A vault with no such line gets none.
/// After a version an aterm before the notes published, the run begins again. A key let in by
/// hand with its line left is named anew once it is let go.
#[test]
fn a_version_notes_each_key_it_names_anew_and_carries_the_rest() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let server = serve_holding(&root.path().join("store"), Duration::from_millis(200));
    let (admin, _source, _password) = team(root.path(), &server);
    let text = |doc: &Document| -> String {
        String::from_utf8(doc.render().as_slice().to_vec()).expect("utf-8")
    };
    let notes = |admin: &Paths| {
        let opened = admin_open(admin).expect("admin opens");
        (
            opened.seq,
            opened.doc.code_only_named_in(),
            opened.doc.code_only_noted(),
        )
    };
    let entry = |admin: &Paths, name: &str| {
        let opened = admin_open(admin).expect("admin opens");
        let mut doc = opened.doc.clone();
        doc.set_entry(&Audience::Everyone, name, b"v").expect("set");
        admin_publish(admin, &opened, &doc).expect("published");
        doc
    };

    // A vault with no `code-only` line is published as it was edited.
    let doc = entry(&admin, "one");
    assert_eq!(
        text(&admin_open(&admin).expect("admin opens").doc),
        text(&doc),
        "no notes"
    );

    let (mb, mc) = (
        Identity::generate().expect("a key").recipient().to_bech32(),
        Identity::generate().expect("a key").recipient().to_bech32(),
    );
    let opened = admin_open(&admin).expect("admin opens");
    let mut doc = opened.doc.clone();
    doc.add_machine("mb", &mb).expect("added");
    doc.add_machine("mc", &mc).expect("added");
    admin_publish_approved(&admin, &opened, &doc, &[mb.clone(), mc.clone()]).expect("let in");

    // A version that lets a key go notes it at its own version.
    let opened = admin_open(&admin).expect("admin opens");
    admin_revoke(&admin, &opened, "mb", false).expect("revoked");
    let (first, named, run) = notes(&admin);
    assert_eq!(named, BTreeMap::from([(mb.clone(), first)]));
    assert_eq!(run, Some((first, first)));

    // One that keeps it keeps its note and carries the run on.
    entry(&admin, "two");
    let (seq, named, run) = notes(&admin);
    assert_eq!(named, BTreeMap::from([(mb.clone(), first)]));
    assert_eq!(run, Some((first, seq)));

    // Another that lets a key go notes it at its own.
    let opened = admin_open(&admin).expect("admin opens");
    admin_revoke(&admin, &opened, "mc", false).expect("revoked");
    let (second, named, run) = notes(&admin);
    assert_eq!(
        named,
        BTreeMap::from([(mb.clone(), first), (mc.clone(), second)])
    );
    assert_eq!(run, Some((first, second)));

    // An approval by code drops the note with the line.
    let opened = admin_open(&admin).expect("admin opens");
    let mut doc = opened.doc.clone();
    doc.add_machine("mb", &mb).expect("added");
    admin_publish_approved(&admin, &opened, &doc, std::slice::from_ref(&mb)).expect("approved");
    let (seq, named, run) = notes(&admin);
    assert_eq!(named, BTreeMap::from([(mc.clone(), second)]));
    assert_eq!(run, Some((first, seq)));
    let published = text(&admin_open(&admin).expect("admin opens").doc);
    assert!(
        !published.contains(&format!("# code-only {mb}")),
        "{published}"
    );

    // A person's edit that deletes the notes gets them back.
    let opened = admin_open(&admin).expect("admin opens");
    let edited: String = text(&opened.doc)
        .lines()
        .filter(|line| !line.starts_with("# code-only"))
        .map(|line| format!("{line}\n"))
        .collect();
    let doc = Document::parse(&edited).expect("parses");
    assert!(!doc.code_only_notes(), "CONTROL: the edit deleted them");
    admin_publish(&admin, &opened, &doc).expect("published");
    let (seq, named, run) = notes(&admin);
    assert_eq!(named, BTreeMap::from([(mc.clone(), second)]));
    assert_eq!(run, Some((first, seq)));

    // A version an aterm before the notes published leaves them as they were, and the next
    // begins the run again.
    publish_before_notes(&admin, |doc| {
        doc.set_entry(&Audience::Everyone, "three", b"v")
            .expect("set");
    });
    let (before, named, run) = notes(&admin);
    assert_eq!(named, BTreeMap::from([(mc.clone(), second)]));
    assert_eq!(run, Some((first, before - 1)));
    entry(&admin, "four");
    let (seq, named, run) = notes(&admin);
    assert_eq!(named, BTreeMap::from([(mc.clone(), second)]));
    assert_eq!(run, Some((seq, seq)));

    // A key a person writes into `[machines]` by hand, its line left, keeps its note while it
    // stays, and is named anew by the version that lets it go.
    let opened = admin_open(&admin).expect("admin opens");
    let edited = text(&opened.doc).replacen("[machines]\n", &format!("[machines]\nmc = {mc}\n"), 1);
    let doc = Document::parse(&edited).expect("parses");
    admin_publish(&admin, &opened, &doc).expect("published");
    let (_, named, _) = notes(&admin);
    assert_eq!(named, BTreeMap::from([(mc.clone(), second)]));
    let opened = admin_open(&admin).expect("admin opens");
    admin_revoke(&admin, &opened, "mc", false).expect("revoked");
    let (seq, named, _) = notes(&admin);
    assert_eq!(named, BTreeMap::from([(mc, seq)]));
    server.stop();
}

/// Exact proof metadata distinguishes same-second requests; missing or damaged
/// metadata preserves the suppression older records could express.
#[test]
fn answered_proof_metadata_distinguishes_requests_and_preserves_legacy_suppression() {
    let held = Answered {
        recipient: format!("age1{}", "a".repeat(58)),
        host: "mb".into(),
        at: 1_000_000,
        proof: Some("a".repeat(64)),
    };
    let mut changed = held.clone();
    changed.proof = Some("b".repeat(64));
    assert!(!held.matches(&changed));
    let encoded = answered_text(&std::collections::BTreeSet::from([held.clone()]));
    assert_eq!(
        answered_lines(&encoded),
        std::collections::BTreeSet::from([held.clone()])
    );

    // An old build ignores the comments and still knows the same answered
    // request. Missing new metadata stays conservative for a new build too.
    let old_lines = encoded
        .lines()
        .filter(|line| !line.starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n");
    let legacy = answered_lines(&old_lines).into_iter().next().unwrap();
    assert!(legacy.proof.is_none());
    assert!(legacy.matches(&held) && legacy.matches(&changed));
    for extra in [
        format!("{ANSWERED_PROOF}0\tnot-a-digest\n"),
        format!("{ANSWERED_PROOF}0\t{}\n", changed.proof.as_ref().unwrap()),
    ] {
        let ambiguous = answered_lines(&format!("{encoded}{extra}"));
        assert_eq!(
            ambiguous,
            std::collections::BTreeSet::from([legacy.clone()])
        );
    }
}

#[test]
fn answered_proof_indices_do_not_rebind_after_request_lines_change() {
    let answered: std::collections::BTreeSet<_> = ['a', 'b']
        .into_iter()
        .map(|letter| Answered {
            recipient: format!("age1{}", letter.to_string().repeat(58)),
            host: "mb".into(),
            at: 1_000_000,
            proof: Some(letter.to_string().repeat(64)),
        })
        .collect();
    let encoded = answered_text(&answered);
    let mut requests: Vec<_> = encoded
        .lines()
        .filter(|line| !line.starts_with('#'))
        .collect();
    requests.reverse();
    let changed = requests
        .into_iter()
        .chain(encoded.lines().filter(|line| line.starts_with('#')))
        .map(|line| format!("{line}\n"))
        .collect::<String>();
    let retained = answered_lines(&changed);
    for original in &answered {
        assert!(
            retained.iter().any(|held| held.matches(original)),
            "changed metadata forgot a standing answered request: {retained:?}"
        );
    }
}

/// The approver record keeps the requests it answered in `answered`, which a build before its
/// queue file reads under 64 KiB, and the queue, the version its last judged pass read and its
/// `code-only` keys in that file, however many: a large vault's record still reads whole there,
/// and a build before the version's line reads the queue and the keys alone. A queue file
/// without that line, and a record of 2026-10-26, with the queue and key lines at the head of
/// `answered`, still read, with no version; the next write moves them.
#[test]
fn an_approver_record_keeps_its_requests_and_judged_version_where_a_build_before_reads_them() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let admin = Paths::for_home(&root.path().join("ma"));
    let seen: std::collections::BTreeSet<String> =
        (0..4000).map(|n| format!("age1{n:058}")).collect();
    let answered: std::collections::BTreeSet<Answered> = (0..ANSWERED_MOST as u64)
        .map(|n| Answered {
            recipient: format!("age1{n:058}"),
            host: format!("mac-{n}"),
            at: 1_790_000_000 + n,
            proof: Some(format!("{n:064x}")),
        })
        .collect();
    write_answered(
        &admin,
        &answered,
        Some("team http://x/v1/team"),
        Some(68),
        &seen,
    )
    .expect("written");
    assert!(
        fsx::read_private(&admin.answered_file(), 64 << 10)
            .expect("readable")
            .is_some(),
        "a build before reads its requests"
    );
    let record = read_answered(&admin);
    assert_eq!(record.answered, answered);
    assert_eq!(record.begun.as_deref(), Some("team http://x/v1/team"));
    assert_eq!(record.judged, Some(68));
    assert_eq!(record.seen, seen);
    let queue = std::fs::read_to_string(admin.answered_queue_file()).expect("queue");
    assert_eq!(
        queue
            .lines()
            .filter(|line| line.starts_with(ANSWERED_BEGUN) || line.starts_with(ANSWERED_SEEN))
            .count(),
        1 + seen.len(),
        "a build before the version's line reads by these heads, and skips it"
    );

    // The queue file a build before the version's line wrote.
    let before: String = queue
        .lines()
        .filter(|line| !line.starts_with(ANSWERED_JUDGED))
        .map(|line| format!("{line}\n"))
        .collect();
    fsx::write_atomic(&admin.answered_queue_file(), before.as_bytes(), 0o600).expect("written");
    let record = read_answered(&admin);
    assert_eq!(record.begun.as_deref(), Some("team http://x/v1/team"));
    assert_eq!(record.judged, None);
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
    assert_eq!(record.judged, None);
    assert_eq!(record.seen, std::collections::BTreeSet::from([key.clone()]));
    let requests: Vec<&str> = record.answered.iter().map(|a| a.host.as_str()).collect();
    assert_eq!(requests, ["mb"]);
    write_answered(
        &admin,
        &record.answered,
        record.begun.as_deref(),
        record.judged,
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
        // Different proofs deliberately arrive within the same server second.
        1_000_000 + AHEAD.load(std::sync::atomic::Ordering::SeqCst)
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
                &deaf(),
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

/// The bell of a subscriber loop that hears nothing: nothing rings it.
static NOBODY: crate::wake::Bell = crate::wake::Bell::new();

/// What a subscriber loop hears in a test that hears nothing of the network.
fn deaf() -> Hearing<'static> {
    Hearing {
        found: &found_none,
        bell: &NOBODY,
        routes: false,
        ask: &ask_nobody,
    }
}

/// A subscriber's ask of ALab's members that reaches none.
fn ask_nobody(_: &Paths) -> crate::Result<usize> {
    Ok(0)
}

/// `mb`, waiting to be let into the vault `ma` makes at `server` (no join password, so only
/// an approval lets it in), and the vault read as ALab's from this thread on: its signer the
/// key this build names.
fn waiting_for_alab(
    root: &Path,
    server: &crate::server::Running,
) -> (Paths, Paths, crate::envelope::Retired) {
    let admin = Paths::for_home(&root.join("ma"));
    let url = format!("http://{}/v1/team", server.addr);
    let created = create(&admin, &url, None, Some("ma")).expect("create");
    let mb = Paths::for_home(&root.join("mb"));
    let asked = join(
        &mb,
        &options(
            &format!("{url}#{}", created.signer),
            JoinPassword::Given(None),
            None,
            Duration::ZERO,
        ),
    );
    assert!(
        matches!(asked, Ok(JoinOutcome::Pending { .. })),
        "{asked:?}"
    );
    let alab = crate::envelope::Retired {
        keys: Vec::new(),
        current: config(&mb).expect("config").expect("a vault").signer,
    };
    crate::envelope::TEST_RETIRED.with(|cell| *cell.borrow_mut() = Some(alab.clone()));
    (admin, mb, alab)
}

/// A Mac waiting to be let into the ALab vault, as System State's join leaves it (that join
/// posts only to the vault's queue, which no member's desk reads), asks ALab's members on the
/// LAN to show its request from its subscriber, and says how many heard it.
/// NEGATIVE CONTROL: a subscriber loop that never calls `ask_members_if_due` (main before this
/// fix) asks no member, and the hang detector fails the test.
#[test]
fn a_mac_waiting_for_alab_asks_its_members_from_its_subscriber() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    // Only the server's stop ends the subscriber's long-poll within the hang detector.
    let server = serve_holding(&root.path().join("store"), Duration::from_secs(600));
    let (_admin, mb, alab) = waiting_for_alab(root.path(), &server);
    let asks = std::sync::atomic::AtomicUsize::new(0);
    let ask = |_: &Paths| {
        asks.fetch_add(1, Ordering::SeqCst);
        Ok(2)
    };
    let stop = AtomicBool::new(false);
    let logged = std::sync::Mutex::new(Vec::<String>::new());
    std::thread::scope(|scope| {
        let _stops = StopsOnDrop(&stop);
        let subscriber = scope.spawn(|| {
            crate::envelope::TEST_RETIRED.with(|cell| *cell.borrow_mut() = Some(alab.clone()));
            subscriber_loop_until(
                &mb,
                &|line| logged.lock().expect("log").push(line.to_owned()),
                &|| stop.load(Ordering::SeqCst),
                &Converging {
                    start: &|_| None,
                    routes: &crate::server::route_sources,
                },
                &Hearing {
                    found: &found_none,
                    bell: &NOBODY,
                    routes: false,
                    ask: &ask,
                },
            )
        });
        until("the waiting Mac asks ALab's members", || {
            asks.load(Ordering::SeqCst) > 0
        });
        stop.store(true, Ordering::SeqCst);
        server.stop();
        subscriber
            .join()
            .expect("the subscriber ends")
            .expect("cleanly");
    });
    assert!(
        logged
            .lock()
            .expect("log")
            .iter()
            .any(|line| line.contains("2 show its request; press Allow on any one")),
        "{:?}",
        logged.lock().expect("log")
    );
}

/// When a waiting Mac's subscriber asks ALab's members: once each minute while it waits for the
/// ALab vault at a URL, saying a changed answer once; never for another vault, never on a Mac
/// that is in, and never once a member's `Allowed` is held (the run let it in, and the vault
/// records it).
/// NEGATIVE CONTROLS: an `ask_members_if_due` without its `holds_allowed` check asks again
/// after a run let this Mac in; one without its ALab check asks ALab's members for a team vault.
#[test]
fn a_waiting_mac_asks_alabs_members_once_a_minute_until_a_member_allows_it() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let server = serve_holding(&root.path().join("store"), Duration::from_millis(200));
    let (admin, mb, alab) = waiting_for_alab(root.path(), &server);
    let asks = std::sync::atomic::AtomicUsize::new(0);
    let ask = |_: &Paths| Ok(asks.fetch_add(1, Ordering::SeqCst));
    let said = std::sync::Mutex::new(Vec::<String>::new());
    let log = |line: &str| said.lock().expect("log").push(line.to_owned());
    let count = || asks.load(Ordering::SeqCst);
    let a_minute_ago = || {
        Instant::now()
            .checked_sub(ASK_MEMBERS_EVERY + Duration::from_secs(1))
            .expect("an instant a minute ago")
    };
    let mut asked = MembersAsked::default();
    crate::envelope::TEST_RETIRED.with(|cell| *cell.borrow_mut() = None);
    ask_members_if_due(&mb, &mut asked, &ask, &log);
    assert_eq!(
        count(),
        0,
        "another vault's waiting Mac asks no ALab member"
    );
    crate::envelope::TEST_RETIRED.with(|cell| *cell.borrow_mut() = Some(alab));
    ask_members_if_due(&mb, &mut asked, &ask, &log);
    assert_eq!(count(), 1, "it asks as it starts waiting");
    ask_members_if_due(&mb, &mut asked, &ask, &log);
    assert_eq!(count(), 1, "and not again within the minute");
    asked.at = Some(a_minute_ago());
    ask_members_if_due(&mb, &mut asked, &ask, &log);
    asked.at = Some(a_minute_ago());
    ask_members_if_due(&mb, &mut asked, &ask, &log);
    assert_eq!(count(), 3, "it asks again each minute");
    assert_eq!(
        said.lock().expect("log").len(),
        3,
        "none, then 1, then 2 heard it: each said once {:?}",
        said.lock().expect("log")
    );
    let mut theirs = MembersAsked::default();
    ask_members_if_due(&admin, &mut theirs, &ask, &log);
    assert_eq!(count(), 3, "a Mac that is in asks nobody");
    super::handover::test_hand_allowed(&admin, &mb, "mb");
    asked.at = Some(a_minute_ago());
    ask_members_if_due(&mb, &mut asked, &ask, &log);
    assert_eq!(count(), 3, "a Mac a member allowed asks nobody");
    crate::envelope::TEST_RETIRED.with(|cell| *cell.borrow_mut() = None);
    server.stop();
}

/// `mb`, let into `admin`'s vault at `source` and holding its version.
fn member_in(root: &Path, admin: &Paths, source: &str) -> Paths {
    let mb = Paths::for_home(&root.join("mb"));
    let asked = join(
        &mb,
        &options(source, JoinPassword::Given(None), None, Duration::ZERO),
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
    approve_request(admin, &key).expect("approves");
    assert!(matches!(sync(&mb, None), Ok(SyncOutcome::Applied { .. })));
    mb
}

/// A `dns-sd -B _alab-vault._tcp local.` row adding `name` on interface 4.
fn browse_add(name: &str) -> String {
    format!("8:30:01.200  Add        3   4 local.               _alab-vault._tcp.    {name}")
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
                &deaf(),
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

/// A subscriber whose vault cannot be reached naps between tries, and a vault server its
/// standing browse hears ends the nap at once: its log says the browse woke it. The browse is a
/// watch fed one `dns-sd -B` row, ringing the bell the loop naps on.
/// NEGATIVE CONTROL: a nap that waits on no bell runs to its timer and the next, and the hang
/// detector fires.
#[test]
fn a_failing_subscriber_ends_its_nap_on_a_browse_add() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let server = serve_holding(&root.path().join("store"), Duration::from_millis(200));
    let (admin, source, _) = team(root.path(), &server);
    let mb = member_in(root.path(), &admin, &source);
    server.stop();
    let bell = std::sync::Arc::new(crate::wake::Bell::default());
    let watch = crate::discover::Watch::idle(&bell, &[]);
    let logged = std::sync::Mutex::new(Vec::<String>::new());
    let said = |what: &str| {
        logged
            .lock()
            .expect("log")
            .iter()
            .any(|line| line.contains(what))
    };
    let stop = AtomicBool::new(false);
    std::thread::scope(|scope| {
        let _stops = StopsOnDrop(&stop);
        let subscriber = scope.spawn(|| {
            subscriber_loop_until(
                &mb,
                &|line| logged.lock().expect("log").push(line.to_owned()),
                &|| stop.load(Ordering::SeqCst),
                &Converging {
                    start: &|_| None,
                    routes: &crate::server::route_sources,
                },
                &Hearing {
                    found: &found_none,
                    bell: &bell,
                    routes: false,
                    ask: &ask_nobody,
                },
            )
        });
        until("a pass fails, and its nap begins", || {
            said("retrying with backoff")
        });
        watch.feed(&browse_add("m3"));
        until("the nap ends on the browse's Add", || {
            said("asking again now: woken by a vault server the browse heard")
        });
        stop.store(true, Ordering::SeqCst);
        subscriber
            .join()
            .expect("the subscriber ends")
            .expect("cleanly");
    });
}

/// The first healthy pass after failed ones starts the converge pass's runner, though it applies
/// nothing and the routes stayed: the vault came back, or this Mac came back to it.
/// NEGATIVE CONTROL: drop the streak rule (`Due::after_pass` forgetting the failed passes) and
/// no runner starts after the one at start: the hang detector fires.
#[test]
fn a_pass_healthy_after_errors_starts_a_runner() {
    use std::net::{IpAddr, Ipv4Addr};
    use std::sync::atomic::AtomicUsize;
    const HOME: crate::converge::Routes =
        [Some(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 7))), None, None];
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let server = serve_holding(&root.path().join("store"), Duration::from_millis(200));
    let (admin, source, _) = team(root.path(), &server);
    let mb = member_in(root.path(), &admin, &source);
    // The vault's address answers nothing until it is put back.
    let reached = config(&mb).expect("readable").expect("joined");
    let gone = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let mut unreached = reached.clone();
    unreached.source = Source::Url(format!(
        "http://{}/v1/team",
        gone.local_addr().expect("addr")
    ));
    drop(gone);
    save_config(&mb, &unreached).expect("saved");
    let starts = AtomicUsize::new(0);
    let start = |_: &Path| {
        starts.fetch_add(1, Ordering::SeqCst);
        None
    };
    let logged = std::sync::Mutex::new(Vec::<String>::new());
    let stop = AtomicBool::new(false);
    std::thread::scope(|scope| {
        let _stops = StopsOnDrop(&stop);
        let subscriber = scope.spawn(|| {
            subscriber_loop_until(
                &mb,
                &|line| logged.lock().expect("log").push(line.to_owned()),
                &|| stop.load(Ordering::SeqCst),
                &Converging {
                    start: &start,
                    routes: &|| HOME,
                },
                &deaf(),
            )
        });
        until("one start as it starts", || {
            starts.load(Ordering::SeqCst) == 1
        });
        until("a pass fails", || {
            logged
                .lock()
                .expect("log")
                .iter()
                .any(|line| line.contains("retrying with backoff"))
        });
        save_config(&mb, &reached).expect("put back");
        until("one more for the healthy pass after errors", || {
            starts.load(Ordering::SeqCst) == 2
        });
        stop.store(true, Ordering::SeqCst);
        subscriber
            .join()
            .expect("the subscriber ends")
            .expect("cleanly");
    });
    server.stop();
}

/// The vault's server goes, and comes back at a new address holding a newer version: the
/// subscriber's standing browse hears it, its nap ends, and its next pass applies that version
/// from there, its source left as it was. The browse is a watch fed the Add as the loop logs the
/// failed pass, before its nap begins, so no timer can end that nap first.
/// NEGATIVE CONTROL: a pass that reads its source only (no browsed server) never applies it,
/// and a nap that waits on no bell never says the browse woke it: the hang detector fires.
#[test]
fn a_server_that_vanishes_and_returns_at_a_new_address_is_applied() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let gone = serve_holding(&root.path().join("gone"), Duration::from_millis(200));
    let (admin, source, _) = team(root.path(), &gone);
    let mb = member_in(root.path(), &admin, &source);
    let before = config(&mb).expect("readable").expect("joined");
    // While mb's subscriber is not running, a newer version, and the server goes.
    let opened = admin_open(&admin).expect("admin opens");
    let mut doc = opened.doc.clone();
    doc.set_entry(&Audience::Everyone, "token", b"inert-token")
        .expect("an entry");
    let newer = admin_publish(&admin, &opened, &doc).expect("published").seq;
    gone.stop();
    // It comes back at a new address, holding what it held.
    let back = serve_holding(&root.path().join("back"), Duration::from_millis(200));
    let back_url = format!("http://{}/v1/team", back.addr);
    let held = std::fs::read(admin.cache_file()).expect("the admin's copy");
    transport::publish(&Source::Url(back_url.clone()), &held).expect("seeded");
    let bell = std::sync::Arc::new(crate::wake::Bell::default());
    let watch = crate::discover::Watch::idle(&bell, &[]);
    let found = |_: &Config| {
        if watch.heard().is_empty() {
            Vec::new()
        } else {
            vec![crate::discover::DiscoveredVault {
                url: back_url.clone(),
                txt: None,
            }]
        }
    };
    let logged = std::sync::Mutex::new(Vec::<String>::new());
    let log = |line: &str| {
        if line.contains("retrying with backoff") {
            watch.feed(&browse_add("ma"));
        }
        logged.lock().expect("log").push(line.to_owned());
    };
    let stop = AtomicBool::new(false);
    std::thread::scope(|scope| {
        let _stops = StopsOnDrop(&stop);
        let subscriber = scope.spawn(|| {
            subscriber_loop_until(
                &mb,
                &log,
                &|| stop.load(Ordering::SeqCst),
                &Converging {
                    start: &|_| None,
                    routes: &crate::server::route_sources,
                },
                &Hearing {
                    found: &found,
                    bell: &bell,
                    routes: false,
                    ask: &ask_nobody,
                },
            )
        });
        until("the newer version is applied", || {
            load_state(&mb).is_ok_and(|state| state.seq == newer)
        });
        stop.store(true, Ordering::SeqCst);
        subscriber
            .join()
            .expect("the subscriber ends")
            .expect("cleanly");
    });
    let logged = logged.into_inner().expect("log");
    assert!(
        logged
            .iter()
            .any(|line| line == "asking again now: woken by a vault server the browse heard"),
        "{logged:?}"
    );
    assert_eq!(
        std::fs::read(mb.home.join(".secrets/token")).expect("materialized"),
        b"inert-token"
    );
    assert_eq!(
        config(&mb).expect("readable").expect("joined"),
        before,
        "the source stays the first choice"
    );
    back.stop();
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
                &deaf(),
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

/// A Mac at `home` that asks `source` to join as `host` with no password, carrying the Allow
/// `member` handed it, and its key.
fn ask_as(root: &Path, home: &str, host: &str, source: &str, member: &Paths) -> (Paths, String) {
    let paths = Paths::for_home(&root.join(home));
    super::handover::test_hand_allowed(member, &paths, host);
    let asked = join(
        &paths,
        &JoinOptions {
            host: Some(host.to_string()),
            ..options(source, JoinPassword::Given(None), None, Duration::ZERO)
        },
    );
    assert!(
        matches!(asked, Ok(JoinOutcome::Pending { .. })),
        "{asked:?}"
    );
    let key = read_identity(&paths)
        .expect("readable")
        .expect("a key")
        .recipient()
        .to_bech32();
    (paths, key)
}

/// A new Mac a member Allowed, asking under the name of a Mac the vault let go, is let in within
/// the pass under that name: macOS gives every new Mac a default name
/// (`andrews` for "Andrew's MacBook Pro", `machine` where none reads), so a new owner Mac or a
/// guest after a revoke asks under the same name as the Mac let go, and the approver cannot tell
/// the two apart. The key let go stays out (its `code-only` line); a name a member holds still
/// gets the next free one.
/// NEGATIVE CONTROL: an approver that holds a new key asking under a let-go name for its code
/// (the let-go record tried and dropped on 2026-10-27) leaves the new Mac waiting.
#[test]
fn a_new_mac_allowed_under_the_name_of_a_mac_let_go_is_let_in() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let server = serve_holding(&root.path().join("store"), Duration::from_millis(200));
    let admin = Paths::for_home(&root.path().join("ma"));
    let url = format!("http://{}/v1/team", server.addr);
    let created = create(&admin, &url, None, Some("ma")).expect("create");
    let source = format!("{url}#{}", created.signer);
    let (_old, first) = ask_as(root.path(), "old", "andrews", &source, &admin);
    let mut approver = Approver::default();
    approver_pass(&admin, &|_| {}, &mut approver);
    approver_pass(&admin, &|_| {}, &mut approver);
    let opened = admin_open(&admin).expect("admin opens");
    assert!(
        opened
            .doc
            .machines()
            .iter()
            .any(|m| m.name == "andrews" && m.recipient == first),
        "CONTROL: the first Mac is in"
    );
    admin_revoke(&admin, &opened, "andrews", false).expect("revoked");
    let (_new, second) = ask_as(root.path(), "new", "andrews", &source, &admin);
    approver_pass(&admin, &|_| {}, &mut approver);
    let opened = admin_open(&admin).expect("admin opens");
    let named = |key: &str| {
        opened
            .doc
            .machines()
            .into_iter()
            .find(|m| m.recipient == key)
            .map(|m| m.name)
    };
    assert_eq!(
        named(&second).as_deref(),
        Some("andrews"),
        "the new Mac is let in under its name: {:?}",
        opened.doc.machines()
    );
    assert_eq!(named(&first), None, "the key let go stays out");
    assert!(opened.doc.code_only().contains(&first), "held for its code");
    let (_third, third) = ask_as(root.path(), "third", "andrews", &source, &admin);
    approver_pass(&admin, &|_| {}, &mut approver);
    let opened = admin_open(&admin).expect("admin opens");
    assert!(
        opened
            .doc
            .machines()
            .iter()
            .any(|m| m.name == "andrews-2" && m.recipient == third),
        "a name a member holds gets the next free one: {:?}",
        opened.doc.machines()
    );
    server.stop();
}

/// The window's mend moves an admin Mac's held approver onto the installed release only where
/// the hold can no longer do what it was for: the held program is older than the first release
/// that reads the admin locker this Mac holds, and nobody pinned it. One that reads the locker
/// (a snapshot held back from a newer release that misbehaves), one a person pinned, one on a
/// development build or a Mac that administers no vault, and one whose release cannot be read
/// stay; while a sync or a publish holds this Mac's state lock nothing is judged.
/// NEGATIVE CONTROLS: a mend that moves any program older than this build moves the 0.142
/// snapshot, which reads the locker; one that ignores the pin moves the pinned job; an
/// [`admin_locker_here`] that reads a held state lock as no vault keeps the job as a member's.
#[test]
fn a_held_approver_moves_only_once_it_cannot_read_the_vaults_admin_locker() {
    use crate::service::{Held, Job};
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let admin = Paths::for_home(&root.path().join("ma"));
    let source = root.path().join("team.vault");
    create(&admin, source.to_str().expect("utf-8"), None, Some("ma")).expect("create");
    assert_eq!(
        admin_locker_here(&admin),
        AdminLocker::Reads(admin_keys::MAGIC),
        "CONTROL: this build reads the admin locker it wrote"
    );
    let release = locker_skew::Own {
        version: (0, 146, 0),
        release: true,
        program: None,
    };
    let held = Held {
        job: Job::Subscriber,
        program: PathBuf::from("/Users//x/.local/share/aterm-secrets/aterm"),
        args: ["secrets", "watch"].map(String::from).into(),
        pin: None,
    };
    let moves = std::cell::Cell::new(0);
    let mend = |paths: &Paths, own: &locker_skew::Own, held: &Held, at: Option<(u64, u64, u64)>| {
        mend_held_with(
            paths,
            own,
            held,
            &|program| {
                assert_eq!(program, held.program, "only the held program is read");
                at.ok_or_else(|| "timed out".to_string())
            },
            &mut |watch| {
                assert_eq!(watch, None, "an approver's move waits on no server");
                moves.set(moves.get() + 1);
                Ok(Some("moved".to_string()))
            },
        )
        .expect("judged")
    };
    let old = Some((0, 125, 0));
    assert_eq!(
        mend(&admin, &release, &held, Some((0, 142, 0))),
        HeldMend::Kept(HeldKept::Reads),
        "a snapshot that reads the locker is a hold that still works"
    );
    let pinned = Held {
        pin: Some(held.program.clone()),
        ..held.clone()
    };
    assert_eq!(
        mend(&admin, &release, &pinned, old),
        HeldMend::Kept(HeldKept::Pinned)
    );
    let development = locker_skew::Own {
        release: false,
        ..release.clone()
    };
    assert_eq!(
        mend(&admin, &development, &held, old),
        HeldMend::Kept(HeldKept::Development)
    );
    let member = Paths::for_home(&root.path().join("no-vault"));
    assert_eq!(
        mend(&member, &release, &held, old),
        HeldMend::Kept(HeldKept::NoAdmin)
    );
    assert!(matches!(
        mend(&admin, &release, &held, None),
        HeldMend::Kept(HeldKept::Unknown(_))
    ));
    {
        let _sync = StateLock::take(&admin).expect("a sync holds the state");
        assert!(
            matches!(mend(&admin, &release, &held, old), HeldMend::Unread(_)),
            "a busy state lock is not judged as no vault"
        );
    }
    assert_eq!(moves.get(), 0, "nothing moved yet");
    assert_eq!(
        mend(&admin, &release, &held, old),
        HeldMend::Moved("moved".to_string()),
        "a 0.125 approver cannot read the locker 0.138 introduced"
    );
    assert_eq!(moves.get(), 1);
}

/// On the ALab vault's admin Mac, the window's mend moves a held approver older than 0.146 onto
/// this release although it reads the admin locker: before 0.146 an approver admits by `open`
/// every Mac that asks and drops each request carrying a member's `Allowed` (it cannot parse
/// ` allowed=`), so a Mac let in by Allow and the phrase would wait on it for as long as the
/// hold lasts. A pinned one stays, as do one of 0.146 or later, one under a build older than
/// 0.146, and one on another vault.
/// NEGATIVE CONTROL: the mend before this fix (a held approver moves only once it cannot read
/// the locker) keeps the ALab admin's 0.145 approver as `Kept(Reads)`.
#[test]
fn the_alab_admins_held_approver_older_than_0_146_moves_onto_this_release() {
    use crate::envelope::{Retired, TEST_RETIRED};
    use crate::service::{Held, Job};
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let admin = Paths::for_home(&root.path().join("ma"));
    let source = root.path().join("alab.vault");
    create(&admin, source.to_str().expect("utf-8"), None, Some("ma")).expect("create");
    let signer = config(&admin).expect("config").expect("a vault").signer;
    let release = locker_skew::Own {
        version: (0, 146, 0),
        release: true,
        program: None,
    };
    let held = Held {
        job: Job::Subscriber,
        program: PathBuf::from("/Users//x/.local/share/aterm-secrets/aterm"),
        args: ["secrets", "watch"].map(String::from).into(),
        pin: None,
    };
    let moves = std::cell::Cell::new(0);
    let mend = |own: &locker_skew::Own, held: &Held, at: (u64, u64, u64)| {
        mend_held_with(&admin, own, held, &|_| Ok(at), &mut |watch| {
            assert_eq!(watch, None, "an approver's move waits on no server");
            moves.set(moves.get() + 1);
            Ok(Some("moved".to_string()))
        })
        .expect("judged")
    };
    let held_back = (0, 145, 0);
    assert_eq!(
        mend(&release, &held, held_back),
        HeldMend::Kept(HeldKept::Reads),
        "CONTROL: on another vault a 0.145 approver that reads the locker stays"
    );
    TEST_RETIRED.with(|cell| {
        *cell.borrow_mut() = Some(Retired {
            keys: Vec::new(),
            current: signer,
        });
    });
    let pinned = Held {
        pin: Some(held.program.clone()),
        ..held.clone()
    };
    assert_eq!(
        mend(&release, &pinned, held_back),
        HeldMend::Kept(HeldKept::Pinned)
    );
    assert_eq!(
        mend(&release, &held, (0, 146, 0)),
        HeldMend::Kept(HeldKept::Reads),
        "a 0.146 approver admits by Allowed"
    );
    let older_build = locker_skew::Own {
        version: (0, 145, 0),
        ..release.clone()
    };
    assert_eq!(
        mend(&older_build, &held, held_back),
        HeldMend::Kept(HeldKept::Reads),
        "a build that admits by open itself moves nothing for it"
    );
    assert_eq!(moves.get(), 0, "nothing moved yet");
    let moved = mend(&release, &held, held_back);
    TEST_RETIRED.with(|cell| *cell.borrow_mut() = None);
    assert_eq!(
        moved,
        HeldMend::Moved("moved".to_string()),
        "the ALab admin's 0.145 approver moves onto 0.146"
    );
    assert_eq!(moves.get(), 1);
}

/// The approver's log says how each Mac it let in got in: on a vault with a join password, the
/// Mac a member allowed is let in "by a member's Allow" and the one that typed the password by
/// the join password, in the same pass.
/// NEGATIVE CONTROL: one line naming every Mac by the vault's own policy (the old log) says the
/// allowed Mac came in by the join password, and the first assertion fails.
#[test]
fn the_approver_says_how_each_mac_it_let_in_got_in() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let server = serve_holding(&root.path().join("store"), Duration::from_millis(200));
    let (admin, source, password) = team(root.path(), &server);
    let mb = Paths::for_home(&root.path().join("mb"));
    super::handover::test_hand_allowed(&admin, &mb, "mb");
    let asked = join(
        &mb,
        &options(&source, JoinPassword::Given(None), None, Duration::ZERO),
    );
    assert!(
        matches!(asked, Ok(JoinOutcome::Pending { .. })),
        "{asked:?}"
    );
    let mc = Paths::for_home(&root.path().join("mc"));
    let asked = join(
        &mc,
        &JoinOptions {
            host: Some("mc".to_string()),
            ..options(
                &source,
                JoinPassword::Given(Some(password)),
                None,
                Duration::ZERO,
            )
        },
    );
    assert!(
        matches!(asked, Ok(JoinOutcome::Pending { .. })),
        "{asked:?}"
    );

    let said = std::cell::RefCell::new(Vec::new());
    let mut approver = Approver::default();
    for _ in 0..2 {
        approver_pass(
            &admin,
            &|line| said.borrow_mut().push(line.to_owned()),
            &mut approver,
        );
    }
    let said = said.into_inner();
    // How the approver's log credits `host`: the `by …` of each `approved` line naming it.
    let by_of = |host: &str| -> Vec<&str> {
        said.iter()
            .filter_map(|line| line.strip_prefix("approved "))
            .filter_map(|line| line.split_once(" by "))
            .filter(|(hosts, _)| hosts.split(", ").any(|named| named == host))
            .map(|(_, by)| by)
            .collect()
    };
    assert_eq!(
        by_of("mb"),
        [ALLOWED_BY.strip_prefix("by ").expect("a by")],
        "{said:?}"
    );
    assert_eq!(by_of("mc"), ["the join password"], "{said:?}");
}
