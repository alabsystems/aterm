// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

/// A plain `set` or `unset` of a shared name is a member's on every vault with a membership
/// log, whatever the seat (a seatless or busy Mac is told so, never sent to the admin locker);
/// `--for`, a vault with no log, and a name a member may not write are an admin's. The
/// `admin key:` line's verbs are read from each verb's route: on a seated log vault neither a
/// plain `set`, a plain `unset` nor `revoke` needs the admin key.
#[cfg(unix)]
#[test]
fn a_plain_set_of_a_shared_name_is_a_members_and_for_is_an_admins() {
    use super::{admin_verbs, by_member};
    use crate::client::WriteRoute;
    for route in [WriteRoute::Seated, WriteRoute::NotSeated, WriteRoute::Busy] {
        assert!(by_member("gh_access_token", &[], route), "{route:?}");
    }
    assert!(!by_member("gh_access_token", &[], WriteRoute::NoLog));
    assert!(
        !by_member("gh_access_token", &["m3".to_owned()], WriteRoute::Seated),
        "--for is an admin's"
    );
    for held in [
        crate::entry_class::RELEASE_GH_TOKEN,
        crate::entry_class::FLEET_ACCOUNTS,
        crate::entry_class::GIT_CONFIG,
        crate::entry_class::FLEET_CODE,
    ] {
        assert!(!by_member(held, &[], WriteRoute::Seated), "{held}");
    }
    let seated = admin_verbs(WriteRoute::Seated);
    for absent in ["set", "unset", "revoke"] {
        assert!(!seated.contains(&absent), "{absent}: {seated:?}");
    }
    for present in [
        "set --for",
        "set of a held name",
        "unset --for",
        "unset of a held name",
        "edit",
    ] {
        assert!(seated.contains(&present), "{present}: {seated:?}");
    }
    let no_log = admin_verbs(WriteRoute::NoLog);
    for present in ["set", "set --for", "unset", "revoke"] {
        assert!(no_log.contains(&present), "{present}: {no_log:?}");
    }
}

/// The line each member write's outcome prints: a write signed here, or one that waits, is
/// "not yet in vault"; a posted one says a per-host copy still wins where one exists; a busy
/// state and a missing seat are refusals that name no admin.
#[cfg(unix)]
#[test]
fn each_member_outcome_says_where_the_write_stands() {
    use super::member_words;
    use crate::client::{MemberWritten, WaitFor};
    let posted = member_words("t", &MemberWritten::Posted).unwrap();
    assert!(
        posted.contains("for every member")
            && posted.contains("a per-host copy, where one exists, still wins on its Mac"),
        "{posted}"
    );
    let mut later = vec![MemberWritten::SignedHere];
    later.extend(
        [
            WaitFor::RotationDue,
            WaitFor::NewestUnopened,
            WaitFor::KeysUnsettled,
            WaitFor::PendingOfThisMac,
            WaitFor::LockDeadline,
        ]
        .map(MemberWritten::Waiting),
    );
    for written in &later {
        let line = member_words("t", written).unwrap();
        assert!(line.contains("not yet in vault"), "{written:?}: {line}");
    }
    assert_eq!(
        member_words("t", &MemberWritten::Waiting(WaitFor::RotationDue)).unwrap(),
        "not yet in vault: waits for the vault's new key after a removal; this Mac's vault pass signs it"
    );
    assert_eq!(
        member_words("t", &MemberWritten::Busy).unwrap_err(),
        "the vault is busy; try again"
    );
    for refused in [
        MemberWritten::Busy,
        MemberWritten::NotSeated,
        MemberWritten::Overtaken(None),
    ] {
        let why = member_words("t", &refused).unwrap_err();
        assert!(!why.contains("admin"), "{why}");
    }
}

/// On a fixture seated Mac with no admin key, `status` says `vault writes: member` and an
/// `admin key:` line whose verbs are those whose route needs the admin locker there (no
/// `revoke`, no plain `set`), and a plain `set` of a shared name writes the vault with no
/// admin: another Mac reads it after a sync. A held name is refused in a member's words,
/// never "not an admin"; `--for` still takes the admin route.
#[cfg(unix)]
#[test]
fn a_seated_mac_sets_with_no_admin_and_status_says_so() {
    use super::{Fill, admin_verbs, unix};
    use crate::client::WriteRoute;
    let macs = crate::client::two_log_macs_for_test(&[]);
    let (m22, m7) = (&macs.m22, &macs.m7);
    let lines = unix::writes_lines(m7);
    assert_eq!(lines[0], "vault writes: member");
    let verbs = admin_verbs(WriteRoute::Seated);
    assert_eq!(
        lines[1],
        format!(
            "admin key: no (on this vault it opens for: {})",
            verbs.join(", ")
        )
    );
    assert!(!lines[1].contains("revoke"), "{}", lines[1]);
    assert!(unix::writes_lines(m22)[1].starts_with("admin key: yes ("));

    let value = macs.root.path().join("value");
    std::fs::write(&value, b"token-v9").unwrap();
    let file = || Some(value.to_str().unwrap().to_owned());
    let code =
        unix::set(m7, "gh_access_token", &[], file(), &Fill::default()).expect("a member's set");
    assert_eq!(code, std::process::ExitCode::SUCCESS);
    crate::client::sync(m22, None).expect("m22 syncs");
    assert_eq!(
        std::fs::read(m22.home.join(".secrets/gh_access_token")).unwrap(),
        b"token-v9"
    );

    let held = crate::entry_class::RELEASE_GH_TOKEN;
    let error = unix::set(m7, held, &[], file(), &Fill::default()).expect_err("held");
    assert_eq!(
        Some(error.0.clone()),
        crate::entry_class::member_refusal(held),
        "{error}"
    );
    let error = unix::set(
        m7,
        "gh_access_token",
        &["m7".to_owned()],
        file(),
        &Fill::default(),
    )
    .expect_err("--for is an admin's");
    assert!(error.0.contains("is not an admin of"), "{error}");
}

/// On the same fixture seated Mac with no admin key, a plain `unset` of a shared name removes
/// it with no admin: another Mac takes its file back after a sync. A held name is refused in
/// a member's words, never "not an admin", and its file stays on the Mac that holds it;
/// `--for` still takes the admin route.
#[cfg(unix)]
#[test]
fn a_seated_mac_unsets_with_no_admin() {
    use super::unix;
    let macs = crate::client::two_log_macs_for_test(&[]);
    let (m22, m7) = (&macs.m22, &macs.m7);
    let held = crate::entry_class::RELEASE_GH_TOKEN;
    for name in ["gh_access_token", held] {
        crate::client::test_put_everyone(m22, name, b"token-v1").expect("m22 puts it");
    }
    crate::client::sync(m22, None).expect("m22 syncs");
    crate::client::sync(m7, None).expect("m7 syncs");
    assert!(
        m22.home.join(".secrets/gh_access_token").exists(),
        "CONTROL"
    );

    let code = unix::unset(m7, "gh_access_token", &[]).expect("a member's unset");
    assert_eq!(code, std::process::ExitCode::SUCCESS);
    crate::client::sync(m22, None).expect("m22 syncs");
    assert!(
        !m22.home.join(".secrets/gh_access_token").exists(),
        "m22 takes its file back"
    );
    let error = unix::unset(m7, "gh_access_token", &[]).expect_err("absent");
    assert_eq!(error.0, "no entry gh_access_token in [everyone]");

    let error = unix::unset(m7, held, &[]).expect_err("held");
    assert_eq!(
        Some(error.0.clone()),
        crate::entry_class::member_refusal(held),
        "{error}"
    );
    crate::client::sync(m22, None).expect("m22 syncs");
    assert_eq!(
        std::fs::read(m22.home.join(format!(".secrets/{held}"))).unwrap(),
        b"token-v1",
        "the held file stays"
    );
    let error =
        unix::unset(m7, "gh_access_token", &["m7".to_owned()]).expect_err("--for is an admin's");
    assert!(error.0.contains("is not an admin of"), "{error}");
}

/// The line each member unset's outcome prints: one signed here is "not yet in vault", a
/// posted one says a per-host copy still wins where one exists, and the refusals name no
/// admin, but for a vault with no log, which only an admin writes.
#[cfg(unix)]
#[test]
fn each_member_unset_outcome_says_where_the_removal_stands() {
    use super::member_unset_words;
    use crate::client::MemberUnset;
    let posted = member_unset_words("t", &MemberUnset::Posted).unwrap();
    assert!(
        posted.contains("for every member")
            && posted.contains("a per-host copy, where one exists, still wins on its Mac"),
        "{posted}"
    );
    let here = member_unset_words("t", &MemberUnset::SignedHere).unwrap();
    assert!(here.contains("not yet in vault"), "{here}");
    let back = member_unset_words("t", &MemberUnset::TookBack).unwrap();
    assert!(back.contains("the vault never held it"), "{back}");
    assert_eq!(
        member_unset_words("t", &MemberUnset::Absent).unwrap_err(),
        "no entry t in [everyone]"
    );
    for refused in [
        MemberUnset::Busy,
        MemberUnset::NotSeated,
        MemberUnset::Overtaken(None),
        MemberUnset::Refused(crate::client::RESEAL_REFUSAL.to_owned()),
    ] {
        let why = member_unset_words("t", &refused).unwrap_err();
        assert!(!why.contains("admin"), "{why}");
    }
}

/// A vault at a path has no membership log a member writes: its admin writes it, and `status`
/// says so, naming a plain `set` and `revoke` among the verbs that need the admin key there.
#[cfg(unix)]
#[test]
fn a_path_vault_is_written_by_its_admin() {
    use super::{admin_verbs, unix};
    use crate::client::WriteRoute;
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let admin = admin_of_a_path_vault(root.path());
    let soon = std::time::Instant::now() + std::time::Duration::from_secs(60);
    assert_eq!(
        crate::client::write_route(&admin, soon).unwrap(),
        WriteRoute::NoLog
    );
    let lines = unix::writes_lines(&admin);
    assert_eq!(lines[0], "vault writes: admin (a vault with no log)");
    assert_eq!(
        lines[1],
        format!(
            "admin key: yes (on this vault it opens for: {})",
            admin_verbs(WriteRoute::NoLog).join(", ")
        )
    );
    for verb in ["set", "revoke"] {
        assert!(admin_verbs(WriteRoute::NoLog).contains(&verb), "{verb}");
    }
}

/// A host Mac's `status` counts the per-host copy that shadows an `[everyone]` entry, which
/// only a host can see; another Mac prints no such line.
#[cfg(unix)]
#[test]
fn a_host_macs_status_counts_its_shadowing_copy() {
    use super::unix;
    use crate::text::Audience;
    let macs = crate::client::two_log_macs_with_for_test(
        &[
            (Audience::Everyone, "hosted", b"e1".as_slice()),
            (Audience::Hosts(vec!["m7".to_owned()]), "hosted", b"h1"),
        ],
        false,
    );
    let lines = unix::writes_lines(&macs.m7);
    assert!(
        lines
            .iter()
            .any(|line| line == "per-host copies: 1 shadow [everyone]"),
        "{lines:?}"
    );
    assert!(
        !unix::writes_lines(&macs.m22)
            .iter()
            .any(|line| line.starts_with("per-host copies:")),
    );
}

#[test]
fn secret_listing_accepts_metadata_flags_without_operands() {
    use super::{Command, parse};
    assert_eq!(
        parse(&["ls".into(), "--describe".into()]).unwrap(),
        Command::Ls {
            describe: true,
            json: false
        }
    );
    assert_eq!(
        parse(&["ls".into(), "--json".into()]).unwrap(),
        Command::Ls {
            describe: false,
            json: true
        }
    );
    assert!(parse(&["ls".into(), "--json".into(), "token".into()]).is_err());
}
use super::*;

#[test]
fn join_requests_json_is_an_optional_flag_without_operands() {
    assert_eq!(
        parse(&["requests".into()]).unwrap(),
        Command::Requests { json: false }
    );
    assert_eq!(
        parse(&["requests".into(), "--json".into()]).unwrap(),
        Command::Requests { json: true }
    );
    assert!(parse(&["requests".into(), "--json".into(), "extra".into()]).is_err());
}

#[cfg(unix)]
#[test]
fn join_requests_report_the_answering_queue_without_disclosing_proofs() {
    let request = crate::client::Request {
        recipient: crate::age::Identity::from_secret_bytes([41; 32])
            .recipient()
            .to_bech32(),
        host: "new-machine".into(),
        code: "abcd-efgh-ijkl-mnop".into(),
        at: Some(1793460000),
        allowed: Some("private-member-allow".into()),
        proof: Some("private-password-proof".into()),
    };
    let inspection = crate::client::RequestInspection {
        requests: vec![request],
        queue_source: Some("https://queue.example/v1/team".into()),
        queue_server: Some("192.0.2.7".parse().unwrap()),
        queue_version: Some(42),
        ..Default::default()
    };
    let report = unix::requests_json(&inspection, None);
    assert_eq!(report["known_members_filtered"].as_bool(), Some(false));
    assert_eq!(report["scope"].as_str(), Some("observed-vault-queue"));
    assert_eq!(
        report["queue"]["source"].as_str(),
        Some("https://queue.example/v1/team")
    );
    assert_eq!(
        report["queue"]["server_address"].as_str(),
        Some("192.0.2.7")
    );
    assert_eq!(report["queue"]["version"].as_u64(), Some(42));
    let row = &report["requests"][0];
    assert_eq!(row["host"].as_str(), Some("new-machine"));
    assert_eq!(row["requested_at"].as_u64(), Some(1793460000));
    assert!(row.get("lan_observed").is_none());
    assert_eq!(row["member_allow_present"].as_bool(), Some(true));
    assert_eq!(row["password_proof_present"].as_bool(), Some(true));
    assert!(row["code_only"].is_null());
    let encoded = aterm_json::to_string(&report).unwrap();
    assert!(!encoded.contains("private-member-allow"));
    assert!(!encoded.contains("private-password-proof"));
}

#[cfg(unix)]
#[test]
fn join_requests_code_only_state_requires_administrator_metadata() {
    let owner = crate::age::Identity::from_secret_bytes([42; 32])
        .recipient()
        .to_bech32();
    let held = crate::age::Identity::from_secret_bytes([43; 32])
        .recipient()
        .to_bech32();
    let other = crate::age::Identity::from_secret_bytes([44; 32])
        .recipient()
        .to_bech32();
    let mut doc = crate::text::Document::new_vault("admin", &owner);
    doc.add_code_only(&held).unwrap();
    let requests: Vec<_> = [held, other]
        .into_iter()
        .map(|recipient| crate::client::Request {
            recipient,
            host: "queued-machine".into(),
            code: "abcd-efgh-ijkl-mnop".into(),
            at: None,
            allowed: None,
            proof: None,
        })
        .collect();
    let inspection = crate::client::RequestInspection {
        requests,
        known_members_filtered: true,
        ..Default::default()
    };
    let report = unix::requests_json(&inspection, Some(&doc));
    assert_eq!(report["known_members_filtered"].as_bool(), Some(true));
    assert!(report["queue"]["source"].is_null());
    assert!(report["queue"]["server_address"].is_null());
    assert!(report["queue"]["version"].is_null());
    assert_eq!(report["requests"][0]["code_only"].as_bool(), Some(true));
    assert_eq!(report["requests"][1]["code_only"].as_bool(), Some(false));
    assert!(report["requests"][1]["requested_at"].is_null());
    assert_eq!(
        report["requests"][1]["member_allow_present"].as_bool(),
        Some(false)
    );
    assert_eq!(
        report["requests"][1]["password_proof_present"].as_bool(),
        Some(false)
    );
}

#[test]
fn sync_diagnostic_is_an_opt_in_flag_without_operands() {
    assert_eq!(
        parse(&["sync".into()]).unwrap(),
        Command::Sync { diagnostic: false }
    );
    assert_eq!(
        parse(&["sync".into(), "--diagnostic".into()]).unwrap(),
        Command::Sync { diagnostic: true }
    );
    assert!(parse(&["sync".into(), "--diagnostic".into(), "extra".into()]).is_err());
}

#[test]
fn mirrors_lists_or_adds_or_removes_urls() {
    assert_eq!(
        parse(&words("mirrors")),
        Ok(Command::Mirrors {
            change: None,
            urls: Vec::new()
        })
    );
    assert_eq!(
        parse(&words("mirrors add http://a:1/v1/x http://b:1/v1/x")),
        Ok(Command::Mirrors {
            change: Some(true),
            urls: words("http://a:1/v1/x http://b:1/v1/x")
        })
    );
    assert_eq!(
        parse(&words("mirrors remove http://a:1/v1/x")),
        Ok(Command::Mirrors {
            change: Some(false),
            urls: words("http://a:1/v1/x")
        })
    );
    for refused in [
        "mirrors add",
        "mirrors remove",
        "mirrors list",
        "mirrors x y",
    ] {
        assert!(parse(&words(refused)).is_err(), "{refused}");
    }
}

