// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::cell::{Cell, RefCell};
use std::sync::Arc;
use std::time::Instant;

const EMAIL: &str = "canonical@example.com";

struct Native<'a> {
    store: &'a accounts::Store,
    roster: &'a Path,
    raw: &'a accounts::Row,
    now: i64,
    case: u8,
    calls: Cell<u32>,
    superseded: Cell<bool>,
    after: RefCell<Option<accounts::Logins>>,
}

impl AccountsHost for Native<'_> {
    fn stdin_is_terminal(&self) -> bool {
        false
    }
    fn auth_status(&self, _: &str) -> Result<Captured, String> {
        panic!("Claude status")
    }
    fn unaskable(&self) -> Option<String> {
        None
    }
    fn login(&self, _: Option<&str>) -> Result<i32, String> {
        panic!("native renewal started login")
    }
    fn now(&self) -> i64 {
        if self.case == 10 && self.calls.get() > 0 && !self.superseded.replace(true) {
            let before = self
                .store
                .read_account_records(Some(self.roster), self.now)
                .unwrap();
            let row = before.roster.row(&self.raw.label).unwrap();
            let key = crate::harness::login_drive::RecoveryKey(
                accounts::RenewalParent::capture(&before.logins, &row.label)
                    .recovery_scope(row, &before.state, &before.logins)
                    .unwrap(),
            );
            let token = accounts::current_workflow(self.raw, &before.logins).unwrap();
            let mut changed = self.raw.clone();
            changed.email = Some("replacement@example.com".into());
            for policy in [&changed, self.raw] {
                aterm_secrets::fsx::write_atomic(
                    self.roster,
                    accounts::row_text(policy).unwrap().as_bytes(),
                    0o600,
                )
                .unwrap();
                self.store
                    .transact_pool_logins_within(
                        &accounts::LOCK_LADDER,
                        self.roster,
                        self.now,
                        &mut None,
                        |_, _, logins| {
                            accounts::admit_workflow(logins, &self.raw.label)?;
                            Ok(Vec::new())
                        },
                    )
                    .unwrap();
            }
            let after = self
                .store
                .read_account_records(Some(self.roster), self.now)
                .unwrap();
            let row = after.roster.row(&self.raw.label).unwrap();
            pool_admission::snapshot_current(row, &after.state, &after.logins, key).unwrap();
            assert_ne!(
                accounts::current_workflow(self.raw, &after.logins),
                Some(token)
            );
            *self.after.borrow_mut() = Some(after.logins);
        }
        self.now
    }
    fn renews_kimi_profiles(&self) -> bool {
        true
    }
    fn sign_in_program(&self, _: Cli) -> Option<PathBuf> {
        Some(PathBuf::from("/missing-native-kimi"))
    }
    fn vendor_sign_in(
        &self,
        _: &crate::harness::login_drive::Request<'_>,
    ) -> crate::harness::login_drive::Drove {
        panic!("published native profile repeated device authentication")
    }
    fn kimi_native_profile(
        &self,
        home: &Path,
        region: crate::ai::connector::Region,
        expected: &str,
        _: i64,
        _: Instant,
        guard: Arc<dyn aterm_http::Guard>,
    ) -> Result<Option<Box<dyn shared_provider::LoginProof>>, shared_provider::ObservationError>
    {
        self.calls.set(self.calls.get() + 1);
        assert_eq!(expected, EMAIL, "provider identity, not browser alias");
        assert!(guard.is_authorized());
        let proof = shared_provider::kimi_pending_proof_fixture(
            home,
            region,
            if self.case == 6 {
                "another@example.com"
            } else {
                EMAIL
            },
            false,
        );
        match self.case {
            1 => {
                let mut changed = self.raw.clone();
                changed.held_by = Some("other-machine".into());
                aterm_secrets::fsx::write_atomic(
                    self.roster,
                    accounts::row_text(&changed).unwrap().as_bytes(),
                    0o600,
                )
                .unwrap();
            }
            2 => {
                self.store
                    .transact_logins(self.now, &mut None, |state, logins| {
                        Ok(accounts::note_renewed(
                            state,
                            logins,
                            &self.raw.label,
                            None,
                            home.to_str().unwrap(),
                            None,
                            self.now,
                        ))
                    })
                    .unwrap();
            }
            3 => {
                self.store
                    .transact_logins(self.now, &mut None, |state, _| {
                        let account = state.accounts.entry(self.raw.label.clone()).or_default();
                        account.auth = Auth::SignedOut;
                        account.auth_at = Some(self.now);
                        Ok(Vec::new())
                    })
                    .unwrap();
            }
            7 => {
                self.store
                    .transact_logins(self.now, &mut None, |_, logins| {
                        logins
                            .labels
                            .entry("other".into())
                            .or_default()
                            .was
                            .push(Some(home.to_str().unwrap().into()));
                        Ok(Vec::new())
                    })
                    .unwrap();
            }
            8 => {
                self.store
                    .transact_logins(self.now, &mut None, |_, logins| {
                        logins
                            .labels
                            .entry("other".into())
                            .or_default()
                            .was
                            .push(None);
                        Ok(Vec::new())
                    })
                    .unwrap();
            }
            9 => {
                self.store
                    .transact_logins(self.now, &mut None, |_, logins| {
                        logins
                            .shared
                            .begin(
                                accounts::shared::Intent {
                                    source: accounts::shared::Source {
                                        network: [1; 32],
                                        account: [2; 32],
                                    },
                                    decision: [3; 32],
                                    generation: 1,
                                    configuration: [4; 32],
                                    request: [6; 16],
                                    label: "protected".into(),
                                    identity: "protected@example.com".into(),
                                    staging: home.to_str().unwrap().into(),
                                    before_generation: None,
                                    begun_at: self.now,
                                },
                                Some([5; 32]),
                            )
                            .unwrap();
                        Ok(Vec::new())
                    })
                    .unwrap();
            }
            _ => {}
        }
        if self.case == 4 {
            let credential = shared_provider::kimi_credential_path(home, region);
            aterm_secrets::fsx::write_atomic(&credential, b"changed-after-proof", 0o600).unwrap();
        }
        *self.after.borrow_mut() = Some(self.store.load_logins(self.now).unwrap().0);
        if self.case == 5 {
            return Err(shared_provider::ObservationError::Unavailable(
                "canonical outage".into(),
            ));
        }
        if matches!(self.case, 1..=3 | 7 | 9)
            || (self.case == 8 && self.store.default_home().as_deref() == Some(home))
        {
            assert!(!guard.is_authorized());
        }
        Ok(Some(Box::new(proof)))
    }
}

