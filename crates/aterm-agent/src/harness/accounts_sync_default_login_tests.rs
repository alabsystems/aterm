// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What a sync says it did to rows it kept, and what it does about one account listed twice beside
//! the default login, on a scratch store; every address is synthetic.

use super::*;
use crate::harness::accounts::rename_tests::NOW;
use crate::harness::alab_roster::{self, Lab, LabAccount};

fn email(local: &str) -> String {
    format!("{local}@example.com")
}

fn lab_account(cli: Cli, local: &str, label: &str, here: bool) -> LabAccount {
    LabAccount {
        pk: format!("ac-{:0>16}", local.len()),
        cli,
        vendor: accounts::Vendor::default(),
        label: label.to_owned(),
        legacy_label: None,
        email: email(local),
        identity: None,
        browser: None,
        joinable: true,
        preferred: if here {
            [("m16".to_owned(), Some(1))].into_iter().collect()
        } else {
            [("m3".to_owned(), Some(1))].into_iter().collect()
        },
    }
}

fn lab(accounts: Vec<LabAccount>) -> Lab {
    Lab {
        me: Some("m16".into()),
        settled: true,
        accounts,
        aliases: BTreeMap::new(),
        mailboxes: BTreeMap::new(),
        places: Default::default(),
    }
}

/// A scratch home with a Codex default home and an `aterm.toml`.
struct Bed {
    _root: aterm_tempfile::TempDir,
    env: Env,
    toml: PathBuf,
}

impl Bed {
    fn new() -> Self {
        let root = aterm_tempfile::TempDir::new().unwrap();
        let home = root.path().join("home");
        let state = root.path().join("state");
        let config = root.path().join("config");
        fs::create_dir_all(home.join(".codex")).unwrap();
        fs::create_dir_all(&config).unwrap();
        fs::create_dir_all(&state).unwrap();
        let toml = config.join("aterm.toml");
        fs::write(&toml, "").unwrap();
        let env = Env {
            state,
            aterm_state: None,
            cwd: home.clone(),
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
            codex_login: None,
            now: NOW,
            tz_s: 0,
            sid: String::new(),
            sock: None,
            config: Some(toml.clone()),
            claude_tmp: None,
            claude_running: None,
        };
        Self {
            _root: root,
            env,
            toml,
        }
    }

    fn store(&self, cli: Cli) -> accounts::Store {
        self.env.accounts_store_for(cli).unwrap()
    }

    fn roster_path(&self, cli: Cli) -> PathBuf {
        accounts::roster_beside_for(&self.toml, cli).unwrap()
    }

    /// `cli`'s login directory `name`, with a login in it where `made`.
    fn dir(&self, cli: Cli, name: &str, made: bool) -> String {
        let dir = self.store(cli).logins_dir().join(name);
        fs::create_dir_all(&dir).unwrap();
        if made {
            fs::write(dir.join("auth.json"), "{}").unwrap();
        }
        dir.to_str().unwrap().to_owned()
    }

    fn seed(&self, cli: Cli, rows: &[Row]) {
        fs::write(
            self.roster_path(cli),
            accounts::append_rows_for(cli, "", rows).unwrap(),
        )
        .unwrap();
    }

    fn rows(&self, cli: Cli) -> Vec<Row> {
        Roster::load_for(cli, &self.roster_path(cli)).unwrap().rows
    }

    /// The default Codex login signed in as `address`.
    fn login(&self, address: &str) {
        let token = crate::harness::codex_app_login::tests::make_chatgpt_jwt(
            Some(address),
            "plus",
            NOW + 3600,
        );
        fs::write(
            self.env.home.as_deref().unwrap().join(".codex/auth.json"),
            aterm_json::json!({"tokens": {"access_token": token}}).to_string(),
        )
        .unwrap();
    }

    /// `accounts sync --cli <id>` from `lab`, `--json`.
    fn sync(&self, cli: Cli, lab: &Lab) -> Value {
        let fetch =
            |_: &Env, _: &str| -> Result<String, String> { panic!("the lab is the source") };
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = alab_roster::with_lab(lab.clone(), || {
            run_cli_accounts_sync_with(
                cli,
                (&self.env, None),
                (Some(alab_roster::SOURCE), None, true),
                (&mut out, &mut err),
                &fetch,
            )
        });
        assert_eq!(code, ExitCode::SUCCESS, "{}", String::from_utf8_lossy(&err));
        aterm_json::from_str(String::from_utf8_lossy(&out).trim()).unwrap()
    }

    /// The same sync's receipt in words.
    fn sync_words(&self, cli: Cli, lab: &Lab) -> String {
        let fetch =
            |_: &Env, _: &str| -> Result<String, String> { panic!("the lab is the source") };
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = alab_roster::with_lab(lab.clone(), || {
            run_cli_accounts_sync_with(
                cli,
                (&self.env, None),
                (Some(alab_roster::SOURCE), None, false),
                (&mut out, &mut err),
                &fetch,
            )
        });
        assert_eq!(code, ExitCode::SUCCESS, "{}", String::from_utf8_lossy(&err));
        String::from_utf8(out).unwrap()
    }
}