/// `aterm secrets mirrors add` saves a mirror of this vault that does not answer yet, refuses
/// one of another vault and one serving bytes this Mac's pin does not verify, saving nothing,
/// and `remove` takes one off.
///
/// NEGATIVE CONTROL: save what `add` is given without reading it first (no `probe_mirror`):
/// fails "a vault this pin does not verify is refused".
#[cfg(unix)]
#[test]
fn mirrors_add_refuses_what_is_no_address_of_this_vault() {
    use crate::client;
    let root = aterm_tempfile::tempdir().unwrap();
    let paths = client::Paths::for_home(&root.path().join("admin"));
    let serve = |store: std::path::PathBuf| {
        crate::server::start(
            std::net::TcpListener::bind("127.0.0.1:0").unwrap(),
            &crate::server::Options {
                listen: "127.0.0.1:0".parse().unwrap(),
                store,
                advertise: None,
                pins: Vec::new(),
                hold: std::time::Duration::from_millis(200),
                keep_awake: false,
            },
        )
        .unwrap()
    };
    let server = serve(root.path().join("store"));
    let url = format!("http://{}/v1/team", server.addr);
    client::create(&paths, &url, None, Some("admin")).unwrap();
    let gone = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let unanswered = format!("http://{}/v1/team", gone.local_addr().unwrap());
    drop(gone);
    assert_eq!(
        unix::mirrors(&paths, Some(true), std::slice::from_ref(&unanswered)).unwrap(),
        std::process::ExitCode::SUCCESS
    );
    assert_eq!(
        client::config(&paths).unwrap().unwrap().mirrors,
        std::slice::from_ref(&unanswered)
    );
    let other = format!("http://{}/v1/other", server.addr);
    assert!(
        unix::mirrors(&paths, Some(true), &[other]).is_err(),
        "another vault's address is refused"
    );
    // An address serving bytes no pin of this vault verifies: the other vault's store.
    let foreign_root = root.path().join("foreign");
    let foreign = client::Paths::for_home(&foreign_root);
    let foreign_server = serve(foreign_root.join("store"));
    let foreign_url = format!("http://{}/v1/team", foreign_server.addr);
    client::create(&foreign, &foreign_url, None, Some("other")).unwrap();
    assert!(
        unix::mirrors(&paths, Some(true), std::slice::from_ref(&foreign_url)).is_err(),
        "a vault this pin does not verify is refused"
    );
    assert_eq!(
        client::config(&paths).unwrap().unwrap().mirrors,
        std::slice::from_ref(&unanswered),
        "nothing saved"
    );
    assert!(unix::mirrors(&paths, Some(false), std::slice::from_ref(&url)).is_err());
    assert_eq!(
        unix::mirrors(&paths, Some(false), &[unanswered]).unwrap(),
        std::process::ExitCode::SUCCESS
    );
    assert!(client::config(&paths).unwrap().unwrap().mirrors.is_empty());
    foreign_server.stop();
    server.stop();
}

#[cfg(unix)]
#[test]
fn removing_an_unknown_mirror_never_echoes_url_credentials() {
    let root = aterm_tempfile::tempdir().unwrap();
    let paths = crate::client::Paths::for_home(root.path());
    crate::fsx::ensure_private_dir(&paths.dir).unwrap();
    crate::fsx::write_atomic(
        &paths.dir.join("source.toml"),
        b"url = 'https://vault.example/v1/team'\nname = 'team'\nhost = 'test'\n",
        0o600,
    )
    .unwrap();
    let credential = "synthetic-unlisted-mirror-credential";
    for url in [
        format!("https://mirror.example/v1/team?token={credential}"),
        format!("https://mirror.example/v1/team#{credential}"),
        format!("https://user:{credential}@mirror.example/v1/team"),
    ] {
        let error = unix::mirrors(&paths, Some(false), &[url]).unwrap_err();
        let shown = error.to_string();
        assert!(!shown.contains(credential));
        assert!(shown.contains("not a mirror of this vault"));
    }
    assert!(
        crate::client::config(&paths)
            .unwrap()
            .unwrap()
            .mirrors
            .is_empty()
    );
}

#[test]
fn keep_takes_one_or_more_names_and_no_flag() {
    assert_eq!(
        parse(&[
            "keep".into(),
            "signing".into(),
            "~/.aterm/machine.key".into()
        ])
        .unwrap(),
        Command::Keep(vec!["signing".into(), "~/.aterm/machine.key".into()])
    );
    assert!(parse(&["keep".into()]).is_err());
    assert!(parse(&["keep".into(), "--all".into()]).is_err());
}

#[cfg(unix)]
#[test]
fn sync_diagnostic_reports_a_real_config_failure_without_its_path_or_contents() {
    let root = aterm_tempfile::tempdir().unwrap();
    let paths = crate::client::Paths::for_home(root.path());
    crate::fsx::ensure_private_dir(&paths.dir).unwrap();
    crate::fsx::write_atomic(
        &paths.dir.join("source.toml"),
        b"private-value = [broken",
        0o600,
    )
    .unwrap();
    let result = unix::sync_diagnostic(&paths);
    assert_eq!(
        result,
        crate::sync_diagnostic::SyncDiagnostic::Failed(crate::ErrorKind::Other)
    );
    assert!(!result.success());
    let line = result.line();
    assert!(!line.contains("private-value"));
    assert!(!line.contains(root.path().to_str().unwrap()));
    assert_eq!(
        crate::sync_diagnostic::SyncDiagnostic::parse(&line),
        Some(result)
    );
}

#[cfg(unix)]
#[test]
fn signed_get_refuses_materialized_bytes_changed_after_sync() {
    use crate::{client, fsx, materialize, text::Audience};
    let root = aterm_tempfile::tempdir().unwrap();
    let paths = client::Paths::for_home(&root.path().join("admin"));
    let source = root.path().join("test.vault");
    client::create(&paths, source.to_str().unwrap(), None, Some("admin")).unwrap();
    let admin = client::admin_open(&paths).unwrap();
    let mut doc = admin.doc.clone();
    doc.set_entry(&Audience::Everyone, "token", b"authentic-value")
        .unwrap();
    client::admin_publish(&paths, &admin, &doc).unwrap();
    assert_eq!(
        unix::get_value(&paths, "token").unwrap().as_slice(),
        b"authentic-value"
    );
    let path = materialize::target_path(&paths.home, "token").unwrap();
    fsx::write_atomic(&path, b"changed-after-sync", 0o600).unwrap();
    assert!(unix::get_value(&paths, "token").is_err());
    let config_file = paths.dir.join("source.toml");
    let source_text = fsx::read_private(&config_file, 64 * 1024).unwrap().unwrap();
    let unsigned = source_text
        .as_str()
        .unwrap()
        .lines()
        .filter(|line| !line.starts_with("signer = "))
        .collect::<Vec<_>>()
        .join("\n");
    fsx::write_atomic(&config_file, unsigned.as_bytes(), 0o600).unwrap();
    assert!(client::config(&paths).unwrap().unwrap().signer.is_none());
    assert!(
        unix::get_value(&paths, "token").is_err(),
        "removing the pin cannot make signed state plaintext"
    );
    std::fs::rename(config_file, root.path().join("kept-config")).unwrap();
    assert!(
        unix::get_value(&paths, "token").is_err(),
        "unconfigured state cannot read a previous vault's file"
    );
}

#[cfg(unix)]
#[test]
fn plaintext_get_retains_local_reads_without_authenticated_evidence() {
    use crate::{client, fsx, materialize};
    let root = aterm_tempfile::tempdir().unwrap();
    let paths = client::Paths::for_home(&root.path().join("reader"));
    let source = root.path().join("plain.secrets");
    fsx::write_atomic(&source, b"token=plain-value\n", 0o600).unwrap();
    client::join(
        &paths,
        &client::JoinOptions {
            source: source.to_str().unwrap().into(),
            host: Some("reader".into()),
            interactive: false,
            wait: std::time::Duration::ZERO,
            confirm: &|_| false,
            password: client::JoinPassword::Given(None),
            fetched: None,
            requested: &|| {},
            say: &|_| {},
        },
    )
    .unwrap();
    assert_eq!(
        unix::get_value(&paths, "token").unwrap().as_slice(),
        b"plain-value"
    );
    let expected = client::config(&paths).unwrap().unwrap();
    assert!(
        client::read_verified_entry(&paths, &expected, "token")
            .unwrap()
            .is_none()
    );
    fsx::write_atomic(
        &materialize::target_path(&paths.home, "token").unwrap(),
        b"local-edit",
        0o600,
    )
    .unwrap();
    assert_eq!(
        unix::get_value(&paths, "token").unwrap().as_slice(),
        b"local-edit"
    );
}

fn words(line: &str) -> Vec<String> {
    line.split_whitespace().map(str::to_string).collect()
}

#[test]
fn bare_verb_is_status() {
    assert_eq!(parse(&[]), Ok(Command::Status));
    assert_eq!(parse(&words("status")), Ok(Command::Status));
    assert_eq!(parse(&words("status --json")), Ok(Command::StatusJson));
    assert!(parse(&words("status --json extra")).is_err());
}

#[test]
fn join_takes_one_source_and_its_switches() {
    assert_eq!(
        parse(&words("join http://h:1/v1/x --name m9 --no-agent")),
        Ok(Command::Join {
            source: "http://h:1/v1/x".into(),
            name: Some("m9".into()),
            agent: false,
            wait: true,
            password_stdin: false,
        })
    );
    assert_eq!(
        parse(&words("join")),
        Ok(Command::Join {
            source: String::new(),
            name: None,
            agent: true,
            wait: true,
            password_stdin: false,
        })
    );
    assert!(matches!(
        parse(&words("join --password-stdin")),
        Ok(Command::Join {
            password_stdin: true,
            ..
        })
    ));
    assert!(parse(&words("join a b")).is_err());
    assert!(parse(&words("join --name")).is_err());
}

#[test]
fn create_takes_one_source_and_its_switches() {
    assert_eq!(
        parse(&words(
            "create http://h:1/v1/x --import ~/.secrets --name m9 --no-agent"
        )),
        Ok(Command::Create {
            source: "http://h:1/v1/x".into(),
            import: Some("~/.secrets".into()),
            name: Some("m9".into()),
            agent: false,
            serve: false,
        })
    );
    assert_eq!(
        parse(&words("create /v/team.vault")),
        Ok(Command::Create {
            source: "/v/team.vault".into(),
            import: None,
            name: None,
            agent: true,
            serve: false,
        })
    );
    assert!(matches!(
        parse(&words("create http://127.0.0.1:7484/v1/alab --serve")),
        Ok(Command::Create { serve: true, .. })
    ));
    for refused in [
        "create",
        "create a --serve b",
        "create a b",
        "create a --no-wait",
        "create a --name",
    ] {
        assert!(parse(&words(refused)).is_err(), "{refused}");
    }
}

#[cfg(unix)]
#[test]
fn the_join_line_never_prints_private_url_parts() {
    let credential = "synthetic-join-url-credential";
    for url in [
        format!("https://vault.example/v1/team?token={credential}"),
        format!("http://127.0.0.1:7484/v1/team?token={credential}"),
        format!("https://user:{credential}@vault.example/v1/team"),
        format!("https://vault.example/v1/team#{credential}"),
    ] {
        let hint = unix::join_command(&url, "age1signer", Some("test.local"));
        assert!(!hint.contains(credential));
        assert!(hint.contains("<vault-url>#age1signer"));
        assert!(hint.contains("full configured URL"));
    }
}

#[cfg(unix)]
#[test]
fn the_join_line_names_this_mac_as_the_others_reach_it() {
    let signer = "age1signer";
    assert_eq!(
        unix::join_command("http://127.0.0.1:7484/v1/alab", signer, Some("m9.local")),
        "aterm secrets join http://m9.local:7484/v1/alab#age1signer"
    );
    assert_eq!(
        unix::join_command("http://localhost:7484/v1/alab", signer, Some("m9.local")),
        "aterm secrets join http://m9.local:7484/v1/alab#age1signer"
    );
    // Control: a host the other Macs already reach, and a loopback one with no LAN name,
    // stay as they are.
    assert_eq!(
        unix::join_command("http://vault.lan:7484/v1/alab", signer, Some("m9.local")),
        "aterm secrets join http://vault.lan:7484/v1/alab#age1signer"
    );
    assert_eq!(
        unix::join_command("http://127.0.0.1:7484/v1/alab", signer, None),
        "aterm secrets join http://127.0.0.1:7484/v1/alab#age1signer"
    );
}

#[cfg(unix)]
#[test]
fn server_on_waits_for_the_vault_it_serves_on_this_mac() {
    let settings = |line: &str| {
        let f = flags(&words(line), SERVE_VALUED, SERVE_SWITCHES).expect("flags");
        serve_settings(&f).expect("settings")
    };
    assert_eq!(
        unix::local_url(&settings("")),
        format!("http://127.0.0.1:{}/v1/alab", crate::DEFAULT_PORT)
    );
    let pin = format!("team={}=", "A".repeat(43));
    assert_eq!(
        unix::local_url(&settings(&format!("--listen 0.0.0.0:9001 --pin {pin}"))),
        "http://127.0.0.1:9001/v1/team"
    );
    assert_eq!(
        unix::local_url(&settings("--listen 10.0.0.7:9001")),
        "http://10.0.0.7:9001/v1/alab"
    );
}

/// `server on` returns once the job it started answers. Negative control: an address
/// nothing answers on ends at the hang.
#[cfg(unix)]
#[test]
fn server_on_returns_once_the_server_answers() {
    let store = aterm_tempfile::tempdir().expect("tempdir");
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let url = format!("http://{}/v1/alab", listener.local_addr().expect("addr"));
    let options = crate::server::Options {
        listen: listener.local_addr().expect("addr"),
        store: store.path().to_path_buf(),
        advertise: None,
        pins: Vec::new(),
        hold: std::time::Duration::from_secs(1),
        keep_awake: false,
    };
    let server = crate::server::start(listener, &options).expect("server starts");
    unix::answering(&url, std::time::Duration::from_secs(60)).expect("answered");
    server.stop();

    let silent = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let nobody = format!("http://{}/v1/alab", silent.local_addr().expect("addr"));
    drop(silent);
    let error =
        unix::answering(&nobody, std::time::Duration::from_millis(300)).expect_err("unanswered");
    assert!(error.to_string().contains("did not answer"), "{error}");
}

#[test]
fn set_never_takes_a_value_on_the_command_line() {
    let error = parse(&words("set token hunter2")).expect_err("refused");
    assert!(!error.contains("hunter2"), "{error}");
    assert_eq!(
        parse(&words("set token --for m3 --for m22 --for m3")),
        Ok(Command::Set {
            name: "token".into(),
            hosts: vec!["m22".into(), "m3".into()],
            file: None,
            fill: Fill::default(),
        })
    );
    assert_eq!(
        parse(&words("set ~/.aterm/machine.key --file /k --if-absent")),
        Ok(Command::Set {
            name: "~/.aterm/machine.key".into(),
            hosts: vec![],
            file: Some("/k".into()),
            fill: Fill {
                if_absent: true,
                ..Fill::default()
            },
        })
    );
}

#[test]
fn set_with_names_the_anchor_of_an_if_absent_entry() {
    assert_eq!(
        parse(&words(
            "set ~/.aterm/release-credentials.toml --if-absent --with ~/.aterm/machine.key --file /p"
        )),
        Ok(Command::Set {
            name: "~/.aterm/release-credentials.toml".into(),
            hosts: vec![],
            file: Some("/p".into()),
            fill: Fill {
                if_absent: true,
                with: Some("~/.aterm/machine.key".into()),
                ..Fill::default()
            },
        })
    );
    assert!(matches!(
        parse(&words("set a --if-absent --no-with --fleet-reads-anchors")),
        Ok(Command::Set {
            fill: Fill {
                if_absent: true,
                with: None,
                no_with: true,
                fleet_reads_anchors: true,
            },
            ..
        })
    ));
    for refused in [
        "set a --with b",
        "set a --if-absent --with",
        "set a --if-absent --with a",
        "set a --if-absent --with ../b",
        "set a --no-with",
        "set a --if-absent --with b --no-with",
    ] {
        assert!(parse(&words(refused)).is_err(), "{refused}");
    }
    let error = parse(&words("set a --if-absent --with hunter2!")).expect_err("refused");
    assert!(!error.contains("hunter2"), "{error}");
}

#[cfg(unix)]
const KEY: &str = "~/.aterm/machine.key";
#[cfg(unix)]
const PROFILE: &str = "~/.aterm/release-credentials.toml";

/// A vault of one machine holding the release key and profile as if-absent entries, with
/// no anchor yet, as the live one does.
#[cfg(unix)]
fn unanchored_vault() -> crate::text::Document {
    let recipient = crate::age::Identity::generate()
        .expect("a key")
        .recipient()
        .to_bech32();
    let mut doc = crate::text::Document::new_vault("m22", &recipient);
    for name in [KEY, PROFILE] {
        doc.set_entry_with(&crate::text::Audience::Everyone, name, b"v\n", true, None)
            .expect("if-absent");
    }
    doc
}

/// A key let go from a closed vault is marked as one, never as code only: a password or a
/// code still lets it in there.
#[cfg(unix)]
#[test]
fn a_closed_vault_marks_a_key_let_go_but_never_code_only() {
    let key = crate::age::Identity::generate()
        .expect("a key")
        .recipient()
        .to_bech32();
    let mut doc = crate::text::Document::new_vault("m22", &key);
    assert!(!doc.open(), "CONTROL: a new vault text is closed");
    assert!(!unix::held_mark(&doc).contains("code only"));
    assert!(unix::held_mark(&doc).contains("a machine let go"));
    doc.set_open(true).expect("open");
    assert!(unix::held_mark(&doc).contains("code only"));
}

