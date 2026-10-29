// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! End-to-end loopback tests for secrets flows.

#![cfg(unix)]

use aterm_secrets::SecretBytes;
use aterm_secrets::age::Identity;
use aterm_secrets::client::{
    self, Approval, JoinOptions, JoinOutcome, Membership, Paths, ProbeOutcome, SyncOutcome,
};
use aterm_secrets::commons::bootstrap::{Bootstrap, Device};
use aterm_secrets::commons::journal::Journal;
use aterm_secrets::envelope::{self, Unsigned};
use aterm_secrets::members::{Allowed, Bundle, desk, join, joiner};
use aterm_secrets::phrase::Phrase;
use aterm_secrets::server::{self, Options, Running};
use aterm_secrets::text::{Audience, Document};
use aterm_secrets::transport::{self, Source};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

fn synthetic(label: &str) -> String {
    format!(
        "synthetic-{label}-{}",
        aterm_uds::rand::hex_token::<6>().expect("random")
    )
}

struct Lab {
    root: aterm_tempfile::TempDir,
    socket: std::net::TcpListener,
    server: Option<Running>,
    url: String,
    hold: Duration,
    gap: Duration,
}

fn start(
    listener: std::net::TcpListener,
    store: PathBuf,
    hold: Duration,
    gap: Duration,
) -> Running {
    server::start_with_join_change_gap(
        listener,
        &Options {
            listen: "127.0.0.1:0".parse().expect("addr"),
            store,
            advertise: None,
            pins: Vec::new(),
            hold,
            keep_awake: false,
        },
        gap,
    )
    .expect("server starts")
}

impl Lab {
    fn new(vault: &str) -> Lab {
        Lab::holding(vault, Duration::from_secs(50))
    }

    fn holding(vault: &str, hold: Duration) -> Lab {
        Lab::tuned(vault, hold, server::JOIN_CHANGE_GAP)
    }

    fn changing(vault: &str) -> Lab {
        Lab::tuned(vault, Duration::from_secs(50), Duration::ZERO)
    }

    fn tuned(vault: &str, hold: Duration, gap: Duration) -> Lab {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let socket = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let copy = socket.try_clone().expect("a copy of the socket");
        let server = start(copy, root.path().join("store"), hold, gap);
        let url = format!("http://{}/v1/{vault}", server.addr);
        Lab {
            root,
            socket,
            server: Some(server),
            url,
            hold,
            gap,
        }
    }

    fn restart_empty(&mut self) {
        self.server.take().expect("running").stop();
        let store = self.root.path().join("store");
        std::fs::remove_dir_all(&store).expect("store removed");
        let copy = self.socket.try_clone().expect("a copy of the socket");
        self.server = Some(start(copy, store, self.hold, self.gap));
    }

    /// Stop the server: no admin's approver, and no vault, can be reached.
    fn stop(&mut self) {
        self.server.take().expect("running").stop();
    }

    /// Serve the same store again at the same address.
    fn restart(&mut self) {
        let copy = self.socket.try_clone().expect("a copy of the socket");
        self.server = Some(start(
            copy,
            self.root.path().join("store"),
            self.hold,
            self.gap,
        ));
    }

    fn home(&self, name: &str) -> Paths {
        let home = self.root.path().join(name);
        std::fs::create_dir_all(&home).expect("home");
        Paths::for_home(&home)
    }
}

impl Drop for Lab {
    fn drop(&mut self) {
        if let Some(server) = self.server.take() {
            server.stop();
        }
    }
}

fn write_private(path: &Path, value: &str) {
    std::fs::create_dir_all(path.parent().expect("parent")).expect("dir");
    std::fs::write(path, value).expect("write");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).expect("chmod");
}

fn read(paths: &Paths, name: &str) -> Option<String> {
    std::fs::read_to_string(paths.home.join(".secrets").join(name)).ok()
}

fn mode(path: &Path) -> u32 {
    std::fs::metadata(path)
        .expect("metadata")
        .permissions()
        .mode()
        & 0o7777
}

fn join_unattended(paths: &Paths, source: &str, host: &str) -> aterm_secrets::Result<JoinOutcome> {
    join_with(paths, source, host, None)
}

fn join_with(
    paths: &Paths,
    source: &str,
    host: &str,
    password: Option<&[u8]>,
) -> aterm_secrets::Result<JoinOutcome> {
    let requested = std::cell::Cell::new(0);
    let outcome = client::join(
        paths,
        &JoinOptions {
            source: source.to_string(),
            host: Some(host.to_string()),
            interactive: false,
            wait: Duration::ZERO,
            confirm: &|_| panic!("an unattended join never asks"),
            password: aterm_secrets::client::JoinPassword::Ask(&|| password.map(SecretBytes::from)),
            fetched: None,
            requested: &|| requested.set(requested.get() + 1),
            say: &|_| {},
        },
    );
    if matches!(outcome, Ok(JoinOutcome::Pending { .. })) {
        assert_eq!(requested.get(), 1, "the request hook runs once per request");
    }
    outcome
}

fn approve_all(admin_paths: &Paths) -> u64 {
    let admin = client::admin_open(admin_paths).expect("admin opens");
    let mut doc = admin.doc.clone();
    let pending = client::requests(admin_paths, Some(&doc)).expect("requests");
    assert!(!pending.is_empty(), "someone asked to join");
    for request in pending {
        let host = client::free_name(&doc, &request.host);
        doc.add_machine(&host, &request.recipient).expect("add");
    }
    client::admin_publish(admin_paths, &admin, &doc)
        .expect("publish")
        .seq
}

fn set(admin_paths: &Paths, audience: &Audience, name: &str, value: &str) -> u64 {
    let admin = client::admin_open(admin_paths).expect("admin opens");
    let mut doc = admin.doc.clone();
    doc.set_entry(audience, name, value.as_bytes())
        .expect("set");
    client::admin_publish(admin_paths, &admin, &doc)
        .expect("publish")
        .seq
}

fn applied(outcome: SyncOutcome) -> (u64, aterm_secrets::materialize::Report) {
    match outcome {
        SyncOutcome::Applied { seq, report } => (seq, report),
        other => panic!("expected a new version, got {other:?}"),
    }
}

#[test]
fn a_lan_vault_from_create_to_revoke() {
    let lab = Lab::new("lab");
    let admin = lab.home("ma");
    let token = synthetic("gh");
    let other = synthetic("other");
    write_private(&admin.home.join(".secrets/gh_access_token"), &token);
    write_private(&admin.home.join(".secrets/other"), &other);

    let created = client::create(
        &admin,
        &lab.url,
        Some(&admin.home.join(".secrets")),
        Some("ma"),
    )
    .expect("create");
    assert_eq!(created.imported, ["gh_access_token", "other"]);
    assert_eq!(
        read(&admin, "gh_access_token").as_deref(),
        Some(token.as_str())
    );
    assert!(
        !admin.home.join(".secrets/.replaced").exists(),
        "nothing was moved aside"
    );
    assert!(matches!(
        client::status(&admin).expect("status").membership,
        Membership::In { seq: 1, .. }
    ));
    assert!(
        client::create(&admin, &lab.url, None, Some("ma")).is_err(),
        "a second create over a live vault is refused"
    );

    let machine = lab.home("mb");
    let pinned = format!("{}#{}", lab.url, created.signer);
    let JoinOutcome::Pending { code, host } =
        join_unattended(&machine, &pinned, "mb").expect("join")
    else {
        panic!("a machine not in the vault waits for approval");
    };
    assert_eq!(host, "mb");
    assert_eq!(
        client::status(&machine).expect("status").membership,
        Membership::Pending
    );
    assert_eq!(read(&machine, "gh_access_token"), None);
    let pending = client::requests(&admin, None).expect("requests");
    assert!(
        pending
            .iter()
            .any(|request| request.code == code && request.host == "mb")
    );

    assert_eq!(approve_all(&admin), 2);
    let (seq, _) = applied(client::sync(&machine, None).expect("sync"));
    assert_eq!(seq, 2);
    assert_eq!(
        read(&machine, "gh_access_token").as_deref(),
        Some(token.as_str())
    );
    assert_eq!(mode(&machine.home.join(".secrets/gh_access_token")), 0o600);
    assert_eq!(mode(&machine.home.join(".secrets")), 0o700);
    assert!(
        client::requests(&admin, Some(&client::admin_open(&admin).expect("open").doc))
            .expect("requests")
            .is_empty()
    );

    let release = synthetic("release");
    assert_eq!(
        set(
            &admin,
            &Audience::Hosts(vec!["ma".into()]),
            "release_key",
            &release
        ),
        3
    );
    applied(client::sync(&machine, None).expect("sync"));
    assert_eq!(read(&machine, "release_key"), None);
    assert_eq!(
        read(&admin, "release_key").as_deref(),
        Some(release.as_str())
    );

    let waiting = {
        let machine = machine.clone();
        std::thread::spawn(move || client::sync(&machine, Some(Duration::from_secs(50))))
    };
    let fresh = synthetic("fresh");
    assert_eq!(set(&admin, &Audience::Everyone, "fresh", &fresh), 4);
    let (seq, _) = applied(waiting.join().expect("thread").expect("long-poll"));
    assert_eq!(seq, 4);
    assert_eq!(read(&machine, "fresh").as_deref(), Some(fresh.as_str()));
    assert_eq!(
        client::sync(&machine, None).expect("sync"),
        SyncOutcome::Unchanged { seq: 4 }
    );

    {
        let admin_vault = client::admin_open(&admin).expect("open");
        let mut doc = admin_vault.doc.clone();
        assert!(
            doc.unset_entry(&Audience::Everyone, "fresh")
                .expect("unset")
        );
        client::admin_publish(&admin, &admin_vault, &doc).expect("publish");
    }
    let (_, report) = applied(client::sync(&machine, None).expect("sync"));
    assert_eq!(report.removed, ["fresh"]);
    assert_eq!(read(&machine, "fresh"), None);
    let by_hand = synthetic("by-hand");
    write_private(&machine.home.join(".secrets/other"), &by_hand);
    let replacement = synthetic("replacement");
    set(&admin, &Audience::Everyone, "other", &replacement);
    let (_, report) = applied(client::sync(&machine, None).expect("sync"));
    assert_eq!(
        read(&machine, "other").as_deref(),
        Some(replacement.as_str())
    );
    let [(name, aside)] = report.moved_aside.as_slice() else {
        panic!("one file moved aside: {report:?}");
    };
    assert_eq!(name, "other");
    assert_eq!(std::fs::read_to_string(aside).expect("aside"), by_hand);

    {
        let admin_vault = client::admin_open(&admin).expect("open");
        client::admin_revoke(&admin, &admin_vault, "mb", false).expect("revoke");
    }
    assert_eq!(
        client::sync(&machine, None).expect("sync"),
        SyncOutcome::NotIn
    );
    assert!(matches!(
        client::status(&machine).expect("status").membership,
        Membership::Revoked { .. }
    ));

    for paths in [&admin, &machine] {
        let shown = format!("{:?}", client::status(paths).expect("status"));
        for value in [&token, &other, &release, &fresh, &replacement] {
            assert!(
                !shown.contains(value.as_str()),
                "status shows a value: {shown}"
            );
        }
    }
}

