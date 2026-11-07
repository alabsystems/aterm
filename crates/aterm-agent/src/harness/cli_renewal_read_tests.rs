// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::cell::RefCell;

fn codex_login(home: &Path) {
    std::fs::create_dir_all(home).unwrap();
    std::fs::write(
        home.join("config.toml"),
        "cli_auth_credentials_store='file'\n",
    )
    .unwrap();
    let token = crate::harness::codex_app_login::tests::make_jwt(&aterm_json::json!({
        "email": "main@example.test",
        "https://api.openai.com/auth": {"chatgpt_user_id": "main", "chatgpt_account_id": "fixture-workspace"}
    }));
    std::fs::write(
        home.join("auth.json"),
        aterm_json::json!({
            "auth_mode": "chatgpt", "tokens": {"id_token": token, "access_token": token}
        })
        .to_string(),
    )
    .unwrap();
}

struct Host {
    emails: RefCell<Vec<String>>,
}

impl AccountsHost for Host {
    fn stdin_is_terminal(&self) -> bool {
        true
    }
    fn auth_status(&self, _: &str) -> Result<Captured, String> {
        panic!("no provider status")
    }
    fn unaskable(&self) -> Option<String> {
        None
    }
    fn login(&self, _: Option<&str>) -> Result<i32, String> {
        panic!("no plain login")
    }
    fn now(&self) -> i64 {
        0
    }
    fn renew_flow(&self, _: &str, email: &str, _: &RenewBrowser) -> Result<i32, String> {
        self.emails.borrow_mut().push(email.into());
        Ok(1)
    }
    fn vendor_sign_in(
        &self,
        req: &crate::harness::login_drive::Request<'_>,
    ) -> crate::harness::login_drive::Drove {
        self.emails
            .borrow_mut()
            .push(req.row.email.clone().unwrap());
        crate::harness::login_drive::Drove {
            outcome: crate::harness::login_drive::Outcome::Failed,
            host: None,
        }
    }
}

struct Bed {
    _root: Tmp,
    cli: Cli,
    env: Env,
    store: accounts::Store,
    path: PathBuf,
    dirs: [PathBuf; 2],
}

impl Bed {
    fn new(cli: Cli) -> Self {
        let root = tmp_dir("manual-renewal-read");
        let env = env_at(&root);
        let store = env.accounts_store_for(cli).unwrap();
        let path = env.accounts_roster_for(cli).unwrap();
        let dirs = [root.path().join("old"), root.path().join("new")];
        for dir in &dirs {
            let credential = dir.join(cli.connector().stores[0]);
            std::fs::create_dir_all(credential.parent().unwrap()).unwrap();
            std::fs::write(credential, b"opaque fixture").unwrap();
            if cli == Cli::Codex {
                codex_login(dir);
            }
        }
        let raw = Roster {
            rows: vec![accounts::Row {
                label: "main".into(),
                email: Some("main@example.test".into()),
                dir: Some(root.path().join("configured").to_str().unwrap().into()),
                vendor: accounts::Vendor {
                    region: (cli == Cli::Kimi).then_some(crate::ai::connector::Region::Global),
                    ..Default::default()
                },
                ..Default::default()
            }],
        };
        std::fs::write(
            &path,
            accounts::append_rows_for(cli, "", &raw.rows).unwrap(),
        )
        .unwrap();
        let bed = Self {
            _root: root,
            cli,
            env,
            store,
            path,
            dirs,
        };
        bed.renew_ended(0).unwrap();
        // A session here ran on main within the week, so `--due` renews its ended login
        // (`DESIGN-alab-one-mesh-2026-11-05.md` §4.15 rule 4).
        bed.store
            .transact(bed.env.now, &mut None, |state| {
                let events = accounts::note_session(state, "s-1", "main", None, bed.env.now - 3600);
                state.sessions.remove("s-1");
                Ok(events)
            })
            .unwrap();
        bed
    }

    fn renew_ended(&self, index: usize) -> Result<Vec<accounts::Event>, String> {
        let raw = Roster::load_for(self.cli, &self.path)?;
        self.store
            .transact_logins(self.env.now, &mut None, |state, logins| {
                let mut events = accounts::note_renewed(
                    state,
                    logins,
                    "main",
                    None,
                    self.dirs[index].to_str().unwrap(),
                    None,
                    self.env.now,
                );
                if self.cli == Cli::Codex {
                    let effective = accounts::bound(&raw, logins);
                    let context = self
                        .store
                        .capture_auth_context(effective.row("main").unwrap())
                        .unwrap();
                    accounts::stage_login_end(
                        state,
                        raw.row("main").unwrap(),
                        logins,
                        context,
                        self.env.now,
                    );
                } else {
                    events.extend(accounts::note_login_end(
                        state,
                        logins,
                        "main",
                        self.env.now,
                    ));
                }
                Ok(events)
            })
    }

    fn pending(&self) {
        let witness = self.store.state_witness_file();
        std::fs::rename(&witness, witness.with_extension("retained")).unwrap();
        std::fs::create_dir(&witness).unwrap();
        assert!(self.renew_ended(1).is_err());
        std::fs::rename(&witness, witness.with_extension("obstruction")).unwrap();
        std::fs::rename(witness.with_extension("retained"), witness).unwrap();
    }