#[cfg(unix)]
#[test]
fn opening_never_offers_a_machine_let_go() {
    let key = || {
        crate::age::Identity::generate()
            .expect("a key")
            .recipient()
            .to_bech32()
    };
    let (gone, asking) = (key(), key());
    let mut doc = crate::text::Document::new_vault("m22", &key());
    doc.add_code_only(&gone).expect("let go");
    let request = |recipient: &str, host: &str| crate::client::Request {
        recipient: recipient.to_owned(),
        host: host.to_owned(),
        code: String::new(),
        at: None,
        allowed: None,
        proof: None,
    };
    let mut said = Vec::new();
    unix::admit_on_opening(
        &mut doc,
        vec![request(&gone, "laptop")],
        &mut |line| said.push(line),
        &mut |_| panic!("asked to let in a machine let go"),
    )
    .expect("named");
    assert!(
        said.iter()
            .any(|line| line.contains("laptop") && line.contains("code only")),
        "{said:?}"
    );
    unix::admit_on_opening(
        &mut doc,
        vec![request(&gone, "laptop"), request(&asking, "m7")],
        &mut |_| {},
        &mut |_| true,
    )
    .expect("let in");
    let members: Vec<String> = doc.machines().into_iter().map(|m| m.recipient).collect();
    assert!(
        members.contains(&asking) && !members.contains(&gone),
        "{members:?}"
    );
    assert_eq!(doc.code_only(), [gone]);

    let (moved, doubled) = (key(), key());
    let mut asked = Vec::new();
    unix::admit_on_opening(
        &mut doc,
        vec![
            request(&moved, "m8"),
            request(&doubled, "m9"),
            request(&moved, "m8"),
            request(&doubled, "evil"),
        ],
        &mut |line| said.push(line),
        &mut |question| {
            asked.push(question.to_owned());
            true
        },
    )
    .expect("one yes lets in the machine that asked from two addresses");
    assert!(asked[0].starts_with("Let these 1 in now?"), "{asked:?}");
    let machines = doc.machines();
    assert_eq!(
        machines.iter().filter(|m| m.recipient == moved).count(),
        1,
        "{machines:?}"
    );
    assert!(!machines.iter().any(|m| m.recipient == doubled));
    assert!(
        said.iter()
            .any(|line| line.contains("evil") && line.contains(&doubled)),
        "{said:?}"
    );
}

#[cfg(unix)]
#[test]
fn setting_an_entry_again_keeps_how_it_fills_unless_told() {
    let everyone = crate::text::Audience::Everyone;
    let anchor_of = |doc: &crate::text::Document, name: &str| {
        let entry = doc.entry(&everyone, name).expect("set");
        (entry.if_absent, entry.anchor.clone())
    };
    let gaps = |with: Option<&str>, no_with: bool| Fill {
        if_absent: true,
        with: with.map(str::to_string),
        no_with,
        fleet_reads_anchors: true,
    };
    let (doc, anchor) = unix::set_in(
        &unanchored_vault(),
        &everyone,
        PROFILE,
        b"v\n",
        &gaps(Some(KEY), false),
    )
    .expect("anchored");
    assert_eq!(anchor.as_deref(), Some(KEY));

    let (doc, anchor) =
        unix::set_in(&doc, &everyone, PROFILE, b"w\n", &gaps(None, false)).expect("rotated");
    assert_eq!(anchor.as_deref(), Some(KEY));
    assert_eq!(anchor_of(&doc, PROFILE), (true, Some(KEY.to_string())));
    assert_eq!(
        doc.entry(&everyone, PROFILE).expect("set").value.as_slice(),
        b"w\n"
    );

    let error =
        unix::set_in(&doc, &everyone, PROFILE, b"x\n", &Fill::default()).expect_err("refused");
    assert!(error.0.contains(&format!("if-absent={KEY}")), "{error}");
    let error = unix::set_in(&doc, &everyone, KEY, b"x\n", &Fill::default()).expect_err("refused");
    assert!(error.0.contains("--if-absent"), "{error}");

    let (dropped, anchor) =
        unix::set_in(&doc, &everyone, PROFILE, b"w\n", &gaps(None, true)).expect("dropped");
    assert_eq!(anchor, None);
    assert_eq!(anchor_of(&dropped, PROFILE), (true, None));
    let (doc, anchor) = unix::set_in(
        &doc,
        &everyone,
        "~/.aterm/other",
        b"o\n",
        &gaps(None, false),
    )
    .expect("new");
    assert_eq!(anchor, None);
    assert_eq!(anchor_of(&doc, "~/.aterm/other"), (true, None));
}

#[cfg(unix)]
#[test]
fn the_vaults_first_anchor_waits_for_the_fleet() {
    let everyone = crate::text::Audience::Everyone;
    let doc = unanchored_vault();
    let with_key = |fleet_reads_anchors| Fill {
        if_absent: true,
        with: Some(KEY.to_string()),
        no_with: false,
        fleet_reads_anchors,
    };
    let error =
        unix::set_in(&doc, &everyone, PROFILE, b"v\n", &with_key(false)).expect_err("refused");
    assert!(error.0.contains("--fleet-reads-anchors"), "{error}");
    let (doc, _) = unix::set_in(&doc, &everyone, PROFILE, b"v\n", &with_key(true))
        .expect("the fleet reads anchors");
    assert!(doc.anchored());
    let toml = Fill {
        if_absent: true,
        with: Some(KEY.to_string()),
        ..Fill::default()
    };
    unix::set_in(&doc, &everyone, "~/.aterm/machine.toml", b"v\n", &toml).expect("a later anchor");
    unix::set_in(&doc, &everyone, "plain", b"v", &Fill::default()).expect("a plain entry");
}

/// Anyone on the LAN can read a waiting machine's key from the listing and queue it from
/// its own address under a name of its choosing, beside the machine's line.
#[cfg(unix)]
#[test]
fn approve_takes_no_key_listed_under_two_names() {
    let (code_k, code_j) = ("aaaa-bbbb-cccc-dddd", "eeee-ffff-gggg-hhhh");
    let request = |recipient: &str, host: &str, code: &str| crate::client::Request {
        recipient: recipient.into(),
        host: host.into(),
        code: code.into(),
        at: None,
        allowed: None,
        proof: None,
    };
    let pair = |k: &str, h: &str| (k.to_string(), h.to_string());
    let pending = [
        request("age1k", "m30", code_k),
        request("age1j", "m31", code_j),
        request("age1k", "a", code_k),
    ];
    assert_eq!(
        unix::from_queue(&pending, &[code_j.into()], false),
        Ok(vec![pair("age1j", "m31")])
    );
    for (which, all) in [(vec![code_k.to_string()], false), (Vec::new(), true)] {
        let refused = unix::from_queue(&pending, &which, all).expect_err("two names");
        assert!(
            refused.0.contains("(a, m30)") && refused.0.contains("approve age1k --name"),
            "{refused:?}"
        );
    }
    let moved = [
        request("age1k", "m30", code_k),
        request("age1k", "m30", code_k),
    ];
    assert_eq!(
        unix::from_queue(&moved, &[], true),
        Ok(vec![pair("age1k", "m30")])
    );
    assert_eq!(
        unix::from_queue(&moved, &[code_k.into(), code_k.into()], false),
        Ok(vec![pair("age1k", "m30")])
    );
    assert!(unix::from_queue(&moved, &[code_j.into()], false).is_err());
}

#[cfg(unix)]
#[test]
fn bulk_approval_refuses_an_incomplete_snapshot_before_asking() {
    use crate::client;

    let _answering = client::ServersAnswering::set();
    let root = aterm_tempfile::tempdir().unwrap();
    let (paths, primary, mirror, _) = client::mirrored_vault_for_test(root.path(), false);
    let crate::transport::Source::Url(primary_url) =
        client::config(&paths).unwrap().unwrap().source
    else {
        unreachable!()
    };
    let (_, line) = client::queued_identity_for_test(root.path(), "reachable");
    crate::transport::post_join(&primary_url, &line).unwrap();
    let before = client::admin_open(&paths).unwrap();
    let recorded = client::approved_recipients(&paths);
    mirror.stop();

    let (mut shown, mut questions) = (Vec::new(), 0);
    let (mut say, mut agree) = (
        |line| shown.push(line),
        |_: &str| {
            questions += 1;
            true
        },
    );
    let approved =
        unix::approve_with_terminal(&paths, &[], true, None, Some((&mut say, &mut agree)));
    let after = client::admin_open(&paths);
    let after_recorded = client::approved_recipients(&paths);
    primary.stop();

    assert_eq!(
        approved.unwrap_err().kind(),
        crate::ErrorKind::JoinQueueIncomplete
    );
    assert_eq!(
        questions, 0,
        "an incomplete all-queue view cannot ask for approval"
    );
    assert!(shown.is_empty());
    let after = after.unwrap();
    assert_eq!(after.seq(), before.seq());
    assert_eq!(after.doc.render(), before.doc.render());
    assert_eq!(after_recorded, recorded);
}

#[cfg(unix)]
#[test]
fn bulk_approval_displays_and_admits_a_replicated_identity_once() {
    use crate::client::{self, AdmissionMethod};

    let _answering = client::ServersAnswering::set();
    let root = aterm_tempfile::tempdir().unwrap();
    let (paths, primary, mirror, mirror_url) = client::mirrored_vault_for_test(root.path(), false);
    let crate::transport::Source::Url(primary_url) =
        client::config(&paths).unwrap().unwrap().source
    else {
        unreachable!()
    };
    let (recipient, line) = client::queued_identity_for_test(root.path(), "replicated");
    for (url, tag) in [(&primary_url, 1), (&mirror_url, 2)] {
        let proof = crate::b64::encode_padded(&[tag; 32]);
        crate::transport::post_join(url, &format!("{line} proof={proof}")).unwrap();
    }
    let (mut shown, mut counts) = (Vec::new(), Vec::new());
    let (mut say, mut agree) = (
        |line| shown.push(line),
        |question: &str| {
            counts.extend(
                question
                    .split_whitespace()
                    .filter_map(|word| word.parse::<usize>().ok()),
            );
            true
        },
    );
    let approved =
        unix::approve_with_terminal(&paths, &[], true, None, Some((&mut say, &mut agree)));
    let after = client::admin_open(&paths);
    let recorded = client::approved_recipients(&paths);
    primary.stop();
    mirror.stop();

    assert_eq!(approved.unwrap(), std::process::ExitCode::SUCCESS);
    assert_eq!(counts, [1]);
    assert_eq!(shown.len(), 1);
    assert!(shown[0].contains(&recipient));
    let after = after.unwrap();
    assert_eq!(
        after
            .doc
            .machines()
            .iter()
            .filter(|machine| machine.recipient == recipient)
            .count(),
        1
    );
    assert_eq!(
        after.commons_admission("replicated").unwrap().method,
        AdmissionMethod::Manual
    );
    assert_eq!(
        recorded,
        [recipient.clone(), recipient],
        "both distinct proof observations survive bookkeeping"
    );
}

#[cfg(unix)]
#[test]
fn bulk_approval_keeps_answers_for_known_members_omitted_from_the_prompt() {
    use crate::client::{self, AdmissionMethod};

    let _answering = client::ServersAnswering::set();
    let root = aterm_tempfile::tempdir().unwrap();
    let (paths, primary, mirror, mirror_url) = client::mirrored_vault_for_test(root.path(), false);
    let crate::transport::Source::Url(primary_url) =
        client::config(&paths).unwrap().unwrap().source
    else {
        unreachable!()
    };
    let (known, known_line) = client::queued_identity_for_test(root.path(), "known");
    crate::transport::post_join(&primary_url, &known_line).unwrap();
    client::approve_request(&paths, &known).unwrap();
    let before = client::admin_open(&paths).unwrap();
    let known_admission = before.commons_admission("known").unwrap();
    assert_eq!(
        client::approved_recipients(&paths).as_slice(),
        std::slice::from_ref(&known)
    );
    let (pending, pending_line) = client::queued_identity_for_test(root.path(), "pending");
    crate::transport::post_join(&mirror_url, &pending_line).unwrap();

    let mut shown = Vec::new();
    let (mut say, mut agree) = (|line| shown.push(line), |_: &str| true);
    let approved =
        unix::approve_with_terminal(&paths, &[], true, None, Some((&mut say, &mut agree)));
    let after = client::admin_open(&paths);
    let recorded = client::approved_recipients(&paths);
    primary.stop();
    mirror.stop();

    assert_eq!(approved.unwrap(), std::process::ExitCode::SUCCESS);
    assert_eq!(shown.len(), 1);
    assert!(shown[0].contains(&pending) && !shown[0].contains(&known));
    let after = after.unwrap();
    assert_eq!(after.commons_admission("known").unwrap(), known_admission);
    assert_eq!(
        after.commons_admission("pending").unwrap().method,
        AdmissionMethod::Manual
    );
    assert_eq!(
        recorded
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>(),
        std::collections::BTreeSet::from([known, pending])
    );
}

#[cfg(unix)]
#[test]
fn bulk_approval_decline_and_missing_terminal_leave_the_vault_unchanged() {
    use crate::client;

    let _answering = client::ServersAnswering::set();
    let root = aterm_tempfile::tempdir().unwrap();
    let (paths, primary, mirror, mirror_url) = client::mirrored_vault_for_test(root.path(), false);
    let (_, line) = client::queued_identity_for_test(root.path(), "waiting");
    crate::transport::post_join(&mirror_url, &line).unwrap();
    let before = client::admin_open(&paths).unwrap();
    let recorded = client::approved_recipients(&paths);

    let mut questions = 0;
    let (mut say, mut decline) = (
        |_: String| {},
        |_: &str| {
            questions += 1;
            false
        },
    );
    let declined =
        unix::approve_with_terminal(&paths, &[], true, None, Some((&mut say, &mut decline)));
    let after_decline = client::admin_open(&paths);
    let missing_terminal = unix::approve_with_terminal(&paths, &[], true, None, None);
    let after_missing = client::admin_open(&paths);
    let after_recorded = client::approved_recipients(&paths);
    primary.stop();
    mirror.stop();

    assert_eq!(declined.unwrap(), std::process::ExitCode::FAILURE);
    assert_eq!(questions, 1);
    assert!(missing_terminal.is_err());
    for after in [after_decline, after_missing] {
        let after = after.unwrap();
        assert_eq!(after.seq(), before.seq());
        assert_eq!(after.doc.render(), before.doc.render());
    }
    assert_eq!(after_recorded, recorded);
}

#[cfg(unix)]
#[test]
fn bulk_approval_refuses_conflicting_names_across_queues_without_asking() {
    use crate::client;

    let _answering = client::ServersAnswering::set();
    let root = aterm_tempfile::tempdir().unwrap();
    let (paths, primary, mirror, mirror_url) = client::mirrored_vault_for_test(root.path(), false);
    let crate::transport::Source::Url(primary_url) =
        client::config(&paths).unwrap().unwrap().source
    else {
        unreachable!()
    };
    let (recipient, line) = client::queued_identity_for_test(root.path(), "first-name");
    let code = client::join_code(&crate::age::Recipient::parse(&recipient).unwrap());
    crate::transport::post_join(&primary_url, &line).unwrap();
    crate::transport::post_join(&mirror_url, &format!("{recipient} second-name {code}")).unwrap();
    let before = client::admin_open(&paths).unwrap();

    let (mut shown, mut questions) = (Vec::new(), 0);
    let (mut say, mut agree) = (
        |line| shown.push(line),
        |_: &str| {
            questions += 1;
            true
        },
    );
    let approved =
        unix::approve_with_terminal(&paths, &[], true, None, Some((&mut say, &mut agree)));
    let after = client::admin_open(&paths);
    let recorded = client::approved_recipients(&paths);
    primary.stop();
    mirror.stop();

    assert!(approved.is_err());
    assert_eq!(questions, 0);
    assert!(shown.is_empty());
    let after = after.unwrap();
    assert_eq!(after.seq(), before.seq());
    assert_eq!(after.doc.render(), before.doc.render());
    assert!(recorded.is_empty());
}

#[cfg(unix)]
#[test]
fn explicit_approval_can_record_existing_recipient_without_duplicating_machine() {
    use crate::client::{self, AdmissionMethod, Paths};
    let root = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(&root.path().join("m1"));
    let source = root.path().join("manual.vault");
    client::create(&paths, source.to_str().unwrap(), None, Some("m1")).unwrap();
    let recipient = crate::age::Identity::from_secret_bytes([38; 32])
        .recipient()
        .to_bech32();
    for expected_seq in [2, 3] {
        assert_eq!(
            unix::approve(
                &paths,
                std::slice::from_ref(&recipient),
                false,
                Some("m2".into())
            )
            .unwrap(),
            std::process::ExitCode::SUCCESS
        );
        let admin = client::admin_open(&paths).unwrap();
        assert_eq!(admin.doc.machines().len(), 2);
        let evidence = admin.commons_admission("m2").unwrap();
        assert_eq!(evidence.sequence, expected_seq);
        assert_eq!(evidence.method, AdmissionMethod::Manual);
    }
    assert!(unix::approve(&paths, &[recipient], false, Some("wrong-host".into())).is_err());
    assert_eq!(client::admin_open(&paths).unwrap().seq(), 3);
}

