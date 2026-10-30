// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::harness::claims::{Book, Claim, Kind, LeaseKey, Row, View};
use crate::harness::headless_oauth::{
    MailboxConfig, MailboxCredential, discover_mailbox_config_files,
    poll_mailbox_config_for_credential,
};
use aterm_secrets::age::Identity;
use aterm_secrets::swarm_roster::{SwarmMember, SwarmRoster};
use std::time::Duration;

fn deterministic_identity(seed: u8) -> Identity {
    let mut secret = [0u8; 32];
    for (idx, b) in secret.iter_mut().enumerate() {
        *b = seed
            .wrapping_mul(37)
            .wrapping_add(u8::try_from(idx).unwrap_or(0).wrapping_mul(11));
    }
    Identity::from_secret_bytes(secret)
}

fn make_member(label: &str, aliases: &[&str], id: &Identity, node_id: Option<&str>) -> SwarmMember {
    SwarmMember {
        recipient: id.recipient().to_bech32(),
        vault_name: label.to_owned(),
        label: label.to_owned(),
        aliases: aliases.iter().map(|s| (*s).to_owned()).collect(),
        hint: None,
        admitted_ms: 1_793_250_000_000,
        claimed_label: None,
        claimed_at_ms: None,
        node_id: node_id.map(str::to_owned),
        node_key: None,
    }
}

#[test]
fn a_mac_decrypts_only_its_warm_accounts_and_catch_all_stays_off_ai_signin() {
    let m3_id = deterministic_identity(3);
    let m14_id = deterministic_identity(14);
    let m22_id = deterministic_identity(22);
    let m99_id = deterministic_identity(99);

    let roster = SwarmRoster {
        members: vec![
            make_member("m3", &[], &m3_id, Some("node-m3")),
            make_member("m14", &["m82"], &m14_id, Some("node-m14")),
            make_member("m22", &[], &m22_id, Some("node-m22")),
            make_member("m99", &[], &m99_id, Some("node-m99")),
        ],
        deny_nodes: Vec::new(),
        deny_labels: vec!["m99".to_owned()],
    };

    let pool = Pool::from_toml(
        r#"
[[account]]
provider = "claude"
email = "acc1@example.com"
warm = ["m3", "m82", "m99"]

[[account]]
provider = "claude"
email = "acc2@example.com"
warm = ["m14"]

[[account]]
provider = "claude"
email = "catchall-row@example.com"
warm = ["m22"]
"#,
        "ai-pool.toml",
    )
    .expect("parse pool");

    let mut admin_cfg = MailboxConfig {
        imap_url: Some("imaps://catchall:embed-secret@imap.example.com:993/INBOX".to_owned()),
        webmail_url: Some("https://mail.example.com/inbox".to_owned()),
        password: Some("catchall-master-secret".to_owned()),
        totp_secret: Some("JBSWY3DPEHPK3PXP".to_owned()),
        ..MailboxConfig::default()
    };
    admin_cfg
        .passwords
        .insert("acc1@example.com".to_owned(), "pw-acc1-only".to_owned());
    admin_cfg
        .totp_secrets
        .insert("acc1@example.com".to_owned(), "TOTPACC1ONLY2345".to_owned());
    admin_cfg
        .passwords
        .insert("acc2@example.com".to_owned(), "pw-acc2-only".to_owned());

    let doc = build_ai_signin(&admin_cfg, &pool, &roster).expect("build ai-signin.toml");
    assert!(doc.relay, "catch-all login on admin sets relay = true");
    assert_eq!(
        doc.imap_url.as_deref(),
        Some("imaps://imap.example.com:993/INBOX"),
        "embedded IMAP credentials must be stripped from ai-signin.toml"
    );
    assert_eq!(
        doc.accounts.len(),
        2,
        "only explicit per-email rows are sealed"
    );

    let toml_text = doc.to_toml().expect("serialize ai-signin.toml");
    assert!(!toml_text.contains("catchall-master-secret"));
    assert!(!toml_text.contains("embed-secret"));
    assert!(!toml_text.contains("pw-acc1-only"));
    assert!(!toml_text.contains("pw-acc2-only"));

    let reloaded = AiSigninDoc::from_toml(&toml_text).expect("parse ai-signin.toml");
    assert_eq!(reloaded, doc);

    // m3 is warm on acc1 only.
    let mut m3_cfg = MailboxConfig::default();
    assert_eq!(reloaded.apply_into_mailbox(Some(&m3_id), &mut m3_cfg), 1);
    assert_eq!(
        m3_cfg.passwords.get("acc1@example.com").map(String::as_str),
        Some("pw-acc1-only")
    );
    assert_eq!(
        m3_cfg
            .totp_secrets
            .get("acc1@example.com")
            .map(String::as_str),
        Some("TOTPACC1ONLY2345")
    );
    assert!(!m3_cfg.passwords.contains_key("acc2@example.com"));
    assert!(m3_cfg.relay_enabled);

    // m14 matches `m82` alias on acc1 AND `m14` primary on acc2.
    let mut m14_cfg = MailboxConfig::default();
    assert_eq!(reloaded.apply_into_mailbox(Some(&m14_id), &mut m14_cfg), 2);
    assert_eq!(
        m14_cfg
            .passwords
            .get("acc1@example.com")
            .map(String::as_str),
        Some("pw-acc1-only")
    );
    assert_eq!(
        m14_cfg
            .passwords
            .get("acc2@example.com")
            .map(String::as_str),
        Some("pw-acc2-only")
    );

    // m22 has no warm per-email accounts (only catchall-row@example.com): opens 0 bundles.
    let mut m22_cfg = MailboxConfig::default();
    assert_eq!(reloaded.apply_into_mailbox(Some(&m22_id), &mut m22_cfg), 0);
    assert!(m22_cfg.passwords.is_empty());
    assert!(m22_cfg.relay_enabled);

    // m99 is listed in acc1's warm set, but `deny_labels` excludes it at build time.
    let mut m99_cfg = MailboxConfig::default();
    assert_eq!(reloaded.apply_into_mailbox(Some(&m99_id), &mut m99_cfg), 0);
    assert!(m99_cfg.passwords.is_empty());

    // Negative control (`seal to everyone`): sealing to all non-denied roster members leaks acc1
    // and acc2 to m22.
    let mutant =
        build_ai_signin_mutant_seal_to_all(&admin_cfg, &pool, &roster).expect("mutant build");
    let mut mutant_m22_cfg = MailboxConfig::default();
    let leaked = mutant.apply_into_mailbox(Some(&m22_id), &mut mutant_m22_cfg);
    assert_eq!(
        leaked, 2,
        "negative control: seal-to-everyone mutant leaks non-warm accounts to m22"
    );
}