    fn records(&self) -> [Option<Vec<u8>>; 7] {
        [
            self.path.clone(),
            self.store.state_file(),
            self.store.logins_file(),
            self.store.state_witness_file(),
            self.store.logins_witness_file(),
            self.store.ledger_path(),
            self.store.dir.join("sync-commit.json"),
        ]
        .map(|path| std::fs::read(path).ok())
    }

    fn run(&self, due: bool, host: &Host) -> (ExitCode, String) {
        let labels = if due { Vec::new() } else { vec!["main".into()] };
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let args = (labels.as_slice(), due, Some([].as_slice()));
        let code = if self.cli == Cli::Claude {
            run_accounts_renew(&self.env, host, args, &mut out, &mut err)
        } else {
            run_accounts_renew_for(self.cli, &self.env, host, args, &mut out, &mut err)
        };
        (code, String::from_utf8(err).unwrap())
    }
}

#[test]
fn manual_renewals_recover_or_refuse_before_dispatching_any_provider() {
    for cli in [Cli::Claude, Cli::Codex, Cli::Kimi, Cli::Grok] {
        for due in [false, true] {
            for fault in 0..7 {
                let bed = Bed::new(cli);
                let credentials_before = bed
                    .dirs
                    .each_ref()
                    .map(|dir| std::fs::read(dir.join(cli.connector().stores[0])).unwrap());
                match fault {
                    1 | 6 => bed.pending(),
                    2 => std::fs::write(&bed.path, "[[account]").unwrap(),
                    3 => std::fs::write(bed.store.logins_file(), "{").unwrap(),
                    4 => std::fs::write(bed.store.state_file(), "{").unwrap(),
                    5 => std::fs::write(bed.store.dir.join("sync-commit.json"), "{").unwrap(),
                    _ => {}
                }
                let _held = (fault == 6).then(|| bed.store.acquire().unwrap());
                let before = bed.records();
                let host = Host {
                    emails: RefCell::new(Vec::new()),
                };
                // The lock stays held for the whole run, so no pause of the ladder can take it:
                // the refusal is the subject, not how long a take waits before it.
                let (code, err) = if fault == 6 {
                    accounts::with_store_ladder(&[], || bed.run(due, &host))
                } else {
                    bed.run(due, &host)
                };
                // The isolated provider stub deliberately fails authentication.
                assert_eq!(
                    code,
                    ExitCode::FAILURE,
                    "{cli:?}, due={due}, fault={fault}: {err}"
                );
                if fault < 2 {
                    // The recovery runs everywhere; the sign-in after it only where it runs
                    // (`links_here`).
                    let asked: &[&str] = if links_here(cli) {
                        &["main@example.test"]
                    } else {
                        &[]
                    };
                    assert_eq!(
                        *host.emails.borrow(),
                        asked,
                        "{cli:?}, due={due}, fault={fault}"
                    );
                    assert_eq!(
                        bed.store
                            .load_logins(bed.env.now)
                            .unwrap()
                            .0
                            .get("main")
                            .unwrap()
                            .dir
                            .as_deref(),
                        bed.dirs[usize::from(fault == 1)].to_str(),
                    );
                } else {
                    assert!(
                        host.emails.borrow().is_empty(),
                        "{cli:?}, due={due}, fault={fault}"
                    );
                    assert!(!err.is_empty());
                    assert_eq!(
                        bed.records(),
                        before,
                        "a refused read changed account records"
                    );
                }
                for (index, dir) in bed.dirs.iter().enumerate() {
                    assert_eq!(
                        std::fs::read(dir.join(cli.connector().stores[0])).unwrap(),
                        if cli == Cli::Codex {
                            credentials_before[index].as_slice()
                        } else {
                            b"opaque fixture"
                        }
                    );
                }
            }
        }
    }
}

#[test]
fn manual_renewal_does_not_invent_a_path_or_a_vendor_home() {
    for cli in [Cli::Claude, Cli::Codex, Cli::Kimi, Cli::Grok] {
        let mut bed = Bed::new(cli);
        let host = Host {
            emails: RefCell::new(Vec::new()),
        };
        let before = bed.records();
        bed.env.config = None;
        assert_eq!(bed.run(false, &host).0, ExitCode::FAILURE);
        assert!(host.emails.borrow().is_empty());
        assert_eq!(bed.records(), before);
        if cli != Cli::Claude {
            bed.env.config = Some(bed.path.with_file_name("aterm.toml"));
            bed.env.home = None;
            assert_eq!(bed.run(false, &host).0, ExitCode::FAILURE);
            assert!(host.emails.borrow().is_empty());
            assert_eq!(bed.records(), before);
        }
    }
}