/// `password open` at a terminal records each machine it lets in as an open admission
/// (`Open`; the prompt checks no code), so in a vault with Commons control the same publish
/// puts it in the control entry's audience.
/// CONTROL: a machine a plain publish adds gets no decision and stays outside, opening with
/// no terminal lets nothing in, and a closed vault records no open admission.
#[cfg(unix)]
#[test]
fn opening_at_a_terminal_records_each_machine_it_lets_in_as_an_open_admission() {
    use crate::client::{self, AdmissionMethod};
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let paths = admin_of_a_path_vault(root.path());
    let key = |n: u8| {
        crate::age::Identity::from_secret_bytes([n; 32])
            .recipient()
            .to_bech32()
    };
    let request = |recipient: &str, host: &str| client::Request {
        recipient: recipient.to_owned(),
        host: host.to_owned(),
        code: String::new(),
        at: None,
        allowed: None,
        proof: None,
    };
    let in_audience = |admin: &client::Admin, host: &str| {
        matches!(
            admin.doc.commons_audience(),
            Some(crate::text::Audience::Hosts(hosts)) if hosts.iter().any(|h| h == host)
        )
    };
    // Closed, with a machine a plain publish adds: no decision, outside the audience.
    let admin = client::admin_open(&paths).expect("open");
    assert!(admin.doc.commons_audience().is_some(), "Commons control");
    let mut doc = admin.doc.clone();
    doc.set_open(false).expect("close");
    doc.add_machine("m8", &key(48)).expect("machine");
    client::admin_publish(&paths, &admin, &doc).expect("publish");
    let admin = client::admin_open(&paths).expect("open");
    assert!(!admin.doc.open());
    assert!(
        admin.commons_admission("m8").is_none() && !in_audience(&admin, "m8"),
        "CONTROL: a plain publish records no decision"
    );
    let mut closed = admin.doc.clone();
    closed.add_machine("m9", &key(49)).expect("machine");
    let refused = client::admin_publish_opened(&paths, &admin, &closed, &[key(49)])
        .expect_err("a closed vault records no open admission");
    assert!(refused.0.contains("open vault"), "{refused}");

    // Opened with no terminal: open, and nothing let in.
    let mut doc = admin.doc.clone();
    unix::open_vault(
        &paths,
        &admin,
        &mut doc,
        vec![request(&key(47), "m7")],
        None,
    )
    .expect("opened");
    let admin = client::admin_open(&paths).expect("open");
    assert!(admin.doc.open());
    assert!(!admin.doc.machines().iter().any(|m| m.recipient == key(47)));

    // Closed again, then opened at a terminal whose person lets m7 in.
    let mut doc = admin.doc.clone();
    doc.set_open(false).expect("close");
    client::admin_publish(&paths, &admin, &doc).expect("publish");
    let admin = client::admin_open(&paths).expect("open");
    let mut doc = admin.doc.clone();
    let (mut say, mut agree) = (|_: String| {}, |_: &str| true);
    unix::open_vault(
        &paths,
        &admin,
        &mut doc,
        vec![request(&key(47), "m7")],
        Some((&mut say, &mut agree)),
    )
    .expect("opened");
    let admin = client::admin_open(&paths).expect("open");
    assert!(admin.doc.open());
    let evidence = admin
        .commons_admission("m7")
        .expect("a decision for the machine let in");
    assert_eq!(evidence.method, AdmissionMethod::Open);
    assert!(
        in_audience(&admin, "m7"),
        "the same publish shares the control entry with it"
    );
    assert!(admin.commons_admission("m8").is_none() && !in_audience(&admin, "m8"));
}

/// `approve <code>` records the request it lets in for this Mac's approver, as the notice's
/// Approve does, so an approver away while that Mac is in shows no ghost once it is let go.
#[cfg(unix)]
#[test]
fn approving_by_code_records_the_request_for_this_macs_approver() {
    use crate::client::{self, Paths};

    let _answering = client::ServersAnswering::set();
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let server = crate::server::start(
        std::net::TcpListener::bind("127.0.0.1:0").expect("bind"),
        &crate::server::Options {
            listen: "127.0.0.1:0".parse().expect("addr"),
            store: root.path().join("store"),
            advertise: None,
            pins: Vec::new(),
            hold: std::time::Duration::from_millis(200),
            keep_awake: false,
        },
    )
    .expect("a loopback server");
    let paths = Paths::for_home(&root.path().join("m1"));
    let url = format!("http://{}/v1/team", server.addr);
    client::create(&paths, &url, None, Some("m1")).expect("create");
    let recipient = crate::age::Identity::from_secret_bytes([38; 32]).recipient();
    let code = client::join_code(&recipient);
    let key = recipient.to_bech32();
    crate::transport::post_join(&url, &format!("{key} m2 {code}")).expect("queued");
    assert!(
        client::approved_recipients(&paths).is_empty(),
        "CONTROL: nothing recorded yet"
    );
    assert_eq!(
        unix::approve(&paths, &[code], false, None).expect("approved"),
        std::process::ExitCode::SUCCESS
    );
    assert_eq!(client::approved_recipients(&paths), [key]);
    server.stop();
}

#[cfg(unix)]
#[test]
fn approving_by_code_records_admission_for_an_existing_document_recipient() {
    use crate::client::{self, AdmissionMethod, Paths};

    let _answering = client::ServersAnswering::set();
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let server = crate::server::start(
        std::net::TcpListener::bind("127.0.0.1:0").expect("bind"),
        &crate::server::Options {
            listen: "127.0.0.1:0".parse().expect("addr"),
            store: root.path().join("store"),
            advertise: None,
            pins: Vec::new(),
            hold: std::time::Duration::from_millis(200),
            keep_awake: false,
        },
    )
    .expect("a loopback server");
    let paths = Paths::for_home(&root.path().join("m1"));
    let url = format!("http://{}/v1/team", server.addr);
    client::create(&paths, &url, None, Some("m1")).expect("create");
    let recipient = crate::age::Identity::from_secret_bytes([38; 32]).recipient();
    let code = client::join_code(&recipient);
    let key = recipient.to_bech32();
    crate::transport::post_join(&url, &format!("{key} m2 {code}")).expect("queued");

    let admin = client::admin_open(&paths).expect("open");
    let mut doc = admin.doc.clone();
    doc.add_machine("m2", &key).expect("document recipient");
    client::admin_publish(&paths, &admin, &doc).expect("plain publish");
    let admin = client::admin_open(&paths).expect("open");
    assert!(
        admin.commons_admission("m2").is_none(),
        "a plain document edit grants no admission"
    );
    assert!(
        client::requests(&paths, Some(&admin.doc))
            .expect("unpublished requests")
            .is_empty(),
        "bulk approval has no unpublished recipient"
    );

    assert_eq!(
        unix::approve(&paths, &[code], false, None).expect("approve existing recipient"),
        std::process::ExitCode::SUCCESS
    );
    let admin = client::admin_open(&paths).expect("open");
    assert_eq!(admin.doc.machines().len(), 2);
    assert_eq!(
        admin.commons_admission("m2").expect("approved").method,
        AdmissionMethod::Manual
    );
    assert_eq!(client::approved_recipients(&paths), [key]);
    server.stop();
}

#[cfg(unix)]
#[test]
fn a_direct_key_approval_preserves_earlier_queue_answers() {
    use crate::client::{self, AdmissionMethod};

    let _answering = client::ServersAnswering::set();
    let root = aterm_tempfile::tempdir().unwrap();
    let (paths, primary, mirror, mirror_url) = client::mirrored_vault_for_test(root.path(), false);
    let (earlier, line) = client::queued_identity_for_test(root.path(), "earlier-queue-request");
    crate::transport::post_join(&mirror_url, &line).unwrap();
    client::approve_request(&paths, &earlier).unwrap();
    let recorded_before = client::approved_recipients(&paths);
    assert_eq!(recorded_before, [earlier]);
    let (direct, _) = client::queued_identity_for_test(root.path(), "direct-key");

    let approved = unix::approve(
        &paths,
        std::slice::from_ref(&direct),
        false,
        Some("direct-key".into()),
    );
    let after = client::admin_open(&paths);
    let recorded_after = client::approved_recipients(&paths);
    primary.stop();
    mirror.stop();

    assert_eq!(approved.unwrap(), std::process::ExitCode::SUCCESS);
    let after = after.unwrap();
    assert!(
        after
            .doc
            .machines()
            .iter()
            .any(|machine| machine.recipient == direct && machine.name == "direct-key")
    );
    assert_eq!(
        after.commons_admission("direct-key").unwrap().method,
        AdmissionMethod::Manual
    );
    assert_eq!(
        recorded_after, recorded_before,
        "an approval that observed no queue leaves prior answers intact"
    );
}

#[cfg(unix)]
#[test]
fn bulk_approval_includes_every_verified_queue() {
    use crate::client::{self, AdmissionMethod};

    let _answering = client::ServersAnswering::set();
    let root = aterm_tempfile::tempdir().unwrap();
    let (paths, primary, mirror, mirror_url) = client::mirrored_vault_for_test(root.path(), false);
    let crate::transport::Source::Url(primary_url) =
        client::config(&paths).unwrap().unwrap().source
    else {
        unreachable!()
    };
    let (primary_key, primary_line) = client::queued_identity_for_test(root.path(), "primary-only");
    let (mirror_key, mirror_line) = client::queued_identity_for_test(root.path(), "mirror-only");
    crate::transport::post_join(&primary_url, &primary_line).unwrap();
    crate::transport::post_join(&mirror_url, &mirror_line).unwrap();

    let mut confirmations = 0;
    let (mut say, mut agree) = (
        |_: String| {},
        |_: &str| {
            confirmations += 1;
            true
        },
    );
    let approved =
        unix::approve_with_terminal(&paths, &[], true, None, Some((&mut say, &mut agree)));
    let after = client::admin_open(&paths);
    primary.stop();
    mirror.stop();

    assert_eq!(approved.unwrap(), std::process::ExitCode::SUCCESS);
    assert_eq!(confirmations, 1);
    let after = after.unwrap();
    let expected = std::collections::BTreeSet::from([primary_key, mirror_key]);
    let admitted: std::collections::BTreeSet<_> = after
        .doc
        .machines()
        .into_iter()
        .map(|machine| machine.recipient)
        .filter(|recipient| expected.contains(recipient))
        .collect();
    assert_eq!(admitted, expected);
    for machine in after.doc.machines() {
        if expected.contains(&machine.recipient) {
            assert_eq!(
                after.commons_admission(&machine.name).unwrap().method,
                AdmissionMethod::Manual
            );
        }
    }
}

#[cfg(unix)]
#[test]
fn a_listed_code_is_approved_when_another_queue_gets_the_next_turn() {
    use crate::client::{self, AdmissionMethod};
    let _answering = client::ServersAnswering::set();
    let root = aterm_tempfile::tempdir().unwrap();
    let (paths, primary, mirror, mirror_url) = client::mirrored_vault_for_test(root.path(), false);
    let crate::transport::Source::Url(primary_url) =
        client::config(&paths).unwrap().unwrap().source
    else {
        unreachable!()
    };
    let (primary_key, primary_line) = client::queued_identity_for_test(root.path(), "primary-only");
    let (mirror_key, mirror_line) = client::queued_identity_for_test(root.path(), "mirror-only");
    crate::transport::post_join(&primary_url, &primary_line).unwrap();
    crate::transport::post_join(&mirror_url, &mirror_line).unwrap();
    let admin = client::admin_open(&paths).unwrap();
    let inspection = client::inspect_requests(&paths, Some(&admin.doc)).unwrap();
    let listing = unix::requests_json(&inspection, Some(&admin.doc));
    assert_eq!(inspection.requests.len(), 1);
    let listed = &inspection.requests[0];
    assert_eq!(
        listing["requests"][0]["code"].as_str(),
        Some(listed.code.as_str())
    );
    assert_eq!(
        listing["requests"][0]["recipient"].as_str(),
        Some(listed.recipient.as_str())
    );
    let other = if listed.recipient == primary_key {
        mirror_key
    } else {
        assert_eq!(listed.recipient, mirror_key);
        primary_key
    };
    let approved = unix::approve(&paths, std::slice::from_ref(&listed.code), false, None);
    let after = client::admin_open(&paths);
    primary.stop();
    mirror.stop();
    assert_eq!(approved.unwrap(), std::process::ExitCode::SUCCESS);
    let after = after.unwrap();
    let member = after
        .doc
        .machines()
        .into_iter()
        .find(|machine| machine.recipient == listed.recipient)
        .expect("the listed recipient was approved");
    assert_eq!(
        after.commons_admission(&member.name).unwrap().method,
        AdmissionMethod::Manual
    );
    assert!(
        !after
            .doc
            .machines()
            .iter()
            .any(|machine| machine.recipient == other)
    );
}

#[cfg(unix)]
fn set_opening_queue_policy(
    paths: &crate::client::Paths,
    mirror: &str,
    audience: crate::text::Audience,
    conditional: bool,
) {
    let admin = crate::client::admin_open(paths).unwrap();
    let policy = admin
        .doc
        .entry(&crate::text::Audience::Everyone, crate::SWARM_POLICY_ENTRY)
        .unwrap()
        .value
        .as_slice()
        .to_vec();
    let mut doc = admin.doc.clone();
    assert!(
        doc.unset_entry(&crate::text::Audience::Everyone, crate::SWARM_POLICY_ENTRY)
            .unwrap()
    );
    doc.set_entry_with(
        &audience,
        crate::SWARM_POLICY_ENTRY,
        &policy,
        conditional,
        None,
    )
    .unwrap();
    crate::client::admin_publish(paths, &admin, &doc).unwrap();
    crate::transport::publish(
        &crate::transport::Source::Url(mirror.to_owned()),
        &std::fs::read(paths.cache_file()).unwrap(),
    )
    .unwrap();
}

#[cfg(unix)]
fn assert_declined_opening_holds_all_queues(host_scoped: bool) {
    use crate::client;
    use std::collections::BTreeSet;

    let _answering = client::ServersAnswering::set();
    let root = aterm_tempfile::tempdir().unwrap();
    let (paths, primary, mirror, mirror_url) = client::mirrored_vault_for_test(root.path(), true);
    let crate::transport::Source::Url(primary_url) =
        client::config(&paths).unwrap().unwrap().source
    else {
        unreachable!()
    };
    if host_scoped {
        set_opening_queue_policy(
            &paths,
            &mirror_url,
            crate::text::Audience::Hosts(vec!["admin".into()]),
            false,
        );
    }
    let (primary_key, primary_line) = client::queued_identity_for_test(root.path(), "primary-only");
    let (mirror_key, mirror_line) = client::queued_identity_for_test(root.path(), "mirror-only");
    crate::transport::post_join(&primary_url, &primary_line).unwrap();
    crate::transport::post_join(&mirror_url, &mirror_line).unwrap();

    let admin = client::admin_open(&paths).unwrap();
    assert!(!admin.doc.open());
    let waiting = client::requests(&paths, Some(&admin.doc)).unwrap();
    assert!(
        !waiting.is_empty(),
        "the opening prompt has a request to decline"
    );
    let mut doc = admin.doc.clone();
    let mut questions = 0;
    let mut say = |_: String| {};
    let mut decline = |_: &str| {
        questions += 1;
        false
    };
    let opened = unix::open_vault(
        &paths,
        &admin,
        &mut doc,
        waiting,
        Some((&mut say, &mut decline)),
    );
    let after = client::admin_open(&paths);
    primary.stop();
    mirror.stop();

    opened.unwrap();
    assert_eq!(
        questions, 1,
        "opening explicitly declined the offered requests"
    );
    let after = after.unwrap();
    assert!(after.doc.open());
    assert_eq!(
        after.doc.code_only().into_iter().collect::<BTreeSet<_>>(),
        BTreeSet::from([primary_key.clone(), mirror_key.clone()]),
        "opening must keep both waiting identities behind explicit code approval"
    );
    assert!(
        after
            .doc
            .machines()
            .iter()
            .all(|machine| { machine.recipient != primary_key && machine.recipient != mirror_key }),
        "declining the prompt admits neither waiting identity"
    );
}

#[cfg(unix)]
#[test]
fn opening_after_declining_requests_holds_every_verified_queue() {
    assert_declined_opening_holds_all_queues(false);
}

#[cfg(unix)]
#[test]
fn opening_holds_queues_from_a_host_scoped_policy() {
    assert_declined_opening_holds_all_queues(true);
}

#[cfg(unix)]
#[test]
fn opening_refuses_a_conditional_queue_policy_without_publication() {
    use crate::client;

    let _answering = client::ServersAnswering::set();
    let root = aterm_tempfile::tempdir().unwrap();
    let (paths, primary, mirror, mirror_url) = client::mirrored_vault_for_test(root.path(), true);
    set_opening_queue_policy(&paths, &mirror_url, crate::text::Audience::Everyone, true);
    let crate::transport::Source::Url(primary_url) =
        client::config(&paths).unwrap().unwrap().source
    else {
        unreachable!()
    };
    for (url, host) in [(&primary_url, "primary-only"), (&mirror_url, "mirror-only")] {
        let (_, line) = client::queued_identity_for_test(root.path(), host);
        crate::transport::post_join(url, &line).unwrap();
    }
    let admin = client::admin_open(&paths).unwrap();
    assert!(!admin.doc.open());
    let before = admin.doc.render();
    let mut doc = admin.doc.clone();
    let opened = unix::open_vault(&paths, &admin, &mut doc, Vec::new(), None);
    let after = client::admin_open(&paths);
    primary.stop();
    mirror.stop();

    assert!(
        opened.is_err(),
        "conditional policy cannot establish all waiting identities"
    );
    let after = after.unwrap();
    assert_eq!(after.seq(), admin.seq());
    assert!(!after.doc.open());
    assert_eq!(after.doc.render().as_slice(), before.as_slice());
}

#[test]
fn approve_takes_codes_never_host_names() {
    let code = "abcd-ef23-ghij-k234";
    assert_eq!(
        parse(&words(&format!("approve {} {code}", code.to_uppercase()))),
        Ok(Command::Approve {
            which: vec![code.into(), code.into()],
            all: false,
            name: None,
        })
    );
    for refused in [
        "approve m22".to_string(),
        "approve abcd-ef23".to_string(),
        format!("approve {code} --all"),
        format!("approve {code} --name m9"),
        "approve age1xyz".to_string(),
        "approve --all --name m9".to_string(),
    ] {
        assert!(parse(&words(&refused)).is_err(), "{refused}");
    }
    assert_eq!(
        parse(&words("approve --all")),
        Ok(Command::Approve {
            which: vec![],
            all: true,
            name: None,
        })
    );
    assert_eq!(
        parse(&words("approve age1xyz --name m9")),
        Ok(Command::Approve {
            which: vec!["age1xyz".into()],
            all: false,
            name: Some("m9".into()),
        })
    );
}

#[test]
fn commons_init_action() {
    assert_eq!(parse(&words("commons init")), Ok(Command::CommonsInit));
    assert!(parse(&words("commons")).is_err());
    assert!(parse(&words("commons replace")).is_err());
}