/// A FIRST LAB SYNC NAMES EACH HOLD IT CLEARS AND EACH ORDER IT CHANGES, in its `--json` and its
/// receipt, a row the lab does not know among them. CONTROL: a sync with nothing left to change
/// names none.
#[test]
fn a_lab_sync_names_each_hold_it_clears_and_each_order_it_changes() {
    let cli = Cli::Codex;
    let bed = Bed::new();
    let known = bed.dir(cli, "codex-aaaaaa", true);
    let unknown = bed.dir(cli, "old", true);
    bed.seed(
        cli,
        &[
            Row {
                label: "codex-aaaaaa".into(),
                dir: Some(known),
                email: Some(email("ann")),
                class: Class::Preferred,
                held_by: Some("m3".into()),
                ..Row::default()
            },
            Row {
                label: "old".into(),
                dir: Some(unknown),
                email: Some(email("old")),
                held_by: Some("m7".into()),
                ..Row::default()
            },
        ],
    );
    let lab = lab(vec![lab_account(cli, "ann", "codex-aaaaaa", false)]);
    let doc = bed.sync(cli, &lab);
    let holds: BTreeMap<&str, (&str, &Value)> = doc["holds_changed"]
        .as_array()
        .unwrap()
        .iter()
        .map(|change| {
            (
                change["label"].as_str().unwrap(),
                (change["was"].as_str().unwrap(), &change["now"]),
            )
        })
        .collect();
    assert_eq!(holds["codex-aaaaaa"], ("m3", &Value::Null), "{doc}");
    assert_eq!(holds["old"], ("m7", &Value::Null), "{doc}");
    let orders = doc["orders_changed"].as_array().unwrap();
    assert_eq!(orders.len(), 1, "{doc}");
    assert_eq!(orders[0]["label"], Value::from("codex-aaaaaa"), "{doc}");
    assert_eq!(orders[0]["was"], Value::from("preferred"), "{doc}");
    assert_eq!(orders[0]["now"], Value::from("pool"), "{doc}");
    let again = bed.sync(cli, &lab);
    assert_eq!(again["holds_changed"], Value::Array(Vec::new()), "CONTROL");
    assert_eq!(again["orders_changed"], Value::Array(Vec::new()), "CONTROL");

    let bed = Bed::new();
    let dir = bed.dir(cli, "old", true);
    bed.seed(
        cli,
        &[Row {
            label: "old".into(),
            dir: Some(dir),
            email: Some(email("old")),
            held_by: Some("m7".into()),
            ..Row::default()
        }],
    );
    let words = bed.sync_words(cli, &lab);
    assert!(
        words
            .lines()
            .any(|line| line.contains("`old`") && line.contains("m7")),
        "{words}"
    );
}

/// AN ACCOUNT THE LAB PREFERS HERE WHOSE LABEL A ROW OF ANOTHER ACCOUNT HOLDS IS NAMED AS LEFT OUT,
/// with the label, in `--json` and the receipt: it was dropped silently. CONTROL: under a free label
/// it is added and nothing is left out.
#[test]
fn a_lab_label_another_row_holds_is_named_as_left_out() {
    let cli = Cli::Kimi;
    let vendor = accounts::Vendor {
        region: Some(crate::ai::connector::Region::Global),
        ..accounts::Vendor::default()
    };
    let bed = Bed::new();
    let dir = bed.dir(cli, "work", true);
    bed.seed(
        cli,
        &[Row {
            label: "work".into(),
            dir: Some(dir),
            email: Some(email("ann")),
            vendor,
            ..Row::default()
        }],
    );
    let lab_with = |label: &str| {
        lab(vec![LabAccount {
            vendor,
            ..lab_account(cli, "gil", label, true)
        }])
    };
    let doc = bed.sync(cli, &lab_with("work"));
    let clashes = doc["label_clashes"].as_array().unwrap();
    assert_eq!(clashes.len(), 1, "{doc}");
    assert_eq!(clashes[0]["label"], Value::from("work"), "{doc}");
    assert_eq!(clashes[0]["email"], Value::from(email("gil")), "{doc}");
    assert_eq!(bed.rows(cli).len(), 1, "the row keeps its label");
    let words = bed.sync_words(cli, &lab_with("work"));
    assert!(
        words
            .lines()
            .any(|line| line.contains("`work`") && line.contains(&email("gil"))),
        "{words}"
    );
    let doc = bed.sync(cli, &lab_with("free"));
    assert_eq!(
        doc["label_clashes"],
        Value::Array(Vec::new()),
        "CONTROL: {doc}"
    );
    assert_eq!(bed.rows(cli).len(), 2, "CONTROL");
}

/// CLAUDE CODE'S LAB SYNC SAYS WHAT THE LAB PREFERS AND NAMES NO FLEET FILE, WHICH IT DOES NOT WRITE:
/// `fleet_roster` is null in `--json` and no `fleet:` line stands in the receipt. CONTROL: under a
/// source that writes one, both name it.
#[test]
fn a_lab_sync_names_no_fleet_file_it_did_not_write() {
    let render = |lab: bool, json: bool| {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let report = SyncReport {
            publication: SyncPublication {
                machine: "m16".into(),
                roster_path: PathBuf::from("/scratch/accounts.toml"),
                fleet_path: PathBuf::from("/scratch/fleet-accounts.toml"),
                fleet_accounts: 2,
                changed: true,
                conflicts: Vec::new(),
                lab,
                rows_changed: RowChanges::default(),
                clashes: Vec::new(),
            },
            records: accounts::AccountRecords::default(),
            enrolled: BTreeSet::new(),
            needs_renew: Vec::new(),
        };
        assert_eq!(
            write_sync_report(&report, NOW, json, &mut out, &mut err),
            ExitCode::SUCCESS
        );
        String::from_utf8(out).unwrap()
    };
    let doc: Value = aterm_json::from_str(render(true, true).trim()).unwrap();
    assert_eq!(doc["fleet_roster"], Value::Null, "{doc}");
    assert!(!render(true, false).lines().any(|l| l.starts_with("fleet:")));
    let doc: Value = aterm_json::from_str(render(false, true).trim()).unwrap();
    assert_eq!(
        doc["fleet_roster"],
        Value::from("/scratch/fleet-accounts.toml"),
        "CONTROL"
    );
    assert!(
        render(false, false)
            .lines()
            .any(|l| l.starts_with("fleet:")),
        "CONTROL"
    );
}