#[test]
fn due_selection_observes_staging_recovery_without_signing_in_again() {
    for cli in [Cli::Claude, Cli::Codex, Cli::Kimi, Cli::Grok] {
        let mut bed = Bed::new(cli);
        // Recovery correctly rejects checkpoints no newer than the last auth
        // observation. This completed sign-in follows the seeded login end.
        bed.env.now += 1;
        let staged = bed.store.claim_staging_dir("main").unwrap();
        let credential = staged.join(cli.connector().stores[0]);
        std::fs::create_dir_all(credential.parent().unwrap()).unwrap();
        std::fs::write(&credential, b"already authenticated fixture").unwrap();
        accounts::write_renewal_checkpoint_for(
            cli,
            (cli == Cli::Kimi).then_some(crate::ai::connector::Region::Global),
            "main",
            "main@example.test",
            staged.to_str().unwrap(),
            None,
            (cli == Cli::Codex).then_some(accounts::Proof::Status),
            bed.env.now,
            &bed.store.load_logins(bed.env.now).unwrap().0,
        )
        .unwrap();
        let host = Host {
            emails: RefCell::new(Vec::new()),
        };
        let (code, err) = bed.run(true, &host);
        assert_eq!(code, ExitCode::SUCCESS, "{cli:?}: {err}");
        assert!(host.emails.borrow().is_empty());
        assert_eq!(
            bed.store
                .read_account_records(Some(&bed.path), bed.env.now)
                .unwrap()
                .roster
                .row("main")
                .unwrap()
                .dir
                .as_deref(),
            staged.to_str(),
        );
        assert_eq!(
            std::fs::read(&credential).unwrap(),
            b"already authenticated fixture"
        );
    }
}

#[test]
fn scheduling_notes_are_emitted_once_even_when_staging_is_checked() {
    for cli in [Cli::Claude, Cli::Codex, Cli::Kimi, Cli::Grok] {
        for recover_staging in [false, true] {
            let bed = Bed::new(cli);
            std::fs::write(bed.store.machine_file(), "invalid machine identity").unwrap();
            let expected = bed
                .store
                .read_account_records(Some(&bed.path), bed.env.now)
                .unwrap()
                .notes;
            assert_eq!(expected.len(), 1);
            let mut err = Vec::new();
            let got = read_renewal_records(
                cli,
                &bed.store,
                &bed.path,
                bed.env.now,
                if recover_staging {
                    RenewalRecovery::Fresh
                } else {
                    RenewalRecovery::None
                },
                bed.env.home.as_deref(),
                &mut err,
            )
            .unwrap();
            assert_eq!(got.notes, expected);
            let err = String::from_utf8(err).unwrap();
            assert_eq!(err.lines().count(), 1);
            assert!(err.contains(&expected[0]));
        }
    }
}

#[test]
fn native_enrollment_runs_existing_provider_once_and_rejects_retargeted_operation() {
    let bed = Bed::new(Cli::Codex);
    let records = bed
        .store
        .read_account_records(Some(&bed.path), bed.env.now)
        .unwrap();
    let request = crate::harness::codex_enrollment::Request {
        id: "a".repeat(32),
        caller: "b".repeat(64),
        label: "main".into(),
        revision: accounts::pool_authority_revision(&records.raw_roster, &records.logins).unwrap(),
    };
    let host = Host {
        emails: RefCell::new(Vec::new()),
    };
    let original = records.logins.get("main").unwrap().dir.clone();
    let first = enroll_codex_operation(&bed.env, &host, &request).unwrap();
    assert_eq!(host.emails.borrow().as_slice(), ["main@example.test"]);
    assert_eq!(
        first.phase,
        crate::harness::codex_enrollment::Phase::Running,
        "the failed provider observation remains uncertain"
    );
    assert_eq!(
        enroll_codex_operation(&bed.env, &host, &request).unwrap(),
        first
    );
    assert_eq!(
        host.emails.borrow().len(),
        1,
        "poll/restart cannot replay provider work"
    );
    let mut retargeted = request.clone();
    retargeted.label = "other".into();
    assert!(enroll_codex_operation(&bed.env, &host, &retargeted).is_err());
    assert_eq!(
        bed.store
            .load_logins(bed.env.now)
            .unwrap()
            .0
            .get("main")
            .unwrap()
            .dir,
        original
    );
    let cancelled = cancel_codex_operation(&bed.env, &request).unwrap();
    assert_eq!(
        cancelled.phase,
        crate::harness::codex_enrollment::Phase::CancelRequested
    );
    assert_eq!(host.emails.borrow().len(), 1);
    // A fresh runtime scope for an old ID cannot release the old driver's custody.
    let lifetime = crate::harness::account_work::Lifetime::default();
    let (work, control) =
        crate::harness::codex_enrollment::owned_work(request.clone(), lifetime.enter().unwrap());
    assert_eq!(work.run(&bed.env).unwrap(), cancelled);
    assert!(control.retired().is_none());
    assert_eq!(host.emails.borrow().len(), 1);
}

struct SuccessfulEnrollmentHost {
    calls: std::cell::Cell<usize>,
    now: i64,
    held: bool,
    artifact: RefCell<Option<(PathBuf, Vec<u8>)>>,
}

#[cfg(unix)]
struct SealedEnrollmentHost {
    now: i64,
    cut: Option<crate::harness::codex_app_login::EnrollmentCut>,
    calls: std::cell::Cell<usize>,
    stage: RefCell<Option<PathBuf>>,
}