#[test]
fn relay_authorizes_warm_and_leased_emails_and_refuses_outside_set() {
    let now = 1_793_250_000;
    let m3_id = deterministic_identity(3);
    let m22_id = deterministic_identity(22);
    let denied_id = deterministic_identity(77);

    let roster = SwarmRoster {
        members: vec![
            make_member("m3", &[], &m3_id, Some("node-m3")),
            make_member("m22", &["m90"], &m22_id, Some("node-m22")),
            make_member("m77", &[], &denied_id, Some("node-denied")),
        ],
        deny_nodes: vec!["node-denied".to_owned()],
        deny_labels: Vec::new(),
    };

    let pool = Pool::from_toml(
        r#"
[[account]]
provider = "claude"
email = "m3-warm@example.com"
warm = ["m3"]

[[account]]
provider = "claude"
email = "m22-warm@example.com"
warm = ["m90"]
"#,
        "ai-pool.toml",
    )
    .expect("parse pool");

    // m22's row holds a lease claim on `borrowed@example.com` that lapses at `now + 600`.
    let leases = (
        View::default(),
        Book::of([Row {
            machine: "aa22".to_owned(),
            label: "m22".to_owned(),
            counter: 1,
            until: now + 600,
            claims: vec![Claim {
                key: LeaseKey::new("claude", "borrowed@example.com").unwrap(),
                kind: Kind::Lease,
                since: now - 60,
            }],
        }]),
    );

    let ask = |email: &str| MailCodeRequest {
        email: email.to_owned(),
        since: Some(now - 30),
        senders: vec!["anthropic".to_owned()],
        claimed_label: None,
    };

    // 1. Warm match (via alias `m90`) succeeds and binds m22's age recipient.
    let ok_warm = authorize_mail_code_requester(
        "node-m22",
        &ask("m22-warm@example.com"),
        &roster,
        Some(&pool),
        Some(&leases),
        now,
    )
    .expect("warm email authorized");
    assert_eq!(ok_warm.label, "m22");
    assert_eq!(ok_warm.recipient, m22_id.recipient());

    // 2. Live claim match (`borrowed@example.com`) succeeds while live, fails once it lapses.
    assert!(
        authorize_mail_code_requester(
            "node-m22",
            &ask("borrowed@example.com"),
            &roster,
            Some(&pool),
            Some(&leases),
            now,
        )
        .is_ok()
    );
    assert_eq!(
        authorize_mail_code_requester(
            "node-m22",
            &ask("borrowed@example.com"),
            &roster,
            Some(&pool),
            Some(&leases),
            now + 600,
        ),
        Err(MailCodeRefusal::UnauthorizedEmail)
    );

    // Denied alias on a member is also refused by `member_is_denied`.
    let mut alias_denied_roster = roster.clone();
    alias_denied_roster.deny_labels.push("m90".to_owned());
    assert_eq!(
        authorize_mail_code_requester(
            "node-m22",
            &ask("m22-warm@example.com"),
            &alias_denied_roster,
            Some(&pool),
            Some(&leases),
            now,
        ),
        Err(MailCodeRefusal::Denied)
    );

    // 3. Email outside requester's warm set and active leases is refused.
    assert_eq!(
        authorize_mail_code_requester(
            "node-m22",
            &ask("m3-warm@example.com"),
            &roster,
            Some(&pool),
            Some(&leases),
            now,
        ),
        Err(MailCodeRefusal::UnauthorizedEmail)
    );

    // 4. Denied node and unknown node are refused.
    assert_eq!(
        authorize_mail_code_requester(
            "node-denied",
            &ask("m22-warm@example.com"),
            &roster,
            Some(&pool),
            Some(&leases),
            now,
        ),
        Err(MailCodeRefusal::Denied)
    );
    assert_eq!(
        authorize_mail_code_requester(
            "node-stranger",
            &ask("m22-warm@example.com"),
            &roster,
            Some(&pool),
            Some(&leases),
            now,
        ),
        Err(MailCodeRefusal::UnknownRequester)
    );

    // 5. Negative control (`relay answers any email`): mutant authorizes m22 for m3's email.
    assert!(
        authorize_mail_code_mutant_any_email(
            "node-m22",
            &ask("m3-warm@example.com"),
            &roster,
            Some(&pool),
            Some(&leases),
            now,
        )
        .is_ok(),
        "negative control: any-email mutant must fail the unauthorized-email refusal check"
    );
}