/// A SYNC WITH NOTHING TO READ NAMES EVERY FILE IT LOOKED FOR, THE POOL FILE FIRST AMONG THEM.
/// CONTROL: with that file there, it reads it and says no such thing.
#[test]
fn a_sync_with_nothing_to_read_names_the_pool_file() {
    let cli = Cli::Codex;
    let bed = Bed::new();
    bed.seed(
        cli,
        &[Row {
            label: "main".into(),
            ..Row::default()
        }],
    );
    let run = || {
        let fetch = |_: &Env, _: &str| -> Result<String, String> { panic!("no source is fetched") };
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let _ = run_cli_accounts_sync_with(
            cli,
            (&bed.env, None),
            (None, None, false),
            (&mut out, &mut err),
            &fetch,
        );
        String::from_utf8(err).unwrap()
    };
    let pool_file = super::super::ai_pool::AI_POOL_FILE;
    let err = run();
    assert!(err.contains("nothing to read"), "{err}");
    assert!(err.contains(pool_file), "{err}");
    let secrets = bed.env.home.as_deref().unwrap().join(".secrets");
    fs::create_dir_all(&secrets).unwrap();
    fs::write(
        secrets.join(pool_file),
        "version = 1\n\n[[account]]\nprovider = \"codex\"\nemail = \"ann@example.com\"\nwarm = [\"m16\"]\n",
    )
    .unwrap();
    let err = run();
    assert!(!err.contains("nothing to read"), "CONTROL: {err}");
}

/// ONE ACCOUNT LISTED TWICE IS MERGED ON A LATER SYNC WHERE NOTHING WOULD BE LOST, KEEPING THE LABEL
/// A PERSON GAVE IT: `main` on the default login names no address, the login names the lab's
/// account, and the row the lab listed holds no login: `main` takes the address and the other goes,
/// and a new row of it is never added. Where the twin holds a login both stay and the sync says so.
/// CONTROL: an account the default login does not name is added as it was.
#[test]
fn a_later_sync_merges_a_twin_of_the_default_row_only_where_nothing_is_lost() {
    let cli = Cli::Codex;
    let main = Row {
        label: "main".into(),
        ..Row::default()
    };
    let lab = lab(vec![lab_account(cli, "ann", "codex-aaaaaa", true)]);

    // A row the sync would add.
    let bed = Bed::new();
    bed.seed(cli, std::slice::from_ref(&main));
    bed.login(&email("ann"));
    let doc = bed.sync(cli, &lab);
    assert_eq!(doc["twin_merged"]["row"], Value::from("main"), "{doc}");
    let rows = bed.rows(cli);
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].label, "main");
    assert_eq!(rows[0].dir, None);
    assert_eq!(rows[0].email, Some(email("ann")));
    let again = bed.sync(cli, &lab);
    assert_eq!(again["twin_merged"], Value::Null, "settled: {again}");
    assert_eq!(bed.rows(cli), rows, "settled");

    // A row an earlier sync added, with no login.
    let bed = Bed::new();
    let dir = bed.dir(cli, "codex-aaaaaa", false);
    bed.seed(
        cli,
        &[
            main.clone(),
            Row {
                label: "codex-aaaaaa".into(),
                dir: Some(dir),
                email: Some(email("ann")),
                ..Row::default()
            },
        ],
    );
    bed.login(&email("ann"));
    let doc = bed.sync(cli, &lab);
    assert_eq!(
        doc["twin_merged"]["merged"],
        Value::from("codex-aaaaaa"),
        "{doc}"
    );
    assert_eq!(bed.rows(cli).len(), 1);

    // One with a login of its own stays.
    let bed = Bed::new();
    let dir = bed.dir(cli, "codex-aaaaaa", true);
    let twin = Row {
        label: "codex-aaaaaa".into(),
        dir: Some(dir),
        email: Some(email("ann")),
        ..Row::default()
    };
    bed.seed(cli, &[main.clone(), twin.clone()]);
    bed.login(&email("ann"));
    let doc = bed.sync(cli, &lab);
    assert_eq!(doc["twin"]["row"], Value::from("main"), "{doc}");
    assert_eq!(doc["twin"]["twin"], Value::from("codex-aaaaaa"), "{doc}");
    assert_eq!(doc["twin_merged"], Value::Null, "{doc}");
    assert_eq!(bed.rows(cli).len(), 2);

    // Another account is added.
    let bed = Bed::new();
    bed.seed(cli, std::slice::from_ref(&main));
    bed.login(&email("zed"));
    let doc = bed.sync(cli, &lab);
    assert_eq!(doc["twin_merged"], Value::Null, "CONTROL: {doc}");
    assert_eq!(bed.rows(cli).len(), 2, "CONTROL");
}

/// A MOVE BETWEEN ONE ACCOUNT'S TWO ROWS IS NO MOVE: the report's `at a weekly limit:` passes over
/// the row of the default login's account, as the handoff's read of it does, and says the twin
/// stands. CONTROL: with no login named both directions are offered.
#[test]
fn a_weekly_limit_does_not_move_a_session_between_one_accounts_two_rows() {
    let main = Row {
        label: "main".into(),
        ..Row::default()
    };
    let twin = Row {
        label: "twin".into(),
        dir: Some("/scratch/twin".into()),
        email: Some(email("ann")),
        ..Row::default()
    };
    let roster = Roster {
        rows: vec![main.clone(), twin.clone()],
    };
    let enrolled: BTreeSet<String> = ["main", "twin"].map(str::to_owned).into();
    let state = accounts::State::default();
    let target = |from: &Row, login: Option<&str>| {
        crate::harness::accounts_pool::native_quota_target(
            (Cli::Codex, accounts::Mode::Early),
            (&roster, &state, &enrolled),
            from,
            login,
            NOW,
        )
    };
    let address = email("ann");
    assert_eq!(target(&main, Some(&address)), None);
    assert_eq!(target(&twin, Some(&address)), None);
    assert_eq!(target(&main, None), Some("twin".to_owned()), "CONTROL");
    assert_eq!(target(&twin, None), Some("main".to_owned()), "CONTROL");
    let (own, found) = accounts::default_login_twin(&roster, Some(&address)).expect("the twin");
    assert_eq!((own.label.as_str(), found.label.as_str()), ("main", "twin"));
    assert!(
        accounts::default_login_twin(&roster, Some("x@example.com")).is_none(),
        "CONTROL"
    );
}

