// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::cell::RefCell;

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
        bed
    }

    fn renew_ended(&self, index: usize) -> Result<Vec<accounts::Event>, String> {
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
                events.extend(accounts::note_login_end(
                    state,
                    logins,
                    "main",
                    self.env.now,
                ));
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
                    assert_eq!(*host.emails.borrow(), ["main@example.test"]);
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
                for dir in &bed.dirs {
                    assert_eq!(
                        std::fs::read(dir.join(cli.connector().stores[0])).unwrap(),
                        b"opaque fixture"
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
                recover_staging,
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