#[test]
fn a_fifo_at_the_store_files_lock_refuses_a_publish_without_a_wait() {
    let lab = Lab::new("fifo");
    let admin = lab.home("ma");
    client::create(&admin, &lab.url, None, Some("ma")).expect("create");
    let lock = lab.root.path().join("store").join("fifo.vault.lock");
    let lock = std::ffi::CString::new(std::os::unix::ffi::OsStringExt::into_vec(
        lock.into_os_string(),
    ))
    .expect("a path");
    // SAFETY: a NUL-terminated path; mkfifo reads it and keeps nothing.
    assert_eq!(unsafe { libc::mkfifo(lock.as_ptr(), 0o600) }, 0, "mkfifo");
    let open = client::admin_open(&admin).expect("admin opens");
    let mut doc = open.doc.clone();
    doc.set_entry(&Audience::Everyone, "k", synthetic("v").as_bytes())
        .expect("set");
    // A server that waited on the FIFO would hold its state lock, so a hang here would hang
    // the lab's stop too: it is detected on a thread, and the lab then left running.
    let (answered, answer) = std::sync::mpsc::channel();
    let publisher = admin.clone();
    std::thread::spawn(move || {
        let _ = answered.send(client::admin_publish(&publisher, &open, &doc));
    });
    let Ok(published) = answer.recv_timeout(Duration::from_secs(60)) else {
        std::mem::forget(lab);
        panic!("the PUT waited on the FIFO at the lock");
    };
    let error = published.expect_err("not a lock it clears");
    assert!(error.0.contains("409"), "{error}");
    assert!(
        matches!(
            transport::fetch(&Source::Url(lab.url.clone()), None, Duration::from_secs(60)),
            Ok(transport::Fetched::Body(_))
        ),
        "still serving"
    );
}

#[test]
fn the_server_and_every_reader_refuse_a_key_the_vault_never_endorsed() {
    let lab = Lab::new("pinned");
    let admin = lab.home("ma");
    let created = client::create(&admin, &lab.url, None, Some("ma")).expect("create");

    let mut seed = [0u8; 32];
    aterm_uds::rand::fill(&mut seed).expect("random");
    let forged = envelope::sign(
        &Unsigned {
            name: "pinned".into(),
            seq: 99,
            time: 0,
            handoff: Vec::new(),
            parents: Vec::new(),
            lockers: Vec::new(),
        },
        &seed,
    )
    .expect("sign");
    assert!(
        transport::publish(&Source::Url(lab.url.clone()), &forged).is_err(),
        "the server keeps the first key it accepted"
    );
    assert!(matches!(
        client::status(&admin).expect("status").membership,
        Membership::In { seq: 1, .. }
    ));

    let stranger = envelope::signer_public(&seed).expect("public");
    let wrong = format!(
        "{}#{}",
        lab.url,
        aterm_secrets::b64::encode_padded(&stranger)
    );
    assert!(join_unattended(&lab.home("mc"), &wrong, "mc").is_err());

    let machine = lab.home("mb");
    assert!(join_unattended(&machine, &lab.url, "mb").is_err());
    assert_eq!(client::config(&machine).expect("config"), None);

    let shown = std::cell::RefCell::new(String::new());
    let outcome = client::join(
        &machine,
        &JoinOptions {
            source: lab.url.clone(),
            host: Some("mb".into()),
            interactive: true,
            wait: Duration::ZERO,
            confirm: &|fingerprint| {
                *shown.borrow_mut() = fingerprint.to_string();
                true
            },
            password: aterm_secrets::client::JoinPassword::Given(None),
            fetched: None,
            requested: &|| {},
            say: &|_| {},
        },
    )
    .expect("join");
    assert!(matches!(outcome, JoinOutcome::Pending { .. }));
    assert_eq!(*shown.borrow(), created.fingerprint);
}

#[test]
fn removing_an_admin_rotates_the_key_and_every_machine_re_pins() {
    let lab = Lab::new("rotating");
    let admin = lab.home("ma");
    let created = client::test_create_unbound(&admin, &lab.url, None, Some("ma")).expect("create");
    let machine = lab.home("mb");
    join_unattended(&machine, &format!("{}#{}", lab.url, created.signer), "mb").expect("join");
    approve_all(&admin);
    applied(client::sync(&machine, None).expect("sync"));
    {
        let vault = client::admin_open(&admin).expect("open");
        let mut doc = vault.doc.clone();
        doc.add_admin("mb").expect("add admin");
        client::admin_publish(&admin, &vault, &doc).expect("publish");
    }
    applied(client::sync(&machine, None).expect("sync"));
    let old = client::config(&machine)
        .expect("config")
        .expect("configured")
        .signer;

    {
        let vault = client::admin_open(&admin).expect("open");
        let mut doc = vault.doc.clone();
        assert!(doc.remove_admin("mb").expect("remove admin"));
        client::admin_rotate(&admin, &vault, &doc).expect("rotate");
    }
    applied(client::sync(&machine, None).expect("sync"));
    let new = client::config(&machine)
        .expect("config")
        .expect("configured")
        .signer;
    assert_ne!(old, new, "the machine re-pinned");
    assert_eq!(
        client::config(&admin)
            .expect("config")
            .expect("configured")
            .signer,
        new
    );
    assert!(
        client::admin_open(&machine).is_err(),
        "mb is no longer an admin"
    );

    let after = synthetic("after");
    set(&admin, &Audience::Everyone, "after", &after);
    applied(client::sync(&machine, None).expect("sync"));
    assert_eq!(read(&machine, "after").as_deref(), Some(after.as_str()));
}

#[test]
fn a_signed_vault_at_a_path_needs_no_server() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let shared: PathBuf = root.path().join("shared").join("team.vault");
    std::fs::create_dir_all(shared.parent().expect("parent")).expect("dir");
    let admin = Paths::for_home(&root.path().join("ma"));
    let created =
        client::create(&admin, shared.to_str().expect("utf-8"), None, Some("ma")).expect("create");
    let machine = Paths::for_home(&root.path().join("mb"));
    let source = format!("{}#{}", shared.display(), created.signer);
    assert!(matches!(
        join_unattended(&machine, &source, "mb").expect("join"),
        JoinOutcome::Pending { .. }
    ));
    let recipient = client::status(&machine)
        .expect("status")
        .machine
        .expect("a key")
        .0
        .to_bech32();
    let token = synthetic("team");
    {
        let vault = client::admin_open(&admin).expect("open");
        let mut doc = vault.doc.clone();
        doc.add_machine("mb", &recipient).expect("add");
        doc.set_entry(&Audience::Everyone, "team_token", token.as_bytes())
            .expect("set");
        client::admin_publish(&admin, &vault, &doc).expect("publish");
    }
    applied(client::sync(&machine, None).expect("sync"));
    assert_eq!(
        read(&machine, "team_token").as_deref(),
        Some(token.as_str())
    );

    let vault = client::admin_open(&admin).expect("open");
    let mut doc = vault.doc.clone();
    doc.set_open(true).expect("open");
    client::admin_publish(&admin, &vault, &doc).expect("publish");
    let vault = client::admin_open(&admin).expect("open");
    let gone = client::admin_revoke(&admin, &vault, "mb", false)
        .expect("revoke")
        .published;
    assert!(gone.kept_out);
    assert!(
        !gone.queued,
        "a path has no queue for a code to come back by"
    );
}

#[test]
fn a_plain_text_file_at_a_path_is_read_as_is() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let file = root.path().join("plain.txt");
    let token = synthetic("plain");
    let only = synthetic("only");
    write_private(
        &file,
        &format!("gh_access_token={token}\n\n[mb]\nonly_mb={only}\n\n[mc]\nonly_mc=x\n"),
    );
    let machine = Paths::for_home(&root.path().join("mb"));
    let JoinOutcome::Plain { names } =
        join_unattended(&machine, file.to_str().expect("utf-8"), "mb").expect("join")
    else {
        panic!("a plain file joins as plain");
    };
    assert_eq!(names, ["gh_access_token", "only_mb"]);
    assert_eq!(
        read(&machine, "gh_access_token").as_deref(),
        Some(token.as_str())
    );
    assert_eq!(read(&machine, "only_mb").as_deref(), Some(only.as_str()));
    assert_eq!(read(&machine, "only_mc"), None);
    assert_eq!(
        client::status(&machine).expect("status").membership,
        Membership::Plain
    );

    let next = synthetic("next");
    write_private(&file, &format!("gh_access_token={next}\n"));
    let (_, report) = applied(client::sync(&machine, None).expect("sync"));
    assert_eq!(
        read(&machine, "gh_access_token").as_deref(),
        Some(next.as_str())
    );
    assert_eq!(report.removed, ["only_mb"]);

    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).expect("chmod");
    assert!(client::sync(&machine, None).is_err());
}

#[test]
fn a_machine_that_knows_the_join_password_is_let_in_by_the_admins_subscriber() {
    let lab = Lab::new("door");
    let admin = lab.home("ma");
    let token = synthetic("gh");
    write_private(&admin.home.join(".secrets/gh_access_token"), &token);
    let created = client::test_create_with_password(
        &admin,
        &lab.url,
        Some(&admin.home.join(".secrets")),
        Some("ma"),
    )
    .expect("create");
    let password = client::admin_open(&admin)
        .expect("open")
        .doc
        .join_password()
        .expect("create makes a join password");
    let pinned = format!("{}#{}", lab.url, created.signer);

    let stranger = lab.home("mx");
    assert!(matches!(
        join_with(&stranger, &pinned, "mx", Some(b"not-the-password-at-all")).expect("join"),
        JoinOutcome::Pending { .. }
    ));
    let machine = lab.home("mb");
    let mut typed = b"  ".to_vec();
    typed.extend_from_slice(password.as_slice());
    typed.push(b'\n');
    assert!(matches!(
        join_with(&machine, &pinned, "mb", Some(&typed)).expect("join"),
        JoinOutcome::Pending { .. }
    ));
    let by_code = lab.home("mc");
    join_unattended(&by_code, &pinned, "mc").expect("join");

    let done = client::auto_approve(&admin).expect("auto-approve");
    assert_eq!(done.approved, ["mb"]);
    assert_eq!(done.refused.len(), 1, "the wrong password is refused");
    assert_eq!(done.seq, Some(2));
    applied(client::sync(&machine, None).expect("sync"));
    assert_eq!(
        read(&machine, "gh_access_token").as_deref(),
        Some(token.as_str())
    );
    assert_eq!(
        client::sync(&stranger, None).expect("sync"),
        SyncOutcome::NotIn
    );
    assert_eq!(
        client::sync(&by_code, None).expect("sync"),
        SyncOutcome::NotIn
    );
    let waiting = client::requests(&admin, Some(&client::admin_open(&admin).expect("open").doc))
        .expect("requests");
    let mut hosts: Vec<&str> = waiting.iter().map(|r| r.host.as_str()).collect();
    hosts.sort_unstable();
    assert_eq!(hosts, ["mc", "mx"], "the others still wait for a person");

    let again = client::auto_approve(&admin).expect("auto-approve");
    assert!(again.approved.is_empty());
    assert_eq!(again.seq, None);

    let shown = std::fs::read_dir(machine.home.join(".secrets"))
        .expect("dir")
        .filter_map(Result::ok)
        .filter_map(|e| std::fs::read(e.path()).ok())
        .any(|bytes| {
            bytes
                .windows(password.as_slice().len())
                .any(|w| w == password.as_slice())
        });
    assert!(!shown, "a member's files hold the join password");
}

