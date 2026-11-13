// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn http_actions_require_complete_headers_and_the_dashboard_origin() {
    let local: SocketAddr = "127.0.0.1:8741".parse().unwrap();
    let request = |fields: &str| format!("POST /use?auto=1 HTTP/1.1\r\n{fields}\r\n");
    let valid = request("Host: localhost:8741\r\nOrigin: http://localhost:8741\r\n");
    let mut input = valid.as_bytes();
    let head = read_request_head(|buf| {
        let len = buf.len().min(3);
        input.read(&mut buf[..len])
    })
    .unwrap();
    assert_eq!(
        admit_request(&head, DEFAULT_SERVE_BIND, local),
        Some(("POST", "/use?auto=1"))
    );
    for fields in [
        "Host: localhost:8741\r\n",
        "Host: localhost:8741\r\nOrigin: http://elsewhere.test\r\n",
        "Host: elsewhere.test\r\nOrigin: http://elsewhere.test\r\n",
        "Host: localhost:8741\r\nOrigin: null\r\n",
        "Host: localhost:8741\r\nOrigin: http://localhost:8741\r\nOrigin: http://elsewhere.test\r\n",
        "Host: localhost:8741\r\nHost: elsewhere.test\r\nOrigin: http://localhost:8741\r\n",
        "Host: localhost:8741\r\nOrigin: http://localhost:8741\r\nSec-Fetch-Site: cross-site\r\n",
        "Host: localhost:8741\r\nOrigin: http://localhost:8741\r\nSec-Fetch-Site: same-site\r\n",
        "Host: localhost:8741\r\n Origin: http://localhost:8741\r\n",
        "Host: localhost:8741\r\nX-Aterm-Accounts-Action: 0\r\n",
        "Host: localhost:8741\r\nX-Aterm-Accounts-Action: 1\r\nOrigin: http://elsewhere.test\r\n",
    ] {
        assert_eq!(
            admit_request(&request(fields), DEFAULT_SERVE_BIND, local),
            None,
            "{fields}"
        );
    }
    let explicit = request("Host: localhost:8741\r\nX-Aterm-Accounts-Action: 1\r\n");
    assert!(admit_request(&explicit, DEFAULT_SERVE_BIND, local).is_some());
    let get = "GET /fleet-accounts.toml HTTP/1.1\r\nHost: localhost:8741\r\n\r\n";
    assert_eq!(
        admit_request(get, DEFAULT_SERVE_BIND, local),
        Some(("GET", "/fleet-accounts.toml"))
    );
    assert!(
        admit_request(
            &get.replace("localhost:8741", "elsewhere.test"),
            DEFAULT_SERVE_BIND,
            local
        )
        .is_none()
    );
    for (bind, addr, host) in [
        ("0.0.0.0:8741", "192.0.2.1:8741", "192.0.2.1:8741"),
        ("machine.local:0", "192.0.2.1:8741", "machine.local:8741"),
        ("[::1]:8741", "[::1]:8741", "[::1]:8741"),
        ("127.0.0.1:80", "127.0.0.1:80", "localhost"),
    ] {
        let req = request(&format!("Host: {host}\r\nOrigin: HTTP://{host}\r\n"));
        assert!(
            admit_request(&req, bind, addr.parse().unwrap()).is_some(),
            "{req}"
        );
    }
    let wildcard_host = request("Host: 0.0.0.0:8741\r\nOrigin: http://0.0.0.0:8741\r\n");
    assert!(
        admit_request(
            &wildcard_host,
            "0.0.0.0:8741",
            "192.0.2.1:8741".parse().unwrap()
        )
        .is_none()
    );
    for raw in [
        b"POST /sync HTTP/1.1\r\nHost: localhost:8741\r\n".to_vec(),
        vec![b'x'; 8192],
    ] {
        let mut input = raw.as_slice();
        assert!(read_request_head(|buf| input.read(buf)).is_err());
    }
    let mut padded = format!(
        "GET / HTTP/1.1\r\nHost: localhost:8741\r\nX-Padding: {}\r\n\r\nbody",
        "a".repeat(8000)
    );
    let mut input = padded.as_bytes();
    let head = read_request_head(|buf| input.read(buf)).unwrap();
    assert!(!head.ends_with("body"));
    assert!(admit_request(&head, DEFAULT_SERVE_BIND, local).is_some());
    let padding = "a".repeat(8192 - head.len() + 8000);
    let exact = head.replace(&"a".repeat(8000), &padding);
    assert_eq!(exact.len(), 8192);
    let mut input = exact.as_bytes();
    assert!(read_request_head(|buf| input.read(buf)).is_ok());
    let oversized = exact.replacen("X-Padding: ", "X-Padding: a", 1);
    let mut input = oversized.as_bytes();
    assert!(read_request_head(|buf| input.read(buf)).is_err());
    padded.insert_str(0, "not-http ");
    assert!(admit_request(&padded, DEFAULT_SERVE_BIND, local).is_none());
}

const SAMPLE_CSV: &str = "\
m,email,computer,,claude,Open AI,google,meta,Payment,notes,,
,alpha@example.test,m8,,1,1,1,,,\"logged in, seems oK?\",,
,bravo.m8@example.test,m8.2,,1,,,,a card,,,
,charlie@example.test,*m12,,1,,1,,,claude account payment disabled. roll off this account.,,
,delta@example.test,m106,m38,1,,,,a card,delta,,
,echo-m31@example.test,m31,,0,1,,,,banned from Claude Code,,
,foxtrot.m38.106.2@example.test,m38.2,,1,,,,,a note,,
,,,,102,,\"$20,400\",,,,,
";

#[test]
fn parses_accounting_csv_and_assigns_machine_slots_and_shared_classes() {
    let mut boundary = b"email,computer,claude,notes\nwithin@example.test,m30,1,".to_vec();
    boundary.resize(SOURCE_TEXT_MAX_BYTES, b'x');
    let admitted = decode_source_text(boundary.clone(), "boundary.csv").unwrap();
    assert_eq!(admitted.len(), SOURCE_TEXT_MAX_BYTES);
    let fleet = parse_accounting_csv(&admitted, "boundary.csv").unwrap();
    assert_eq!(fleet.accounts.len(), 1);
    assert_eq!(fleet.accounts[0].email, "within@example.test");
    boundary.push(b'\n');
    assert!(decode_source_text(boundary, "oversized.csv").is_err());
    assert!(decode_source_text(vec![0xff], "invalid-utf8.csv").is_err());

    let owned = aterm_tempfile::Builder::new()
        .prefix("aterm-account-source-")
        .tempdir()
        .expect("exclusive source fixtures");
    let source = owned.path().join("source.csv");
    fs::write(&source, &admitted).unwrap();
    let text = read_local_source_text(&source).expect("the exact boundary is admitted");
    assert_eq!(text, admitted);
    let fleet = parse_accounting_csv(&text, "source.csv").unwrap();
    assert_eq!(fleet.accounts[0].email, "within@example.test");
    fs::OpenOptions::new()
        .write(true)
        .open(&source)
        .unwrap()
        .set_len((SOURCE_TEXT_MAX_BYTES + 1) as u64)
        .unwrap();
    assert!(read_local_source_text(&source).is_err());
    fs::write(&source, [0xff]).unwrap();
    assert!(read_local_source_text(&source).is_err());
    assert!(read_local_source_text(owned.path()).is_err());
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt as _;
        fs::write(&source, "ordinary@example.test").unwrap();
        let link = owned.path().join("source-link");
        std::os::unix::fs::symlink(&source, &link).unwrap();
        assert!(read_local_source_text(&link).is_err());
        let fifo = owned.path().join("source.fifo");
        let name = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
        // SAFETY: `name` is a NUL-terminated path in this test's exclusive directory, and
        // mkfifo retains no pointer.
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        assert!(read_local_source_text(&fifo).is_err());
        assert_eq!(
            read_local_source_text(&source).unwrap(),
            "ordinary@example.test"
        );
    }

    for ending in ["\n", "\r\n"] {
        for marker in [
            "banned",
            "roll off this account",
            "payment disabled",
            "deactivated",
            "SuSpEnDeD",
        ] {
            let text = format!(
                "email,computer,claude,notes{ending}\"blocked@example.test\",m20,1,\"reviewed{ending}{marker}\"{ending}ok@example.test,m21,1,usable{ending}"
            );
            let parsed = parse_accounting_csv(&text, "captured.csv").unwrap();
            assert_eq!(parsed.accounts.len(), 1, "{marker}: {ending:?}");
            assert_eq!(parsed.accounts[0].email, "ok@example.test");
            assert_eq!(parsed.accounts[0].label, "m21");
        }
    }

    let text = concat!(
        "\"email\",computer,claude,notes\r\n",
        "\"first@example.test\",\"m22\",\"1\",\"ready, \"\"quoted\"\"\r\n",
        "phantom@example.test,m99,1,still a note\r\n",
        "ready\"\r\n",
        "second@example.test,m23,1,\"ordinary\""
    );
    let parsed = parse_accounting_csv(text, "captured.csv").unwrap();
    assert_eq!(
        parsed
            .accounts
            .iter()
            .map(|a| a.label.as_str())
            .collect::<Vec<_>>(),
        ["m22", "m23"]
    );
    assert_eq!(
        parse_csv_line("\"a,b\",\"a\"\"b\",\"line1\r\nline2\","),
        ["a,b", "a\"b", "line1\r\nline2", ""]
    );

    for bad in [
        "bad@example.test,m24,1,\"unterminated\nnext line",
        "bad@example.test,m24,1,unquoted\"quote",
        "bad@example.test,m24,1,\"closed\"trailing",
        "bad@example.test,m24,1,\"closed\"\"",
    ] {
        let text = format!("email,computer,claude,notes\nok@example.test,m21,1,usable\n{bad}");
        assert!(parse_accounting_csv(&text, "malformed.csv").is_err());
    }
    assert!(parse_accounting_csv("\"email,computer,claude", "header.csv").is_err());

    let fleet = parse_accounting_csv(SAMPLE_CSV, "sample.csv").expect("valid csv");
    assert_eq!(fleet.accounts.len(), 4);

    let m8 = fleet.accounts_for_machine("m8");
    assert_eq!(m8.len(), 2);
    assert_eq!(m8[0].label, "m8");
    assert_eq!(m8[0].email, "alpha@example.test");
    assert_eq!(m8[0].class, Class::Preferred);
    assert_eq!(m8[1].label, "m8-2");
    assert_eq!(m8[1].email, "bravo.m8@example.test");
    assert_eq!(m8[1].class, Class::Preferred);

    let m38 = fleet.accounts_for_machine("m38");
    assert_eq!(m38.len(), 2);
    assert!(m38.iter().all(|a| a.class == Class::Pool));
    let m106 = fleet.accounts_for_machine("m106");
    assert_eq!(m106.len(), 2);

    let toml = fleet.to_toml();
    let reparsed = FleetRoster::from_toml(&toml, "fleet-accounts.toml").expect("valid toml");
    assert_eq!(reparsed.accounts, fleet.accounts);

    let mut fleet = fleet.clone();
    let controls: String = (0u8..=31).map(char::from).collect();
    fleet.accounts[0].browser = Some(format!("{controls}\u{7f}\"\\ profile \u{a0} \u{1f680}"));
    let reparsed = FleetRoster::from_toml(&fleet.to_toml(), "captured.toml").unwrap();
    assert_eq!(reparsed.accounts, fleet.accounts);

    let mut kimi_fleet = fleet.clone();
    for acct in &mut kimi_fleet.accounts {
        acct.vendor.region = Some(Region::Global);
    }
    let reparsed_kimi =
        FleetRoster::from_toml_for(Cli::Kimi, &kimi_fleet.to_toml(), "kimi.toml").unwrap();
    assert_eq!(reparsed_kimi.accounts, kimi_fleet.accounts);

    let mut codex_fleet = fleet.clone();
    for acct in &mut codex_fleet.accounts {
        acct.vendor.flow = Some(Flow::Device);
    }
    let reparsed_codex =
        FleetRoster::from_toml_for(Cli::Codex, &codex_fleet.to_toml(), "codex.toml").unwrap();
    assert_eq!(reparsed_codex.accounts, codex_fleet.accounts);

    let duplicates = "email,computer,claude\none@example.test,m8,1\ntwo@example.test,m8,1\n";
    let fleet = parse_accounting_csv(duplicates, "captured.csv").unwrap();
    assert_eq!(fleet.accounts[0].label, "m8");
    assert_eq!(fleet.accounts[1].label, "m8-2");

    let long = (100_000_000..100_000_008)
        .map(|n| n.to_string())
        .collect::<Vec<_>>()
        .join(".");
    let csv = format!("email,computer,claude\none@example.test,m{long},1\n");
    assert!(parse_accounting_csv(&csv, "captured.csv").is_err());
    let boundary = "m100000000.100000001.100000002.100000003.100000004.10000000";
    let csv = format!("email,computer,claude\none@example.test,{boundary},1\n");
    let fleet = parse_accounting_csv(&csv, "captured.csv").unwrap();
    assert_eq!(fleet.accounts[0].label.len(), accounts::LABEL_MAX);
    let csv = format!("{csv}two@example.test,{boundary},1\n");
    assert!(parse_accounting_csv(&csv, "captured.csv").is_err());
}

#[test]
fn fleet_sync_keeps_an_explicit_claude_alias_only_for_the_same_login_address() {
    let w = World::new();
    let store = accounts::Store::at(&w.state);
    let mut fleet = parse_accounting_csv(
        "email,computer,claude\nfirst@example.test,m8,1\nalias@example.test,m8,1\n",
        "aliases.csv",
    )
    .unwrap();
    let mut existing = build_machine_roster(&fleet, "m8", &store, None, None);
    let alias = existing
        .rows
        .iter_mut()
        .find(|row| row.email.as_deref() == Some("alias@example.test"))
        .unwrap();
    alias.identity_email = Some("canonical@example.test".into());
    let label = alias.label.clone();
    let synced = build_machine_roster(
        &fleet,
        "m8",
        &store,
        Some(&existing),
        Some("canonical@example.test"),
    );
    assert_eq!(synced.rows.len(), 2, "no duplicate canonical account");
    assert_eq!(synced.row_for_login(None).unwrap().label, label);
    assert_eq!(
        synced.row(&label).unwrap().identity_email.as_deref(),
        Some("canonical@example.test")
    );
    assert_eq!(
        Roster::parse(&format_roster_toml(&synced).unwrap()).unwrap(),
        synced
    );

    let mut omitted = fleet.clone();
    omitted.accounts.retain(|row| row.label != label);
    let unlisted = build_machine_roster(
        &omitted,
        "m8",
        &store,
        Some(&synced),
        Some("canonical@example.test"),
    );
    assert_eq!(unlisted.rows.len(), 2);
    let own = unlisted.row_for_login(None).unwrap();
    assert_eq!(own.label, label);
    assert_eq!(own.email.as_deref(), Some("alias@example.test"));
    assert_eq!(
        own.identity_email.as_deref(),
        Some("canonical@example.test")
    );

    fleet
        .accounts
        .iter_mut()
        .find(|row| row.label == label)
        .unwrap()
        .email = "replacement@example.test".into();
    let changed = build_machine_roster(&fleet, "m8", &store, Some(&synced), None);
    assert_eq!(changed.row(&label).unwrap().identity_email, None);
}