#[cfg(unix)]
impl AccountsHost for SealedEnrollmentHost {
    fn stdin_is_terminal(&self) -> bool {
        false
    }
    fn auth_status(&self, _: &str) -> Result<Captured, String> {
        panic!("native recovery starts no status process")
    }
    fn unaskable(&self) -> Option<String> {
        None
    }
    fn login(&self, _: Option<&str>) -> Result<i32, String> {
        panic!("native recovery starts no login process")
    }
    fn now(&self) -> i64 {
        self.now
    }
    fn codex_account(&self, home: &Path) -> Option<String> {
        crate::harness::codex_app_login::read_account_email(home)
    }
    fn vendor_sign_in(
        &self,
        request: &crate::harness::login_drive::Request<'_>,
    ) -> crate::harness::login_drive::Drove {
        assert_eq!(
            self.calls.replace(self.calls.get() + 1),
            0,
            "the retained request must never repeat its provider effect"
        );
        assert!(
            request.store.signin_now().unwrap().is_none(),
            "the actual original signer owns the provider attempt"
        );
        *self.stage.borrow_mut() = Some(request.staging.to_path_buf());
        let work = self.work_scope();
        crate::harness::codex_app_login::tests::native_enrollment_exchange_fixture(
            request,
            self.cut,
            self.now,
            work.as_ref(),
            "main@example.test",
        )
    }
}

#[cfg(unix)]
fn enrollment_request(bed: &Bed) -> crate::harness::codex_enrollment::Request {
    let records = bed
        .store
        .read_account_records(Some(&bed.path), bed.env.now)
        .unwrap();
    crate::harness::codex_enrollment::Request {
        id: "e".repeat(32),
        caller: "b".repeat(64),
        label: "main".into(),
        revision: accounts::pool_authority_revision(&records.raw_roster, &records.logins).unwrap(),
    }
}

#[cfg(unix)]
#[test]
fn original_native_enrollment_publishes_one_canonical_home_and_unblocks_refresh() {
    use crate::harness::{account_work::Lifetime, codex_enrollment as enrollment};
    let bed = Bed::new(Cli::Codex);
    let request = enrollment_request(&bed);
    let lifetime = Lifetime::default();
    let host = SealedEnrollmentHost {
        now: bed.env.now + 1,
        cut: None,
        calls: std::cell::Cell::new(0),
        stage: RefCell::new(None),
    };
    let (work, _) = enrollment::owned_work(request.clone(), lifetime.enter().unwrap());
    let applied = work.run_with_host(&bed.env, &host).unwrap();
    assert_eq!(applied.phase, enrollment::Phase::Applied);
    assert!(applied.generation.is_some());
    let stage = host.stage.borrow().clone().unwrap();
    assert_eq!(std::fs::canonicalize(&stage).unwrap(), stage);
    let records = bed
        .store
        .read_account_records(Some(&bed.path), bed.env.now)
        .unwrap();
    assert_eq!(
        records.logins.get("main").unwrap().dir.as_deref(),
        stage.to_str()
    );
    assert!(!stage.join("auth.json.refreshed").exists());
    let before = std::fs::read(stage.join("auth.json")).unwrap();
    let (repeat, _) = enrollment::owned_work(request, lifetime.enter().unwrap());
    assert_eq!(repeat.run_with_host(&bed.env, &host).unwrap(), applied);
    assert_eq!(std::fs::read(stage.join("auth.json")).unwrap(), before);
    assert_eq!(host.calls.get(), 1);
    crate::harness::codex_app_login::tests::native_enrollment_refresh_fixture(
        &stage,
        bed.env.now + 2,
    );
    assert!(lifetime.pause().unwrap().try_wait().is_ok());
}

#[cfg(unix)]
#[path = "cli_native_recovery_controls.rs"]
mod native_recovery_controls;

#[cfg(unix)]
#[path = "cli_ordinary_publication_tests.rs"]
mod ordinary_publication;