#[test]
fn commons_recovery_requires_an_explicit_valid_plan_digest_to_write() {
    assert_eq!(
        parse(&words("commons restore")),
        Ok(Command::CommonsRestore { expected: None })
    );
    let command = format!("commons restore --expect {}", "01".repeat(32));
    assert_eq!(
        parse(&words(&command)),
        Ok(Command::CommonsRestore {
            expected: Some([1; 32])
        })
    );
    assert!(parse(&words("commons restore --expect bad")).is_err());
    assert!(parse(&words("commons init --expect bad")).is_err());
    assert!(parse(&words("commons restore --expect 00 --expect 00")).is_err());
    assert!(parse(&words("commons restore extra")).is_err());
}

#[test]
fn genesis_takes_one_file() {
    assert_eq!(
        parse(&words("genesis alab.genesis")),
        Ok(Command::Genesis("alab.genesis".into()))
    );
    assert!(parse(&words("genesis")).is_err());
    assert!(parse(&words("genesis a b")).is_err());
}

#[test]
fn password_actions() {
    assert_eq!(
        parse(&words("password")),
        Ok(Command::Password {
            action: Password::Show,
            agent: true
        })
    );
    assert_eq!(
        parse(&words("password new")),
        Ok(Command::Password {
            action: Password::New,
            agent: true
        })
    );
    assert_eq!(
        parse(&words("password off")),
        Ok(Command::Password {
            action: Password::Off,
            agent: true
        })
    );
    assert_eq!(
        parse(&words("password open")),
        Ok(Command::Password {
            action: Password::Open,
            agent: true
        })
    );
    assert_eq!(
        parse(&words("password open --no-agent")),
        Ok(Command::Password {
            action: Password::Open,
            agent: false
        })
    );
    assert!(parse(&words("password hunter2-hunter2")).is_err());
    let error = parse(&words("password set hunter2-hunter2")).expect_err("refused");
    assert!(!error.contains("hunter2"), "{error}");
}

#[test]
fn serve_flags_are_checked_before_anything_runs() {
    let key = crate::b64::encode_padded(&[9u8; 32]);
    assert!(parse(&words(&format!("serve --pin alab={key} --hold 60"))).is_ok());
    for refused in [
        "serve --hold 0".to_string(),
        "serve --hold 61".to_string(),
        "serve --listen nowhere".to_string(),
        format!("serve --pin {key}"),
        "serve --pin alab=short".to_string(),
        format!("serve --pin alab={key} --pin alab={key}"),
        "serve --advertise Bad_Name".to_string(),
        "server on --hold 0".to_string(),
        "serve --advertise lab-vault --no-advertise".to_string(),
    ] {
        assert!(parse(&words(&refused)).is_err(), "{refused}");
    }
    for accepted in [
        "serve --store /srv/vaults",
        "server on --store ~/vaults",
        "server on --store vault-store",
    ] {
        assert!(parse(&words(accepted)).is_ok(), "{accepted}");
    }
}

/// A relative `--store` is under the home directory whether launchd (from `/`), systemd
/// (from the home directory) or a person in another directory runs the server.
#[cfg(unix)]
#[test]
fn a_relative_store_is_under_the_home_directory() {
    let paths = crate::client::Paths::for_home(std::path::Path::new("/Users//someone"));
    for (store, dir) in [
        ("vault-store", "/Users//someone/vault-store"),
        ("~/vaults", "/Users//someone/vaults"),
        ("/srv/vaults", "/srv/vaults"),
    ] {
        assert_eq!(unix::store_dir(&paths, store), std::path::Path::new(dir));
    }
}

/// On macOS, `serve` may advertise the ALab vault's name only when listening on all
/// addresses at the default port and not told otherwise ([`alab_role`] decides whether it
/// does).
#[test]
fn serve_advertises_the_default_vaults_name_unless_told() {
    let advertised = |line: &str| {
        let f = flags(&words(line), SERVE_VALUED, SERVE_SWITCHES).expect("flags");
        serve_settings(&f).expect("settings").advertise
    };
    let default = if cfg!(target_os = "macos") {
        Advertise::AlabIfHeld
    } else {
        Advertise::Off
    };
    assert_eq!(advertised(""), default);
    assert_eq!(advertised("--listen 0.0.0.0:7484"), default);
    assert_eq!(advertised("--listen [::]:7484"), default);
    for elsewhere in [
        "--listen 127.0.0.1:7485",
        "--listen 127.0.0.1:7484",
        "--listen 0.0.0.0:7485",
        "--listen [::1]:7484",
    ] {
        assert_eq!(advertised(elsewhere), Advertise::Off, "{elsewhere}");
    }
    assert_eq!(
        advertised("--listen 127.0.0.1:7485 --advertise lab-vault"),
        Advertise::Named("lab-vault".into())
    );
    assert_eq!(
        advertised("--advertise lab-vault"),
        Advertise::Named("lab-vault".into())
    );
    assert_eq!(advertised("--no-advertise"), Advertise::Off);
    assert_eq!(
        parse(&words("server on --no-advertise")),
        Ok(Command::Server {
            action: Action::On,
            program: None,
            serve: vec!["--no-advertise".into()],
        })
    );
}

/// Only ALab's server answers the ALab name or takes another Mac's first version of the ALab
/// vault: one told so, or one whose store holds it on a Mac that follows it.
#[test]
fn only_alabs_server_advertises_the_alab_name_or_pins_the_alab_vault() {
    let key = [7; 32];
    let alab = crate::alab_advertised().to_string();
    let role = |advertise: Advertise, holds: bool, given: &[(&str, [u8; 32])]| {
        let mut pins: Vec<(String, [u8; 32])> = given
            .iter()
            .map(|(name, key)| ((*name).into(), *key))
            .collect();
        let here = if holds {
            AlabHere::Server
        } else {
            AlabHere::Empty
        };
        let (advertised, held_back) = alab_role(advertise, here, &mut pins, Some(key));
        let pinned: Vec<&str> = pins.iter().map(|(name, _)| name.as_str()).collect();
        (advertised, held_back.is_some(), pinned.join(","))
    };
    // A personal server: no ALab name, no ALab pin, and a line that says why.
    assert_eq!(
        role(Advertise::AlabIfHeld, false, &[]),
        (None, true, String::new())
    );
    assert_eq!(
        role(Advertise::Named("home".into()), false, &[]),
        (Some("home".into()), false, String::new())
    );
    assert_eq!(
        role(Advertise::Off, false, &[]),
        (None, false, String::new())
    );
    // ALab's server, by its store or by being told (a new one starts with an empty store).
    assert_eq!(
        role(Advertise::AlabIfHeld, true, &[]),
        (Some(alab.clone()), false, "alab".into())
    );
    assert_eq!(
        role(Advertise::Named(alab.clone()), false, &[]),
        (Some(alab.clone()), false, "alab".into())
    );
    assert_eq!(
        role(Advertise::Off, true, &[]),
        (None, false, "alab".into())
    );
    // A pin given by hand stands as given.
    assert_eq!(
        role(
            Advertise::AlabIfHeld,
            true,
            &[("alab", [9; 32]), ("home", [8; 32])]
        ),
        (Some(alab.clone()), false, "alab,home".into())
    );
    // A copy an ALab Mac put back on a server whose Mac does not follow the ALab vault (one
    // that took it before servers stopped) neither names nor pins, and says why.
    let mut pins = Vec::new();
    let (advertised, held_back) =
        alab_role(Advertise::AlabIfHeld, AlabHere::Copy, &mut pins, Some(key));
    assert_eq!(advertised, None);
    assert!(pins.is_empty());
    assert!(
        held_back.is_some_and(|line| line.contains("holds a copy of the ALab vault")),
        "says why"
    );
    // Told so, it is ALab's server whatever its store holds.
    let mut pins = Vec::new();
    let (advertised, _) = alab_role(
        Advertise::Named(alab.clone()),
        AlabHere::Copy,
        &mut pins,
        Some(key),
    );
    assert_eq!(advertised, Some(alab));
    assert_eq!(pins, [("alab".to_string(), key)]);
}

#[cfg(unix)]
#[test]
fn a_rotation_of_the_alab_vault_names_both_lines_the_next_release_changes() {
    let key = |n: u8| crate::b64::encode_padded(&[n; 32]);
    let note = unix::release_note(&crate::client::AlabRelease {
        signer: key(3),
        retired: vec![key(1), key(2)],
    });
    assert!(
        note.contains(&format!(
            "\n  pub const ALAB_SIGNER: &str = \"{}\";\n",
            key(3)
        )),
        "{note}"
    );
    assert!(
        note.ends_with(&format!(
            "\n  pub const ALAB_RETIRED: &[&str] = &[\"{}\", \"{}\"];\n",
            key(1),
            key(2)
        )),
        "{note}"
    );
}

#[test]
fn server_on_passes_serve_flags_through() {
    assert_eq!(
        parse(&words(
            "server on --advertise alab-secrets --program /x/aterm --no-keep-awake"
        )),
        Ok(Command::Server {
            action: Action::On,
            program: Some("/x/aterm".into()),
            serve: vec![
                "--advertise".into(),
                "alab-secrets".into(),
                "--no-keep-awake".into()
            ],
        })
    );
    assert!(parse(&words("server on --bogus")).is_err());
}

#[test]
fn agent_on_force_restarts_and_plain_on_does_not() {
    assert_eq!(
        parse(&words("agent on --force")),
        Ok(Command::Agent {
            action: Action::On,
            program: None,
            force: true,
        })
    );
    assert_eq!(
        parse(&words("agent on")),
        Ok(Command::Agent {
            action: Action::On,
            program: None,
            force: false,
        })
    );
}

#[cfg(unix)]
#[test]
fn a_join_without_a_terminal_does_not_wait_for_its_approval() {
    assert_eq!(unix::join_wait(true, false), std::time::Duration::ZERO);
    assert_eq!(unix::join_wait(false, true), std::time::Duration::ZERO);
    // CONTROL: at a terminal, a join waits ten minutes for the approval.
    assert_eq!(
        unix::join_wait(true, true),
        std::time::Duration::from_secs(600)
    );
}

#[cfg(unix)]
#[test]
fn the_join_password_is_shown_only_at_a_terminal_no_agent_runs() {
    let password = crate::SecretBytes::from("pear-otter-quilt".as_bytes());
    for (at_terminal, agent) in [
        (false, None),
        (true, Some("CLAUDECODE")),
        (false, Some("CODEX")),
    ] {
        let mut out = Vec::new();
        let code = unix::show_password(&mut out, at_terminal, agent, &password).expect("runs");
        assert_eq!(code, ExitCode::FAILURE, "{at_terminal} {agent:?}");
        assert!(
            out.is_empty(),
            "{at_terminal} {agent:?}: {} bytes",
            out.len()
        );
    }
    // CONTROL: a person at a terminal sees it.
    let mut out = Vec::new();
    let code = unix::show_password(&mut out, true, None, &password).expect("runs");
    assert_eq!(code, ExitCode::SUCCESS);
    assert_eq!(out, b"pear-otter-quilt\n");
}

#[cfg(unix)]
#[test]
fn a_new_join_password_is_told_only_at_a_terminal_no_agent_runs() {
    let password = crate::SecretBytes::from("pear-otter-quilt".as_bytes());
    for (at_terminal, agent) in [(false, None), (true, Some("CLAUDECODE"))] {
        let mut out = Vec::new();
        unix::tell_new_password(&mut out, at_terminal, agent, &password).expect("runs");
        assert!(
            out.is_empty(),
            "{at_terminal} {agent:?}: {} bytes",
            out.len()
        );
    }
    // CONTROL: a person at a terminal reads the password the vault now takes.
    let mut out = Vec::new();
    unix::tell_new_password(&mut out, true, None, &password).expect("runs");
    assert_eq!(out, b"join password: pear-otter-quilt\n");
}

#[test]
fn unknown_commands_and_options_are_usage_errors() {
    assert!(parse(&words("frobnicate")).is_err());
    assert!(parse(&words("--aterm-front-door-routing-probe")).is_err());
    assert!(parse(&words("sync --fast")).is_err());
    assert!(parse(&words("approve")).is_err());
    assert!(parse(&words("admins promote m3")).is_err());
}

/// What a waiting Mac names for an admin Mac is an `approve` that lets it in: its code for a
/// vault at a URL, whose server queues the request, and its key and name for one at a path,
/// which has no queue, so no code there is ever approved.
#[cfg(unix)]
#[test]
fn a_waiting_machine_names_the_approve_its_vault_takes() {
    use crate::client::{Config, Membership, Status};
    use crate::transport::Source;
    let key = crate::age::Recipient([7; 32]);
    let waiting = |source| Status {
        config: Some(Config {
            source,
            mirrors: Vec::new(),
            name: "home".into(),
            signer: Some([3; 32]),
            followed: Vec::new(),
            host: "mb".into(),
        }),
        machine: Some((key, "abcd-efgh-ijkm-npqr".into())),
        membership: Membership::Pending,
        names: Vec::new(),
    };
    let by_code = waiting(Source::Url("http://h:7484/v1/home".into()))
        .approve_command()
        .expect("a key");
    assert_eq!(by_code, "aterm secrets approve abcd-efgh-ijkm-npqr");
    let by_key = waiting(Source::Path("/Users//someone/home.vault".into()))
        .approve_command()
        .expect("a key");
    assert_eq!(
        by_key,
        format!("aterm secrets approve {} --name mb", key.to_bech32())
    );
    for command in [by_code, by_key] {
        let args = command
            .strip_prefix("aterm secrets ")
            .expect("a secrets verb");
        assert!(
            matches!(parse(&words(args)), Ok(Command::Approve { .. })),
            "{command}"
        );
    }
    let mut keyless = waiting(Source::Url("http://h:7484/v1/home".into()));
    keyless.machine = None;
    assert_eq!(keyless.approve_command(), None);
}

/// `join`'s last line and `status`'s membership line for a waiting Mac say how a
/// mistyped join password is fixed, beside what an admin runs.
#[cfg(unix)]
#[test]
fn a_waiting_machine_is_told_how_a_mistyped_join_password_is_fixed() {
    use crate::transport::Source;
    let code = "abcd-efgh-ijkm-npqr";
    let alab = Source::Url(crate::ALAB_DEFAULT_URL.into());
    let hint = unix::retype(&alab, None, false).expect("a URL has a join queue");
    assert!(
        hint.contains("mistyped join password is fixed by running aterm secrets join again"),
        "{hint}"
    );
    let approve = format!("aterm secrets approve {code}");
    for line in [
        unix::waiting_line("m9", code, &approve, Some(&hint)),
        unix::pending_membership(&approve, Some(&hint)),
    ] {
        assert!(
            line.contains(&format!("aterm secrets approve {code}")),
            "{line}"
        );
        assert!(line.contains(&hint), "{line}");
    }
    assert!(!unix::waiting_line("m9", code, &approve, None).contains("mistyped"));
    let other = unix::retype(&Source::Url("http://h:1/v1/x".into()), Some("m9"), false)
        .expect("a URL has a join queue");
    assert!(
        other.contains("running aterm secrets join <vault-url> --name m9 again"),
        "{other}"
    );
    let private = Source::Url("https://h/v1/x?token=private-query#private-pin".into());
    assert_eq!(unix::source_text(&private), "https://h/v1/x");
    for piped in [false, true] {
        let hint = unix::retype(&private, Some("m9"), piped).expect("a URL");
        assert!(hint.contains("<vault-url>"), "{hint}");
        assert!(hint.contains("full configured URL"), "{hint}");
        assert!(
            !hint.contains("private-query") && !hint.contains("private-pin"),
            "{hint}"
        );
    }
    let piped = unix::retype(&alab, None, true).expect("a URL has a join queue");
    assert!(
        piped.contains("piping it again to aterm secrets join --password-stdin"),
        "{piped}"
    );
    assert_eq!(
        unix::retype(&Source::Path("/v/x.vault".into()), None, false),
        None
    );
}

/// An admin of a new vault at a path under `root`.
#[cfg(unix)]
fn admin_of_a_path_vault(root: &std::path::Path) -> crate::client::Paths {
    let shared = root.join("shared").join("team.vault");
    std::fs::create_dir_all(shared.parent().expect("parent")).expect("dir");
    let admin = crate::client::Paths::for_home(&root.join("ma"));
    crate::client::create(&admin, shared.to_str().expect("utf-8"), None, Some("ma"))
        .expect("create");
    admin
}

/// The edit directories left under `paths`.
#[cfg(unix)]
fn edit_dirs(paths: &crate::client::Paths) -> Vec<std::path::PathBuf> {
    std::fs::read_dir(&paths.dir)
        .expect("state directory")
        .map(|entry| entry.expect("entry").path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("edit-"))
        })
        .collect()
}

#[cfg(unix)]
#[test]
fn an_editor_that_fails_on_the_published_text_leaves_no_draft() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let admin = admin_of_a_path_vault(root.path());
    let nothing_asked = |_: &str| -> Option<String> { panic!("nothing to ask") };
    let error = unix::edit_with(&admin, &mut |_: &std::path::Path| Ok(false), &nothing_asked)
        .expect_err("the editor failed");
    assert!(error.0.contains("nothing published"), "{error}");
    let error = unix::edit_with(
        &admin,
        &mut |_: &std::path::Path| Err(crate::Error::new("synthetic-editor: not found")),
        &nothing_asked,
    )
    .expect_err("the editor did not start");
    assert!(error.0.contains("synthetic-editor"), "{error}");
    assert_eq!(edit_dirs(&admin), Vec::<std::path::PathBuf>::new());
    let error = unix::edit_with(
        &admin,
        &mut |file: &std::path::Path| {
            let mut draft = std::fs::OpenOptions::new()
                .append(true)
                .open(file)
                .expect("draft");
            std::io::Write::write_all(&mut draft, b"# new work\n").expect("write");
            Ok(false)
        },
        &nothing_asked,
    )
    .expect_err("the editor failed");
    assert!(error.0.contains("the draft is kept"), "{error}");
    assert_eq!(edit_dirs(&admin).len(), 1);
}