/// One thing [`merge_default_twin`] keeps a twin of the default row for, each on a world that folds
/// but for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TwinGuard {
    /// The control: nothing stands in the way.
    Nothing,
    TwinLogin,
    TwinRecord,
    TwinBoundElsewhere,
    TwinHeld,
    TwinInCustody,
    OwnHeld,
    OwnInCustody,
    OwnSignedOut,
    /// The source gives the default row's label another address.
    SourceNamesOwn,
}

/// A completed local check keeps its named recovery route even when the twin's bound
/// directory is empty. The same roster without staged work still folds.
#[test]
fn a_local_completed_checkpoint_keeps_its_default_twin_recoverable() {
    let cli = Cli::Codex;
    let label = "codex-aaaaaa";
    for (existing, prefix, owner) in [
        (true, None, None),
        (true, Some(label), Some(label)),
        (true, Some("another-request"), Some(label)),
        (true, Some(label), None),
        (true, Some(label), Some("another-owner")),
        (false, None, None),
        (false, Some(label), Some(label)),
        (false, Some("another-request"), Some(label)),
    ] {
        let completed = owner == Some(label);
        let bed = Bed::new();
        let address = email("ann");
        bed.fleet_file(cli, &[(label, &address)]);
        let empty = bed.dir(cli, label, false);
        let mut seeded = vec![Row {
            label: "main".into(),
            ..Row::default()
        }];
        if existing {
            seeded.push(Row {
                label: label.into(),
                dir: Some(empty),
                email: Some(address.clone()),
                ..Row::default()
            });
        }
        bed.seed(cli, &seeded);
        bed.login(&address);
        fs::write(
            bed.env.home.as_deref().unwrap().join(".codex/config.toml"),
            "cli_auth_credentials_store = \"file\"\n",
        )
        .unwrap();
        let store = bed.store(cli);
        let staging = prefix.map(|prefix| {
            let staging = store.claim_staging_dir(prefix).unwrap();
            let token = crate::harness::codex_app_login::tests::make_chatgpt_jwt(
                Some(&address),
                "plus",
                NOW + 3600,
            );
            fs::write(
                staging.join("auth.json"),
                aterm_json::json!({"tokens": {"access_token": token}}).to_string(),
            )
            .unwrap();
            fs::write(
                staging.join("config.toml"),
                "cli_auth_credentials_store = \"file\"\n",
            )
            .unwrap();
            let logins = store.load_logins(NOW).unwrap().0;
            assert!(logins.labels.is_empty());
            if let Some(owner) = owner {
                accounts::write_renewal_checkpoint_for(
                    cli,
                    None,
                    owner,
                    &address,
                    staging.to_str().unwrap(),
                    None,
                    Some(accounts::Proof::Status),
                    NOW - 5,
                    &logins,
                )
                .unwrap();
                let actual = fs::canonicalize(&staging).unwrap();
                let proof =
                    accounts::read_renewal_checkpoint_for(cli, &staging, &actual, NOW).unwrap();
                assert_eq!(proof.label, owner);
                assert_eq!(proof.email, address);
                if existing && prefix == label && owner == label {
                    let healthy = local_renewal_checkpoints(cli, &store, NOW).unwrap();
                    assert!(healthy.complete);
                    let unrelated = Row {
                        label: "not-owned".into(),
                        email: Some(address.clone()),
                        ..Row::default()
                    };
                    assert!(!healthy.holds_twin(cli, &unrelated));
                    let read_checkpoint = |dir: &Path, actual: &Path| {
                        accounts::read_renewal_checkpoint_checked_for(cli, dir, actual, NOW)
                    };
                    let partial = decode_local_renewal_checkpoints(
                        cli,
                        std::iter::once(Err(std::io::Error::other(
                            "injected directory enumeration error",
                        )))
                        .chain(fs::read_dir(store.logins_dir()).unwrap()),
                        std::fs::DirEntry::file_type,
                        read_checkpoint,
                    );
                    assert!(!partial.complete);
                    assert_eq!(
                        partial.checkpoints.len(),
                        healthy.checkpoints.len(),
                        "an incomplete scan still yields available recovery candidates"
                    );
                    assert!(
                        partial.holds_twin(cli, &unrelated),
                        "incomplete enumeration cannot prove a twin owns nothing"
                    );
                    let unclassified = decode_local_renewal_checkpoints(
                        cli,
                        fs::read_dir(store.logins_dir()).unwrap(),
                        |_| Err(std::io::Error::other("injected classification error")),
                        read_checkpoint,
                    );
                    assert!(!unclassified.complete);
                    assert!(unclassified.holds_twin(cli, &unrelated));
                    let irrelevant = decode_local_renewal_checkpoints(
                        cli,
                        fs::read_dir(store.logins_dir()).unwrap().filter(|entry| {
                            entry.as_ref().is_ok_and(|entry| entry.file_name() == label)
                        }),
                        |_| panic!("noncandidate names need no classification"),
                        read_checkpoint,
                    );
                    assert!(irrelevant.complete);
                    assert!(!irrelevant.holds_twin(cli, &unrelated));
                }
            }
            staging
        });
        let staged_auth = staging
            .as_ref()
            .map(|dir| fs::read(dir.join("auth.json")).unwrap());
        assert!(store.load(NOW).unwrap().0.accounts.is_empty());
        let (code, document, error) = bed.sync_plain(cli);
        assert_eq!(code, ExitCode::SUCCESS, "{error}");
        let rows = bed.rows(cli);
        assert_eq!(
            rows.len(),
            if completed { 2 } else { 1 },
            "completed={completed}: {document}"
        );
        assert_eq!(document["twin_merged"].is_null(), completed);
        assert_eq!(
            document["twin"],
            if completed {
                aterm_json::json!({ "row": "main", "twin": label, "kept": "twin-checkpoint" })
            } else {
                Value::Null
            },
            "existing={existing}, completed={completed}: {document}"
        );
        if let Some(staging) = staging {
            assert_eq!(
                fs::read(staging.join("auth.json")).unwrap(),
                staged_auth.unwrap(),
                "sync never discards or rewrites staged credentials"
            );
            if !completed {
                continue;
            }
            let raw = Roster::load_for(cli, &bed.roster_path(cli)).unwrap();
            let logins = store.load_logins(NOW).unwrap().0;
            let bound = accounts::bound(&raw, &logins);
            assert_eq!(
                bound.row(label).unwrap().dir.as_deref(),
                staging.to_str(),
                "the original named row recovers the checked login"
            );
            assert_eq!(raw.row("main").unwrap().email, None);
        }
    }
}