#[test]
fn builds_machine_roster_adopting_current_default_profile_email() {
    let fleet = parse_accounting_csv(SAMPLE_CSV, "sample.csv").expect("valid csv");
    let tmp = crate::supervise::ScratchDir::new("sync", "roster");
    let store = accounts::Store::at(&tmp);

    let roster = build_machine_roster(&fleet, "m8", &store, None, Some("bravo.m8@example.test"));
    assert_eq!(roster.rows.len(), 2);
    assert_eq!(roster.rows[0].label, "m8-2");
    assert_eq!(roster.rows[0].dir, None);
    assert_eq!(roster.rows[1].label, "m8");
    assert_eq!(
        roster.rows[1].dir.as_deref(),
        Some(
            store
                .login_dir("m8")
                .expect("admissible label")
                .to_string_lossy()
                .as_ref()
        )
    );

    let toml = format_roster_toml(&roster).expect("valid roster text");
    let parsed = Roster::parse(&toml).expect("roster admits");
    assert_eq!(parsed, roster);

    let mut duplicate = roster.clone();
    duplicate.rows.push(duplicate.rows[0].clone());
    assert!(format_roster_toml(&duplicate).is_err());

    let owned = aterm_tempfile::Builder::new()
        .prefix("aterm-sync-publication-")
        .suffix(".noindex")
        .tempdir()
        .expect("exclusive owned files");
    let root = owned.path();
    let store = accounts::Store::at(&root.join("state"));
    let config = root.join("config");
    fs::create_dir(&config).unwrap();
    let roster_path = config.join(accounts::ACCOUNTS_FILE);
    let target = if cfg!(unix) {
        root.join("dotfiles").join(accounts::ACCOUNTS_FILE)
    } else {
        roster_path.clone()
    };
    fs::create_dir_all(target.parent().unwrap()).unwrap();
    let login = root.join("existing-login").to_string_lossy().into_owned();
    let previous = Roster {
        rows: vec![
            Row {
                label: "m8".into(),
                dir: Some(login.clone()),
                email: Some("one@example.test".into()),
                browser: Some("kept profile".into()),
                ..Row::default()
            },
            Row {
                label: "m8-2".into(),
                email: Some("two@example.test".into()),
                ..Row::default()
            },
        ],
    };
    fs::write(&target, format_roster_toml(&previous).unwrap()).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(&target, fs::Permissions::from_mode(0o640)).unwrap();
        std::os::unix::fs::symlink(&target, &roster_path).unwrap();
    }
    let csv = "email,computer,claude\none@example.test,m8,1\ntwo@example.test,m8,1\n";
    let fleet = parse_accounting_csv(csv, "captured.csv").unwrap();
    let now = 1_800_000_000;
    let mut note = None;
    store
        .transact(now, &mut note, |state| {
            let until = now + 2 * 3600;
            let mut rows = accounts::note_wall(
                state,
                ACCOUNTS_OWN_LABEL,
                accounts::WEEK_LIMIT,
                Some(until),
                now - 60,
                now,
            );
            assert_eq!(
                accounts::claim_reset(
                    state,
                    ACCOUNTS_OWN_LABEL,
                    (accounts::WEEK_LIMIT, until),
                    "s-before-adoption",
                    41,
                    "start",
                    now - 30
                ),
                accounts::ResetClaimed::Taken
            );
            rows.extend(accounts::reset_outcome(
                state,
                ACCOUNTS_OWN_LABEL,
                until,
                "s-before-adoption",
                &accounts::ResetDone::Used {
                    kind: aterm_phase::ResetKind::Full,
                    left: None,
                },
                now - 10,
            ));
            rows.extend(accounts::note_wall(
                state,
                ACCOUNTS_OWN_LABEL,
                accounts::SESSION_LIMIT,
                Some(until),
                now - 5,
                now,
            ));
            Ok(rows)
        })
        .expect("observed before adoption");
    let empty_walls = BTreeMap::new();
    let saved = persist_accounts_sync(
        &store,
        &roster_path,
        (&fleet, OptIn::Named),
        ("m8", Some("two@example.test")),
        &empty_walls,
        now,
        (&[], &[]),
    )
    .expect("publish captured inputs");
    let (adopted, _) = store.load(now).expect("adopted state");
    let target_account = adopted.account("m8-2");
    assert!(
        !target_account.walls.contains_key(accounts::WEEK_LIMIT),
        "a scoped clear is not replayed as a standing wall"
    );
    assert!(
        target_account.walls[accounts::SESSION_LIMIT]
            .stands_until()
            .is_some(),
        "a different live wall is still adopted"
    );
    let saved_roster = Roster::load(&roster_path).unwrap();
    assert_eq!(saved_roster.row_for_login(None).unwrap().label, "m8-2");
    assert_eq!(
        saved_roster.row("m8").unwrap().dir.as_deref(),
        Some(login.as_str())
    );
    assert_eq!(
        saved_roster.row("m8").unwrap().browser.as_deref(),
        Some("kept profile")
    );
    assert_eq!(Roster::load(&target).unwrap(), saved_roster);
    assert!(
        store.try_acquire().unwrap().is_some(),
        "the publication released its lock"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        assert!(roster_path.symlink_metadata().unwrap().is_symlink());
        assert_eq!(
            fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o640
        );
        assert_eq!(
            fs::metadata(&saved.fleet_path)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    persist_accounts_sync(
        &store,
        &roster_path,
        (&fleet, OptIn::Named),
        ("m8", None),
        &empty_walls,
        now,
        (&[], &[]),
    )
    .expect("preserve the current default without a profile hint");
    assert_eq!(
        Roster::load(&roster_path)
            .unwrap()
            .row_for_login(None)
            .unwrap()
            .label,
        "m8-2"
    );
    let before_roster = fs::read(&target).unwrap();
    let before_fleet = fs::read(&saved.fleet_path).unwrap();
    let malformed = b"[[account]\nlabel = \"broken\"\n";
    fs::write(&target, malformed).unwrap();
    assert!(
        persist_accounts_sync(
            &store,
            &roster_path,
            (&fleet, OptIn::Named),
            ("m8", None),
            &empty_walls,
            now,
            (&[], &[])
        )
        .is_err()
    );
    assert_eq!(fs::read(&target).unwrap(), malformed);
    assert_eq!(fs::read(&saved.fleet_path).unwrap(), before_fleet);
    fs::write(&target, &before_roster).unwrap();
    let mut changed = fleet.clone();
    changed.accounts[0].browser = Some("new profile".into());
    for path in [store.state_file(), store.logins_file()] {
        let before = fs::read(&path).unwrap();
        fs::write(&path, "{\"schema\":999}").unwrap();
        assert!(
            persist_accounts_sync(
                &store,
                &roster_path,
                (&changed, OptIn::Named),
                ("m8", None),
                &empty_walls,
                now,
                (&[], &[]),
            )
            .is_err()
        );
        assert_eq!(fs::read(&target).unwrap(), before_roster);
        assert_eq!(fs::read(&saved.fleet_path).unwrap(), before_fleet);
        assert_eq!(fs::read(&path).unwrap(), b"{\"schema\":999}");
        fs::write(&path, before).unwrap();
    }
    let mut duplicated_fleet = fleet.clone();
    duplicated_fleet.accounts.push(fleet.accounts[0].clone());
    assert!(
        persist_accounts_sync(
            &store,
            &roster_path,
            (&duplicated_fleet, OptIn::Named),
            ("m8", None),
            &empty_walls,
            now,
            (&[], &[]),
        )
        .is_err()
    );
    assert_eq!(fs::read(&target).unwrap(), before_roster);
    assert_eq!(fs::read(&saved.fleet_path).unwrap(), before_fleet);

    fs::remove_file(&saved.fleet_path).unwrap();
    fs::create_dir(&saved.fleet_path).unwrap();
    assert!(
        persist_accounts_sync(
            &store,
            &roster_path,
            (&changed, OptIn::Named),
            ("m8", None),
            &empty_walls,
            now,
            (&[], &[]),
        )
        .is_err()
    );
    assert_eq!(
        fs::read(&target).unwrap(),
        before_roster,
        "an auxiliary publication failure preserves the admitted roster"
    );
    assert!(saved.fleet_path.is_dir());
    fs::remove_dir(&saved.fleet_path).unwrap();
    persist_accounts_sync(
        &store,
        &roster_path,
        (&fleet, OptIn::Named),
        ("m8", None),
        &empty_walls,
        now,
        (&[], &[]),
    )
    .unwrap();

    let reset = Roster::load(&roster_path).unwrap();

    let source = root.join("source.csv");
    fs::write(&source, csv).unwrap();
    fs::remove_file(store.wake_path()).unwrap();
    fs::create_dir(store.wake_path()).unwrap();
    let env = Env {
        state: root.join("state"),
        aterm_state: Some(root.join("aterm-state")),
        cwd: root.to_path_buf(),
        home: None,
        claude_dir: None,
        claude_config_unusable: None,
        claude_profile: None,
        claude_custom_oauth: false,
        claude_session: None,
        claude_pid: None,
        codex_thread: None,
        agy_conversation: None,
        codex_home: None,
        xdg_config_home: None,
        codex_login: None,
        now,
        tz_s: 0,
        sid: String::new(),
        sock: None,
        config: Some(config.join("aterm.toml")),
        claude_tmp: None,
        claude_running: None,
    };
    // A sync that finds what is already there writes nothing, the wake marker included.
    let (mut out, mut err) = (Vec::new(), Vec::new());
    assert_eq!(
        run_accounts_sync(
            &env,
            None,
            source.to_str(),
            Some("m8"),
            false,
            &mut out,
            &mut err
        ),
        ExitCode::SUCCESS,
        "{}",
        String::from_utf8_lossy(&err)
    );
    // One that writes a file and cannot write the marker says so.
    fs::remove_file(&saved.fleet_path).unwrap();
    let mut out = Vec::new();
    let mut err = Vec::new();
    assert_eq!(
        run_accounts_sync(
            &env,
            None,
            source.to_str(),
            Some("m8"),
            false,
            &mut out,
            &mut err
        ),
        ExitCode::FAILURE
    );
    assert!(out.is_empty());
    assert!(!err.is_empty());
    assert!(store.wake_path().is_dir());
    assert_eq!(Roster::load(&target).unwrap(), reset);
}

#[test]
fn normalizes_machine_ids_from_hostnames() {
    assert_eq!(
        normalize_machine_id("m8-Macbook-Pro-Max.local"),
        Some("m8".to_owned())
    );
    assert_eq!(
        normalize_machine_id("MacbookAir-m27-M5-32GB-4TB.local"),
        Some("m27".to_owned())
    );
    assert_eq!(
        normalize_machine_id("MacbookAir-M27-M5-32GB-4TB"),
        Some("m27".to_owned())
    );
    assert_eq!(
        normalize_machine_id("MacbookAir-M8-32GB.local"),
        Some("m8".to_owned())
    );
    assert_eq!(
        normalize_machine_id("Andrew's MacBook Pro (m15)"),
        Some("m15".to_owned())
    );
    assert_eq!(normalize_machine_id("MacBook-Pro-M4-Max.local"), None);
    assert_eq!(normalize_machine_id("MacBook-Pro-m4-Max.local"), None);
    assert_eq!(normalize_machine_id("MacBook-Air-m3-32GB.local"), None);
    assert_eq!(normalize_machine_id("M109"), Some("m109".to_owned()));
    assert_eq!(normalize_machine_id("*m12"), None);
    assert_eq!(normalize_machine_id("macbook"), None);
    assert_eq!(
        email_machines_and_slot("sol-m27@example.test"),
        (vec!["m27".to_owned()], None)
    );
    assert_eq!(
        email_machines_and_slot("sun.m27.2@example.test"),
        (vec!["m27".to_owned()], Some(2))
    );
    assert_eq!(
        email_machines_and_slot("solar.m27.3@example.test"),
        (vec!["m27".to_owned()], Some(3))
    );
    assert_eq!(
        email_machines_and_slot("owner+m27.2@example.test"),
        (vec!["m27".to_owned()], Some(2))
    );
    assert_eq!(
        email_machines_and_slot("owner+m27@example.test"),
        (vec!["m27".to_owned()], None)
    );
    assert_eq!(
        email_machines_and_slot("sol-m27+2@example.test"),
        (vec!["m27".to_owned()], Some(2))
    );
    assert_eq!(
        email_machines_and_slot("sun.m27.2+claude@example.test"),
        (vec!["m27".to_owned()], Some(2)),
        "CONTROL: trailing service tag does not clobber base machine or slot"
    );
    assert_eq!(
        email_machines_and_slot("owner+claude@example.test"),
        (Vec::<String>::new(), None),
        "CONTROL: non-machine plus tag yields neither machine nor slot"
    );
}

#[test]
fn this_macs_id_is_its_first_host_name_that_names_a_machine_or_none() {
    let names = |names: &[&str]| names.iter().map(|n| (*n).to_owned()).collect::<Vec<_>>();
    let past_the_match = std::iter::once_with(|| -> String {
        panic!("a host name named the machine, so no later one is asked")
    });
    assert_eq!(
        machine_id_among(
            names(&["MacBook-Pro-M4-Max.local\n", "m15-MacBook-M5-Max\n"])
                .into_iter()
                .chain(past_the_match),
        ),
        Some("m15".to_owned())
    );
    assert_eq!(machine_id_among(names(&["MacBook-Pro-M4-Max.local"])), None);
    assert_eq!(machine_id_among(names(&[])), None);
}

/// A transcript longer than the window's per-read fold budget (`footer::FOLD_BUDGET`) is read
/// whole: its newest walls and `/login` rows lie past that many bytes, and booking them needs the
/// `session_context` at its start. A last line with no newline yet (one Claude Code is still
/// writing) books nothing until it is whole.
#[test]
fn transcript_walls_past_the_fold_budget_are_booked_and_an_unfinished_last_line_is_not() {
    use std::io::Write as _;

    let dir = aterm_tempfile::Builder::new()
        .prefix("aterm-walls-long-")
        .tempdir()
        .expect("tempdir");
    let proj = dir.path().join("projects").join("ws");
    fs::create_dir_all(&proj).unwrap();
    let context = |who: &str| {
        format!(
            "{{\"type\":\"attachment\",\"attachment\":{{\"type\":\"session_context\",\"context\":{{\"userEmail\":\"The user's email address is {who}@example.test. Use it only to identify the user.\"}}}}}}\n"
        )
    };
    let wall = |at: &str, resets: u64| {
        format!(
            "{{\"type\":\"assistant\",\"timestamp\":\"{at}\",\"isApiErrorMessage\":true,\"error\":\"rate_limit\",\"message\":{{\"model\":\"<synthetic>\",\"content\":[{{\"type\":\"text\",\"text\":\"You've hit your weekly limit\"}}]}},\"quotaLimits\":{{\"status\":\"rejected\",\"resetsAt\":{resets},\"rateLimitType\":\"seven_day\"}}}}\n"
        )
    };
    let login = "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"<local-command-stdout>Login successful</local-command-stdout>\"}}\n";
    let filler = format!(
        "{{\"type\":\"progress\",\"data\":\"{}\"}}\n",
        "x".repeat(1 << 20)
    );
    let line_len = filler.len() as u64;
    let pad = usize::try_from(super::super::footer::FOLD_BUDGET / line_len + 2).unwrap();
    let long = proj.join("long.jsonl");
    {
        let mut out = io::BufWriter::new(fs::File::create(&long).unwrap());
        out.write_all(context("first").as_bytes()).unwrap();
        for _ in 0..pad {
            out.write_all(filler.as_bytes()).unwrap();
        }
        out.write_all(wall("2026-10-20T06:50:53Z", 1_792_767_600).as_bytes())
            .unwrap();
        out.write_all(login.as_bytes()).unwrap();
        out.write_all(context("second").as_bytes()).unwrap();
        out.write_all(wall("2026-10-22T06:21:44Z", 1_792_731_600).as_bytes())
            .unwrap();
        out.flush().unwrap();
    }
    assert!(fs::metadata(&long).unwrap().len() > super::super::footer::FOLD_BUDGET);
    let roster = Roster {
        rows: vec![
            Row {
                label: "m15-2".into(),
                dir: None,
                email: Some("second@example.test".into()),
                ..Row::default()
            },
            Row {
                label: "m15".into(),
                dir: Some("/tmp/m15".into()),
                email: Some("first@example.test".into()),
                ..Row::default()
            },
            Row {
                label: "m15-3".into(),
                dir: Some("/tmp/m15-3".into()),
                email: Some("third@example.test".into()),
                ..Row::default()
            },
        ],
    };
    let now = 1_792_660_000;
    let walls = find_transcript_walls_by_email(dir.path(), &roster, || Some(BTreeMap::new()), now);
    assert_eq!(
        walls.get("m15").and_then(|w| w.until),
        Some(1_792_767_600),
        "the wall past the fold budget is booked to the session_context at the start: {walls:?}"
    );
    assert_eq!(
        walls.get("m15-2").and_then(|w| w.until),
        Some(1_792_731_600),
        "the /login and session_context past the fold budget book the newest wall: {walls:?}"
    );
    fs::remove_file(&long).unwrap();

    let short = proj.join("short.jsonl");
    let unfinished = wall("2026-10-22T06:21:44Z", 1_792_731_600);
    fs::write(
        &short,
        [context("third"), unfinished.trim_end().to_owned()].concat(),
    )
    .unwrap();
    let walls = find_transcript_walls_by_email(dir.path(), &roster, || Some(BTreeMap::new()), now);
    assert_eq!(
        walls.get("m15-3"),
        None,
        "an unfinished last line books nothing"
    );
    fs::write(&short, [context("third"), unfinished].concat()).unwrap();
    let walls = find_transcript_walls_by_email(dir.path(), &roster, || Some(BTreeMap::new()), now);
    assert_eq!(
        walls.get("m15-3").and_then(|w| w.until),
        Some(1_792_731_600),
        "control: the same line, once whole, books its wall"
    );
}

/// One long newest transcript leaves every older one of the scan its fold budget, and a
/// transcript with none after it may read all that is left.
#[test]
fn a_long_transcript_leaves_the_older_ones_their_fold_budget() {
    let fold = super::super::footer::FOLD_BUDGET;
    let total = TRANSCRIPT_WALLS_SCAN_BYTES;
    assert_eq!(transcript_allowance(total, 15), total - 15 * fold);
    assert!(
        transcript_allowance(total, 15) > 8 * fold,
        "a long one reads well past the fold budget"
    );
    assert_eq!(transcript_allowance(total, 0), total);
    assert_eq!(transcript_allowance(fold, 3), 0);
}

/// The scan's byte budget cuts a transcript at a line it reads only in part: that line books
/// nothing, the lines before it do, and what was read is taken off the budget.
#[test]
fn a_transcript_the_scan_budget_cuts_books_no_part_line() {
    let dir = aterm_tempfile::Builder::new()
        .prefix("aterm-walls-budget-")
        .tempdir()
        .expect("tempdir");
    let path = dir.path().join("cut.jsonl");
    let login = "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"<local-command-stdout>Login successful</local-command-stdout>\"}}\n";
    fs::write(&path, [login, login].concat()).unwrap();
    let roster = Roster::default();
    let whole = u64::try_from(login.len()).unwrap();
    let mut budget = whole + whole - 1;
    let rows = transcript_rows(&path, &roster, 0, &mut budget);
    assert_eq!(rows.len(), 1, "the line the budget cut books nothing");
    assert_eq!(budget, 0);
    let mut budget = whole * 3;
    let rows = transcript_rows(&path, &roster, 0, &mut budget);
    assert_eq!(rows.len(), 2, "control: both whole lines book");
    assert_eq!(budget, whole);
}

#[test]
fn transcript_walls_by_email_retains_prior_account_wall_after_in_session_login_switch() {
    let dir = aterm_tempfile::Builder::new()
        .prefix("aterm-walls-by-email-")
        .tempdir()
        .expect("tempdir");
    let proj = dir.path().join("projects").join("ws");
    fs::create_dir_all(&proj).unwrap();
    let transcript = proj.join("conv.jsonl");
    let lines = concat!(
        "{\"type\":\"attachment\",\"attachment\":{\"type\":\"session_context\",\"context\":{\"userEmail\":\"The user's email address is first@example.test. Use it only to identify the user.\"}}}\n",
        "{\"type\":\"assistant\",\"timestamp\":\"2026-10-20T06:50:53Z\",\"isApiErrorMessage\":true,\"error\":\"rate_limit\",\"message\":{\"model\":\"<synthetic>\",\"content\":[{\"type\":\"text\",\"text\":\"You've hit your weekly limit · resets Oct 23 at 8am\"}]},\"quotaLimits\":{\"status\":\"rejected\",\"resetsAt\":1792767600,\"rateLimitType\":\"seven_day\"}}\n",
        "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"<local-command-stdout>Login successful</local-command-stdout>\"}}\n",
        "{\"type\":\"attachment\",\"attachment\":{\"type\":\"session_context\",\"context\":{\"userEmail\":\"The user's email address is second@example.test. Use it only to identify the user.\"},\"changed\":true,\"reason\":\"account_change\"}}\n",
        "{\"type\":\"assistant\",\"timestamp\":\"2026-10-20T07:00:00Z\",\"message\":{\"model\":\"claude-opus-5-5\",\"content\":[{\"type\":\"text\",\"text\":\"done\"}]}}\n",
        "{\"type\":\"assistant\",\"timestamp\":\"2026-10-22T06:21:44Z\",\"isApiErrorMessage\":true,\"error\":\"rate_limit\",\"message\":{\"model\":\"<synthetic>\",\"content\":[{\"type\":\"text\",\"text\":\"You've hit your weekly limit · resets Oct 22 at 10pm\"}]},\"quotaLimits\":{\"status\":\"rejected\",\"resetsAt\":1792731600,\"rateLimitType\":\"seven_day\"}}\n"
    );
    fs::write(&transcript, lines).unwrap();
    let roster = Roster {
        rows: vec![
            Row {
                label: "m15-2".into(),
                dir: None,
                email: Some("second@example.test".into()),
                ..Row::default()
            },
            Row {
                label: "m15".into(),
                dir: Some("/tmp/m15".into()),
                email: Some("first@example.test".into()),
                ..Row::default()
            },
            Row {
                label: "m15-3".into(),
                dir: Some("/tmp/m15-3".into()),
                email: Some("third@example.test".into()),
                ..Row::default()
            },
        ],
    };
    let now = 1_792_660_000;
    let walls = find_transcript_walls_by_email(dir.path(), &roster, || Some(BTreeMap::new()), now);
    assert_eq!(
        walls.get("m15").and_then(|w| w.until),
        Some(1_792_767_600),
        "first account's weekly wall stands even after switching to second account"
    );
    assert_eq!(
        walls.get("m15-2").and_then(|w| w.until),
        Some(1_792_731_600),
        "second account's weekly wall is also recorded"
    );
    assert!(!walls.contains_key("m15-3"));
}

/// The 2026-10-27 displacement on m2, turned the other way: a person's `/login` in one tab
/// makes `m2` (walled for the week) the default login in place of `m2-2`. Another tab goes on
/// naming `m2-2` in its `session_context`, before the `/login` and again 40 minutes after it,
/// while `m2`'s wall answers its requests. Neither of its walls is `m2-2`'s: one stands across
/// the `/login` from the account it names, the other is booked after the newest `/login` to a
/// row with no login on this machine. The `/login` tab's own wall is `m2`'s. Control: where
/// `m2-2` holds a login in its own directory, a session on it is its own after the `/login`.
#[test]
fn a_stale_session_context_after_a_login_elsewhere_books_no_wall_to_the_displaced_row() {
    let dir = aterm_tempfile::Builder::new()
        .prefix("aterm-walls-displaced-")
        .tempdir()
        .expect("tempdir");
    let proj = dir.path().join("projects").join("ws");
    fs::create_dir_all(&proj).unwrap();
    let context = |at: &str, who: &str| {
        format!(
            "{{\"type\":\"attachment\",\"timestamp\":\"{at}\",\"attachment\":{{\"type\":\"session_context\",\"context\":{{\"userEmail\":\"The user's email address is {who}@example.test. Use it only to identify the user.\"}}}}}}\n"
        )
    };
    let wall = |at: &str| {
        format!(
            "{{\"type\":\"assistant\",\"timestamp\":\"{at}\",\"isApiErrorMessage\":true,\"error\":\"rate_limit\",\"message\":{{\"model\":\"<synthetic>\",\"content\":[{{\"type\":\"text\",\"text\":\"You've hit your weekly limit\"}}]}},\"quotaLimits\":{{\"status\":\"rejected\",\"resetsAt\":1793246400,\"rateLimitType\":\"seven_day\"}}}}\n"
        )
    };
    let answer = |at: &str| {
        format!(
            "{{\"type\":\"assistant\",\"timestamp\":\"{at}\",\"message\":{{\"model\":\"claude-opus-5-5\",\"content\":[{{\"type\":\"text\",\"text\":\"done\"}}]}}}}\n"
        )
    };
    let login = "{\"type\":\"user\",\"timestamp\":\"2026-10-27T16:02:57Z\",\"message\":{\"role\":\"user\",\"content\":\"<local-command-stdout>Login successful</local-command-stdout>\"}}\n";
    let untimed_context = |who: &str| {
        format!(
            "{{\"type\":\"attachment\",\"attachment\":{{\"type\":\"session_context\",\"context\":{{\"userEmail\":\"The user's email address is {who}@example.test. Use it only to identify the user.\"}}}}}}\n"
        )
    };
    fs::write(
        proj.join("login.jsonl"),
        [
            context("2026-10-27T10:00:00Z", "m2-2"),
            login.to_string(),
            untimed_context("m2"),
            wall("2026-10-27T16:05:00Z"),
        ]
        .concat(),
    )
    .unwrap();
    fs::write(
        proj.join("other.jsonl"),
        [
            context("2026-10-27T09:00:00Z", "m2-2"),
            answer("2026-10-27T15:00:00Z"),
            wall("2026-10-27T16:10:00Z"),
            context("2026-10-27T16:42:44Z", "m2-2"),
            wall("2026-10-27T16:50:00Z"),
        ]
        .concat(),
    )
    .unwrap();
    let own = dir.path().join("logins").join("m2-2");
    let roster = Roster {
        rows: vec![
            Row {
                label: "m2".into(),
                dir: None,
                email: Some("m2@example.test".into()),
                ..Row::default()
            },
            Row {
                label: "m2-2".into(),
                dir: Some(own.to_string_lossy().into_owned()),
                email: Some("m2-2@example.test".into()),
                ..Row::default()
            },
        ],
    };
    let now = 1_793_120_400;
    let walls = find_transcript_walls_by_email(dir.path(), &roster, || Some(BTreeMap::new()), now);
    assert_eq!(
        walls.get("m2").map(|w| w.at),
        Some(1_793_117_100),
        "the /login tab's wall is the new default login's"
    );
    assert_eq!(walls.get("m2-2"), None, "{walls:?}");

    fs::create_dir_all(&own).unwrap();
    fs::write(own.join(".credentials.json"), "{}").unwrap();
    let walls = find_transcript_walls_by_email(dir.path(), &roster, || Some(BTreeMap::new()), now);
    assert_eq!(
        walls.get("m2-2").map(|w| w.at),
        Some(1_793_119_800),
        "control: a row with its own login keeps the wall its session names after the /login"
    );
    assert_eq!(walls.get("m2").map(|w| w.at), Some(1_793_117_100));
}

/// R2 then R6 of `claude-rotation-proof` (rp1103a, 2026-11-03; its transcript's rows, cut to
/// the fields the scan reads): the conversation began on `lab-a`, the default login, so its one
/// `session_context` names `lab-a`; at 21:33:08 aterm moved it onto `lab-c`'s login by
/// `--resume`, and at 21:33:11 `lab-c` met its limit. The sync's scan at 21:37:59 booked that
/// wall to `lab-a`, and the tab then waited on `lab-c` for a wall `lab-a` never met. Noted
/// under a login directory from the move on, the conversation books no row in that stretch:
/// `lab-a`'s answer before it stands, the wall in it is nobody's; and a record that cannot be
/// read books nothing. Controls: no stretch books the wall to `lab-a` (what the run did), and
/// so do a stretch from after the wall and one that ended before it (moved back onto the
/// default login, which `lab-a`'s is).
#[test]
fn a_conversation_moved_onto_a_login_directory_books_no_wall_to_the_row_its_context_names() {
    const CONVERSATION: &str = "abd992c0-3cfa-46a4-bb90-3a295f008f70";
    const MOVED: i64 = 1_793_741_588;
    const WALL_AT: u64 = 1_793_741_591;
    let dir = aterm_tempfile::Builder::new()
        .prefix("aterm-walls-moved-")
        .tempdir()
        .expect("tempdir");
    let proj = dir
        .path()
        .join("projects")
        .join("-private-tmp-at-pWnj-proj");
    fs::create_dir_all(&proj).unwrap();
    let answer = |at: &str, n: u32| {
        format!(
            "{{\"isSidechain\":false,\"message\":{{\"role\":\"assistant\",\"model\":\"claude-opus-5-5\",\"content\":[{{\"type\":\"text\",\"text\":\"Hi! I am the local lab stub, not a model. (reply {n})\"}}]}},\"type\":\"assistant\",\"timestamp\":\"{at}\"}}\n"
        )
    };
    fs::write(
        proj.join(format!("{CONVERSATION}.jsonl")),
        [
            "{\"isSidechain\":false,\"attachment\":{\"type\":\"session_context\",\"context\":{\"userEmail\":\"The user's email address is nobody-a@example.test. Use it only to identify the user.\"}},\"type\":\"attachment\",\"timestamp\":\"2026-11-03T21:21:00.547Z\"}\n".to_string(),
            answer("2026-11-03T21:33:00.809Z", 10),
            answer("2026-11-03T21:33:09.128Z", 11),
            "{\"isSidechain\":false,\"type\":\"assistant\",\"timestamp\":\"2026-11-03T21:33:11.461Z\",\"message\":{\"model\":\"<synthetic>\",\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"You've hit your 5-hour limit \\u00b7 resets 2:33pm (America/Los_Angeles)\"}]},\"quotaLimits\":{\"status\":\"rejected\",\"resetsAt\":1793745191,\"unifiedRateLimitFallbackAvailable\":false,\"rateLimitType\":\"five_hour\",\"isUsingOverage\":false},\"error\":\"rate_limit\",\"isApiErrorMessage\":true}\n".to_string(),
        ]
        .concat(),
    )
    .unwrap();
    let roster = Roster {
        rows: vec![
            Row {
                label: "lab-a".into(),
                dir: None,
                email: Some("nobody-a@example.test".into()),
                ..Row::default()
            },
            Row {
                label: "lab-c".into(),
                dir: Some(
                    dir.path()
                        .join("logins/lab-c")
                        .to_string_lossy()
                        .into_owned(),
                ),
                email: Some("nobody-c@example.test".into()),
                ..Row::default()
            },
        ],
    };
    let now = 1_793_741_879;
    let wall_at = i64::try_from(WALL_AT).unwrap();
    // The relaunched process the host last saw run it under lab-c's login.
    let seen = accounts::NotedProcess {
        pid: 63_957,
        start: "Tue Nov 3 21:20:58 2026".to_string(),
    };
    let noted_at = |since: i64, until: Option<i64>, at: i64| {
        let seen = seen.clone();
        move || {
            Some(BTreeMap::from([(
                CONVERSATION.to_string(),
                accounts::LoginDirSpans {
                    spans: vec![accounts::DirSpan { since, until }],
                    at,
                    process: Some(seen.clone()),
                },
            )]))
        }
    };
    let noted = |since: i64, until: Option<i64>| noted_at(since, until, since);
    type Spans<'a> = &'a dyn Fn() -> Option<BTreeMap<String, accounts::LoginDirSpans>>;
    let scan = |spans: Spans<'_>| find_transcript_walls_by_email(dir.path(), &roster, spans, now);
    let walls = scan(&noted(MOVED, None));
    assert!(walls.is_empty(), "moved onto lab-c: {walls:?}");
    let walls = scan(&|| None);
    assert!(
        walls.is_empty(),
        "no record read, nothing booked: {walls:?}"
    );

    let to_lab_a = (WALL_AT, Some(1_793_745_191));
    let walls = scan(&|| Some(BTreeMap::new()));
    assert_eq!(
        walls.get("lab-a").map(|w| (w.at, w.until)),
        Some(to_lab_a),
        "control: unnoted, the wall goes to the row the context names, as the run booked it"
    );
    for (stretch, why) in [
        ((wall_at + 1, None), "a stretch from after the wall"),
        (
            (MOVED, Some(wall_at)),
            "a stretch that ended before the wall",
        ),
    ] {
        let walls = scan(&noted(stretch.0, stretch.1));
        assert_eq!(
            walls.get("lab-a").map(|w| (w.at, w.until)),
            Some(to_lab_a),
            "control: {why} cuts no row outside it"
        );
    }
    // Lapsed as the process that wrote the `session_context` (21:21:00Z) started: a resume
    // no host saw is booked as its context says.
    const CONTEXT_AT: i64 = 1_793_740_860;
    let lapsed = CONTEXT_AT - accounts::LOGIN_DIR_NOTED_S;
    let walls = scan(&noted_at(lapsed - 60, None, lapsed));
    assert_eq!(
        walls.get("lab-a").map(|w| (w.at, w.until)),
        Some(to_lab_a),
        "control: an open stretch no host noted in the hour before lapsed"
    );
    let walls = scan(&noted_at(
        lapsed - 60,
        None,
        wall_at - accounts::LOGIN_DIR_NOTED_S + 1,
    ));
    assert!(
        walls.is_empty(),
        "noted within the hour before the wall: {walls:?}"
    );
    // Its process found still running by a sync: a turn hours long with no account point is
    // a sighting, saved, so the stretch stays covered once that process is gone (aterm quit,
    // a crash) and its relaunch notes a stretch of its own from its start.
    let long_turn = noted_at(lapsed - 60, None, lapsed);
    for (running, covered) in [(seen.pid, true), (seen.pid + 1, false)] {
        let store = accounts::Store::at(&dir.path().join(format!("state-{running}")));
        store
            .transact(now, &mut None, |state| {
                state.login_dir_spans = long_turn().expect("a record");
                Ok(Vec::new())
            })
            .unwrap();
        let seen_now = login_dir_spans_seen(&store, &[], now, |process| process.pid == running);
        assert!(seen_now.is_some());
        let relaunched = now + 60;
        store
            .transact(relaunched, &mut None, |state| {
                accounts::note_on_login_dir(state, CONVERSATION, relaunched, relaunched);
                Ok(Vec::new())
            })
            .unwrap();
        let saved = || {
            store
                .load(relaunched)
                .ok()
                .map(|(state, _)| state.login_dir_spans)
        };
        let walls = scan(&saved);
        if covered {
            assert!(walls.is_empty(), "its process ran on: {walls:?}");
        } else {
            assert_eq!(
                walls.get("lab-a").map(|w| (w.at, w.until)),
                Some(to_lab_a),
                "control: another process running is no sighting of it"
            );
        }
    }
}

/// A sync's look at the store's open stretches (`login_dir_spans_live`) finds a noted process
/// running by its pid and its start: this test's own process, noted with its kernel start, is
/// a sighting at the look, saved under the store's lock.
///
/// NEGATIVE CONTROL: the same pid noted with another start (a pid reused since) is none, and
/// its record stands as its host noted it.
#[test]
fn a_syncs_look_finds_a_noted_process_running_by_its_pid_and_its_start() {
    const CONVERSATION: &str = "abd992c0-3cfa-46a4-bb90-3a295f008f70";
    let dir = aterm_tempfile::Builder::new()
        .prefix("aterm-sync-live-")
        .tempdir()
        .expect("temp dir");
    let me = std::process::id();
    let start = super::super::restart_stamp::kernel_start(me).expect("this process's start");
    let now = 1_793_745_000;
    let noted_at = now - accounts::LOGIN_DIR_NOTED_S / 2;
    for (start, seen) in [(start.as_str(), true), ("Thu Jan  1 00:00:00 1970", false)] {
        let store = accounts::Store::at(&dir.path().join(format!("state-{seen}")));
        let noted = accounts::LoginDirSpans {
            spans: vec![accounts::DirSpan {
                since: now - 7200,
                until: None,
            }],
            at: noted_at,
            process: Some(accounts::NotedProcess {
                pid: me,
                start: start.to_string(),
            }),
        };
        store
            .transact(now, &mut None, |state| {
                state.login_dir_spans = BTreeMap::from([(CONVERSATION.to_string(), noted)]);
                Ok(Vec::new())
            })
            .expect("noted");
        let looked = login_dir_spans_live(&store, &[], now).expect("the record read");
        let saved = store.load(now).expect("the store").0.login_dir_spans;
        assert_eq!(looked, saved, "what the scan reads is saved");
        assert_eq!(
            saved[CONVERSATION].at,
            if seen { now } else { noted_at },
            "{start}: {saved:?}"
        );
    }
}

#[test]
fn renewal_admission_outlives_unstarted_work_resources() {
    use std::sync::{Arc, Mutex};

    struct HeldWork {
        _signin: accounts::SigninTurn,
        store: accounts::Store,
        guard: Arc<Mutex<AutoRenewGuard>>,
        observed: Arc<Mutex<Option<(bool, bool)>>>,
    }

    impl Drop for HeldWork {
        fn drop(&mut self) {
            let busy = self.guard.lock().unwrap().in_flight;
            let locked = self.store.signin_now().unwrap().is_none();
            *self.observed.lock().unwrap() = Some((busy, locked));
        }
    }

    for mode in ["run", "refuse", "discard", "panic"] {
        let tmp = aterm_tempfile::tempdir().unwrap();
        let store = accounts::Store::at(tmp.path());
        let guard = Arc::new(Mutex::new(AutoRenewGuard::default()));
        let observed = Arc::new(Mutex::new(None));
        let held = HeldWork {
            _signin: store.signin_now().unwrap().unwrap(),
            store: store.clone(),
            guard: guard.clone(),
            observed: observed.clone(),
        };
        let permit = AutoRenewPermit::claim(&guard, "work", Instant::now(), true).unwrap();
        let mut queued = None;
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            start_auto_renew(
                permit,
                move || {
                    let _held = held;
                    if mode == "panic" {
                        panic!("synthetic renewal work panic");
                    }
                },
                |work| match mode {
                    "run" | "panic" => {
                        work();
                        Ok(())
                    }
                    "refuse" => Err(io::Error::other("synthetic spawn refusal")),
                    "discard" => {
                        queued = Some(work);
                        Ok(())
                    }
                    _ => unreachable!(),
                },
            )
        }));
        if mode == "panic" {
            assert!(result.is_err());
        } else {
            assert_eq!(result.unwrap().is_err(), mode == "refuse");
        }
        if let Some(work) = queued.take() {
            assert!(guard.lock().unwrap().in_flight);
            assert!(store.signin_now().unwrap().is_none());
            drop(work);
        }
        assert_eq!(
            *observed.lock().unwrap(),
            Some((true, true)),
            "{mode}: admission must remain held while original work resources drop"
        );
        assert!(!guard.lock().unwrap().in_flight);
        assert!(store.signin_now().unwrap().is_some());
    }
}

#[test]
fn a_refused_renewal_worker_releases_admission_without_running_it() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};

    let state = Arc::new(Mutex::new(AutoRenewGuard::default()));
    let now = Instant::now();
    let ran = Arc::new(AtomicBool::new(false));
    let called = Arc::clone(&ran);
    let permit = AutoRenewPermit::claim(&state, "first", now, false).expect("admitted");
    let result = start_auto_renew(
        permit,
        move || called.store(true, Ordering::SeqCst),
        |_work| Err(io::Error::other("injected spawn refusal")),
    );
    assert!(result.is_err());
    assert!(!ran.load(Ordering::SeqCst), "a refused spawn ran nothing");
    assert!(!state.lock().unwrap().in_flight, "refusal releases busy");
    assert!(
        AutoRenewPermit::claim(&state, "first", now, false).is_none(),
        "the per-label cooldown still prevents repeated background attempts"
    );
    let mtime_v1 = std::time::UNIX_EPOCH + Duration::from_secs(1_000);
    let mtime_permit =
        AutoRenewPermit::claim_with_mailbox(&state, "first", now, false, Some(mtime_v1))
            .expect("a new or updated mailbox config mtime clears the retry cooldown");
    drop(mtime_permit);
    assert!(
        AutoRenewPermit::claim_with_mailbox(&state, "first", now, false, Some(mtime_v1)).is_none(),
        "same mailbox mtime respects cooldown"
    );
    let manual_permit = AutoRenewPermit::claim(&state, "first", now, true)
        .expect("explicit operator selection bypasses the background cooldown when idle");
    assert!(
        AutoRenewPermit::claim(&state, "first", now, true).is_none(),
        "in_flight still serializes concurrent explicit selections"
    );
    drop(manual_permit);

    let permit = AutoRenewPermit::claim(&state, "second", now, false)
        .expect("spawn refusal cannot block another account");
    let called = Arc::clone(&ran);
    let mut queued = None;
    start_auto_renew(
        permit,
        move || called.store(true, Ordering::SeqCst),
        |work| {
            queued = Some(work);
            Ok(())
        },
    )
    .expect("accepted by injected builder");
    assert!(!ran.load(Ordering::SeqCst), "still queued");
    assert!(state.lock().unwrap().in_flight);
    assert!(AutoRenewPermit::claim(&state, "third", now, false).is_none());
    queued.take().expect("queued worker")();
    assert!(ran.load(Ordering::SeqCst), "accepted worker ran");
    assert!(!state.lock().unwrap().in_flight, "completion releases busy");
    let later = now + Duration::from_secs(900);
    let permit = AutoRenewPermit::claim(&state, "first", later, false)
        .expect("the original account can retry after its cooldown");
    drop(permit);
    assert!(!state.lock().unwrap().in_flight);
}

/// A background renewal that stopped for a person (a block page, a page that waits on
/// one) parks its row: no background renewal of it starts again past the 15-minute cooldown
/// until [`PERSON_BACKOFF`] has passed, and a renewal by hand still may. Any other end
/// unparks it. CONTROL: a renewal that failed otherwise is tried again after the cooldown.
#[test]
fn a_background_renewal_that_stopped_for_a_person_is_left_alone_for_its_backoff() {
    use super::super::cli::Renewal;
    use std::sync::{Arc, Mutex};
    let state = Arc::new(Mutex::new(AutoRenewGuard::default()));
    let now = Instant::now();
    let after_cooldown = now + Duration::from_secs(901);
    drop(AutoRenewPermit::claim(&state, "grok:grok-1", now, false).expect("admitted"));
    note_background_end(&state, "grok:grok-1", Renewal::NeedsPerson, now);
    assert!(AutoRenewPermit::claim(&state, "grok:grok-1", after_cooldown, false).is_none());
    let by_hand = AutoRenewPermit::claim(&state, "grok:grok-1", after_cooldown, true);
    assert!(by_hand.is_some(), "a renewal by hand is no background one");
    drop(by_hand);
    let past = now + PERSON_BACKOFF;
    drop(AutoRenewPermit::claim(&state, "grok:grok-1", past, false).expect("backed off"));
    note_background_end(&state, "grok:grok-1", Renewal::Renewed, past);
    assert!(state.lock().unwrap().parked.is_empty(), "unparked");

    drop(AutoRenewPermit::claim(&state, "kimi:kimi-1", now, false).expect("admitted"));
    note_background_end(&state, "kimi:kimi-1", Renewal::Failed, now);
    assert!(
        AutoRenewPermit::claim(&state, "kimi:kimi-1", after_cooldown, false).is_some(),
        "CONTROL: another failure waits out the cooldown alone"
    );
}

/// A background renewal that met Cloudflare's bot check holds its row as long as one that
/// stopped for a person, though nobody is asked (design R-13): not past the 15-minute
/// cooldown, nor a minute before [`PERSON_BACKOFF`], and again once that has passed; the
/// row's activity says when. A renewal by hand is admitted through it, and the row's next end
/// decides: a plain failure lifts it. CONTROL: that failure waits out the cooldown alone.
#[test]
fn a_background_renewal_that_met_a_bot_check_is_left_alone_for_the_person_backoff() {
    use super::super::cli::Renewal;
    use std::sync::{Arc, Mutex};
    let state = Arc::new(Mutex::new(AutoRenewGuard::default()));
    let store = accounts::Store::at(Path::new("/synthetic/bot-check"));
    let now = Instant::now();
    let key = "kimi:kimi-1";
    let permit = AutoRenewPermit::claim(&state, key, now, false).expect("admitted");
    permit.attribute(
        &store,
        Cli::Kimi,
        "kimi-1",
        1_000,
        &accounts::Logins::default(),
    );
    drop(permit);
    note_background_end(&state, key, Renewal::BotCheck, now);
    let cooled = now + Duration::from_secs(901);
    for later in [cooled, now + PERSON_BACKOFF - Duration::from_secs(60)] {
        assert!(
            AutoRenewPermit::claim(&state, key, later, false).is_none(),
            "{:?} later",
            later - now
        );
    }
    for ago in [60, 901] {
        let activity = login_activity_of(
            &state.lock().unwrap(),
            &store,
            Cli::Kimi,
            1_000,
            now + Duration::from_secs(ago),
        );
        assert_eq!(
            activity.first().and_then(|row| row.next_at),
            Some(1_000 + PERSON_BACKOFF_SECS - i64::try_from(ago).unwrap()),
            "{ago} s later, inside the cooldown too: {activity:?}"
        );
    }
    let by_hand = AutoRenewPermit::claim(&state, key, cooled, true);
    assert!(by_hand.is_some(), "a renewal by hand is no background one");
    drop(by_hand);
    let past = now + PERSON_BACKOFF;
    drop(AutoRenewPermit::claim(&state, key, past, false).expect("backed off"));
    note_background_end(&state, key, Renewal::Failed, past);
    assert!(state.lock().unwrap().bot_checked.is_empty(), "lifted");
    assert!(
        AutoRenewPermit::claim(&state, key, past + Duration::from_secs(901), false).is_some(),
        "CONTROL: a plain failure waits out the cooldown alone"
    );
}

