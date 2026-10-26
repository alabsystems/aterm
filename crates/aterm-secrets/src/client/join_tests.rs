// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A join's fetches and machine key, and the verdict an admin Mac posts on a mistyped join
//! password, against a server on loopback.

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
    let created = create(&admin, &url, None, Some("ma")).expect("create");
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
    assert_eq!(pause, Duration::from_secs(10), "by code, read every 10 s");
    let recorded: Vec<String> = asking(&admin).into_iter().map(|a| a.recipient).collect();
    assert_eq!(recorded, std::slice::from_ref(&key));
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