/// A valid local owner temporarily unreadable at the actual decoder input is not absent.
#[test]
fn unavailable_checkpoint_io_does_not_establish_an_empty_twin() {
    let bed = Bed::new();
    let cli = Cli::Codex;
    let store = bed.store(cli);
    let address = email("ann");
    let mut owner = None;
    for label in ["owner", "other"] {
        let staging = store.claim_staging_dir(label).unwrap();
        let token = crate::harness::codex_app_login::tests::make_chatgpt_jwt(
            Some(&address),
            "plus",
            NOW + 3600,
        );
        fs::write(
            staging.join("auth.json"),
            aterm_json::json!({"tokens": {"access_token": token}}).to_string(),
        )
        .unwrap();
        accounts::write_renewal_checkpoint_for(
            cli,
            None,
            label,
            &address,
            staging.to_str().unwrap(),
            None,
            Some(accounts::Proof::Status),
            NOW - 5,
            &store.load_logins(NOW).unwrap().0,
        )
        .unwrap();
        let actual = fs::canonicalize(&staging).unwrap();
        assert_eq!(
            accounts::read_renewal_checkpoint_for(cli, &staging, &actual, NOW)
                .unwrap()
                .label,
            label
        );
        if label == "owner" {
            owner = Some(staging);
        }
    }
    let owner = owner.unwrap();
    let row = Row {
        label: "owner".into(),
        email: Some(address),
        ..Row::default()
    };
    let healthy = local_renewal_checkpoints(cli, &store, NOW).unwrap();
    assert_eq!(healthy.checkpoints.len(), 2);
    assert!(healthy.complete && healthy.holds_twin(cli, &row));
    let unavailable = decode_local_renewal_checkpoints(
        cli,
        fs::read_dir(store.logins_dir()).unwrap(),
        std::fs::DirEntry::file_type,
        |dir, actual| {
            if dir == owner {
                Err(std::io::ErrorKind::PermissionDenied.into())
            } else {
                accounts::read_renewal_checkpoint_checked_for(cli, dir, actual, NOW)
            }
        },
    );
    assert_eq!(
        unavailable.checkpoints.len(),
        1,
        "available recovery candidates remain readable"
    );
    assert_eq!(unavailable.checkpoints[0].proof.label, "other");
    assert!(unavailable.holds_twin(cli, &row));
    assert!(
        !unavailable.complete,
        "unavailable evidence cannot establish absence"
    );
}

impl Bed {
    /// A fleet file in `~/.secrets` of `accounts`, each `(label, address)`, for machine `m16`.
    fn fleet_file(&self, cli: Cli, accounts: &[(&str, &str)]) {
        let home = self.env.home.as_deref().unwrap();
        let secrets = home.join(".secrets");
        fs::create_dir_all(&secrets).unwrap();
        let rows: String = accounts
            .iter()
            .map(|(label, email)| {
                format!(
                    "[[account]]\nlabel = \"{label}\"\nemail = \"{email}\"\nmachines = [\"m16\"]\n\n"
                )
            })
            .collect();
        fs::write(secrets.join(cli_fleet_file(cli)), rows).unwrap();
        fs::write(&self.toml, "[fleet]\nmachine = \"m16\"\n").unwrap();
    }

    /// `accounts sync --cli <id>` with no source named, `--json`: its exit, document and stderr.
    fn sync_plain(&self, cli: Cli) -> (ExitCode, Value, String) {
        let fetch = |_: &Env, _: &str| -> Result<String, String> { panic!("no source is fetched") };
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = run_cli_accounts_sync_with(
            cli,
            (&self.env, None),
            (None, None, true),
            (&mut out, &mut err),
            &fetch,
        );
        let err = String::from_utf8(err).unwrap();
        let doc = aterm_json::from_str(String::from_utf8_lossy(&out).trim())
            .unwrap_or_else(|_| panic!("a document: {err}"));
        (code, doc, err)
    }

    /// [`Self::sync_plain`], whatever it answers: a refusal prints no document.
    fn sync_plain_any(&self, cli: Cli) -> (ExitCode, Value, String) {
        let fetch = |_: &Env, _: &str| -> Result<String, String> { panic!("no source is fetched") };
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = run_cli_accounts_sync_with(
            cli,
            (&self.env, None),
            (None, None, true),
            (&mut out, &mut err),
            &fetch,
        );
        let doc = aterm_json::from_str(String::from_utf8_lossy(&out).trim()).unwrap_or(Value::Null);
        (code, doc, String::from_utf8(err).unwrap())
    }

    /// A shared renewal `label` began and whose outcome is unknown: the row is in shared custody.
    fn in_custody(&self, cli: Cli, label: &str, email: &str) {
        let store = self.store(cli);
        let staging = store.claim_staging_dir(label).unwrap();
        store
            .transact_logins(NOW - 10, &mut None, |_, logins| {
                let token = logins.shared.begin(
                    accounts::shared::Intent {
                        source: accounts::shared::Source {
                            network: [1; 32],
                            account: [2; 32],
                        },
                        decision: [1; 32],
                        generation: 1,
                        configuration: [4; 32],
                        request: [5; 16],
                        label: label.into(),
                        identity: email.into(),
                        staging: staging.to_str().unwrap().into(),
                        before_generation: None,
                        begun_at: NOW - 10,
                    },
                    None,
                )?;
                logins
                    .shared
                    .finish(token, accounts::shared::Outcome::Unknown, None, NOW - 10)?;
                Ok(Vec::new())
            })
            .unwrap();
    }
}