#[test]
fn published_kimi_profiles_renew_in_place_and_refuse_changed_authority_without_device_login() {
    for default_profile in [false, true] {
        for case in 0..11 {
            let tmp = tmp_dir("kimi-native-profile");
            let mut env = env_at(&tmp);
            env.now = aterm_time::unix_secs()
                .and_then(|n| i64::try_from(n).ok())
                .unwrap();
            let store = env.accounts_store_for(Cli::Kimi).unwrap();
            let path = env.accounts_roster_for(Cli::Kimi).unwrap();
            let dir = if default_profile {
                store.default_home().unwrap()
            } else {
                store.claim_staging_dir("one").unwrap()
            };
            drop(shared_provider::kimi_pending_proof_fixture(
                &dir,
                crate::ai::connector::Region::Global,
                EMAIL,
                true,
            ));
            let raw = accounts::Row {
                label: "one".into(),
                email: Some("browser-alias@example.com".into()),
                identity_email: Some(EMAIL.into()),
                vendor: accounts::Vendor {
                    region: Some(crate::ai::connector::Region::Global),
                    ..Default::default()
                },
                ..Default::default()
            };
            aterm_secrets::fsx::ensure_private_dir(path.parent().unwrap()).unwrap();
            aterm_secrets::fsx::write_atomic(
                &path,
                accounts::row_text(&raw).unwrap().as_bytes(),
                0o600,
            )
            .unwrap();
            store
                .transact_logins(env.now, &mut None, |state, logins| {
                    if default_profile {
                        logins.labels.entry(raw.label.clone()).or_default();
                        return Ok(Vec::new());
                    }
                    let events = accounts::note_renewed(
                        state,
                        logins,
                        &raw.label,
                        None,
                        dir.to_str().unwrap(),
                        None,
                        env.now,
                    );
                    accounts::note_identity(
                        logins,
                        &raw.label,
                        accounts::Proof::Status,
                        Some(EMAIL),
                    );
                    Ok(events)
                })
                .unwrap();
            let records = store.read_account_records(Some(&path), env.now).unwrap();
            let row = records.roster.row(&raw.label).unwrap();
            let before = records.logins.get(&raw.label).unwrap().clone();
            let host = Native {
                store: &store,
                roster: &path,
                raw: &raw,
                now: env.now,
                case,
                calls: Cell::new(0),
                superseded: Cell::new(false),
                after: RefCell::new(None),
            };
            let mut out = Vec::new();
            let mut err = Vec::new();
            let result = renew_one_for(
                Cli::Kimi,
                env.now,
                &host,
                &store,
                (row, &path, &records.logins),
                (
                    None,
                    crate::harness::sign_in_ladder::asked_fixture(Cli::Kimi, &row.label),
                ),
                &mut out,
                &mut err,
            );
            assert_eq!(
                host.calls.get(),
                1,
                "case {case}: {}",
                String::from_utf8_lossy(&err)
            );
            let after = store.load_logins(env.now).unwrap().0;
            if case == 10 {
                assert!(
                    host.superseded.get(),
                    "post-guard workflow replacement was reached"
                );
            }
            if case == 0 || (case == 8 && !default_profile) {
                assert_eq!(
                    result,
                    Renewal::Renewed,
                    "{}",
                    String::from_utf8_lossy(&err)
                );
                let current = after.get(&raw.label).unwrap();
                assert_eq!(current.dir.as_deref(), dir.to_str());
                assert_eq!(current.was, before.was, "no superseded profile was created");
                assert!(current.renewal_generation > before.renewal_generation);
                assert_eq!(current.proved_email.as_deref(), Some(EMAIL));
                assert_eq!(current.proved_by, Some(accounts::Proof::Status));
            } else {
                assert_ne!(result, Renewal::Renewed);
                assert_eq!(
                    after,
                    host.after.borrow().clone().unwrap(),
                    "failed publication changed a binding"
                );
            }
            assert!(!String::from_utf8_lossy(&out).contains("NEEDS YOU"));
            assert!(dir.is_dir(), "the live profile is retained");
            assert_eq!(
                std::fs::read_dir(store.logins_dir()).unwrap().count(),
                usize::from(!default_profile),
                "unused stage removed"
            );
        }
    }
}

