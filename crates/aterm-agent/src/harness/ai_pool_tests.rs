// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::harness::accounts;
#[cfg(unix)]
use crate::harness::accounts_sync;
use std::collections::BTreeSet;
use std::fs;

const NOW: i64 = 1_762_000_000;

#[cfg(unix)]
pub(in crate::harness) fn seed_local_swarm_roster(home: &Path, label: &str, aliases: &[&str]) {
    seed_local_swarm_roster_denying(home, label, aliases, &[]);
}

/// [`seed_local_swarm_roster`] whose roster denies `deny_labels`.
#[cfg(unix)]
pub(in crate::harness) fn seed_local_swarm_roster_denying(
    home: &Path,
    label: &str,
    aliases: &[&str],
    deny_labels: &[&str],
) {
    let srv_dir = aterm_tempfile::tempdir().expect("server dir");
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
    let paths = aterm_secrets::client::Paths::for_home(home);
    aterm_secrets::client::create(&paths, &url, None, Some(label)).expect("create vault");
    let recipient = aterm_secrets::client::machine_identity(&paths)
        .expect("identity")
        .recipient()
        .to_bech32();
    let adm = aterm_secrets::client::admin_open(&paths).expect("admin open");
    let mut doc = adm.doc.clone();
    let roster = aterm_secrets::swarm_roster::SwarmRoster {
        members: vec![aterm_secrets::swarm_roster::SwarmMember {
            recipient,
            vault_name: label.to_owned(),
            label: label.to_owned(),
            aliases: aliases.iter().map(|s| (*s).to_owned()).collect(),
            hint: None,
            hw_hint: None,
            admitted_ms: 1,
            claimed_label: None,
            claimed_at_ms: None,
            node_id: None,
            node_key: None,
        }],
        deny_labels: deny_labels.iter().map(|s| (*s).to_owned()).collect(),
        deny_nodes: Vec::new(),
    };
    doc.set_entry(
        &aterm_secrets::text::Audience::Everyone,
        aterm_secrets::swarm_roster::ROSTER_ENTRY,
        roster.to_toml().as_bytes(),
    )
    .expect("set roster");
    aterm_secrets::client::admin_publish(&paths, &adm, &doc).expect("publish roster");
    aterm_secrets::client::sync(&paths, None).expect("sync local copy");
}

#[test]
fn pool_toml_round_trips_and_refuses_invalid_rows() {
    let raw = r#"
version = 1

[last_heard]
m3 = "2026-10-27"
m14 = "2026-10-27"

[[account]]
provider = "claude"
email = "a01@example.test"
warm = ["m3", "m14"]
since = ["2026-10-20T00:00:00Z", "2026-10-26T04:12:00Z"]
why = ["pin", "fill"]
used = ["2026-10-27", "2026-10-27"]

[[account]]
provider = "claude"
email = "a19@example.test"
warm = []

[[account]]
provider = "codex"
flow = "device"
email = "c01@example.test"
warm = ["m1", "m7", "m12"]

[[account]]
provider = "kimi:global"
email = "k01@example.test"
warm = ["m3"]

[[account]]
provider = "kimi:mainland-cn"
email = "k01@example.test"
warm = ["m3"]
"#;
    let parsed = Pool::from_toml(raw, "ai-pool.toml").expect("valid pool parses");
    assert_eq!(parsed.accounts.len(), 5);
    assert_eq!(parsed.accounts[0].home_mac(), Some("m3"));
    assert_eq!(parsed.accounts[0].class(), Class::Pool);
    assert_eq!(parsed.accounts[1].holder_word(), POOL_HOLDER_UNASSIGNED);
    assert_eq!(parsed.accounts[3].class(), Class::Preferred);

    let rendered = parsed.to_toml();
    let reparsed = Pool::from_toml(&rendered, "round-trip").expect("rendered pool parses");
    assert_eq!(reparsed.accounts, parsed.accounts);
    assert_eq!(reparsed.last_heard, parsed.last_heard);

    // Negative controls: bad version, >3 warm slots, duplicate warm label, non-m<N> warm
    // label ("Mac-mini-M4" is never "m4"), parallel array mismatch, duplicate realm+email.
    for bad in [
        "version = 2\n",
        "pool = 1\n",
        "version = 1\n[[account]]\nemail = \"a@example.test\"\nwarm = [\"m1\", \"m2\", \"m3\", \"m4\"]\n",
        "version = 1\n[[account]]\nemail = \"a@example.test\"\nwarm = [\"m3\", \"m3\"]\n",
        "version = 1\n[[account]]\nemail = \"a@example.test\"\nwarm = [\"Mac-mini-M4\"]\n",
        "version = 1\n[[account]]\nemail = \"a@example.test\"\nwarm = [\"m3\"]\nsince = [\"t1\", \"t2\"]\n",
        "version = 1\n[[account]]\nprovider = \"claude\"\nemail = \"a@example.test\"\n[[account]]\nprovider = \"claude\"\nemail = \"A@example.test\"\n",
        "version = 1\n[[account]]\nprovider = \"kimi\"\nemail = \"k@example.test\"\n",
    ] {
        assert!(
            Pool::from_toml(bad, "bad.toml").is_err(),
            "expected refusal for:\n{bad}"
        );
    }
}