/// WHAT A TWIN HOLDS IS NEVER LOST, AND THE REPORT'S SYNC IS THE SYNC: for each thing the merge
/// keeps a twin of the default row for, the rows stay, and what the report reads to name that sync
/// says what the sync then does. CONTROL: with nothing in the way the same world folds.
#[test]
fn each_thing_a_twin_or_the_default_row_holds_keeps_both_rows() {
    use TwinGuard::{
        Nothing, OwnHeld, OwnInCustody, OwnSignedOut, SourceNamesOwn, TwinBoundElsewhere, TwinHeld,
        TwinInCustody, TwinLogin, TwinRecord,
    };
    let cli = Cli::Codex;
    for guard in [
        Nothing,
        TwinLogin,
        TwinRecord,
        TwinBoundElsewhere,
        TwinHeld,
        TwinInCustody,
        OwnHeld,
        OwnInCustody,
        OwnSignedOut,
        SourceNamesOwn,
    ] {
        let bed = Bed::new();
        let ann = email("ann");
        let mut fleet = vec![("codex-aaaaaa", ann.as_str())];
        let zed = email("zed");
        if guard == SourceNamesOwn {
            fleet.push(("main", zed.as_str()));
        }
        bed.fleet_file(cli, &fleet);
        let dir = bed.dir(cli, "codex-aaaaaa", guard == TwinLogin);
        bed.seed(
            cli,
            &[
                Row {
                    label: "main".into(),
                    held_by: (guard == OwnHeld).then(|| "m3".to_owned()),
                    ..Row::default()
                },
                Row {
                    label: "codex-aaaaaa".into(),
                    dir: Some(dir),
                    email: Some(ann.clone()),
                    held_by: (guard == TwinHeld).then(|| "m3".to_owned()),
                    ..Row::default()
                },
            ],
        );
        bed.login(&ann);
        let store = bed.store(cli);
        match guard {
            TwinRecord => store
                .transact(NOW, &mut None, |state| {
                    Ok(accounts::note_auth(
                        state,
                        "codex-aaaaaa",
                        accounts::Auth::Ok,
                        NOW - 5,
                    ))
                })
                .map(|_| ())
                .unwrap(),
            TwinBoundElsewhere => {
                let elsewhere = bed.dir(cli, "elsewhere", false);
                store
                    .transact_logins(NOW, &mut None, |_, logins| {
                        logins.labels.insert(
                            "codex-aaaaaa".into(),
                            accounts::Binding {
                                dir: Some(elsewhere.clone()),
                                ..accounts::Binding::default()
                            },
                        );
                        Ok(Vec::new())
                    })
                    .unwrap();
            }
            TwinInCustody => bed.in_custody(cli, "codex-aaaaaa", &ann),
            OwnInCustody => bed.in_custody(cli, "main", &zed),
            OwnSignedOut => crate::ai::testing::current_codex_auth(
                &store,
                &bed.roster_path(cli),
                "main",
                accounts::Auth::SignedOut,
                NOW,
            ),
            _ => {}
        }
        let records = store
            .read_account_records(Some(&bed.roster_path(cli)), NOW)
            .unwrap();
        let (own, twin) = accounts::default_login_twin(&records.roster, Some(&ann)).unwrap();
        let named = sync_twin_verdict(
            cli,
            &bed.env,
            (&own.label, &twin.label, &ann),
            &records.roster,
            (&records.state, &records.logins),
            NOW,
        )
        .is_ok();
        let (code, doc, err) = bed.sync_plain(cli);
        assert_eq!(code, ExitCode::SUCCESS, "{guard:?}: {err}");
        let folded = doc["twin_merged"] != Value::Null;
        assert_eq!(folded, guard == Nothing, "{guard:?}: {doc}");
        assert_eq!(
            doc["twin"],
            if folded {
                Value::Null
            } else {
                aterm_json::json!({ "row": "main", "twin": "codex-aaaaaa", "kept": match guard {
                    TwinHeld | TwinInCustody => "twin-held",
                    OwnHeld | OwnInCustody => "row-held",
                    OwnSignedOut => "row-signed-out",
                    SourceNamesOwn => "source-names-row",
                    _ => "twin-holds",
                } })
            },
            "{guard:?}: {doc}"
        );
        assert_eq!(named, folded, "{guard:?}: the report's sync: {doc}");
        let rows = bed.rows(cli);
        assert_eq!(
            rows.len(),
            if folded { 1 } else { 2 },
            "{guard:?}: {rows:?}"
        );
        assert_eq!(doc["rows"], Value::from(rows.len() as u64), "{guard:?}");
        assert_eq!(doc["default_login_names"], Value::from(ann.clone()));
        if folded {
            assert_eq!(rows[0].email, Some(ann.clone()));
        } else {
            let main = rows.iter().find(|row| row.label == "main").unwrap();
            assert_eq!(main.email, None, "{guard:?}: `main` names no address");
        }
    }
}

/// A SYNC COUNTS THE ROWS IT LEAVES: the receipt's count after a merge is the roster's. CONTROL: the
/// roster did lose the row.
#[test]
fn a_syncs_receipt_counts_the_rows_it_leaves_after_a_merge() {
    let cli = Cli::Codex;
    let bed = Bed::new();
    let dir = bed.dir(cli, "codex-aaaaaa", false);
    bed.seed(
        cli,
        &[
            Row {
                label: "main".into(),
                ..Row::default()
            },
            Row {
                label: "codex-aaaaaa".into(),
                dir: Some(dir),
                email: Some(email("ann")),
                ..Row::default()
            },
            Row {
                label: "codex-bbbbbb".into(),
                dir: Some(bed.dir(cli, "codex-bbbbbb", true)),
                email: Some(email("bea")),
                ..Row::default()
            },
        ],
    );
    bed.login(&email("ann"));
    let lab = lab(vec![
        lab_account(cli, "ann", "codex-aaaaaa", true),
        lab_account(cli, "bea", "codex-bbbbbb", true),
    ]);
    let doc = bed.sync(cli, &lab);
    assert_eq!(doc["rows"], Value::from(2u64), "{doc}");
    assert_eq!(bed.rows(cli).len(), 2, "CONTROL");
    let words = bed.sync_words(cli, &lab);
    assert!(words.contains("2 rows in the roster"), "{words}");
}

