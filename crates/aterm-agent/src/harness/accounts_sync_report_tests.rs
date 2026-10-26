// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const NOW: i64 = 1_790_000_000;
const CSV: &str = "email,computer,claude\none@example.test,m1,1\ntwo@example.test,m1,1\nthree@example.test,m2,1\n";

struct Bed {
    _root: aterm_tempfile::TempDir,
    env: Env,
    store: accounts::Store,
    path: PathBuf,
}

impl Bed {
    fn new() -> Self {
        let root = aterm_tempfile::TempDir::new().unwrap();
        let home = root.path().join("home");
        let config = home.join(".config/aterm/aterm.toml");
        fs::create_dir_all(config.parent().unwrap()).unwrap();
        fs::write(&config, "[fleet]\nmachine = \"m1\"\n").unwrap();
        let env = Env {
            state: root.path().join("state"),
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
            now: NOW,
            tz_s: 0,
            sid: String::new(),
            sock: None,
            config: Some(config),
            claude_tmp: None,
            claude_running: None,
        };
        let path = env.accounts_roster().unwrap();
        let store = env.accounts_store().bounded_by(&[]);
        Self {
            _root: root,
            env,
            store,
            path,
        }
    }

    fn sync(&self) -> SyncReport {
        sync_accounts_with(
            &self.env,
            Some("fixture.csv"),
            None,
            &SyncHow {
                fetch: &|_, _| Ok(CSV.into()),
                ladder: &[],
                fleet_snapshots: &[],
            },
        )
        .unwrap()
    }

    fn publication(&self, changed: bool) -> SyncPublication {
        SyncPublication {
            machine: "m1".into(),
            roster_path: self.path.clone(),
            fleet_path: self.path.with_file_name(FLEET_ROSTER_FILE),
            fleet_accounts: 3,
            changed,
        }
    }

    fn seed(&self) {
        self.sync();
        self.renew("old").unwrap();
        self.store
            .transact(NOW, &mut None, |state| {
                Ok(accounts::note_wall(
                    state,
                    "m1-2",
                    "weekly",
                    Some(NOW + 3600),
                    NOW,
                    NOW,
                ))
            })
            .unwrap();
    }

    fn renew(&self, directory: &str) -> Result<Vec<accounts::Event>, String> {
        let dir = self.env.state.join(directory);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(".credentials.json"), "opaque fixture").unwrap();
        self.store.transact_logins(NOW, &mut None, |state, logins| {
            Ok(accounts::note_renewed(
                state,
                logins,
                "m1",
                None,
                dir.to_str().unwrap(),
                None,
                NOW,
            ))
        })
    }

    fn pending(&self) {
        let witness = self.store.state_witness_file();
        fs::rename(&witness, witness.with_extension("retained")).unwrap();
        fs::create_dir(&witness).unwrap();
        assert!(self.renew("new").is_err());
        fs::rename(&witness, witness.with_extension("obstruction")).unwrap();
        fs::rename(witness.with_extension("retained"), &witness).unwrap();
        assert!(self.store.sync_needs_recovery());
    }

    fn bytes(&self) -> [Option<Vec<u8>>; 5] {
        [
            self.path.clone(),
            self.store.state_file(),
            self.store.logins_file(),
            self.store.ledger_path(),
            self.store.dir.join("sync-commit.json"),
        ]
        .map(|path| fs::read(path).ok())
    }
}

fn render(report: &SyncReport) -> Value {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    assert_eq!(
        write_sync_report(report, NOW, true, &mut out, &mut err),
        ExitCode::SUCCESS
    );
    aterm_json::from_slice(&out).unwrap()
}

