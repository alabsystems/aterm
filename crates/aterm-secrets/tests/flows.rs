// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The flows of docs/DESIGN-secrets-2026-10-20.md §7–§9 end to end on loopback: one server,
//! an admin's home and machines' homes, each a scratch directory. Every value is synthetic and
//! made here; nothing reads the real `~/.secrets`.

#![cfg(unix)]

use aterm_secrets::SecretBytes;
use aterm_secrets::age::Identity;
use aterm_secrets::client::{
    self, Approval, JoinOptions, JoinOutcome, Membership, Paths, ProbeOutcome, SyncOutcome,
};
use aterm_secrets::envelope::{self, Unsigned};
use aterm_secrets::server::{self, Options, Running};
use aterm_secrets::text::{Audience, Document};
use aterm_secrets::transport::{self, Source};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// A value no real credential looks like.
fn synthetic(label: &str) -> String {
    format!(
        "synthetic-{label}-{}",
        aterm_uds::rand::hex_token::<6>().expect("random")
    )
}

struct Lab {
    root: aterm_tempfile::TempDir,
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

    /// A lab whose server holds a long-poll at most `hold`.
    fn holding(vault: &str, hold: Duration) -> Lab {
        Lab::tuned(vault, hold, server::JOIN_CHANGE_GAP)
    }

    /// A lab whose server lets a key change its join request at once (no
    /// [`server::JOIN_CHANGE_GAP`] sat out on a real clock).
    fn changing(vault: &str) -> Lab {
        Lab::tuned(vault, Duration::from_secs(50), Duration::ZERO)
    }

    fn tuned(vault: &str, hold: Duration, gap: Duration) -> Lab {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let server = start(listener, root.path().join("store"), hold, gap);
        let url = format!("http://{}/v1/{vault}", server.addr);
        Lab {
            root,
            server: Some(server),
            url,
            hold,
            gap,
        }
    }

    /// The server's Mac lost its store: a new server, empty, at the same address.
    fn restart_empty(&mut self) {
        let server = self.server.take().expect("running");
        let addr = server.addr;
        server.stop();
        let store = self.root.path().join("store");
        std::fs::remove_dir_all(&store).expect("store removed");
        let listener = std::net::TcpListener::bind(addr).expect("the same address again");
        self.server = Some(start(listener, store, self.hold, self.gap));
    }

    /// A machine's home, with its `~/.secrets` absent.
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

/// An unattended join that types `password` when asked for the join password.
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
            password: &|| password.map(SecretBytes::from),
            requested: &|| requested.set(requested.get() + 1),
            say: &|_| {},
        },
    );
    if matches!(outcome, Ok(JoinOutcome::Pending { .. })) {
        assert_eq!(requested.get(), 1, "the request hook runs once per request");
    }
    outcome
}

/// Approve every pending request at the admin's URL.
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

    // The admin's existing files become the vault, and stay exactly where they were.
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

    // A machine asks; nothing opens until an admin approves its code.
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

    // A host section reaches only its host.
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

    // A long-poll returns with the next version.
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

    // Unset removes the file it wrote; a file changed by hand is moved aside, never lost.
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

    // Revoked: the next version has no locker for it.
    {
        let admin_vault = client::admin_open(&admin).expect("open");
        let mut doc = admin_vault.doc.clone();
        assert!(doc.remove_machine("mb", false).expect("remove"));
        client::admin_publish(&admin, &admin_vault, &doc).expect("publish");
    }
    assert_eq!(
        client::sync(&machine, None).expect("sync"),
        SyncOutcome::NotIn
    );
    assert!(matches!(
        client::status(&machine).expect("status").membership,
        Membership::Revoked { .. }
    ));

    // Nothing printed about either machine carries a value.
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

    // A reader pinned to another key refuses the real vault outright.
    let stranger = envelope::signer_public(&seed).expect("public");
    let wrong = format!(
        "{}#{}",
        lab.url,
        aterm_secrets::b64::encode_padded(&stranger)
    );
    assert!(join_unattended(&lab.home("mc"), &wrong, "mc").is_err());

    // An unattended join of an unpinned source is refused before it trusts anything.
    let machine = lab.home("mb");
    assert!(join_unattended(&machine, &lab.url, "mb").is_err());
    assert_eq!(client::config(&machine).expect("config"), None);

    // At a terminal, the person compares the fingerprint.
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
            password: &|| None,
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
    let created = client::create(&admin, &lab.url, None, Some("ma")).expect("create");
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

    // The new key keeps publishing, and every reader keeps following.
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

    // A world-readable file of secrets is refused rather than trusted.
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).expect("chmod");
    assert!(client::sync(&machine, None).is_err());
}