/// A DEFAULT LOGIN THAT NAMES NO ROW'S ACCOUNT IS SAID: the row on it keeps its address, since
/// nothing else holds that login's, and the receipt and `--json` say so with the step that puts it
/// back. CONTROL: the login back on the row's address says nothing.
#[test]
fn a_sync_says_a_default_login_no_row_holds() {
    let cli = Cli::Codex;
    let bed = Bed::new();
    let (main, far) = (email("main"), email("far"));
    bed.seed(
        cli,
        &[Row {
            label: "main".into(),
            email: Some(main.clone()),
            ..Row::default()
        }],
    );
    let lab = lab(vec![lab_account(cli, "main", "main", true)]);
    bed.login(&far);
    let doc = bed.sync(cli, &lab);
    assert_eq!(
        doc["default_login_names"],
        Value::from(far.clone()),
        "{doc}"
    );
    assert_eq!(doc["default_login_moved_off"]["row"], Value::from("main"));
    assert_eq!(
        doc["default_login_moved_off"]["email"],
        Value::from(main.clone())
    );
    assert_eq!(doc["default_login_refused"], Value::Null, "{doc}");
    let words = bed.sync_words(cli, &lab);
    assert!(
        words.contains(&format!("default login: it names {far}")),
        "{words}"
    );
    assert!(
        words.contains("`aterm harness accounts login main --cli codex`"),
        "{words}"
    );
    assert_eq!(bed.rows(cli)[0].email, Some(main.clone()));

    bed.login(&main);
    let doc = bed.sync(cli, &lab);
    assert_eq!(
        doc["default_login_names"],
        Value::from(main),
        "CONTROL: {doc}"
    );
    assert_eq!(
        doc["default_login_moved_off"],
        Value::Null,
        "CONTROL: {doc}"
    );
    assert!(
        !bed.sync_words(cli, &lab).contains("default login:"),
        "CONTROL"
    );
}

/// A REFUSED MOVE OF THE DEFAULT LOGIN LEAVES A RECEIPT OF WHAT THE SYNC WROTE: the hold it cleared
/// before the refusal is named, in words and `--json`, beside the refusal, which names the step, and
/// the sync fails. CONTROL: taking that step, the same sync moves the login.
#[test]
fn a_refused_move_of_the_default_login_still_says_what_the_sync_changed() {
    let cli = Cli::Codex;
    let bed = Bed::new();
    let (main, work) = (email("main"), email("work"));
    let foreign = bed.store(cli).login_dir("main").unwrap();
    fs::create_dir_all(&foreign).unwrap();
    fs::write(foreign.join("auth.json"), "{}").unwrap();
    bed.seed(
        cli,
        &[
            Row {
                label: "main".into(),
                email: Some(main.clone()),
                held_by: Some("m3".into()),
                ..Row::default()
            },
            Row {
                label: "work".into(),
                dir: Some(bed.dir(cli, "work", false)),
                email: Some(work.clone()),
                ..Row::default()
            },
        ],
    );
    bed.login(&work);
    let lab = lab(vec![
        lab_account(cli, "main", "main", true),
        lab_account(cli, "work", "work", true),
    ]);
    let run = |json: bool| {
        let fetch =
            |_: &Env, _: &str| -> Result<String, String> { panic!("the lab is the source") };
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = alab_roster::with_lab(lab.clone(), || {
            run_cli_accounts_sync_with(
                cli,
                (&bed.env, None),
                (Some(alab_roster::SOURCE), None, json),
                (&mut out, &mut err),
                &fetch,
            )
        });
        (
            code,
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
        )
    };
    let (code, out, err) = run(false);
    assert_eq!(code, ExitCode::FAILURE, "{out}{err}");
    assert!(out.contains("hold cleared: `main`"), "{out}");
    assert!(err.contains("holds a login that names no address"), "{err}");
    assert!(
        err.contains("move that directory away and sync again"),
        "{err}"
    );
    assert!(
        err.contains("`aterm harness accounts login main --cli codex`"),
        "{err}"
    );
    assert!(!err.contains("metadata is unchanged"), "{err}");
    let main_row = |rows: &[Row]| {
        rows.iter()
            .find(|row| row.label == "main")
            .cloned()
            .unwrap()
    };
    assert_eq!(
        main_row(&bed.rows(cli)).held_by,
        None,
        "as the receipt says"
    );
    assert_eq!(main_row(&bed.rows(cli)).dir, None, "no row moved");

    let (code, out, _) = run(true);
    assert_eq!(code, ExitCode::FAILURE);
    let doc: Value = aterm_json::from_str(out.trim()).unwrap();
    assert!(doc["default_login_refused"].as_str().is_some(), "{doc}");
    assert_eq!(doc["default_login_moved_off"]["row"], Value::from("main"));

    fs::rename(&foreign, foreign.with_extension("away")).unwrap();
    let (code, out, err) = run(false);
    assert_eq!(code, ExitCode::SUCCESS, "CONTROL: {out}{err}");
    assert!(out.contains("on this machine's own login: work"), "{out}");
    let rows = bed.rows(cli);
    assert_eq!(main_row(&rows).dir.as_deref(), foreign.to_str(), "CONTROL");
}