#[test]
fn mail_code_wire_and_sealed_credential_round_trip() {
    let req = MailCodeRequest {
        email: "Alice@Example.com".to_owned(),
        since: Some(1_793_250_123),
        senders: vec!["Anthropic".to_owned(), "claude.ai".to_owned()],
        claimed_label: Some("m22".to_owned()),
    };
    let wire = req.to_wire();
    assert_eq!(
        wire,
        "mail-code alice@example.com since=1793250123 senders=anthropic,claude.ai label=m22"
    );
    let parsed = MailCodeRequest::parse_wire(&wire).expect("parse wire");
    assert_eq!(parsed.email, "alice@example.com");
    assert_eq!(parsed.since, Some(1_793_250_123));
    assert_eq!(parsed.senders, vec!["anthropic", "claude.ai"]);
    assert_eq!(parsed.claimed_label.as_deref(), Some("m22"));

    let m22_id = deterministic_identity(22);
    let m3_id = deterministic_identity(3);
    for cred in [
        MailboxCredential::Otp("842917".to_owned()),
        MailboxCredential::MagicLink("https://claude.ai/magic-link#token-xyz".to_owned()),
    ] {
        let (sealed, _) =
            seal_credential_to_recipient(&cred, &m22_id.recipient()).expect("seal credential");
        assert!(!sealed.contains('\n'));
        assert_eq!(
            open_sealed_credential(&sealed, &m22_id).expect("decrypt as m22"),
            cred
        );
        assert!(
            open_sealed_credential(&sealed, &m3_id).is_err(),
            "another Mac's identity must fail to open m22's sealed credential"
        );
    }
}

#[cfg(unix)]
mod unix_relay_tests {
    use super::*;
    use aterm_net::fleet::{FleetOp, FleetScope, FleetSecret, Listening};
    use aterm_net::fleet_member::{
        Began, Peer, SECRET_LEN, Via, node_identity, request_mail_code_from_peer, write_held,
    };
    use aterm_secrets::client::{
        JoinOptions, JoinPassword, Paths, admin_open, admin_publish, create, join,
        machine_identity, sync,
    };
    use aterm_secrets::swarm_roster::ROSTER_ENTRY;
    use aterm_secrets::text::Audience;
    use std::net::{IpAddr, Ipv4Addr, TcpListener, TcpStream};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    const HANG: Duration = Duration::from_secs(60);
    const OPS: &[FleetOp] = &[FleetOp::Survey, FleetOp::MailCode];