/// `member` lets `joiner` in the way a person does: `joiner` asks its desk, Allow there shows a
/// phrase, `joiner` runs the phrase and gets the member's bundle. Returns the bundle, whose
/// Allowed the member signed.
fn allow_through(member: &Paths, joiner: &Paths, host: &str) -> Bundle {
    let local = desk::Local::of(member)
        .expect("member state")
        .expect("a Commons member");
    let desk = Mutex::new(desk::Desk::new(local));
    client::machine_identity(joiner).expect("identity");
    let device = Device::open(joiner).expect("device").expect("an identity");
    let joining = joiner::Joining {
        device: &device,
        node: [7; 32],
        name: host.into(),
        model: "Mac15,3".into(),
    };
    let from: std::net::IpAddr = [127, 0, 0, 1].into();
    let bundle = |handing: &desk::Handing<'_>| client::hand_over(member, handing);
    let connect = |speak: &dyn Fn(&mut UnixStream)| {
        let (mut ours, mut theirs) = UnixStream::pair().expect("pair");
        for end in [&ours, &theirs] {
            end.set_read_timeout(Some(Duration::from_secs(60)))
                .expect("a one-minute hang detector");
        }
        std::thread::scope(|scope| {
            let served = scope.spawn(|| desk::serve(&desk, &mut ours, from, &bundle));
            speak(&mut theirs);
            drop(theirs);
            served.join().expect("member").expect("served")
        })
    };
    connect(&|stream| {
        let greeting = joiner::greeting(stream).expect("greeting");
        joiner::ask(stream, &greeting, &joining).expect("asked");
    });
    let words = {
        let mut desk = desk.lock().unwrap();
        let now = aterm_time::Instant::now();
        let row = desk.rows(now)[0].id;
        let phrase = desk.allow(row, now).expect("Allow");
        desk.words(phrase, now).expect("live").join(" ")
    };
    let phrase = Phrase::parse(&words).expect("three words");
    let got = std::cell::RefCell::new(None);
    let let_in = connect(&|stream| {
        let greeting = joiner::greeting(stream).expect("greeting");
        *got.borrow_mut() = Some(joiner::run(stream, &greeting, &joining, &phrase).expect("run"));
    })
    .expect("the run sent its bundle");
    let bundle = got.into_inner().expect("a bundle");
    assert_eq!(bundle.allowed, let_in.allowed);
    bundle
}

/// install.sh's windowless lanes run `aterm members join --no-wait` (§3.3.3): it asks every
/// member it finds at once and returns within install.sh's bound (`VERIFY_WAIT_SECS`, 60 s),
/// a member that never answers and one that is gone included, leaving its request on the
/// member's desk. The phrase a person then reads off that member and types gets the new Mac
/// GitHub, publishing and AI accounts in `~/.secrets` at once, before any admin's approver
/// runs; the request it posts with its Allowed is admitted when one does.
#[test]
fn the_install_lane_returns_within_its_bound_and_leaves_a_pending_request() {
    let lab = Lab::changing("lane");
    let admin = lab.home("ma");
    let files = [
        ("gh_access_token", synthetic("gh")),
        ("gh_access_token_alabsystems", synthetic("publish")),
        ("ai-accounts.csv", "claude,first@example.com\n".to_owned()),
    ];
    for (name, value) in &files {
        write_private(&admin.home.join(".secrets").join(name), value);
    }
    client::create(
        &admin,
        &lab.url,
        Some(&admin.home.join(".secrets")),
        Some("ma"),
    )
    .expect("create");
    let member = Mutex::new(desk::Desk::new(
        desk::Local::of(&admin)
            .expect("member state")
            .expect("a Commons member"),
    ));
    let machine = lab.home("mb");
    client::machine_identity(&machine).expect("identity");
    let device = Device::open(&machine)
        .expect("device")
        .expect("an identity");
    let joining = joiner::Joining {
        device: &device,
        node: [7; 32],
        name: "mb".into(),
        model: "Mac15,3".into(),
    };
    let bundle = |handing: &desk::Handing<'_>| client::hand_over(&admin, handing);
    std::thread::scope(|scope| {
        let mut silent_ends = Vec::new();
        let silent = Mutex::new(&mut silent_ends);
        let dial = |at: &str| -> aterm_secrets::Result<Box<dyn join::Stream>> {
            let (ours, theirs) = UnixStream::pair()?;
            match at {
                "ma" => {
                    for end in [&ours, &theirs] {
                        end.set_read_timeout(Some(Duration::from_secs(60)))?;
                    }
                    let (member, bundle) = (&member, &bundle);
                    scope.spawn(move || {
                        let mut ours = ours;
                        let _ = desk::serve(member, &mut ours, [10, 0, 0, 9].into(), bundle);
                    });
                    Ok(Box::new(theirs))
                }
                // A member that takes the connection and never speaks: the dial's own bound,
                // as `dial_ask`'s handshake has one, ends the wait.
                "silent" => {
                    theirs.set_read_timeout(Some(Duration::from_secs(2)))?;
                    silent.lock().unwrap().push(ours);
                    Ok(Box::new(theirs))
                }
                _ => Err(aterm_secrets::Error::new("connection refused")),
            }
        };
        let ats = ["ma", "silent", "gone"].map(String::from);
        let started = std::time::Instant::now();
        let (heard, missed) = join::ask_all(&dial, &joining, &ats);
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "past install.sh's bound: {:?}",
            started.elapsed()
        );
        assert_eq!(heard.len(), 1, "{missed:?}");
        assert_eq!(heard[0].answer, desk::Answer::Asking);
        let mut missed: Vec<&str> = missed.iter().map(|(at, _)| at.as_str()).collect();
        missed.sort_unstable();
        assert_eq!(missed, ["gone", "silent"]);
        let now = aterm_time::Instant::now();
        let rows = member.lock().unwrap().rows(now);
        assert_eq!(rows.len(), 1, "the request waits on the member's desk");
        assert_eq!(
            (rows[0].name.as_str(), rows[0].state),
            ("mb", desk::RowState::Asking)
        );
        assert_eq!(
            read(&machine, "gh_access_token"),
            None,
            "nothing before an Allow"
        );
        assert!(
            join::pick(&heard, None).is_err(),
            "no phrase is shown for this Mac yet"
        );

        // A person presses Allow on ma, and types its phrase on mb; no approver runs.
        let words = {
            let mut desk = member.lock().unwrap();
            let phrase = desk.allow(rows[0].id, now).expect("Allow");
            desk.words(phrase, now).expect("live").join(" ")
        };
        let phrase = Phrase::parse(&words).expect("three words");
        let (heard, _) = join::ask_all(&dial, &joining, &ats);
        assert_eq!(heard[0].answer, desk::Answer::Allowed);
        let joined = join::run_phrase(&machine, &dial, &joining, &heard, &phrase, None)
            .expect("the phrase let it in");
        assert_eq!((joined.member.as_str(), joined.name.as_str()), ("ma", "mb"));
        assert!(
            matches!(joined.admission, join::Admission::Asked(Ok(_))),
            "a vault with no log asks its server with the Allowed: {:?}",
            joined.admission
        );
        for (name, value) in &files {
            assert_eq!(
                read(&machine, name).as_deref(),
                Some(value.as_str()),
                "{name}"
            );
        }
        assert!(matches!(
            client::status(&machine).expect("status").membership,
            Membership::Pending
        ));
        assert_eq!(
            member.lock().unwrap().rows(aterm_time::Instant::now())[0].state,
            desk::RowState::Closed
        );
    });

    assert_eq!(approved(&admin), ["mb"], "the request carried its Allowed");
    applied(client::sync(&machine, None).expect("sync"));
    assert!(matches!(
        client::status(&machine).expect("status").membership,
        Membership::In { .. }
    ));
}

/// What the approver judged of `admin`'s queue: the names it let in.
fn approved(admin: &Paths) -> Vec<String> {
    let mut approved = client::auto_approve(admin).expect("approve").approved;
    approved.sort_unstable();
    approved
}

/// An open vault (`open = yes`) lets no machine in by itself: not one that asks with no
/// password, nor one that proves an old one. Only a request carrying a member's Allow is
/// admitted, recorded as a person's decision, and once the vault lets that machine go, its
/// Allow, still queued, cannot bring it back (docs/DESIGN-fluid-swarm-2026-10-28.md F10).
#[test]
fn a_request_with_no_allow_is_never_admitted() {
    let lab = Lab::changing("open");
    let admin = lab.home("ma");
    let token = synthetic("gh");
    write_private(&admin.home.join(".secrets/gh_access_token"), &token);
    let created = client::test_create_with_password(
        &admin,
        &lab.url,
        Some(&admin.home.join(".secrets")),
        Some("ma"),
    )
    .expect("create");
    let password = client::admin_open(&admin)
        .expect("open")
        .doc
        .join_password()
        .expect("create makes a join password");
    let opened = client::admin_open(&admin).expect("open");
    let mut doc = opened.doc.clone();
    doc.set_open(true).expect("open");
    client::admin_publish(&admin, &opened, &doc).expect("publish");
    let doc = client::admin_open(&admin).expect("open").doc.clone();
    assert_eq!(Approval::of(&doc), Some(Approval::Open));
    let pinned = format!("{}#{}", lab.url, created.signer);

    let machine = lab.home("mb");
    assert!(matches!(
        join_unattended(&machine, &pinned, "mb").expect("join"),
        JoinOutcome::Pending { .. }
    ));
    let proving = lab.home("mc");
    join_with(&proving, &pinned, "mc", Some(password.as_slice())).expect("join");
    let done = client::auto_approve(&admin).expect("approve");
    assert!(done.approved.is_empty(), "{:?}", done.approved);
    let mut asking: Vec<&str> = done.asking.iter().map(|a| a.host.as_str()).collect();
    asking.sort_unstable();
    assert_eq!(asking, ["mb", "mc"], "both still wait for a person");
    assert_eq!(
        client::sync(&machine, None).expect("sync"),
        SyncOutcome::NotIn
    );

    let bundle = allow_through(&admin, &machine, "mb");
    client::post_allowed(&admin, &bundle.allowed).expect("posted");
    assert_eq!(approved(&admin), ["mb"], "the Allowed request");
    let doc = client::admin_open(&admin).expect("open").doc.clone();
    let mb = recipient_of(&doc, "mb");
    assert_eq!(
        client::admin_cached(&admin)
            .expect("admin")
            .commons_admission("mb")
            .expect("recorded")
            .method,
        client::AdmissionMethod::Manual
    );
    applied(client::sync(&machine, None).expect("sync"));
    assert_eq!(
        read(&machine, "gh_access_token").as_deref(),
        Some(token.as_str())
    );
    assert_eq!(
        client::sync(&proving, None).expect("sync"),
        SyncOutcome::NotIn
    );

    let open = client::admin_open(&admin).expect("open");
    client::admin_revoke(&admin, &open, "mb", false).expect("revoke");
    let kept = client::admin_open(&admin).expect("open").doc.clone();
    assert_eq!(kept.code_only(), std::slice::from_ref(&mb));
    client::post_allowed(&admin, &bundle.allowed).expect("posted again");
    assert!(approved(&admin).is_empty(), "a let-go key's old Allow");
    assert!(matches!(
        client::sync(&machine, None).expect("sync"),
        SyncOutcome::NotIn
    ));
}

/// What `paths` holds of Commons control: its state (one head, or a conflict of several) and
/// every control node it holds; `None` while it holds no Commons control at all.
fn control(paths: &Paths) -> Option<(String, Vec<String>)> {
    let bootstrap = Bootstrap::cached(paths).expect("Commons state")?;
    let journal = Journal::read_existing(bootstrap.journal_dir(), bootstrap.trust())
        .expect("control journal");
    let mut nodes: Vec<String> = journal.inventory().map(|id| format!("{id:?}")).collect();
    nodes.sort_unstable();
    Some((format!("{:?}", journal.state()), nodes))
}