/// The report's verdict on folding `codex-aaaaaa` into `main`, under the lab where it reads one.
fn report_folds(bed: &Bed, cli: Cli, ann: &str, lab: Option<&Lab>) -> Result<(), TwinKept> {
    let records = bed
        .store(cli)
        .read_account_records(Some(&bed.roster_path(cli)), NOW)
        .unwrap();
    let verdict = || {
        sync_twin_verdict(
            cli,
            &bed.env,
            ("main", "codex-aaaaaa", ann),
            &records.roster,
            (&records.state, &records.logins),
            NOW,
        )
    };
    match lab {
        Some(lab) => alab_roster::with_lab(lab.clone(), verdict),
        None => verdict(),
    }
}

/// THE REPORT AND THE SYNC ANSWER ONE PREDICATE WHERE THE SOURCE CHANGES A HOLD THE ROSTER SHOWS: a
/// lab that clears a hold folds on both, and a pool that holds the twin elsewhere folds on neither.
/// CONTROL: the same pool with the twin warm here folds on both.
#[test]
fn the_report_and_the_sync_judge_a_source_that_changes_a_hold_alike() {
    let cli = Cli::Codex;
    let seed = |bed: &Bed, ann: &str, held: bool| {
        let dir = bed.dir(cli, "codex-aaaaaa", false);
        bed.seed(
            cli,
            &[
                Row {
                    label: "main".into(),
                    ..Row::default()
                },
                Row {
                    label: "codex-aaaaaa".into(),
                    dir: Some(dir),
                    email: Some(ann.to_owned()),
                    held_by: held.then(|| "m3".to_owned()),
                    ..Row::default()
                },
            ],
        );
        bed.login(ann);
    };

    let bed = Bed::new();
    let ann = email("ann");
    seed(&bed, &ann, true);
    let lab = lab(vec![lab_account(cli, "ann", "codex-aaaaaa", true)]);
    let named = report_folds(&bed, cli, &ann, Some(&lab));
    let doc = bed.sync(cli, &lab);
    assert_eq!(
        named.is_ok(),
        doc["twin_merged"] != Value::Null,
        "{named:?}: {doc}"
    );

    // A source this Mac is in no row of: a sync refuses, and the report names no sync.
    let bed = Bed::new();
    let ann = email("ann");
    bed.fleet_file(cli, &[("codex-aaaaaa", &ann)]);
    fs::write(&bed.toml, "").unwrap();
    seed(&bed, &ann, false);
    let (code, doc, err) = bed.sync_plain_any(cli);
    if code == ExitCode::SUCCESS {
        assert_eq!(report_folds(&bed, cli, &ann, None), Ok(()), "{doc}");
    } else {
        assert_eq!(
            report_folds(&bed, cli, &ann, None),
            Err(TwinKept::NoSync),
            "{err}"
        );
    }

    for (warm, folds) in [("m3", false), ("m16", true)] {
        let bed = Bed::new();
        let ann = email("ann");
        let secrets = bed.env.home.as_deref().unwrap().join(".secrets");
        fs::create_dir_all(&secrets).unwrap();
        fs::write(
            secrets.join(crate::harness::ai_pool::AI_POOL_FILE),
            format!(
                "version = 1\n\n[last_heard]\nm3 = \"2026-10-27\"\nm16 = \"2026-10-27\"\n\n\
                 [[account]]\nprovider = \"codex\"\nemail = \"{ann}\"\nwarm = [\"{warm}\"]\n"
            ),
        )
        .unwrap();
        fs::write(&bed.toml, "[fleet]\nmachine = \"m16\"\n").unwrap();
        seed(&bed, &ann, false);
        let named = report_folds(&bed, cli, &ann, None);
        let (code, doc, err) = bed.sync_plain(cli);
        assert_eq!(code, ExitCode::SUCCESS, "{warm}: {err}");
        assert_eq!(named.is_ok(), folds, "{warm}: {named:?}: {doc}");
        assert_eq!(doc["twin_merged"] != Value::Null, folds, "{warm}: {doc}");
    }
}

/// A RENEWAL'S COMPLETION THAT NAMES THE TWIN KEEPS IT ON BOTH SURFACES, for that reason and no
/// other. CONTROL: with no completion both fold.
#[test]
fn a_completion_that_names_the_twin_keeps_it_on_the_report_and_the_sync() {
    let cli = Cli::Codex;
    let label = "codex-aaaaaa";
    for completed in [false, true] {
        let bed = Bed::new();
        let address = email("ann");
        bed.fleet_file(cli, &[(label, &address)]);
        let empty = bed.dir(cli, label, false);
        bed.seed(
            cli,
            &[
                Row {
                    label: "main".into(),
                    ..Row::default()
                },
                Row {
                    label: label.into(),
                    dir: Some(empty),
                    email: Some(address.clone()),
                    ..Row::default()
                },
            ],
        );
        bed.login(&address);
        let store = bed.store(cli);
        if completed {
            let staging = store.claim_staging_dir(label).unwrap();
            let token = crate::harness::codex_app_login::tests::make_chatgpt_jwt(
                Some(&address),
                "plus",
                NOW + 3600,
            );
            fs::write(
                staging.join("auth.json"),
                aterm_json::json!({"tokens": {"access_token": token}}).to_string(),
            )
            .unwrap();
            fs::write(
                staging.join("config.toml"),
                "cli_auth_credentials_store = \"file\"\n",
            )
            .unwrap();
            let logins = store.load_logins(NOW).unwrap().0;
            accounts::write_renewal_checkpoint_for(
                cli,
                None,
                label,
                &address,
                staging.to_str().unwrap(),
                None,
                Some(accounts::Proof::Status),
                NOW - 5,
                &logins,
            )
            .unwrap();
        }
        let named = report_folds(&bed, cli, &address, None);
        assert_eq!(
            named,
            if completed {
                Err(TwinKept::TwinCheckpoint)
            } else {
                Ok(())
            }
        );
        let (code, doc, err) = bed.sync_plain(cli);
        assert_eq!(code, ExitCode::SUCCESS, "{err}");
        assert_eq!(doc["twin_merged"] == Value::Null, completed, "{doc}");
        assert_eq!(bed.rows(cli).len(), if completed { 2 } else { 1 });
    }
}