    #[test]
    fn live_fleet_mail_code_relays_newest_code_sealed_to_asker_and_logs_audit() {
        let now = 1_793_250_500;
        let root = aterm_tempfile::tempdir().expect("tempdir");

        // 1. Create a signed vault on Steward Mac (`m1`) and join Member Mac (`m22`).
        let vault_file = root.path().join("alab-vault.age");
        let steward_home = root.path().join("steward-home");
        let steward_state = root.path().join("steward-state");
        let member_home = root.path().join("member-home");
        let member_state = root.path().join("member-state");

        std::fs::create_dir_all(steward_home.join(".secrets")).unwrap();
        std::fs::create_dir_all(member_home.join(".secrets")).unwrap();
        std::fs::create_dir_all(member_home.join(".config/aterm")).unwrap();
        aterm_uds::private_file::ensure_private_dir(&steward_state).unwrap();
        aterm_uds::private_file::ensure_private_dir(&member_state).unwrap();

        let steward_paths = Paths::for_home(&steward_home);
        let created = create(
            &steward_paths,
            &vault_file.to_string_lossy(),
            None,
            Some("m1"),
        )
        .expect("create signed path vault");
        let steward_id = machine_identity(&steward_paths).unwrap();

        let member_paths = Paths::for_home(&member_home);
        let member_age_id = machine_identity(&member_paths).unwrap();

        let steward_node = node_identity(&steward_state).expect("steward node");
        let member_node = node_identity(&member_state).expect("member node");

        let pool_toml = r#"
[[account]]
provider = "claude"
email = "m22-catchall@example.com"
warm = ["m22"]

[[account]]
provider = "claude"
email = "m1-private@example.com"
warm = ["m1"]
"#;

        // Publish `aterm-swarm-roster`, `ai-pool.toml`, and `ai-signin.toml` (`relay = true`) to the vault,
        // and admit `m22` to `[machines]`.
        {
            let adm = admin_open(&steward_paths).expect("admin open");
            let mut doc = adm.doc.clone();
            doc.add_machine("m22", &member_age_id.recipient().to_bech32())
                .expect("add m22 machine");
            let swarm_roster = SwarmRoster {
                members: vec![
                    make_member("m1", &[], &steward_id, Some(steward_node.node())),
                    make_member("m22", &[], &member_age_id, Some(member_node.node())),
                ],
                deny_nodes: Vec::new(),
                deny_labels: Vec::new(),
            };
            doc.set_entry(
                &Audience::Everyone,
                ROSTER_ENTRY,
                swarm_roster.to_toml().as_bytes(),
            )
            .unwrap();
            doc.set_entry(&Audience::Everyone, "ai-pool.toml", pool_toml.as_bytes())
                .unwrap();
            let signin_doc = AiSigninDoc {
                imap_url: None,
                webmail_url: None,
                http_url: None,
                relay: true,
                accounts: Vec::new(),
            };
            doc.set_entry(
                &Audience::Everyone,
                AI_SIGNIN_FILE,
                signin_doc.to_toml().unwrap().as_bytes(),
            )
            .unwrap();
            admin_publish(&steward_paths, &adm, &doc).expect("admin publish");
        }
        sync(&steward_paths, None).expect("steward sync");

        let pinned_source = format!("{}#{}", vault_file.display(), created.signer);
        join(
            &member_paths,
            &JoinOptions {
                source: pinned_source,
                host: Some("m22".to_owned()),
                interactive: false,
                wait: Duration::ZERO,
                confirm: &|_| false,
                password: JoinPassword::Given(None),
                fetched: None,
                requested: &|| {},
                say: &|_| {},
            },
        )
        .expect("member join");
        sync(&member_paths, None).expect("member sync");

        // 2. Give Steward Mac a local catch-all Maildir (`ai-mailbox.toml` in admin's `.secrets/`)
        //    with an older mbox message (`111111`) followed by a newer message (`842917`) for m22,
        //    plus a message (`999999`) for m1's private account.
        let maildir = root.path().join("steward-catchall.mbox");
        let mbox_body = concat!(
            "From mailer@anthropic.com Thu Oct 28 09:00:00 2026\n",
            "From: Anthropic <noreply@anthropic.com>\n",
            "To: <m22-catchall@example.com>\n",
            "Date: Wed, 28 Oct 2026 09:00:00 +0000\n",
            "Subject: Sign in code\n\n",
            "Your verification code is 111111\n\n",
            "From mailer@anthropic.com Thu Oct 28 09:05:00 2026\n",
            "From: Anthropic <noreply@anthropic.com>\n",
            "To: <m22-catchall@example.com>\n",
            "Date: Wed, 28 Oct 2026 09:05:00 +0000\n",
            "Subject: Sign in code\n\n",
            "Your verification code is 842917\n\n",
            "From mailer@anthropic.com Thu Oct 28 09:06:00 2026\n",
            "From: Anthropic <noreply@anthropic.com>\n",
            "To: <m1-private@example.com>\n",
            "Date: Wed, 28 Oct 2026 09:06:00 +0000\n",
            "Subject: Sign in code\n\n",
            "Your verification code is 999999\n"
        );
        std::fs::write(&maildir, mbox_body).unwrap();
        std::fs::write(
            steward_home.join(".secrets/ai-mailbox.toml"),
            format!("dir = \"{}\"\n", maildir.display()),
        )
        .unwrap();

        // 3. Start Steward's fleet listener serving `FleetOp::MailCode`.
        let secret_bytes = [0x5a; SECRET_LEN];
        let secret = FleetSecret::new(&secret_bytes).unwrap();
        let tcp_listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let addr = tcp_listener.local_addr().unwrap();
        let running = Arc::new(AtomicBool::new(true));
        let listening = Listening {
            secret: secret.clone(),
            scope: FleetScope::Facts,
            node: Some(steward_node.clone()),
            serves: OPS,
            slots: 16,
            per_source: 16,
            lifetime: HANG,
        };
        let run_flag = Arc::clone(&running);
        let st_dir = steward_state.clone();
        let st_home = steward_home.clone();
        let server_thread = std::thread::spawn(move || {
            aterm_net::fleet::serve(
                &tcp_listener,
                &listening,
                &run_flag,
                move |served, op| {
                    assert_eq!(op, FleetOp::MailCode);
                    let proof = served.peer_node().cloned();
                    let st = st_dir.clone();
                    let hm = st_home.clone();
                    served.answer(|stream| {
                        let _ = serve_fleet_mail_code(stream, proof.as_ref(), &st, Some(&hm), now)?;
                        Ok(())
                    })
                },
                |_| {},
            );
        });

        // 4. Configure Member `m22`'s fleet book pointing to Steward (`m1`).
        let mark = secret.mark();
        let steward_node_id = steward_node.node().to_owned();
        let steward_key = steward_node.key();
        write_held(&member_state, Began::Here, |book| {
            book.write_key(&secret_bytes).unwrap();
            book.update(|peers| {
                peers.push(Peer {
                    node: Some(steward_node_id),
                    key: Some(steward_key),
                    key_from: None,
                    host: "m1.local".to_owned(),
                    addr: Some(IpAddr::V4(Ipv4Addr::LOCALHOST)),
                    port: addr.port(),
                    version: Some("0.146.0".to_owned()),
                    seen_ms: Some(1_000),
                    via: Via::Issuer,
                    fleet: Some(mark),
                    signed: None,
                });
                Ok(())
            })
        })
        .unwrap()
        .unwrap();

        // 5. Member `m22` discovers its mailbox config from `~/.secrets/ai-signin.toml` alone
        //    (no local mailbox!) and polls for `m22-catchall@example.com`.
        let member_store_dir = member_state.join("accounts");
        std::fs::create_dir_all(&member_store_dir).unwrap();
        let discovered =
            discover_mailbox_config_files(Some(&member_home), Some(&member_store_dir), None)
                .expect("discover member mailbox config via ai-signin.toml");
        assert!(discovered.relay_enabled);
        assert!(discovered.is_configured_for(Some("m22-catchall@example.com")));

        let got =
            poll_mailbox_config_for_credential(&discovered, Some("m22-catchall@example.com"), HANG)
                .expect("relay newest OTP to m22");
        assert_eq!(
            got,
            MailboxCredential::Otp("842917".to_owned()),
            "steward must return ONLY the newest OTP for m22-catchall@example.com"
        );

        // 6. Verify Steward's audit log recorded the answer without leaking the OTP value.
        let audit_log =
            std::fs::read_to_string(steward_state.join(MAIL_CODE_AUDIT_FILE)).expect("audit log");
        assert!(audit_log.contains("label=m22"), "audit={audit_log}");
        assert!(
            audit_log.contains("email=m22-catchall@example.com"),
            "audit={audit_log}"
        );
        assert!(audit_log.contains("kind=otp"), "audit={audit_log}");
        assert!(
            !audit_log.contains("842917"),
            "audit log must never record secret OTP values: {audit_log}"
        );

        // 7. Member `m22` asking directly for `m1-private@example.com` is refused (`unauthorized-email`).
        let direct_unauth = request_mail_code_from_peer(
            "m1.local",
            Some(IpAddr::V4(Ipv4Addr::LOCALHOST)),
            addr.port(),
            (&secret, Some(&member_node)),
            "mail-code m1-private@example.com",
            HANG,
        )
        .expect("fleet exchange");
        assert_eq!(direct_unauth, Err("unauthorized-email".to_owned()));

        // 7b. Once Steward's book binds `member_node.node()` to `member_node.key()`, a caller
        //     claiming `member_node.node()` with a different Ed25519 key is refused (`denied`).
        let member_node_id = member_node.node().to_owned();
        let member_key = member_node.key();
        write_held(&steward_state, Began::Here, |book| {
            book.write_key(&secret_bytes).unwrap();
            book.update(|peers| {
                peers.push(Peer {
                    node: Some(member_node_id),
                    key: Some(member_key),
                    key_from: None,
                    host: "m22.local".to_owned(),
                    addr: Some(IpAddr::V4(Ipv4Addr::LOCALHOST)),
                    port: 7100,
                    version: Some("0.146.0".to_owned()),
                    seen_ms: Some(1_000),
                    via: Via::Joined,
                    fleet: Some(mark),
                    signed: None,
                });
                Ok(())
            })
        })
        .unwrap()
        .unwrap();
        let spoof_member =
            aterm_net::fleet::NodeIdentity::from_seed(member_node.node(), &[0x99; 32])
                .expect("spoof member id");
        let spoof_res = request_mail_code_from_peer(
            "m1.local",
            Some(IpAddr::V4(Ipv4Addr::LOCALHOST)),
            addr.port(),
            (&secret, Some(&spoof_member)),
            "mail-code m22-catchall@example.com",
            HANG,
        )
        .expect("fleet exchange");
        assert_eq!(spoof_res, Err("denied".to_owned()));

        // 8. Member `m22` (which only has `ai-signin.toml`, no steward `ai-mailbox.toml`) refuses
        //    `steward_handle_mail_code_in` with `NotSteward`.
        assert_eq!(
            steward_handle_mail_code_in(
                steward_node.node(),
                "mail-code m22-catchall@example.com",
                &member_state,
                Some(&member_home),
                now,
            ),
            Err(MailCodeRefusal::NotSteward)
        );

        running.store(false, Ordering::Release);
        let _ = TcpStream::connect(addr);
        let _ = server_thread.join();
    }