/// An Allow writes no Commons control: the member signs an `Allowed` with its writer key, the new
/// Mac holds no control of its own while it waits, and the admin's approver records a vault
/// admission, never a control node. Two Allows of two Macs, as members on the two sides of a
/// split press them, so leave every control history one head; a control node is still written
/// only by the enrollment an admin's Commons runtime makes (`docs/measured/
/// swarm-partition-2026-10-28.md`: admins enrolling on both sides fork it until F18).
#[test]
fn an_allow_writes_no_commons_control_so_two_sides_of_a_split_cannot_fork_it() {
    let lab = Lab::changing("no-fork");
    let admin = lab.home("ma");
    write_private(
        &admin.home.join(".secrets/gh_access_token"),
        &synthetic("gh"),
    );
    client::create(
        &admin,
        &lab.url,
        Some(&admin.home.join(".secrets")),
        Some("ma"),
    )
    .expect("create");
    let before = control(&admin).expect("the founder holds Commons control");
    assert!(before.0.starts_with("Ready"), "{}", before.0);

    let (mb, mc) = (lab.home("mb"), lab.home("mc"));
    let bundles = [
        allow_through(&admin, &mb, "mb"),
        allow_through(&admin, &mc, "mc"),
    ];
    for (machine, bundle) in [(&mb, &bundles[0]), (&mc, &bundles[1])] {
        client::write_from_bundle(machine, bundle).expect("written");
        assert_eq!(control(machine), None, "a Mac let in by an Allow");
    }
    assert_eq!(control(&admin).as_ref(), Some(&before), "an Allow");

    for bundle in &bundles {
        client::post_allowed(&admin, &bundle.allowed).expect("posted");
    }
    assert_eq!(approved(&admin), ["mb", "mc"]);
    assert_eq!(
        control(&admin).as_ref(),
        Some(&before),
        "the approver's admissions"
    );
    for machine in [&mb, &mc] {
        applied(client::sync(machine, None).expect("sync"));
        assert!(
            control(machine).is_none_or(|held| held.1.iter().all(|id| before.1.contains(id))),
            "an admitted Mac holds a control node the founder does not"
        );
    }
}

/// A vault `create` makes is open (owner, 2026-10-26: "default ON") and holds no join password,
/// and it lets in only a machine a member Allowed: one that types nothing still waits.
#[test]
fn a_new_vault_lets_in_a_machine_a_member_allowed() {
    let lab = Lab::changing("new-open");
    let admin = lab.home("ma");
    let token = synthetic("gh");
    write_private(&admin.home.join(".secrets/gh_access_token"), &token);
    let created = client::create(
        &admin,
        &lab.url,
        Some(&admin.home.join(".secrets")),
        Some("ma"),
    )
    .expect("create");
    let doc = client::admin_open(&admin).expect("open").doc.clone();
    assert!(doc.open());
    assert!(doc.join_password().is_none());
    assert!(doc.code_only().is_empty());
    assert_eq!(Approval::of(&doc), Some(Approval::Open));

    let pinned = format!("{}#{}", lab.url, created.signer);
    let machine = lab.home("mb");
    assert!(matches!(
        join_unattended(&machine, &pinned, "mb").expect("join"),
        JoinOutcome::Pending { .. }
    ));
    assert!(approved(&admin).is_empty());
    let bundle = allow_through(&admin, &machine, "mb");
    client::post_allowed(&admin, &bundle.allowed).expect("posted");
    let done = client::auto_approve(&admin).expect("approve");
    assert_eq!(done.approved, ["mb"]);
    assert!(done.refused.is_empty() && done.code_only.is_empty());
    applied(client::sync(&machine, None).expect("sync"));
    assert_eq!(
        read(&machine, "gh_access_token").as_deref(),
        Some(token.as_str())
    );
}

/// An Allow signed by a key the admin's Commons roster binds to no member admits nothing: the
/// envelope's `[machines]` keys are age keys, which cannot sign, so only the roster says who
/// allowed.
#[test]
fn an_allowed_signed_by_a_key_no_roster_member_binds_is_refused() {
    let lab = Lab::changing("forged");
    let admin = lab.home("ma");
    let created = client::create(&admin, &lab.url, None, Some("ma")).expect("create");
    let pinned = format!("{}#{}", lab.url, created.signer);
    let machine = lab.home("mb");
    join_unattended(&machine, &pinned, "mb").expect("join");
    let trust = Bootstrap::cached(&admin)
        .expect("bootstrap")
        .expect("Commons")
        .trust();
    let device = Device::open(&machine).expect("device").expect("identity");
    let request = device
        .request(trust.network, trust.authority)
        .expect("request");
    let stranger = [0x5a; 32];
    let forged = Allowed::sign(request.binding(), "mb", &stranger).expect("signed");
    let post = |allowed: &Allowed| {
        let recipient = request.binding().recipient();
        let line = transport::JoinRequest {
            recipient: recipient.to_bech32(),
            host: "mb".into(),
            code: client::join_code(&recipient),
            time: None,
            allowed: Some(allowed.to_text()),
            proof: None,
        }
        .line();
        transport::post_join(&lab.url, &line).expect("posted");
    };
    post(&forged);
    assert!(approved(&admin).is_empty(), "a forged Allow admitted");
    assert_eq!(
        client::sync(&machine, None).expect("sync"),
        SyncOutcome::NotIn
    );
    // Control: the same request, Allowed by the member the roster binds, is let in.
    let bundle = allow_through(&admin, &machine, "mb");
    post(&bundle.allowed);
    assert_eq!(approved(&admin), ["mb"]);
}

/// A Mac a member Allowed while no admin's approver can run writes `~/.secrets` from the
/// member's bundle at once, stays pending, and the admission it later gets finds those files
/// already the vault's.
#[test]
fn a_mac_allowed_with_no_admin_reachable_writes_its_secrets_from_the_bundle() {
    let mut lab = Lab::changing("handover");
    let admin = lab.home("ma");
    let token = synthetic("gh");
    write_private(&admin.home.join(".secrets/gh_access_token"), &token);
    client::create(
        &admin,
        &lab.url,
        Some(&admin.home.join(".secrets")),
        Some("ma"),
    )
    .expect("create");
    set(
        &admin,
        &Audience::Hosts(vec!["ma".into()]),
        "only_ma",
        "kept",
    );
    let machine = lab.home("mb");
    let bundle = allow_through(&admin, &machine, "mb");
    lab.stop();
    let report = client::write_from_bundle(&machine, &bundle).expect("written");
    assert_eq!(report.written, ["gh_access_token"]);
    assert_eq!(
        read(&machine, "gh_access_token").as_deref(),
        Some(token.as_str())
    );
    assert_eq!(read(&machine, "only_ma"), None, "only [everyone] files");
    assert_eq!(mode(&machine.home.join(".secrets/gh_access_token")), 0o600);
    assert!(matches!(
        client::status(&machine).expect("status").membership,
        Membership::Pending
    ));
    assert!(
        client::write_from_bundle(&lab.home("mx"), &bundle).is_err(),
        "a bundle for another Mac's key"
    );

    lab.restart();
    client::post_allowed(&admin, &bundle.allowed).expect("posted");
    assert_eq!(approved(&admin), ["mb"]);
    assert_eq!(
        client::config(&machine)
            .expect("config")
            .expect("set")
            .source,
        Source::Url(lab.url.clone())
    );
    let (_, report) = applied(client::sync(&machine, None).expect("sync"));
    assert!(report.written.is_empty(), "{:?}", report.written);
    assert!(matches!(
        client::status(&machine).expect("status").membership,
        Membership::In { .. }
    ));
    assert!(
        !machine.dir.join("allowed").exists(),
        "an admitted Mac forgets its Allow"
    );
}

fn recipient_of(doc: &Document, name: &str) -> String {
    doc.machines()
        .into_iter()
        .find(|machine| machine.name == name)
        .expect(name)
        .recipient
}

#[test]
fn letting_a_machine_go_renews_the_join_password_so_it_is_not_let_in_again() {
    let lab = Lab::new("leave");
    let admin = lab.home("ma");
    write_private(
        &admin.home.join(".secrets/gh_access_token"),
        &synthetic("gh"),
    );
    let created = client::test_create_with_password(
        &admin,
        &lab.url,
        Some(&admin.home.join(".secrets")),
        Some("ma"),
    )
    .expect("create");
    let password = client::admin_open(&admin)
        .expect("open")
        .doc
        .join_password()
        .expect("create makes a join password");
    let pinned = format!("{}#{}", lab.url, created.signer);
    let machine = lab.home("mb");
    join_with(&machine, &pinned, "mb", Some(password.as_slice())).expect("join");
    assert_eq!(
        client::auto_approve(&admin).expect("approve").approved,
        ["mb"]
    );
    applied(client::sync(&machine, None).expect("sync"));

    let open = client::admin_open(&admin).expect("open");
    let mb = recipient_of(&open.doc, "mb");
    let done = client::admin_revoke(&admin, &open, "mb", false)
        .expect("revoke")
        .published;
    assert!(done.password_renewed && !done.kept_out && done.held.is_empty());
    assert_eq!(
        client::admin_open(&admin).expect("open").doc.code_only(),
        std::slice::from_ref(&mb),
        "the closed vault names the key it let go"
    );
    let renewed = client::admin_open(&admin)
        .expect("open")
        .doc
        .join_password()
        .expect("still a join password");
    assert_ne!(renewed.as_slice(), password.as_slice());
    let again = client::auto_approve(&admin).expect("approve");
    assert!(again.approved.is_empty(), "{:?}", again.approved);
    assert_eq!(
        client::sync(&machine, None).expect("sync"),
        SyncOutcome::NotIn
    );
    let stale = lab.home("mc");
    join_with(&stale, &pinned, "mc", Some(password.as_slice())).expect("join");
    let fresh = lab.home("md");
    join_with(&fresh, &pinned, "md", Some(renewed.as_slice())).expect("join");
    let admitted = client::auto_approve(&admin).expect("approve");
    assert_eq!(admitted.approved, ["md"]);

    set(&admin, &Audience::Everyone, "note", "kept");
    let kept = client::admin_open(&admin)
        .expect("open")
        .doc
        .join_password()
        .expect("a join password");
    assert_eq!(kept.as_slice(), renewed.as_slice());

    let open = client::admin_open(&admin).expect("open");
    assert_eq!(open.doc.code_only(), std::slice::from_ref(&mb));
    let mut doc = open.doc.clone();
    doc.set_open(true).expect("open");
    let published = client::admin_publish(&admin, &open, &doc).expect("publish");
    // The key let go is named already; opening holds the one stale request left waiting.
    let asked: Vec<&str> = published.held.iter().map(|r| r.host.as_str()).collect();
    assert_eq!(asked, ["mc"]);
    let opened = client::auto_approve(&admin).expect("approve");
    assert!(opened.approved.is_empty(), "{:?}", opened.approved);
    assert!(opened.code_only.contains(&mb));
    assert_eq!(
        client::sync(&machine, None).expect("sync"),
        SyncOutcome::NotIn
    );
}