/// AN ACCOUNT'S POOL ROW IS FOUND BY ITS CLI AND ITS ADDRESS. The address matches whatever its
/// case; another CLI's account of the same address is another row; an address the file does not
/// list has none.
#[test]
fn home_of_email_finds_the_clis_row_by_address() {
    let raw = r#"
version = 1

[[account]]
provider = "claude"
email = "a01@example.test"
warm = ["m3", "m14"]
since = ["2026-10-20T00:00:00Z", "2026-10-26T04:12:00Z"]
why = ["pin", "fill"]
used = ["2026-10-27", "2026-10-27"]

[[account]]
provider = "codex"
flow = "device"
email = "a01@example.test"
warm = ["m7"]
"#;
    let pool = Pool::from_toml(raw, "ai-pool.toml").expect("valid pool parses");
    let claude = home_of_email(&pool, Cli::Claude, "A01@Example.test").expect("the Claude row");
    assert_eq!(claude.home_mac(), Some("m3"));
    assert_eq!(claude.why, [SlotWhy::Pin, SlotWhy::Fill]);
    let codex = home_of_email(&pool, Cli::Codex, "a01@example.test").expect("the Codex row");
    assert_eq!(codex.home_mac(), Some("m7"));
    assert!(home_of_email(&pool, Cli::Claude, "a02@example.test").is_none());
    assert!(home_of_email(&pool, Cli::Grok, "a01@example.test").is_none());
}