    #[cfg(unix)]
    #[test]
    fn select_steward_relay_peers_excludes_ordinary_members_and_orders_standbys_issuer() {
        use aterm_net::fleet_member::{Peer, Via};
        use std::net::{IpAddr, Ipv4Addr};

        let mk_peer = |host: String, port: u16, node: &str, via: Via| Peer {
            node: Some(node.to_owned()),
            key: None,
            key_from: None,
            host,
            addr: Some(IpAddr::V4(Ipv4Addr::LOCALHOST)),
            port,
            version: None,
            seen_ms: Some(1_000),
            via,
            fleet: None,
            signed: None,
        };
        let mut peers = Vec::new();
        for idx in (10..36u16).filter(|&idx| idx != 22) {
            peers.push(mk_peer(
                format!("m{idx}.local"),
                7100 + idx,
                &format!("{idx:032x}"),
                Via::IssuerBook,
            ));
        }
        peers.push(mk_peer(
            "m22.local".to_owned(),
            7100,
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            Via::Joined,
        ));
        peers.push(mk_peer(
            "m1.local".to_owned(),
            7101,
            "01010101010101010101010101010101",
            Via::Issuer,
        ));
        peers.push(mk_peer(
            "m2.local".to_owned(),
            7102,
            "02020202020202020202020202020202",
            Via::IssuerBook,
        ));
        peers.push(mk_peer(
            "m3.local".to_owned(),
            7103,
            "03030303030303030303030303030303",
            Via::IssuerBook,
        ));

        let own_node = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        // Negative control: dialing every non-self peer in `fleet-peers.toml` + `issuer-book.toml`
        // would fan out across all 28 peers (including 25 ordinary non-steward members).
        let unpruned_count = peers
            .iter()
            .filter(|p| p.node.as_deref() != Some(own_node))
            .count();
        assert_eq!(unpruned_count, 28);

        let standbys = vec!["m2".to_owned(), "m22".to_owned()];
        let selected = select_steward_relay_peers(&peers, own_node, &standbys, None);
        let hosts: Vec<&str> = selected.iter().map(|p| p.host.as_str()).collect();
        assert_eq!(
            hosts,
            vec!["m2.local", "m1.local"],
            "relay must dial only standby stewards, then the issuer (never own_node or ordinary members)"
        );

        // A peer spoofing `m2.local` with a mismatched node_id or a denied node_id must be
        // rejected when a roster binds `m2` to `0202...`.
        let roster = SwarmRoster {
            members: vec![make_member(
                "m2",
                &[],
                &deterministic_identity(2),
                Some("02020202020202020202020202020202"),
            )],
            deny_labels: Vec::new(),
            deny_nodes: vec!["99999999999999999999999999999999".to_owned()],
        };
        assert!(
            !peer_matches_label(
                "m2.local",
                Some("ffffffffffffffffffffffffffffffff"),
                None,
                "m2",
                Some(&roster),
            ),
            "a peer with a different node_id than the roster-bound standby must not match"
        );
        assert!(
            !peer_matches_label(
                "m2.local",
                Some("99999999999999999999999999999999"),
                None,
                "m2",
                Some(&roster),
            ),
            "a denied node_id must not match"
        );
        assert!(peer_matches_label(
            "m2.local",
            Some("02020202020202020202020202020202"),
            None,
            "m2",
            Some(&roster),
        ));
    }