/// A session at m15-2's weekly wall: NO SWEEP (`DESIGN-alab-one-mesh-2026-11-05.md` §4.15
/// rule 4), the upkeep signs in no row that could take it, since no session here ran on
/// m15-3, so its sign-in is a person's (`accounts login m15-3`); that sign-in, by mailbox,
/// makes the login the session then rotates to.
#[test]
fn weekly_limit_rollover_signs_in_no_successor_and_a_persons_mailbox_sign_in_rotates_the_session() {
    let dir = aterm_tempfile::Builder::new()
        .prefix("aterm-rollover-mailbox-")
        .tempdir()
        .expect("tempdir");
    let root = dir.path();
    let home = root.join("home");
    let state_dir = root.join("state");
    let config_dir = root.join("config");
    let claude_dir = home.join(".claude");
    let projects_dir = claude_dir.join("projects").join("ws");
    let store = accounts::Store::at(&state_dir);
    let mailbox_dir = store.dir.join(super::super::open_url::DEFAULT_MAILBOX_DIR);
    fs::create_dir_all(&projects_dir).unwrap();
    fs::create_dir_all(&config_dir).unwrap();
    fs::create_dir_all(&mailbox_dir).unwrap();

    let csv_path = root.join("fleet.csv");
    fs::write(
        &csv_path,
        concat!(
            "email,computer,claude\n",
            "m15@example.test,m15,1\n",
            "m15-2@example.test,m15,1\n",
            "m15-3@example.test,m15,1\n"
        ),
    )
    .unwrap();

    let claude_profile = home.join(".claude.json");
    fs::write(
        &claude_profile,
        "{\"oauthAccount\":{\"emailAddress\":\"m15-2@example.test\"}}\n",
    )
    .unwrap();

    let transcript = projects_dir.join("session.jsonl");
    fs::write(
        &transcript,
        concat!(
            "{\"type\":\"attachment\",\"attachment\":{\"type\":\"session_context\",\"context\":{\"userEmail\":\"The user's email address is m15@example.test. Use it only to identify the user.\"}}}\n",
            "{\"type\":\"assistant\",\"timestamp\":\"2026-10-20T06:50:53Z\",\"isApiErrorMessage\":true,\"error\":\"rate_limit\",\"message\":{\"model\":\"<synthetic>\",\"content\":[{\"type\":\"text\",\"text\":\"You've hit your weekly limit · resets Oct 26 at 10am\"}]},\"quotaLimits\":{\"status\":\"rejected\",\"resetsAt\":1792999200,\"rateLimitType\":\"seven_day\"}}\n",
            "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"<local-command-stdout>Login successful</local-command-stdout>\"}}\n",
            "{\"type\":\"attachment\",\"attachment\":{\"type\":\"session_context\",\"context\":{\"userEmail\":\"The user's email address is m15-2@example.test. Use it only to identify the user.\"},\"changed\":true,\"reason\":\"account_change\"}}\n",
            "{\"type\":\"assistant\",\"timestamp\":\"2026-10-22T06:21:44Z\",\"isApiErrorMessage\":true,\"error\":\"rate_limit\",\"message\":{\"model\":\"<synthetic>\",\"content\":[{\"type\":\"text\",\"text\":\"You've hit your weekly limit · resets Oct 25 at 2pm\"}]},\"quotaLimits\":{\"status\":\"rejected\",\"resetsAt\":1792936800,\"rateLimitType\":\"seven_day\"}}\n"
        ),
    )
    .unwrap();

    let now = 1_792_660_000;
    let aterm_toml = config_dir.join("aterm.toml");
    let env = Env {
        state: state_dir,
        aterm_state: Some(root.join("aterm-state")),
        cwd: root.to_path_buf(),
        home: Some(home),
        claude_dir: Some(claude_dir),
        claude_config_unusable: None,
        claude_profile: Some(claude_profile),
        claude_custom_oauth: false,
        claude_session: None,
        claude_pid: None,
        codex_thread: None,
        agy_conversation: None,
        codex_home: None,
        xdg_config_home: None,
        codex_login: None,
        now,
        tz_s: 0,
        sid: String::new(),
        sock: None,
        config: Some(aterm_toml),
        claude_tmp: None,
        claude_running: None,
    };

    let mut sync_out = Vec::new();
    let mut sync_err = Vec::new();
    assert_eq!(
        run_accounts_sync(
            &env,
            None,
            csv_path.to_str(),
            Some("m15"),
            false,
            &mut sync_out,
            &mut sync_err
        ),
        ExitCode::SUCCESS,
        "sync failed: {}",
        String::from_utf8_lossy(&sync_err)
    );

    let roster_path = config_dir.join("accounts.toml");
    let file_roster = Roster::load(&roster_path).expect("accounts.toml written");
    let (logins_before, _) = store.load_logins(now).expect("logins loaded");

    let who_m15_2 = accounts::ProcessLogin {
        login: None,
        default_home: true,
        route: crate::harness::accounts::AuthRoute::Stored,
    };
    let standing_wall = super::super::upgrade::StandingWall {
        at: u64::try_from(now).unwrap(),
        until: Some(1_792_936_800),
        window: Some("seven_day".to_owned()),
        credits: None,
    };
    assert!(matches!(
        accounts::record_wall(
            &store,
            &store.read_usage_roster(Some(&roster_path), now).unwrap(),
            "sid-m15-live",
            (&who_m15_2, &crate::harness::observe::Observation::Unread),
            &standing_wall,
            now - 60,
            now,
        ),
        accounts::Recorded::Wall { .. }
    ));

    let (state_walled, _) = store.load(now).unwrap();
    assert_eq!(
        next_auto_renew_target(&file_roster, &state_walled, &logins_before, now).map(|r| r.label),
        None,
        "the wall signs in no successor nobody here ran on"
    );
    let mut session_only_state = state_walled;
    session_only_state.active = None;
    assert_eq!(
        next_auto_renew_target(&file_roster, &session_only_state, &logins_before, now)
            .map(|r| r.label),
        None,
        "nor when state.active is None and only state.sessions names m15-2"
    );
    let next_row = accounts::bound(&file_roster, &logins_before)
        .row("m15-3")
        .cloned()
        .expect("the person's named row");
    assert_eq!(next_row.email.as_deref(), Some("m15-3@example.test"));
    assert_eq!(
        select_auto_renew_browser(None, None, Some(PathBuf::from("/bin/aterm-open-url"))),
        None,
        "without a mailbox or profile, background auto-renew must never fall back to Default browser"
    );
    assert_eq!(
        select_auto_renew_browser(
            Some("Claude m15-3"),
            Some(&mailbox_dir),
            Some(PathBuf::from("/bin/aterm-open-url"))
        ),
        Some(super::super::cli::RenewBrowser::Remote {
            opener: Some(PathBuf::from("/bin/aterm-open-url")),
        }),
        "when a mailbox directory is present, headless Remote opener is preferred over GUI profile"
    );

    let staging = store
        .claim_staging_dir(&next_row.label)
        .expect("claim staging dir");
    let staging_text = staging.to_str().unwrap().to_owned();

    fs::write(
        mailbox_dir.join("00_decoy_spoof.eml"),
        concat!(
            "From: Claude <no-reply@claude.ai>\r\n",
            "To: <not-m15-3@example.test>\r\n\r\n",
            "https://claude.ai/magic?token=spoofed_wrong_account\r\n"
        ),
    )
    .unwrap();
    fs::write(
        mailbox_dir.join("01_magic_m15_3.eml"),
        concat!(
            "From: Claude <no-reply@claude.ai>\r\n",
            "To: Claude Fleet\r\n",
            "  <m15-3@example.test>\r\n",
            "Content-Transfer-Encoding: quoted-printable\r\n\r\n",
            "<html><body>\r\n",
            "<a href=3D\"https://claude.ai/unsubscribe\">Unsubscribe</a>\r\n",
            "<a href=3D\"https://claude.ai/magic?token=3Dtok_m15_3&amp;=\r\n",
            "hint=3Dm15-3\">Sign in to Claude.ai</a>\r\n",
            "</body></html>\r\n"
        ),
    )
    .unwrap();

    let authorize_url = "https://claude.ai/oauth/authorize?redirect_uri=http%3A%2F%2F127.0.0.1%3A9123%2Fcallback&state=st_m15_3&login_hint=m15-3%40example.test";
    let responses = [
        "HTTP/1.1 200 OK\r\nSet-Cookie: session=sess_m15_3; Secure; Path=/\r\n\r\nCheck your email",
        concat!(
            "HTTP/1.1 200 OK\r\n\r\n",
            "<form method=\"POST\" action=\"/oauth/approve\">",
            "<input type=\"hidden\" name=\"approve\" value=\"allow\"></form>"
        ),
        "HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:9123/callback?code=code_m15_3&state=st_m15_3\r\n\r\n",
        "HTTP/1.1 200 OK\r\n\r\nLogin successful",
    ];
    let mut resp_iter = responses.iter();
    let mut seen_urls = Vec::new();
    super::super::open_url::authorize_using(
        authorize_url,
        next_row.email.as_deref(),
        |email| {
            super::super::open_url::poll_mailbox_for_magic_link(
                &mailbox_dir,
                email,
                Duration::from_secs(60),
            )
        },
        &mut |_method, url, cookies, _body| {
            seen_urls.push(url.to_owned());
            super::super::open_url::parse_raw_http_response(
                resp_iter.next().expect("expected request"),
                cookies,
            )
            .ok_or("malformed response")
        },
    )
    .expect("OAuth + mailbox magic-link authorization must succeed");
    assert_eq!(
        seen_urls,
        vec![
            authorize_url.to_owned(),
            "https://claude.ai/magic?token=tok_m15_3&hint=m15-3".to_owned(),
            "https://claude.ai/oauth/approve".to_owned(),
            "http://127.0.0.1:9123/callback?code=code_m15_3&state=st_m15_3".to_owned(),
        ]
    );

    fs::write(
        staging.join(".claude.json"),
        "{\"oauthAccount\":{\"emailAddress\":\"m15-3@example.test\",\"subscriptionType\":\"max\"}}\n",
    )
    .unwrap();
    let status_got = super::super::align::Captured {
        stdout: b"{\"loggedIn\":true,\"authMethod\":\"claude.ai\",\"email\":\"m15-3@example.test\",\"subscriptionType\":\"max\"}\n".to_vec(),
        code: Some(0),
    };
    assert!(
        complete_staging_renewal(
            &store,
            &file_roster,
            &next_row.label,
            &staging,
            &staging_text,
            &status_got,
            now + 5,
        ),
        "complete_staging_renewal must bind m15-3"
    );
    assert!(
        store.wake_path().exists(),
        "complete_staging_renewal must touch wake so waiting sessions rotate immediately"
    );

    let (state_after_renew, _) = store.load(now + 5).unwrap();
    let (logins_after_renew, _) = store.load_logins(now + 5).unwrap();
    let bound_roster = accounts::bound(&file_roster, &logins_after_renew);
    let enrolled_after = accounts::enrolled(&bound_roster);
    let wall_point = accounts::Point::Wall {
        window: "seven_day".to_owned(),
        at: Some(1_792_936_800),
    };
    let me_after = who_m15_2.me(&bound_roster);
    assert_eq!(
        accounts::advise(
            &bound_roster,
            &state_after_renew,
            &enrolled_after,
            &me_after,
            &wall_point,
            accounts::Mode::Wall,
            now + 5,
        ),
        accounts::Advice::Move {
            to: "m15-3".to_owned(),
            ahead: false,
            rotates: true,
            takes_canary: false,
            yields: None,
        }
    );

    let expect_active = state_after_renew.active.clone();
    let mut note = None;
    store
        .transact(now + 5, &mut note, |st| {
            let ev = accounts::begin_rotation(
                st,
                &bound_roster,
                &enrolled_after,
                expect_active.as_deref(),
                accounts::Mover {
                    sid: "sid-m15-live",
                    account: "m15-2",
                },
                "m15-3",
                now + 5,
            )
            .map_err(|lost| format!("rotation lost: {lost:?}"))?;
            Ok(vec![ev])
        })
        .expect("begin_rotation transaction");
    let (state_rotated, _) = store.load(now + 5).unwrap();
    assert_eq!(state_rotated.active.as_deref(), Some("m15-3"));
    let target_row = bound_roster.row("m15-3").expect("m15-3 row");
    assert_eq!(
        accounts::login_env(target_row),
        (accounts::LOGIN_ENV, staging_text)
    );
}

struct World {
    _dir: aterm_tempfile::TempDir,
    home: PathBuf,
    state: PathBuf,
    config: PathBuf,
}

impl World {
    fn new() -> Self {
        let dir = aterm_tempfile::Builder::new()
            .prefix("aterm-fleet-upkeep-")
            .tempdir()
            .expect("tempdir");
        let root = dir.path().to_path_buf();
        let home = root.join("home");
        fs::create_dir_all(&home).unwrap();
        Self {
            _dir: dir,
            home,
            state: root.join("state"),
            config: root.join("config"),
        }
    }

    fn aterm_toml(&self) -> PathBuf {
        self.config.join("aterm.toml")
    }

    fn roster(&self) -> PathBuf {
        self.config.join("accounts.toml")
    }

    fn fleet_file(&self) -> PathBuf {
        self.config.join(FLEET_ROSTER_FILE)
    }

    fn write_config(&self, text: &str) {
        fs::create_dir_all(&self.config).unwrap();
        aterm_uds::atomic_file::write(&self.aterm_toml(), text.as_bytes(), 0o600).unwrap();
    }

    fn env(&self, now: i64) -> Env {
        Env {
            state: self.state.clone(),
            aterm_state: None,
            cwd: self.home.clone(),
            home: Some(self.home.clone()),
            claude_dir: Some(self.home.join(".claude")),
            claude_config_unusable: None,
            claude_profile: Some(self.home.join(".claude.json")),
            claude_custom_oauth: false,
            claude_session: None,
            claude_pid: None,
            codex_thread: None,
            agy_conversation: None,
            codex_home: None,
            xdg_config_home: None,
            codex_login: None,
            now,
            tz_s: 0,
            sid: String::new(),
            sock: None,
            config: Some(self.aterm_toml()),
            claude_tmp: None,
            claude_running: None,
        }
    }
}

struct Witness {
    fetched: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    spawned: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    fetch: Box<Fetch<'static>>,
    spawn: Box<SpawnRenewal<'static>>,
    cooldown: std::sync::Mutex<Option<Instant>>,
    rediscovery: std::sync::Mutex<Rediscovery>,
    guard: std::sync::Arc<std::sync::Mutex<AutoRenewGuard>>,
    /// Whether a headless browser is installed: none unless a test says so.
    browser: Box<dyn Fn(&Path) -> bool + Sync>,
    /// The vault's sync: this machine is in none unless a test says so.
    vault_sync: fn(
        &Path,
        Option<&crate::harness::account_work::Scope>,
    ) -> super::super::sign_in_ladder::Vault,
    /// The automatic sign-ins' lines, as the window's `aterm.log` would have them.
    logged: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    /// One run of a route: none unless a test scripts one.
    run_attempt: Option<RunAttempt>,
}

/// A spawn seam that runs its work at once, on the caller's thread.
fn run_inline(work: Box<dyn FnOnce() + Send>) -> io::Result<()> {
    work();
    Ok(())
}

impl Witness {
    fn answering(text: &'static str) -> Self {
        let fetched: std::sync::Arc<std::sync::Mutex<Vec<String>>> = std::sync::Arc::default();
        let spawned: std::sync::Arc<std::sync::atomic::AtomicUsize> = std::sync::Arc::default();
        let into = std::sync::Arc::clone(&fetched);
        let count = std::sync::Arc::clone(&spawned);
        Self {
            fetch: Box::new(move |_: &Env, source: &str| {
                into.lock().unwrap().push(source.to_owned());
                Ok(text.to_owned())
            }),
            spawn: Box::new(move |_work| {
                count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(())
            }),
            fetched,
            spawned,
            cooldown: std::sync::Mutex::new(None),
            rediscovery: std::sync::Mutex::new(Rediscovery::default()),
            guard: std::sync::Arc::new(std::sync::Mutex::new(AutoRenewGuard::default())),
            browser: Box::new(|_: &Path| false),
            vault_sync: |_: &Path, _scope| super::super::sign_in_ladder::Vault::NotMember,
            logged: std::sync::Arc::default(),
            run_attempt: None,
        }
    }

    fn fx(&self) -> UpkeepEffects<'_> {
        let logged = std::sync::Arc::clone(&self.logged);
        UpkeepEffects {
            scope: None,
            fetch: &*self.fetch,
            spawn_renewal: &*self.spawn,
            sync_cooldown: &self.cooldown,
            rediscovery: &self.rediscovery,
            renew_guard: &self.guard,
            find_browser: &*self.browser,
            vault_sync: self.vault_sync,
            spawn_vault: &run_inline,
            default_check: |_: &Path, _scope| {
                crate::harness::cli::LoginCheck::CouldNotRun(String::new())
            },
            mailbox: &ladder_mailbox_files,
            log: std::sync::Arc::new(move |line: &str| {
                logged.lock().unwrap().push(line.to_owned());
            }),
            run_attempt: self.run_attempt.clone(),
            save_oauth: |_, _| Ok("inert".into()),
            shared: shared_renewal::Effects::default(),
        }
    }

    fn logged(&self) -> Vec<String> {
        self.logged.lock().unwrap().clone()
    }

    fn fetched(&self) -> Vec<String> {
        self.fetched.lock().unwrap().clone()
    }

    fn spawned(&self) -> usize {
        self.spawned.load(std::sync::atomic::Ordering::SeqCst)
    }
}

const LOOPBACK_SOURCE: &str = "http://127.0.0.1:9/fleet.csv";
const ONE_ACCOUNT_CSV: &str = "email,computer,claude\nm1@example.test,m1,1\n";
const UPKEEP_NOW: i64 = 1_792_660_000;
const UNCONFIGURED: Upkept = Upkept::Unconfigured {
    fleet_file_ignored: false,
    renewal: false,
};

#[test]
fn an_unconfigured_upkeep_fetches_nothing_and_starts_nothing() {
    let w = World::new();
    let wit = Witness::answering(ONE_ACCOUNT_CSV);
    let upkept = |wit: &Witness| {
        upkeep(
            &w.home,
            &w.state,
            (&w.roster(), UPKEEP_NOW, UpkeepStep::Both),
            &wit.fx(),
        )
    };
    assert_eq!(upkept(&wit), UNCONFIGURED, "a fresh home");
    assert!(!w.config.exists(), "nothing made in a fresh home");
    for text in [
        "[fleet]\nmachine = \"m1\"\n",
        "[fleet]\naccounts_source = \"  \"\nmachine = \"m1\"\n",
        "accounts_source = \"http://127.0.0.1:9/fleet.csv\"\n[fleet]\nmachine = \"m1\"\n",
        "[fleet\naccounts_source = \"http://127.0.0.1:9/fleet.csv\"\n",
        "[fleet]\naccounts_source = 7\nmachine = \"m1\"\n",
    ] {
        w.write_config(text);
        assert_eq!(upkept(&wit), UNCONFIGURED, "{text}");
    }
    assert_eq!(wit.fetched(), Vec::<String>::new(), "nothing fetched");
    assert_eq!(wit.spawned(), 0, "no renewal started");
    assert!(!w.roster().exists() && !w.fleet_file().exists());
    assert!(!w.state.exists(), "the accounts store is never made");

    w.write_config(&format!(
        "[fleet]\naccounts_source = \"{LOOPBACK_SOURCE}\"\nmachine = \"m1\"\n"
    ));
    assert_eq!(
        upkept(&wit),
        Upkept::Configured {
            kind: "key",
            roster: true,
            renewal: false
        }
    );
    assert_eq!(wit.fetched(), vec![LOOPBACK_SOURCE.to_owned()]);
    let roster = fs::read_to_string(w.roster()).expect("the roster the source answered");
    assert!(roster.contains("m1@example.test"), "{roster}");
    assert_eq!(fleet_file_opt_in(&w.fleet_file()), Some(OptIn::Key));
}

#[test]
fn a_sync_handed_no_source_reads_only_a_configured_one() {
    let w = World::new();
    w.write_config("[fleet]\nmachine = \"m1\"\n");
    let wit = Witness::answering(ONE_ACCOUNT_CSV);
    let sync = |w: &World| {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let how = SyncHow {
            fetch: &*wit.fetch,
            ladder: &[],
            fleet_snapshots: &[],
            heal: None,
            allow_empty_pool: false,
        };
        let code = run_accounts_sync_with(
            (&w.env(UPKEEP_NOW), None),
            (None, None, false),
            (&mut out, &mut err),
            &how,
        );
        (code, String::from_utf8_lossy(&err).into_owned())
    };
    let (code, err) = sync(&w);
    assert_eq!(code, ExitCode::FAILURE);
    assert!(err.contains(NO_SOURCE), "{err}");
    assert_eq!(wit.fetched(), Vec::<String>::new());
    assert!(!w.roster().exists());

    w.write_config(&format!(
        "[fleet]\naccounts_source = \"{LOOPBACK_SOURCE}\"\nmachine = \"m1\"\n"
    ));
    let (code, err) = sync(&w);
    assert_eq!(code, ExitCode::SUCCESS, "{err}");
    assert_eq!(wit.fetched(), vec![LOOPBACK_SOURCE.to_owned()]);
    assert_eq!(fleet_file_opt_in(&w.fleet_file()), Some(OptIn::Key));
    assert_eq!(
        configured_source(Some(&w.aterm_toml()), Some(&w.roster())),
        Some(FleetSource::Key(LOOPBACK_SOURCE.to_owned()))
    );
    w.write_config("[fleet]\nmachine = \"m1\"\n");
    assert_eq!(
        configured_source(Some(&w.aterm_toml()), Some(&w.roster())),
        None
    );
}

#[test]
fn a_source_is_fetched_only_as_it_is_written() {
    let w = World::new();
    let env = w.env(UPKEEP_NOW);
    fs::write(w.home.join("fleet.csv"), ONE_ACCOUNT_CSV).unwrap();
    let ran: std::sync::Arc<std::sync::Mutex<Vec<Ran>>> = std::sync::Arc::default();
    let run_as = |code: i32, stdout: &'static str| {
        let ran = std::sync::Arc::clone(&ran);
        move |cmd: aterm_uds::spawnfd::Command| {
            let lossy = |s: &std::ffi::OsStr| s.to_string_lossy().into_owned();
            ran.lock().unwrap().push((
                lossy(cmd.get_program()),
                cmd.get_args().map(lossy).collect(),
            ));
            Ok(super::super::align::Captured {
                stdout: stdout.as_bytes().to_vec(),
                code: Some(code),
            })
        }
    };
    let failing = run_as(22, "refused");
    let took = |source: &str, run: &RunFetcher| {
        let got = fetch_source_text_with(&env, source, run);
        (got, std::mem::take(&mut *ran.lock().unwrap()))
    };
    let sheet_export = "https://docs.google.com/spreadsheets/d/FAKE-SHEET/export?format=csv&gid=0";
    let (refused, ran) = took("rclone:gdrive:AI Email Accounting.csv", &failing);
    let refused = refused.unwrap_err();
    assert!(
        refused.contains("rclone remotes are not read")
            && refused.contains("/export?format=csv")
            && refused.contains(FLEET_ROSTER_FILE),
        "{refused}"
    );
    assert!(ran.is_empty(), "an rclone remote runs no program: {ran:?}");
    for (source, program, last) in [
        (
            "https://docs.google.com/spreadsheets/d/FAKE-SHEET/edit#gid=0",
            "curl",
            sheet_export,
        ),
        (LOOPBACK_SOURCE, "curl", LOOPBACK_SOURCE),
    ] {
        let (got, ran) = took(source, &failing);
        assert!(got.is_err(), "{source}: a failed fetch is the answer");
        assert_eq!(ran.len(), 1, "{source}: one program, then nothing: {ran:?}");
        let (exe, args) = &ran[0];
        assert_eq!(
            Path::new(exe).file_name().and_then(|n| n.to_str()),
            Some(program),
            "{source}"
        );
        assert_eq!(args.last().map(String::as_str), Some(last), "{source}");
    }
    let (got, ran) = took("~/fleet.csv", &failing);
    assert_eq!(got.as_deref(), Ok(ONE_ACCOUNT_CSV));
    assert!(ran.is_empty(), "a file runs no program: {ran:?}");
    let (got, ran) = took("remote:Fleet.csv", &failing);
    assert!(got.is_err(), "no such file here");
    assert!(ran.is_empty(), "a bare `remote:path` is a file: {ran:?}");

    let (got, ran) = took(LOOPBACK_SOURCE, &run_as(0, ONE_ACCOUNT_CSV));
    assert_eq!(got.as_deref(), Ok(ONE_ACCOUNT_CSV));
    assert_eq!(ran.len(), 1);
}

type Ran = (String, Vec<String>);

#[test]
fn a_fleet_file_no_one_here_named_a_source_for_opts_nothing_in() {
    let w = World::new();
    w.write_config("[fleet]\nmachine = \"m1\"\n");
    let wit = Witness::answering(ONE_ACCOUNT_CSV);
    let upkept = || {
        upkeep(
            &w.home,
            &w.state,
            (&w.roster(), UPKEEP_NOW, UpkeepStep::Both),
            &wit.fx(),
        )
    };
    let as_0_113_wrote_it = concat!(
        "# Managed by `aterm harness accounts sync` — shared LAN fleet account roster.\n",
        "# Run `aterm harness accounts sync <path-or-url>` on any machine to materialize\n",
        "# its local `accounts.toml`.\n\n",
        "[[account]]\n",
        "label = \"m1\"\n",
        "machines = [\"m1\"]\n",
        "class = \"dedicated\"\n",
        "email = \"m1@example.test\"\n\n",
    );
    let fleet = FleetRoster::from_toml(as_0_113_wrote_it, "old").expect("0.113.0's file");
    for text in [
        as_0_113_wrote_it.to_owned(),
        fleet.to_toml(),
        fleet.to_toml_for(Some(OptIn::Key)),
    ] {
        fs::write(w.fleet_file(), &text).unwrap();
        assert_eq!(
            upkept(),
            Upkept::Unconfigured {
                fleet_file_ignored: true,
                renewal: false,
            },
            "{text}"
        );
    }

    let mut oversized = format!("{OPT_IN_KEY} = \"named\"\n#");
    oversized.push_str(&"x".repeat(SOURCE_TEXT_MAX_BYTES + 1 - oversized.len()));
    fs::write(w.fleet_file(), &oversized).unwrap();
    assert_eq!(
        configured_source(Some(&w.aterm_toml()), Some(&w.roster())),
        None
    );
    assert_eq!(
        upkept(),
        Upkept::Unconfigured {
            fleet_file_ignored: true,
            renewal: false,
        }
    );
    assert_eq!(wit.fetched(), Vec::<String>::new(), "nothing fetched");
    assert_eq!(wit.spawned(), 0, "no renewal started");
    assert!(!w.roster().exists(), "no roster written");

    let store = accounts::Store::at(&w.state);
    let paths = [
        w.roster(),
        store.state_file(),
        store.logins_file(),
        store.ledger_path(),
    ];
    let images = || {
        paths
            .iter()
            .map(|path| fs::read(path).ok())
            .collect::<Vec<_>>()
    };
    let before = images();
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let synced = run_accounts_sync_with(
        (&w.env(UPKEEP_NOW), None),
        (Some("~/fleet.csv"), None, false),
        (&mut out, &mut err),
        &SyncHow {
            fetch: &*wit.fetch,
            ladder: &[],
            fleet_snapshots: &[],
            heal: None,
            allow_empty_pool: false,
        },
    );
    assert_eq!(synced, ExitCode::FAILURE);
    assert!(String::from_utf8_lossy(&err).contains("sync file exceeds its limit"));
    assert_eq!(fs::read(w.fleet_file()).unwrap(), oversized.as_bytes());
    assert_eq!(
        images(),
        before,
        "an unretained image permits no publication"
    );
    assert!(!store.sync_needs_recovery());
}

#[test]
fn an_explicit_source_opts_in_a_bounded_local_fleet() {
    let w = World::new();
    w.write_config("[fleet]\nmachine = \"m1\"\n");
    let wit = Witness::answering(ONE_ACCOUNT_CSV);
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let synced = run_accounts_sync_with(
        (&w.env(UPKEEP_NOW), None),
        (Some("~/fleet.csv"), None, false),
        (&mut out, &mut err),
        &SyncHow {
            fetch: &*wit.fetch,
            ladder: &[],
            fleet_snapshots: &[],
            heal: None,
            allow_empty_pool: false,
        },
    );
    assert_eq!(
        synced,
        ExitCode::SUCCESS,
        "{}",
        String::from_utf8_lossy(&err)
    );
    assert_eq!(fleet_file_opt_in(&w.fleet_file()), Some(OptIn::Named));
    assert_eq!(
        upkeep(
            &w.home,
            &w.state,
            (&w.roster(), UPKEEP_NOW, UpkeepStep::Both),
            &wit.fx(),
        ),
        Upkept::Configured {
            kind: "fleet-file",
            roster: true,
            renewal: false
        }
    );
}

/// The login a session here runs on ends, and the upkeep signs it in by default
/// (`DESIGN-alab-one-mesh-2026-11-05.md` §4.15 rule 4); on a machine with no route, the
/// row's attempt is an error on record naming every route it tried, written once.
#[test]
fn a_signed_out_login_a_session_runs_on_is_renewed_by_default_and_a_machine_without_a_route_says_so()
 {
    let w = World::new();
    w.write_config("[fleet]\nmachine = \"m15\"\n");
    fs::write(
        w.home.join(".claude.json"),
        "{\"oauthAccount\":{\"emailAddress\":\"m15-2@example.test\"}}\n",
    )
    .unwrap();
    let wit = Witness::answering(concat!(
        "email,computer,claude\n",
        "m15@example.test,m15,1\n",
        "m15-2@example.test,m15,1\n",
        "m15-3@example.test,m15,1\n"
    ));
    let now = UPKEEP_NOW;
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let synced = run_accounts_sync_with(
        (&w.env(now), None),
        (Some("fleet.csv"), None, false),
        (&mut out, &mut err),
        &SyncHow {
            fetch: &*wit.fetch,
            ladder: &[],
            fleet_snapshots: &[],
            heal: None,
            allow_empty_pool: false,
        },
    );
    assert_eq!(
        synced,
        ExitCode::SUCCESS,
        "{}",
        String::from_utf8_lossy(&err)
    );
    let store = accounts::Store::at(&w.state);
    let mailbox_dir = store.dir.join(super::super::open_url::DEFAULT_MAILBOX_DIR);
    assert!(
        !mailbox_dir.exists(),
        "accounts sync must not fabricate an empty mailbox directory"
    );
    let file_roster = Roster::load(&w.roster()).unwrap();
    // A session here runs on m15-2, whose login here has just ended.
    store
        .transact_logins(now - 60, &mut None, |state, logins| {
            let mut events = accounts::note_session(state, "sid-m15", "m15-2", None, now - 60);
            events.extend(accounts::note_login_end(state, logins, "m15-2", now - 60));
            Ok(events)
        })
        .unwrap();
    assert_eq!(
        upkeep(
            &w.home,
            &w.state,
            (&w.roster(), now, UpkeepStep::Both),
            &wit.fx()
        ),
        Upkept::Configured {
            kind: "fleet-file",
            roster: true,
            renewal: false,
        }
    );
    assert_eq!(
        wit.spawned(),
        0,
        "without a mailbox or browser profile, upkeep starts no background sign-in"
    );
    // The incident of 2026-10-24: a session's row never signed in, and no route on the
    // machine. The row's attempt is an error on record naming every route it tried, never a
    // silent failure, and written once however often it is tried.
    let (logins, _) = store.load_logins(now).unwrap();
    let tried: Vec<(String, accounts::SignInAttempt)> = accounts::failing_sign_ins(
        &file_roster,
        &file_roster,
        &store.load(now).unwrap().0,
        &logins,
        now,
    );
    assert_eq!(
        tried
            .iter()
            .map(|(label, _)| label.as_str())
            .collect::<Vec<_>>(),
        ["m15-2"],
        "the session's row's attempt is on record, and no other row's"
    );
    for (label, attempt) in &tried {
        assert_eq!(
            attempt.outcome,
            accounts::AttemptOutcome::NoRoute,
            "{label}"
        );
        for route in [
            "profile(none)",
            "mailbox(none configured)",
            "vault(not a member)",
            "headless-browser(none installed)",
            "hosts(none configured)",
        ] {
            assert!(
                attempt.routes.contains(route),
                "{label}: {}",
                attempt.routes
            );
        }
    }
    let sign_ins = || {
        store
            .tail(100)
            .unwrap()
            .into_iter()
            .filter(|e| e.kind == accounts::Kind::SignIn)
            .count()
    };
    assert_eq!(sign_ins(), tried.len(), "one ledger event per row");
    let no_route_lines = || {
        wit.logged()
            .into_iter()
            .filter(|line| line.contains("no-route"))
            .collect::<Vec<_>>()
    };
    let lines = no_route_lines();
    assert_eq!(lines.len(), tried.len(), "one log line per try: {lines:?}");
    for (label, _) in &tried {
        assert!(
            lines
                .iter()
                .any(|l| l.contains(&format!("account {label}:")) && l.contains("mailbox(")),
            "{label}: {lines:?}"
        );
    }
    let upkept = |step| upkeep(&w.home, &w.state, (&w.roster(), now, step), &wit.fx());
    upkept(UpkeepStep::Renew);
    assert_eq!(
        sign_ins(),
        tried.len(),
        "an unchanged failure is not written again; first={tried:?}, ledger={:?}",
        store
            .tail(100)
            .unwrap()
            .into_iter()
            .filter(|event| event.kind == accounts::Kind::SignIn)
            .map(|event| (event.account, event.why, event.workflow))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        no_route_lines(),
        lines,
        "nothing logged while each row is held"
    );

    fs::create_dir_all(&mailbox_dir).unwrap();
    let fleet_aside = w.config.join("fleet-accounts.aside");
    fs::rename(w.fleet_file(), &fleet_aside).unwrap();

    assert_eq!(
        upkept(UpkeepStep::Both),
        Upkept::Unconfigured {
            fleet_file_ignored: false,
            renewal: true,
        },
        "a roster that stands is signed in with no source configured, by default"
    );
    assert_eq!(
        wit.spawned(),
        1,
        "the mailbox that appeared ends the backoff at once"
    );

    fs::rename(&fleet_aside, w.fleet_file()).unwrap();
    assert_eq!(
        upkept(UpkeepStep::Sync),
        Upkept::Configured {
            kind: "fleet-file",
            roster: true,
            renewal: false
        },
        "the sync step alone starts no sign-in"
    );
    assert_eq!(wit.spawned(), 1);
}

#[test]
fn a_held_accounts_lock_fails_the_sync_and_says_so() {
    let w = World::new();
    w.write_config("[fleet]\nmachine = \"m1\"\n");
    let store = accounts::Store::at(&w.state);
    let sync_in_thread = |w: &World| {
        let env = w.env(UPKEEP_NOW);
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let fetch = |_: &Env, _: &str| Ok(ONE_ACCOUNT_CSV.to_owned());
            let (mut out, mut err) = (Vec::new(), Vec::new());
            let code = run_accounts_sync_with(
                (&env, None),
                (Some("fleet.csv"), None, false),
                (&mut out, &mut err),
                &SyncHow {
                    fetch: &fetch,
                    ladder: &[Duration::from_millis(1); 3],
                    fleet_snapshots: &[],
                    heal: None,
                    allow_empty_pool: false,
                },
            );
            let _ = tx.send((code, String::from_utf8_lossy(&err).into_owned()));
        });
        rx.recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .expect("a sync under a held lock gives up within its ladder")
    };
    let held = store.acquire().expect("the test holds the lock");
    let (code, err) = sync_in_thread(&w);
    assert_eq!(code, ExitCode::FAILURE);
    assert!(err.contains(accounts::LOCK_HELD), "{err}");
    assert!(!w.roster().exists(), "no roster under a held lock");

    drop(held);
    let (code, err) = sync_in_thread(&w);
    assert_eq!(code, ExitCode::SUCCESS, "{err}");
    assert!(w.roster().exists());
}

#[test]
fn sync_accounts_for_init_preserves_key_opt_in_and_existing_fleet_roster() {
    let w = World::new();
    w.write_config("[fleet]\nmachine = \"m1\"\n");
    fs::write(
        w.home.join(".claude.json"),
        "{\"oauthAccount\":{\"emailAddress\":\"m1@example.test\"}}\n",
    )
    .unwrap();
    assert!(
        !has_init_accounts_source(&w.home, &w.state, &w.roster(), UPKEEP_NOW),
        "a single local account without a configured source must not trigger init sync"
    );

    let csv_path = w.home.join("fleet.csv");
    fs::write(
        &csv_path,
        "email,computer,claude\nm1@example.test,m1,1\nm1-2@example.test,m1,1\nm2@example.test,m2,1\n",
    )
    .unwrap();
    w.write_config(&format!(
        "[fleet]\naccounts_source = '{}'\nmachine = \"m1\"\n",
        csv_path.display()
    ));
    let (machine, m_count, enrolled, f_count) =
        sync_accounts_for_init(&w.home, &w.state, &w.roster(), UPKEEP_NOW)
            .expect("init sync from key");
    assert_eq!(machine, "m1");
    assert_eq!(m_count, 2);
    assert_eq!(enrolled, 1);
    assert_eq!(f_count, 3);
    assert_eq!(
        fleet_file_opt_in(&w.fleet_file()),
        Some(OptIn::Key),
        "sync_accounts_for_init from [fleet] accounts_source must record opt_in = \"key\""
    );

    w.write_config("[fleet]\nmachine = \"m1\"\n");
    assert_eq!(
        configured_source(Some(&w.aterm_toml()), Some(&w.roster())),
        None,
        "removing [fleet] accounts_source must opt the machine out"
    );
    assert!(
        has_init_accounts_source(&w.home, &w.state, &w.roster(), UPKEEP_NOW),
        "existing accounts.toml + fleet-accounts.toml can be upgraded by explicit init"
    );
    let (_, m_count2, _, f_count2) =
        sync_accounts_for_init(&w.home, &w.state, &w.roster(), UPKEEP_NOW)
            .expect("init sync from existing fleet-accounts.toml");
    assert_eq!(m_count2, 2);
    assert_eq!(
        f_count2, 3,
        "init sync must preserve all fleet rows in existing fleet-accounts.toml"
    );
    assert_eq!(fleet_file_opt_in(&w.fleet_file()), Some(OptIn::Named));
}