#[cfg(unix)]
#[test]
fn native_recovery_reuses_only_its_exact_preexisting_completion_header() {
    use crate::harness::{
        account_work::Lifetime, codex_app_login::EnrollmentCut, codex_enrollment as enrollment,
    };
    for matches in [false, true] {
        let bed = Bed::new(Cli::Codex);
        let request = enrollment_request(&bed);
        let lifetime = Lifetime::default();
        let host = SealedEnrollmentHost {
            now: bed.env.now + 1,
            cut: Some(EnrollmentCut::AfterAuthFlush),
            calls: std::cell::Cell::new(0),
            stage: RefCell::new(None),
        };
        let (work, _) = enrollment::owned_work(request.clone(), lifetime.enter().unwrap());
        assert_eq!(
            work.run_with_host(&bed.env, &host).unwrap().phase,
            enrollment::Phase::Running
        );
        let stage = host.stage.borrow().clone().unwrap();
        let signer = bed.store.signin_now().unwrap().unwrap();
        let cut = bed.store.transact_roster_logins_within(
            &accounts::LOCK_LADDER,
            &bed.path,
            bed.env.now,
            &mut None,
            |_, _, logins| {
                signer
                    .check_store(&bed.store)
                    .map_err(|_| "signer changed")?;
                accounts::write_enrollment_checkpoint(
                    &request,
                    if matches {
                        "main@example.test"
                    } else {
                        "other@example.test"
                    },
                    stage.to_str().unwrap(),
                    accounts::Proof::Status,
                    bed.env.now,
                    logins,
                )
                .map_err(|_| "checkpoint write failed")?;
                Err("synthetic cut after the actual completion-header write".into())
            },
        );
        drop(signer);
        assert!(cut.is_err());
        let header = std::fs::read(stage.join(accounts::RENEWAL_CHECKPOINT_FILE)).unwrap();
        let receipt = std::fs::read(stage.join("auth.json.refreshed")).unwrap();
        let (recover, _) = enrollment::owned_work(request.clone(), lifetime.enter().unwrap());
        let result = recover.run_with_host(&bed.env, &host).unwrap();
        assert_eq!(
            result.phase,
            if matches {
                enrollment::Phase::Applied
            } else {
                enrollment::Phase::Running
            }
        );
        assert_eq!(host.calls.get(), 1);
        assert_eq!(
            std::fs::read(stage.join(accounts::RENEWAL_CHECKPOINT_FILE)).unwrap(),
            header
        );
        if matches {
            assert!(result.generation.is_some());
            assert!(!stage.join("auth.json.refreshed").exists());
        } else {
            assert!(result.generation.is_none());
            assert_eq!(
                std::fs::read(stage.join("auth.json.refreshed")).unwrap(),
                receipt
            );
            let records = bed
                .store
                .read_account_records(Some(&bed.path), bed.env.now)
                .unwrap();
            assert_eq!(
                records.logins.get("main").unwrap().dir.as_deref(),
                bed.dirs[0].to_str()
            );
            for _ in 0..2 {
                let (poll, _) = enrollment::owned_work(request.clone(), lifetime.enter().unwrap());
                assert_eq!(poll.run_with_host(&bed.env, &host).unwrap(), result);
                let current = bed
                    .store
                    .read_account_records(Some(&bed.path), bed.env.now)
                    .unwrap();
                assert_eq!(current.logins, records.logins);
            }
            assert_eq!(host.calls.get(), 1);
            assert_eq!(
                std::fs::read(stage.join("auth.json.refreshed")).unwrap(),
                receipt
            );
            assert_eq!(
                std::fs::read(stage.join(accounts::RENEWAL_CHECKPOINT_FILE)).unwrap(),
                header
            );
        }
        assert!(lifetime.pause().unwrap().try_wait().is_ok());
    }
}

#[cfg(unix)]
#[test]
fn a_sealed_native_response_recovers_only_after_original_work_retirement_without_http_replay() {
    use crate::harness::{
        account_work::Lifetime, codex_app_login::EnrollmentCut, codex_enrollment as enrollment,
    };
    for cut in [
        EnrollmentCut::AfterSeal,
        EnrollmentCut::AfterAuthWrite,
        EnrollmentCut::AfterAuthFlush,
    ] {
        let bed = Bed::new(Cli::Codex);
        let request = enrollment_request(&bed);
        let old_auth = std::fs::read(bed.dirs[0].join("auth.json")).unwrap();
        let lifetime = Lifetime::default();
        let scope = lifetime.enter().unwrap();
        let detached = scope.clone();
        let (work, original) = enrollment::owned_work(request.clone(), scope);
        let host = SealedEnrollmentHost {
            now: bed.env.now + 1,
            cut: Some(cut),
            calls: std::cell::Cell::new(0),
            stage: RefCell::new(None),
        };
        assert_eq!(
            work.run_with_host(&bed.env, &host).unwrap().phase,
            enrollment::Phase::Running
        );
        assert!(
            original.retired().is_none(),
            "a real retained scope prevents retirement evidence"
        );
        let stage = host.stage.borrow().clone().unwrap();
        let receipt = stage.join("auth.json.refreshed");
        let exact = std::fs::read(&receipt).unwrap();
        let (replayed, replay_control) =
            enrollment::owned_work(request.clone(), lifetime.enter().unwrap());
        assert_eq!(
            replayed.run_with_host(&bed.env, &host).unwrap().phase,
            enrollment::Phase::Running
        );
        assert!(
            replay_control.retired().is_none(),
            "a fresh replay scope cannot retire the original work"
        );
        assert_eq!(std::fs::read(&receipt).unwrap(), exact);
        assert!(!stage.join(accounts::RENEWAL_CHECKPOINT_FILE).exists());
        assert_eq!(
            std::fs::read(bed.dirs[0].join("auth.json")).unwrap(),
            old_auth
        );
        drop(detached);
        original.retired().unwrap().finish(&bed.env).unwrap();
        let (recover, _) = enrollment::owned_work(request.clone(), lifetime.enter().unwrap());
        let applied = recover.run_with_host(&bed.env, &host).unwrap();
        assert_eq!(applied.phase, enrollment::Phase::Applied);
        assert!(applied.generation.is_some());
        assert_eq!(host.calls.get(), 1);
        let records = bed
            .store
            .read_account_records(Some(&bed.path), bed.env.now)
            .unwrap();
        assert_eq!(
            records.logins.get("main").unwrap().dir.as_deref(),
            stage.to_str()
        );
        assert!(stage.join(accounts::RENEWAL_CHECKPOINT_FILE).is_file());
        assert!(!receipt.exists());
        assert_eq!(
            std::fs::read(bed.dirs[0].join("auth.json")).unwrap(),
            old_auth
        );
        let (repeat, _) = enrollment::owned_work(request, lifetime.enter().unwrap());
        assert_eq!(repeat.run_with_host(&bed.env, &host).unwrap(), applied);
        assert_eq!(host.calls.get(), 1);
        assert!(bed.store.signin_now().unwrap().is_some());
        lifetime
            .pause()
            .unwrap()
            .try_wait()
            .expect("all actual original and recovery resources retired");
    }
}