#[test]
fn a_join_whose_subscriber_applied_the_approval_first_is_done() {
    let lab = Lab::holding("race", Duration::from_secs(1));
    let admin = lab.home("ma");
    write_private(
        &admin.home.join(".secrets/gh_access_token"),
        &synthetic("gh"),
    );
    let created = client::test_create_with_password(
        &admin,
        &lab.url,
        Some(&admin.home.join(".secrets")),
        Some("ma"),
    )
    .expect("create");
    let password = client::admin_open(&admin)
        .expect("open")
        .doc
        .join_password()
        .expect("create makes a join password");
    let machine = lab.home("mb");
    let subscriber = || {
        assert_eq!(
            client::auto_approve(&admin).expect("approve").approved,
            ["mb"]
        );
        applied(client::sync(&machine, None).expect("the subscriber's sync"));
    };
    let outcome = client::join(
        &machine,
        &JoinOptions {
            source: format!("{}#{}", lab.url, created.signer),
            host: Some("mb".into()),
            interactive: false,
            wait: Duration::from_secs(60),
            confirm: &|_| panic!("an unattended join never asks"),
            password: aterm_secrets::client::JoinPassword::Ask(&|| {
                Some(SecretBytes::from(password.as_slice()))
            }),
            fetched: None,
            requested: &subscriber,
            say: &|_| {},
        },
    )
    .expect("join");
    let JoinOutcome::Joined { seq, names } = outcome else {
        panic!("the join waited on a version its subscriber had applied: {outcome:?}");
    };
    assert_eq!(seq, 2);
    assert!(
        names.iter().any(|name| name == "gh_access_token"),
        "{names:?}"
    );
}

#[test]
fn removing_an_admin_by_any_publish_rotates_and_later_versions_carry_the_handoff() {
    let lab = Lab::new("handoff");
    let admin = lab.home("ma");
    let created = client::test_create_unbound(&admin, &lab.url, None, Some("ma")).expect("create");
    let pinned = format!("{}#{}", lab.url, created.signer);
    let second = lab.home("mb");
    let asleep = lab.home("mc");
    join_unattended(&second, &pinned, "mb").expect("join");
    join_unattended(&asleep, &pinned, "mc").expect("join");
    approve_all(&admin);
    applied(client::sync(&second, None).expect("sync"));
    applied(client::sync(&asleep, None).expect("sync"));
    {
        let vault = client::admin_open(&admin).expect("open");
        let mut doc = vault.doc.clone();
        doc.add_admin("mb").expect("add admin");
        assert!(
            !client::admin_publish(&admin, &vault, &doc)
                .expect("publish")
                .rotated
        );
    }
    applied(client::sync(&asleep, None).expect("sync"));

    {
        let vault = client::admin_open(&admin).expect("open");
        let mut doc = vault.doc.clone();
        assert!(doc.remove_admin("mb").expect("remove admin"));
        assert!(
            client::admin_publish(&admin, &vault, &doc)
                .expect("publish")
                .rotated
        );
    }
    set(&admin, &Audience::Everyone, "one", &synthetic("one"));
    let last = synthetic("two");
    set(&admin, &Audience::Everyone, "two", &last);
    applied(client::sync(&asleep, None).expect("sync"));
    assert_eq!(read(&asleep, "two").as_deref(), Some(last.as_str()));
    assert_eq!(
        client::config(&asleep)
            .expect("config")
            .expect("configured")
            .signer,
        client::config(&admin)
            .expect("config")
            .expect("configured")
            .signer,
        "the sleeper re-pinned through the carried handoff"
    );
}

fn pinned_signer(paths: &Paths) -> Option<[u8; 32]> {
    client::config(paths)
        .expect("config")
        .expect("configured")
        .signer
}

#[test]
fn every_version_carries_the_whole_chain_so_the_first_key_still_admits() {
    let lab = Lab::new("chain");
    let admin = lab.home("ma");
    let created = client::create(&admin, &lab.url, None, Some("ma")).expect("create");
    let token = synthetic("token");
    set(&admin, &Audience::Everyone, "token", &token);
    let first = format!("{}#{}", lab.url, created.signer);
    let asleep = lab.home("mz");
    join_unattended(&asleep, &first, "mz").expect("join");
    approve_all(&admin);
    applied(client::sync(&asleep, None).expect("sync"));

    for rotations in 1..=3 {
        let vault = client::admin_open(&admin).expect("open");
        let doc = vault.doc.clone();
        let done = client::admin_rotate(&admin, &vault, &doc).expect("rotate");
        assert!(done.rotated);
        assert_eq!(done.first_long_chain, rotations == 2, "{rotations}");
        if rotations == 1 {
            continue;
        }
        let host = format!("m{rotations}");
        let machine = lab.home(&host);
        assert!(matches!(
            join_unattended(&machine, &first, &host)
                .expect("the vault verifies under its first key"),
            JoinOutcome::Pending { .. }
        ));
        assert_eq!(
            pinned_signer(&machine),
            pinned_signer(&admin),
            "it pinned the key that signs now"
        );
        approve_all(&admin);
        applied(client::sync(&machine, None).expect("sync"));
        assert_eq!(read(&machine, "token").as_deref(), Some(token.as_str()));
    }
    assert_ne!(pinned_signer(&asleep), pinned_signer(&admin));
    applied(client::sync(&asleep, None).expect("sync"));
    assert_eq!(pinned_signer(&asleep), pinned_signer(&admin));
    let followed = client::config(&asleep)
        .expect("config")
        .expect("configured")
        .followed;
    assert_eq!(followed.len(), 3);
    assert_eq!(
        aterm_secrets::b64::encode_padded(&followed[0]),
        created.signer
    );
}

#[test]
fn an_admin_puts_back_a_vault_its_server_lost() {
    let mut lab = Lab::new("lost");
    let admin = lab.home("ma");
    let created = client::create(&admin, &lab.url, None, Some("ma")).expect("create");
    let machine = lab.home("mb");
    join_unattended(&machine, &format!("{}#{}", lab.url, created.signer), "mb").expect("join");
    approve_all(&admin);
    let token = synthetic("kept");
    set(&admin, &Audience::Everyone, "kept", &token);
    applied(client::sync(&machine, None).expect("sync"));

    lab.restart_empty();
    assert!(
        client::create(&lab.home("other"), &lab.url, None, Some("other")).is_ok(),
        "an empty server takes a new vault under a name it does not hold"
    );
    lab.restart_empty();
    let reopened = client::admin_open(&admin).expect("the admin reopens and restores");
    assert_eq!(reopened.seq(), 3);
    assert_eq!(
        client::sync(&machine, None).expect("sync"),
        SyncOutcome::Unchanged { seq: 3 }
    );
    let next = synthetic("next");
    set(&admin, &Audience::Everyone, "next", &next);
    applied(client::sync(&machine, None).expect("sync"));
    assert_eq!(read(&machine, "next").as_deref(), Some(next.as_str()));
}

#[test]
fn a_probe_names_a_vault_only_under_its_signer() {
    let lab = Lab::new("probed");
    let mut pin = [0u8; 32];
    assert_eq!(
        client::probe(&lab.url, &pin, HANG_DETECTOR).expect("answered"),
        ProbeOutcome::Absent,
        "an empty server holds no vault"
    );
    let admin = lab.home("ma");
    let created = client::create(&admin, &lab.url, None, Some("ma")).expect("create");
    let signer = aterm_secrets::b64::decode_padded(&created.signer).expect("base64");
    pin.copy_from_slice(&signer);
    let ProbeOutcome::Found(probed) =
        client::probe(&lab.url, &pin, HANG_DETECTOR).expect("answered")
    else {
        panic!("a vault");
    };
    assert_eq!(probed.seq, 1);
    assert_eq!(probed.fingerprint, created.fingerprint);
    let other = client::probe(&lab.url, &[7u8; 32], HANG_DETECTOR);
    assert!(
        matches!(other, Ok(ProbeOutcome::AnotherVault(_))),
        "a vault under another signer: {other:?}"
    );
    assert!(client::probe("http://127.0.0.1:9/v1/probed", &pin, HANG_DETECTOR).is_err());
}

/// Only an envelope of the asked-for name whose signature holds is another vault: the same
/// bytes under another name, or with a signature that fails, are no vault at all.
#[test]
fn another_vault_is_a_signed_envelope_of_the_asked_name() {
    let lab = Lab::new("alab");
    let admin = lab.home("ma");
    let created = client::create(&admin, &lab.url, None, Some("ma")).expect("create");
    let signer = aterm_secrets::b64::decode_padded(&created.signer).expect("base64");
    let pin: [u8; 32] = signer.as_slice().try_into().expect("32 bytes");
    let ProbeOutcome::Found(probed) =
        client::probe(&lab.url, &pin, HANG_DETECTOR).expect("answered")
    else {
        panic!("a vault");
    };
    let served = |body: Vec<u8>| -> &'static [u8] {
        let mut reply =
            format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", body.len()).into_bytes();
        reply.extend_from_slice(&body);
        Box::leak(reply.into_boxed_slice())
    };
    let ask = |reply: &'static [u8]| {
        let (url, heard) = answer_once(reply);
        let answered = client::probe(&url, &[7u8; 32], HANG_DETECTOR);
        heard
            .recv_timeout(HANG_DETECTOR)
            .expect("the probe reached the stub");
        answered
    };
    let answered = ask(served(probed.envelope.clone()));
    assert!(
        matches!(answered, Ok(ProbeOutcome::AnotherVault(_))),
        "{answered:?}"
    );
    // One bit of its signature changed: it still parses, and no longer verifies.
    let text = String::from_utf8(probed.envelope.clone()).expect("text");
    let (head, line) = text.trim_end().rsplit_once('\n').expect("a signature line");
    let encoded = line.strip_prefix("signature: ").expect("signature last");
    let mut signature = aterm_secrets::b64::decode_padded(encoded).expect("base64");
    signature[0] ^= 1;
    let forged = format!(
        "{head}\nsignature: {}\n",
        aterm_secrets::b64::encode_padded(&signature)
    );
    aterm_secrets::envelope::parse(forged.as_bytes()).expect("a forged envelope parses");
    let answered = ask(served(forged.into_bytes()));
    assert!(
        matches!(answered, Ok(ProbeOutcome::NotThisVault(_))),
        "{answered:?}"
    );
    // A vault of another name served at this address is no answer about the one asked for.
    let elsewhere = Lab::new("elsewhere");
    let made =
        client::create(&elsewhere.home("mb"), &elsewhere.url, None, Some("mb")).expect("create");
    let signer = aterm_secrets::b64::decode_padded(&made.signer).expect("base64");
    let its_pin: [u8; 32] = signer.as_slice().try_into().expect("32 bytes");
    let ProbeOutcome::Found(named) =
        client::probe(&elsewhere.url, &its_pin, HANG_DETECTOR).expect("answered")
    else {
        panic!("a vault");
    };
    let answered = ask(served(named.envelope));
    assert!(
        matches!(answered, Ok(ProbeOutcome::NotThisVault(_))),
        "{answered:?}"
    );
}

fn new_key() -> String {
    Identity::generate()
        .expect("random")
        .recipient()
        .to_bech32()
}

fn held(paths: &Paths) -> u64 {
    match client::status(paths).expect("status").membership {
        Membership::In { seq, .. } => seq,
        other => panic!("the machine is in: {other:?}"),
    }
}