fn report_case(fault: i64, published: bool, edit: bool) -> (bool, bool, bool) {
    let bed = Bed::new();
    bed.seed();
    match fault {
        1 | 6 => bed.pending(),
        2 => fs::write(bed.store.dir.join("sync-commit.json"), "{").unwrap(),
        3 => fs::write(bed.store.logins_file(), "{").unwrap(),
        4 => fs::write(bed.store.state_file(), "{").unwrap(),
        5 => fs::write(&bed.path, "[[account]").unwrap(),
        _ => {}
    }
    let _held = (fault == 6).then(|| bed.store.acquire().unwrap());
    let before = bed.bytes();
    let result = read_sync_report(&bed.store, NOW, bed.publication(published), None);
    if fault != 1 {
        assert_eq!(bed.bytes(), before, "only admitted recovery can write");
    }
    let report = match result {
        Ok(report) => report,
        Err(why) => {
            assert!(why.contains("report could not be read"), "{why}");
            assert!(why.contains("published files are kept"), "{why}");
            return (false, published, true);
        }
    };
    let first = render(&report);
    assert_eq!(
        first["machine_accounts"][0]["enrolled"].as_bool(),
        Some(true)
    );
    assert_eq!(
        first["machine_accounts"][0]["condition"].as_str(),
        Some("usable")
    );
    assert!(first["needs_renew"].as_array().unwrap().is_empty());
    let expected_dir = bed.env.state.join(if fault == 1 { "new" } else { "old" });
    assert_eq!(
        report.records.roster.row("m1").unwrap().dir.as_deref(),
        expected_dir.to_str()
    );
    let changed = report.publication.changed;
    if edit {
        fs::write(&bed.path, "").unwrap();
        fs::write(&report.publication.fleet_path, "not valid toml [").unwrap();
        bed.store
            .transact(NOW + 1, &mut None, |state| {
                Ok(accounts::note_auth(
                    state,
                    "m1",
                    accounts::Auth::SignedOut,
                    NOW + 1,
                ))
            })
            .unwrap();
    }
    let again = render(&report);
    let init = report.into_init(false);
    let retained = first == again
        && init.machine == "m1"
        && init.rows == 2
        && init.enrolled == 1
        && init.fleet == 3
        && init.to_sign_in.is_empty()
        && init.changed == changed;
    (true, changed, retained)
}

pub(crate) fn report_transition(
    state: &aterm_spec::interp::State,
    action: &str,
) -> aterm_spec::interp::State {
    let mut next = state.clone();
    match action {
        "AnotherFault" => {
            next.insert("fault", state["fault"] + 1);
        }
        "Published" => {
            next.insert("published", 1);
        }
        "LaterEdit" => {
            next.insert("edit", 1);
        }
        "Read" => {
            let (available, changed, retained) =
                report_case(state["fault"], state["published"] == 1, state["edit"] == 1);
            next.insert("phase", 1);
            next.insert("available", i64::from(available));
            next.insert("changed", i64::from(changed));
            next.insert("retained", i64::from(retained));
        }
        _ => unreachable!(),
    }
    next
}

#[test]
fn sync_report_refines_recovery_refusal_and_snapshot_retention() {
    let model = aterm_models_harness::account_sync_report_model();
    assert_eq!(model.mutant_dials(), vec![1, 2, 3, 4, 5]);
    assert!(aterm_spec::interp::prove_and_catch(&model) > 1);
    let mut cases = 0;
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                let next = report_transition(&state, action.name);
                assert!(
                    model.successors(action.name, &state).contains(&next),
                    "{state:?} -> {next:?}"
                );
                cases += usize::from(action.name == "Read");
            }
        }
    }
    assert_eq!(cases, 28);
    let mut bad = model.init_state();
    bad.insert("phase", 1);
    bad.insert("fault", 2);
    bad.insert("available", 1);
    assert!(!model.check_invariant("UnknownRefused", &bad));
    bad.insert("fault", 0);
    bad.insert("available", 0);
    assert!(!model.check_invariant("KnownAvailable", &bad));
    bad.insert("fault", 1);
    bad.insert("available", 1);
    assert!(!model.check_invariant("RecoveryCounted", &bad));
    bad.insert("published", 1);
    assert!(!model.check_invariant("PublishedCounted", &bad));
    bad.insert("retained", 0);
    assert!(!model.check_invariant("SnapshotRetained", &bad));
}