#[cfg(unix)]
#[test]
fn a_failed_first_native_seal_cannot_claim_a_recoverable_response() {
    use crate::harness::{
        account_work::Lifetime, codex_app_login::EnrollmentCut, codex_enrollment as enrollment,
    };
    let bed = Bed::new(Cli::Codex);
    let old_auth = std::fs::read(bed.dirs[0].join("auth.json")).unwrap();
    let request = enrollment_request(&bed);
    let lifetime = Lifetime::default();
    let host = SealedEnrollmentHost {
        now: bed.env.now + 1,
        cut: Some(EnrollmentCut::BeforeSeal),
        calls: std::cell::Cell::new(0),
        stage: RefCell::new(None),
    };
    let (work, _) = enrollment::owned_work(request.clone(), lifetime.enter().unwrap());
    assert_eq!(
        work.run_with_host(&bed.env, &host).unwrap().phase,
        enrollment::Phase::Running
    );
    let stage = host.stage.borrow().clone().unwrap();
    assert!(!stage.join("auth.json.refreshed").exists());
    assert!(!stage.join(accounts::RENEWAL_CHECKPOINT_FILE).exists());
    let (repeat, _) = enrollment::owned_work(request, lifetime.enter().unwrap());
    if let Ok(status) = repeat.run_with_host(&bed.env, &host) {
        assert_eq!(status.phase, enrollment::Phase::Running);
    }
    assert_eq!(host.calls.get(), 1);
    assert_eq!(
        std::fs::read(bed.dirs[0].join("auth.json")).unwrap(),
        old_auth
    );
    let records = bed
        .store
        .read_account_records(Some(&bed.path), bed.env.now)
        .unwrap();
    assert_eq!(
        records
            .logins
            .enrollment
            .retained_for(&"b".repeat(64))
            .unwrap()[0]
            .1
            .phase,
        enrollment::Phase::Running
    );
    assert!(
        bed.store
            .load_logins(bed.env.now)
            .unwrap()
            .0
            .get("main")
            .unwrap()
            .dir
            .as_deref()
            == bed.dirs[0].to_str()
    );
}

impl AccountsHost for SuccessfulEnrollmentHost {
    fn stdin_is_terminal(&self) -> bool {
        false
    }
    fn auth_status(&self, _: &str) -> Result<Captured, String> {
        panic!("no default auth status")
    }
    fn unaskable(&self) -> Option<String> {
        None
    }
    fn login(&self, _: Option<&str>) -> Result<i32, String> {
        panic!("no default login")
    }
    fn now(&self) -> i64 {
        self.now
    }
    fn renew_flow(&self, _: &str, _: &str, _: &RenewBrowser) -> Result<i32, String> {
        panic!("no desktop browser flow")
    }
    fn vendor_sign_in(
        &self,
        req: &crate::harness::login_drive::Request<'_>,
    ) -> crate::harness::login_drive::Drove {
        self.calls.set(self.calls.get() + 1);
        codex_login(req.staging);
        if self.held {
            *self.artifact.borrow_mut() = Some((
                req.staging.to_path_buf(),
                std::fs::read(req.staging.join("auth.json")).unwrap(),
            ));
            return crate::harness::login_drive::Drove {
                outcome: crate::harness::login_drive::Outcome::CompletedHeld,
                host: None,
            };
        }
        let work = self.work_scope();
        crate::harness::codex_app_login::tests::native_enrollment_exchange_fixture(
            req,
            None,
            self.now,
            work.as_ref(),
            "main@example.test",
        )
    }
}

#[test]
fn native_enrollment_publishes_only_verified_staging_and_returns_committed_generation() {
    let bed = Bed::new(Cli::Codex);
    let records = bed
        .store
        .read_account_records(Some(&bed.path), bed.env.now)
        .unwrap();
    let request = crate::harness::codex_enrollment::Request {
        id: "d".repeat(32),
        caller: "b".repeat(64),
        label: "main".into(),
        revision: accounts::pool_authority_revision(&records.raw_roster, &records.logins).unwrap(),
    };
    let before = records.logins.get("main").unwrap().last_success_generation;
    let host = SuccessfulEnrollmentHost {
        calls: std::cell::Cell::new(0),
        now: bed.env.now + 1,
        held: false,
        artifact: RefCell::new(None),
    };
    let status = enroll_codex_operation(&bed.env, &host, &request).unwrap();
    assert_eq!(host.calls.get(), 1);
    assert_eq!(
        status.phase,
        crate::harness::codex_enrollment::Phase::Applied
    );
    let logins = bed.store.load_logins(host.now).unwrap().0;
    let binding = logins.get("main").unwrap();
    assert!(binding.last_success_generation > before);
    assert_eq!(status.generation, Some(binding.last_success_generation));
    assert_eq!(binding.proved_by, Some(accounts::Proof::Status));
    assert_eq!(
        enroll_codex_operation(&bed.env, &host, &request).unwrap(),
        status
    );
    assert_eq!(host.calls.get(), 1);
}