#[test]
fn removing_an_admin_renews_the_join_password_it_read() {
    let lab = Lab::new("demote");
    let admin = lab.home("ma");
    let created = client::test_create_unbound(&admin, &lab.url, None, Some("ma")).expect("create");
    let password = client::admin_open(&admin)
        .expect("open")
        .doc
        .join_password()
        .expect("create makes a join password");
    let pinned = format!("{}#{}", lab.url, created.signer);
    let second = lab.home("mb");
    join_with(&second, &pinned, "mb", Some(password.as_slice())).expect("join");
    assert_eq!(
        client::auto_approve(&admin).expect("approve").approved,
        ["mb"]
    );
    {
        let vault = client::admin_open(&admin).expect("open");
        let mut doc = vault.doc.clone();
        doc.add_admin("mb").expect("add admin");
        let done = client::admin_publish(&admin, &vault, &doc).expect("publish");
        assert!(!done.password_renewed, "an admin added leaves it be");
    }
    applied(client::sync(&second, None).expect("sync"));
    let known = client::admin_open(&second)
        .expect("mb opens the admin locker")
        .doc
        .join_password()
        .expect("mb reads the password");

    let vault = client::admin_open(&admin).expect("open");
    let mut doc = vault.doc.clone();
    assert!(doc.remove_admin("mb").expect("remove admin"));
    let done = client::admin_rotate(&admin, &vault, &doc).expect("rotate");
    assert!(done.password_renewed, "the admin removed read the password");
    let renewed = client::admin_open(&admin)
        .expect("open")
        .doc
        .join_password()
        .expect("still a join password");
    assert_ne!(renewed.as_slice(), known.as_slice());
    applied(client::sync(&second, None).expect("mb is still a machine"));
    let minted = lab.home("mb-minted");
    join_with(&minted, &pinned, "mbx", Some(known.as_slice())).expect("join");
    let refused = client::auto_approve(&admin).expect("approve");
    assert!(refused.approved.is_empty(), "{:?}", refused.approved);
    assert_eq!(refused.refused.len(), 1);

    let vault = client::admin_open(&admin).expect("open");
    let doc = vault.doc.clone();
    let done = client::admin_rotate(&admin, &vault, &doc).expect("rotate");
    assert!(done.rotated);
    assert!(!done.password_renewed);
    let kept = client::admin_open(&admin)
        .expect("open")
        .doc
        .join_password()
        .expect("a join password");
    assert_eq!(kept.as_slice(), renewed.as_slice());
}

#[test]
fn the_approver_judges_a_corrected_request_at_once_and_rests_once_all_are_judged() {
    let lab = Lab::changing("settle");
    let admin = lab.home("ma");
    let created =
        client::test_create_with_password(&admin, &lab.url, None, Some("ma")).expect("create");
    let password = client::admin_open(&admin)
        .expect("open")
        .doc
        .join_password()
        .expect("create makes a join password");
    let pinned = format!("{}#{}", lab.url, created.signer);
    let listed = || client::requests(&admin, None).expect("requests");
    let mut settled = client::Settled::default();

    let machine = lab.home("mb");
    assert!(matches!(
        join_with(
            &machine,
            &pinned,
            "mb",
            Some(&b"a-mistyped-join-password"[..])
        )
        .expect("join"),
        JoinOutcome::Pending { .. }
    ));
    let source = client::config(&admin).expect("config").expect("configured");
    assert!(settled.due(&source, held(&admin), &listed(), Approval::Password));
    let refused = client::auto_approve(&admin).expect("approve");
    assert!(refused.approved.is_empty());
    assert_eq!(refused.judged.len(), 1);
    settled.settle(refused.judged);
    assert!(
        !settled.due(&source, held(&admin), &listed(), Approval::Password),
        "a refused proof is not judged again"
    );

    join_with(&machine, &pinned, "mb", Some(password.as_slice())).expect("join");
    assert_eq!(held(&admin), 1, "nothing was published since");
    assert!(
        settled.due(&source, held(&admin), &listed(), Approval::Password),
        "the corrected request is judged"
    );
    let approved = client::auto_approve(&admin).expect("approve");
    assert_eq!(approved.approved, ["mb"]);
    assert_eq!(approved.seq, Some(2));
    settled.settle(approved.judged);
    applied(client::sync(&machine, None).expect("sync"));

    assert!(listed().iter().any(|request| request.host == "mb"));
    assert!(
        settled.due(&source, held(&admin), &listed(), Approval::Password),
        "a new version is judged afresh"
    );
    let member = client::auto_approve(&admin).expect("approve");
    assert!(member.approved.is_empty() && member.refused.is_empty());
    assert_eq!(member.seq, None);
    settled.settle(member.judged);
    assert!(
        !settled.due(&source, held(&admin), &listed(), Approval::Password),
        "nothing is left to judge"
    );

    let other = Lab::new("other");
    let other_admin = other.home("other-admin");
    let created =
        client::test_create_with_password(&other_admin, &other.url, None, Some("other-admin"))
            .expect("create other vault");
    let opened = client::admin_open(&other_admin).expect("open other vault");
    let mut doc = opened.doc.clone();
    doc.set_open(true).expect("open admission");
    doc.set_entry(&Audience::Everyone, "token", b"inert-other")
        .expect("other token");
    client::admin_publish(&other_admin, &opened, &doc).expect("publish other seq2");
    let other_source = client::config(&other_admin)
        .expect("config")
        .expect("configured");
    assert_eq!(held(&other_admin), held(&admin));
    let request = listed().pop().expect("held member request");
    let allowed = allow_through(&other_admin, &machine, "mb").allowed;
    let line = format!(
        "{} {} {} allowed={} proof={}",
        request.recipient,
        request.host,
        request.code,
        allowed.to_text(),
        request.proof.as_deref().expect("its original proof")
    );
    transport::post_join(&other.url, &line).expect("same request to other open vault");
    let waiting = client::requests(&other_admin, None).expect("other queue");
    assert_eq!(waiting.len(), 1);
    assert_eq!(waiting[0].recipient, request.recipient);
    assert_eq!(waiting[0].host, request.host);
    assert_eq!(waiting[0].proof, request.proof);
    assert!(!settled.due(&source, held(&admin), &listed(), Approval::Open));
    assert!(settled.due(&other_source, held(&other_admin), &waiting, Approval::Open));
    let admitted = client::auto_approve(&other_admin).expect("other approves independently");
    assert_eq!(admitted.approved, ["mb"]);
    settled.settle(admitted.judged);
    assert!(!settled.due(&other_source, 2, &waiting, Approval::Open));
    join_unattended(&machine, &format!("{}#{}", other.url, created.signer), "mb")
        .expect("same machine joins other vault");
    assert_eq!(read(&machine, "token").as_deref(), Some("inert-other"));
}

#[test]
fn a_taken_name_of_62_or_63_characters_still_gets_a_free_one() {
    let cases = [
        ("b".repeat(62), Vec::new(), format!("{}-2", "b".repeat(61))),
        (
            format!("{}-bc", "a".repeat(60)),
            vec![format!("{}-2", "a".repeat(60))],
            format!("{}-3", "a".repeat(60)),
        ),
    ];
    for (wanted, also_taken, expected) in cases {
        let (sent, got) = std::sync::mpsc::channel();
        let asked = wanted.clone();
        std::thread::spawn(move || {
            let mut doc = Document::new_vault(&asked, &new_key());
            for name in also_taken {
                doc.add_machine(&name, &new_key()).expect("add");
            }
            let _ = sent.send(client::free_name(&doc, &asked));
        });
        let name = got
            .recv_timeout(Duration::from_secs(60))
            .expect("free_name returns");
        assert_eq!(name, expected, "for {wanted}");
        assert!(aterm_secrets::text::valid_machine_name(&name));
    }
}

#[test]
fn an_edit_saved_over_a_machine_admitted_meanwhile_keeps_it() {
    let (ma, mb, mc, md) = (new_key(), new_key(), new_key(), new_key());
    let mut base = Document::new_vault("ma", &ma);
    base.add_machine("mb", &mb).expect("add");
    let mut current = base.clone();
    current.add_machine("mc", &mc).expect("add");
    let mut edited = base.clone();
    assert!(edited.remove_machine("mb", false).expect("remove"));
    edited.add_machine("mc", &md).expect("add");
    let added: Vec<String> = client::admitted_since(&base, &current)
        .into_iter()
        .map(|machine| machine.name)
        .collect();
    assert_eq!(added, ["mc"]);
    let kept = client::keep_admitted(&base, &current, &mut edited).expect("merge");
    assert_eq!(kept, ["mc-2"]);
    let machines: Vec<(String, String)> = edited
        .machines()
        .into_iter()
        .map(|machine| (machine.name, machine.recipient))
        .collect();
    assert_eq!(
        machines,
        [
            ("ma".to_string(), ma),
            ("mc".to_string(), md),
            ("mc-2".to_string(), mc)
        ],
        "mb stays deleted"
    );
    assert!(
        client::keep_admitted(&base, &current, &mut edited)
            .expect("merge")
            .is_empty(),
        "a text that names the key already keeps it"
    );
}

#[test]
fn an_edit_saved_over_an_open_vault_that_let_a_machine_go_keeps_it_out() {
    let (ma, mb, mc) = (new_key(), new_key(), new_key());
    let mut base = Document::new_vault("ma", &ma);
    base.add_machine("mb", &mb).expect("add");
    base.add_machine("mc", &mc).expect("add");
    base.set_open(true).expect("open");
    let mut current = base.clone();
    for (name, key) in [("mb", &mb), ("mc", &mc)] {
        assert!(current.remove_machine(name, false).expect("remove"));
        current.add_code_only(key).expect("code-only");
    }
    let mut edited = base.clone();
    assert!(edited.remove_machine("mb", false).expect("remove"));
    client::keep_admitted(&base, &current, &mut edited).expect("merge");
    assert_eq!(
        edited.code_only(),
        [mb, mc.clone()],
        "both lines are kept: mc, which the text left as it began, is let go too"
    );
    assert!(
        !edited
            .machines()
            .iter()
            .any(|machine| machine.recipient == mc)
    );
}

#[test]
fn an_edit_saved_over_another_admins_changes_to_who_is_in_keeps_them() {
    let (ma, mb, mc, md) = (new_key(), new_key(), new_key(), new_key());
    let mut base = Document::new_vault("ma", &ma);
    for (name, key) in [("mb", &mb), ("mc", &mc), ("md", &md)] {
        base.add_machine(name, key).expect("add");
    }
    base.add_admin("mb").expect("admin");
    base.set_entry(&Audience::Hosts(vec!["mc".into()]), "own", b"synthetic-own")
        .expect("mc's own section");
    base.set_join_password(Some(b"synthetic-join-password-one"))
        .expect("password");
    let mut current = base.clone();
    assert!(current.remove_admin("mb").expect("demote"));
    assert!(current.remove_machine("mc", true).expect("revoke"));
    current.add_admin("md").expect("admin");
    current
        .set_join_password(Some(b"synthetic-join-password-two"))
        .expect("renew");
    let mut edited = base.clone();
    edited
        .set_entry(&Audience::Everyone, "note", b"synthetic-note")
        .expect("set");
    assert!(
        client::keep_admitted(&base, &current, &mut edited)
            .expect("merge")
            .is_empty()
    );
    let names: Vec<String> = edited.machines().into_iter().map(|m| m.name).collect();
    assert_eq!(names, ["ma", "mb", "md"]);
    assert_eq!(edited.admins(), ["ma", "md"]);
    assert!(
        aterm_secrets::text::merge_for("mc", edited.entries())
            .expect("merges")
            .iter()
            .all(|entry| entry.name != "own")
    );
    assert!(edited.entry(&Audience::Everyone, "note").is_some());
    let password = |doc: &Document| doc.join_password().map(|p| p.as_slice().to_vec());
    assert_eq!(password(&edited), password(&current));
    let mut edited = base.clone();
    edited
        .set_join_password(Some(b"synthetic-join-password-mine"))
        .expect("theirs");
    assert!(edited.remove_machine("md", false).expect("remove"));
    client::keep_admitted(&base, &current, &mut edited).expect("merge");
    assert_eq!(
        password(&edited),
        Some(b"synthetic-join-password-mine".to_vec())
    );
    assert_eq!(edited.admins(), ["ma"]);
    let rekeyed = new_key();
    let spelled = |doc: &Document, key: &str| {
        let text = String::from_utf8(doc.render().as_slice().to_vec()).expect("utf-8");
        Document::parse(&text.replace(&format!("mc = {mc}"), &format!("mc = {key}")))
            .expect("valid")
    };
    let current = spelled(&base, &rekeyed);
    let mut edited = base.clone();
    let error = client::keep_admitted(&base, &current, &mut edited).expect_err("refused");
    assert!(error.0.contains(&rekeyed), "{error}");
    let mut edited = spelled(&base, &rekeyed);
    assert!(
        client::keep_admitted(&base, &current, &mut edited)
            .expect("the text gives mc the new key")
            .is_empty()
    );
    assert!(
        aterm_secrets::text::merge_for("mc", edited.entries())
            .expect("merges")
            .iter()
            .any(|entry| entry.name == "own")
    );
}