    #[cfg(unix)]
    #[test]
    fn append_audit_record_rotates_bounded_private_log_at_one_mib() {
        use std::os::unix::fs::PermissionsExt as _;

        let tmp = aterm_tempfile::tempdir().expect("tempdir");
        let state_dir = tmp.path().join("state");
        aterm_uds::private_file::ensure_private_dir(&state_dir).unwrap();
        let log_path = state_dir.join(MAIL_CODE_AUDIT_FILE);
        let prev_path = state_dir.join(MAIL_CODE_AUDIT_PREV_FILE);

        let seed = vec![b'x'; usize::try_from(MAIL_CODE_AUDIT_MAX_BYTES).unwrap()];
        std::fs::write(&log_path, &seed).unwrap();
        std::fs::set_permissions(&log_path, std::fs::Permissions::from_mode(0o600)).unwrap();

        // Negative control before append: no rotated `.1` file exists and current log is at cap.
        assert!(!prev_path.exists());
        assert_eq!(
            std::fs::metadata(&log_path).unwrap().len(),
            MAIL_CODE_AUDIT_MAX_BYTES
        );

        let audit = MailCodeAudit {
            at: 1_793_198_000,
            requester_label: "m22".to_owned(),
            requester_node: "22222222222222222222222222222222".to_owned(),
            email: "m22-catchall@example.com".to_owned(),
            kind: "otp",
        };
        append_audit_record(&state_dir, &audit);

        let prev_meta = std::fs::metadata(&prev_path).expect("rotated .1 file must exist");
        assert_eq!(prev_meta.len(), MAIL_CODE_AUDIT_MAX_BYTES);
        assert_eq!(prev_meta.permissions().mode() & 0o777, 0o600);

        let cur_meta = std::fs::metadata(&log_path).expect("fresh log file must exist");
        assert!(cur_meta.len() < 512, "fresh log len={}", cur_meta.len());
        assert_eq!(cur_meta.permissions().mode() & 0o777, 0o600);
        let fresh = std::fs::read_to_string(&log_path).unwrap();
        assert!(fresh.contains("label=m22") && fresh.contains("kind=otp"));
    }
}