#[test]
fn native_enrollment_keeps_completed_held_artifact_running_without_a_second_provider_attempt() {
    let bed = Bed::new(Cli::Codex);
    let records = bed
        .store
        .read_account_records(Some(&bed.path), bed.env.now)
        .unwrap();
    let request = crate::harness::codex_enrollment::Request {
        id: "f".repeat(32),
        caller: "b".repeat(64),
        label: "main".into(),
        revision: accounts::pool_authority_revision(&records.raw_roster, &records.logins).unwrap(),
    };
    let original = records.logins.get("main").unwrap().clone();
    let original_auth =
        std::fs::read(Path::new(original.dir.as_ref().unwrap()).join("auth.json")).unwrap();
    let roster_before = std::fs::read(&bed.path).unwrap();
    let host = SuccessfulEnrollmentHost {
        calls: std::cell::Cell::new(0),
        now: bed.env.now + 1,
        held: true,
        artifact: RefCell::new(None),
    };
    let first = enroll_codex_operation(&bed.env, &host, &request).unwrap();
    assert_eq!(
        first.phase,
        crate::harness::codex_enrollment::Phase::Running
    );
    assert_eq!(first.generation, None);
    assert_eq!(
        enroll_codex_operation(&bed.env, &host, &request).unwrap(),
        first
    );
    let mut different = request.clone();
    different.id = "e".repeat(32);
    assert!(enroll_codex_operation(&bed.env, &host, &different).is_err());
    assert_eq!(host.calls.get(), 1);
    let artifact = host.artifact.borrow();
    let (staging, exact) = artifact.as_ref().unwrap();
    assert_eq!(std::fs::read(staging.join("auth.json")).unwrap(), *exact);
    assert!(!staging.join(accounts::RENEWAL_CHECKPOINT_FILE).exists());
    let current = bed.store.load_logins(host.now).unwrap().0;
    let binding = current.get("main").unwrap();
    assert_eq!(binding.dir, original.dir);
    assert_eq!(
        binding.last_success_generation,
        original.last_success_generation
    );
    assert_eq!(binding.sign_in_task, original.sign_in_task);
    assert_eq!(std::fs::read(&bed.path).unwrap(), roster_before);
    assert_eq!(
        std::fs::read(Path::new(binding.dir.as_ref().unwrap()).join("auth.json")).unwrap(),
        original_auth
    );
}

#[cfg(unix)]
#[test]
fn queued_due_reread_recovers_a_completed_checkpoint_with_its_original_signer() {
    struct Queued<'a> {
        bed: &'a Bed,
        held: RefCell<Option<accounts::SigninTurn>>,
        completed: RefCell<Option<PathBuf>>,
    }
    impl AccountsHost for Queued<'_> {
        fn stdin_is_terminal(&self) -> bool {
            true
        }
        fn unaskable(&self) -> Option<String> {
            None
        }
        fn now(&self) -> i64 {
            self.bed.env.now + 1
        }
        fn auth_status(&self, _: &str) -> Result<Captured, String> {
            panic!("completed recovery starts no provider status")
        }
        fn login(&self, _: Option<&str>) -> Result<i32, String> {
            panic!("completed recovery starts no login")
        }
        fn renew_flow(&self, _: &str, _: &str, _: &RenewBrowser) -> Result<i32, String> {
            panic!("completed recovery must not replay provider work")
        }
        fn signin_queued(&self, position: accounts::SigninPosition) {
            if self.held.borrow().is_none() {
                return;
            }
            assert_eq!(position.blocker, accounts::SigninBlocker::UnscheduledSignin);
            let staged = self.bed.store.claim_staging_dir("main").unwrap();
            let credential = staged.join(Cli::Claude.connector().stores[0]);
            std::fs::create_dir_all(credential.parent().unwrap()).unwrap();
            std::fs::write(credential, b"completed local fixture").unwrap();
            accounts::write_renewal_checkpoint_for(
                Cli::Claude,
                None,
                "main",
                "main@example.test",
                staged.to_str().unwrap(),
                None,
                Some(accounts::Proof::Status),
                self.now(),
                &self.bed.store.load_logins(self.now()).unwrap().0,
            )
            .unwrap();
            *self.completed.borrow_mut() = Some(staged);
            drop(self.held.borrow_mut().take());
        }
    }
    let bed = Bed::new(Cli::Claude);
    let host = Queued {
        bed: &bed,
        held: RefCell::new(Some(bed.store.signin_now().unwrap().unwrap())),
        completed: RefCell::new(None),
    };
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = run_accounts_renew(&bed.env, &host, (&[], true, Some(&[])), &mut out, &mut err);
    assert_eq!(code, ExitCode::SUCCESS, "{}", String::from_utf8_lossy(&err));
    assert!(String::from_utf8_lossy(&out).contains(RENEWED_WHILE_WAITING));
    let staged = host
        .completed
        .borrow()
        .clone()
        .expect("actual queue callback completed the receipt");
    let records = bed
        .store
        .read_account_records(Some(&bed.path), host.now())
        .unwrap();
    assert_eq!(
        records.roster.row("main").unwrap().dir.as_deref(),
        staged.to_str()
    );
    assert!(
        bed.store.signin_now().unwrap().is_some(),
        "original queued signer retired"
    );
    let host = Host {
        emails: RefCell::new(Vec::new()),
    };
    assert_eq!(bed.run(true, &host).0, ExitCode::SUCCESS);
    assert!(
        host.emails.borrow().is_empty(),
        "completed checkpoint cannot replay on a later due pass"
    );
}