#[cfg(unix)]
#[test]
fn local_pool_identity_reads_swarm_roster_aliases_and_rejects_mac_mini_m4() {
    assert_eq!(leading_m_label("Mac-mini-M4"), None);
    assert_eq!(leading_m_label("m4-air.local"), Some((4, "m4".to_owned())));

    let tmp = aterm_tempfile::tempdir().expect("tempdir");
    let home = tmp.path();
    seed_local_swarm_roster(home, "m100", &["m32"]);
    let config = home.join(".config/aterm/aterm.toml");
    fs::create_dir_all(config.parent().unwrap()).unwrap();
    crate::test_config::write(&config, "[fleet]\nmachine = \"m999\"\n").unwrap();

    let paths = aterm_secrets::client::Paths::for_home(home);
    let writer = std::rc::Rc::new(std::cell::RefCell::new(Some(
        aterm_secrets::client::test_hold_state_lock(&paths).unwrap(),
    )));
    let pass = || resolve_local_pool_identity(Some(home), Some(&config), None).unwrap();
    assert!(
        read_pass_roster_until(&paths, std::time::Instant::now())
            .unwrap_err()
            .contains("roster busy")
    );
    assert_ne!(
        test_without_roster_wait(pass).map(|id| id.primary),
        Some("m100".to_owned()),
        "negative control loses the assigned label under the writer",
    );
    let release = writer.clone();
    let id = aterm_secrets::client::test_with_denied_nodes_busy(
        move || drop(release.borrow_mut().take()),
        pass,
    )
    .expect("the pass reads the roster after the writer");
    assert_eq!(id.primary, "m100");
    assert_eq!(id.aliases, ["m32"]);
    assert!(writer.borrow().is_none(), "the pass met the writer");

    let id = resolve_local_pool_identity(Some(home), Some(&config), None)
        .unwrap()
        .expect("swarm-roster resolves identity");
    assert_eq!(id.primary, "m100");
    assert_eq!(id.aliases, vec!["m32".to_owned()]);
    assert!(id.matches("m100"));
    assert!(id.matches("m32"));
    assert!(!id.matches("m999"));
    assert!(!id.matches("m4"));

    // When `m100` is denied in `aterm-swarm-roster` (`deny_labels = ["m100"]`), neither its
    // recipient row, `[fleet] machine = "m100"` in `aterm.toml`, nor `resolve_local_swarm_label`
    // may grant `m100` a pool identity.
    let denied_home = tmp.path().join("denied-home");
    let denied_vault = tmp.path().join("denied.vault");
    let denied_paths = aterm_secrets::client::Paths::for_home(&denied_home);
    aterm_secrets::client::create(
        &denied_paths,
        denied_vault.to_str().unwrap(),
        None,
        Some("m100"),
    )
    .expect("create denied vault");
    let denied_rec = aterm_secrets::client::machine_identity(&denied_paths)
        .expect("identity")
        .recipient()
        .to_bech32();
    let adm = aterm_secrets::client::admin_open(&denied_paths).expect("admin open");
    let mut doc = adm.doc.clone();
    let mut denied_roster = aterm_secrets::swarm_roster::SwarmRoster {
        members: vec![aterm_secrets::swarm_roster::SwarmMember {
            recipient: denied_rec,
            vault_name: "m100".to_owned(),
            label: "m100".to_owned(),
            aliases: vec!["m32".to_owned()],
            hint: None,
            hw_hint: None,
            admitted_ms: 1,
            claimed_label: None,
            claimed_at_ms: None,
            node_id: None,
            node_key: None,
        }],
        deny_labels: vec!["m100".to_owned()],
        deny_nodes: Vec::new(),
    };
    doc.set_entry(
        &aterm_secrets::text::Audience::Everyone,
        aterm_secrets::swarm_roster::ROSTER_ENTRY,
        denied_roster.to_toml().as_bytes(),
    )
    .unwrap();
    aterm_secrets::client::admin_publish(&denied_paths, &adm, &doc).unwrap();
    aterm_secrets::client::sync(&denied_paths, None).unwrap();
    let denied_cfg = denied_home.join(".config/aterm/aterm.toml");
    fs::create_dir_all(denied_cfg.parent().unwrap()).unwrap();
    crate::test_config::write(&denied_cfg, "[fleet]\nmachine = \"m100\"\n").unwrap();
    assert_eq!(
        aterm_secrets::client::resolve_local_swarm_label(&denied_paths, &["m100".to_owned()])
            .unwrap(),
        None
    );
    assert_eq!(
        resolve_local_pool_identity(Some(&denied_home), Some(&denied_cfg), None).unwrap(),
        None
    );

    // Removing `m100` from `members` while keeping `deny_labels = ["m100"]` also refuses
    // fallback to `aterm.toml` (`machine = "m100"`) or `source.toml` (`host = "m100"`).
    denied_roster.members.clear();
    let adm2 = aterm_secrets::client::admin_open(&denied_paths).expect("admin open 2");
    let mut doc2 = adm2.doc.clone();
    doc2.set_entry(
        &aterm_secrets::text::Audience::Everyone,
        aterm_secrets::swarm_roster::ROSTER_ENTRY,
        denied_roster.to_toml().as_bytes(),
    )
    .unwrap();
    aterm_secrets::client::admin_publish(&denied_paths, &adm2, &doc2).unwrap();
    aterm_secrets::client::sync(&denied_paths, None).unwrap();
    assert_eq!(
        aterm_secrets::client::resolve_local_swarm_label(&denied_paths, &["m100".to_owned()])
            .unwrap(),
        None
    );
    assert_eq!(
        resolve_local_pool_identity(Some(&denied_home), Some(&denied_cfg), None).unwrap(),
        None
    );
    // Even if `aterm.toml` claims a different label (`m200`), a machine whose vault
    // `source.toml` host (`m100`) is in `deny_labels` has `recipient_denied = true` and
    // cannot fall back to `aterm.toml`.
    crate::test_config::write(&denied_cfg, "[fleet]\nmachine = \"m200\"\n").unwrap();
    assert_eq!(
        resolve_local_pool_identity(Some(&denied_home), Some(&denied_cfg), None).unwrap(),
        None
    );
}