#[test]
fn sync_accounts_for_init_merges_newly_signed_in_local_accounts() {
    let w = World::new();
    w.write_config("[fleet]\nmachine = \"m27\"\n");
    fs::write(
        w.home.join(".claude.json"),
        "{\"oauthAccount\":{\"emailAddress\":\"sol-m27@example.test\"}}\n",
    )
    .unwrap();
    fs::write(
        w.home.join(".claude.json.backup"),
        "{\"oauthAccount\":{\"emailAddress\":\"m27-2@example.test\"}}\n",
    )
    .unwrap();
    let (machine, count, enrolled, fleet_count) =
        sync_accounts_for_init(&w.home, &w.state, &w.roster(), UPKEEP_NOW)
            .expect("initial sync from local profiles");
    assert_eq!(machine, "m27");
    assert_eq!(count, 2);
    assert_eq!(enrolled, 1);
    assert_eq!(fleet_count, 2);

    fs::remove_file(w.home.join(".claude.json.backup")).unwrap();
    fs::write(
        w.home.join(".claude.json"),
        "{\"oauthAccount\":{\"emailAddress\":\"solar.m27.3@example.test\"}}\n",
    )
    .unwrap();
    let (machine2, count2, _, fleet_count2) =
        sync_accounts_for_init(&w.home, &w.state, &w.roster(), UPKEEP_NOW + 10)
            .expect("second sync merges new local account");
    assert_eq!(machine2, "m27");
    assert_eq!(count2, 3);
    assert_eq!(fleet_count2, 3);

    let before = Roster::load(&w.roster()).unwrap();
    fs::write(
        w.home.join(".claude.json"),
        "{\"oauthAccount\":{\"emailAddress\":\"new@example.test\"}}\n",
    )
    .unwrap();
    let (_, count, _, fleet_count) =
        sync_accounts_for_init(&w.home, &w.state, &w.roster(), UPKEEP_NOW + 20)
            .expect("merge a colliding fallback label");
    assert_eq!((count, fleet_count), (4, 4));
    let merged = Roster::load(&w.roster()).unwrap();
    for row in &before.rows {
        assert_eq!(merged.row(&row.label).unwrap().email, row.email);
    }
    assert_eq!(
        merged.row("m27-4").unwrap().email.as_deref(),
        Some("new@example.test")
    );
    assert_eq!(merged.row_for_login(None).unwrap().label, "m27-4");
    let (_, count, _, fleet_count) =
        sync_accounts_for_init(&w.home, &w.state, &w.roster(), UPKEEP_NOW + 21)
            .expect("the same discovered email is not another account");
    assert_eq!((count, fleet_count), (4, 4));
    assert_eq!(Roster::load(&w.roster()).unwrap(), merged);

    fs::write(
        w.home.join(".claude.json"),
        "{\"oauthAccount\":{\"emailAddress\":\"later@example.test\"}}\n",
    )
    .unwrap();
    let roster_bytes = fs::read(w.roster()).unwrap();
    let fleet_bytes = fs::read(w.fleet_file()).unwrap();
    let malformed = b"[[account]\nlabel = \"broken\"\n";
    fs::write(w.roster(), malformed).unwrap();
    assert!(sync_accounts_for_init(&w.home, &w.state, &w.roster(), UPKEEP_NOW + 30).is_err());
    assert_eq!(fs::read(w.roster()).unwrap(), malformed);
    assert_eq!(fs::read(w.fleet_file()).unwrap(), fleet_bytes);
    fs::write(w.roster(), &roster_bytes).unwrap();

    let store = accounts::Store::at(&w.state);
    for path in [store.state_file(), store.logins_file()] {
        let prior = path.exists().then(|| fs::read(&path).unwrap());
        fs::write(&path, "{\"schema\":999}").unwrap();
        assert!(sync_accounts_for_init(&w.home, &w.state, &w.roster(), UPKEEP_NOW + 30).is_err());
        assert_eq!(fs::read(w.roster()).unwrap(), roster_bytes);
        assert_eq!(fs::read(w.fleet_file()).unwrap(), fleet_bytes);
        assert_eq!(fs::read(&path).unwrap(), b"{\"schema\":999}");
        match prior {
            Some(bytes) => fs::write(path, bytes).unwrap(),
            None => fs::remove_file(path).unwrap(),
        }
    }
    let (_, count, _, fleet_count) =
        sync_accounts_for_init(&w.home, &w.state, &w.roster(), UPKEEP_NOW + 30)
            .expect("publish after account state admits again");
    assert_eq!((count, fleet_count), (5, 5));

    let fresh = World::new();
    fresh.write_config("[fleet]\nmachine = \"m27\"\n");
    let fresh_secrets = fresh.home.join(".secrets");
    fs::create_dir(&fresh_secrets).unwrap();
    let fresh_source = fresh_secrets.join(FLEET_ROSTER_FILE);
    for (file, email) in [
        (".claude.json", "one@example.test"),
        (".claude.json.backup", "two@example.test"),
    ] {
        fs::write(
            fresh.home.join(file),
            format!("{{\"oauthAccount\":{{\"emailAddress\":\"{email}\"}}}}\n"),
        )
        .unwrap();
    }
    let store = accounts::Store::at(&fresh.state);
    fs::create_dir_all(&store.dir).unwrap();
    fs::write(store.state_file(), "{\"schema\":999}").unwrap();
    assert!(
        sync_accounts_for_init(&fresh.home, &fresh.state, &fresh.roster(), UPKEEP_NOW).is_err()
    );
    assert!(!fresh.roster().exists());
    assert!(!fresh.fleet_file().exists());
    assert!(!fresh_source.exists());
    assert_eq!(fs::read(store.state_file()).unwrap(), b"{\"schema\":999}");
    fs::remove_file(store.state_file()).unwrap();
    let (_, count, _, fleet_count) =
        sync_accounts_for_init(&fresh.home, &fresh.state, &fresh.roster(), UPKEEP_NOW)
            .expect("initial discovery publishes after admission");
    assert_eq!((count, fleet_count), (2, 2));
    assert!(
        !fresh_source.exists(),
        "discovery writes only its local projection"
    );

    let captured = fs::read(w.fleet_file()).unwrap();
    let mut concurrent =
        FleetRoster::from_toml(std::str::from_utf8(&captured).unwrap(), "captured").unwrap();
    let mut another = concurrent.accounts[0].clone();
    another.label = "m27-other".to_owned();
    another.email = "concurrent@example.test".to_owned();
    concurrent.accounts.push(another);
    let replacement = concurrent.to_toml_for(Some(OptIn::Named));
    let sync_across_change = |w: &World, expected: Option<Vec<u8>>, checked_path: &Path| {
        let state_dir = w.state.clone();
        let changed_path = checked_path.to_path_buf();
        let answer = String::from_utf8(captured.clone()).unwrap();
        let replacement = replacement.clone();
        let fetch = move |_: &Env, _: &str| {
            let store = accounts::Store::at(&state_dir);
            let _held = store.acquire().unwrap();
            fs::write(&changed_path, &replacement).unwrap();
            Ok(answer.clone())
        };
        let expected = FleetSnapshot {
            path: checked_path.to_path_buf(),
            bytes: expected,
        };
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let input = w.home.join("captured-input.toml");
        fs::write(&input, &captured).unwrap();
        let source = input.to_string_lossy().into_owned();
        let code = run_accounts_sync_with(
            (&w.env(UPKEEP_NOW + 40), None),
            (Some(&source), None, true),
            (&mut out, &mut err),
            &SyncHow {
                fetch: &fetch,
                ladder: &[],
                fleet_snapshots: &[expected],
                heal: None,
                allow_empty_pool: false,
            },
        );
        assert_eq!(code, ExitCode::FAILURE);
    };
    let roster_bytes = fs::read(w.roster()).unwrap();
    sync_across_change(&w, Some(captured.clone()), &w.fleet_file());
    assert_eq!(fs::read(w.roster()).unwrap(), roster_bytes);
    assert_eq!(fs::read(w.fleet_file()).unwrap(), replacement.as_bytes());

    let first = World::new();
    first.write_config("[fleet]\nmachine = \"m27\"\n");
    assert!(!first.fleet_file().exists());
    sync_across_change(&first, None, &first.fleet_file());
    assert!(!first.roster().exists());
    assert_eq!(
        fs::read(first.fleet_file()).unwrap(),
        replacement.as_bytes()
    );
    let first_secrets = first.home.join(".secrets");
    fs::create_dir(&first_secrets).unwrap();
    let first_source = first_secrets.join(FLEET_ROSTER_FILE);
    sync_across_change(&first, None, &first_source);
    assert!(!first.roster().exists());
    assert_eq!(fs::read(&first_source).unwrap(), replacement.as_bytes());
    assert_eq!(
        fs::read(first.fleet_file()).unwrap(),
        replacement.as_bytes()
    );

    for (secrets_source, merging) in [(true, false), (true, true), (false, false), (false, true)] {
        let race = World::new();
        race.write_config("[fleet]\nmachine = \"m27\"\n");
        let source = if secrets_source {
            let secrets = race.home.join(".secrets");
            fs::create_dir(&secrets).unwrap();
            secrets.join(FLEET_ROSTER_FILE)
        } else {
            race.fleet_file()
        };
        let mut initial =
            FleetRoster::from_toml(std::str::from_utf8(&captured).unwrap(), "initial").unwrap();
        initial.accounts.truncate(2);
        // The list discovery wrote: a named list takes only accounts this Mac shows
        // all of, and here only the first is still shown.
        fs::write(&source, initial.to_toml_for(Some(OptIn::Discovered))).unwrap();
        fs::write(
            race.home.join(".claude.json"),
            format!(
                "{{\"oauthAccount\":{{\"emailAddress\":\"{}\"}}}}\n",
                initial.accounts[0].email
            ),
        )
        .unwrap();
        let store = accounts::Store::at(&race.state);
        store.machine_id("0123456789abcdef").unwrap();
        let source_before = fs::read(&source).unwrap();
        sync_accounts_for_init(&race.home, &race.state, &race.roster(), UPKEEP_NOW + 50).unwrap();
        if merging {
            fs::write(
                race.home.join(".claude.json.backup"),
                "{\"oauthAccount\":{\"emailAddress\":\"pending.m27.4@example.test\"}}\n",
            )
            .unwrap();
        }
        let checked = store.claim_staging_dir("m27-3").unwrap();
        let paths = [
            source.clone(),
            race.roster(),
            race.fleet_file(),
            store.state_file(),
            store.logins_file(),
            store.ledger_path(),
            store.machine_file(),
            checked.join(accounts::RENEWAL_CHECKPOINT_FILE),
        ];
        let snapshot = || {
            paths
                .iter()
                .map(|path| fs::read(path).unwrap())
                .collect::<Vec<_>>()
        };
        let mut newer = None;
        let result = sync_accounts_for_init_with(
            &race.home,
            &race.state,
            &race.roster(),
            UPKEEP_NOW + 52,
            || {
                // Q completes after P's real source capture, before its lock admission.
                fs::write(
                    race.home.join(".claude.json.backup"),
                    "{\"oauthAccount\":{\"emailAddress\":\"newer.m27.3@example.test\"}}\n",
                )
                .unwrap();
                fs::write(
                    checked.join(".claude.json"),
                    "{\"oauthAccount\":{\"emailAddress\":\"newer.m27.3@example.test\"}}\n",
                )
                .unwrap();
                accounts::write_renewal_checkpoint(
                    "m27-3",
                    "newer.m27.3@example.test",
                    checked.to_str().unwrap(),
                    Some("max"),
                    UPKEEP_NOW + 51,
                    &store.load_logins(UPKEEP_NOW + 51).unwrap().0,
                )
                .unwrap();
                let (_, rows, _, fleet_rows) = sync_accounts_for_init(
                    &race.home,
                    &race.state,
                    &race.roster(),
                    UPKEEP_NOW + 51,
                )
                .unwrap();
                assert_eq!((rows, fleet_rows), (3, 3));
                let (logins, _) = store.load_logins(UPKEEP_NOW + 51).unwrap();
                assert_eq!(
                    logins.get("m27-3").unwrap().dir.as_deref(),
                    checked.to_str()
                );
                assert!(
                    fs::read_to_string(race.fleet_file())
                        .unwrap()
                        .contains("newer.m27.3@example.test")
                );
                if secrets_source {
                    assert_eq!(fs::read(&source).unwrap(), source_before);
                }
                newer = Some(snapshot());
            },
            &fetch_source_text,
        );
        assert_eq!(result.unwrap_err().retry_reason(), Some(Retry::Changed));
        assert_eq!(snapshot(), newer.unwrap());
    }
}

#[test]
fn upkeep_converges_from_secrets_mailbox_and_proactively_renews_due_or_signed_out_login() {
    let w = World::new();
    // The mailbox's accounts are m27's: the world says it is m27 rather
    // than leaving discovery to read the host's own name.
    w.write_config("[fleet]\nmachine = \"m27\"\n");
    let secrets_dir = w.home.join(".secrets");
    fs::create_dir_all(&secrets_dir).unwrap();
    fs::write(
        secrets_dir.join(super::super::headless_oauth::SECRETS_MAILBOX_TOML_FILE),
        concat!(
            "[passwords]\n",
            "\"sol-m27@example.test\" = \"pw1\"\n",
            "\"sun.m27.2@example.test\" = \"pw2\"\n",
        ),
    )
    .unwrap();

    let wit = Witness::answering("");
    assert!(has_init_accounts_source(
        &w.home,
        &w.state,
        &w.roster(),
        UPKEEP_NOW
    ));
    assert_eq!(
        upkeep(
            &w.home,
            &w.state,
            (&w.roster(), UPKEEP_NOW, UpkeepStep::Sync),
            &wit.fx()
        ),
        Upkept::Configured {
            kind: "fleet-file",
            roster: true,
            renewal: false,
        },
        "upkeep auto-discovers accounts from ~/.secrets/ai-mailbox.toml even without .claude.json"
    );
    let file_roster = Roster::load(&w.roster()).expect("accounts.toml materialized");
    assert_eq!(file_roster.rows.len(), 2);
    assert_eq!(
        file_roster.rows[0].email.as_deref(),
        Some("sol-m27@example.test")
    );
    assert_eq!(
        file_roster.rows[1].email.as_deref(),
        Some("sun.m27.2@example.test")
    );
    assert!(
        !needs_init_accounts_sync(&w.home, &w.state, &w.roster(), UPKEEP_NOW),
        "needs_init_accounts_sync is false when accounts.toml and fleet-accounts.toml match mailbox"
    );

    // A new mailbox account extends the local discovery projection on upkeep.
    fs::write(
        secrets_dir.join(super::super::headless_oauth::SECRETS_MAILBOX_TOML_FILE),
        concat!(
            "[passwords]\n",
            "\"sol-m27@example.test\" = \"pw1\"\n",
            "\"sun.m27.2@example.test\" = \"pw2\"\n",
            "\"star.m27.3@example.test\" = \"pw3\"\n",
        ),
    )
    .unwrap();
    assert!(
        needs_init_accounts_sync(&w.home, &w.state, &w.roster(), UPKEEP_NOW),
        "needs_init_accounts_sync detects a new mailbox account"
    );
    assert_eq!(
        upkeep(
            &w.home,
            &w.state,
            (&w.roster(), UPKEEP_NOW + 1, UpkeepStep::Sync),
            &wit.fx()
        ),
        Upkept::Configured {
            kind: "fleet-file",
            roster: true,
            renewal: false,
        }
    );
    let file_roster = Roster::load(&w.roster()).expect("accounts.toml updated with 3rd account");
    assert_eq!(file_roster.rows.len(), 3);
    assert_eq!(
        file_roster.rows[2].email.as_deref(),
        Some("star.m27.3@example.test")
    );
    assert!(!secrets_dir.join(FLEET_ROSTER_FILE).exists());
    assert!(
        fs::read_to_string(w.fleet_file())
            .unwrap()
            .contains("star.m27.3@example.test")
    );
    assert!(!needs_init_accounts_sync(
        &w.home,
        &w.state,
        &w.roster(),
        UPKEEP_NOW + 1
    ));

    let store = accounts::Store::at(&w.state);
    let old_dir = store.claim_staging_dir("m27").unwrap();
    let old_dir_text = old_dir.to_str().unwrap().to_owned();
    fs::write(
        old_dir.join(".claude.json"),
        "{\"oauthAccount\":{\"emailAddress\":\"sol-m27@example.test\",\"subscriptionType\":\"max\"}}\n",
    )
    .unwrap();
    let mut note = None;
    store
        .transact_logins(UPKEEP_NOW, &mut note, |state, logins| {
            let mut ev = accounts::note_renewed(
                state,
                logins,
                "m27",
                file_roster.row("m27").and_then(|r| r.dir.as_deref()),
                &old_dir_text,
                Some("max"),
                UPKEEP_NOW - 28 * 86_400,
            );
            accounts::note_login_notice(logins, "m27", 2, UPKEEP_NOW);
            ev.push(accounts::Event::new(UPKEEP_NOW, accounts::Kind::Use, "m27"));
            // A session here runs on m27: this Mac uses the login it renews.
            ev.extend(accounts::note_session(
                state,
                "s-1",
                "m27",
                None,
                UPKEEP_NOW - 3600,
            ));
            state.active = Some("m27".to_owned());
            Ok(ev)
        })
        .unwrap();

    let (state_due, _) = store.load(UPKEEP_NOW).unwrap();
    let (logins_due, _) = store.load_logins(UPKEEP_NOW).unwrap();
    let due_target = next_auto_renew_target(&file_roster, &state_due, &logins_due, UPKEEP_NOW)
        .expect("proactive renewal must select due enrolled account m27 before it expires");
    assert_eq!(due_target.label, "m27");

    let new_staging = store.claim_staging_dir("m27").unwrap();
    let new_staging_text = new_staging.to_str().unwrap().to_owned();
    fs::write(
        new_staging.join(".claude.json"),
        "{\"oauthAccount\":{\"emailAddress\":\"sol-m27@example.test\",\"subscriptionType\":\"max\"}}\n",
    )
    .unwrap();
    let status_got = super::super::align::Captured {
        stdout: b"{\"loggedIn\":true,\"authMethod\":\"claude.ai\",\"email\":\"sol-m27@example.test\",\"subscriptionType\":\"max\"}\n".to_vec(),
        code: Some(0),
    };
    assert!(complete_staging_renewal(
        &store,
        &file_roster,
        &due_target.label,
        &new_staging,
        &new_staging_text,
        &status_got,
        UPKEEP_NOW + 10,
    ));

    let (state_renewed, _) = store.load(UPKEEP_NOW + 10).unwrap();
    let (logins_renewed, _) = store.load_logins(UPKEEP_NOW + 10).unwrap();
    let bound_after = accounts::bound(&file_roster, &logins_renewed);
    let enrolled_after = accounts::enrolled(&bound_after);
    let who_old = accounts::ProcessLogin {
        login: Some(old_dir_text),
        default_home: true,
        route: crate::harness::accounts::AuthRoute::Stored,
    };
    let me_old = who_old.me(&bound_after);
    assert_eq!(
        me_old.left,
        Some("m27"),
        "running session on old staging dir must recognize its login was superseded by m27's renewal"
    );
    assert_eq!(
        accounts::advise(
            &bound_after,
            &state_renewed,
            &enrolled_after,
            &me_old,
            &accounts::Point::TurnEnd,
            accounts::Mode::Wall,
            UPKEEP_NOW + 10,
        ),
        accounts::Advice::Move {
            to: "m27".to_owned(),
            ahead: true,
            rotates: false,
            takes_canary: false,
            yields: None,
        },
        "at the next turn end (Point::TurnEnd), the session follows m27 to the new staging login with zero wall downtime"
    );

    let mut note = None;
    store
        .transact_logins(UPKEEP_NOW + 20, &mut note, |state, logins| {
            Ok(accounts::note_login_end(
                state,
                logins,
                "m27",
                UPKEEP_NOW + 20,
            ))
        })
        .unwrap();
    let (state_signed_out, _) = store.load(UPKEEP_NOW + 20).unwrap();
    let (logins_signed_out, _) = store.load_logins(UPKEEP_NOW + 20).unwrap();
    let signed_out_target = next_auto_renew_target(
        &file_roster,
        &state_signed_out,
        &logins_signed_out,
        UPKEEP_NOW + 20,
    )
    .expect("when the login a session runs on is signed out, it is the renewal target");
    assert_eq!(
        signed_out_target.label, "m27",
        "the session's row, and never m27-2, which has no login here and no session"
    );
}

#[cfg(unix)]
#[test]
fn renewal_checkpoints_preserve_obstructions_and_refuse_links_and_special_files() {
    use std::os::unix::ffi::OsStrExt as _;
    for kind in ["file", "directory", "symlink", "fifo"] {
        let w = World::new();
        let store = accounts::Store::at(&w.state);
        let staging = store.claim_staging_dir("m27").unwrap();
        let path = staging.join(accounts::RENEWAL_CHECKPOINT_FILE);
        let target = w.home.join("owner-work");
        accounts::write_renewal_checkpoint(
            "m27",
            "sol-m27@example.test",
            staging.to_str().unwrap(),
            Some("max"),
            UPKEEP_NOW,
            &store.load_logins(UPKEEP_NOW).unwrap().0,
        )
        .unwrap();
        let valid_bytes = fs::read(&path).unwrap();
        fs::remove_file(&path).unwrap();
        fs::write(&target, &valid_bytes).unwrap();
        match kind {
            "file" => fs::write(&path, b"unknown content").unwrap(),
            "directory" => fs::create_dir(&path).unwrap(),
            "symlink" => std::os::unix::fs::symlink(&target, &path).unwrap(),
            "fifo" => {
                let name = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
                // SAFETY: `name` is NUL-terminated and lives in this test's
                // exclusive directory. mkfifo retains no pointer.
                assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
            }
            _ => unreachable!(),
        }
        let before = fs::symlink_metadata(&path).unwrap();
        assert!(
            accounts::write_renewal_checkpoint(
                "m27",
                "sol-m27@example.test",
                staging.to_str().unwrap(),
                Some("max"),
                UPKEEP_NOW,
                &store.load_logins(UPKEEP_NOW).unwrap().0,
            )
            .is_err(),
            "{kind}"
        );
        assert!(
            accounts::read_renewal_checkpoint(
                &staging,
                &fs::canonicalize(&staging).unwrap(),
                UPKEEP_NOW,
            )
            .is_none(),
            "{kind}"
        );
        assert_eq!(
            fs::symlink_metadata(&path).unwrap().file_type(),
            before.file_type()
        );
        if kind == "file" {
            assert_eq!(fs::read(&path).unwrap(), b"unknown content");
        }
        assert_eq!(fs::read(&target).unwrap(), valid_bytes);
    }
}

#[test]
fn recovery_refuses_unproven_or_stale_checkpoints_without_changing_accounts() {
    for case in [
        "missing",
        "malformed",
        "schema",
        "legacy-schema",
        "parent-missing",
        "parent-negative",
        "parent-clock-missing",
        "parent-clock-null",
        "parent-exceeds-clock",
        "program",
        "label",
        "email",
        "dir",
        "future",
        "zero",
        "subscription",
        "oversized",
        "directory",
        "auth-equal",
        "auth-newer",
        "renewed-equal",
        "renewed-newer",
        "ended-equal",
        "wrong-equal",
        "served-newer",
        "notice",
        "limited",
    ] {
        let w = World::new();
        let store = accounts::Store::at(&w.state);
        store.machine_id("0123456789abcdef").unwrap();
        let row = Row {
            label: "m27".into(),
            class: Class::Preferred,
            dir: None,
            was: Vec::new(),
            vendor: Default::default(),
            email: Some("sol-m27@example.test".into()),
            identity_email: None,
            browser: None,
            held_by: None,
        };
        let roster = Roster { rows: vec![row] };
        let staging = store.claim_staging_dir("m27").unwrap();
        let profile = br#"{"oauthAccount":{"emailAddress":"sol-m27@example.test"}}"#;
        fs::write(staging.join(".claude.json"), profile).unwrap();
        let checked_at = UPKEEP_NOW - 10;
        // Keep the parent unchanged while testing independent timestamp, limit and
        // checkpoint-shape guards; adding a binding must not mask those refusals.
        store
            .transact_logins(UPKEEP_NOW, &mut None, |_, logins| {
                logins
                    .labels
                    .insert("m27".into(), accounts::Binding::default());
                Ok(Vec::new())
            })
            .unwrap();
        accounts::write_renewal_checkpoint(
            "m27",
            "sol-m27@example.test",
            staging.to_str().unwrap(),
            Some("max"),
            checked_at,
            &store.load_logins(UPKEEP_NOW).unwrap().0,
        )
        .unwrap();
        let proof_path = staging.join(accounts::RENEWAL_CHECKPOINT_FILE);
        let mut proof: Value =
            aterm_json::from_str(&fs::read_to_string(&proof_path).unwrap()).unwrap();
        match case {
            "missing" => fs::remove_file(&proof_path).unwrap(),
            "malformed" => fs::write(&proof_path, b"unfinished {").unwrap(),
            "oversized" => fs::write(&proof_path, vec![b' '; 4097]).unwrap(),
            "directory" => {
                fs::remove_file(&proof_path).unwrap();
                fs::create_dir(&proof_path).unwrap();
            }
            "schema"
            | "legacy-schema"
            | "parent-missing"
            | "parent-negative"
            | "parent-clock-missing"
            | "parent-clock-null"
            | "parent-exceeds-clock"
            | "program"
            | "label"
            | "email"
            | "dir"
            | "future"
            | "zero"
            | "subscription" => {
                let (key, value) = match case {
                    "schema" => ("schema", Value::from(3)),
                    "legacy-schema" => ("schema", Value::from(1)),
                    "parent-missing" => ("parent_generation", Value::Null),
                    "parent-negative" => ("parent_generation", Value::from(-1)),
                    "parent-clock-missing" | "parent-clock-null" => ("parent_clock", Value::Null),
                    "parent-exceeds-clock" => ("parent_generation", Value::from(1)),
                    "program" => ("program", Value::from("other")),
                    "label" => ("label", Value::from("m28")),
                    "email" => ("email", Value::from("someone@example.test")),
                    "dir" => ("dir", Value::from(w.home.to_str().unwrap())),
                    "future" => ("checked_at", Value::from(UPKEEP_NOW + 1)),
                    "zero" => ("checked_at", Value::from(0)),
                    _ => ("subscription", Value::from("x".repeat(33))),
                };
                proof.as_object_mut().unwrap().insert(key.into(), value);
                if case == "parent-missing" {
                    proof.as_object_mut().unwrap().remove("parent_generation");
                }
                if case == "parent-clock-missing" {
                    proof.as_object_mut().unwrap().remove("parent_clock");
                }
                fs::write(&proof_path, proof.to_string()).unwrap();
            }
            _ => {
                store
                    .transact_logins(UPKEEP_NOW, &mut None, |state, logins| {
                        let account = state.accounts.entry("m27".into()).or_default();
                        let binding = logins.labels.entry("m27".into()).or_default();
                        match case {
                            "auth-equal" | "auth-newer" => {
                                account.auth = accounts::Auth::SignedOut;
                                account.auth_at =
                                    Some(checked_at + i64::from(case == "auth-newer"));
                            }
                            "renewed-equal" | "renewed-newer" => {
                                binding.renewed_at =
                                    Some(checked_at + i64::from(case == "renewed-newer"))
                            }
                            "ended-equal" => binding.ended_at = Some(checked_at),
                            "wrong-equal" => binding.wrong_at = Some(checked_at),
                            "served-newer" => binding.served_at = Some(checked_at + 1),
                            "notice" => binding.notice_end = Some(UPKEEP_NOW + 100),
                            "limited" => {
                                account.walls.insert(
                                    "seven_day".into(),
                                    accounts::Wall {
                                        until: Some(UPKEEP_NOW + 100),
                                        at: UPKEEP_NOW - 1,
                                        by: "aa".into(),
                                        attempts: 0,
                                        cleared: false,
                                    },
                                );
                            }
                            _ => unreachable!(),
                        }
                        Ok(Vec::new())
                    })
                    .unwrap();
            }
        }
        let state = store.load(UPKEEP_NOW).unwrap().0;
        let logins = store.load_logins(UPKEEP_NOW).unwrap().0;
        let ledger = store.tail(10).unwrap();
        let guard = std::sync::Arc::new(std::sync::Mutex::new(AutoRenewGuard::default()));
        assert!(
            !recover_interrupted_staging_renewals(&store, &roster, &guard, UPKEEP_NOW),
            "{case}"
        );
        assert_eq!(store.load(UPKEEP_NOW).unwrap().0, state, "{case}");
        assert_eq!(store.load_logins(UPKEEP_NOW).unwrap().0, logins, "{case}");
        assert_eq!(store.tail(10).unwrap(), ledger, "{case}");
        assert_eq!(
            fs::read(staging.join(".claude.json")).unwrap(),
            profile,
            "{case}"
        );
        assert!(staging.exists(), "{case}");
    }
}