#[cfg(unix)]
#[test]
fn an_edit_that_adds_an_ai_clis_login_file_publishes_nothing() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let admin = admin_of_a_path_vault(root.path());
    let began = crate::client::admin_open(&admin).expect("open").seq();
    let asked = std::cell::RefCell::new(Vec::new());
    let code = unix::edit_with(
        &admin,
        &mut |file: &std::path::Path| {
            let mut draft = std::fs::read_to_string(file).expect("draft");
            draft.push_str("[everyone]\n~/.codex/auth.json={}\n");
            std::fs::write(file, draft).expect("save");
            Ok(true)
        },
        &|prompt: &str| {
            asked.borrow_mut().push(prompt.to_string());
            Some("n".to_string())
        },
    )
    .expect("edit");
    assert_eq!(code, ExitCode::FAILURE);
    assert_eq!(*asked.borrow(), ["Edit again? [Y/n] "]);
    assert_eq!(
        crate::client::admin_open(&admin).expect("open").seq(),
        began,
        "nothing published"
    );
}

/// An edit's draft holds no Claude Code token the vault's log holds, so no plaintext copy of
/// one is written to disk (DESIGN-claude-setup-token §4.4). The editor is a stand-in that
/// copies the draft into the test's scratch directory and fails, so nothing is published.
/// CONTROL: a shared entry put the same way, with the same value, is in the draft.
#[cfg(unix)]
#[test]
fn an_edit_draft_holds_no_claude_token() {
    const TOKEN: &str = "aterm-claude-token-ac-0123456789abcdef";
    const FIXTURE: &[u8] = b"inert-claude-fixture-value";
    let macs = crate::client::two_log_macs_for_test(&[]);
    let m22 = &macs.m22;
    crate::client::test_put_everyone(m22, TOKEN, FIXTURE).expect("m22 puts the token");
    crate::client::test_put_everyone(m22, "shared-control", b"control-value")
        .expect("m22 puts a shared entry");
    let copy = macs.root.path().join("draft-copy");
    let nothing_asked = |_: &str| -> Option<String> { panic!("nothing to ask") };
    let error = unix::edit_with(
        m22,
        &mut |file: &std::path::Path| {
            std::fs::copy(file, &copy).expect("copy the draft");
            Ok(false)
        },
        &nothing_asked,
    )
    .expect_err("the stand-in editor fails");
    assert!(error.0.contains("nothing published"), "{error}");
    let draft = std::fs::read(&copy).expect("the copy");
    let holds = |value: &[u8]| draft.windows(value.len()).any(|w| w == value);
    assert!(
        holds(b"control-value"),
        "CONTROL: the draft holds the log's entries"
    );
    assert!(!holds(FIXTURE), "no token value in the draft");
    assert!(!holds(TOKEN.as_bytes()), "no token name in the draft");
}

/// `secrets get` refuses a Claude Code token entry, in any spelling, and prints nothing; `ls
/// --describe` lists the token and its meta with no file pointer, the token's row with its
/// account and state, and no byte of the value (DESIGN-claude-setup-token §4.2, §5.10).
/// CONTROL: a shared entry reads through `get`.
#[cfg(unix)]
#[test]
fn get_refuses_a_claude_token_and_ls_describe_names_no_file() {
    const VALUE: &[u8] = concat!("sk-ant-", "oat01-inert_cli-fixture-token").as_bytes();
    let macs = crate::client::two_log_macs_for_test(&[]);
    let m7 = &macs.m7;
    let key = crate::claude_token::Key::parse("ac-0123456789abcdef").unwrap();
    let meta = crate::claude_token::Meta {
        email: "inert@example.invalid".into(),
        account_id: "c-0123456789ab".into(),
        minted_at: 1,
        expires_at: 4_102_444_800,
        expiry_from: crate::claude_token::ExpiryFrom::Assumed,
        minted_on: "m7".into(),
        cli: "2.1.318".into(),
        verified: None,
    };
    let soon = std::time::Instant::now() + std::time::Duration::from_secs(60);
    assert_eq!(
        crate::claude_token::write(
            m7,
            &key,
            &crate::SecretBytes::from(VALUE),
            &meta,
            crate::client::OnBusy::Refuse(soon),
        )
        .unwrap(),
        crate::client::MemberWritten::Posted
    );
    crate::client::test_put_everyone(m7, "shared-control", b"control-value").unwrap();
    crate::client::sync(m7, None).unwrap();
    for name in [
        key.entry(),
        key.meta_entry(),
        key.entry().to_ascii_uppercase(),
    ] {
        let Err(error) = unix::get_value(m7, &name) else {
            panic!("{name}: get returned a value");
        };
        assert!(error.0.contains(unix::CLAUDE_TOKEN_GET), "{name}: {error}");
    }
    assert_eq!(
        unix::get_value(m7, "shared-control").unwrap().as_slice(),
        b"control-value",
        "CONTROL"
    );
    let crate::claude_token::Lookup::Found(standings) = crate::claude_token::listed(m7).unwrap()
    else {
        panic!("the vault reads");
    };
    let rows = super::inventory::token_rows(&standings, 0);
    let names = crate::client::registered_names(m7).unwrap();
    let text = super::inventory::listing(&names, &rows, true, false).unwrap();
    let holds = |text: &str, value: &[u8]| {
        text.as_bytes()
            .windows(8)
            .any(|w| value.windows(8).any(|v| v == w))
    };
    assert!(text.contains(&format!(
        "{}\tno file\tClaude Code token for inert@example.invalid",
        key.entry()
    )));
    assert!(text.contains("unverified"), "the row says its state");
    assert!(text.contains("ends 2100-01-01"), "and when it ends");
    assert!(text.contains(&format!(
        "{}\tno file\tmetadata for that token (no secret)",
        key.meta_entry()
    )));
    assert!(
        !text.contains("~/.secrets/aterm-claude-token"),
        "no pointer"
    );
    assert!(!holds(&text, VALUE), "no byte of the value");
    let json = super::inventory::listing(&names, &rows, false, true).unwrap();
    let parsed: aterm_json::Value = aterm_json::from_str(&json).unwrap();
    let aterm_json::Value::Array(entries) = &parsed["entries"] else {
        panic!("entries");
    };
    let token = entries
        .iter()
        .find(|row| row["name"].as_str() == Some(key.entry().as_str()))
        .expect("the token's row");
    assert_eq!(token["pointer"], aterm_json::Value::Null);
    assert!(!holds(&json, VALUE));
    let plain = super::inventory::listing(&names, &rows, false, false).unwrap();
    assert!(plain.lines().any(|line| line == key.entry()));
    macs.server.stop();
}

/// Run an edit where a concurrent publish forces a reopen, then quit unchanged and answer
/// `publish`.
#[cfg(unix)]
fn edit_reopened_unchanged(
    root: &std::path::Path,
    publish: &str,
) -> (crate::client::Paths, u64, ExitCode, Vec<String>) {
    let admin = admin_of_a_path_vault(root);
    let began = crate::client::admin_open(&admin).expect("open").seq();
    let mut opened = 0;
    let asked = std::cell::RefCell::new(Vec::new());
    let code = unix::edit_with(
        &admin,
        &mut |file: &std::path::Path| {
            opened += 1;
            if opened == 1 {
                let mut draft = std::fs::read_to_string(file).expect("draft");
                draft.push_str("[everyone]\nnote=synthetic-note\n");
                std::fs::write(file, draft).expect("save");
                let other = crate::client::admin_open(&admin).expect("open");
                let mut doc = other.doc.clone();
                let mb = crate::age::Identity::generate().expect("identity");
                doc.add_machine("mb", &mb.recipient().to_bech32())
                    .expect("add");
                doc.set_entry(
                    &crate::text::Audience::Everyone,
                    "other",
                    b"synthetic-other",
                )
                .expect("set");
                crate::client::admin_publish(&admin, &other, &doc).expect("publish");
            }
            Ok(true)
        },
        &|prompt: &str| {
            asked.borrow_mut().push(prompt.to_string());
            Some(
                if prompt.contains("as it is") {
                    publish
                } else {
                    ""
                }
                .to_string(),
            )
        },
    )
    .expect("edit");
    assert_eq!(opened, 2, "the editor reopened once");
    (admin, began, code, asked.into_inner())
}

#[cfg(unix)]
#[test]
fn a_draft_reopened_unchanged_on_a_vault_that_moved_publishes_only_when_the_person_says_so() {
    let everyone = crate::text::Audience::Everyone;
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let (admin, began, code, asked) = edit_reopened_unchanged(&root.path().join("a"), "");
    assert_eq!(code, ExitCode::FAILURE);
    assert!(asked.iter().any(|p| p.contains("as it is")), "{asked:?}");
    let after = crate::client::admin_open(&admin).expect("open");
    assert_eq!(after.seq(), began + 1, "only the other admin's version");
    assert!(after.doc.entry(&everyone, "other").is_some());
    assert!(after.doc.entry(&everyone, "note").is_none());
    assert_eq!(edit_dirs(&admin).len(), 1, "the draft is kept");
    let (admin, began, code, _) = edit_reopened_unchanged(&root.path().join("b"), "y");
    assert_eq!(code, ExitCode::SUCCESS);
    let after = crate::client::admin_open(&admin).expect("open");
    assert_eq!(after.seq(), began + 2, "the edit went on top");
    assert!(after.doc.entry(&everyone, "note").is_some());
    assert!(after.doc.entry(&everyone, "other").is_none());
    assert!(after.doc.machines().iter().any(|m| m.name == "mb"));
    assert_eq!(edit_dirs(&admin), Vec::<std::path::PathBuf>::new());
}

/// A key approved with `--name` gets that name or none: a name that is not a machine name
/// is a usage error, never a generic `machine` in its place.
#[test]
fn a_key_is_never_approved_under_a_name_it_was_not_given() {
    for refused in [
        "M9",
        "join",
        "everyone",
        "-m9",
        "m9 extra",
        "abcd-efgh-ijkm-npqr",
    ] {
        let line = vec![
            "approve".to_string(),
            "age1xyz".to_string(),
            "--name".to_string(),
            refused.to_string(),
        ];
        let why = parse(&line).expect_err(refused);
        assert!(why.contains("--name"), "{refused}: {why}");
    }
    assert!(parse(&words("approve age1xyz --name m9")).is_ok());
}

/// A value piped to `set` loses its line end only when it is one line of text; a binary
/// value that happens to end in LF (or CR LF) keeps every byte, as `--file` keeps it.
#[cfg(unix)]
#[test]
fn a_piped_value_loses_its_line_end_only_when_it_is_one_line_of_text() {
    let trimmed = |bytes: &[u8]| {
        let mut bytes = bytes.to_vec();
        unix::trim_piped_line(&mut bytes);
        bytes
    };
    assert_eq!(trimmed(b"token\n"), b"token");
    assert_eq!(trimmed(b"token\r\n"), b"token");
    assert_eq!(trimmed("t\u{f6}ken\n".as_bytes()), "t\u{f6}ken".as_bytes());
    for kept in [
        &b"two\nlines\n"[..],
        b"no line end",
        b"\xff\x00\x41\n",
        b"\x80\x0d\x0a",
        b"\x9f\x13\xc0\xfe\x0a",
        b"a\x00b\n",
        b"a\rb\n",
    ] {
        assert_eq!(trimmed(kept), kept, "{kept:?}");
    }
}

/// A piped join password longer than a `[join]` line holds is refused, never cut to the
/// part that fits, and so is one a CR breaks; the blanks around a password that fits are
/// dropped. Control: exactly [`crate::join::MAX_LEN`] bytes and a CR LF are taken whole.
#[cfg(unix)]
#[test]
fn a_piped_password_is_refused_rather_than_cut_or_split() {
    let read = |bytes: &[u8]| {
        let mut input = bytes;
        unix::read_piped_password("--password-stdin", &mut input)
            .map(|password| password.as_slice().to_vec())
    };
    let mut exact = vec![b'p'; crate::join::MAX_LEN];
    exact.extend_from_slice(b"\r\n");
    assert_eq!(read(&exact).unwrap(), vec![b'p'; crate::join::MAX_LEN]);
    let mut padded = b"  \n".to_vec();
    padded.extend_from_slice(&exact);
    padded.extend(std::iter::repeat_n(b'\n', 3000));
    assert_eq!(read(&padded).unwrap(), vec![b'p'; crate::join::MAX_LEN]);

    // What the old two-byte margin read as a whole password: the first MAX_LEN bytes and
    // the CR LF after them, with the rest of the line dropped unread.
    let mut cut = exact.clone();
    cut.push(b'x');
    let mut longer = vec![b'p'; crate::join::MAX_LEN + 1];
    longer.push(b'\n');
    let mut blank_tail = exact.clone();
    blank_tail.extend(std::iter::repeat_n(b' ', 4096));
    for refused in [
        cut.as_slice(),
        longer.as_slice(),
        blank_tail.as_slice(),
        b"first\rsecond\n",
    ] {
        assert!(read(refused).is_err(), "{:?}", refused.len());
    }
}

/// `revoke` and `admins remove` list every secret the machine could read (everything for an
/// admin, `[everyone]` plus its own host sections for a member).
#[cfg(unix)]
#[test]
fn letting_a_machine_go_names_everything_it_could_read() {
    use crate::text::{Audience, Document};
    let key = || {
        crate::age::Identity::generate()
            .expect("a key")
            .recipient()
            .to_bech32()
    };
    let mut doc = Document::new_vault("m22", &key());
    for name in ["m5", "m3", "build"] {
        doc.add_machine(name, &key()).expect("machine");
    }
    doc.add_admin("m5").expect("admin");
    let hosts = |name: &str| Audience::Hosts(vec![name.to_string()]);
    for (audience, name) in [
        (Audience::Everyone, "gh_token"),
        (Audience::Everyone, "npm_token"),
        (hosts("build"), "deploy_key"),
        (hosts("m3"), "signing_key"),
        (hosts("m3"), "npm_token"),
    ] {
        doc.set_entry(&audience, name, b"synthetic").expect("set");
    }
    let admin = unix::exposure(&doc, "m5").expect("an admin read everything");
    assert!(
        admin.contains(
            "m5 was an admin, so it could read every secret (5: gh_token, npm_token, deploy_key [build], signing_key [m3], npm_token [m3])"
        ),
        "{admin}"
    );
    let m3 = unix::exposure(&doc, "m3").expect("m3 read its own and everyone's");
    assert!(
        m3.contains(
            "m3 could read 4 secrets (gh_token, npm_token, signing_key [m3], npm_token [m3])"
        ),
        "{m3}"
    );
    assert!(!m3.contains("deploy_key"), "{m3}");
    assert_eq!(
        unix::exposure(&Document::new_vault("m22", &key()), "m22"),
        None,
        "nothing to read, nothing to rotate"
    );
}

/// A join code typed for a machine resolves to the machine whose key has that code, and a
/// code no key has is refused even if a machine bears it as its name.
#[cfg(unix)]
#[test]
fn a_code_typed_for_a_machine_reaches_the_key_that_has_it() {
    use crate::text::Document;
    let key = || crate::age::Identity::generate().expect("a key").recipient();
    let mac = key();
    let code = crate::client::join_code(&mac);
    let mut doc = Document::new_vault("m22", &key().to_bech32());
    doc.add_machine("mb", &mac.to_bech32()).expect("machine");
    let typed = [code.clone(), code.to_uppercase(), "m3".to_string()];
    assert_eq!(
        unix::machines_named(&doc, &typed).expect("named"),
        ["mb", "mb", "m3"]
    );
    assert!(unix::machines_named(&doc, &["aaaa-bbbb-cccc-dddd".to_string()]).is_err());

    let mut both = doc.clone();
    let stranger = key();
    both.add_machine(&code, &stranger.to_bech32())
        .expect("a code is a valid machine name in a text");
    let error = unix::machines_named(&both, std::slice::from_ref(&code))
        .expect_err("either machine may be meant");
    assert!(
        error.0.contains("mb") && error.0.contains(&stranger.to_bech32()),
        "{error}"
    );
    let unkeyed = "aaaa-bbbb-cccc-dddd".to_string();
    let named = key();
    doc.add_machine(&unkeyed, &named.to_bech32())
        .expect("machine");
    let error =
        unix::machines_named(&doc, std::slice::from_ref(&unkeyed)).expect_err("a code no key has");
    let own = crate::client::join_code(&named);
    assert!(error.0.contains(&own), "{error}");
    assert_eq!(
        unix::machines_named(&doc, &[own]).expect("its own code"),
        [unkeyed]
    );
}

#[cfg(unix)]
#[test]
fn status_says_what_the_node_service_last_did_with_the_vault() {
    let running = || {
        Ok(crate::service::State::Installed {
            pid: Some(7),
            unit: std::path::PathBuf::from("unit"),
        })
    };
    let now_ms = aterm_time::unix_secs().unwrap() * 1000;
    let receipt = |said| crate::client::NodePassReceipt {
        ran_ms: now_ms,
        said,
        signed: 0,
        applied: None,
    };
    let words = |said, off| unix::node_pass_words(&running(), Some(&receipt(said)), off);
    assert!(
        words(crate::client::NodePassSaid::Done, false)
            .starts_with("running (pid 7), looked at the vault ")
    );
    assert!(
        words(crate::client::NodePassSaid::Busy, false)
            .starts_with("running (pid 7), waited for another vault write ")
    );
    assert!(
        words(crate::client::NodePassSaid::Failed, false)
            .starts_with("running (pid 7), could not finish its look at the vault ")
    );
    assert_eq!(
        words(crate::client::NodePassSaid::Done, true),
        "running (pid 7), keeps no vault work (aterm secrets agent off)"
    );
    assert_eq!(
        unix::node_pass_words(&running(), None, false),
        "running (pid 7), has not looked at the vault yet"
    );
    assert_eq!(
        unix::node_pass_words(&Ok(crate::service::State::Absent), None, false),
        "not installed, has not looked at the vault yet"
    );
}