#[test]
fn a_machine_that_knows_the_join_password_is_let_in_by_the_admins_subscriber() {
    let lab = Lab::new("door");
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
    let password = client::admin_open(&admin)
        .expect("open")
        .doc
        .join_password()
        .expect("create makes a join password");
    let pinned = format!("{}#{}", lab.url, created.signer);

    // A wrong password asks too, but nothing lets it in.
    let stranger = lab.home("mx");
    assert!(matches!(
        join_with(&stranger, &pinned, "mx", Some(b"not-the-password-at-all")).expect("join"),
        JoinOutcome::Pending { .. }
    ));
    // The right one, typed with blanks around it.
    let machine = lab.home("mb");
    let mut typed = b"  ".to_vec();
    typed.extend_from_slice(password.as_slice());
    typed.push(b'\n');
    assert!(matches!(
        join_with(&machine, &pinned, "mb", Some(&typed)).expect("join"),
        JoinOutcome::Pending { .. }
    ));
    // No password: the code path, which the subscriber leaves to a person.
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

    // A second pass approves nothing new and publishes nothing.
    let again = client::auto_approve(&admin).expect("auto-approve");
    assert!(again.approved.is_empty());
    assert_eq!(again.seq, None);

    // The password never reaches a machine that is not an admin.
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

#[test]
fn an_open_vault_lets_in_every_machine_that_asks_with_no_password() {
    let lab = Lab::new("open");
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
    let password = client::admin_open(&admin)
        .expect("open")
        .doc
        .join_password()
        .expect("create makes a join password");
    let pinned = format!("{}#{}", lab.url, created.signer);
    // Before the vault is open, a machine that asks with no password waits for a person.
    let early = lab.home("me");
    join_unattended(&early, &pinned, "me").expect("join");
    let closed = client::auto_approve(&admin).expect("approve");
    assert!(closed.approved.is_empty(), "{:?}", closed.approved);

    // The admin opens it: the password goes, and only admins could ever read either. The
    // machine already waiting joins only by code: it may be one let go while the vault was
    // closed, its approved request still queued.
    let opened = client::admin_open(&admin).expect("open");
    let me = client::requests(&admin, Some(&opened.doc)).expect("requests")[0]
        .recipient
        .clone();
    let mut doc = opened.doc.clone();
    doc.set_open(true).expect("open");
    let published = client::admin_publish(&admin, &opened, &doc).expect("publish");
    let asked: Vec<&str> = published.held.iter().map(|r| r.host.as_str()).collect();
    assert_eq!(asked, ["me"]);
    assert!(!published.kept_out);
    let doc = client::admin_open(&admin).expect("open").doc.clone();
    assert!(doc.open());
    assert!(doc.join_password().is_none());
    assert_eq!(doc.code_only(), std::slice::from_ref(&me));
    assert_eq!(Approval::of(&doc), Some(Approval::Open));

    // A machine that asks with no password, and one that still sends a proof of the old
    // password, are both let in in one publish; the one that asked before is not.
    let machine = lab.home("mb");
    assert!(matches!(
        join_unattended(&machine, &pinned, "mb").expect("join"),
        JoinOutcome::Pending { .. }
    ));
    let proving = lab.home("mc");
    join_with(&proving, &pinned, "mc", Some(password.as_slice())).expect("join");
    let listed = || client::requests(&admin, None).expect("requests");
    let mut settled = client::Settled::default();
    let approval = Approval::Open;
    let source = client::config(&admin).expect("config").expect("configured");
    assert!(settled.due(&source, held(&admin), &listed(), approval));
    let done = client::auto_approve(&admin).expect("approve");
    let mut approved = done.approved.clone();
    approved.sort_unstable();
    assert_eq!(approved, ["mb", "mc"]);
    assert!(done.refused.is_empty());
    assert_eq!(done.code_only, std::slice::from_ref(&me));
    settled.settle(done.judged);
    assert_eq!(
        client::sync(&early, None).expect("sync"),
        SyncOutcome::NotIn
    );
    // Approved by its code, it is in, and its line goes.
    approve_key(&admin, "me", &me);
    assert!(
        client::admin_open(&admin)
            .expect("open")
            .doc
            .code_only()
            .is_empty()
    );
    for member in [&machine, &proving, &early] {
        applied(client::sync(member, None).expect("sync"));
        assert_eq!(
            read(member, "gh_access_token").as_deref(),
            Some(token.as_str())
        );
    }
    // The approved requests stay queued: the new version judges them once, as members,
    // publishes nothing, and the approver rests.
    let seq = held(&admin);
    assert!(settled.due(&source, seq, &listed(), approval));
    let again = client::auto_approve(&admin).expect("approve");
    assert!(again.approved.is_empty() && again.code_only.is_empty());
    assert_eq!(again.seq, None);
    assert_eq!(held(&admin), seq);
    settled.settle(again.judged);
    assert!(!settled.due(&source, held(&admin), &listed(), approval));

    // Letting a machine go puts its key on a `code-only` line, so the request mb joined
    // with, still queued, does not let it straight back in, nor does asking again.
    let open = client::admin_open(&admin).expect("open");
    let mb = recipient_of(&open.doc, "mb");
    let mut doc = open.doc.clone();
    assert!(doc.remove_machine("mb", false).expect("remove"));
    let gone = client::admin_publish(&admin, &open, &doc).expect("publish");
    assert!(gone.kept_out && !gone.password_renewed && gone.held.is_empty());
    let kept = client::admin_open(&admin).expect("open").doc.clone();
    assert!(kept.open());
    assert_eq!(kept.code_only(), std::slice::from_ref(&mb));
    join_unattended(&machine, &pinned, "mb").expect("join");
    let after = client::auto_approve(&admin).expect("approve");
    assert!(after.approved.is_empty(), "{:?}", after.approved);
    assert_eq!(after.code_only, std::slice::from_ref(&mb));
    assert_eq!(after.seq, None);
    assert_eq!(
        client::sync(&machine, None).expect("sync"),
        SyncOutcome::NotIn
    );
    // A version that lets nobody go leaves the lines be.
    set(&admin, &Audience::Everyone, "note", "kept");
    let noted = client::admin_open(&admin).expect("open").doc.clone();
    assert!(noted.open());
    assert_eq!(noted.code_only(), std::slice::from_ref(&mb));

    // Closed to approval by code, a machine that asks waits for a person again.
    let open = client::admin_open(&admin).expect("open");
    let mut doc = open.doc.clone();
    doc.set_open(false).expect("close");
    client::admin_publish(&admin, &open, &doc).expect("publish");
    let closed = client::admin_open(&admin).expect("open").doc.clone();
    assert!(!closed.open() && closed.join_password().is_none());
    assert_eq!(Approval::of(&closed), None);
    let late = lab.home("md");
    join_unattended(&late, &pinned, "md").expect("join");
    let refused = client::auto_approve(&admin).expect("approve");
    assert!(refused.approved.is_empty(), "{:?}", refused.approved);
    assert_eq!(client::sync(&late, None).expect("sync"), SyncOutcome::NotIn);

    // Opened again, the machine let go is still kept out (its line outlived closing and
    // opening), and the one that asked while it was closed joins only by code too.
    let open = client::admin_open(&admin).expect("open");
    let md = client::requests(&admin, Some(&open.doc))
        .expect("requests")
        .into_iter()
        .find(|request| request.host == "md")
        .expect("md asks")
        .recipient;
    let mut doc = open.doc.clone();
    doc.set_open(true).expect("open");
    let reopened = client::admin_publish(&admin, &open, &doc).expect("publish");
    let asked: Vec<&str> = reopened.held.iter().map(|r| r.host.as_str()).collect();
    assert_eq!(asked, ["md"], "mb's key is code-only already");
    let none = client::auto_approve(&admin).expect("approve");
    assert!(none.approved.is_empty(), "{:?}", none.approved);
    let mut waiting = none.code_only.clone();
    waiting.sort_unstable();
    let mut want = vec![mb.clone(), md.clone()];
    want.sort_unstable();
    assert_eq!(waiting, want);

    // Approved by its code, the machine let go is back, and its line is gone; md's stays.
    approve_key(&admin, "mb", &mb);
    let back = client::admin_open(&admin).expect("open").doc.clone();
    assert_eq!(back.code_only(), std::slice::from_ref(&md));
    applied(client::sync(&machine, None).expect("sync"));
}

/// Admit `key` as `host` by hand, as `aterm secrets approve <code>` does.
fn approve_key(admin_paths: &Paths, host: &str, key: &str) {
    let open = client::admin_open(admin_paths).expect("open");
    let mut doc = open.doc.clone();
    let host = client::free_name(&doc, host);
    doc.add_machine(&host, key).expect("add");
    client::admin_publish(admin_paths, &open, &doc).expect("publish");
}

/// The key of `doc`'s machine `name`.
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
    let created = client::create(
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

    // Revoked: the version that lets it go carries a new password...
    let open = client::admin_open(&admin).expect("open");
    let mb = recipient_of(&open.doc, "mb");
    let mut doc = open.doc.clone();
    assert!(doc.remove_machine("mb", false).expect("remove"));
    let done = client::admin_publish(&admin, &open, &doc).expect("publish");
    assert!(done.password_renewed && !done.kept_out && done.held.is_empty());
    let renewed = client::admin_open(&admin)
        .expect("open")
        .doc
        .join_password()
        .expect("still a join password");
    assert_ne!(renewed.as_slice(), password.as_slice());
    // ...so the request it queued with the old one is not approved again,
    let again = client::auto_approve(&admin).expect("approve");
    assert!(again.approved.is_empty(), "{:?}", again.approved);
    assert_eq!(
        client::sync(&machine, None).expect("sync"),
        SyncOutcome::NotIn
    );
    // nor one under a new key that proves the old one; the new one still admits.
    let stale = lab.home("mc");
    join_with(&stale, &pinned, "mc", Some(password.as_slice())).expect("join");
    let fresh = lab.home("md");
    join_with(&fresh, &pinned, "md", Some(renewed.as_slice())).expect("join");
    let admitted = client::auto_approve(&admin).expect("approve");
    assert_eq!(admitted.approved, ["md"]);

    // A version that lets nobody go keeps the password.
    set(&admin, &Audience::Everyone, "note", "kept");
    let kept = client::admin_open(&admin)
        .expect("open")
        .doc
        .join_password()
        .expect("a join password");
    assert_eq!(kept.as_slice(), renewed.as_slice());

    // A closed vault writes no `code-only` line (an older aterm could not read it), but
    // opening it while the request mb joined with is still queued holds every waiting
    // request for its code: mb's, and mc's, which never was one.
    let open = client::admin_open(&admin).expect("open");
    assert!(open.doc.code_only().is_empty());
    let mut doc = open.doc.clone();
    doc.set_open(true).expect("open");
    let published = client::admin_publish(&admin, &open, &doc).expect("publish");
    let mut asked: Vec<&str> = published.held.iter().map(|r| r.host.as_str()).collect();
    asked.sort_unstable();
    assert_eq!(asked, ["mb", "mc"]);
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
    // A short hold, so the join's wait answers within a second of each pass.
    let lab = Lab::holding("race", Duration::from_secs(1));
    let admin = lab.home("ma");
    write_private(
        &admin.home.join(".secrets/gh_access_token"),
        &synthetic("gh"),
    );
    let created = client::create(
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
    // The subscriber the request hook installs runs at once: the admin's approver lets the
    // machine in and the machine's subscriber applies that version before the join's own
    // wait looks.
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
            // A hang detector: the join is done at its first pass.
            wait: Duration::from_secs(60),
            confirm: &|_| panic!("an unattended join never asks"),
            password: &|| Some(SecretBytes::from(password.as_slice())),
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
    let created = client::create(&admin, &lab.url, None, Some("ma")).expect("create");
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

    // A plain publish that drops an admin rotates by itself.
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
    // mc sleeps through the rotating version and two more.
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
    // What a build that carries the vault's first key pins (`ALAB_SIGNER` before a release
    // catches up with a rotation).
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
        // The version that makes the second pair is the one builds before chains cannot read.
        assert_eq!(done.first_long_chain, rotations == 2, "{rotations}");
        if rotations == 1 {
            continue;
        }
        // A new Mac pinned to the first key, two rotations on and three.
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
    // A machine asleep across all three rotations follows the chain too, and keeps the keys
    // it was reached through, which a build that retires them judges the pin by.
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
    assert!(client::sync(&machine, None).is_err(), "nothing is served");
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
        client::probe(&lab.url, &pin, Duration::ZERO).expect("answered"),
        ProbeOutcome::Absent,
        "an empty server holds no vault"
    );
    let admin = lab.home("ma");
    let created = client::create(&admin, &lab.url, None, Some("ma")).expect("create");
    let signer = aterm_secrets::b64::decode_padded(&created.signer).expect("base64");
    pin.copy_from_slice(&signer);
    let ProbeOutcome::Found(probed) =
        client::probe(&lab.url, &pin, Duration::ZERO).expect("answered")
    else {
        panic!("a vault");
    };
    assert_eq!(probed.seq, 1);
    assert_eq!(probed.fingerprint, created.fingerprint);
    // Another signer's vault answered: not the one asked for, which no later try changes.
    let other = client::probe(&lab.url, &[7u8; 32], Duration::ZERO);
    assert!(
        matches!(other, Ok(ProbeOutcome::NotThisVault(_))),
        "{other:?}"
    );
    // Nothing answering is a failure to reach the server, which a later try may get past.
    assert!(client::probe("http://127.0.0.1:9/v1/probed", &pin, Duration::ZERO).is_err());
}

/// A fresh machine key, as `age1…`.
fn new_key() -> String {
    Identity::generate()
        .expect("random")
        .recipient()
        .to_bech32()
}

/// The version `paths` holds, as a machine in the vault.
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
    let created = client::create(&admin, &lab.url, None, Some("ma")).expect("create");
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

    // `admins remove` keeps mb in [machines]; the version that drops it carries a new password,
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
    // so a key mb mints and proves with the password it read is not let in.
    let minted = lab.home("mb-minted");
    join_with(&minted, &pinned, "mbx", Some(known.as_slice())).expect("join");
    let refused = client::auto_approve(&admin).expect("approve");
    assert!(refused.approved.is_empty(), "{:?}", refused.approved);
    assert_eq!(refused.refused.len(), 1);

    // A rotate-key that drops no admin keeps the password.
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
    let created = client::create(&admin, &lab.url, None, Some("ma")).expect("create");
    let password = client::admin_open(&admin)
        .expect("open")
        .doc
        .join_password()
        .expect("create makes a join password");
    let pinned = format!("{}#{}", lab.url, created.signer);
    let listed = || client::requests(&admin, None).expect("requests");
    let mut settled = client::Settled::default();

    // The person mistypes the password: the approver judges the request once, refuses it and
    // leaves it be.
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

    // They join again and type it right, the server letting the key change its request
    // (`Lab::changing`): the new proof is judged at once, with nothing published between.
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

    // The approved request stays queued for days: the version that let it in is judged once,
    // finding mb a machine, and then the approver rests.
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

    // Another open vault at the same sequence can receive the identical request (including
    // a proof of the old vault's password). Its judgment cannot inherit this vault's rest.
    let other = Lab::new("other");
    let other_admin = other.home("other-admin");
    let created = client::create(&other_admin, &other.url, None, Some("other-admin"))
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
    let line = format!(
        "{} {} {} proof={}",
        request.recipient,
        request.host,
        request.code,
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
    // A cut that falls just after a `-` leaves no `-` before the suffix.
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
        // A hang detector: the search ends at once.
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
    // The approver admits mc while the person edits...
    let mut current = base.clone();
    current.add_machine("mc", &mc).expect("add");
    // ...who deletes mb, and adds a machine of their own under the name mc.
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
    // Another admin lets mb and mc go while the person edits, which puts their keys on
    // `code-only` lines...
    let mut current = base.clone();
    for (name, key) in [("mb", &mb), ("mc", &mc)] {
        assert!(current.remove_machine(name, false).expect("remove"));
        current.add_code_only(key).expect("code-only");
    }
    // ...and the person's text, which began with both in, deletes mb and keeps mc.
    let mut edited = base.clone();
    assert!(edited.remove_machine("mb", false).expect("remove"));
    client::keep_admitted(&base, &current, &mut edited).expect("merge");
    assert_eq!(
        edited.code_only(),
        std::slice::from_ref(&mb),
        "mb's line is kept; mc, a machine of the saved text, needs none"
    );
    assert!(
        edited
            .machines()
            .iter()
            .any(|machine| machine.recipient == mc)
    );
}

#[test]
fn an_edit_that_lands_after_the_approver_keeps_the_machine_and_the_password() {
    let lab = Lab::new("edit");
    let admin = lab.home("ma");
    let created = client::create(&admin, &lab.url, None, Some("ma")).expect("create");
    // The edit begins; mb proves the password and is let in while the editor is open.
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
        let mut doc = vault.doc.clone();
        assert!(doc.remove_machine("mb", false).expect("remove"));
        client::admin_publish(&admin, &vault, &doc).expect("publish");
    }
    assert_eq!(
        client::sync(&machine, None).expect("sync"),
        SyncOutcome::NotIn
    );
    assert!(matches!(
        client::status(&machine).expect("status").membership,
        Membership::Revoked { .. }
    ));

    // Asking again makes it a machine that waits, whose subscriber re-asks hourly.
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
    // source.toml is gone (moved aside, or a restore that missed it); state.toml stays.
    std::fs::remove_file(machine.dir.join("source.toml")).expect("remove source.toml");

    // Another vault, at a lower version than the leftover state holds.
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
    // A short hold, so each join's one-second wait ends with its first long-poll.
    let lab = Lab::holding("hint", Duration::from_secs(1));
    let admin = lab.home("ma");
    let created = client::create(&admin, &lab.url, None, Some("ma")).expect("create");
    let pinned = format!("{}#{}", lab.url, created.signer);
    // At a terminal the join asks for the password again; with it piped in, it is piped again,
    // since a join without a terminal that is not given it asks by code alone.
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
                password: &|| Some(SecretBytes::from(&b"a-mistyped-join-password"[..])),
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
        // On screen before the wait, which a Ctrl-C may end.
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
fn an_edit_at_a_path_that_lands_after_another_admin_keeps_the_machine_it_admitted() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let shared: PathBuf = root.path().join("shared").join("team.vault");
    std::fs::create_dir_all(shared.parent().expect("parent")).expect("dir");
    let admin = Paths::for_home(&root.path().join("ma"));
    let created =
        client::create(&admin, shared.to_str().expect("utf-8"), None, Some("ma")).expect("create");
    // The edit begins; meanwhile another admin admits mc, which joined the path.
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
    // The path, like the server, refuses a version that is not newer than the one it holds.
    assert!(
        client::admin_publish(&admin, &began, &edited).is_err(),
        "the save lands on a vault that moved"
    );
    let fresh = client::admin_open(&admin).expect("reopen");
    assert_eq!(fresh.seq(), began.seq() + 1);
    let kept = client::keep_admitted(&base, &fresh.doc, &mut edited).expect("merge");
    assert_eq!(kept, ["mc"]);
    let done = client::admin_publish(&admin, &fresh, &edited).expect("publish");
    assert!(!done.password_renewed, "nobody was let go");
    applied(client::sync(&machine, None).expect("mc is still in"));
    assert_eq!(read(&machine, "note").as_deref(), Some("synthetic-note"));
}