#[test]
fn recovery_prefers_the_latest_checked_account_across_prefixes_and_numeric_ties() {
    for case in ["cross-prefix", "numeric-tie", "verbatim-dot"] {
        let cross_prefix = case == "cross-prefix";
        let numeric_tie = case == "numeric-tie";
        let w = World::new();
        let store = accounts::Store::at(&w.state);
        let roster = Roster {
            rows: [
                ("m27", "sol-m27@example.test"),
                ("m27-2", "sun.m27.2@example.test"),
            ]
            .into_iter()
            .map(|(label, email)| Row {
                label: label.into(),
                class: Class::Preferred,
                dir: Some(store.login_dir(label).unwrap().to_str().unwrap().into()),
                was: Vec::new(),
                vendor: Default::default(),
                email: Some(email.into()),
                identity_email: None,
                browser: None,
                held_by: None,
            })
            .collect(),
        };
        let old_prefix = if cross_prefix { "m27" } else { "m27-2" };
        if numeric_tie {
            store.claim_staging_dir(old_prefix).unwrap();
        }
        let older = store.claim_staging_dir(old_prefix).unwrap();
        if numeric_tie {
            for _ in 3..10 {
                store.claim_staging_dir(old_prefix).unwrap();
            }
        }
        let newer = store.claim_staging_dir("m27-2").unwrap();
        let newer_text = if case == "verbatim-dot" {
            format!("{}/./", newer.display())
        } else {
            newer.to_str().unwrap().to_owned()
        };
        // Synthetic crash points after the real checkpoint writer and
        // before binding; each status must pass the production fact reader.
        let newer_at = UPKEEP_NOW - 5;
        for (dir, checked_at, subscription) in [
            (&older, newer_at - i64::from(!numeric_tie), "pro"),
            (&newer, newer_at, "max"),
        ] {
            fs::write(dir.join(".claude.json"), b"private login fixture").unwrap();
            let status = super::super::align::Captured {
                stdout: format!(r#"{{"loggedIn":true,"authMethod":"claude.ai","email":"sun.m27.2@example.test","subscriptionType":"{subscription}"}}"#).into_bytes(),
                code: Some(0),
            };
            let login_text = if dir == &newer {
                newer_text.as_str()
            } else {
                dir.to_str().unwrap()
            };
            let row = &roster.rows[1];
            let email = row.email.as_deref().unwrap();
            let facts = super::super::cli::status_facts(&status, email).unwrap();
            assert!(facts.renews());
            accounts::write_renewal_checkpoint(
                &row.label,
                email,
                login_text,
                facts.subscription.as_deref(),
                checked_at,
                &store.load_logins(UPKEEP_NOW).unwrap().0,
            )
            .unwrap();
            assert!(!store.state_file().exists());
        }
        let guard = std::sync::Arc::new(std::sync::Mutex::new(AutoRenewGuard::default()));
        assert!(recover_interrupted_staging_renewals(
            &store, &roster, &guard, UPKEEP_NOW
        ));
        let logins = store.load_logins(UPKEEP_NOW).unwrap().0;
        let binding = &logins.labels["m27-2"];
        assert_eq!(binding.dir.as_deref(), Some(newer_text.as_str()));
        assert_eq!(binding.renewed_at, Some(newer_at));
        assert_eq!(binding.subscription.as_deref(), Some("max"));
        assert!(!logins.labels.contains_key("m27"));
        assert!(older.join(accounts::RENEWAL_CHECKPOINT_FILE).is_file());
        let events = store.tail(10).unwrap();
        assert_eq!(events.len(), 2);
        assert!(events.iter().all(|event| event.t == newer_at));
        assert!(!recover_interrupted_staging_renewals(
            &store,
            &roster,
            &guard,
            UPKEEP_NOW + 20
        ));
        assert_eq!(store.load_logins(UPKEEP_NOW + 20).unwrap().0, logins);
    }
}

#[test]
fn claude_alias_completion_and_recovery_require_the_canonical_identity() {
    for recover in [false, true] {
        for reported in ["canonical@example.test", "alias@example.test"] {
            let w = World::new();
            let store = accounts::Store::at(&w.state);
            let roster = Roster {
                rows: vec![Row {
                    label: "m27".into(),
                    email: Some("alias@example.test".into()),
                    identity_email: Some("canonical@example.test".into()),
                    dir: Some(store.login_dir("m27").unwrap().to_str().unwrap().into()),
                    ..Row::default()
                }],
            };
            let staging = store.claim_staging_dir("m27").unwrap();
            let staging_text = staging.to_str().unwrap();
            fs::write(
                staging.join(".claude.json"),
                format!(r#"{{"oauthAccount":{{"emailAddress":"{reported}"}}}}"#),
            )
            .unwrap();
            let accepted = if recover {
                accounts::write_renewal_checkpoint(
                    "m27",
                    reported,
                    staging_text,
                    Some("max"),
                    UPKEEP_NOW - 1,
                    &store.load_logins(UPKEEP_NOW).unwrap().0,
                )
                .unwrap();
                let guard = std::sync::Arc::new(std::sync::Mutex::new(AutoRenewGuard::default()));
                recover_interrupted_staging_renewals(&store, &roster, &guard, UPKEEP_NOW)
            } else {
                let status = super::super::align::Captured {
                    stdout: format!(
                        r#"{{"loggedIn":true,"authMethod":"claude.ai","email":"{reported}","subscriptionType":"max"}}"#
                    )
                    .into_bytes(),
                    code: Some(0),
                };
                complete_staging_renewal(
                    &store,
                    &roster,
                    "m27",
                    &staging,
                    staging_text,
                    &status,
                    UPKEEP_NOW,
                )
            };
            let canonical = reported == "canonical@example.test";
            assert_eq!(
                accepted, canonical,
                "recover={recover}, reported={reported}"
            );
            let logins = store.load_logins(UPKEEP_NOW).unwrap().0;
            assert_eq!(
                logins.get("m27").and_then(|binding| binding.dir.as_deref()),
                canonical.then_some(staging_text)
            );
            if canonical {
                let proof = accounts::read_renewal_checkpoint(
                    &staging,
                    &fs::canonicalize(&staging).unwrap(),
                    UPKEEP_NOW,
                )
                .unwrap();
                assert_eq!(proof.email, "canonical@example.test");
            }
        }
    }
}

#[test]
fn recover_interrupted_staging_renewals_adopts_completed_login_and_preserves_other_work() {
    let w = World::new();
    let store = accounts::Store::at(&w.state);
    let roster = Roster {
        rows: vec![
            Row {
                label: "m27".to_owned(),
                class: Class::Preferred,
                dir: Some(
                    store
                        .logins_dir()
                        .join("m27")
                        .to_string_lossy()
                        .into_owned(),
                ),
                was: Vec::new(),
                vendor: Default::default(),
                email: Some("sol-m27@example.test".to_owned()),
                identity_email: None,
                browser: None,
                held_by: None,
            },
            Row {
                label: "m27-2".to_owned(),
                class: Class::Preferred,
                dir: Some(
                    store
                        .logins_dir()
                        .join("m27-2")
                        .to_string_lossy()
                        .into_owned(),
                ),
                was: Vec::new(),
                vendor: Default::default(),
                email: Some("sun.m27.2@example.test".to_owned()),
                identity_email: None,
                browser: None,
                held_by: None,
            },
        ],
    };
    let aborted = store.claim_staging_dir("m27").unwrap();
    fs::write(
        aborted.join(".claude.json"),
        "{\"firstStartTime\":\"2026-10-22T23:54:34.139Z\",\"userID\":\"3bbbc341\"}\n",
    )
    .unwrap();
    let another = store.claim_staging_dir("m27-2").unwrap();
    fs::write(
        another.join(".claude.json"),
        r#"{"oauthAccount":{"emailAddress":"sun.m27.2@example.test","organizationType":"claude_pro"}}"#,
    )
    .unwrap();
    let completed = store.claim_staging_dir("m27-2").unwrap();
    fs::write(
        completed.join(".claude.json"),
        "{\"oauthAccount\":{\"emailAddress\":\"sun.m27.2@example.test\",\"organizationType\":\"claude_max\"}}\n",
    )
    .unwrap();
    // Synthetic crash point after the real writer, before binding. The
    // production status reader still guards the checkpoint's identity.
    let checked_at = UPKEEP_NOW - 5;
    let status = super::super::align::Captured {
        stdout: br#"{"loggedIn":true,"authMethod":"claude.ai","email":"sun.m27.2@example.test","subscriptionType":"max"}"#.to_vec(),
        code: Some(0),
    };
    let row = &roster.rows[1];
    let email = row.email.as_deref().unwrap();
    let facts = super::super::cli::status_facts(&status, email).unwrap();
    assert!(facts.renews());
    accounts::write_renewal_checkpoint(
        &row.label,
        email,
        completed.to_str().unwrap(),
        facts.subscription.as_deref(),
        checked_at,
        &store.load_logins(UPKEEP_NOW).unwrap().0,
    )
    .unwrap();
    assert!(completed.join(accounts::RENEWAL_CHECKPOINT_FILE).is_file());
    assert!(!store.state_file().exists());
    let malformed = store.claim_staging_dir("m27").unwrap();
    fs::write(malformed.join(".claude.json"), b"unfinished profile {").unwrap();
    fs::write(malformed.join("credentials"), b"keep these bytes").unwrap();
    for dir in [&aborted, &malformed, &another] {
        fs::OpenOptions::new()
            .write(true)
            .open(dir.join(".claude.json"))
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(std::time::UNIX_EPOCH))
            .unwrap();
    }
    let unrelated = store.claim_staging_dir("m27").unwrap();
    fs::write(unrelated.join("owner-note"), b"unrelated work").unwrap();
    let wrong_account = store.claim_staging_dir("m27").unwrap();
    fs::write(
        wrong_account.join(".claude.json"),
        r#"{"oauthAccount":{"emailAddress":"unlisted@example.test"}}"#,
    )
    .unwrap();

    let guard = std::sync::Arc::new(std::sync::Mutex::new(AutoRenewGuard::default()));
    assert!(
        recover_interrupted_staging_renewals(&store, &roster, &guard, UPKEEP_NOW),
        "completed staging login for m27-2 must be recovered"
    );
    assert!(aborted.exists(), "an unfinished login is preserved");
    let (state, _) = store.load(UPKEEP_NOW).unwrap();
    let (logins, _) = store.load_logins(UPKEEP_NOW).unwrap();
    assert_eq!(
        state.accounts.get("m27-2").map(|a| a.auth),
        Some(accounts::Auth::Ok)
    );
    assert_eq!(
        logins.labels.get("m27-2").and_then(|l| l.dir.as_deref()),
        completed.to_str(),
        "the newest completed staging sequence is adopted before older credentials"
    );
    assert_eq!(
        logins
            .labels
            .get("m27-2")
            .and_then(|l| l.subscription.as_deref()),
        Some("max")
    );
    assert_eq!(logins.labels["m27-2"].renewed_at, Some(checked_at));
    assert_eq!(state.accounts["m27-2"].auth_at, Some(checked_at));

    assert!(
        !recover_interrupted_staging_renewals(&store, &roster, &guard, UPKEEP_NOW + 10),
        "already-recovered login is not recovered a second time"
    );
    assert!(
        aborted.exists(),
        "time does not grant permission to remove an unfinished login"
    );
    assert!(
        completed.exists(),
        "active enrolled staging directory is kept"
    );
    assert_eq!(
        fs::read(another.join(".claude.json")).unwrap(),
        br#"{"oauthAccount":{"emailAddress":"sun.m27.2@example.test","organizationType":"claude_pro"}}"#,
        "older completed credentials are retained"
    );
    assert_eq!(
        fs::read(malformed.join(".claude.json")).unwrap(),
        b"unfinished profile {"
    );
    assert_eq!(
        fs::read(malformed.join("credentials")).unwrap(),
        b"keep these bytes"
    );
    assert_eq!(
        fs::read(unrelated.join("owner-note")).unwrap(),
        b"unrelated work"
    );
    assert!(wrong_account.join(".claude.json").exists());
    assert!(
        !logins.labels.contains_key("m27"),
        "an unmatched email is not adopted"
    );

    assert!(!recover_interrupted_staging_renewals(
        &store,
        &roster,
        &guard,
        UPKEEP_NOW + 20,
    ));
    assert_eq!(store.load_logins(UPKEEP_NOW + 20).unwrap().0, logins);

    // Already-bound aliases and historical directories cannot be adopted
    // as a new renewal merely because the current account is signed out.
    accounts::write_renewal_checkpoint(
        "m27-2",
        "sun.m27.2@example.test",
        another.to_str().unwrap(),
        Some("max"),
        UPKEEP_NOW + 35,
        &store.load_logins(UPKEEP_NOW + 35).unwrap().0,
    )
    .unwrap();
    let alias = completed.join(".").to_string_lossy().into_owned();
    store
        .transact_logins(UPKEEP_NOW + 30, &mut None, |state, logins| {
            let binding = logins.labels.get_mut("m27-2").unwrap();
            binding.dir = Some(alias.clone());
            binding
                .was
                .push(Some(another.to_string_lossy().into_owned()));
            Ok(accounts::note_auth_in_context(
                state,
                logins,
                "m27-2",
                accounts::Auth::SignedOut,
                UPKEEP_NOW + 30,
            ))
        })
        .unwrap();
    let before = store.load_logins(UPKEEP_NOW + 30).unwrap().0;
    assert!(!recover_interrupted_staging_renewals(
        &store,
        &roster,
        &guard,
        UPKEEP_NOW + 40,
    ));
    assert_eq!(store.load_logins(UPKEEP_NOW + 40).unwrap().0, before);
    assert_eq!(
        store.load(UPKEEP_NOW + 40).unwrap().0.accounts["m27-2"].auth,
        accounts::Auth::SignedOut
    );
}

#[test]
fn held_account_selections_resume_the_latest_request_without_automatic_source_opt_in() {
    use std::sync::{Arc, Mutex};

    let w = World::new();
    w.write_config("[fleet]\nmachine = \"m27\"\n");
    let store = accounts::Store::at(&w.state);
    store.machine_id("0123456789abcdef").unwrap();
    let roster = Roster {
        rows: ["m27", "m27-2"]
            .into_iter()
            .map(|label| Row {
                label: label.into(),
                dir: Some(store.logins_dir().join(label).to_str().unwrap().into()),
                email: Some(format!("{label}@example.test")),
                ..Row::default()
            })
            .collect(),
    };
    fs::write(w.roster(), format_roster_toml(&roster).unwrap()).unwrap();
    let guard = Arc::new(Mutex::new(AutoRenewGuard::default()));
    type Work = Box<dyn FnOnce() + Send>;
    let queued: Arc<Mutex<Vec<Work>>> = Arc::new(Mutex::new(Vec::new()));
    let into_queue = Arc::clone(&queued);
    let spawn = move |work| {
        into_queue.lock().unwrap().push(work);
        Ok(())
    };
    let fetch = |_: &Env, _: &str| -> Result<String, String> {
        panic!("an explicit saved request must not fetch an automatic source")
    };
    let cooldown = Mutex::new(None);
    let lifetime = crate::harness::account_work::Lifetime::default();
    let mut fx = UpkeepEffects {
        scope: lifetime.enter(),
        fetch: &fetch,
        spawn_renewal: &spawn,
        sync_cooldown: &cooldown,
        rediscovery: &Mutex::new(Rediscovery::default()),
        renew_guard: &guard,
        find_browser: &|_: &Path| false,
        vault_sync: |_: &Path, _scope| super::super::sign_in_ladder::Vault::NotMember,
        spawn_vault: &|_work| Ok(()),
        default_check: |_: &Path, _scope| {
            crate::harness::cli::LoginCheck::CouldNotRun(String::new())
        },
        mailbox: &ladder_mailbox_files,
        log: std::sync::Arc::new(|_: &str| {}),
        run_attempt: None,
        save_oauth: |_, _| Ok("inert".into()),
        shared: shared_renewal::Effects::default(),
    };
    // Historical saved intent remains readable even where new FIFO admission
    // is unsupported; this test exercises continuation, not new admission.
    let select = |label| store.request_pin(label, UPKEEP_NOW).unwrap();
    let first = select("m27");
    let mut held = AutoRenewPermit::claim(&guard, "other", Instant::now(), false).unwrap();
    held.wake_pending = Some(w.state.clone());
    assert_eq!(
        renew_requested_pin(
            &w.home,
            &w.state,
            &w.roster(),
            first.clone(),
            UPKEEP_NOW,
            &guard,
            &spawn
        ),
        RenewalDispatch::Held
    );
    assert!(queued.lock().unwrap().is_empty());
    assert_eq!(
        store.load(UPKEEP_NOW).unwrap().0.pending_pin,
        Some(first.clone().into())
    );

    let latest = select("m27-2");
    assert_eq!(
        renew_requested_pin(
            &w.home,
            &w.state,
            &w.roster(),
            first.clone(),
            UPKEEP_NOW,
            &guard,
            &spawn
        ),
        RenewalDispatch::Superseded
    );
    drop(held);
    assert!(!guard.lock().unwrap().in_flight);
    assert!(store.wake_path().is_file());
    let admitted_scope = fx.scope.take();
    assert_eq!(
        resume_pending_pin(&w.home, &w.state, &w.roster(), UPKEEP_NOW, &fx),
        Some(RenewalDispatch::Held),
        "unscoped upkeep cannot detach a manual continuation"
    );
    let closed = crate::harness::account_work::Lifetime::default();
    fx.scope = closed.enter();
    let closed_pause = closed.pause().unwrap();
    assert_eq!(
        resume_pending_pin(&w.home, &w.state, &w.roster(), UPKEEP_NOW, &fx),
        Some(RenewalDispatch::Held),
        "closed upkeep cannot detach a manual continuation"
    );
    assert!(queued.lock().unwrap().is_empty());
    fx.scope = admitted_scope;
    closed_pause
        .try_wait()
        .expect("closed control retired its scope");
    assert_eq!(
        upkeep(
            &w.home,
            &w.state,
            (&w.roster(), UPKEEP_NOW, UpkeepStep::Renew),
            &fx
        ),
        Upkept::Unconfigured {
            fleet_file_ignored: false,
            renewal: true,
        },
        "the saved selection progressed with no source configured"
    );
    assert_eq!(queued.lock().unwrap().len(), 1);
    {
        let admitted = guard.lock().unwrap();
        assert!(
            !admitted.in_flight,
            "queued manual work has not begun authentication"
        );
        assert!(!admitted.last_attempt.contains_key("m27-2"));
        assert!(!admitted.last_attempt.contains_key("m27"));
    }
    store.cancel_pin_request(&first, UPKEEP_NOW).unwrap();
    assert_eq!(
        store.load(UPKEEP_NOW).unwrap().0.pending_pin,
        Some(latest.clone().into())
    );

    // Dropping injected work retires its counted owner without authentication.
    queued.lock().unwrap().clear();
    assert!(!guard.lock().unwrap().in_flight);
    assert_eq!(
        spawn_row_renewal(
            accounts::Cli::Claude,
            &w.home,
            &w.state,
            &w.roster(),
            roster.clone(),
            &accounts::Logins::default(),
            (
                roster.rows[1].clone(),
                crate::harness::sign_in_ladder::asked_fixture(
                    accounts::Cli::Claude,
                    &roster.rows[1].label
                ),
            ),
            (None, true),
            UPKEEP_NOW,
            &guard,
            &spawn,
            None,
            None,
            &mut None,
        ),
        RenewalDispatch::Started,
        "unstarted manual work does not fabricate a completed sign-in cooldown"
    );
    queued.lock().unwrap().clear();
    let refusal = |_work: Box<dyn FnOnce() + Send>| Err(io::Error::other("injected spawn refusal"));
    let refused_fx = UpkeepEffects {
        spawn_renewal: &refusal,
        ..fx
    };
    assert_eq!(
        resume_pending_pin(&w.home, &w.state, &w.roster(), UPKEEP_NOW, &refused_fx),
        Some(RenewalDispatch::Held),
        "refused unstarted work retains its intent without late cancellation"
    );
    assert_eq!(
        store.load(UPKEEP_NOW).unwrap().0.pending_pin,
        Some(latest.into())
    );
    assert!(!guard.lock().unwrap().in_flight);

    let final_request = select("m27");
    let login = Path::new(roster.rows[0].dir.as_deref().unwrap());
    fs::create_dir_all(login).unwrap();
    fs::write(login.join(".credentials.json"), "private fixture").unwrap();
    // Exercise synchronous recovery under the original request directly;
    // enqueueing a detached owner is not evidence that recovery completed.
    let recover = |request: accounts::PinRequest| {
        Some(renew_requested_pin(
            &w.home,
            &w.state,
            &w.roster(),
            request,
            UPKEEP_NOW,
            &guard,
            &refusal,
        ))
    };
    assert_eq!(
        recover(final_request.clone()),
        Some(RenewalDispatch::Recovered)
    );
    let pinned = store.load(UPKEEP_NOW).unwrap().0;
    assert_eq!(pinned.active.as_deref(), Some("m27"));
    assert!(pinned.pinned);
    assert!(pinned.pending_pin.is_none());
    store
        .cancel_pin_request(&final_request, UPKEEP_NOW)
        .unwrap();
    assert_eq!(store.load(UPKEEP_NOW).unwrap().0, pinned);

    let retry = store.request_pin("m27", UPKEEP_NOW).unwrap();
    let before_fault = store.load(UPKEEP_NOW).unwrap().0;
    let ledger = store.ledger_path();
    let retained = ledger.with_extension("retained");
    fs::rename(&ledger, &retained).unwrap();
    fs::create_dir(&ledger).unwrap();
    assert_eq!(recover(retry.clone()), Some(RenewalDispatch::Fenced));
    assert_eq!(store.load(UPKEEP_NOW).unwrap().0, before_fault);
    assert_eq!(
        before_fault
            .pending_pin
            .as_ref()
            .and_then(accounts::PendingSelection::account),
        Some(&retry)
    );
    fs::rename(&ledger, ledger.with_extension("obstruction")).unwrap();
    fs::rename(&retained, &ledger).unwrap();
    assert_eq!(recover(retry.clone()), Some(RenewalDispatch::Recovered));
    assert!(store.load(UPKEEP_NOW).unwrap().0.pending_pin.is_none());
    drop(refused_fx);
    lifetime
        .pause()
        .unwrap()
        .try_wait()
        .expect("all queued work and fixture scopes retired");
}

/// A PERSON'S PIN OF A CLAUDE CODE ROW WAITS OUT ANOTHER CLI'S SIGN-IN: while a vendor's
/// sign-in (another process's, so this one's admission is free) holds the machine's sign-in
/// lock, the pin's renewal is Held before anything is admitted, its request stays pending
/// for a later upkeep, no cooldown starts and no work is queued. CONTROL: once the lock is
/// free the same request starts its renewal (dropped here unrun), holding the lock itself.
/// A saved account choice served with nobody at the desk (resumed by a tick, or a click
/// that waited in the sign-in queue) on a row whose only route is its desk browser profile,
/// which opens a window, is refused
/// before anything starts, so its caller retires it instead of opening a browser on a
/// timer. CONTROL: the same choice served for an uncontended click starts its sign-in,
/// which may open the window the person asked for.
// Its pin request needs a manual sign-in, which only Unix queues (`manual_signin_capability`).
#[cfg(unix)]
#[test]
fn a_saved_choice_nobody_waits_on_opens_no_window() {
    use std::sync::{Arc, Mutex};

    let w = World::new();
    w.write_config("[fleet]\nmachine = \"m27\"\n");
    let store = accounts::Store::at(&w.state);
    store.machine_id("0123456789abcdef").unwrap();
    let roster = Roster {
        rows: vec![Row {
            label: "m27".into(),
            dir: Some(store.logins_dir().join("m27").to_str().unwrap().into()),
            email: Some("m27@example.test".into()),
            // The row's desk browser profile: its only route, and one that opens a window.
            browser: Some("Profile 1".into()),
            ..Row::default()
        }],
    };
    fs::write(w.roster(), format_roster_toml(&roster).unwrap()).unwrap();
    let guard = Arc::new(Mutex::new(AutoRenewGuard::default()));
    type Work = Box<dyn FnOnce() + Send>;
    let queued: Arc<Mutex<Vec<Work>>> = Arc::new(Mutex::new(Vec::new()));
    let into_queue = Arc::clone(&queued);
    let spawn = move |work| {
        into_queue.lock().unwrap().push(work);
        Ok(())
    };
    let request = store.request_pin("m27", UPKEEP_NOW).unwrap();
    let serve = |at_desk: bool| {
        spawn_row_renewal(
            accounts::Cli::Claude,
            &w.home,
            &w.state,
            &w.roster(),
            roster.clone(),
            &accounts::Logins::default(),
            (
                roster.rows[0].clone(),
                crate::harness::sign_in_ladder::asked_fixture(accounts::Cli::Claude, "m27"),
            ),
            (Some(request.clone()), at_desk),
            UPKEEP_NOW,
            &guard,
            &spawn,
            None,
            None,
            &mut None,
        )
    };
    assert_eq!(serve(false), RenewalDispatch::Failed);
    assert!(
        queued.lock().unwrap().is_empty(),
        "nothing started for a choice nobody waits on"
    );
    assert!(!guard.lock().unwrap().in_flight);
    assert_eq!(serve(true), RenewalDispatch::Started);
    assert_eq!(queued.lock().unwrap().len(), 1);
    queued.lock().unwrap().clear();
}

// Its pin request needs a manual sign-in, which only Unix queues (`manual_signin_capability`).
#[cfg(unix)]
#[test]
fn a_vendor_sign_in_holding_the_lock_keeps_a_claude_pin_request_pending() {
    use std::sync::{Arc, Mutex};

    let w = World::new();
    w.write_config("[fleet]\nmachine = \"m27\"\n");
    let store = accounts::Store::at(&w.state);
    store.machine_id("0123456789abcdef").unwrap();
    let roster = Roster {
        rows: ["m27", "m27-2"]
            .into_iter()
            .map(|label| Row {
                label: label.into(),
                dir: Some(store.logins_dir().join(label).to_str().unwrap().into()),
                email: Some(format!("{label}@example.test")),
                ..Row::default()
            })
            .collect(),
    };
    fs::write(w.roster(), format_roster_toml(&roster).unwrap()).unwrap();
    let guard = Arc::new(Mutex::new(AutoRenewGuard::default()));
    type Work = Box<dyn FnOnce() + Send>;
    let queued: Arc<Mutex<Vec<Work>>> = Arc::new(Mutex::new(Vec::new()));
    let into_queue = Arc::clone(&queued);
    let spawn = move |work| {
        into_queue.lock().unwrap().push(work);
        Ok(())
    };
    let request = match store
        .select_account(&roster, &w.roster(), "m27", UPKEEP_NOW)
        .unwrap()
    {
        accounts::PinSelection::Renew(request) => request,
        accounts::PinSelection::Pinned => panic!("the fixture login is absent"),
    };
    let kimi = accounts::Store::at_for(&w.state, accounts::Cli::Kimi, &w.home);
    let vendor = kimi
        .signin_now()
        .unwrap()
        .expect("the vendor's sign-in takes the lock");
    assert!(
        store.signin_now().unwrap().is_none(),
        "the lock is held against this store too"
    );
    let renew = || {
        renew_requested_pin(
            &w.home,
            &w.state,
            &w.roster(),
            request.clone(),
            UPKEEP_NOW,
            &guard,
            &spawn,
        )
    };
    assert_eq!(renew(), RenewalDispatch::Held);
    assert!(queued.lock().unwrap().is_empty(), "nothing queued");
    assert_eq!(
        store.load(UPKEEP_NOW).unwrap().0.pending_pin,
        Some(request.clone().into()),
        "the person's pin stays pending"
    );
    {
        let admitted = guard.lock().unwrap();
        assert!(!admitted.in_flight);
        assert!(!admitted.last_attempt.contains_key("m27"), "no cooldown");
    }

    drop(vendor);
    assert_eq!(renew(), RenewalDispatch::Started, "CONTROL");
    assert_eq!(queued.lock().unwrap().len(), 1);
    assert!(
        kimi.signin_now().unwrap().is_none(),
        "CONTROL: the queued renewal holds the lock"
    );
    queued.lock().unwrap().clear();
    assert!(!guard.lock().unwrap().in_flight);
    assert!(
        kimi.signin_now().unwrap().is_some(),
        "released with its work"
    );
    store.cancel_pin_request(&request, UPKEEP_NOW).unwrap();
}

#[test]
fn recovering_another_login_keeps_a_pending_pin_until_its_account_is_eligible() {
    let w = World::new();
    let store = accounts::Store::at(&w.state);
    store.machine_id("0123456789abcdef").unwrap();
    let roster = Roster {
        rows: [
            ("m27", "sol-m27@example.test"),
            ("m27-2", "sun.m27.2@example.test"),
        ]
        .into_iter()
        .map(|(label, email)| Row {
            label: label.into(),
            class: Class::Preferred,
            dir: Some(store.logins_dir().join(label).to_str().unwrap().into()),
            was: Vec::new(),
            vendor: Default::default(),
            email: Some(email.into()),
            identity_email: None,
            browser: None,
            held_by: None,
        })
        .collect(),
    };
    let request = store.request_pin("m27", UPKEEP_NOW).unwrap();
    let staging = store.claim_staging_dir("m27-2").unwrap();
    fs::write(staging.join(".credentials.json"), b"private login fixture").unwrap();
    accounts::write_renewal_checkpoint(
        "m27-2",
        "sun.m27.2@example.test",
        staging.to_str().unwrap(),
        Some("max"),
        UPKEEP_NOW - 5,
        &store.load_logins(UPKEEP_NOW).unwrap().0,
    )
    .unwrap();
    let guard = std::sync::Arc::new(std::sync::Mutex::new(AutoRenewGuard::default()));
    assert!(recover_interrupted_staging_renewals(
        &store, &roster, &guard, UPKEEP_NOW
    ));
    assert_eq!(
        store.load_logins(UPKEEP_NOW).unwrap().0.labels["m27-2"]
            .dir
            .as_deref(),
        staging.to_str()
    );

    let complete = |request: &accounts::PinRequest, now| {
        let bound = store.bound(&roster, now).unwrap();
        let enrolled = accounts::enrolled(&bound);
        store
            .transact_within(&accounts::LOCK_LADDER, now, &mut None, |state| {
                Ok(
                    accounts::complete_requested_pin(state, &bound, &enrolled, request, now)
                        .into_iter()
                        .collect(),
                )
            })
            .unwrap()
    };
    let before = store.load(UPKEEP_NOW).unwrap().0;
    assert_eq!(
        before
            .pending_pin
            .as_ref()
            .and_then(accounts::PendingSelection::account),
        Some(&request)
    );
    assert!(complete(&request, UPKEEP_NOW).is_empty());
    assert_eq!(
        accounts::Store::at(&w.state).load(UPKEEP_NOW).unwrap().0,
        before,
        "recovering m27-2 cannot consume the still-unenrolled m27 selection"
    );

    let requested_home = Path::new(roster.rows[0].dir.as_deref().unwrap());
    fs::create_dir_all(requested_home).unwrap();
    fs::write(
        requested_home.join(".credentials.json"),
        b"private login fixture",
    )
    .unwrap();
    store
        .transact(UPKEEP_NOW + 1, &mut None, |state| {
            Ok(accounts::note_auth(
                state,
                "m27",
                accounts::Auth::SignedOut,
                UPKEEP_NOW + 1,
            ))
        })
        .unwrap();
    let signed_out = store.load(UPKEEP_NOW + 1).unwrap().0;
    assert!(complete(&request, UPKEEP_NOW + 1).is_empty());
    assert_eq!(
        accounts::Store::at(&w.state)
            .load(UPKEEP_NOW + 1)
            .unwrap()
            .0,
        signed_out,
        "a signed-out account also keeps its pending request"
    );
    store
        .transact(UPKEEP_NOW + 2, &mut None, |state| {
            Ok(accounts::note_auth(
                state,
                "m27",
                accounts::Auth::Ok,
                UPKEEP_NOW + 2,
            ))
        })
        .unwrap();
    let events = complete(&request, UPKEEP_NOW + 2);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].kind, accounts::Kind::Use);
    let pinned = store.load(UPKEEP_NOW + 2).unwrap().0;
    assert_eq!(pinned.active.as_deref(), Some("m27"));
    assert!(pinned.pinned);
    assert!(pinned.pending_pin.is_none());
    assert!(complete(&request, UPKEEP_NOW + 2).is_empty());
    assert_eq!(store.load(UPKEEP_NOW + 2).unwrap().0, pinned);

    let newer = store.request_pin("m27-2", UPKEEP_NOW + 3).unwrap();
    let superseded = store.load(UPKEEP_NOW + 3).unwrap().0;
    assert!(complete(&request, UPKEEP_NOW + 3).is_empty());
    assert_eq!(store.load(UPKEEP_NOW + 3).unwrap().0, superseded);
    let events = complete(&newer, UPKEEP_NOW + 3);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].kind, accounts::Kind::Use);
    let pinned = accounts::Store::at(&w.state)
        .load(UPKEEP_NOW + 3)
        .unwrap()
        .0;
    assert_eq!(pinned.active.as_deref(), Some("m27-2"));
    assert!(pinned.pinned);
    assert!(pinned.pending_pin.is_none());
}