#[test]
fn an_edit_that_lands_after_a_revocation_keeps_the_machine_out() {
    let lab = Lab::new("revoked");
    let admin = lab.home("ma");
    let created =
        client::test_create_with_password(&admin, &lab.url, None, Some("ma")).expect("create");
    let password = client::admin_open(&admin)
        .expect("open")
        .doc
        .join_password()
        .expect("create makes a join password");
    let machine = lab.home("mb");
    let pinned = format!("{}#{}", lab.url, created.signer);
    join_with(&machine, &pinned, "mb", Some(password.as_slice())).expect("join");
    assert_eq!(
        client::auto_approve(&admin).expect("approve").approved,
        ["mb"]
    );
    applied(client::sync(&machine, None).expect("mb is in"));
    let began = client::admin_open(&admin).expect("open");
    let base = began.doc.clone();
    {
        let other = client::admin_open(&admin).expect("open");
        let done = client::admin_revoke(&admin, &other, "mb", false)
            .expect("revoke")
            .published;
        assert!(done.password_renewed);
    }
    let mut edited = base.clone();
    edited
        .set_entry(&Audience::Everyone, "note", b"synthetic-note")
        .expect("set");
    assert!(
        client::admin_publish(&admin, &began, &edited).is_err(),
        "the save lands on a vault that moved"
    );
    let fresh = client::admin_open(&admin).expect("reopen");
    let renewed = fresh.doc.join_password().expect("renewed");
    client::keep_admitted(&base, &fresh.doc, &mut edited).expect("merge");
    client::admin_publish(&admin, &fresh, &edited).expect("publish");
    let after = client::admin_open(&admin).expect("open").doc.clone();
    assert!(after.machines().iter().all(|machine| machine.name != "mb"));
    assert_eq!(
        after.join_password().map(|p| p.as_slice().to_vec()),
        Some(renewed.as_slice().to_vec()),
        "not the password mb joined with"
    );
    assert!(after.entry(&Audience::Everyone, "note").is_some());
    assert_eq!(
        client::sync(&machine, None).expect("sync"),
        SyncOutcome::NotIn
    );
    assert!(
        client::auto_approve(&admin)
            .expect("approve")
            .approved
            .is_empty()
    );
}

#[test]
fn an_edit_that_lands_after_the_approver_keeps_the_machine_and_the_password() {
    let lab = Lab::new("edit");
    let admin = lab.home("ma");
    let created =
        client::test_create_with_password(&admin, &lab.url, None, Some("ma")).expect("create");
    let began = client::admin_open(&admin).expect("open");
    let base = began.doc.clone();
    let password = base.join_password().expect("create makes a join password");
    let machine = lab.home("mb");
    let pinned = format!("{}#{}", lab.url, created.signer);
    join_with(&machine, &pinned, "mb", Some(password.as_slice())).expect("join");
    assert_eq!(
        client::auto_approve(&admin).expect("approve").approved,
        ["mb"]
    );
    let mut edited = base.clone();
    edited
        .set_entry(&Audience::Everyone, "note", b"synthetic-note")
        .expect("set");
    assert!(
        client::admin_publish(&admin, &began, &edited).is_err(),
        "the save lands on a vault that moved"
    );
    let fresh = client::admin_open(&admin).expect("reopen");
    let kept = client::keep_admitted(&base, &fresh.doc, &mut edited).expect("merge");
    assert_eq!(kept, ["mb"]);
    let done = client::admin_publish(&admin, &fresh, &edited).expect("publish");
    assert!(!done.password_renewed, "nobody was let go");
    applied(client::sync(&machine, None).expect("mb is still in"));
    assert_eq!(read(&machine, "note").as_deref(), Some("synthetic-note"));
}

#[test]
fn a_revoked_machine_that_joins_again_waits_and_asks_again() {
    let lab = Lab::new("again");
    let admin = lab.home("ma");
    let created = client::create(&admin, &lab.url, None, Some("ma")).expect("create");
    let pinned = format!("{}#{}", lab.url, created.signer);
    let machine = lab.home("mb");
    join_unattended(&machine, &pinned, "mb").expect("join");
    approve_all(&admin);
    applied(client::sync(&machine, None).expect("sync"));
    {
        let vault = client::admin_open(&admin).expect("open");
        client::admin_revoke(&admin, &vault, "mb", false).expect("revoke");
    }
    assert_eq!(
        client::sync(&machine, None).expect("sync"),
        SyncOutcome::NotIn
    );
    assert!(matches!(
        client::status(&machine).expect("status").membership,
        Membership::Revoked { .. }
    ));

    assert!(matches!(
        join_unattended(&machine, &pinned, "mb").expect("join"),
        JoinOutcome::Pending { .. }
    ));
    assert_eq!(
        client::status(&machine).expect("status").membership,
        Membership::Pending
    );
    approve_all(&admin);
    applied(client::sync(&machine, None).expect("let back in"));
}

#[test]
fn a_join_beside_another_vaults_leftover_state_starts_afresh() {
    let first = Lab::new("first");
    let admin = first.home("ma");
    let created = client::create(&admin, &first.url, None, Some("ma")).expect("create");
    let machine = first.home("mb");
    join_unattended(&machine, &format!("{}#{}", first.url, created.signer), "mb").expect("join");
    approve_all(&admin);
    for n in 0..3 {
        set(
            &admin,
            &Audience::Everyone,
            &format!("n{n}"),
            &synthetic("n"),
        );
    }
    applied(client::sync(&machine, None).expect("sync"));
    assert_eq!(held(&machine), 5);
    std::fs::remove_file(machine.dir.join("source.toml")).expect("remove source.toml");

    let second = Lab::new("second");
    let other = second.home("mz");
    let theirs = client::create(&other, &second.url, None, Some("mz")).expect("create");
    let outcome = join_unattended(&machine, &format!("{}#{}", second.url, theirs.signer), "mb")
        .expect("no rollback across vaults");
    assert!(matches!(outcome, JoinOutcome::Pending { .. }));
    assert_eq!(
        client::status(&machine).expect("status").membership,
        Membership::Pending
    );
    assert_eq!(approve_all(&other), 2);
    applied(client::sync(&machine, None).expect("sync"));
    assert_eq!(held(&machine), 2);
}

#[test]
fn a_join_with_the_password_says_how_to_fix_a_mistyped_one_before_it_waits() {
    let lab = Lab::holding("hint", Duration::from_secs(1));
    let admin = lab.home("ma");
    let created = client::create(&admin, &lab.url, None, Some("ma")).expect("create");
    let pinned = format!("{}#{}", lab.url, created.signer);
    for (host, interactive) in [("mb", true), ("mc", false)] {
        let said = std::cell::RefCell::new(Vec::<String>::new());
        let outcome = client::join(
            &lab.home(host),
            &JoinOptions {
                source: pinned.clone(),
                host: Some(host.into()),
                interactive,
                wait: Duration::from_secs(1),
                confirm: &|_| panic!("a pinned source is not confirmed"),
                password: aterm_secrets::client::JoinPassword::Ask(&|| {
                    Some(SecretBytes::from(&b"a-mistyped-join-password"[..]))
                }),
                fetched: None,
                requested: &|| {},
                say: &|line| said.borrow_mut().push(line.to_string()),
            },
        )
        .expect("join");
        assert!(matches!(outcome, JoinOutcome::Pending { .. }));
        let said = said.into_inner();
        let at = |words: &str| said.iter().position(|line| line.contains(words));
        let (Some(hint), Some(waiting)) = (at("may be mistyped"), at("waiting for the approval"))
        else {
            panic!("{said:?}");
        };
        assert!(hint < waiting, "{said:?}");
        let line = &said[hint];
        assert!(
            line.contains(&format!("aterm secrets join <vault-url> --name {host}"))
                && line.contains("full configured URL"),
            "{line}"
        );
        assert!(
            !line.contains(&pinned),
            "the configured vault URL stays private"
        );
        assert!(
            !line.contains(&created.signer),
            "the signer pin stays private"
        );
        assert_eq!(line.contains("--password-stdin"), !interactive, "{line}");
    }
}

#[test]
fn a_machine_admitted_under_another_name_is_named_by_its_code() {
    let lab = Lab::new("names");
    let admin = lab.home("ma");
    let created =
        client::test_create_with_password(&admin, &lab.url, None, Some("ma")).expect("create");
    let pinned = format!("{}#{}", lab.url, created.signer);
    let password = client::admin_open(&admin)
        .expect("open")
        .doc
        .join_password()
        .expect("create makes a join password");
    let first = lab.home("mb");
    join_with(&first, &pinned, "mb", Some(password.as_slice())).expect("join");
    assert_eq!(
        client::auto_approve(&admin).expect("approve").approved,
        ["mb"]
    );
    let second = lab.home("mb-again");
    let JoinOutcome::Pending { code, host } =
        join_with(&second, &pinned, "mb", Some(password.as_slice())).expect("join")
    else {
        panic!("a machine not in the vault waits for approval");
    };
    assert_eq!(host, "mb");
    assert_eq!(
        client::auto_approve(&admin).expect("approve").approved,
        ["mb-2"]
    );
    applied(client::sync(&second, None).expect("sync"));

    let refused = client::admin_open(&second)
        .expect_err("not an admin")
        .to_string();
    assert!(
        refused.contains(&format!("aterm secrets admins add {code}")),
        "{refused}"
    );
    assert!(!refused.ends_with("admins add mb"), "{refused}");

    let vault = client::admin_open(&admin).expect("open");
    let named = client::machine_named(&vault.doc, &code.to_uppercase()).expect("its code");
    assert_eq!(named, "mb-2");
    let mut doc = vault.doc.clone();
    doc.add_admin(&named).expect("admin");
    client::admin_publish(&admin, &vault, &doc).expect("publish");
    applied(client::sync(&second, None).expect("sync"));
    client::admin_open(&second).expect("an admin now");
    assert!(
        client::admin_open(&first).is_err(),
        "the machine that holds the name it asked for is not"
    );

    let shaped = "aaaa-bbbb-cccc-dddd";
    assert!(client::machine_named(&doc, shaped).is_err());
    doc.add_machine(shaped, &new_key())
        .expect("a valid machine name");
    assert_eq!(client::machine_named(&doc, "mb").expect("a name"), "mb");
    assert!(client::machine_named(&doc, shaped).is_err());
}