#[test]
fn an_ambiguous_default_kimi_credential_defers_before_any_login_or_refresh() {
    let tmp = tmp_dir("kimi-default-ambiguous");
    let mut env = env_at(&tmp);
    env.now = aterm_time::unix_secs()
        .and_then(|n| i64::try_from(n).ok())
        .unwrap();
    let store = env.accounts_store_for(Cli::Kimi).unwrap();
    let path = env.accounts_roster_for(Cli::Kimi).unwrap();
    let home = store.default_home().unwrap();
    aterm_secrets::fsx::ensure_private_dir(&home.join("credentials")).unwrap();
    let credential =
        shared_provider::kimi_credential_path(&home, crate::ai::connector::Region::Global);
    let target = home.join("synthetic-token");
    aterm_secrets::fsx::write_atomic(&target, b"synthetic", 0o600).unwrap();
    std::os::unix::fs::symlink(&target, &credential).unwrap();
    let raw = accounts::Row {
        label: "one".into(),
        email: Some(EMAIL.into()),
        vendor: accounts::Vendor {
            region: Some(crate::ai::connector::Region::Global),
            ..Default::default()
        },
        ..Default::default()
    };
    aterm_secrets::fsx::ensure_private_dir(path.parent().unwrap()).unwrap();
    aterm_secrets::fsx::write_atomic(&path, accounts::row_text(&raw).unwrap().as_bytes(), 0o600)
        .unwrap();
    let records = store.read_account_records(Some(&path), env.now).unwrap();
    let host = Native {
        store: &store,
        roster: &path,
        raw: &raw,
        now: env.now,
        case: 0,
        calls: Cell::new(0),
        superseded: Cell::new(false),
        after: RefCell::new(None),
    };
    let mut err = Vec::new();
    let result = renew_one_for(
        Cli::Kimi,
        env.now,
        &host,
        &store,
        (records.roster.row("one").unwrap(), &path, &records.logins),
        (
            None,
            crate::harness::sign_in_ladder::asked_fixture(Cli::Kimi, "one"),
        ),
        &mut Vec::new(),
        &mut err,
    );
    assert_eq!(
        result,
        Renewal::Deferred,
        "{}",
        String::from_utf8_lossy(&err)
    );
    assert_eq!(host.calls.get(), 0);
    assert!(String::from_utf8_lossy(&err).contains("not a regular file"));
    assert_eq!(std::fs::read(&target).unwrap(), b"synthetic");
    assert!(
        std::fs::symlink_metadata(&credential)
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

#[test]
fn default_kimi_discovery_distinguishes_missing_credentials_from_dangling_paths() {
    for case in 0..5 {
        let tmp = tmp_dir("kimi-default-paths");
        let env = env_at(&tmp);
        let store = env.accounts_store_for(Cli::Kimi).unwrap();
        let home = store.default_home().unwrap();
        aterm_secrets::fsx::ensure_private_dir(home.parent().unwrap()).unwrap();
        if case == 1 {
            std::os::unix::fs::symlink(home.parent().unwrap().join("missing"), &home).unwrap();
        } else {
            aterm_secrets::fsx::ensure_private_dir(&home).unwrap();
            let credentials = home.join("credentials");
            if case == 0 {
                std::os::unix::fs::symlink(home.join("missing"), &credentials).unwrap();
            } else if case != 3 {
                aterm_secrets::fsx::ensure_private_dir(&credentials).unwrap();
                if case == 2 {
                    let file = shared_provider::kimi_credential_path(
                        &home,
                        crate::ai::connector::Region::Global,
                    );
                    std::os::unix::fs::symlink(home.join("missing"), file).unwrap();
                }
            }
        }
        let row = accounts::Row {
            label: "one".into(),
            identity_email: Some(EMAIL.into()),
            vendor: accounts::Vendor {
                region: Some(crate::ai::connector::Region::Global),
                ..Default::default()
            },
            ..Default::default()
        };
        let found =
            crate::harness::cli::pool_kimi::profile(&store, &row, &accounts::Logins::default());
        if case < 3 {
            assert!(found.is_err(), "case {case}");
        } else {
            assert!(found.unwrap().is_none(), "case {case}");
        }
    }
}

struct Reauthenticate<'a> {
    old: &'a Path,
    browser: &'a Path,
    case: u8,
    calls: Cell<u32>,
    stage: RefCell<Option<PathBuf>>,
}

impl AccountsHost for Reauthenticate<'_> {
    fn stdin_is_terminal(&self) -> bool {
        false
    }
    fn auth_status(&self, _: &str) -> Result<Captured, String> {
        panic!("Claude status")
    }
    fn unaskable(&self) -> Option<String> {
        None
    }
    fn login(&self, _: Option<&str>) -> Result<i32, String> {
        panic!("interactive login")
    }
    fn now(&self) -> i64 {
        2000
    }
    fn renews_kimi_profiles(&self) -> bool {
        true
    }
    fn machine_var(&self, name: &str) -> Option<OsString> {
        (name == "ATERM_BROWSER" && self.case != 3).then(|| self.browser.as_os_str().to_owned())
    }
    fn system_browsers(&self) -> bool {
        false
    }
    fn sign_in_program(&self, _: Cli) -> Option<PathBuf> {
        (self.case == 4).then(|| PathBuf::from("/missing-kimi-reauthentication"))
    }
    fn kimi_native_profile(
        &self,
        _: &Path,
        _: crate::ai::connector::Region,
        _: &str,
        _: i64,
        _: Instant,
        guard: Arc<dyn aterm_http::Guard>,
    ) -> Result<Option<Box<dyn shared_provider::LoginProof>>, shared_provider::ObservationError>
    {
        assert!(guard.is_authorized());
        Err(match self.case {
            1 => shared_provider::ObservationError::RecoveryRequired("fresh rejection".into()),
            2 => shared_provider::ObservationError::Unavailable("temporary outage".into()),
            _ => shared_provider::ObservationError::ReauthenticationRequired(
                "prior rejection".into(),
            ),
        })
    }
    fn vendor_sign_in(
        &self,
        req: &crate::harness::login_drive::Request<'_>,
    ) -> crate::harness::login_drive::Drove {
        use crate::harness::{browser_window::ProfileLock, login_drive};
        self.calls.set(self.calls.get() + 1);
        req.check_admission().unwrap();
        *self.stage.borrow_mut() = Some(req.staging.to_owned());
        let proof = shared_provider::kimi_pending_proof_fixture(
            req.staging,
            crate::ai::connector::Region::Global,
            EMAIL,
            true,
        );
        let claim = Arc::new(ProfileLock::claim(req.staging).unwrap());
        req.admission.unwrap().retain_kimi(proof, claim).unwrap();
        if self.case == 6 {
            let credential = shared_provider::kimi_credential_path(
                self.old,
                crate::ai::connector::Region::Global,
            );
            aterm_secrets::fsx::write_atomic(&credential, b"changed-source-after-new-proof", 0o600)
                .unwrap();
            assert!(req.check_admission().is_err());
        }
        login_drive::Drove {
            outcome: login_drive::Outcome::SignedIn {
                email: Some(EMAIL.into()),
                page: false,
            },
            host: None,
        }
    }
}