#[test]
fn sync_use_and_renew_due_honor_bound_staging_logins_and_recover_checkpoints() {
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    let home = root.join("home");
    let cfg_dir = home.join(".config").join("aterm");
    let state_dir = root.join("state");
    fs::create_dir_all(&cfg_dir).unwrap();
    crate::test_config::write(cfg_dir.join("aterm.toml"), "[fleet]\nmachine = \"m27\"\n").unwrap();
    let source_path = root.join("accounts.csv");
    fs::write(
        &source_path,
        concat!(
            "email,computer,claude,notes\n",
            "sol-m27@example.test,m27,1,usable\n",
            "sun.m27.2@example.test,m27,1,usable\n",
            "solar.m27.3@example.test,m27,1,usable\n",
        ),
    )
    .unwrap();

    let store = accounts::Store::at(&state_dir);
    store
        .transact(UPKEEP_NOW - 20, &mut None, |state| {
            Ok(accounts::note_auth(
                state,
                "m27",
                accounts::Auth::SignedOut,
                UPKEEP_NOW - 20,
            ))
        })
        .unwrap();
    let staged_m27 = store.claim_staging_dir("m27").unwrap();
    fs::write(staged_m27.join(".credentials.json"), b"{}").unwrap();
    accounts::write_renewal_checkpoint(
        "m27",
        "sol-m27@example.test",
        staged_m27.to_str().unwrap(),
        Some("max"),
        UPKEEP_NOW - 10,
        &store.load_logins(UPKEEP_NOW).unwrap().0,
    )
    .unwrap();

    let env = Env {
        state: state_dir.clone(),
        aterm_state: Some(root.join("aterm-state")),
        cwd: root.to_path_buf(),
        home: Some(home.clone()),
        claude_dir: Some(home.join(".claude")),
        claude_config_unusable: None,
        claude_profile: Some(home.join(".claude.json")),
        claude_custom_oauth: false,
        claude_session: None,
        claude_pid: None,
        codex_thread: None,
        agy_conversation: None,
        codex_home: None,
        xdg_config_home: None,
        codex_login: None,
        now: UPKEEP_NOW,
        tz_s: 0,
        sid: String::new(),
        sock: None,
        config: Some(cfg_dir.join("aterm.toml")),
        claude_tmp: None,
        claude_running: None,
    };
    let mut out = Vec::new();
    let mut err = Vec::new();
    let code = run_accounts_sync(
        &env,
        None,
        Some(source_path.to_str().unwrap()),
        Some("m27"),
        true,
        &mut out,
        &mut err,
    );
    assert_eq!(
        code,
        ExitCode::SUCCESS,
        "stderr: {}",
        String::from_utf8_lossy(&err)
    );
    let sync_json: Value = aterm_json::from_str(&String::from_utf8_lossy(&out)).expect("sync json");
    let m27_entry = sync_json["machine_accounts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["label"].as_str() == Some("m27"))
        .expect("m27 row in sync output");
    assert_eq!(m27_entry["enrolled"].as_bool(), Some(true));
    assert_eq!(m27_entry["condition"].as_str(), Some("usable"));
    let recovered = store
        .read_account_records(Some(&cfg_dir.join(accounts::ACCOUNTS_FILE)), UPKEEP_NOW)
        .unwrap();
    assert_eq!(
        recovered
            .state
            .account("m27")
            .current_auth(recovered.roster.row("m27").unwrap()),
        accounts::Auth::Ok,
    );
    assert_eq!(
        store
            .load_logins(UPKEEP_NOW)
            .unwrap()
            .0
            .get("m27")
            .and_then(|binding| binding.dir.as_deref()),
        staged_m27.to_str(),
    );
    let needs_renew: Vec<&str> = sync_json["needs_renew"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert!(
        !needs_renew.contains(&"m27"),
        "recovered and bound staging login m27 must not be in needs_renew: {needs_renew:?}"
    );

    // `accounts use m27` succeeds even though `logins/m27` is empty and the login is in `m27+1`.
    out.clear();
    err.clear();
    let use_code = super::super::cli::run_accounts(
        &super::super::cli::AccountsCmd::Use {
            label: "m27".to_owned(),
        },
        &env,
        &super::super::cli::LiveAccounts::default(),
        &mut out,
        &mut err,
    );
    assert_eq!(
        use_code,
        ExitCode::SUCCESS,
        "accounts use m27 failed: {}",
        String::from_utf8_lossy(&err)
    );

    // Now write a staging checkpoint for m27-2 while m27-3 is limited and m27 is limited,
    // and verify `accounts renew --due` recovers m27-2 without needing an interactive renew flow.
    store
        .transact(UPKEEP_NOW, &mut None, |st| {
            let mut ev = accounts::note_wall(
                st,
                "m27-3",
                "seven_day",
                Some(UPKEEP_NOW + 3600),
                UPKEEP_NOW,
                UPKEEP_NOW,
            );
            ev.extend(accounts::note_wall(
                st,
                "m27",
                "seven_day",
                Some(UPKEEP_NOW + 3600),
                UPKEEP_NOW,
                UPKEEP_NOW,
            ));
            Ok(ev)
        })
        .unwrap();
    let staged_m27_2 = store.claim_staging_dir("m27-2").unwrap();
    fs::write(staged_m27_2.join(".credentials.json"), b"{}").unwrap();
    accounts::write_renewal_checkpoint(
        "m27-2",
        "sun.m27.2@example.test",
        staged_m27_2.to_str().unwrap(),
        Some("max"),
        UPKEEP_NOW,
        &store.load_logins(UPKEEP_NOW).unwrap().0,
    )
    .unwrap();
    out.clear();
    err.clear();
    let renew_code = super::super::cli::run_accounts(
        &super::super::cli::AccountsCmd::Renew {
            labels: Vec::new(),
            due: true,
            on: None,
        },
        &env,
        &super::super::cli::LiveAccounts::default(),
        &mut out,
        &mut err,
    );
    assert_eq!(
        renew_code,
        ExitCode::SUCCESS,
        "renew --due failed: {}",
        String::from_utf8_lossy(&err)
    );
    assert!(
        String::from_utf8_lossy(&out).contains("nothing is due on this machine"),
        "stdout: {}",
        String::from_utf8_lossy(&out)
    );
    let (logins_after, _) = store.load_logins(UPKEEP_NOW).unwrap();
    assert_eq!(
        logins_after.get("m27-2").and_then(|b| b.dir.as_deref()),
        staged_m27_2.to_str(),
        "renew --due should recover interrupted staging checkpoint for m27-2"
    );

    // Verify `discover_local_fleet` finds emails from `<store>/logins/` checkpoints and `.claude.json`.
    fs::write(
        home.join(".claude.json"),
        r#"{"oauthAccount":{"emailAddress":"solar.m27.3@example.test"}}"#,
    )
    .unwrap();
    let discovered = discover_local_fleet(&env).expect("discover local fleet from logins_dir");
    let discovered_emails: BTreeSet<&str> = discovered
        .accounts
        .iter()
        .map(|a| a.email.as_str())
        .collect();
    assert!(discovered_emails.contains("sol-m27@example.test"));
    assert!(discovered_emails.contains("sun.m27.2@example.test"));
    assert!(discovered_emails.contains("solar.m27.3@example.test"));

    // Mark default row `m27-3` as SignedOut, then re-sync: `SignedOut` must not be overwritten
    // merely because `~/.claude.json` still contains `solar.m27.3@example.test`.
    store
        .transact_logins(UPKEEP_NOW + 5, &mut None, |st, lg| {
            Ok(accounts::note_login_end(st, lg, "m27-3", UPKEEP_NOW + 5))
        })
        .unwrap();
    out.clear();
    err.clear();
    assert_eq!(
        run_accounts_sync(
            &env,
            None,
            Some(source_path.to_str().unwrap()),
            Some("m27"),
            false,
            &mut out,
            &mut err,
        ),
        ExitCode::SUCCESS
    );
    let (state_resynced, _) = store.load(UPKEEP_NOW + 5).unwrap();
    assert_eq!(
        state_resynced.accounts["m27-3"].auth,
        accounts::Auth::SignedOut,
        "re-syncing must not overwrite a SignedOut default row with Auth::Ok"
    );
}

#[test]
fn automatic_attempt_summary_refuses_a_replacement_binding() {
    for changed in [false, true] {
        let w = World::new();
        let store = accounts::Store::at(&w.state);
        let path = store.login_dir("main").unwrap();
        let raw = Roster {
            rows: vec![Row {
                label: "main".into(),
                email: Some("main@example.test".into()),
                dir: Some(path.to_string_lossy().into_owned()),
                ..Row::default()
            }],
        };
        fs::create_dir_all(&w.config).unwrap();
        fs::write(w.roster(), format_roster_toml(&raw).unwrap()).unwrap();
        let renew = || {
            store
                .transact_pool_logins_within(
                    &[],
                    &w.roster(),
                    UPKEEP_NOW,
                    &mut None,
                    |_, state, logins| {
                        Ok(accounts::note_renewed(
                            state,
                            logins,
                            "main",
                            path.to_str(),
                            path.to_str().unwrap(),
                            None,
                            UPKEEP_NOW,
                        ))
                    },
                )
                .unwrap();
            store
                .transact_pool_logins_within(
                    &[],
                    &w.roster(),
                    UPKEEP_NOW,
                    &mut None,
                    |_, _, logins| {
                        accounts::admit_workflow(logins, "main")?;
                        Ok(Vec::new())
                    },
                )
                .unwrap()
        };
        renew();
        let captured = store.load_logins(UPKEEP_NOW).unwrap().0;
        let captured_row = accounts::bound(&raw, &captured)
            .row("main")
            .unwrap()
            .clone();
        if changed {
            renew();
        }
        let files = [
            store.state_file(),
            store.logins_file(),
            store.ledger_path(),
            store.wake_path(),
        ];
        let before = files.each_ref().map(|path| fs::read(path).ok());
        note_attempt(
            &store,
            (&w.roster(), &captured_row, &captured),
            accounts::AttemptOutcome::Failed,
            "fixture route",
            UPKEEP_NOW,
        );
        if changed {
            assert_eq!(files.each_ref().map(|path| fs::read(path).ok()), before);
        } else {
            assert_eq!(
                store
                    .load_logins(UPKEEP_NOW)
                    .unwrap()
                    .0
                    .get("main")
                    .unwrap()
                    .sign_in_attempt
                    .as_ref()
                    .unwrap()
                    .outcome,
                accounts::AttemptOutcome::Failed
            );
        }
    }
}

#[test]
fn automatic_completion_checks_the_generation_of_the_account_it_actually_matches() {
    for changed in [None, Some("main"), Some("other")] {
        let w = World::new();
        let store = accounts::Store::at(&w.state);
        let raw = Roster {
            rows: [
                ("main", "first@example.test"),
                ("other", "matched@example.test"),
            ]
            .into_iter()
            .map(|(label, email)| Row {
                label: label.into(),
                email: Some(email.into()),
                dir: Some(store.login_dir(label).unwrap().to_str().unwrap().into()),
                ..Default::default()
            })
            .collect(),
        };
        let path = w.roster();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, format_roster_toml(&raw).unwrap()).unwrap();
        store
            .transact_logins(UPKEEP_NOW - 10, &mut None, |state, logins| {
                let mut events = Vec::new();
                for row in &raw.rows {
                    events.extend(accounts::note_renewed(
                        state,
                        logins,
                        &row.label,
                        row.dir.as_deref(),
                        row.dir.as_deref().unwrap(),
                        None,
                        UPKEEP_NOW - 10,
                    ));
                }
                Ok(events)
            })
            .unwrap();
        let captured_logins = store.load_logins(UPKEEP_NOW).unwrap().0;
        let captured_rows = accounts::bound(&raw, &captured_logins);
        if let Some(label) = changed {
            store
                .transact_logins(UPKEEP_NOW - 10, &mut None, |state, logins| {
                    let old = logins.get(label).unwrap().clone();
                    let events = accounts::note_renewed(
                        state,
                        logins,
                        label,
                        old.dir.as_deref(),
                        old.dir.as_deref().unwrap(),
                        None,
                        UPKEEP_NOW - 10,
                    );
                    logins.labels.get_mut(label).unwrap().was = old.was;
                    Ok(events)
                })
                .unwrap();
        }
        assert_eq!(
            accounts::bound(&raw, &store.load_logins(UPKEEP_NOW).unwrap().0),
            captured_rows
        );
        let staging = store.claim_staging_dir("main").unwrap();
        fs::write(staging.join(".claude.json"), b"verified alternate account").unwrap();
        let files = [
            store.state_file(),
            store.logins_file(),
            store.ledger_path(),
            store.wake_path(),
        ];
        let before = files.each_ref().map(|path| fs::read(path).ok());
        let status = super::super::align::Captured {
            stdout: br#"{"loggedIn":true,"authMethod":"claude.ai","email":"matched@example.test","subscriptionType":"max"}"#.to_vec(),
            code: Some(0),
        };
        let outcome = complete_staging_renewal_result(
            &store,
            accounts::RenewalPolicy {
                roster_path: &path,
                roster: &raw,
                row: captured_rows.row("main").unwrap(),
                logins: &captured_logins,
            },
            &staging,
            staging.to_str().unwrap(),
            (&status, Some("fixture route")),
            UPKEEP_NOW,
            &|_| {},
        );
        if changed.is_none() {
            assert_eq!(outcome, super::super::cli::Renewal::OtherRenewed);
            assert_eq!(
                store
                    .load_logins(UPKEEP_NOW)
                    .unwrap()
                    .0
                    .get("main")
                    .unwrap()
                    .sign_in_attempt
                    .as_ref()
                    .unwrap()
                    .outcome,
                accounts::AttemptOutcome::Failed,
            );
            assert_eq!(
                store
                    .load_logins(UPKEEP_NOW)
                    .unwrap()
                    .0
                    .get("other")
                    .unwrap()
                    .dir
                    .as_deref(),
                staging.to_str()
            );
        } else {
            assert_eq!(outcome, super::super::cli::Renewal::Failed);
            assert_eq!(files.each_ref().map(|path| fs::read(path).ok()), before);
            assert!(!staging.join(accounts::RENEWAL_CHECKPOINT_FILE).exists());
        }
        assert_eq!(
            fs::read(staging.join(".claude.json")).unwrap(),
            b"verified alternate account"
        );
    }
}

#[test]
fn automatic_completion_refuses_removed_policy_without_recoverable_evidence() {
    for email in ["owner@example.test", "foreign@example.test"] {
        let w = World::new();
        let store = accounts::Store::at(&w.state);
        let raw = Roster {
            rows: vec![Row {
                label: "main".into(),
                email: Some("owner@example.test".into()),
                ..Row::default()
            }],
        };
        let roster_path = w.roster();
        fs::create_dir_all(roster_path.parent().unwrap()).unwrap();
        fs::write(&roster_path, format_roster_toml(&raw).unwrap()).unwrap();
        let captured = raw.row("main").unwrap();
        let staging = store.claim_staging_dir("main").unwrap();
        fs::write(staging.join(".claude.json"), b"completed private login").unwrap();
        fs::write(&roster_path, "").unwrap();
        let status = super::super::align::Captured {
            stdout: format!(r#"{{"loggedIn":true,"authMethod":"claude.ai","email":"{email}"}}"#)
                .into_bytes(),
            code: Some(0),
        };
        assert_eq!(
            complete_staging_renewal_result(
                &store,
                accounts::RenewalPolicy {
                    roster_path: &roster_path,
                    roster: &raw,
                    row: captured,
                    logins: &accounts::Logins::default(),
                },
                &staging,
                staging.to_str().unwrap(),
                (&status, None),
                UPKEEP_NOW,
                &|_| panic!("removed policy cannot publish"),
            ),
            super::super::cli::Renewal::Failed
        );
        assert!(!store.logins_file().exists());
        assert!(!store.state_file().exists());
        assert!(!staging.join(accounts::RENEWAL_CHECKPOINT_FILE).exists());
        assert_eq!(
            fs::read(staging.join(".claude.json")).unwrap(),
            b"completed private login"
        );
    }
}

#[test]
fn staging_completion_failures_stop_fallback_and_keep_verified_logins() {
    use super::super::cli::Renewal;

    for case in [
        "checkpoint",
        "ledger",
        "missing-snapshot",
        "wrong-account",
        "signed-out",
        "interrupted-status",
    ] {
        let dir = aterm_tempfile::Builder::new()
            .prefix("aterm-renewal-completion-")
            .tempdir()
            .unwrap();
        let store = accounts::Store::at(dir.path());
        store.machine_id("0123456789abcdef").unwrap();
        let roster = Roster {
            rows: vec![Row {
                label: "main".to_owned(),
                email: Some("owner@example.test".to_owned()),
                ..Row::default()
            }],
        };
        let roster_path = dir.path().join(accounts::ACCOUNTS_FILE);
        fs::write(&roster_path, format_roster_toml(&roster).unwrap()).unwrap();
        let staging = store.claim_staging_dir("main").unwrap();
        let profile = b"private verified login fixture";
        fs::write(staging.join(".claude.json"), profile).unwrap();
        let checkpoint = staging.join(accounts::RENEWAL_CHECKPOINT_FILE);
        if case == "checkpoint" {
            fs::write(&checkpoint, b"unrecognized private work").unwrap();
        } else if case == "ledger" {
            let mut state = accounts::State::default();
            accounts::note_auth(
                &mut state,
                "main",
                accounts::Auth::SignedOut,
                UPKEEP_NOW - 1,
            );
            store.save(&state).unwrap();
            fs::create_dir(store.ledger_path()).unwrap();
        } else if case == "missing-snapshot" {
            fs::create_dir(store.ledger_path()).unwrap();
        }
        let status = super::super::align::Captured {
            stdout: if case == "signed-out" {
                br#"{"loggedIn":false,"authMethod":"none"}"#.to_vec()
            } else {
                let email = if case == "wrong-account" {
                    "foreign@example.test"
                } else {
                    "owner@example.test"
                };
                format!(
                    r#"{{"loggedIn":true,"authMethod":"claude.ai","email":"{email}","subscriptionType":"max"}}"#
                )
                .into_bytes()
            },
            code: if case == "interrupted-status" {
                None
            } else {
                Some(if case == "signed-out" { 1 } else { 0 })
            },
        };
        let callbacks = std::cell::Cell::new(0);
        let outcome = complete_staging_renewal_result(
            &store,
            accounts::RenewalPolicy {
                roster_path: &roster_path,
                roster: &roster,
                row: roster.row("main").unwrap(),
                logins: &accounts::Logins::default(),
            },
            &staging,
            staging.to_str().unwrap(),
            (&status, None),
            UPKEEP_NOW,
            &|_| callbacks.set(callbacks.get() + 1),
        );
        assert_eq!(callbacks.get(), 0, "{case}: refused completion callback");
        match case {
            "checkpoint" | "ledger" | "missing-snapshot" => {
                assert_eq!(outcome, Renewal::Failed);
                assert!(!outcome.can_retry_authentication());
                assert_eq!(fs::read(staging.join(".claude.json")).unwrap(), profile);
                if case == "checkpoint" {
                    assert_eq!(fs::read(&checkpoint).unwrap(), b"unrecognized private work");
                    assert!(!store.logins_file().exists() && !store.state_file().exists());
                    assert!(store.tail(10).unwrap().is_empty());
                } else if case == "missing-snapshot" {
                    assert!(!store.logins_file().exists() && !store.state_file().exists());
                    assert!(store.ledger_path().is_dir());
                } else {
                    let binding = store.load_logins(UPKEEP_NOW).unwrap().0;
                    assert!(binding.get("main").is_none());
                    assert_eq!(
                        store.load(UPKEEP_NOW).unwrap().0.account("main").auth,
                        accounts::Auth::SignedOut
                    );
                    let proof = accounts::read_renewal_checkpoint(
                        &staging,
                        &fs::canonicalize(&staging).unwrap(),
                        UPKEEP_NOW,
                    )
                    .unwrap();
                    assert_eq!(proof.dir, staging.to_str().unwrap());
                    assert_eq!(proof.checked_at, UPKEEP_NOW);
                    assert!(store.ledger_path().is_dir());
                    fs::rename(
                        store.ledger_path(),
                        dir.path().join("retained-ledger-obstruction"),
                    )
                    .unwrap();
                    assert!(recover_interrupted_staging_renewals_with(
                        Cli::Claude,
                        &store,
                        &roster_path,
                        &std::sync::Arc::new(std::sync::Mutex::new(AutoRenewGuard::default())),
                        UPKEEP_NOW,
                        &[],
                        || {},
                    ));
                    assert_eq!(
                        store
                            .load_logins(UPKEEP_NOW)
                            .unwrap()
                            .0
                            .get("main")
                            .unwrap()
                            .dir
                            .as_deref(),
                        staging.to_str()
                    );
                    assert_eq!(
                        store.load(UPKEEP_NOW).unwrap().0.account("main").auth,
                        accounts::Auth::Ok
                    );
                    assert_eq!(fs::read(staging.join(".claude.json")).unwrap(), profile);
                }
            }
            "wrong-account" => {
                assert_eq!(outcome, Renewal::WrongAccount);
                assert!(outcome.can_retry_authentication());
                assert!(!staging.exists());
                let bindings = store.load_logins(UPKEEP_NOW).unwrap().0;
                let binding = bindings.get("main").unwrap();
                assert_eq!(binding.dir, None);
                assert_eq!(binding.wrong_at, Some(UPKEEP_NOW));
            }
            "interrupted-status" => {
                assert_eq!(outcome, Renewal::Failed);
                assert!(!outcome.can_retry_authentication());
                assert!(!staging.exists());
                assert!(!store.logins_file().exists() && !store.state_file().exists());
            }
            _ => {
                assert_eq!(outcome, Renewal::AuthFailed);
                assert!(outcome.can_retry_authentication());
                assert!(!staging.exists());
                assert!(!store.logins_file().exists() && !store.state_file().exists());
            }
        }
    }
}

#[cfg(unix)]
#[test]
fn complete_staging_renewal_records_wrong_account_and_auto_renew_advances_past_cooldown_or_unconfigured_row()
 {
    let w = World::new();
    let store = accounts::Store::at(&w.state);
    let m27_dir = store.logins_dir().join("m27");
    fs::create_dir_all(&m27_dir).unwrap();
    fs::write(
        m27_dir.join(".claude.json"),
        "{\"oauthAccount\":{\"emailAddress\":\"sol-m27@example.test\"}}\n",
    )
    .unwrap();

    let roster = Roster {
        rows: vec![
            Row {
                label: "m27".to_owned(),
                class: Class::Preferred,
                dir: Some(m27_dir.to_string_lossy().into_owned()),
                was: Vec::new(),
                vendor: Default::default(),
                email: Some("sol-m27@example.test".to_owned()),
                identity_email: None,
                browser: None,
                held_by: None,
            },
            Row {
                label: "m27-2".to_owned(),
                class: Class::Preferred,
                dir: Some(
                    store
                        .logins_dir()
                        .join("m27-2")
                        .to_string_lossy()
                        .into_owned(),
                ),
                was: Vec::new(),
                vendor: Default::default(),
                email: Some("sun.m27.2@example.test".to_owned()),
                identity_email: None,
                browser: None,
                held_by: None,
            },
            Row {
                label: "m27-3".to_owned(),
                class: Class::Preferred,
                dir: Some(
                    store
                        .logins_dir()
                        .join("m27-3")
                        .to_string_lossy()
                        .into_owned(),
                ),
                was: Vec::new(),
                vendor: Default::default(),
                email: Some("star.m27.3@example.test".to_owned()),
                identity_email: None,
                browser: Some("Claude m27-3".to_owned()),
                held_by: None,
            },
        ],
    };
    let mut roster_toml = String::new();
    for r in &roster.rows {
        roster_toml.push_str(&accounts::row_text(r).unwrap());
        roster_toml.push('\n');
    }
    fs::create_dir_all(&w.config).unwrap();
    fs::write(w.roster(), &roster_toml).unwrap();

    // m27-2 and m27-3 each carried a session here yesterday, and their logins here have
    // ended since: due logins this Mac uses (`DESIGN-alab-one-mesh-2026-11-05.md` §4.15
    // rule 4), which the upkeep and `accounts renew --due` renew.
    for label in ["m27-2", "m27-3"] {
        let made = store.logins_dir().join(label);
        fs::create_dir_all(&made).unwrap();
        fs::write(made.join(".claude.json"), "{}").unwrap();
    }
    let mut note = None;
    store
        .transact_logins(UPKEEP_NOW, &mut note, |state, logins| {
            let mut ev = accounts::note_auth(state, "m27", accounts::Auth::Ok, UPKEEP_NOW);
            for (sid, label) in [("s-2", "m27-2"), ("s-3", "m27-3")] {
                ev.extend(accounts::note_session(
                    state,
                    sid,
                    label,
                    None,
                    UPKEEP_NOW - 86_400,
                ));
                state.sessions.remove(sid);
                ev.extend(accounts::note_login_end(state, logins, label, UPKEEP_NOW));
            }
            let acc = state.accounts.entry("m27".to_owned()).or_default();
            acc.sample = Some(accounts::Sample {
                session_pct: Some(95),
                session_reset: Some(UPKEEP_NOW + 3600),
                week_pct: Some(50),
                week_reset: Some(UPKEEP_NOW + 86_400),
                at: UPKEEP_NOW,
                by: "m27".to_owned(),
                ..accounts::Sample::default()
            });
            state.active = Some("m27".to_owned());
            ev.push(accounts::Event::new(UPKEEP_NOW, accounts::Kind::Use, "m27"));
            Ok(ev)
        })
        .unwrap();

    let (logins_before_wrong, _) = store.load_logins(UPKEEP_NOW + 1).unwrap();
    let staging_m27_2 = store.claim_staging_dir("m27-2").unwrap();
    let staging_m27_2_text = staging_m27_2.to_str().unwrap().to_owned();
    fs::write(
        staging_m27_2.join(".claude.json"),
        "{\"oauthAccount\":{\"emailAddress\":\"sol-m27@example.test\"}}\n",
    )
    .unwrap();
    let wrong_status = super::super::align::Captured {
        stdout: b"{\"loggedIn\":true,\"authMethod\":\"claude.ai\",\"email\":\"sol-m27@example.test\",\"subscriptionType\":\"max\"}\n".to_vec(),
        code: Some(0),
    };
    assert!(
        !complete_staging_renewal(
            &store,
            &roster,
            &roster.rows[1].label,
            &staging_m27_2,
            &staging_m27_2_text,
            &wrong_status,
            UPKEEP_NOW + 5,
        ),
        "signing into already-enrolled low account m27 during m27-2's renewal must be refused as wrong-account"
    );
    assert!(
        !staging_m27_2.exists(),
        "refused wrong-account staging directory must be removed"
    );
    let (logins_after_wrong, _) = store.load_logins(UPKEEP_NOW + 5).unwrap();
    assert!(
        logins_after_wrong.get("m27").is_none(),
        "m27's binding must not be overwritten by m27-2's failed staging login"
    );
    assert_eq!(
        logins_after_wrong.get("m27-2").and_then(|b| b.wrong_at),
        Some(UPKEEP_NOW + 5),
        "wrong_at must be recorded on m27-2 when renewal signs into a different account"
    );

    let (state_now, _) = store.load(UPKEEP_NOW + 10).unwrap();
    let cands_before_wrong: Vec<String> =
        auto_renew_candidates(&roster, &state_now, &logins_before_wrong, UPKEEP_NOW + 10)
            .into_iter()
            .map(|r| r.label)
            .collect();
    assert_eq!(
        cands_before_wrong,
        vec!["m27-2".to_owned(), "m27-3".to_owned()],
        "without wrong_at, the due logins this Mac uses are returned in roster order"
    );
    let cands: Vec<String> =
        auto_renew_candidates(&roster, &state_now, &logins_after_wrong, UPKEEP_NOW + 10)
            .into_iter()
            .map(|r| r.label)
            .collect();
    assert_eq!(
        cands,
        vec!["m27-3".to_owned(), "m27-2".to_owned()],
        "m27-3 must be ordered ahead of m27-2 after m27-2 recorded wrong_at"
    );

    // m27-3's desk browser profile opens a window on the owner's screen, so an upkeep never
    // drives it: with nothing else on the machine, no sign-in starts, and each row's
    // attempt names the routes it tried.
    let wit = Witness::answering("");
    assert!(
        !auto_renew_next_account(&w.home, &w.state, &w.roster(), UPKEEP_NOW + 10, &wit.fx()),
        "a desk browser is never driven unattended"
    );
    assert_eq!(wit.spawned(), 0);
    let (logins_tried, _) = store.load_logins(UPKEEP_NOW + 10).unwrap();
    let attempt = |logins: &accounts::Logins, label: &str| {
        logins
            .get(label)
            .and_then(|b| b.sign_in_attempt.clone())
            .unwrap_or_else(|| panic!("{label}'s attempt is on record"))
    };
    let m27_3_tried = attempt(&logins_tried, "m27-3");
    assert_eq!(m27_3_tried.outcome, accounts::AttemptOutcome::NoRoute);
    assert!(
        m27_3_tried
            .routes
            .contains("desk-browser(opens a window: not used unattended)"),
        "{}",
        m27_3_tried.routes
    );
    assert_eq!(
        attempt(&logins_tried, "m27-2").outcome,
        accounts::AttemptOutcome::NoRoute
    );

    // A headless browser and a stored profile for m27-3 are a route: it is the one signed in.
    let m27_3_profile =
        super::super::headless_oauth::browser_profile_dir(&store.dir, Some("m27-3"));
    fs::create_dir_all(&m27_3_profile).unwrap();
    let wit = Witness {
        browser: Box::new(|_: &Path| true),
        ..Witness::answering("")
    };
    assert!(
        auto_renew_next_account(&w.home, &w.state, &w.roster(), UPKEEP_NOW + 10, &wit.fx()),
        "auto_renew_next_account must spawn renewal for m27-3"
    );
    assert_eq!(wit.spawned.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert!(
        wit.guard.lock().unwrap().last_attempt.contains_key("m27-3"),
        "m27-3 must be the account claimed by AutoRenewPermit"
    );
    fs::remove_dir_all(&m27_3_profile).unwrap();

    // Even with a fresh AutoRenewGuard (as after a process restart or CLI run),
    // if m27-2 has a route and m27-3 has none, background auto_renew_next_account
    // must respect m27-2's persisted 15-minute wrong_at cooldown before retrying it.
    let mut roster_m27_2_mailbox = roster.clone();
    roster_m27_2_mailbox.rows[2].browser = None;
    let mut roster_m27_2_toml = String::new();
    for r in &roster_m27_2_mailbox.rows {
        roster_m27_2_toml.push_str(&accounts::row_text(r).unwrap());
        roster_m27_2_toml.push('\n');
    }
    fs::write(w.roster(), &roster_m27_2_toml).unwrap();
    let m27_2_mailbox = store
        .dir
        .join(super::super::headless_oauth::MAILBOX_TOML_FILE);
    fs::write(
        &m27_2_mailbox,
        "imap_url = \"imaps://127.0.0.1:9/INBOX\"\n[passwords]\n\"sun.m27.2@example.test\" = \"pw\"\n",
    )
    .unwrap();
    let before_wrong =
        std::time::UNIX_EPOCH + Duration::from_secs(u64::try_from(UPKEEP_NOW - 100).unwrap());
    fs::OpenOptions::new()
        .append(true)
        .open(&m27_2_mailbox)
        .unwrap()
        .set_modified(before_wrong)
        .unwrap();
    let wit_cooldown = Witness::answering("");
    assert!(
        !auto_renew_next_account(
            &w.home,
            &w.state,
            &w.roster(),
            UPKEEP_NOW + 10,
            &wit_cooldown.fx()
        ),
        "background auto_renew_next_account must hold m27-2 during its 15-minute wrong_at cooldown"
    );
    assert_eq!(
        wit_cooldown
            .spawned
            .load(std::sync::atomic::Ordering::SeqCst),
        0
    );
    assert!(
        auto_renew_next_account(
            &w.home,
            &w.state,
            &w.roster(),
            UPKEEP_NOW + 910,
            &wit_cooldown.fx()
        ),
        "background auto_renew_next_account must retry m27-2 once its 15-minute wrong_at cooldown expires"
    );
    assert_eq!(
        wit_cooldown
            .spawned
            .load(std::sync::atomic::Ordering::SeqCst),
        1
    );
    assert!(
        wit_cooldown
            .guard
            .lock()
            .unwrap()
            .last_attempt
            .contains_key("m27-2")
    );
    fs::remove_file(&m27_2_mailbox).unwrap();

    // Configure a mailbox password for m27-3 only (and remove its browser profile so only
    // its mailbox credential makes it headless-capable).
    let mut roster_no_browser = roster.clone();
    roster_no_browser.rows[2].browser = None;
    let mut roster_no_browser_toml = String::new();
    for r in &roster_no_browser.rows {
        roster_no_browser_toml.push_str(&accounts::row_text(r).unwrap());
        roster_no_browser_toml.push('\n');
    }
    fs::write(w.roster(), &roster_no_browser_toml).unwrap();
    let secrets = w.home.join(".secrets");
    fs::create_dir_all(&secrets).unwrap();
    fs::write(
        secrets.join(super::super::headless_oauth::SECRETS_MAILBOX_TOML_FILE),
        "webmail_url = \"https://mail.proton.me/u/0/inbox\"\n[passwords]\nm27-3 = \"secret-pw\"\n",
    )
    .unwrap();
    let arr_cfg = super::super::headless_oauth::MailboxConfig::from_toml_str(
        r#"
[[accounts]]
label = "m27-2"
email = "sun.m27.2@example.test"
password = "arr-pw"
totp = "JBSWY3DPEHPK3PXP"

[passwords]
"*" = "fallback-pw"
"#,
        None,
    )
    .expect("parse [[accounts]] and wildcard passwords");
    assert_eq!(
        arr_cfg
            .password_for(Some("sun.m27.2@example.test"))
            .as_deref(),
        Some("arr-pw")
    );
    assert_eq!(
        arr_cfg
            .totp_secret_for(Some("sun.m27.2@example.test"))
            .as_deref(),
        Some("JBSWY3DPEHPK3PXP")
    );
    assert_eq!(
        arr_cfg.password_for(Some("other@example.test")).as_deref(),
        Some("fallback-pw")
    );

    struct RecordingHost {
        tty: bool,
        flows: std::sync::Mutex<Vec<(String, super::super::cli::RenewBrowser)>>,
    }
    impl super::super::cli::AccountsHost for RecordingHost {
        fn stdin_is_terminal(&self) -> bool {
            self.tty
        }
        fn auth_status(&self, _: &str) -> Result<super::super::align::Captured, String> {
            Err("unused".to_owned())
        }
        fn unaskable(&self) -> Option<String> {
            None
        }
        fn login(&self, _: Option<&str>) -> Result<i32, String> {
            Err("unused".to_owned())
        }
        fn now(&self) -> i64 {
            UPKEEP_NOW + 20
        }
        fn renew_flow(
            &self,
            home: &str,
            email: &str,
            browser: &super::super::cli::RenewBrowser,
        ) -> Result<i32, String> {
            let _ = fs::write(Path::new(home).join(".credentials.json"), b"{}");
            self.flows
                .lock()
                .unwrap()
                .push((email.to_owned(), browser.clone()));
            Ok(0)
        }
        fn status_in_home(&self, _: &str) -> Result<super::super::align::Captured, String> {
            Ok(super::super::align::Captured {
                stdout: br#"{"loggedIn":true,"authMethod":"claude.ai","email":"star.m27.3@example.test","subscriptionType":"max"}"#.to_vec(),
                code: Some(0),
            })
        }
        fn current_exe(&self) -> Option<PathBuf> {
            Some(std::env::current_exe().expect("the test executable"))
        }
    }

    let env = w.env(UPKEEP_NOW + 20);
    let non_tty_host = RecordingHost {
        tty: false,
        flows: std::sync::Mutex::new(Vec::new()),
    };
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code_unconfigured = super::super::cli::run_accounts(
        &super::super::cli::AccountsCmd::Renew {
            labels: vec!["m27-2".to_owned()],
            due: false,
            on: None,
        },
        &env,
        &non_tty_host,
        &mut out,
        &mut err,
    );
    assert_eq!(
        code_unconfigured,
        ExitCode::from(1),
        "a named row left as it was is no renewal"
    );
    assert!(
        non_tty_host.flows.lock().unwrap().is_empty(),
        "non-terminal renew of m27-2 (which has no mailbox password) must not run renew_flow"
    );
    assert!(
        String::from_utf8_lossy(&out).contains("due here: m27-2"),
        "stdout: {}",
        String::from_utf8_lossy(&out)
    );

    out.clear();
    err.clear();
    let code_due = super::super::cli::run_accounts(
        &super::super::cli::AccountsCmd::Renew {
            labels: Vec::new(),
            due: true,
            on: None,
        },
        &env,
        &non_tty_host,
        &mut out,
        &mut err,
    );
    assert_eq!(
        code_due,
        ExitCode::SUCCESS,
        "stderr: {}",
        String::from_utf8_lossy(&err)
    );
    let recorded = non_tty_host.flows.lock().unwrap().clone();
    assert_eq!(
        recorded,
        vec![(
            "star.m27.3@example.test".to_owned(),
            super::super::cli::RenewBrowser::Remote {
                opener: Some(store.dir.join("bin").join(super::super::open_url::OPENER)),
            }
        )],
        "renew --due must skip unconfigured m27-2 and run headless-capable m27-3 with Remote browser"
    );

    let secret_file = w.home.join(".secrets/m27_2_password");
    fs::write(&secret_file, "secret-m27-2\n").unwrap();
    let mtime_with_secret = latest_mailbox_mtime(Some(&w.home), &store).expect("mtime");
    let secret_meta_mtime = fs::metadata(&secret_file).unwrap().modified().unwrap();
    assert!(
        mtime_with_secret >= secret_meta_mtime,
        "latest_mailbox_mtime must include individual secret files under ~/.secrets"
    );
    let _ = fs::remove_file(&secret_file);

    let maildir_new = store.dir.join("mailbox/new");
    fs::create_dir_all(&maildir_new).unwrap();
    let eml_file = maildir_new.join("msg1.eml");
    fs::write(
        &eml_file,
        "To: <sun.m27.2@example.test>\r\n\r\nCode: 123456\r\n",
    )
    .unwrap();
    let eml_meta_mtime = fs::metadata(&eml_file).unwrap().modified().unwrap();
    let mtime_with_eml = latest_mailbox_mtime(Some(&w.home), &store).expect("eml mtime");
    assert!(
        mtime_with_eml >= eml_meta_mtime,
        "latest_mailbox_mtime must include individual unclicked message files in Maildir"
    );
    let _ = fs::remove_dir_all(store.dir.join("mailbox"));

    // A cached headless browser profile for m27-2 must not be treated as headless-capable
    // when m27-2 already has wrong_at recorded and no mailbox is configured for m27-2.
    let m27_2_browser_dir =
        super::super::headless_oauth::browser_profile_dir(&store.dir, Some("m27-2"));
    fs::create_dir_all(&m27_2_browser_dir).unwrap();
    non_tty_host.flows.lock().unwrap().clear();
    out.clear();
    err.clear();
    let code_wrong_profile = super::super::cli::run_accounts(
        &super::super::cli::AccountsCmd::Renew {
            labels: vec!["m27-2".to_owned()],
            due: false,
            on: None,
        },
        &env,
        &non_tty_host,
        &mut out,
        &mut err,
    );
    assert_eq!(
        code_wrong_profile,
        ExitCode::from(1),
        "a named row left as it was is no renewal"
    );
    assert!(
        non_tty_host.flows.lock().unwrap().is_empty(),
        "row with wrong_at and no mailbox must not run headless via stale browser profile"
    );
    let _ = fs::remove_dir_all(&m27_2_browser_dir);

    let status = super::super::align::Captured {
        stdout: br#"{"loggedIn":true,"authMethod":"claude.ai","email":"sun.m27.2@example.test","subscriptionType":"max"}"#.to_vec(),
        code: Some(0),
    };
    // Failed store reads cannot publish a proof that recovery could later adopt.
    let state_bytes = fs::read(store.state_file()).unwrap();
    let login_bytes = fs::read(store.logins_file()).unwrap();
    let ledger = store.tail(20).unwrap();
    for blocked_file in [store.state_file(), store.logins_file()] {
        let kept = blocked_file.with_extension("saved");
        fs::rename(&blocked_file, &kept).unwrap();
        fs::create_dir(&blocked_file).unwrap();
        let staging = store.claim_staging_dir("m27-2").unwrap();
        let profile = b"private verified sign-in fixture";
        fs::write(staging.join(".claude.json"), profile).unwrap();
        assert!(!complete_staging_renewal(
            &store,
            &roster,
            "m27-2",
            &staging,
            staging.to_str().unwrap(),
            &status,
            UPKEEP_NOW + 20,
        ));
        assert_eq!(fs::read(staging.join(".claude.json")).unwrap(), profile);
        assert!(!staging.join(accounts::RENEWAL_CHECKPOINT_FILE).exists());
        assert!(blocked_file.is_dir());
        fs::remove_dir(&blocked_file).unwrap();
        fs::rename(&kept, &blocked_file).unwrap();
        assert_eq!(fs::read(store.state_file()).unwrap(), state_bytes);
        assert_eq!(fs::read(store.logins_file()).unwrap(), login_bytes);
        assert_eq!(store.tail(20).unwrap(), ledger);
    }

    let state_file = store.state_file();
    fs::write(&state_file, [0xff]).unwrap();
    let staging = store.claim_staging_dir("m27-2").unwrap();
    fs::write(staging.join(".claude.json"), b"checked sign-in fixture").unwrap();
    assert!(!complete_staging_renewal(
        &store,
        &roster,
        "m27-2",
        &staging,
        staging.to_str().unwrap(),
        &status,
        UPKEEP_NOW + 20,
    ));
    assert_eq!(fs::read(&state_file).unwrap(), [0xff]);
    assert_eq!(fs::read(store.logins_file()).unwrap(), login_bytes);
    assert_eq!(store.tail(20).unwrap(), ledger);
    assert_eq!(
        fs::read(staging.join(".claude.json")).unwrap(),
        b"checked sign-in fixture"
    );
    assert!(!staging.join(accounts::RENEWAL_CHECKPOINT_FILE).exists());
    fs::write(&state_file, &state_bytes).unwrap();

    let staging = store.claim_staging_dir("m27").unwrap();
    fs::write(staging.join(".claude.json"), b"new fallback login").unwrap();
    let staging_text = staging.to_str().unwrap();
    assert!(complete_staging_renewal(
        &store,
        &roster,
        "m27",
        &staging,
        staging_text,
        &status,
        UPKEEP_NOW + 30,
    ));
    let bound = store.load_logins(UPKEEP_NOW + 30).unwrap().0;
    let renewed = bound.get("m27-2").unwrap().clone();
    assert_eq!(renewed.dir.as_deref(), Some(staging_text));
    assert_eq!(renewed.renewed_at, Some(UPKEEP_NOW + 30));
    let proof = accounts::read_renewal_checkpoint(
        &staging,
        &fs::canonicalize(&staging).unwrap(),
        UPKEEP_NOW + 30,
    )
    .unwrap();
    assert_eq!(proof.label, "m27-2");
    assert_eq!(proof.dir, staging_text);

    for limited in [false, true] {
        if limited {
            store
                .transact_logins(UPKEEP_NOW + 35, &mut None, |state, _| {
                    Ok(accounts::note_wall(
                        state,
                        "m27-2",
                        "seven_day",
                        Some(UPKEEP_NOW + 3600),
                        UPKEEP_NOW + 35,
                        UPKEEP_NOW + 35,
                    ))
                })
                .unwrap();
        }
        let before_state = store.load(UPKEEP_NOW + 40).unwrap().0;
        let staging = store.claim_staging_dir("m27").unwrap();
        fs::write(staging.join(".claude.json"), b"wrong-account fixture").unwrap();
        assert!(!complete_staging_renewal(
            &store,
            &roster,
            "m27",
            &staging,
            staging.to_str().unwrap(),
            &status,
            UPKEEP_NOW + 40,
        ));
        assert!(!staging.exists());
        assert_eq!(store.load(UPKEEP_NOW + 40).unwrap().0, before_state);
        assert_eq!(
            store.load_logins(UPKEEP_NOW + 40).unwrap().0.get("m27-2"),
            Some(&renewed),
        );
    }

    // An interrupted staging renewal with a valid checkpoint must be recovered by `accounts use`.
    store
        .transact(UPKEEP_NOW + 45, &mut None, |state| {
            Ok(accounts::note_auth(
                state,
                "m27-3",
                accounts::Auth::SignedOut,
                UPKEEP_NOW + 45,
            ))
        })
        .unwrap();
    let use_staging = store.claim_staging_dir("m27-3").unwrap();
    fs::write(use_staging.join(".claude.json"), b"checkpointed m27-3").unwrap();
    let use_staging_text = use_staging.to_str().unwrap();
    accounts::write_renewal_checkpoint(
        "m27-3",
        "star.m27.3@example.test",
        use_staging_text,
        Some("max"),
        UPKEEP_NOW + 50,
        &store.load_logins(UPKEEP_NOW + 50).unwrap().0,
    )
    .unwrap();
    let env_use = w.env(UPKEEP_NOW + 55);
    out.clear();
    err.clear();
    let use_code = super::super::cli::run_accounts(
        &super::super::cli::AccountsCmd::Use {
            label: "m27-3".to_owned(),
        },
        &env_use,
        &non_tty_host,
        &mut out,
        &mut err,
    );
    assert_eq!(use_code, ExitCode::SUCCESS);
    let recovered_m27_3 = store
        .load_logins(UPKEEP_NOW + 55)
        .unwrap()
        .0
        .get("m27-3")
        .cloned()
        .expect("m27-3 login recovered by accounts use");
    assert_eq!(recovered_m27_3.dir.as_deref(), Some(use_staging_text));
    assert_eq!(recovered_m27_3.renewed_at, Some(UPKEEP_NOW + 50));

    // An identified single-account machine with only a per-account `~/.secrets/m27_password` file
    // (and no `ai-mailbox.toml` or `fleet-accounts.toml`) must be recognized by
    // `has_init_accounts_source` and converged by `sync_accounts_for_init`.
    let single_w = World::new();
    single_w.write_config("[fleet]\nmachine = \"m27\"\n");
    fs::write(
        single_w.home.join(".claude.json"),
        "{\"oauthAccount\":{\"emailAddress\":\"sol-m27@example.test\"}}\n",
    )
    .unwrap();
    let single_secrets = single_w.home.join(".secrets");
    fs::create_dir_all(&single_secrets).unwrap();
    fs::write(single_secrets.join("m27_password"), "single-secret\n").unwrap();
    assert!(has_init_accounts_source(
        &single_w.home,
        &single_w.state,
        &single_w.roster(),
        UPKEEP_NOW,
    ));
    let (mach, rows_n, _, _) = sync_accounts_for_init(
        &single_w.home,
        &single_w.state,
        &single_w.roster(),
        UPKEEP_NOW,
    )
    .expect("sync single account with per-account secret file");
    assert_eq!(mach, "m27");
    assert_eq!(rows_n, 1);

    // When `~/.secrets/ai-accounts.csv` gains a new account row, `needs_init_accounts_sync`
    // must detect the un-converged CSV row.
    let _ = fs::remove_file(single_secrets.join(FLEET_ROSTER_FILE));
    fs::write(
        single_secrets.join("ai-accounts.csv"),
        concat!(
            "email,computer,claude\n",
            "sol-m27@example.test,m27,1\n",
            "sun.m27.2@example.test,m27.2,1\n",
        ),
    )
    .unwrap();
    assert!(needs_init_accounts_sync(
        &single_w.home,
        &single_w.state,
        &single_w.roster(),
        UPKEEP_NOW + 60,
    ));
}

/// NO SWEEP (`DESIGN-alab-one-mesh-2026-11-05.md` §4.15 rule 4): a session's walled, low or
/// warned account signs in none of the rows that could take it (no session here runs on them
/// or ran on them within a week: the rotation takes a ready row, and a person's `accounts
/// login <label>` the rest), and a due
/// login is renewed only once a session on this Mac ran on it within the week, whatever the
/// pressure on it. CONTROL: the same records with that use renew the due login.
#[test]
fn a_pressed_session_signs_in_no_successor_and_a_due_login_waits_for_its_use_here() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let m27_2_dir = dir.path().join("m27-2-empty");
    let m27_3_dir = dir.path().join("m27-3-empty");
    fs::create_dir_all(&m27_2_dir).unwrap();
    fs::create_dir_all(&m27_3_dir).unwrap();
    let roster = Roster {
        rows: vec![
            accounts::Row {
                label: "m27".to_owned(),
                email: Some("m27@example.test".to_owned()),
                class: Class::Preferred,
                ..accounts::Row::default()
            },
            accounts::Row {
                label: "m27-2".to_owned(),
                dir: Some(m27_2_dir.display().to_string()),
                email: Some("m27-2@example.test".to_owned()),
                class: Class::Preferred,
                ..accounts::Row::default()
            },
            accounts::Row {
                label: "m27-3".to_owned(),
                dir: Some(m27_3_dir.display().to_string()),
                email: Some("m27-3@example.test".to_owned()),
                class: Class::Preferred,
                ..accounts::Row::default()
            },
        ],
    };
    let now = UPKEEP_NOW;
    let healthy = accounts::State {
        active: Some("m27".to_owned()),
        proven: true,
        ..accounts::State::default()
    };
    let mut low = healthy.clone();
    let _ = accounts::note_sample(
        &mut low,
        "m27",
        accounts::Sample {
            week_pct: Some(accounts::LOW_WEEK_PCT),
            week_reset: Some(now + 3 * 86_400),
            at: now - 30,
            ..accounts::Sample::default()
        },
        now,
    );
    // Claude Code's own warning that m27's week is filling, long before m27 is low.
    let mut warned = healthy.clone();
    warned
        .accounts
        .entry("m27".to_owned())
        .or_default()
        .warned_week = Some(accounts::WindowSaid {
        pct: Some(80),
        reset: Some(now + 3 * 86_400),
        at: now - 30,
    });
    assert!(!warned.accounts["m27"].low(now));
    let mut logins = accounts::Logins::default();
    logins.labels.insert(
        "m27".to_owned(),
        accounts::Binding {
            renewed_at: Some(now - 360 * 86_400),
            ..accounts::Binding::default()
        },
    );
    logins.labels.insert(
        "m27-2".to_owned(),
        accounts::Binding {
            wrong_at: Some(now - 86_400),
            ..accounts::Binding::default()
        },
    );
    let labels = |state: &accounts::State| -> Vec<String> {
        auto_renew_candidates(&roster, state, &logins, now)
            .into_iter()
            .map(|r| r.label)
            .collect()
    };
    for (pressure, state) in [("healthy", &healthy), ("low", &low), ("warned", &warned)] {
        assert!(
            labels(state).is_empty(),
            "{pressure}: no session here ran on m27, and none runs on m27-2 or m27-3"
        );
        let mut used = state.clone();
        let _ = accounts::note_session(&mut used, "s-1", "m27", None, now - 3600);
        used.sessions.clear();
        assert_eq!(
            labels(&used),
            ["m27"],
            "{pressure}: the due login this Mac used yesterday, and still no successor"
        );
        let mut stale = state.clone();
        let _ = accounts::note_session(
            &mut stale,
            "s-1",
            "m27",
            None,
            now - accounts::USED_WITHIN_S,
        );
        stale.sessions.clear();
        assert!(
            labels(&stale).is_empty(),
            "{pressure}: a week since this Mac ran on m27"
        );
    }
}

/// THE OWNER'S FIRST INCIDENT (`lab claude-rotation-proof` R4): a walled session's successor
/// this Mac ran on within the week, whose login directory has gone since (the sync records it
/// signed out), is the upkeep's to sign in again, by the renewal door ([`upkeep_sign_in`],
/// tier 1), though no login of it is enrolled here and no session runs on it now.
/// CONTROL: once a week has passed since a session here ran on it, it waits for a person.
#[test]
fn a_walled_sessions_used_successor_whose_login_went_is_signed_in_again() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let gone = dir.path().join("m27-2-gone");
    let roster = Roster {
        rows: vec![
            accounts::Row {
                label: "m27".to_owned(),
                email: Some("m27@example.test".to_owned()),
                class: Class::Preferred,
                ..accounts::Row::default()
            },
            accounts::Row {
                label: "m27-2".to_owned(),
                dir: Some(gone.display().to_string()),
                email: Some("m27-2@example.test".to_owned()),
                class: Class::Preferred,
                ..accounts::Row::default()
            },
        ],
    };
    assert!(
        !accounts::enrolled(&roster).contains("m27-2"),
        "its login is not here"
    );
    let now = UPKEEP_NOW;
    let logins = accounts::Logins::default();
    let at_wall = |ran: i64| {
        let mut state = accounts::State {
            active: Some("m27".to_owned()),
            proven: true,
            ..accounts::State::default()
        };
        let _ = accounts::note_session(&mut state, "s-2", "m27-2", None, ran);
        state.sessions.clear();
        let _ = accounts::note_auth(&mut state, "m27-2", accounts::Auth::SignedOut, now - 60);
        let _ = accounts::note_session(&mut state, "s-1", "m27", None, now - 120);
        let _ = accounts::note_wall(
            &mut state,
            "m27",
            "five_hour",
            Some(now + 3600),
            now - 30,
            now,
        );
        state
    };
    let tiers = |state: &accounts::State| -> Vec<(u8, String)> {
        auto_renew_candidates_tiered(
            Cli::Claude,
            None,
            &roster,
            state,
            &logins,
            &accounts::OtherMacs::NONE,
            now,
        )
        .into_iter()
        .map(|(tier, row)| (tier, row.label))
        .collect()
    };
    let used = at_wall(now - 3600);
    assert_eq!(
        tiers(&used),
        [(1, "m27-2".to_owned())],
        "a successor this Mac used"
    );
    let records = accounts::AccountRecords {
        raw_roster: roster.clone(),
        roster: roster.clone(),
        logins: logins.clone(),
        roster_present: true,
        state: used,
        notes: Vec::new(),
    };
    let none = accounts::OtherMacs::NONE;
    let bound = accounts::bound(&records.raw_roster, &records.logins);
    assert!(
        SignIn::session_sign_in(Cli::Claude, bound.row("m27-2").unwrap(), &records.state).is_none(),
        "no session here runs on m27-2: only the renewal door can admit it"
    );
    assert!(
        upkeep_sign_in(Cli::Claude, (&records, &none), &roster.rows[1], now).is_some(),
        "the renewal door admits it"
    );
    let stale = at_wall(now - accounts::USED_WITHIN_S);
    assert!(
        tiers(&stale).is_empty(),
        "CONTROL: a week since a session here ran on m27-2"
    );
    let stale_records = accounts::AccountRecords {
        state: stale,
        ..records
    };
    assert!(
        upkeep_sign_in(Cli::Claude, (&stale_records, &none), &roster.rows[1], now).is_none(),
        "CONTROL: nor does the door admit it"
    );
}

/// A session whose own row signed out waits on that row's sign-in (its lost login follows
/// only its own row), so the row is signed in, ahead of a due login this Mac uses and even
/// where another row is ready to run now; a signed-out row no session here runs on or ran on
/// within a week, and a row with no login here that none ran on, wait for a person
/// (`DESIGN-alab-one-mesh-2026-11-05.md` §4.15 rule 4).
#[test]
fn a_signed_out_row_a_session_runs_on_is_signed_in_first() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let made = |name: &str| {
        let at = dir.path().join(name);
        fs::create_dir_all(&at).unwrap();
        fs::write(at.join(".claude.json"), "{}").unwrap();
        at.display().to_string()
    };
    let empty = dir.path().join("m2-3-empty");
    fs::create_dir_all(&empty).unwrap();
    let row = |label: &str, dir: Option<String>| accounts::Row {
        label: label.to_owned(),
        dir,
        email: Some(format!("{label}@example.test")),
        class: Class::Preferred,
        ..accounts::Row::default()
    };
    let roster = Roster {
        rows: vec![
            row("m2", None),
            row("m2-3", Some(empty.display().to_string())),
            row("m2-4", Some(made("m2-4"))),
            row("m2-1", Some(made("m2-1"))),
            row("m2-2", Some(made("m2-2"))),
        ],
    };
    let now = UPKEEP_NOW;
    let mut state = accounts::State {
        active: Some("m2".to_owned()),
        proven: true,
        ..accounts::State::default()
    };
    for label in ["m2-1", "m2-2"] {
        let _ = accounts::note_auth(&mut state, label, accounts::Auth::SignedOut, now - 60);
    }
    let logins = accounts::Logins::default();
    let order = |state: &accounts::State, logins: &accounts::Logins| -> Vec<String> {
        auto_renew_candidates_tiered(
            Cli::Claude,
            None,
            &roster,
            state,
            logins,
            &accounts::OtherMacs::NONE,
            now,
        )
        .into_iter()
        .map(|(tier, row)| format!("{tier}:{}", row.label))
        .collect()
    };
    assert!(
        order(&state, &logins).is_empty(),
        "the control: no session runs on m2-1 or m2-2, and none here ran on them"
    );
    let mut running = state.clone();
    let _ = accounts::note_session(&mut running, "s-1", "m2-2", None, now - 30);
    assert_eq!(
        order(&running, &logins),
        ["0:m2-2"],
        "the session's own signed-out row, though m2-4 is ready"
    );
    // m2-1 ran here yesterday: its ended login is due, after the row a session waits on.
    let mut used = running.clone();
    let _ = accounts::note_session(&mut used, "s-0", "m2-1", None, now - 86_400);
    used.sessions.remove("s-0");
    assert_eq!(order(&used, &logins), ["0:m2-2", "1:m2-1"]);
    let mut back = used.clone();
    let _ = accounts::note_auth(&mut back, "m2-2", accounts::Auth::Ok, now - 10);
    assert_eq!(
        order(&back, &logins),
        ["1:m2-1"],
        "a row signed in again is no candidate"
    );
    // Two sessions' own rows: a wrong account sorts its row after the other.
    let mut both = running.clone();
    let _ = accounts::note_session(&mut both, "s-0", "m2-1", None, now - 30);
    let mut wrong = accounts::Logins::default();
    let _ = accounts::note_wrong_account(&mut wrong, "m2-1", now - 20);
    assert_eq!(order(&both, &wrong), ["0:m2-2", "0:m2-1"]);
    let mut wrong = accounts::Logins::default();
    let _ = accounts::note_wrong_account(&mut wrong, "m2-2", now - 20);
    assert_eq!(
        order(&both, &wrong),
        ["0:m2-1", "0:m2-2"],
        "the control: the other row's wrong account sorts it last"
    );
}