#[test]
fn enrollment_proven_before_provider_refusal_does_not_keep_uncertain_custody() {
    struct Stopped;
    impl AccountsHost for Stopped {
        fn stdin_is_terminal(&self) -> bool {
            false
        }
        fn work_cancelled(&self) -> bool {
            true
        }
        fn auth_status(&self, _: &str) -> Result<Captured, String> {
            panic!("no provider status")
        }
        fn unaskable(&self) -> Option<String> {
            None
        }
        fn login(&self, _: Option<&str>) -> Result<i32, String> {
            panic!("no provider login")
        }
        fn now(&self) -> i64 {
            1
        }
        fn renew_flow(&self, _: &str, _: &str, _: &RenewBrowser) -> Result<i32, String> {
            panic!("no browser")
        }
        fn vendor_sign_in(
            &self,
            _: &crate::harness::login_drive::Request<'_>,
        ) -> crate::harness::login_drive::Drove {
            panic!("no provider dispatch")
        }
    }
    let bed = Bed::new(Cli::Codex);
    let records = bed
        .store
        .read_account_records(Some(&bed.path), bed.env.now)
        .unwrap();
    let mut request = crate::harness::codex_enrollment::Request {
        id: "d".repeat(32),
        caller: "b".repeat(64),
        label: "main".into(),
        revision: accounts::pool_authority_revision(&records.raw_roster, &records.logins).unwrap(),
    };
    let first = enroll_codex_operation(&bed.env, &Stopped, &request).unwrap();
    assert_eq!(
        first.phase,
        crate::harness::codex_enrollment::Phase::Refused
    );
    assert_eq!(first.generation, None);
    assert_eq!(
        enroll_codex_operation(&bed.env, &Stopped, &request).unwrap(),
        first
    );
    request.id = "e".repeat(32);
    assert_eq!(
        enroll_codex_operation(&bed.env, &Stopped, &request).unwrap(),
        first
    );
    let lifetime = crate::harness::account_work::Lifetime::default();
    let host = LiveAccounts::manual_work(lifetime.enter().unwrap());
    assert!(
        !host.automatic_sign_in(),
        "explicit native management uses the existing manual FIFO"
    );
}

#[test]
fn enrollment_uses_only_typed_no_action_for_provider_route_refusal() {
    struct Typed {
        outcome: crate::harness::login_drive::Outcome,
        calls: std::cell::Cell<usize>,
    }
    impl AccountsHost for Typed {
        fn stdin_is_terminal(&self) -> bool {
            false
        }
        fn auth_status(&self, _: &str) -> Result<Captured, String> {
            panic!("no default status")
        }
        fn unaskable(&self) -> Option<String> {
            None
        }
        fn login(&self, _: Option<&str>) -> Result<i32, String> {
            panic!("no default login")
        }
        fn now(&self) -> i64 {
            0
        }
        fn renew_flow(&self, _: &str, _: &str, _: &RenewBrowser) -> Result<i32, String> {
            panic!("no browser")
        }
        fn vendor_sign_in(
            &self,
            _: &crate::harness::login_drive::Request<'_>,
        ) -> crate::harness::login_drive::Drove {
            self.calls.set(self.calls.get() + 1);
            crate::harness::login_drive::Drove {
                outcome: self.outcome.clone(),
                host: None,
            }
        }
    }
    use crate::harness::{codex_enrollment::Phase, login_drive::Outcome};
    for (outcome, expected) in [
        (
            Outcome::NotRun("original budget expired before native effects".into()),
            Phase::Refused,
        ),
        (
            Outcome::PageNotOpened("provider may already have seen the page request".into()),
            Phase::Running,
        ),
    ] {
        let bed = Bed::new(Cli::Codex);
        let records = bed
            .store
            .read_account_records(Some(&bed.path), bed.env.now)
            .unwrap();
        let request = crate::harness::codex_enrollment::Request {
            id: "f".repeat(32),
            caller: "b".repeat(64),
            label: "main".into(),
            revision: accounts::pool_authority_revision(&records.raw_roster, &records.logins)
                .unwrap(),
        };
        let host = Typed {
            outcome,
            calls: std::cell::Cell::new(0),
        };
        let status = enroll_codex_operation(&bed.env, &host, &request).unwrap();
        assert_eq!(status.phase, expected);
        assert_eq!(status.generation, None);
        assert_eq!(host.calls.get(), 1);
        assert_eq!(
            enroll_codex_operation(&bed.env, &host, &request).unwrap(),
            status
        );
        assert_eq!(host.calls.get(), 1, "exact ID never dispatches twice");
    }
}

#[cfg(unix)]
#[path = "cli_native_owner_recovery_tests.rs"]
mod native_owner_recovery;