#[test]
fn with_no_roster_this_mac_is_its_first_host_name_that_starts_with_a_label() {
    let names = |names: &[&str]| names.iter().map(|n| (*n).to_owned()).collect::<Vec<_>>();
    let id =
        resolve_pool_identity_pure(&[], None, None, None, &names(&["Mac-mini-M4", "m7.local"]))
            .expect("m7.local names this Mac");
    assert_eq!((id.primary.as_str(), id.from_roster), ("m7", false));
    assert_eq!(
        resolve_pool_identity_pure(&[], None, None, None, &names(&["Mac-mini-M4"])),
        None
    );
    let roster = [("age1m100".to_owned(), "m100".to_owned(), Vec::new())];
    assert_eq!(
        resolve_pool_identity_pure(&roster, None, None, None, &names(&["m7.local"])),
        None,
        "a roster that gives this Mac no row leaves its host names unread"
    );
}

#[test]
fn an_account_leased_elsewhere_is_never_picked_and_turn_end_yields() {
    let roster = accounts::Roster {
        rows: vec![
            accounts::Row {
                label: "pool-a".into(),
                class: Class::Pool,
                dir: Some("/tmp/pool-a".into()),
                email: Some("a@example.test".into()),
                ..accounts::Row::default()
            },
            accounts::Row {
                label: "pool-b".into(),
                class: Class::Pool,
                dir: Some("/tmp/pool-b".into()),
                email: Some("b@example.test".into()),
                ..accounts::Row::default()
            },
            accounts::Row {
                label: "cold-c".into(),
                class: Class::Pool,
                dir: Some("/tmp/cold-c".into()),
                email: Some("c@example.test".into()),
                held_by: Some("m9".into()),
                ..accounts::Row::default()
            },
        ],
    };
    let enrolled: BTreeSet<String> = ["pool-a", "pool-b", "cold-c"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    let mut state = accounts::State {
        swarm_label: Some("m100".into()),
        swarm_aliases: vec!["m32".into()],
        home_accounts: ["pool-a".to_owned()].into_iter().collect(),
        ..accounts::State::default()
    };
    state.note_lease("pool-a", "m9", NOW + 600, NOW);

    // Positive check: leased elsewhere (`m9`) and held elsewhere (`cold-c`) are never picked.
    let cands = accounts::candidates(
        &roster,
        &state,
        &enrolled,
        None,
        &accounts::Point::TurnEnd,
        NOW,
    );
    let verdict_a = cands.iter().find(|(l, _)| l == "pool-a").unwrap().1;
    let verdict_b = cands.iter().find(|(l, _)| l == "pool-b").unwrap().1;
    let verdict_c = cands.iter().find(|(l, _)| l == "cold-c").unwrap().1;
    assert_eq!(
        verdict_a,
        Err(accounts::WhyNot::LeasedElsewhere { until: NOW + 600 })
    );
    assert!(verdict_b.is_ok(), "{verdict_b:?}");
    assert_eq!(verdict_c, Err(accounts::WhyNot::HeldElsewhere));

    let picked = accounts::launch_row(&roster, &state, &enrolled, NOW).unwrap();
    assert_eq!(picked.label, "pool-b");

    // Running session on `pool-a` yields at safe boundary `TurnEnd` under `Mode::Early` (and
    // stays when `Mode::Off` disables rotation).
    let me = accounts::Me {
        account: Some("pool-a"),
        left: None,
        default_home: true,
        route: crate::harness::accounts::AuthRoute::Stored,
    };
    assert_eq!(
        accounts::advise(
            &roster,
            &state,
            &enrolled,
            &me,
            &accounts::Point::TurnEnd,
            accounts::Mode::Off,
            NOW,
        ),
        accounts::Advice::Stay(accounts::StayWhy::Off)
    );
    assert_eq!(
        accounts::advise(
            &roster,
            &state,
            &enrolled,
            &me,
            &accounts::Point::TurnEnd,
            accounts::Mode::Early,
            NOW,
        ),
        accounts::Advice::Move {
            to: "pool-b".into(),
            ahead: true,
            rotates: true,
            takes_canary: false,
            yields: Some(NOW + crate::harness::claims::RECHECK_S),
        }
    );

    // State JSON round-trip preserves leases, swarm_label, swarm_aliases, home_accounts,
    // unrecorded.
    state.unrecorded = true;
    let round = accounts::State::from_json(&state.to_json(), NOW).expect("state json round-trips");
    assert_eq!(round, state);

    // Negative control ("ignore the lease"): once the lease expires (or belongs to our alias
    // `m32`), `pool-a` becomes selectable again; while claims are unrecorded, `pool-a` (our
    // home account) is selectable while `pool-b` (not home, unclaimed) is refused with
    // `Unclaimed`.
    let after_expiry = NOW + 601;
    let picked_home = accounts::launch_row(&roster, &state, &enrolled, after_expiry).unwrap();
    assert_eq!(picked_home.label, "pool-a");
    let cands_down = accounts::candidates(
        &roster,
        &state,
        &enrolled,
        None,
        &accounts::Point::TurnEnd,
        after_expiry,
    );
    assert!(
        cands_down
            .iter()
            .find(|(l, _)| l == "pool-a")
            .unwrap()
            .1
            .is_ok()
    );
    assert_eq!(
        cands_down.iter().find(|(l, _)| l == "pool-b").unwrap().1,
        Err(accounts::WhyNot::Unclaimed)
    );

    // Alias ownership: a claim held by our alias `m32` is recognized as ours.
    state.unrecorded = false;
    state.note_lease("pool-a", "m32", after_expiry + 600, after_expiry);
    assert_eq!(state.lease_verdict("pool-a", after_expiry), Ok(()));
}

#[cfg(unix)]
#[test]
fn sync_from_ai_pool_outranks_sheet_tracks_aliases_and_marks_held_by() {
    let tmp = aterm_tempfile::tempdir().expect("tempdir");
    let home = tmp.path().join("home");
    let state_dir = tmp.path().join("state");
    let config_dir = home.join(".config/aterm");
    let secrets_dir = home.join(".secrets");
    fs::create_dir_all(&config_dir).unwrap();
    seed_local_swarm_roster(&home, "m100", &["m32"]);
    aterm_types::fs_restricted::ensure_private_dir(&secrets_dir).unwrap();
    fs::create_dir_all(&state_dir).unwrap();
    aterm_uds::atomic_file::write(&config_dir.join("aterm.toml"), b"", 0o600).unwrap();

    let roster_path = config_dir.join("accounts.toml");
    let codex_roster_path = config_dir.join("accounts-codex.toml");

    // Negative control before `ai-pool.toml` exists: 0.144 `ai-accounts.csv` still syncs.
    fs::write(
        secrets_dir.join(accounts_sync::ACCOUNTS_CSV_FILE),
        "email,computer,claude,codex\ncsv-only@example.test,m100,1,1\n",
    )
    .unwrap();
    crate::test_config::write(
        config_dir.join("aterm.toml"),
        "[fleet]\nmachine = \"m100\"\n",
    )
    .unwrap();
    let (m_csv, rows_csv, _, _) =
        accounts_sync::sync_accounts_for_init(&home, &state_dir, &roster_path, NOW)
            .expect("csv fallback syncs");
    assert_eq!((m_csv.as_str(), rows_csv), ("m100", 1));
    assert_eq!(
        accounts::Roster::load(&roster_path).unwrap().rows[0]
            .email
            .as_deref(),
        Some("csv-only@example.test")
    );

    // Now deliver `~/.secrets/ai-pool.toml`: it outranks `ai-accounts.csv`, resolves `m100`
    // + alias `m32` from `swarm-roster.toml`, warms both `a01` (home `m100`) and `a02` (via
    // alias `m32`, home `m9`), retains `csv-only` as `held_by = "pool"`, and syncs Codex too.
    crate::test_config::write(config_dir.join("aterm.toml"), "").unwrap();
    let pool_v1 = r#"
version = 1

[last_heard]
m100 = "2026-10-27"

[[account]]
provider = "claude"
email = "a01@example.test"
warm = ["m100", "m14"]

[[account]]
provider = "claude"
email = "a02@example.test"
warm = ["m9", "m32"]

[[account]]
provider = "claude"
email = "a03@example.test"
warm = ["m9"]

[[account]]
provider = "codex"
email = "c01@example.test"
warm = ["m32"]

[[account]]
provider = "codex"
email = "c02@example.test"
warm = ["m9"]
"#;
    fs::write(secrets_dir.join(AI_POOL_FILE), pool_v1).unwrap();
    assert!(accounts_sync::needs_init_accounts_sync(
        &home,
        &state_dir,
        &roster_path,
        NOW + 10
    ));
    assert_eq!(
        accounts_sync::cli_pools_due(&config_dir.join("aterm.toml"), &state_dir, &home, NOW + 10),
        vec![Cli::Codex]
    );
    assert!(
        matches!(accounts_sync::read_resync(&home, &state_dir, &roster_path, NOW + 10), accounts_sync::ResyncRead::Due(why) if why.contains("codex")),
        "the combined probe resolves swarm-roster alias m32 -> m100"
    );

    let resync1 = accounts_sync::resync_for_setup(&home, &state_dir, &roster_path, NOW + 10, false);
    assert_eq!(resync1.failed, None);
    assert!(resync1.cli_failed.is_empty(), "{:?}", resync1.cli_failed);
    assert!(resync1.synced);
    assert!(resync1.changed);
    // Only warm unenrolled rows (`a01` and `a02`) are queued for sign-in, never `csv-only` or `a03`.
    assert_eq!(resync1.to_sign_in.len(), 2);

    // Immediately converged: init needs no sync and the combined probe reads Current.
    assert!(!accounts_sync::needs_init_accounts_sync(
        &home,
        &state_dir,
        &roster_path,
        NOW + 11
    ));
    assert_eq!(
        accounts_sync::read_resync(&home, &state_dir, &roster_path, NOW + 11),
        accounts_sync::ResyncRead::Current
    );

    let claude_roster = accounts::Roster::load(&roster_path).unwrap();
    let held_csv = claude_roster
        .rows
        .iter()
        .find(|r| r.email.as_deref() == Some("csv-only@example.test"))
        .expect("previously enrolled row retained");
    assert_eq!(held_csv.held_by.as_deref(), Some("pool"));
    assert!(
        claude_roster
            .rows
            .iter()
            .any(|r| r.email.as_deref() == Some("a01@example.test") && r.held_by.is_none())
    );
    assert!(
        claude_roster
            .rows
            .iter()
            .any(|r| r.email.as_deref() == Some("a02@example.test") && r.held_by.is_none())
    );
    assert!(
        !claude_roster
            .rows
            .iter()
            .any(|r| r.email.as_deref() == Some("a03@example.test"))
    );

    let store = accounts::Store::at(&state_dir);
    let (state, _) = store.load(NOW + 10).unwrap();
    assert_eq!(state.swarm_label.as_deref(), Some("m100"));
    assert_eq!(state.swarm_aliases, vec!["m32".to_owned()]);
    // Only `a01` has `warm[0] == m100`; `a02` has `warm[0] == m9`, so `home_accounts` has 1 row.
    assert_eq!(state.home_accounts.len(), 1);

    let codex_roster =
        accounts::Roster::parse_for(Cli::Codex, &fs::read_to_string(&codex_roster_path).unwrap())
            .unwrap();
    assert!(
        codex_roster
            .rows
            .iter()
            .any(|r| r.email.as_deref() == Some("c01@example.test") && r.held_by.is_none())
    );

    // Reassign `a01` and `c01` away to `m9`: both rows stay in their rosters with
    // `held_by = Some("m9")` and are excluded from `to_sign_in`.
    let pool_v2 = r#"
version = 1

[[account]]
provider = "claude"
email = "a01@example.test"
warm = ["m9"]

[[account]]
provider = "claude"
email = "a02@example.test"
warm = ["m32"]

[[account]]
provider = "codex"
email = "c01@example.test"
warm = ["m9"]
"#;
    fs::write(secrets_dir.join(AI_POOL_FILE), pool_v2).unwrap();
    assert!(accounts_sync::needs_init_accounts_sync(
        &home,
        &state_dir,
        &roster_path,
        NOW + 20
    ));
    let resync2 = accounts_sync::resync_for_setup(&home, &state_dir, &roster_path, NOW + 20, false);
    assert_eq!(resync2.failed, None);
    assert!(resync2.cli_failed.is_empty(), "{:?}", resync2.cli_failed);
    assert_eq!(resync2.to_sign_in.len(), 1);

    let claude_after = accounts::Roster::load(&roster_path).unwrap();
    let a01_after = claude_after
        .rows
        .iter()
        .find(|r| r.email.as_deref() == Some("a01@example.test"))
        .expect("a01 retained when moved to m9");
    assert_eq!(a01_after.held_by.as_deref(), Some("m9"));

    let codex_after =
        accounts::Roster::parse_for(Cli::Codex, &fs::read_to_string(&codex_roster_path).unwrap())
            .unwrap();
    let c01_after = codex_after
        .rows
        .iter()
        .find(|r| r.email.as_deref() == Some("c01@example.test"))
        .expect("c01 retained when moved to m9");
    assert_eq!(c01_after.held_by.as_deref(), Some("m9"));
}

#[test]
fn read_bounded_text_and_small_text_enforce_size_and_file_type() {
    let dir = aterm_tempfile::tempdir().expect("tempdir");
    let file = dir.path().join("cfg.toml");
    fs::write(&file, "label = \"m1\"\n").unwrap();
    assert_eq!(
        super::read_bounded_text(&file, 64).as_deref(),
        Some("label = \"m1\"\n")
    );
    assert_eq!(super::read_bounded_text(&file, 4), None);
    assert_eq!(super::read_bounded_text(dir.path(), 64), None);
    assert_eq!(
        super::super::disk::discover::small_text(&file).as_deref(),
        Some("label = \"m1\"\n")
    );
    #[cfg(unix)]
    {
        let link = dir.path().join("link.toml");
        std::os::unix::fs::symlink(&file, &link).unwrap();
        assert_eq!(super::super::disk::discover::small_text(&link), None);
    }
}