/// `status` says what the server job's store holds, read from the store its unit names: a server
/// that runs and holds no vault says so (a Linux member's ran that way for its whole life while
/// `status` said only "running").
#[cfg(unix)]
#[test]
fn status_says_when_the_server_holds_no_vault() {
    use crate::service::{Job, State};
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let paths = crate::client::Paths::for_home(&root.path().join("home"));
    let program = root.path().join("aterm");
    std::fs::write(&program, b"").expect("a program file");
    let args: Vec<String> = ["secrets", "serve", "--no-keep-awake"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    if cfg!(target_os = "macos") {
        crate::service::write_plist(&paths.home, Job::Server, &program, &args)
    } else {
        crate::service::write_unit(&paths.home, Job::Server, &program, &args)
    }
    .expect("the server's unit");
    let running = Ok(State::Installed {
        pid: Some(7),
        unit: crate::service::unit_file(&paths.home, Job::Server),
    });
    let words = |state: &crate::Result<State>| unix::server_words(&paths, state);
    assert_eq!(words(&running), "running (pid 7), holds no vault");
    assert_eq!(words(&Ok(State::Absent)), "not installed");

    std::fs::create_dir_all(paths.served()).expect("the store");
    let vault = |name: &str| {
        crate::envelope::sign(
            &crate::envelope::Unsigned {
                name: name.to_owned(),
                seq: 3,
                time: 1_792_530_003,
                handoff: Vec::new(),
                parents: Vec::new(),
                lockers: vec![crate::envelope::Locker {
                    label: "admin".to_owned(),
                    armored: crate::envelope::test_armor(&[3; 40]),
                }],
            },
            &[5; 32],
        )
        .expect("a signed vault")
    };
    // A file that names another vault than its own is not held.
    std::fs::write(paths.served().join("team.vault"), vault("alab")).unwrap();
    assert_eq!(words(&running), "running (pid 7), holds no vault");
    std::fs::write(paths.served().join("alab.vault"), vault("alab")).unwrap();
    std::fs::write(paths.served().join("team.vault"), vault("team")).unwrap();
    assert_eq!(words(&running), "running (pid 7), holds alab, team");
}

/// `status` of a machine of a signed vault names it by its code: the name it asked for
/// may be another machine's in the vault.
#[cfg(unix)]
#[test]
fn status_names_a_machine_of_a_signed_vault_by_its_code() {
    use crate::transport::Source;
    let config = |signer: Option<[u8; 32]>| crate::client::Config {
        source: Source::Path("/v/team.vault".into()),
        mirrors: Vec::new(),
        name: "team".to_string(),
        signer,
        followed: Vec::new(),
        host: "m5".to_string(),
    };
    let code = "abcd-efgh-ijkm-npqr";
    let signed = unix::machine_line(Some(&config(Some([7; 32]))), "age1k", code);
    assert_eq!(
        signed,
        format!("machine: code {code}, key age1k, asked to join as m5")
    );
    let plain = unix::machine_line(Some(&config(None)), "age1k", code);
    assert_eq!(plain, format!("machine: m5, code {code}, key age1k"));
    assert_eq!(
        unix::machine_line(None, "age1k", code),
        format!("machine: code {code}, key age1k")
    );
}

/// A subscriber played with an inert arm: `state` says its state and `install` stands in
/// for an install (each test counts them), never a launchd job or a systemd unit.
#[cfg(unix)]
fn played<'a>(
    state: &'a dyn Fn() -> crate::Result<crate::service::State>,
    install: &'a dyn Fn() -> crate::Result<String>,
) -> unix::Subscriber<'a> {
    unix::Subscriber {
        state,
        install,
        node: None,
    }
}

#[cfg(unix)]
fn installed(pid: Option<u32>) -> crate::service::State {
    crate::service::State::Installed {
        pid,
        unit: std::path::PathBuf::from("/played/systems.alab.aterm-secrets.plist"),
    }
}

/// `put_in_approver` installs a stopped or absent subscriber, leaves a running one alone, and
/// skips installation when the state cannot be read.
#[cfg(unix)]
#[test]
fn the_approver_is_put_in_only_where_it_does_not_run() {
    use crate::service::State;
    use unix::Approver;
    let installs = std::cell::Cell::new(0);
    let install = || {
        installs.set(installs.get() + 1);
        Ok("played: installed".to_string())
    };
    let refused = || -> crate::Result<String> {
        installs.set(installs.get() + 1);
        Err(crate::Error::new("played: no launchd"))
    };
    let running = || Ok(installed(Some(42)));
    assert_eq!(
        unix::put_in_approver(&played(&running, &install)),
        Approver::Running
    );
    assert_eq!(installs.get(), 0, "a running subscriber is left be");
    let stopped = || Ok(installed(None));
    let absent = || Ok(State::Absent);
    for state in [&stopped as &dyn Fn() -> crate::Result<State>, &absent] {
        assert_eq!(
            unix::put_in_approver(&played(state, &install)),
            Approver::Installed("played: installed".into())
        );
    }
    assert_eq!(installs.get(), 2);
    assert_eq!(
        unix::put_in_approver(&played(&absent, &refused)),
        Approver::Failed("played: no launchd".into())
    );
    let unreadable = || Err(crate::Error::new("played: launchctl hung"));
    assert_eq!(
        unix::put_in_approver(&played(&unreadable, &install)),
        Approver::Unknown("played: launchctl hung".into())
    );
    assert_eq!(installs.get(), 3, "an unreadable state installs nothing");
    assert_eq!(
        unix::approver_here(false, &played(&running, &install)),
        Approver::Running
    );
    for state in [
        &stopped as &dyn Fn() -> crate::Result<State>,
        &absent,
        &unreadable,
    ] {
        assert_eq!(
            unix::approver_here(false, &played(state, &install)),
            Approver::Skipped
        );
    }
    assert_eq!(
        unix::approver_here(true, &played(&absent, &install)),
        Approver::Installed("played: installed".into())
    );
    assert_eq!(
        installs.get(),
        4,
        "only the one asked to put it in installs"
    );
}

/// When no approver runs locally, commands that need one report why and name
/// `aterm secrets agent on`.
#[cfg(unix)]
#[test]
fn a_command_whose_approver_does_not_run_says_what_starts_one() {
    use crate::client::Approval;
    use unix::Approver;
    assert_eq!(
        unix::approver_words(&Approver::Running, Approval::Password),
        None
    );
    assert_eq!(
        unix::approver_words(&Approver::Installed("line".into()), Approval::Open),
        Some("line".into())
    );
    for (approver, why) in [
        (Approver::Skipped, "(--no-agent)"),
        (Approver::Failed("played".into()), "not installed: played"),
        (Approver::Unknown("played".into()), "unknown (played)"),
    ] {
        for (approval, who) in [
            (Approval::Password, "a machine that types the join password"),
            (Approval::Open, unix::joiner(Approval::Open)),
        ] {
            let words = unix::approver_words(&approver, approval).expect("said");
            assert!(words.contains(why), "{words}");
            assert!(words.contains(&format!("{who} waits")), "{words}");
            assert!(
                words.contains("aterm secrets agent on starts one"),
                "{words}"
            );
            assert_eq!(
                words.starts_with("warning: "),
                approver != Approver::Skipped,
                "{words}"
            );
        }
    }
    assert!(Approver::Running.runs() && Approver::Installed(String::new()).runs());
    assert!(!Approver::Skipped.runs() && !Approver::Failed(String::new()).runs());
}

/// A vault at a path has no join queue, so `lets_in_words` reports that machines join only by
/// admin approval of their key.
#[cfg(unix)]
#[test]
fn a_vault_at_a_path_says_a_machine_joins_it_by_key() {
    let mut open = crate::text::Document::new_vault("ma", &crate::transport::test_recipient(1));
    let mut password = open.clone();
    let closed = open.clone();
    open.set_open(true).expect("open");
    password
        .set_join_password(Some(b"a-long-enough-join-password"))
        .expect("password");
    for doc in [&open, &password] {
        let words = unix::lets_in_words(false, doc).expect("said");
        assert!(words.contains("no join queue"), "{words}");
        assert!(
            words.contains("aterm secrets approve <age1…> --name <host>"),
            "{words}"
        );
        assert!(!words.contains("let in"), "{words}");
    }
    assert_eq!(unix::lets_in_words(false, &closed), None);
    // An open vault at a URL admits a machine by a person's Allow on a member, never by
    // asking alone (docs/DESIGN-fluid-swarm-2026-10-28.md F10).
    let words = unix::lets_in_words(true, &open).expect("said");
    assert!(words.contains("presses Allow on any member"), "{words}");
    assert!(!words.contains("every machine"), "{words}");
    assert_eq!(unix::lets_in_words(true, &password), None);
    assert_eq!(unix::lets_in_words(true, &closed), None);
}

/// `create` installs the approver unless `--no-agent` is set or the vault is at a path, and a
/// failed install still succeeds with a warning.
#[cfg(unix)]
#[test]
fn create_puts_in_the_approver_unless_told_not_to() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let server = crate::server::start(
        std::net::TcpListener::bind("127.0.0.1:0").expect("bind"),
        &crate::server::Options {
            listen: "127.0.0.1:0".parse().expect("addr"),
            store: root.path().join("store"),
            advertise: None,
            pins: Vec::new(),
            hold: std::time::Duration::from_millis(200),
            keep_awake: false,
        },
    )
    .expect("a loopback server");
    let installs = std::cell::Cell::new(0);
    let install = || {
        installs.set(installs.get() + 1);
        Ok("played: installed".to_string())
    };
    let refused = || -> crate::Result<String> {
        installs.set(installs.get() + 1);
        Err(crate::Error::new("played: no launchd"))
    };
    let absent = || Ok(crate::service::State::Absent);
    let unread =
        || -> crate::Result<crate::service::State> { panic!("this create reads no subscriber") };
    let reads = std::cell::Cell::new(0);
    let running = || {
        reads.set(reads.get() + 1);
        Ok(installed(Some(42)))
    };
    let create = |dir: &str, agent: bool, subscriber: &unix::Subscriber<'_>| {
        let source = if dir == "path" {
            let shared = root.path().join(dir).join("team.vault");
            std::fs::create_dir_all(shared.parent().expect("parent")).expect("dir");
            shared.to_str().expect("utf-8").to_string()
        } else {
            format!("http://{}/v1/{dir}", server.addr)
        };
        let admin = crate::client::Paths::for_home(&root.path().join(dir).join("ma"));
        let code = unix::create(&admin, &source, None, Some("ma".into()), agent, subscriber)
            .expect("created");
        assert_eq!(code, ExitCode::SUCCESS);
        crate::client::admin_open(&admin).expect("an admin of it");
    };
    let off = |dir: &str| crate::service::subscriber_turned_off(&root.path().join(dir).join("ma"));
    create("a", true, &played(&absent, &install));
    assert_eq!(installs.get(), 1, "create installs the subscriber");
    assert!(!off("a"), "no choice to record");
    create("b", false, &played(&absent, &install));
    assert_eq!(installs.get(), 1, "--no-agent installs nothing");
    assert!(off("b"), "--no-agent is the person's choice, recorded");
    create("d", false, &played(&running, &install));
    assert_eq!(
        reads.get(),
        2,
        "--no-agent reads whether the approver runs, for the record and for the approver"
    );
    assert_eq!(installs.get(), 1, "a running subscriber is left be");
    assert!(
        !off("d"),
        "a subscriber that runs is left be, its choice not recorded"
    );
    create("c", true, &played(&absent, &refused));
    assert_eq!(installs.get(), 2, "a failed install still makes the vault");
    create("path", true, &played(&unread, &install));
    assert_eq!(
        installs.get(),
        2,
        "a vault at a path has nothing to approve"
    );
    server.stop();
}

/// Test server that accepts the first `PUT` (204) and returns 503 on subsequent `GET`s.
#[cfg(unix)]
struct LostAfterThePut {
    url: String,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    addr: std::net::SocketAddr,
    server: std::thread::JoinHandle<()>,
}

#[cfg(unix)]
impl LostAfterThePut {
    fn start() -> LostAfterThePut {
        Self::start_with_dropped_reply(false)
    }

    fn start_with_dropped_reply(drop_reply: bool) -> LostAfterThePut {
        use std::io::{Read, Write};
        use std::sync::atomic::Ordering;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("address");
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let seen = std::sync::Arc::clone(&stop);
        let server = std::thread::spawn(move || {
            let mut put = false;
            loop {
                let stream = aterm_uds::accept(
                    &listener,
                    aterm_uds::AcceptWait::Forever,
                    aterm_uds::AcceptedMode::Blocking,
                )
                .map(|(stream, _)| stream);
                if seen.load(Ordering::SeqCst) {
                    return;
                }
                let Ok(mut stream) = stream else {
                    continue;
                };
                let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(60)));
                let mut head = Vec::new();
                let mut byte = [0u8; 1];
                while !head.ends_with(b"\r\n\r\n") {
                    match stream.read(&mut byte) {
                        Ok(1) => head.push(byte[0]),
                        _ => break,
                    }
                }
                let head = String::from_utf8_lossy(&head).into_owned();
                let length = head
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| {
                                let s = value.trim();
                                (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
                                    .then(|| s.parse::<usize>().ok())
                                    .flatten()
                            })
                            .flatten()
                    })
                    .unwrap_or(0);
                let mut body = vec![0u8; length];
                let _ = stream.read_exact(&mut body);
                let status = if head.starts_with("PUT ") {
                    put = true;
                    if drop_reply {
                        continue;
                    }
                    "204 No Content"
                } else if put {
                    "503 Service Unavailable"
                } else {
                    "404 Not Found"
                };
                let _ = stream.write_all(
                    format!("HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                        .as_bytes(),
                );
            }
        });
        LostAfterThePut {
            url: format!("http://{addr}/v1/team"),
            stop,
            addr,
            server,
        }
    }

    /// Answer no more: the connection this makes wakes the server to see it.
    fn end(self) {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        std::net::TcpStream::connect(self.addr).expect("the server is woken");
        self.server.join().expect("the server ends");
    }
}

/// A server becoming unavailable after publication does not prevent the
/// retained initial envelope from installing locally with its approver.
#[cfg(unix)]
#[test]
fn a_create_installs_locally_when_the_server_stops_answering_after_put() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let installs = std::cell::Cell::new(0);
    let install = || {
        installs.set(installs.get() + 1);
        Ok("played: installed".to_string())
    };
    let absent = || Ok(crate::service::State::Absent);
    let lost = LostAfterThePut::start();
    let admin = crate::client::Paths::for_home(&root.path().join("ma"));
    let code = unix::create(
        &admin,
        &lost.url,
        None,
        Some("ma".into()),
        true,
        &played(&absent, &install),
    )
    .expect("created");
    assert_eq!(code, ExitCode::SUCCESS);
    assert_eq!(installs.get(), 1, "the approver goes in");
    let status = crate::client::status(&admin).expect("status");
    assert_eq!(
        status.config.map(|config| config.source),
        Some(crate::transport::Source::Url(lost.url.clone()))
    );
    assert!(matches!(
        status.membership,
        crate::client::Membership::In { seq: 1, .. }
    ));
    lost.end();

    let lost = LostAfterThePut::start();
    let other = crate::client::Paths::for_home(&root.path().join("mb"));
    let created = crate::client::create(&other, &lost.url, None, Some("mb")).expect("created");
    assert_eq!(
        created.local, None,
        "installs from retained authenticated bytes"
    );
    lost.end();
}

#[cfg(unix)]
#[test]
fn a_lost_creation_reply_still_starts_its_recovery_subscriber() {
    let root = aterm_tempfile::tempdir().unwrap();
    let paths = crate::client::Paths::for_home(&root.path().join("m1"));
    let installs = std::cell::Cell::new(0);
    let install = || {
        installs.set(installs.get() + 1);
        Ok("played: installed".to_string())
    };
    let absent = || Ok(crate::service::State::Absent);
    let lost = LostAfterThePut::start_with_dropped_reply(true);
    assert!(
        unix::create(
            &paths,
            &lost.url,
            None,
            Some("m1".into()),
            true,
            &played(&absent, &install)
        )
        .is_err()
    );
    assert_eq!(installs.get(), 1);
    assert!(crate::client::creation_pending(&paths).unwrap());
    assert!(crate::client::config(&paths).unwrap().is_none());
    lost.end();
}