fn vault_ladder_fixture() -> (World, Row) {
    let w = World::new();
    let row = Row {
        label: "m2".to_owned(),
        class: Class::Preferred,
        email: Some("m2@example.test".to_owned()),
        ..Row::default()
    };
    fs::create_dir_all(&w.config).unwrap();
    fs::write(w.roster(), accounts::row_text(&row).unwrap()).unwrap();
    let source = w.home.join(".config/aterm/secrets/source.toml");
    fs::create_dir_all(source.parent().unwrap()).unwrap();
    fs::write(source, "# vault A\n").unwrap();
    (w, row)
}

#[test]
fn vault_work_claims_retire_without_penalizing_unstarted_or_cancelled_work() {
    use super::super::sign_in_ladder::{Vault, VaultTurn};
    for mode in 0..6 {
        let (w, row) = vault_ladder_fixture();
        let witness = Witness::answering("");
        let VaultInput::Read(key) = vault_input(&w.home) else {
            panic!("fixture source is readable")
        };
        let queued = std::sync::Mutex::new(Vec::<Box<dyn FnOnce() + Send>>::new());
        let spawn = |work: Box<dyn FnOnce() + Send>| match mode {
            1 => {
                drop(work);
                Err(io::Error::other("synthetic refusal"))
            }
            3 | 4 => {
                work();
                Ok(())
            }
            _ => {
                queued.lock().unwrap().push(work);
                Ok(())
            }
        };
        let mut fx = witness.fx();
        fx.spawn_vault = &spawn;
        fx.vault_sync = if mode == 3 {
            |_, _| panic!("synthetic vault unwind")
        } else {
            |_, scope| {
                assert!(scope.is_some());
                Vault::Unchanged(83)
            }
        };
        let lifetime = crate::harness::account_work::Lifetime::default();
        let scope = lifetime.enter().unwrap();
        let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            with_scope_effects(scope, fx, |fx| {
                let ctx = LadderCtx::of(fx, &w.roster());
                ladder_route(
                    &w.home,
                    &accounts::Store::at(&w.state),
                    &row,
                    UPKEEP_NOW,
                    &witness.guard,
                    &ctx,
                )
            })
        }));
        assert_eq!(ran.is_err(), mode == 3);
        let pause = lifetime.pause().unwrap();
        if matches!(mode, 0 | 2 | 5) {
            assert_eq!(queued.lock().unwrap().len(), 1);
            assert_eq!(
                witness
                    .guard
                    .lock()
                    .unwrap()
                    .ladder
                    .vault_turn(Some(key), Instant::now()),
                VaultTurn::Running
            );
        }
        let other = [42; 32];
        if mode == 5 {
            let mut guard = witness.guard.lock().unwrap();
            guard.ladder.vault_cancelled(key);
            assert_eq!(
                guard.ladder.vault_turn(Some(other), Instant::now()),
                VaultTurn::Due
            );
        }
        if mode == 2 {
            queued.lock().unwrap().pop().unwrap()();
        } else {
            queued.lock().unwrap().clear();
        }
        let drained = pause.try_wait().expect("all actual work has retired");
        let next = witness
            .guard
            .lock()
            .unwrap()
            .ladder
            .vault_turn(Some(if mode == 5 { other } else { key }), Instant::now());
        match mode {
            4 => assert_eq!(next, VaultTurn::Seen(Vault::Unchanged(83))),
            5 => assert_eq!(next, VaultTurn::Running),
            _ => assert_eq!(next, VaultTurn::Due),
        }
        assert!(drained.resume().is_ok());
    }
}

fn vault_ladder_pass(w: &World, witness: &Witness, row: &Row) -> LadderStep {
    let mut roster = Roster::load(&w.roster()).unwrap();
    if roster.row(&row.label) != Some(row) {
        if let Some(current) = roster
            .rows
            .iter_mut()
            .find(|current| current.label == row.label)
        {
            *current = row.clone();
        } else {
            roster.rows.push(row.clone());
        }
        fs::write(w.roster(), format_roster_toml(&roster).unwrap()).unwrap();
    }
    let effects = witness.fx();
    let context = LadderCtx::of(&effects, &w.roster());
    ladder_route(
        &w.home,
        &accounts::Store::at(&w.state),
        row,
        UPKEEP_NOW,
        &witness.guard,
        &context,
    )
}

/// A changed vault source is retried with the same guard, without waiting out the old
/// source's failed refresh. An unchanged source still shares that cached failure.
#[test]
fn a_changed_vault_source_does_not_reuse_the_previous_sources_failure() {
    use super::super::sign_in_ladder::Vault;
    use std::sync::atomic::{AtomicUsize, Ordering};
    static OLD: AtomicUsize = AtomicUsize::new(0);
    static NEW: AtomicUsize = AtomicUsize::new(0);
    fn old_source(_: &Path) -> Vault {
        OLD.fetch_add(1, Ordering::SeqCst);
        Vault::Failed("old source unavailable".to_owned())
    }
    fn new_source(_: &Path) -> Vault {
        NEW.fetch_add(1, Ordering::SeqCst);
        Vault::Unchanged(83)
    }
    OLD.store(0, Ordering::SeqCst);
    NEW.store(0, Ordering::SeqCst);
    let (w, row) = vault_ladder_fixture();
    let store = accounts::Store::at(&w.state);
    let source = w.home.join(".config/aterm/secrets/source.toml");
    let stamp = fs::metadata(&source).unwrap().modified().unwrap();
    let mut witness = Witness {
        vault_sync: |home, _scope| old_source(home),
        ..Witness::answering("")
    };
    let pass = |witness: &Witness| vault_ladder_pass(&w, witness, &row);
    assert!(matches!(pass(&witness), LadderStep::NoRoute));
    assert!(matches!(pass(&witness), LadderStep::Held));
    assert_eq!(OLD.load(Ordering::SeqCst), 1);
    fs::write(&source, "# vault B\n").unwrap();
    fs::File::options()
        .write(true)
        .open(&source)
        .unwrap()
        .set_modified(stamp)
        .unwrap();
    witness.vault_sync = |home, _scope| new_source(home);
    assert!(matches!(pass(&witness), LadderStep::NoRoute));
    assert_eq!(
        NEW.load(Ordering::SeqCst),
        1,
        "the new source must refresh before the previous source's wait expires"
    );
    assert_eq!(OLD.load(Ordering::SeqCst), 1);
    let second = Row {
        label: "m2-2".to_owned(),
        email: Some("m2-2@example.test".to_owned()),
        dir: Some(w.home.join("second-login").to_string_lossy().into_owned()),
        ..row.clone()
    };
    assert!(matches!(
        vault_ladder_pass(&w, &witness, &second),
        LadderStep::NoRoute
    ));
    assert_eq!(
        NEW.load(Ordering::SeqCst),
        1,
        "another row reuses the new source's cache"
    );
    let (logins, _) = store.load_logins(UPKEEP_NOW).unwrap();
    assert!(
        logins
            .get("m2")
            .unwrap()
            .sign_in_attempt
            .as_ref()
            .unwrap()
            .routes
            .contains("vault(seq 83, nothing new)")
    );
}

#[test]
fn a_vault_heartbeat_or_version_keeps_the_cache_context_and_a_new_standing_moves_it() {
    let (w, _) = vault_ladder_fixture();
    let state = w.home.join(".config/aterm/secrets/state.toml");
    fs::write(&state, "membership = \"in\"\nseq = 1\nheard = 1\n").unwrap();
    let initial = vault_input(&w.home);
    assert!(matches!(initial, VaultInput::Read(_)));
    fs::write(&state, "membership = \"in\"\nseq = 1\nheard = 2\n").unwrap();
    assert_eq!(vault_input(&w.home), initial);
    fs::write(
        &state,
        "membership = \"in\"\nseq = 2\ndigest = \"02\"\nheard = 2\n",
    )
    .unwrap();
    assert_eq!(vault_input(&w.home), initial, "a version applied here");
    fs::write(
        &state,
        "membership = \"in\"\nseq = 2\nheard = 2\n\n[network_materialization]\nversion = 1\n\
         source_identity = \"0c\"\n",
    )
    .unwrap();
    assert_eq!(vault_input(&w.home), initial, "a receipt's own identity");
    fs::write(&state, "membership = \"pending\"\nseq = 2\nheard = 2\n").unwrap();
    assert_ne!(vault_input(&w.home), initial, "a new membership");
    fs::write(
        &state,
        "membership = \"in\"\nseq = 2\nsource_identity = \"0b\"\nheard = 2\n",
    )
    .unwrap();
    assert_ne!(vault_input(&w.home), initial, "another vault");

    let refreshed = |at: u64, outcome: &str| {
        fs::write(
            &state,
            format!(
                "membership = \"in\"\nseq = 2\nheard = 2\n\n[network_refresh]\nversion = 1\n\
                 completed_at_ms = {at}\noutcome = \"{outcome}\"\n"
            ),
        )
        .unwrap();
        vault_input(&w.home)
    };
    let answered = refreshed(1_000, "succeeded");
    assert_eq!(refreshed(2_000, "succeeded"), answered, "another pass");
    assert_ne!(
        refreshed(3_000, "failed"),
        answered,
        "the vault stopped answering"
    );
    assert_eq!(refreshed(4_000, "succeeded"), answered, "and answers again");
}

/// A version published elsewhere that this Mac applied and that wrote nothing here neither
/// asks the vault again for a row with no route (another such row reuses the refresh) nor
/// ends another row's backoff: a refresh could only find that version applied. A new standing
/// does both, at once, since the vault may now hold what neither row had. NEGATIVE CONTROL:
/// binding the applied `seq` to the refresh's context again refreshes at the new version and
/// frees the waiting row.
#[test]
fn a_version_that_wrote_nothing_here_keeps_every_rows_wait() {
    use super::super::sign_in_ladder::Vault;
    use std::sync::atomic::{AtomicUsize, Ordering};
    static CALLS: AtomicUsize = AtomicUsize::new(0);
    fn unchanged(_: &Path) -> Vault {
        CALLS.fetch_add(1, Ordering::SeqCst);
        Vault::Unchanged(51)
    }
    let (w, row) = vault_ladder_fixture();
    let state = w.home.join(".config/aterm/secrets/state.toml");
    let standing = |membership: &str, seq: u64| {
        fs::write(
            &state,
            format!("membership = \"{membership}\"\nseq = {seq}\ndigest = \"{seq:064x}\"\n"),
        )
        .unwrap();
    };
    standing("in", 51);
    let witness = Witness {
        vault_sync: |home, _scope| unchanged(home),
        ..Witness::answering("")
    };
    assert!(matches!(
        vault_ladder_pass(&w, &witness, &row),
        LadderStep::NoRoute
    ));
    assert_eq!(CALLS.load(Ordering::SeqCst), 1);
    // A row whose route ran and failed waits on the same ladder.
    let (failing, ran, at) = ("m3", "mailbox ran", Instant::now());
    witness
        .guard
        .lock()
        .unwrap()
        .ladder
        .failed(failing, ran, at);

    standing("in", 52);
    assert!(matches!(
        vault_ladder_pass(&w, &witness, &row),
        LadderStep::Held
    ));
    let second = Row {
        label: "m2-2".to_owned(),
        email: Some("m2-2@example.test".to_owned()),
        dir: Some(w.home.join("second-login").to_string_lossy().into_owned()),
        ..row.clone()
    };
    assert!(matches!(
        vault_ladder_pass(&w, &witness, &second),
        LadderStep::NoRoute
    ));
    assert_eq!(CALLS.load(Ordering::SeqCst), 1, "nothing to ask the vault");
    assert!(
        witness.guard.lock().unwrap().ladder.held(failing, ran, at),
        "the other row still waits"
    );

    standing("pending", 52);
    assert!(matches!(
        vault_ladder_pass(&w, &witness, &row),
        LadderStep::NoRoute
    ));
    assert_eq!(
        CALLS.load(Ordering::SeqCst),
        2,
        "CONTROL: a new standing is asked"
    );
    assert!(
        !witness.guard.lock().unwrap().ladder.held(failing, ran, at),
        "and every wait ends"
    );
}

/// A row's refresh that failed while the vault did not answer this Mac stays cached through
/// the subscriber's other failed passes (another row with no route reuses it), and is asked
/// again at once when the subscriber's network refresh succeeds, not after its doubling wait.
/// NEGATIVE CONTROL: leaving `[network_refresh]`'s outcome out of the refresh's context keeps
/// the failure, and the row's backoff, past the vault's answer.
#[test]
fn a_failed_vault_refresh_is_asked_again_once_the_vault_answers() {
    use super::super::sign_in_ladder::Vault;
    use std::sync::atomic::{AtomicUsize, Ordering};
    static CALLS: AtomicUsize = AtomicUsize::new(0);
    fn reached(home: &Path) -> Vault {
        CALLS.fetch_add(1, Ordering::SeqCst);
        let state = fs::read_to_string(home.join(".config/aterm/secrets/state.toml")).unwrap();
        if state.contains("outcome = \"failed\"") {
            Vault::Failed("request timed out".to_owned())
        } else {
            Vault::Unchanged(51)
        }
    }
    let (w, row) = vault_ladder_fixture();
    let state = w.home.join(".config/aterm/secrets/state.toml");
    let refreshed = |seq: u64, at: u64, outcome: &str| {
        fs::write(
            &state,
            format!(
                "membership = \"in\"\nseq = {seq}\n\n[network_refresh]\nversion = 1\n\
                 completed_at_ms = {at}\noutcome = \"{outcome}\"\n"
            ),
        )
        .unwrap();
    };
    refreshed(51, 1_000, "failed");
    let witness = Witness {
        vault_sync: |home, _scope| reached(home),
        ..Witness::answering("")
    };
    assert!(matches!(
        vault_ladder_pass(&w, &witness, &row),
        LadderStep::NoRoute
    ));
    assert_eq!(CALLS.load(Ordering::SeqCst), 1);

    refreshed(51, 61_000, "failed");
    let second = Row {
        label: "m2-2".to_owned(),
        email: Some("m2-2@example.test".to_owned()),
        dir: Some(w.home.join("second-login").to_string_lossy().into_owned()),
        ..row.clone()
    };
    assert!(matches!(
        vault_ladder_pass(&w, &witness, &second),
        LadderStep::NoRoute
    ));
    assert_eq!(
        CALLS.load(Ordering::SeqCst),
        1,
        "another failed pass is no answer"
    );

    // Nothing was published meanwhile: the answer alone asks again.
    refreshed(51, 121_000, "succeeded");
    assert!(matches!(
        vault_ladder_pass(&w, &witness, &row),
        LadderStep::NoRoute
    ));
    assert_eq!(
        CALLS.load(Ordering::SeqCst),
        2,
        "the vault's answer asks it again"
    );
    let (logins, _) = accounts::Store::at(&w.state)
        .load_logins(UPKEEP_NOW)
        .unwrap();
    let routes = &logins
        .get(&row.label)
        .unwrap()
        .sign_in_attempt
        .as_ref()
        .unwrap()
        .routes;
    assert!(routes.contains("vault(seq 51, nothing new)"), "{routes}");
}