#[test]
fn a_staging_recovery_is_a_sync_change_and_is_reported_from_its_binding() {
    let bed = Bed::new();
    assert!(bed.sync().publication.changed);
    assert!(!bed.sync().publication.changed);
    let staged = bed.store.claim_staging_dir("m1-2").unwrap();
    fs::write(staged.join(".credentials.json"), "opaque fixture").unwrap();
    accounts::write_renewal_checkpoint(
        "m1-2",
        "two@example.test",
        staged.to_str().unwrap(),
        None,
        NOW - 1,
        &bed.store.load_logins(NOW).unwrap().0,
    )
    .unwrap();
    let report = bed.sync();
    assert!(
        report.publication.changed,
        "adopting the staged login wrote account records"
    );
    assert_eq!(
        report.records.roster.row("m1-2").unwrap().dir.as_deref(),
        staged.to_str()
    );
    assert!(report.enrolled.contains("m1-2"));
    let json = render(&report);
    assert_eq!(json["changed"].as_bool(), Some(true));
    assert!(json["needs_renew"].as_array().unwrap().is_empty());
    let mut text = Vec::new();
    write_sync_report(&report, NOW, false, &mut text, &mut Vec::new());
    assert!(
        !String::from_utf8(text)
            .unwrap()
            .contains("no account changes")
    );
    let init = report.into_init(false);
    assert_eq!((init.rows, init.enrolled, init.fleet), (2, 2, 3));
    assert!(init.to_sign_in.is_empty());
    assert!(
        !bed.sync().publication.changed,
        "the consumed checkpoint is not adopted twice"
    );
}

#[test]
fn sync_counts_initial_journal_recovery_even_when_its_source_is_unchanged() {
    let bed = Bed::new();
    bed.seed();
    assert!(!bed.sync().publication.changed);
    bed.pending();
    let report = bed.sync();
    assert!(report.publication.changed);
    assert_eq!(
        report.records.roster.row("m1").unwrap().dir.as_deref(),
        bed.env.state.join("new").to_str()
    );
    assert!(!bed.store.sync_needs_recovery());
    assert!(!bed.sync().publication.changed);
}

#[test]
fn a_completed_concurrent_commit_is_observed_without_rewriting_sync_source_metrics() {
    let bed = Bed::new();
    bed.seed();
    let publication = bed.publication(false);
    bed.renew("later").unwrap();
    let report = read_sync_report(&bed.store, NOW, publication, None).unwrap();
    assert!(
        !report.publication.changed,
        "the other completed writer is not this sync's change"
    );
    assert_eq!(
        report.records.roster.row("m1").unwrap().dir.as_deref(),
        bed.env.state.join("later").to_str()
    );
    assert_eq!(report.publication.fleet_accounts, 3);
    assert_eq!(report.into_init(false).enrolled, 1);
}

#[test]
fn a_default_row_bound_to_a_missing_isolated_login_needs_renewal() {
    let bed = Bed::new();
    bed.seed();
    let mut roster = Roster::load(&bed.path).unwrap();
    roster.rows[0].dir = None;
    fs::write(&bed.path, format_roster_toml(&roster).unwrap()).unwrap();
    fs::rename(
        bed.env.state.join("old/.credentials.json"),
        bed.env.state.join("kept-credential"),
    )
    .unwrap();
    let report = read_sync_report(&bed.store, NOW, bed.publication(false), None).unwrap();
    let json = render(&report);
    assert_eq!(
        json["machine_accounts"][0]["default_login"].as_bool(),
        Some(false)
    );
    assert_eq!(
        json["machine_accounts"][0]["enrolled"].as_bool(),
        Some(false)
    );
    assert_eq!(report.needs_renew, vec!["m1"]);
    let init = report.into_init(false);
    assert_eq!(init.enrolled, 0);
    assert_eq!(init.to_sign_in, vec!["m1"]);
}