#[test]
fn rejected_kimi_profile_reauthenticates_only_with_current_evidence_and_prerequisites() {
    for default_profile in [false, true] {
        for case in 0..9 {
            let tmp = tmp_dir("kimi-reauthentication");
            let mut env = env_at(&tmp);
            env.now = 2000;
            let store = env.accounts_store_for(Cli::Kimi).unwrap();
            let roster = env.accounts_roster_for(Cli::Kimi).unwrap();
            let old = if default_profile {
                store.default_home().unwrap()
            } else {
                store.claim_staging_dir("one").unwrap()
            };
            let region = crate::ai::connector::Region::Global;
            drop(shared_provider::kimi_pending_proof_fixture(
                &old, region, EMAIL, true,
            ));
            if case != 5 {
                shared_provider::kimi_rejection_fixture(&old, region, EMAIL);
            }
            let row = accounts::Row {
                label: "one".into(),
                identity_email: Some(EMAIL.into()),
                vendor: accounts::Vendor {
                    region: Some(region),
                    ..Default::default()
                },
                ..Default::default()
            };
            aterm_secrets::fsx::ensure_private_dir(roster.parent().unwrap()).unwrap();
            aterm_secrets::fsx::write_atomic(
                &roster,
                accounts::row_text(&row).unwrap().as_bytes(),
                0o600,
            )
            .unwrap();
            store
                .transact_logins(env.now, &mut None, |state, logins| {
                    if default_profile {
                        logins.labels.entry("one".into()).or_default();
                        return Ok(Vec::new());
                    }
                    let events = accounts::note_renewed(
                        state,
                        logins,
                        "one",
                        None,
                        old.to_str().unwrap(),
                        None,
                        env.now,
                    );
                    accounts::note_identity(logins, "one", accounts::Proof::Status, Some(EMAIL));
                    Ok(events)
                })
                .unwrap();
            if case == 7 || case == 8 {
                store
                    .transact_pool_logins_within(
                        &[],
                        &roster,
                        env.now,
                        &mut None,
                        |raw, _, logins| {
                            assert_eq!(raw.row("one"), Some(&row));
                            accounts::admit_workflow(logins, "one")?;
                            if case == 7 {
                                accounts::note_sign_in_task(
                                    logins,
                                    "one",
                                    accounts::SignInAction::Browser,
                                    env.now,
                                );
                                Ok(Vec::new())
                            } else {
                                Ok(vec![
                                    accounts::Event::new(env.now, accounts::Kind::SignIn, "one")
                                        .because("provider browser bot-check"),
                                ])
                            }
                        },
                    )
                    .unwrap();
                let logins = store.load_logins(env.now).unwrap().0;
                let recorded = if case == 7 {
                    crate::harness::accounts_sync::unaddressed_sign_in_task_at(
                        &store, &logins, &row,
                    )
                } else {
                    crate::harness::accounts_sync::bot_check_on_record_at(&store, &row, &logins)
                };
                assert_eq!(recorded, Some(env.now), "CONTROL: retry hold is current");
            }
            let browser = tmp.path().join("synthetic-browser");
            std::fs::write(&browser, b"never executed").unwrap();
            let host = Reauthenticate {
                old: &old,
                browser: &browser,
                case,
                calls: Cell::new(0),
                stage: RefCell::new(None),
            };
            let records = store.read_account_records(Some(&roster), env.now).unwrap();
            let before = records.logins.get("one").unwrap().clone();
            let mut err = Vec::new();
            let result = renew_one_for(
                Cli::Kimi,
                env.now,
                &host,
                &store,
                (records.roster.row("one").unwrap(), &roster, &records.logins),
                (
                    None,
                    crate::harness::sign_in_ladder::asked_fixture(Cli::Kimi, "one"),
                ),
                &mut Vec::new(),
                &mut err,
            );
            let after = store.load_logins(env.now).unwrap().0;
            let current = after.get("one").unwrap();
            assert_eq!(
                host.calls.get(),
                u32::from(case == 0 || case == 6),
                "case {case}, default={default_profile}: {}",
                String::from_utf8_lossy(&err)
            );
            if case == 0 {
                assert_eq!(
                    result,
                    Renewal::Renewed,
                    "{}",
                    String::from_utf8_lossy(&err)
                );
                assert_ne!(current.dir, before.dir);
                assert_eq!(
                    current.dir.as_deref(),
                    host.stage.borrow().as_ref().and_then(|p| p.to_str())
                );
                assert_eq!(current.proved_email.as_deref(), Some(EMAIL));
                assert!(old.is_dir(), "old profile retained");
            } else {
                assert_ne!(result, Renewal::Renewed, "case {case}");
                assert_eq!(current.dir, before.dir, "failure changed live binding");
                if case == 6 {
                    assert!(
                        host.stage.borrow().as_ref().unwrap().is_dir(),
                        "unpublished new credential is retained"
                    );
                }
            }
        }
    }
}