#[test]
fn a_changed_row_does_not_inherit_the_previous_identitys_retry_wait() {
    use super::super::sign_in_ladder::Vault;
    for changed in ["email", "identity", "browser", "directory"] {
        let (w, mut row) = vault_ladder_fixture();
        row.browser = Some("Profile 1".to_owned());
        let witness = Witness {
            vault_sync: |_, _scope| Vault::Unchanged(83),
            ..Witness::answering("")
        };
        assert!(matches!(
            vault_ladder_pass(&w, &witness, &row),
            LadderStep::NoRoute
        ));
        assert!(matches!(
            vault_ladder_pass(&w, &witness, &row),
            LadderStep::Held
        ));
        let mut replacement = row.clone();
        match changed {
            "email" => replacement.email = Some("replacement@example.test".to_owned()),
            "identity" => {
                replacement.identity_email = Some("provider@example.test".to_owned());
            }
            "browser" => replacement.browser = Some("Profile 2".to_owned()),
            "directory" => {
                replacement.dir = Some(w.home.join("replacement").to_string_lossy().into_owned());
            }
            _ => unreachable!(),
        }
        assert!(
            matches!(
                vault_ladder_pass(&w, &witness, &replacement),
                LadderStep::NoRoute
            ),
            "changed {changed} must be examined without inheriting the old row's wait"
        );
        assert!(matches!(
            vault_ladder_pass(&w, &witness, &replacement),
            LadderStep::Held
        ));
        assert!(matches!(
            vault_ladder_pass(&w, &witness, &row),
            LadderStep::NoRoute
        ));
    }
}

#[cfg(unix)]
#[test]
fn a_large_vault_inventory_uses_the_client_state_limit() {
    let (w, _) = vault_ladder_fixture();
    let store = accounts::Store::at(&w.state);
    let root = w.home.join(".config/aterm/secrets");
    let state = root.join("state.toml");
    let mut text = String::from("membership = \"in\"\nseq = 1\nheard = 1\n# ");
    text.push_str(&"x".repeat(SOURCE_TEXT_MAX_BYTES));
    fs::write(&state, &text).unwrap();
    let initial = vault_input(&w.home);
    let row_inputs = ladder_inputs(&w.home, &store);
    assert!(matches!(initial, VaultInput::Read(_)));
    fs::write(&state, text.replacen("heard = 1", "heard = 2", 1)).unwrap();
    assert_eq!(vault_input(&w.home), initial);
    assert_eq!(ladder_inputs(&w.home, &store), row_inputs);
    fs::write(&state, text.replacen("seq = 1", "seq = 2", 1)).unwrap();
    assert_eq!(vault_input(&w.home), initial, "a version is no context");
    assert_eq!(
        ladder_inputs(&w.home, &store),
        row_inputs,
        "nor a row input"
    );
    fs::write(
        &state,
        text.replacen("membership = \"in\"", "membership = \"revoked\"", 1),
    )
    .unwrap();
    assert_ne!(vault_input(&w.home), initial);
    assert_ne!(ladder_inputs(&w.home, &store), row_inputs);

    fs::OpenOptions::new()
        .write(true)
        .open(&state)
        .unwrap()
        .set_len(aterm_secrets::client::MAX_STATE as u64 + 1)
        .unwrap();
    assert_eq!(vault_input(&w.home), VaultInput::Unreadable);
    fs::write(&state, "membership = \"in\"\nseq = 1\n").unwrap();
    fs::OpenOptions::new()
        .write(true)
        .open(root.join("source.toml"))
        .unwrap()
        .set_len(SOURCE_TEXT_MAX_BYTES as u64 + 1)
        .unwrap();
    assert_eq!(vault_input(&w.home), VaultInput::Unreadable);
}

#[test]
fn an_unreadable_vault_source_is_not_reported_as_missing_and_can_recover() {
    use super::super::sign_in_ladder::Vault;
    let (w, row) = vault_ladder_fixture();
    let source = w.home.join(".config/aterm/secrets/source.toml");
    let saved = source.with_extension("saved");
    fs::rename(&source, &saved).unwrap();
    fs::create_dir(&source).unwrap();
    assert_eq!(vault_input(&w.home), VaultInput::Unreadable);
    let witness = Witness {
        vault_sync: |_, _scope| Vault::Unchanged(83),
        ..Witness::answering("")
    };
    assert!(matches!(
        vault_ladder_pass(&w, &witness, &row),
        LadderStep::NoRoute
    ));
    let store = accounts::Store::at(&w.state);
    let (logins, _) = store.load_logins(UPKEEP_NOW).unwrap();
    let routes = &logins
        .get("m2")
        .unwrap()
        .sign_in_attempt
        .as_ref()
        .unwrap()
        .routes;
    assert!(routes.contains("vault context unreadable"), "{routes}");
    fs::rename(&source, source.with_extension("directory")).unwrap();
    fs::rename(&saved, &source).unwrap();
    assert!(matches!(
        vault_ladder_pass(&w, &witness, &row),
        LadderStep::NoRoute
    ));
    let (logins, _) = store.load_logins(UPKEEP_NOW).unwrap();
    assert!(
        logins
            .get("m2")
            .unwrap()
            .sign_in_attempt
            .as_ref()
            .unwrap()
            .routes
            .contains("vault(seq 83, nothing new)")
    );
}

#[test]
fn a_vault_source_changed_during_refresh_defers_its_old_result() {
    use super::super::sign_in_ladder::Vault;
    fn changed(home: &Path) -> Vault {
        fs::write(
            home.join(".config/aterm/secrets/source.toml"),
            "# vault B\n",
        )
        .unwrap();
        Vault::Unchanged(1)
    }
    let (w, row) = vault_ladder_fixture();
    let mut witness = Witness {
        vault_sync: |home, _scope| changed(home),
        ..Witness::answering("")
    };
    assert!(matches!(
        vault_ladder_pass(&w, &witness, &row),
        LadderStep::Held
    ));
    assert!(
        accounts::Store::at(&w.state)
            .load_logins(UPKEEP_NOW)
            .unwrap()
            .0
            .get("m2")
            .is_none()
    );
    witness.vault_sync = |_, _scope| Vault::Unchanged(83);
    assert!(matches!(
        vault_ladder_pass(&w, &witness, &row),
        LadderStep::NoRoute
    ));
    let (logins, _) = accounts::Store::at(&w.state)
        .load_logins(UPKEEP_NOW)
        .unwrap();
    assert!(
        logins
            .get("m2")
            .unwrap()
            .sign_in_attempt
            .as_ref()
            .unwrap()
            .routes
            .contains("vault(seq 83, nothing new)")
    );
}

#[test]
fn a_vault_completion_invalidated_inline_leaves_no_unstarted_refresh() {
    use super::super::sign_in_ladder::Vault;
    let (w, row) = vault_ladder_fixture();
    let witness = Witness {
        vault_sync: |_, _scope| Vault::Unchanged(83),
        ..Witness::answering("")
    };
    let guard = std::sync::Arc::clone(&witness.guard);
    let spawn = move |work: Box<dyn FnOnce() + Send>| {
        work();
        guard.lock().unwrap().ladder.moved(Instant::now());
        Ok(())
    };
    let mut effects = witness.fx();
    effects.spawn_vault = &spawn;
    let context = LadderCtx::of(&effects, &w.roster());
    assert!(matches!(
        ladder_route(
            &w.home,
            &accounts::Store::at(&w.state),
            &row,
            UPKEEP_NOW,
            &witness.guard,
            &context
        ),
        LadderStep::Held
    ));
    assert!(matches!(
        vault_ladder_pass(&w, &witness, &row),
        LadderStep::NoRoute
    ));
}

#[test]
fn an_async_vault_change_cannot_restore_a_rows_obsolete_wait() {
    use super::super::sign_in_ladder::Vault;
    use std::sync::atomic::{AtomicUsize, Ordering};
    type Work = Box<dyn FnOnce() + Send>;
    static CALLS: AtomicUsize = AtomicUsize::new(0);
    fn synced(_: &Path) -> Vault {
        CALLS.fetch_add(1, Ordering::SeqCst);
        Vault::Unchanged(83)
    }
    for (other_row, unreadable) in [(false, false), (true, false), (false, true)] {
        CALLS.store(0, Ordering::SeqCst);
        let (w, row) = vault_ladder_fixture();
        let second = Row {
            label: "m2-2".to_owned(),
            email: Some("m2-2@example.test".to_owned()),
            dir: Some(w.home.join("second-login").to_string_lossy().into_owned()),
            ..row.clone()
        };
        let witness = Witness {
            vault_sync: |home, _scope| synced(home),
            ..Witness::answering("")
        };
        assert!(matches!(
            vault_ladder_pass(&w, &witness, &row),
            LadderStep::NoRoute
        ));
        if other_row {
            assert!(matches!(
                vault_ladder_pass(&w, &witness, &second),
                LadderStep::NoRoute
            ));
        }
        let source = w.home.join(".config/aterm/secrets/source.toml");
        let stamp = fs::metadata(&source).unwrap().modified().unwrap();
        let queue = std::sync::Arc::new(std::sync::Mutex::<Option<Work>>::new(None));
        let pending = std::sync::Arc::clone(&queue);
        let spawn = move |work: Work| {
            assert!(
                pending.lock().unwrap().replace(work).is_none(),
                "only one refresh may run"
            );
            Ok(())
        };
        let queued = |row: &Row| {
            let mut effects = witness.fx();
            effects.spawn_vault = &spawn;
            let context = LadderCtx::of(&effects, &w.roster());
            ladder_route(
                &w.home,
                &accounts::Store::at(&w.state),
                row,
                UPKEEP_NOW,
                &witness.guard,
                &context,
            )
        };
        fs::write(&source, "# vault B\n").unwrap();
        assert!(matches!(
            queued(if other_row { &second } else { &row }),
            LadderStep::Held
        ));
        let saved = source.with_extension("saved");
        if unreadable {
            fs::rename(&source, &saved).unwrap();
            fs::create_dir(&source).unwrap();
        } else {
            fs::write(&source, "# vault A\n").unwrap();
            fs::File::options()
                .write(true)
                .open(&source)
                .unwrap()
                .set_modified(stamp)
                .unwrap();
        }
        assert!(
            matches!(queued(&row), LadderStep::Held),
            "no outcome while the refresh runs"
        );
        assert_eq!(CALLS.load(Ordering::SeqCst), 1);
        let work = queue.lock().unwrap().take().unwrap();
        work();
        if unreadable {
            fs::rename(&source, source.with_extension("obstruction")).unwrap();
            fs::rename(&saved, &source).unwrap();
            fs::write(&source, "# vault A\n").unwrap();
            fs::File::options()
                .write(true)
                .open(&source)
                .unwrap()
                .set_modified(stamp)
                .unwrap();
        }
        assert!(matches!(
            vault_ladder_pass(&w, &witness, &row),
            LadderStep::NoRoute
        ));
        assert_eq!(
            CALLS.load(Ordering::SeqCst),
            3,
            "A must refresh after the old B completion"
        );
    }
}

#[test]
fn an_applied_vault_revision_converges_on_the_next_upkeep() {
    use super::super::sign_in_ladder::Vault;
    use std::sync::atomic::{AtomicUsize, Ordering};
    static CALLS: AtomicUsize = AtomicUsize::new(0);
    fn applied(home: &Path) -> Vault {
        CALLS.fetch_add(1, Ordering::SeqCst);
        fs::write(
            home.join(".config/aterm/secrets/state.toml"),
            "membership = \"in\"\nseq = 83\n",
        )
        .unwrap();
        if home.join("bring-mailbox").is_file() {
            let secrets = home.join(".secrets");
            fs::create_dir_all(&secrets).unwrap();
            fs::write(
                secrets.join(super::super::headless_oauth::SECRETS_MAILBOX_TOML_FILE),
                "http_url = \"http://127.0.0.1:9/inbox\"\n",
            )
            .unwrap();
        }
        Vault::Applied(83)
    }
    for with_mailbox in [false, true] {
        CALLS.store(0, Ordering::SeqCst);
        let (w, row) = vault_ladder_fixture();
        if with_mailbox {
            fs::write(w.home.join("bring-mailbox"), "").unwrap();
        }
        let witness = Witness {
            vault_sync: |home, _scope| applied(home),
            ..Witness::answering("")
        };
        assert!(matches!(
            vault_ladder_pass(&w, &witness, &row),
            LadderStep::Held
        ));
        assert_eq!(CALLS.load(Ordering::SeqCst), 1);
        let next = vault_ladder_pass(&w, &witness, &row);
        if with_mailbox {
            assert!(matches!(next, LadderStep::Go { .. }));
            assert_eq!(
                CALLS.load(Ordering::SeqCst),
                1,
                "the delivered route needs no second refresh"
            );
        } else {
            assert!(matches!(next, LadderStep::NoRoute));
            assert_eq!(CALLS.load(Ordering::SeqCst), 2);
            assert!(matches!(
                vault_ladder_pass(&w, &witness, &row),
                LadderStep::Held
            ));
            assert_eq!(
                CALLS.load(Ordering::SeqCst),
                2,
                "a stable standing stops the immediate retries"
            );
        }
    }
    // A member that already held an earlier version: the version the refresh applies is
    // no new context, so that one refresh answers the row.
    CALLS.store(0, Ordering::SeqCst);
    let (w, row) = vault_ladder_fixture();
    fs::write(
        w.home.join(".config/aterm/secrets/state.toml"),
        "membership = \"in\"\nseq = 82\n",
    )
    .unwrap();
    let witness = Witness {
        vault_sync: |home, _scope| applied(home),
        ..Witness::answering("")
    };
    assert!(matches!(
        vault_ladder_pass(&w, &witness, &row),
        LadderStep::NoRoute
    ));
    assert!(matches!(
        vault_ladder_pass(&w, &witness, &row),
        LadderStep::Held
    ));
    assert_eq!(CALLS.load(Ordering::SeqCst), 1, "one refresh");
}

#[test]
fn a_partial_vault_apply_backs_off_against_its_delivered_inputs() {
    use super::super::sign_in_ladder::Vault;
    use std::sync::atomic::{AtomicUsize, Ordering};
    static CALLS: AtomicUsize = AtomicUsize::new(0);
    fn partial(home: &Path) -> Vault {
        CALLS.fetch_add(1, Ordering::SeqCst);
        let secrets = home.join(".secrets");
        fs::create_dir_all(&secrets).unwrap();
        fs::write(secrets.join("delivered.txt"), "delivered").unwrap();
        if home.join("bring-mailbox").is_file() {
            fs::write(
                secrets.join(super::super::headless_oauth::SECRETS_MAILBOX_TOML_FILE),
                "http_url = \"http://127.0.0.1:9/inbox\"\n",
            )
            .unwrap();
        }
        Vault::AppliedPartly(83)
    }
    for with_mailbox in [false, true] {
        CALLS.store(0, Ordering::SeqCst);
        let (w, row) = vault_ladder_fixture();
        if with_mailbox {
            fs::write(w.home.join("bring-mailbox"), "").unwrap();
        }
        let witness = Witness {
            vault_sync: |home, _scope| partial(home),
            ..Witness::answering("")
        };
        let first = vault_ladder_pass(&w, &witness, &row);
        if with_mailbox {
            let LadderStep::Go { inputs, .. } = first else {
                panic!("the delivered mailbox must be ready");
            };
            // The sign-in fails after it uses the delivered route.
            witness
                .guard
                .lock()
                .unwrap()
                .ladder
                .failed(&row.label, &inputs, Instant::now());
        } else {
            assert!(matches!(first, LadderStep::NoRoute));
        }
        assert!(matches!(
            vault_ladder_pass(&w, &witness, &row),
            LadderStep::Held
        ));
        assert_eq!(CALLS.load(Ordering::SeqCst), 1);
    }
}

#[cfg(unix)]
#[test]
fn dangling_vault_paths_are_unreadable_not_absent() {
    use std::os::unix::fs::symlink;
    let (w, _) = vault_ladder_fixture();
    let root = w.home.join(".config/aterm/secrets");
    let source = root.join("source.toml");
    let saved = root.join("source.saved");
    fs::rename(&source, &saved).unwrap();
    symlink(root.join("missing"), &source).unwrap();
    assert_eq!(vault_input(&w.home), VaultInput::Unreadable);
    fs::rename(&source, root.join("dangling-source")).unwrap();
    fs::rename(&saved, &source).unwrap();
    symlink(root.join("missing"), root.join("state.toml")).unwrap();
    assert_eq!(vault_input(&w.home), VaultInput::Unreadable);
}

/// A due login this Mac used yesterday (m2), on a machine with no route, as m2 was on
/// 2026-10-24, while a session waits at m2-2's wall: the vault is refreshed once per wait, the
/// failure is on record, the mailbox a refresh brings signs the row in, and the session at the
/// wall moves to it.
#[cfg(unix)]
#[test]
fn a_vault_refresh_that_brings_a_mailbox_signs_in_the_row_a_walled_session_moves_to() {
    use super::super::sign_in_ladder::Vault;
    static SYNCS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    fn vault_brings_nothing(_: &Path) -> Vault {
        SYNCS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Vault::Unchanged(51)
    }
    fn vault_brings_a_mailbox(home: &Path) -> Vault {
        let secrets = home.join(".secrets");
        fs::create_dir_all(&secrets).unwrap();
        fs::write(
            secrets.join(super::super::headless_oauth::SECRETS_MAILBOX_TOML_FILE),
            "http_url = \"http://127.0.0.1:9/inbox\"\n",
        )
        .unwrap();
        // A refresh that wrote the mailbox but failed another file still brings it: the
        // row is read again after a partial apply too (`Vault::wrote_files`).
        Vault::AppliedPartly(52)
    }

    let w = World::new();
    let store = accounts::Store::at(&w.state);
    let m2_2_dir = store.logins_dir().join("m2-2");
    fs::create_dir_all(&m2_2_dir).unwrap();
    fs::write(
        m2_2_dir.join(".claude.json"),
        "{\"oauthAccount\":{\"emailAddress\":\"m2-2@example.test\"}}\n",
    )
    .unwrap();
    let mut roster_toml = String::new();
    for label in ["m2", "m2-2"] {
        let row = Row {
            label: label.to_owned(),
            class: Class::Preferred,
            dir: Some(
                store
                    .logins_dir()
                    .join(label)
                    .to_string_lossy()
                    .into_owned(),
            ),
            email: Some(format!("{label}@example.test")),
            ..Row::default()
        };
        roster_toml.push_str(&accounts::row_text(&row).unwrap());
        roster_toml.push('\n');
    }
    fs::create_dir_all(&w.config).unwrap();
    fs::write(w.roster(), &roster_toml).unwrap();
    let m2_dir = store.logins_dir().join("m2");
    fs::create_dir_all(&m2_dir).unwrap();
    fs::write(m2_dir.join(".claude.json"), "{}").unwrap();
    let wall_at = UPKEEP_NOW - 60;
    store
        .transact_logins(UPKEEP_NOW, &mut None, |st, logins| {
            st.active = Some("m2-2".to_owned());
            // A session here ran on m2 yesterday; its login here has ended since.
            let mut events = accounts::note_session(st, "s-0", "m2", None, UPKEEP_NOW - 86_400);
            st.sessions.remove("s-0");
            events.extend(accounts::note_login_end(st, logins, "m2", UPKEEP_NOW - 120));
            events.extend(accounts::note_wall(
                st,
                "m2-2",
                "seven_day",
                Some(UPKEEP_NOW + 86_400),
                wall_at,
                UPKEEP_NOW,
            ));
            Ok(events)
        })
        .unwrap();
    let secrets_config = w.home.join(".config/aterm/secrets");
    fs::create_dir_all(&secrets_config).unwrap();
    fs::write(secrets_config.join("source.toml"), "# a vault member\n").unwrap();

    let wit = Witness {
        vault_sync: |home, _scope| vault_brings_nothing(home),
        ..Witness::answering("")
    };
    let pass = |wit: &Witness| {
        auto_renew_next_account(&w.home, &w.state, &w.roster(), UPKEEP_NOW, &wit.fx())
    };
    assert!(!pass(&wit), "no route: nothing started");
    assert_eq!(
        (
            wit.spawned(),
            SYNCS.load(std::sync::atomic::Ordering::SeqCst)
        ),
        (0, 1)
    );
    let tried = store
        .load_logins(UPKEEP_NOW)
        .unwrap()
        .0
        .get("m2")
        .and_then(|b| b.sign_in_attempt.clone())
        .expect("the attempt is on record");
    assert_eq!(tried.outcome, accounts::AttemptOutcome::NoRoute);
    assert!(
        tried.routes.contains("vault(seq 51, nothing new)"),
        "{}",
        tried.routes
    );
    assert!(!pass(&wit));
    assert_eq!(
        SYNCS.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "inside its backoff the row is not read again, and the vault not refreshed again"
    );

    let wit = Witness {
        vault_sync: |home, _scope| vault_brings_a_mailbox(home),
        ..Witness::answering("")
    };
    assert!(pass(&wit), "the mailbox the vault brought is a route");
    assert_eq!(wit.spawned(), 1);
    assert!(wit.guard.lock().unwrap().last_attempt.contains_key("m2"));

    // The sign-in that started ends with m2's login made. A session at m2-2's wall, which
    // had nowhere to go, now moves to m2; the wake file a renewal touches is what makes
    // each waiting session's supervisor re-advise at once (`account_news`, turn_end_loop).
    let file_roster = Roster::load(&w.roster()).unwrap();
    let m2_2_text = m2_2_dir.to_string_lossy().into_owned();
    let advise_at = |at: i64| {
        let (state, _) = store.load(at).unwrap();
        let (logins, _) = store.load_logins(at).unwrap();
        let bound = accounts::bound(&file_roster, &logins);
        let enrolled = accounts::enrolled(&bound);
        let who = accounts::ProcessLogin {
            login: Some(m2_2_text.clone()),
            default_home: true,
            route: crate::harness::accounts::AuthRoute::Stored,
        };
        accounts::advise(
            &bound,
            &state,
            &enrolled,
            &who.me(&bound),
            &accounts::Point::Wall {
                window: "seven_day".to_owned(),
                at: Some(wall_at),
            },
            accounts::Mode::Wall,
            at,
        )
    };
    assert!(
        !matches!(advise_at(UPKEEP_NOW), accounts::Advice::Move { .. }),
        "{:?}",
        advise_at(UPKEEP_NOW)
    );
    let _ = fs::remove_file(store.wake_path());
    let staging = store.claim_staging_dir("m2").unwrap();
    let staging_text = staging.to_str().unwrap().to_owned();
    fs::write(
        staging.join(".claude.json"),
        "{\"oauthAccount\":{\"emailAddress\":\"m2@example.test\",\"subscriptionType\":\"max\"}}\n",
    )
    .unwrap();
    let status = super::super::align::Captured {
        stdout: b"{\"loggedIn\":true,\"authMethod\":\"claude.ai\",\"email\":\"m2@example.test\",\"subscriptionType\":\"max\"}\n".to_vec(),
        code: Some(0),
    };
    assert!(complete_staging_renewal(
        &store,
        &file_roster,
        "m2",
        &staging,
        &staging_text,
        &status,
        UPKEEP_NOW + 5,
    ));
    assert!(store.wake_path().exists(), "the waiting sessions are woken");
    assert_eq!(
        advise_at(UPKEEP_NOW + 5),
        accounts::Advice::Move {
            to: "m2".to_owned(),
            ahead: false,
            rotates: true,
            takes_canary: false,
            yields: None,
        }
    );
    let (logins, _) = store.load_logins(UPKEEP_NOW + 5).unwrap();
    assert_eq!(
        logins.get("m2").and_then(|b| b.sign_in_attempt.clone()),
        None,
        "the login made ends the failure on record"
    );
}

/// A row with no mailbox, on a vault member whose refresh gave none, asks the lab for the
/// row's mailbox password through the need ledger at its no-route outcome; the refresh that
/// brings the mailbox meets the need, so its ask comes down: a mailbox read over HTTP, and a
/// webmail-only one with a password, whether or not a headless browser is here to read it,
/// the row's own `m29_password` (the lab's answer) included. A webmail file with no password
/// is tried beside a browser but leaves the ask standing. NEGATIVE CONTROL on main: the
/// webmail cases with a password keep the ask (m2's, measured 2026-11-02: `NEEDS YOU: m2-3's
/// mailbox password` stood beside the webmail mailbox its every sign-in used).
#[cfg(unix)]
#[test]
fn a_row_with_no_mailbox_asks_the_lab_for_it_and_its_arrival_meets_the_need() {
    use super::super::sign_in_ladder::Vault;
    fn brings(home: &Path, mailbox: &str) -> Vault {
        let secrets = home.join(".secrets");
        fs::create_dir_all(&secrets).unwrap();
        fs::write(
            secrets.join(super::super::headless_oauth::SECRETS_MAILBOX_TOML_FILE),
            mailbox,
        )
        .unwrap();
        Vault::Applied(4)
    }
    fn brings_an_http_mailbox(
        home: &Path,
        _scope: Option<&crate::harness::account_work::Scope>,
    ) -> Vault {
        brings(home, "http_url = \"http://127.0.0.1:9/inbox\"\n")
    }
    fn brings_a_webmail_mailbox(
        home: &Path,
        _scope: Option<&crate::harness::account_work::Scope>,
    ) -> Vault {
        brings(
            home,
            "webmail_url = \"https://mail.example.test/inbox\"\n\
             user = \"m29@example.test\"\npassword = \"p\"\n",
        )
    }
    fn brings_a_webmail_mailbox_with_no_password(
        home: &Path,
        _scope: Option<&crate::harness::account_work::Scope>,
    ) -> Vault {
        brings(
            home,
            "webmail_url = \"https://mail.example.test/inbox\"\nuser = \"m29@example.test\"\n",
        )
    }
    /// The lab's own answer to `mailbox:m29`: the `m29_password` entry, beside a webmail
    /// file that names no password.
    fn brings_the_rows_password_beside_webmail(
        home: &Path,
        _scope: Option<&crate::harness::account_work::Scope>,
    ) -> Vault {
        brings_a_webmail_mailbox_with_no_password(home, None);
        fs::write(home.join(".secrets/m29_password"), "p\n").unwrap();
        Vault::Applied(4)
    }
    for (brought, browser, routed, meets) in [
        (
            brings_an_http_mailbox
                as fn(&Path, Option<&crate::harness::account_work::Scope>) -> Vault,
            false,
            true,
            true,
        ),
        (brings_a_webmail_mailbox, true, true, true),
        (brings_a_webmail_mailbox, false, false, true),
        (brings_a_webmail_mailbox_with_no_password, true, true, false),
        (brings_the_rows_password_beside_webmail, true, true, true),
    ] {
        its_arrival_meets_the_need(brought, browser, routed, meets);
    }
}

/// [`a_row_with_no_mailbox_asks_the_lab_for_it_and_its_arrival_meets_the_need`] with the
/// mailbox the vault `brought`, beside a headless browser or none, `routed` saying whether
/// that is a route and `meets` whether it ends the ask.
#[cfg(unix)]
fn its_arrival_meets_the_need(
    brought: fn(
        &Path,
        Option<&crate::harness::account_work::Scope>,
    ) -> super::super::sign_in_ladder::Vault,
    browser: bool,
    routed: bool,
    meets: bool,
) {
    use super::super::sign_in_ladder::Vault;
    use aterm_secrets::need::{self, Purpose, Want};
    fn nothing_new(_: &Path) -> Vault {
        Vault::Unchanged(3)
    }
    let w = World::new();
    let store = accounts::Store::at(&w.state);
    let row = Row {
        label: "m29".to_owned(),
        class: Class::Preferred,
        dir: Some(
            store
                .logins_dir()
                .join("m29")
                .to_string_lossy()
                .into_owned(),
        ),
        email: Some("m29@example.test".to_owned()),
        ..Row::default()
    };
    fs::create_dir_all(&w.config).unwrap();
    fs::write(
        w.roster(),
        format!("{}\n", accounts::row_text(&row).unwrap()),
    )
    .unwrap();
    // m29 is a due login this Mac uses: a session here ran on it yesterday, and its login
    // here has ended since (`DESIGN-alab-one-mesh-2026-11-05.md` §4.15 rule 4).
    let made = store.logins_dir().join("m29");
    fs::create_dir_all(&made).unwrap();
    fs::write(made.join(".claude.json"), "{}").unwrap();
    store
        .transact_logins(UPKEEP_NOW - 120, &mut None, |state, logins| {
            let mut events = accounts::note_session(state, "s-0", "m29", None, UPKEEP_NOW - 86_400);
            state.sessions.remove("s-0");
            events.extend(accounts::note_login_end(
                state,
                logins,
                "m29",
                UPKEEP_NOW - 120,
            ));
            Ok(events)
        })
        .unwrap();
    let paths = aterm_secrets::client::Paths::for_home(&w.home);
    let source = w.home.join("lab.vault");
    aterm_secrets::client::test_create_unbound(&paths, source.to_str().unwrap(), None, Some("m1"))
        .unwrap();
    need::test_alab_policy(&paths);
    let mailbox = Want::parse("mailbox:m29").unwrap();
    let pass = |wit: &Witness| {
        auto_renew_next_account(&w.home, &w.state, &w.roster(), UPKEEP_NOW, &wit.fx())
    };

    let wit = Witness {
        vault_sync: |home, _scope| nothing_new(home),
        ..Witness::answering("")
    };
    assert!(!pass(&wit), "no route");
    let needs = need::peek(&paths).unwrap();
    let asked = needs
        .iter()
        .find(|need| need.want == mailbox)
        .expect("the no-route outcome raised the mailbox need");
    assert_eq!(asked.purpose, Purpose::HarnessSignIn);
    assert!(!asked.state.ended(), "{}", asked.line);
    assert!(
        wit.logged()
            .iter()
            .any(|line| line.contains("the lab is asked for its mailbox password")),
        "{:?}",
        wit.logged()
    );

    let wit = Witness {
        vault_sync: brought,
        browser: Box::new(move |_: &Path| browser),
        ..Witness::answering("")
    };
    assert_eq!(
        pass(&wit),
        routed,
        "the mailbox the vault brought is a route only where it can be read (browser {browser})"
    );
    let met = need::peek(&paths).unwrap();
    let met = met.iter().find(|need| need.want == mailbox).unwrap();
    assert_eq!(
        met.state.ended(),
        meets,
        "the mailbox's arrival ends the need only where it holds the password \
         (browser {browser}, routed {routed}): {}",
        met.line
    );
    if meets {
        assert!(need::exceptions(&need::peek(&paths).unwrap()).is_empty());
    }
}
#[test]
fn app_scope_cancellation_during_fetch_does_not_publish_the_fetched_roster() {
    for cancel_during_fetch in [false, true] {
        let w = World::new();
        w.write_config(&format!(
            "[fleet]\naccounts_source = \"{LOOPBACK_SOURCE}\"\nmachine = \"m1\"\n"
        ));
        let wit = Witness::answering(ONE_ACCOUNT_CSV);
        let prior_failure = cancel_during_fetch.then(|| FailedSync {
            inputs: [99; 32],
            at: Instant::now(),
            why: "an earlier independent source failure".to_owned(),
            holds: true,
        });
        wit.rediscovery.lock().unwrap().failed = prior_failure.clone();
        let lifetime = crate::harness::account_work::Lifetime::default();
        let scope = lifetime.enter().unwrap();
        let paused = std::sync::Mutex::new(None);
        let fetch = |env: &Env, source: &str| {
            let result = (wit.fetch)(env, source);
            if cancel_during_fetch {
                *paused.lock().unwrap() = lifetime.pause();
            }
            result
        };
        let mut fx = wit.fx();
        fx.fetch = &fetch;
        with_scope_effects(scope, fx, |fx| {
            upkeep(
                &w.home,
                &w.state,
                (&w.roster(), UPKEEP_NOW, UpkeepStep::Sync),
                fx,
            )
        });
        assert_eq!(wit.fetched(), vec![LOOPBACK_SOURCE.to_owned()]);
        assert_eq!(w.roster().exists(), !cancel_during_fetch);
        assert_eq!(wit.spawned(), 0);
        assert_eq!(wit.rediscovery.lock().unwrap().failed, prior_failure);
        if let Some(pause) = paused.into_inner().unwrap() {
            assert!(
                pause
                    .try_wait()
                    .expect("all actual work has retired")
                    .resume()
                    .is_ok()
            );
            let scope = lifetime.enter().unwrap();
            with_scope_effects(scope, wit.fx(), |fx| {
                upkeep(
                    &w.home,
                    &w.state,
                    (&w.roster(), UPKEEP_NOW, UpkeepStep::Sync),
                    fx,
                )
            });
            assert!(
                w.roster().exists(),
                "resumed work retries the same inputs immediately"
            );
            assert_eq!(
                wit.fetched(),
                vec![LOOPBACK_SOURCE.to_owned(), LOOPBACK_SOURCE.to_owned()]
            );
            // This missing-roster path publishes through sync_if_missing, not the
            // periodic failure ledger. Cancellation must preserve unrelated history.
            assert_eq!(wit.rediscovery.lock().unwrap().failed, prior_failure);
        }
    }
}