/// A wait for something that must happen: a hang detector, never a latency budget.
const HANG_DETECTOR: Duration = Duration::from_secs(60);

/// Something at a loopback address that is no vault server: it reads one request head, sends
/// `reply` (nothing at all when it is empty), closes, and hands the head back. Read with a
/// hang detector, so a probe that never connects fails the test instead of hanging it.
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

/// Something that is no vault server answering at the vault's address (a web page on its
/// port, a router's redirect or login, another protocol) is not the vault either: an answer
/// no later try changes, not a failure to reach it. What the vault server itself can say
/// stays a failure, since a later try may get past it.
#[test]
fn a_probe_answered_by_something_else_is_not_the_vault() {
    let probed = |reply: &'static [u8]| {
        let (url, heard) = answer_once(reply);
        let answered = client::probe(&url, &[7u8; 32], Duration::ZERO);
        let head = heard
            .recv_timeout(HANG_DETECTOR)
            .expect("the probe reached the stub");
        assert!(head.starts_with("GET /v1/alab HTTP/1.1\r\n"), "{head}");
        answered
    };
    for reply in [
        &b"HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: 35\r\n\r\n<html><body>It works!</body></html>"[..],
        // The vault server never redirects, and asks no credentials of a GET.
        b"HTTP/1.1 301 Moved Permanently\r\nLocation: https://alab-secrets.local/v1/alab\r\nContent-Length: 0\r\n\r\n",
        b"HTTP/1.1 302 Found\r\nLocation: /login\r\nContent-Length: 0\r\n\r\n",
        b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\n\r\n",
        b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n",
        // Not HTTP at all, and a body larger than any envelope.
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
    // A cap the vault server met, and a connection closed with nothing said (a server that
    // stopped mid-request).
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