#[test]
fn a_machine_asking_for_another_machines_code_as_its_name_is_let_in_under_another() {
    let lab = Lab::new("squat");
    let admin = lab.home("ma");
    let created = client::create(&admin, &lab.url, None, Some("ma")).expect("create");
    let pinned = format!("{}#{}", lab.url, created.signer);
    let opened = client::admin_open(&admin).expect("open");
    let mut doc = opened.doc.clone();
    doc.set_open(true).expect("open");
    client::admin_publish(&admin, &opened, &doc).expect("publish");

    let mac = lab.home("mb");
    let JoinOutcome::Pending { code, .. } = join_unattended(&mac, &pinned, "mb").expect("join")
    else {
        panic!("a machine not in the vault waits for approval");
    };
    let squatter = lab.home("mx");
    join_unattended(&squatter, &pinned, &code).expect("any machine name may be asked for");
    for (joiner, host) in [(&mac, "mb"), (&squatter, code.as_str())] {
        let bundle = allow_through(&admin, joiner, host);
        client::post_allowed(&admin, &bundle.allowed).expect("posted");
    }
    assert_eq!(approved(&admin), ["machine", "mb"]);
    let doc = client::admin_open(&admin).expect("open").doc.clone();
    assert!(doc.machines().iter().all(|machine| machine.name != code));
    assert_eq!(client::machine_named(&doc, &code).expect("its code"), "mb");
    assert_eq!(
        client::free_name(&doc, &code),
        "machine-2",
        "a code is never a new machine's name"
    );
}

#[test]
fn an_edit_at_a_path_that_lands_after_another_admin_keeps_the_machine_it_admitted() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let shared: PathBuf = root.path().join("shared").join("team.vault");
    std::fs::create_dir_all(shared.parent().expect("parent")).expect("dir");
    let admin = Paths::for_home(&root.path().join("ma"));
    let created =
        client::create(&admin, shared.to_str().expect("utf-8"), None, Some("ma")).expect("create");
    let began = client::admin_open(&admin).expect("open");
    let base = began.doc.clone();
    let machine = Paths::for_home(&root.path().join("mc"));
    let source = format!("{}#{}", shared.display(), created.signer);
    join_unattended(&machine, &source, "mc").expect("join");
    let recipient = client::status(&machine)
        .expect("status")
        .machine
        .expect("a key")
        .0
        .to_bech32();
    {
        let other = client::admin_open(&admin).expect("open");
        let mut doc = other.doc.clone();
        doc.add_machine("mc", &recipient).expect("add");
        client::admin_publish(&admin, &other, &doc).expect("approve");
    }
    applied(client::sync(&machine, None).expect("mc is in"));
    let mut edited = base.clone();
    edited
        .set_entry(&Audience::Everyone, "note", b"synthetic-note")
        .expect("set");
    assert!(
        client::admin_publish(&admin, &began, &edited).is_err(),
        "the save lands on a vault that moved"
    );
    let fresh = client::admin_open(&admin).expect("reopen");
    assert_eq!(fresh.seq(), began.seq() + 1);
    let kept = client::keep_admitted(&base, &fresh.doc, &mut edited).expect("merge");
    assert_eq!(kept, ["mc"]);
    let done = client::admin_publish(&admin, &fresh, &edited).expect("publish");
    assert!(
        !done.password_renewed && !done.kept_out && done.held.is_empty(),
        "nobody was let go"
    );
    applied(client::sync(&machine, None).expect("mc is still in"));
    assert_eq!(read(&machine, "note").as_deref(), Some("synthetic-note"));
}

const HANG_DETECTOR: Duration = Duration::from_secs(60);

fn answer_once(reply: &'static [u8]) -> (String, std::sync::mpsc::Receiver<String>) {
    use std::io::{Read as _, Write as _};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let url = format!("http://{}/v1/alab", listener.local_addr().expect("addr"));
    let (heard, head) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        stream
            .set_read_timeout(Some(HANG_DETECTOR))
            .expect("timeout");
        let mut head = Vec::new();
        let mut byte = [0u8; 1];
        while !head.ends_with(b"\r\n\r\n") && stream.read(&mut byte).expect("read") == 1 {
            head.push(byte[0]);
        }
        stream.write_all(reply).expect("reply");
        drop(stream);
        let _ = heard.send(String::from_utf8_lossy(&head).into_owned());
    });
    (url, head)
}

#[test]
fn a_probe_answered_by_something_else_is_not_the_vault() {
    let probed = |reply: &'static [u8]| {
        let (url, heard) = answer_once(reply);
        let answered = client::probe(&url, &[7u8; 32], HANG_DETECTOR);
        let head = heard
            .recv_timeout(HANG_DETECTOR)
            .expect("the probe reached the stub");
        assert!(head.starts_with("GET /v1/alab HTTP/1.1\r\n"), "{head}");
        answered
    };
    for reply in [
        &b"HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: 35\r\n\r\n<html><body>It works!</body></html>"[..],
        b"HTTP/1.1 301 Moved Permanently\r\nLocation: https://alab-secrets.local/v1/alab\r\nContent-Length: 0\r\n\r\n",
        b"HTTP/1.1 302 Found\r\nLocation: /login\r\nContent-Length: 0\r\n\r\n",
        b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\n\r\n",
        b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n",
        b"SSH-2.0-synthetic\r\n",
        b"HTTP/1.1 200 OK\r\nContent-Length: 999999999\r\n\r\n",
    ] {
        let answered = probed(reply);
        assert!(
            matches!(answered, Ok(ProbeOutcome::NotThisVault(_))),
            "{:?}: {answered:?}",
            String::from_utf8_lossy(reply)
        );
    }
    for reply in [
        &b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\n\r\n"[..],
        b"",
    ] {
        let answered = probed(reply);
        assert!(
            answered.is_err(),
            "{:?}: {answered:?}",
            String::from_utf8_lossy(reply)
        );
    }
}

/// Across `DEFAULT_VAULT_URLS` and `_alab-vault._tcp` browse candidates, the highest verified
/// `seq` wins while a forged higher `seq` (a vault under another signer, or a tampered `seq`
/// header with an invalid signature) is ignored; negative control: accepting unverified parsed
/// envelopes picks the forged higher `seq`.
#[test]
fn default_urls_and_browse_pick_the_highest_verified_seq_and_ignore_a_forged_higher_seq() {
    use aterm_secrets::discover::{DiscoveredVault, candidate_urls};
    use aterm_secrets::server::VaultTxt;

    let frozen = Lab::new("alab");
    let admin = frozen.home("ma");
    let created = client::create(&admin, &frozen.url, None, Some("ma")).expect("create");
    let signer = aterm_secrets::b64::decode_padded(&created.signer).expect("base64");
    let pin: [u8; 32] = signer.as_slice().try_into().expect("32 bytes");

    let ProbeOutcome::Found(seq1) = client::probe(&frozen.url, &pin, HANG_DETECTOR).expect("seq1")
    else {
        panic!("frozen server holds seq 1");
    };
    assert_eq!(seq1.seq, 1);

    // Advance the genuine vault to seq 2 on a second server while `frozen` stays stuck at seq 1.
    let fresh = Lab::new("alab");
    aterm_secrets::transport::publish(
        &aterm_secrets::transport::Source::Url(fresh.url.clone()),
        &seq1.envelope,
    )
    .expect("seed fresh");
    set(&admin, &Audience::Everyone, "token", "v2\n");
    let ProbeOutcome::Found(seq2_from_admin) =
        client::probe(&frozen.url, &pin, HANG_DETECTOR).expect("seq2")
    else {
        panic!("seq 2");
    };
    assert_eq!(seq2_from_admin.seq, 2);
    // Reset `frozen` to a fresh server stuck at seq 1, and put seq 2 on `fresh`.
    let stuck = Lab::new("alab");
    aterm_secrets::transport::publish(
        &aterm_secrets::transport::Source::Url(stuck.url.clone()),
        &seq1.envelope,
    )
    .expect("seed stuck at seq 1");
    aterm_secrets::transport::publish(
        &aterm_secrets::transport::Source::Url(fresh.url.clone()),
        &seq2_from_admin.envelope,
    )
    .expect("seed fresh at seq 2");

    // Rogue server 1: a validly signed `alab` vault under an attacker's key, advanced to seq 3.
    let rogue = Lab::new("alab");
    let rogue_admin = rogue.home("evil");
    let rogue_created =
        client::create(&rogue_admin, &rogue.url, None, Some("evil")).expect("create rogue");
    set(&rogue_admin, &Audience::Everyone, "k", "1\n");
    set(&rogue_admin, &Audience::Everyone, "k", "2\n");
    let rogue_signer = aterm_secrets::b64::decode_padded(&rogue_created.signer).expect("base64");
    let rogue_pin: [u8; 32] = rogue_signer.as_slice().try_into().expect("32 bytes");
    let ProbeOutcome::Found(rogue_env) =
        client::probe(&rogue.url, &rogue_pin, HANG_DETECTOR).expect("rogue probe")
    else {
        panic!("rogue seq 3");
    };
    assert_eq!(rogue_env.seq, 3);

    // Rogue server 2: a tampered envelope claiming `seq: 99` with an invalid signature.
    let tampered_bytes = String::from_utf8(seq2_from_admin.envelope.clone())
        .expect("utf8 envelope")
        .replacen("seq: 2\n", "seq: 99\n", 1)
        .into_bytes();
    let mut tampered_reply = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n",
        tampered_bytes.len()
    )
    .into_bytes();
    tampered_reply.extend_from_slice(&tampered_bytes);
    let (tampered_url, tampered_heard) = answer_once(Box::leak(tampered_reply.into_boxed_slice()));

    let mk_txt = |seq: u64| {
        Some(VaultTxt {
            vault: "alab".into(),
            signer: created.fingerprint.clone(),
            seq,
            head: [0xab; 32],
            label: None,
        })
    };
    let defaults = ["http://127.0.0.1:9/v1/alab", stuck.url.as_str()];
    let discovered = [
        DiscoveredVault {
            url: fresh.url.clone(),
            txt: mk_txt(2),
        },
        DiscoveredVault {
            url: rogue.url.clone(),
            txt: mk_txt(3),
        },
        DiscoveredVault {
            url: tampered_url.clone(),
            txt: mk_txt(99),
        },
    ];
    let urls = candidate_urls(&defaults, &discovered, "alab");
    let outcome = client::probe_candidates(&urls, &pin, HANG_DETECTOR).expect("probed");
    tampered_heard
        .recv_timeout(HANG_DETECTOR)
        .expect("tampered server was probed and rejected");
    let ProbeOutcome::Found(winner) = outcome else {
        panic!("expected verified winner, got {outcome:?}");
    };
    assert_eq!(winner.seq, 2, "highest verified seq wins");
    assert_eq!(winner.url, fresh.url);

    // Negative control: accepting unverified envelopes by header `seq` picks the forged `seq: 99`.
    let accept_unverified = |bodies: &[(&str, &[u8])]| -> Option<(String, u64)> {
        bodies
            .iter()
            .filter_map(|(url, bytes)| {
                let parsed = aterm_secrets::envelope::parse(bytes).ok()?;
                Some(((*url).to_owned(), parsed.seq))
            })
            .max_by_key(|(_, seq)| *seq)
    };
    assert_eq!(
        accept_unverified(&[
            (&stuck.url, &seq1.envelope),
            (&fresh.url, &seq2_from_admin.envelope),
            (&rogue.url, &rogue_env.envelope),
            (&tampered_url, &tampered_bytes),
        ]),
        Some((tampered_url, 99)),
        "CONTROL: accepting unverified envelopes takes the forged higher seq"
    );
}