/// Opening a vault or setting a join password installs the approver when none runs; closing it
/// (`off`) reads no subscriber.
#[cfg(unix)]
#[test]
fn a_vault_that_lets_machines_in_gets_an_approver_and_status_says_whether_it_runs() {
    use crate::client::{Approval, approving_here};
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let server = crate::server::start(
        std::net::TcpListener::bind("127.0.0.1:0").expect("bind"),
        &crate::server::Options {
            listen: "127.0.0.1:0".parse().expect("addr"),
            store: root.path().join("store"),
            advertise: None,
            pins: Vec::new(),
            hold: std::time::Duration::from_millis(200),
            keep_awake: false,
        },
    )
    .expect("a loopback server");
    let installs = std::cell::Cell::new(0);
    let install = || {
        installs.set(installs.get() + 1);
        Ok("played: installed".to_string())
    };
    let absent = || Ok(crate::service::State::Absent);
    let running = || Ok(installed(Some(42)));
    let unread = || -> crate::Result<crate::service::State> {
        panic!("closing the vault reads no subscriber")
    };
    let admin = crate::client::Paths::for_home(&root.path().join("ma"));
    let url = format!("http://{}/v1/team", server.addr);
    assert_eq!(
        unix::create(
            &admin,
            &url,
            None,
            Some("ma".into()),
            true,
            &played(&running, &install),
        )
        .expect("created"),
        ExitCode::SUCCESS
    );
    assert_eq!(installs.get(), 0, "a running subscriber is left be");
    assert_eq!(
        approving_here(&admin),
        Ok(Some(Approval::Open)),
        "a new vault is open"
    );
    let password = |action, subscriber: &unix::Subscriber<'_>| {
        assert_eq!(
            unix::password(&admin, action, true, subscriber).expect("published"),
            ExitCode::SUCCESS
        );
    };
    password(Password::Off, &played(&unread, &install));
    assert_eq!(approving_here(&admin), Ok(None), "closed, nothing approves");
    password(Password::Open, &played(&absent, &install));
    assert_eq!(installs.get(), 1, "opening puts the approver in");
    assert_eq!(approving_here(&admin), Ok(Some(Approval::Open)));
    password(Password::Open, &played(&absent, &install));
    assert_eq!(
        installs.get(),
        2,
        "open again, it still says the vault lets in"
    );
    password(Password::Open, &played(&running, &install));
    assert_eq!(installs.get(), 2, "a running approver is left be");
    assert_eq!(
        unix::password(&admin, Password::Open, false, &played(&absent, &install))
            .expect("published"),
        ExitCode::SUCCESS
    );
    assert_eq!(installs.get(), 2, "--no-agent installs nothing");
    password(Password::Off, &played(&unread, &install));
    assert_eq!(approving_here(&admin), Ok(None), "closed, nothing approves");
    password(Password::Off, &played(&unread, &install));
    assert_eq!(installs.get(), 2);
    server.stop();
    let stranger = crate::client::Paths::for_home(&root.path().join("stranger"));
    assert_eq!(approving_here(&stranger), Ok(None));
    let at_a_path = admin_of_a_path_vault(&root.path().join("path"));
    assert_eq!(approving_here(&at_a_path), Ok(None));
    let no_queue =
        || -> crate::Result<crate::service::State> { panic!("a path has no queue to approve") };
    // A new path vault is open too: close it so opening runs the arm that publishes.
    assert_eq!(
        unix::password(
            &at_a_path,
            Password::Off,
            true,
            &played(&no_queue, &install),
        )
        .expect("closed"),
        ExitCode::SUCCESS
    );
    assert_eq!(
        unix::password(
            &at_a_path,
            Password::Open,
            true,
            &played(&no_queue, &install),
        )
        .expect("published"),
        ExitCode::SUCCESS
    );
    assert_eq!(installs.get(), 2, "nor is one put in there");

    let line = |approval, state| unix::approver_line(approval, &state, None);
    let ok = line(Approval::Password, Ok(installed(Some(42))));
    assert!(ok.contains("running in the subscriber (pid 42)"), "{ok}");
    assert!(ok.contains("types the join password is let in"), "{ok}");
    let why = "admin locker: alab-vault-admin/4 is newer";
    let said = unix::approver_line(Approval::Open, &Ok(installed(Some(42))), Some(why));
    assert!(
        said.contains("(pid 42) but not approving: admin locker: alab-vault-admin/4 is newer;"),
        "{said}"
    );
    assert!(
        said.contains(&format!(
            "{} waits for approval by code",
            unix::joiner(Approval::Open)
        )),
        "{said}"
    );
    // A status that cannot read what stopped the approver still says it is stuck, and
    // says nothing where no record is, or no subscriber runs.
    let status = |approving, state, stuck| unix::approver_status(&approving, &state, stuck);
    let unreadable = || Err(crate::Error::new(why));
    let stuck = status(unreadable(), Ok(installed(Some(42))), Some(why)).expect("a line");
    assert!(
        stuck.contains("(pid 42) cannot tell whether this Mac approves joins: admin locker"),
        "{stuck}"
    );
    assert_eq!(status(unreadable(), Ok(installed(Some(42))), None), None);
    assert_eq!(status(unreadable(), Ok(installed(None)), Some(why)), None);
    assert_eq!(status(Ok(None), Ok(installed(Some(42))), Some(why)), None);
    assert_eq!(
        status(Ok(Some(Approval::Password)), Ok(installed(Some(42))), None),
        Some(ok.clone())
    );
    for (state, why) in [
        (
            Ok(crate::service::State::Absent),
            "no subscriber is installed",
        ),
        (Ok(installed(None)), "installed but not running"),
        (Err(crate::Error::new("played")), "unknown (played)"),
    ] {
        let said = line(Approval::Open, state);
        assert!(said.contains(why), "{said}");
        assert!(
            said.contains(&format!("{} waits", unix::joiner(Approval::Open))),
            "{said}"
        );
        assert!(said.contains("aterm secrets agent on starts one"), "{said}");
    }
}

#[test]
fn git_credential_parses_options_and_operations() {
    assert_eq!(
        parse(&words("git-credential")),
        Ok(Command::GitCredential {
            dev_user: None,
            token_file: None,
            token_key: None,
            operation: None,
        })
    );
    assert_eq!(
        parse(&words("git-credential get")),
        Ok(Command::GitCredential {
            dev_user: None,
            token_file: None,
            token_key: None,
            operation: Some("get".into()),
        })
    );
    assert_eq!(
        parse(&words("git-credential --dev-user octo-cat get")),
        Ok(Command::GitCredential {
            dev_user: Some("octo-cat".into()),
            token_file: None,
            token_key: None,
            operation: Some("get".into()),
        })
    );
    assert_eq!(
        parse(&words(
            "git-credential --dev-user octo-cat --token-file /t/env --token-key GH_TOKEN get"
        )),
        Ok(Command::GitCredential {
            dev_user: Some("octo-cat".into()),
            token_file: Some("/t/env".into()),
            token_key: Some("GH_TOKEN".into()),
            operation: Some("get".into()),
        })
    );
    for refused in [
        "git-credential get extra",
        "git-credential --dev-user -bad get",
        "git-credential --dev-user a--b get",
        "git-credential --dev-user a --dev-user b get",
        "git-credential --unknown get",
        "git-credential --token-file relative/token get",
        "git-credential --token-file /a --token-file /b get",
        "git-credential --token-key GH_TOKEN get",
        "git-credential --token-file /t --token-key GH-TOKEN get",
    ] {
        assert!(parse(&words(refused)).is_err(), "{refused}");
    }
}

#[cfg(unix)]
#[test]
fn git_credential_routes_vault_tokens_and_quits_when_unusable() {
    use std::os::unix::fs::PermissionsExt as _;

    let root = aterm_tempfile::tempdir().expect("tempdir");
    let home = root.path().join("home");
    let secrets = home.join(".secrets");
    crate::fsx::ensure_private_dir(&secrets).expect("secrets dir");
    let dev_path = secrets.join("gh_access_token");
    let rel_path = secrets.join("gh_access_token_alabsystems");
    crate::fsx::write_atomic(&dev_path, b"ghp_dev_token_1\n", 0o600).expect("dev token");
    crate::fsx::write_atomic(&rel_path, b"ghp_rel_token_2\n", 0o600).expect("rel token");
    // The applied registry names both GitHub entries: the one path the helper resolves them by.
    let paths = crate::client::Paths::locate_for_home(&home);
    crate::fsx::ensure_private_dir(&paths.dir).expect("vault dir");
    let registry = |text: &str| {
        crate::fsx::write_atomic(&paths.dir.join("state.toml"), text.as_bytes(), 0o600)
            .expect("state.toml");
    };
    registry("[manifest]\ngh_access_token = 'a'\ngh_access_token_alabsystems = 'b'\n");

    let answer = |dev: &unix::DevToken<'_>,
                  dev_user: Option<&str>,
                  op: Option<&str>,
                  term: bool,
                  stdin: &[u8]|
     -> (String, Option<crate::need::GhProfile>) {
        let mut cursor = std::io::Cursor::new(stdin);
        let answer = unix::git_credential_reply(Some(&paths), dev_user, dev, op, term, &mut cursor);
        (
            String::from_utf8(answer.reply.as_slice().to_vec()).expect("utf8"),
            answer.missing,
        )
    };
    let ask_from = |dev: &unix::DevToken<'_>,
                    dev_user: Option<&str>,
                    op: Option<&str>,
                    term: bool,
                    stdin: &[u8]|
     -> String { answer(dev, dev_user, op, term, stdin).0 };
    let ask = |dev_user: Option<&str>, op: Option<&str>, term: bool, stdin: &[u8]| -> String {
        ask_from(&unix::DevToken::Vault, dev_user, op, term, stdin)
    };

    // A --token-file answers in the vault file's place, whole or by its --token-key line;
    // a loose file, or a key assigned twice, answers quit=1.
    let raw = root.path().join("profile_token");
    crate::fsx::write_atomic(&raw, b"ghp_profile_3\n", 0o600).expect("profile token");
    let env = root.path().join("profile.env");
    crate::fsx::write_atomic(&env, b"# x\nexport GH_TOKEN='ghp_keyed_4'\n", 0o600)
        .expect("profile env");
    let request = b"protocol=https\nhost=github.com\n\n";
    assert_eq!(
        ask_from(
            &unix::DevToken::File {
                file: &raw,
                key: None
            },
            Some("octocat"),
            Some("get"),
            false,
            request
        ),
        "username=octocat\npassword=ghp_profile_3\n"
    );
    assert_eq!(
        ask_from(
            &unix::DevToken::File {
                file: &env,
                key: Some("GH_TOKEN")
            },
            Some("octocat"),
            Some("get"),
            false,
            request
        ),
        "username=octocat\npassword=ghp_keyed_4\n"
    );
    crate::fsx::write_atomic(&env, b"GH_TOKEN=a\nGH_TOKEN=b\n", 0o600).expect("twice");
    assert_eq!(
        ask_from(
            &unix::DevToken::File {
                file: &env,
                key: Some("GH_TOKEN")
            },
            Some("octocat"),
            Some("get"),
            false,
            request
        ),
        "quit=1\n"
    );
    std::fs::set_permissions(&raw, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(
        ask_from(
            &unix::DevToken::File {
                file: &raw,
                key: None
            },
            Some("octocat"),
            Some("get"),
            false,
            request
        ),
        "quit=1\n"
    );

    // Without a known login, an unnamed github.com lookup gets the dev token as x-access-token.
    assert_eq!(
        ask(
            None,
            Some("get"),
            false,
            b"protocol=https\nhost=github.com\n\ntrailing=ignored"
        ),
        "username=x-access-token\npassword=ghp_dev_token_1\n"
    );
    // gist.github.com and default port / trailing dot route the same way.
    assert_eq!(
        ask(
            Some("alice"),
            Some("get"),
            false,
            b"protocol=HTTPS\nhost=gist.github.com.:0443\n\n"
        ),
        "username=alice\npassword=ghp_dev_token_1\n"
    );
    // alabsystems routes to gh_access_token_alabsystems regardless of dev user.
    assert_eq!(
        ask(
            Some("alice"),
            Some("get"),
            false,
            b"protocol=https\nhost=github.com\nusername=ALABSYSTEMS\n\n"
        ),
        "username=alabsystems\npassword=ghp_rel_token_2\n"
    );

    // NEGATIVE CONTROL: a token file the registry does not name is no token. git's request
    // fails and the helper names gh:dev to file in the need ledger; the same with no registry.
    let unnamed = b"protocol=https\nhost=github.com\n\n";
    registry("[manifest]\ngh_access_token_alabsystems = 'b'\n");
    assert_eq!(
        answer(&unix::DevToken::Vault, None, Some("get"), false, unnamed),
        ("quit=1\n".to_owned(), Some(crate::need::GhProfile::Dev))
    );
    std::fs::remove_file(paths.dir.join("state.toml")).unwrap();
    assert_eq!(
        answer(&unix::DevToken::Vault, None, Some("get"), false, unnamed),
        ("quit=1\n".to_owned(), Some(crate::need::GhProfile::Dev))
    );
    // A --token-file is no registry token: its absence files nothing.
    assert_eq!(
        answer(
            &unix::DevToken::File {
                file: &root.path().join("absent"),
                key: None
            },
            None,
            Some("get"),
            false,
            unnamed
        ),
        ("quit=1\n".to_owned(), None)
    );
    registry("[manifest]\ngh_access_token = 'a'\ngh_access_token_alabsystems = 'b'\n");

    // A named login answers only where --dev-user or init.toml's [github] account names it;
    // stranger usernames, non-get ops, other hosts and plain HTTP get no answer.
    crate::fsx::write_atomic(&dev_path, b"ghp_rotated_token\n", 0o600).unwrap();
    assert_eq!(
        ask(
            None,
            Some("get"),
            false,
            b"protocol=https\nhost=github.com\nusername=octocat\n\n"
        ),
        ""
    );
    crate::fsx::write_atomic(
        &home.join(".config").join("aterm").join("init.toml"),
        b"[github]\naccount = \"octocat\"\n",
        0o600,
    )
    .unwrap();
    assert_eq!(
        ask(
            None,
            Some("get"),
            false,
            b"protocol=https\nhost=github.com\nusername=octocat\n\n"
        ),
        "username=octocat\npassword=ghp_rotated_token\n"
    );
    for silent in [
        ask(
            Some("octocat"),
            Some("get"),
            false,
            b"protocol=https\nhost=github.com\nusername=stranger\n\n",
        ),
        ask(
            Some("octocat"),
            Some("store"),
            false,
            b"protocol=https\nhost=github.com\nusername=octocat\npassword=x\n\n",
        ),
        ask(
            Some("octocat"),
            Some("erase"),
            false,
            b"protocol=https\nhost=github.com\n\n",
        ),
        ask(
            Some("octocat"),
            None,
            false,
            b"protocol=https\nhost=github.com\n\n",
        ),
        ask(
            Some("octocat"),
            Some("get"),
            false,
            b"protocol=http\nhost=github.com\n\n",
        ),
        ask(
            Some("octocat"),
            Some("get"),
            false,
            b"protocol=https\nhost=gitlab.com\n\n",
        ),
        ask(
            Some("octocat"),
            Some("get"),
            false,
            b"protocol=https\nhost=github.com:8443\n\n",
        ),
    ] {
        assert_eq!(silent, "");
    }
    // A request whose last line ends without its LF was cut short (here a `:8443` port git
    // never sent past): no answer. Control: the same lines ended, at the end of input as at a
    // blank line, answer.
    assert_eq!(
        ask(
            Some("octocat"),
            Some("get"),
            false,
            b"protocol=https\nhost=github.com"
        ),
        ""
    );
    for ended in [
        &b"protocol=https\nhost=github.com\n"[..],
        b"protocol=https\r\nhost=github.com\r\n",
    ] {
        assert_eq!(
            ask(Some("octocat"), Some("get"), false, ended),
            "username=octocat\npassword=ghp_rotated_token\n"
        );
    }

    // Terminal stdin, loose mode, empty/malformed content, symlink or missing token -> quit=1.
    assert_eq!(
        ask(
            Some("octocat"),
            Some("get"),
            true,
            b"protocol=https\nhost=github.com\n\n"
        ),
        "quit=1\n"
    );
    std::fs::set_permissions(&dev_path, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(
        ask(
            None,
            Some("get"),
            false,
            b"protocol=https\nhost=github.com\n\n"
        ),
        "quit=1\n"
    );
    crate::fsx::write_atomic(&dev_path, b"   \n", 0o600).unwrap();
    assert_eq!(
        ask(
            None,
            Some("get"),
            false,
            b"protocol=https\nhost=github.com\n\n"
        ),
        "quit=1\n"
    );
    crate::fsx::write_atomic(&dev_path, b"two words\n", 0o600).unwrap();
    assert_eq!(
        ask(
            None,
            Some("get"),
            false,
            b"protocol=https\nhost=github.com\n\n"
        ),
        "quit=1\n"
    );
    std::fs::remove_file(&dev_path).unwrap();
    assert_eq!(
        ask(
            None,
            Some("get"),
            false,
            b"protocol=https\nhost=github.com\nusername=octocat\n\n"
        ),
        "quit=1\n"
    );
    std::fs::remove_file(&rel_path).unwrap();
    assert_eq!(
        answer(
            &unix::DevToken::Vault,
            None,
            Some("get"),
            false,
            b"protocol=https\nhost=github.com\nusername=alabsystems\n\n"
        ),
        ("quit=1\n".to_owned(), Some(crate::need::GhProfile::Release))
    );
}

#[cfg(unix)]
#[test]
fn git_credential_files_a_missing_token_once_under_its_agh_purpose() {
    use crate::need::{GhProfile, Purpose, Want};

    let root = aterm_tempfile::tempdir().expect("tempdir");
    let paths = crate::client::Paths::locate_for_home(&root.path().join("home"));
    let file = |profile: GhProfile| -> String {
        let mut err = Vec::new();
        unix::file_missing(&paths, profile, &mut err);
        String::from_utf8(err).expect("utf8")
    };
    for (profile, purpose) in [
        (GhProfile::Dev, Purpose::AghDev),
        (GhProfile::Release, Purpose::AghRelease),
    ] {
        let want = Want::Gh(profile);
        let first = file(profile);
        let again = file(profile);
        let rows: Vec<_> = crate::need::list(&paths)
            .expect("ledger")
            .into_iter()
            .filter(|need| need.want == want)
            .collect();
        assert_eq!(rows.len(), 1, "one row per want: {rows:?}");
        assert_eq!(rows[0].purpose, purpose);
        let id = aterm_codec::hex::encode_lower(&rows[0].id);
        for line in [&first, &again] {
            assert!(line.contains(&want.key()) && line.contains(&id), "{line}");
        }
    }
}