/// What a Mac built before routed bundles reads of one: only the per-email fields, any other
/// ignored.
#[derive(Debug, PartialEq, Eq, serde::Deserialize)]
struct PreRouteBundle {
    password: Option<String>,
    mailbox_password: Option<String>,
    totp_secret: Option<String>,
}

/// A ROUTED ACCOUNT'S OWN MAILBOX IS SEALED UNDER `route`, AND NO SHARED ONE IS SEALED: an
/// account's own `[[mailboxes]]` entry (its one address its login) is sealed to its warm Macs
/// whole under `route` (its login, URL and secrets, a missing password taken from the per-email
/// one), beside exactly the per-email values a Mac built before `route` reads; the shared Proton
/// entry (the top-level login), a base address's entry that also serves a `+tag` account, and
/// an address read through another login's mailbox seal nothing, so neither the owner's Proton
/// nor Gmail secrets leave the stewards; and the warm Mac reads the sealed entry as a route.
/// NEGATIVE CONTROL: dropping `serves_another` from `sealed_route_for` seals the base address's
/// mailbox, and this fails on the account count; filing the route's password as the bundle's
/// own lets the pre-route reader see `own-entry-pw`, and this fails.
#[test]
fn an_own_mailbox_is_sealed_under_its_route_and_a_shared_one_stays_with_the_stewards() {
    let m3_id = deterministic_identity(3);
    let roster = SwarmRoster {
        members: vec![make_member("m3", &[], &m3_id, Some("node-m3"))],
        deny_nodes: Vec::new(),
        deny_labels: Vec::new(),
    };
    let account = |email: &str| {
        format!("[[account]]\nprovider = \"claude\"\nemail = \"{email}\"\nwarm = [\"m3\"]\n\n")
    };
    let pool_text: String = [
        "own@custom.example.test",
        "solo@custom.example.test",
        "alias1@pm.example.test",
        "base@pm.example.test",
        "base+t@pm.example.test",
        "routed@outlook.example.test",
    ]
    .iter()
    .map(|e| account(e))
    .collect();
    let pool = Pool::from_toml(&pool_text, "ai-pool.toml").expect("pool");
    let admin = MailboxConfig::from_toml_str(
        r#"
user = "owner@proton.example.test"
password = "owner-proton-pw"
totp_secret = "JBSWY3DPEHPK3PXP"

[passwords]
"own@custom.example.test" = "own-legacy-pw"
"solo@custom.example.test" = "solo-legacy-pw"
"routed@outlook.example.test" = "routed-own-pw"

[[mailboxes]]
login = "own@custom.example.test"
password = "own-entry-pw"
imap_url = "imaps://imap.custom.example.test:993/INBOX"
receives = ["own@custom.example.test"]

[[mailboxes]]
login = "solo@custom.example.test"
password = ""
receives = ["solo@custom.example.test"]

[[mailboxes]]
login = "owner@proton.example.test"
receives = ["*@pm.example.test"]

[[mailboxes]]
login = "base@pm.example.test"
password = "base-pw"
receives = ["base@pm.example.test"]

[[mailboxes]]
login = "owner-gmail@gmail.example.test"
password = "owner-gmail-pw"
receives = ["routed@outlook.example.test"]
"#,
        None,
    )
    .expect("admin mailbox");
    let doc = build_ai_signin(&admin, &pool, &roster).expect("build ai-signin.toml");
    let sealed: Vec<&str> = doc.accounts.iter().map(|a| a.email.as_str()).collect();
    assert_eq!(
        sealed,
        ["own@custom.example.test", "solo@custom.example.test"],
        "only the accounts' own mailboxes are sealed"
    );
    for entry in &doc.accounts {
        let plain = age::decrypt(std::slice::from_ref(&m3_id), entry.sealed.as_bytes())
            .expect("open")
            .as_slice()
            .to_vec();
        let text = String::from_utf8(plain.clone()).expect("utf8");
        for foreign in [
            "owner-proton-pw",
            "owner-gmail-pw",
            "base-pw",
            "routed-own-pw",
        ] {
            assert!(!text.contains(foreign), "{}: {foreign}", entry.email);
        }
        let old: PreRouteBundle = aterm_json::from_slice(&plain).expect("pre-route read");
        let legacy = if entry.email.starts_with("own") {
            "own-legacy-pw"
        } else {
            "solo-legacy-pw"
        };
        assert_eq!(
            old,
            PreRouteBundle {
                password: Some(legacy.to_owned()),
                mailbox_password: None,
                totp_secret: None,
            },
            "a pre-route Mac reads exactly the per-email values it read before"
        );
    }

    let mut warm = MailboxConfig::default();
    assert_eq!(doc.apply_into_mailbox(Some(&m3_id), &mut warm), 2);
    let own = Some("own@custom.example.test");
    assert_eq!(
        warm.webmail_user_for(own).as_deref(),
        Some("own@custom.example.test")
    );
    assert_eq!(warm.password_for(own).as_deref(), Some("own-entry-pw"));
    assert_eq!(
        warm.effective_imap_url(own).as_deref(),
        Some("imaps://imap.custom.example.test:993/INBOX")
    );
    assert_eq!(
        warm.password_for(Some("solo@custom.example.test"))
            .as_deref(),
        Some("solo-legacy-pw"),
        "an own entry with no password of its own seals the per-email one"
    );
    assert!(warm.route_for(Some("alias1@pm.example.test")).is_none());
}

#[test]
fn denied_member_stale_claim_cannot_block_active_member_mail_code() {
    use crate::harness::claims::{Book, Claim, Kind, LeaseKey, Row, View};

    let m12_id = deterministic_identity(12);
    let m60_id = deterministic_identity(60);
    let roster = SwarmRoster {
        members: vec![
            make_member("m12", &[], &m12_id, Some("node-m12")),
            make_member("m60", &[], &m60_id, Some("node-m60")),
        ],
        deny_labels: vec!["m60".to_owned()],
        deny_nodes: Vec::new(),
    };
    let key = LeaseKey::new("claude", "shared@example.com").unwrap();
    let now = 1_760_000_000_i64;
    let book = Book::of([
        Row {
            machine: "00000000000000000000000000000060".to_owned(),
            label: "m60".to_owned(),
            counter: 1,
            until: now + 3_600,
            claims: vec![Claim {
                key: key.clone(),
                kind: Kind::Lease,
                since: now - 500,
            }],
        },
        Row {
            machine: "00000000000000000000000000000012".to_owned(),
            label: "m12".to_owned(),
            counter: 1,
            until: now + 3_600,
            claims: vec![Claim {
                key,
                kind: Kind::Lease,
                since: now - 100,
            }],
        },
    ]);
    let view = View {
        me: "00000000000000000000000000000001".to_owned(),
        presence: None,
    };
    let req = MailCodeRequest {
        email: "shared@example.com".to_owned(),
        since: None,
        senders: Vec::new(),
        claimed_label: Some("m12".to_owned()),
    };
    let claims = (view, book);
    let auth = authorize_mail_code_requester("node-m12", &req, &roster, None, Some(&claims), now)
        .expect("active member m12 must not be blocked by denied m60's earlier claim");
    assert_eq!(auth.label, "m12");
    assert_eq!(auth.recipient, m12_id.recipient());
}

#[cfg(unix)]
#[test]
fn read_standby_steward_labels_filters_denied_stewards() {
    use std::time::Duration;

    let srv_dir = aterm_tempfile::tempdir().expect("server dir");
    let home_dir = aterm_tempfile::tempdir().expect("home dir");
    let socket = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let running = aterm_secrets::server::start(
        socket,
        &aterm_secrets::server::Options {
            listen: "127.0.0.1:0".parse().unwrap(),
            store: srv_dir.path().to_path_buf(),
            advertise: None,
            pins: Vec::new(),
            hold: Duration::from_millis(50),
            keep_awake: false,
        },
    )
    .expect("start server");
    let url = format!("http://{}/v1/test", running.addr);
    let paths = aterm_secrets::client::Paths::for_home(home_dir.path());
    aterm_secrets::client::create(&paths, &url, None, Some("m1")).expect("create vault");
    let adm = aterm_secrets::client::admin_open(&paths).expect("admin open");
    let mut doc = adm.doc.clone();
    let roster = SwarmRoster {
        members: Vec::new(),
        deny_labels: vec!["m2".to_owned()],
        deny_nodes: Vec::new(),
    };
    doc.set_entry(
        &aterm_secrets::text::Audience::Everyone,
        aterm_secrets::swarm_roster::ROSTER_ENTRY,
        roster.to_toml().as_bytes(),
    )
    .expect("set roster");
    aterm_secrets::client::admin_publish(&paths, &adm, &doc).expect("publish");
    aterm_secrets::client::sync(&paths, None).expect("sync");

    std::fs::write(
        home_dir.path().join(".secrets/swarm-policy.toml"),
        "[[steward]]\nlabel = \"m2\"\nrank = 1\nalways_on = true\n\n[[steward]]\nlabel = \"m22\"\nrank = 2\nalways_on = true\n",
    )
    .unwrap();

    let labels = read_standby_steward_labels(home_dir.path());
    assert_eq!(
        labels,
        vec!["m22".to_owned()],
        "denied steward m2 must be filtered out of standby steward labels"
    );
    running.stop();
}
