// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::harness::cli::shared_provider::kimi_pending_proof_fixture;
use crate::harness::{accounts, google_browser, google_browser_store};
use std::cell::Cell;

const REGISTRY: &[u8] = b"[[identities]]\nid='one'\nemail='person@example.com'\n[[bindings]]\nprovider='kimi-global'\naccount='one'\nidentity='one'\nexpected_email='person@example.com'\n";
const COOKIES: &[u8] = br#"[{"name":"fixture","value":"pending-kimi-cookie","domain":".google.com","path":"/","secure":true}]"#;

struct Fixture {
    state: PathBuf,
    home: PathBuf,
    store: accounts::Store,
    row: accounts::Row,
}
impl Fixture {
    fn at(root: &Path) -> Self {
        let state = root.join("state");
        let home = root.join("home");
        for path in [&state, &home] {
            aterm_secrets::fsx::ensure_private_dir(path).unwrap();
        }
        aterm_secrets::fsx::write_atomic(&state.join(REGISTRY_FILE), REGISTRY, 0o600).unwrap();
        let store = accounts::Store::at_for(&state, Cli::Kimi, &home);
        let row = accounts::Row {
            label: "one".into(),
            email: Some("person@example.com".into()),
            vendor: accounts::Vendor {
                region: Some(Region::Global),
                ..Default::default()
            },
            ..Default::default()
        };
        Self {
            state,
            home,
            store,
            row,
        }
    }
    fn stage(&self, name: &str) -> PathBuf {
        let path = self.state.join(name);
        aterm_secrets::fsx::ensure_private_dir(&path).unwrap();
        path
    }
    fn request<'a>(&'a self, stage: &'a Path, key: u8) -> login_drive::Request<'a> {
        let route = Cli::Kimi
            .accounts()
            .signin
            .as_ref()
            .unwrap()
            .route(Some(crate::ai::connector::Flow::Device))
            .unwrap();
        login_drive::Request {
            recovery: Some(login_drive::RecoveryKey([key; 32])),
            admission: None,
            cli: Cli::Kimi,
            route,
            row: &self.row,
            staging: stage,
            store: &self.store,
            hosts: route.hosts.of(Some(Region::Global)).unwrap(),
        }
    }
    fn identity(&self) -> Identity {
        parse_registry(REGISTRY)
            .unwrap()
            .resolve("kimi-global", "one")
            .unwrap()
            .0
            .clone()
    }
    fn record(&self) -> PathBuf {
        let shared = self.state.join("pending-native-kimi-google-shared");
        let directory = if shared.exists() {
            shared
        } else {
            self.state.join("pending-native-kimi-google")
        };
        let entries = std::fs::read_dir(directory)
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(entries.len(), 1);
        entries[0].path().join("candidate.age")
    }
    fn pending(&self, journal: &Journal) -> PendingFederation {
        PendingFederation {
            scope: journal.scope.clone(),
            native: None,
            session: Some(google_browser::pending_fixture(
                &self.identity(),
                &self.state,
                &self.home,
                COOKIES,
            )),
        }
    }
    fn save(&self, stage: &Path) {
        self.save_for(stage, false);
    }
    fn save_root(&self, stage: &Path) {
        self.save_for(stage, true);
    }
    fn save_for(&self, stage: &Path, root: bool) {
        let admission =
            login_drive::RecoveryAdmission::new(login_drive::RecoveryKey([4; 32]), false, |_| {
                Ok(())
            });
        let request = login_drive::Request {
            admission: root.then_some(&admission),
            ..self.request(stage, 4)
        };
        let mut journal = Journal::open(&request, &self.home).unwrap().unwrap();
        drop(kimi_pending_proof_fixture(
            stage,
            Region::Global,
            "person@example.com",
            true,
        ));
        let pending = self.pending(&journal);
        journal
            .capture(&request, Some(&pending), &self.home)
            .unwrap();
    }
    fn save_expired_pool(&self, stage: &Path) {
        let request = self.request(stage, 4);
        let mut journal = Journal::open(&request, &self.home).unwrap().unwrap();
        drop(kimi_pending_proof_fixture(
            stage,
            Region::Global,
            "person@example.com",
            true,
        ));
        expire_kimi_pending(stage, Region::Global).unwrap();
        let pending = self.pending(&journal);
        journal
            .capture(&request, Some(&pending), &self.home)
            .unwrap();
    }
    fn no_cookies(&self) {
        let identity = self.identity();
        let claim = identity.claim(&self.state).unwrap();
        assert!(
            google_browser_store::load(&self.home, &identity, &claim)
                .unwrap()
                .is_none()
        );
    }
}

fn outcome() -> login_drive::Outcome {
    login_drive::Outcome::SignedIn {
        email: Some("person@example.com".into()),
        page: false,
    }
}
fn deadline() -> login_drive::Deadline<'static> {
    login_drive::Deadline::after(Duration::from_secs(60))
}

#[test]
fn recovered_kimi_results_keep_the_admitted_work_with_or_without_cookie_state() {
    for cookies in [false, true] {
        let root = aterm_tempfile::tempdir().unwrap();
        let f = Fixture::at(root.path());
        let first = f.stage("first");
        {
            let request = f.request(&first, 4);
            let mut journal = Journal::open(&request, &f.home).unwrap().unwrap();
            drop(kimi_pending_proof_fixture(
                &first,
                Region::Global,
                "person@example.com",
                true,
            ));
            let pending = cookies.then(|| f.pending(&journal));
            journal
                .capture(&request, pending.as_ref(), &f.home)
                .unwrap();
        }
        // This encrypted checkpoint is local synthetic data. No browser, endpoint or
        // Google cookie publication runs: cancellation must precede the verify callback.
        let record = std::fs::read(f.record()).unwrap();
        std::fs::remove_dir_all(&first).unwrap();
        let stage = f.stage("resumed");
        let request = f.request(&stage, 4);
        let lifetime = crate::harness::account_work::Lifetime::default();
        let mut journal = Journal::open_scoped(&request, &f.home, lifetime.enter())
            .unwrap()
            .unwrap();
        assert!(journal.candidate.is_some());
        let mut pending = journal
            .resume_using(&request, &f.home, deadline(), |scope, _| {
                Ok(Some(kimi_pending_proof_fixture(
                    &stage,
                    scope.region,
                    &scope.expected,
                    false,
                )))
            })
            .unwrap()
            .unwrap();
        assert_eq!(pending.session.is_some(), cookies);
        assert!(
            pending.scope.current().is_ok(),
            "unchanged admitted recovery is current"
        );
        // A later caller cannot rebind a retained result to a different live operation.
        let other = crate::harness::account_work::Lifetime::default();
        journal.retain_work(other.enter());
        pending.retain_work(other.enter());
        assert!(
            other
                .pause()
                .unwrap()
                .try_wait()
                .expect("no replacement scope retained")
                .resume()
                .is_ok()
        );
        drop(journal);
        let pause = lifetime
            .pause()
            .unwrap()
            .try_wait()
            .expect_err("the recovered result retains its original operation");
        let result = pending.complete_using(&request, &outcome(), deadline(), |_| {
            panic!("cancelled recovery cannot verify or publish")
        });
        assert!(result.is_err());
        let drained = pause.try_wait().expect("all recovered claims retired");
        assert_eq!(std::fs::read(f.record()).unwrap(), record);
        assert!(crate::harness::browser_window::ProfileLock::claim(&stage).is_ok());
        assert!(drained.resume().is_ok());
    }
}

#[test]
fn closed_kimi_work_preserves_completed_native_receipt_without_admitting_publication() {
    let root = aterm_tempfile::tempdir().unwrap();
    let f = Fixture::at(root.path());
    let stage = f.stage("completed");
    let request = f.request(&stage, 4);
    let lifetime = crate::harness::account_work::Lifetime::default();
    let mut journal = Journal::open_scoped(&request, &f.home, lifetime.enter())
        .unwrap()
        .unwrap();
    drop(kimi_pending_proof_fixture(
        &stage,
        Region::Global,
        "person@example.com",
        true,
    ));
    let pause = lifetime
        .pause()
        .unwrap()
        .try_wait()
        .expect_err("completed effect still owned");
    journal
        .capture(&request, None, &f.home)
        .expect("exact completed native receipt retained");
    assert!(
        journal.check(&request).is_err(),
        "receipt is not permission to publish"
    );
    assert!(
        journal.pending(&f.home).is_err(),
        "closed work cannot produce a publishable result"
    );
    let record = std::fs::read(f.record()).unwrap();
    drop(journal);
    let drained = pause.try_wait().expect("receipt resources retired");
    assert!(drained.resume().is_ok());
    let recovered = Journal::open_scoped(&request, &f.home, lifetime.enter())
        .unwrap()
        .unwrap();
    assert!(
        recovered.candidate.is_some(),
        "next admitted operation can see completed evidence"
    );
    assert_eq!(std::fs::read(f.record()).unwrap(), record);
    drop(recovered);
    assert!(
        lifetime
            .pause()
            .unwrap()
            .try_wait()
            .unwrap()
            .resume()
            .is_ok()
    );
}

#[test]
fn admitted_pool_resume_renews_retained_completion_and_keeps_proof_custody_until_publication() {
    let root = aterm_tempfile::tempdir().unwrap();
    let f = Fixture::at(root.path());
    let first = f.stage("first");
    f.save_expired_pool(&first);
    let old_record = std::fs::read(f.record()).unwrap();
    std::fs::remove_dir_all(first).unwrap();
    let stage = f.stage("pool-resumed");
    let admission =
        login_drive::RecoveryAdmission::pool(login_drive::RecoveryKey([4; 32]), |_| Ok(()));
    assert!(!admission.is_shared_root());
    {
        let req = login_drive::Request {
            admission: Some(&admission),
            ..f.request(&stage, 4)
        };
        let mut journal = Journal::open(&req, &f.home).unwrap().unwrap();
        assert!(
            journal.candidate.is_some(),
            "a current pooled capability must preserve its old pool namespace"
        );
        let pending = journal.resume_using(&req, &f.home, deadline(), |scope, now| {
            f.no_cookies();
            let (_, restored) = kimi_pending_files(&stage, scope.region).unwrap();
            assert!(kimi_pending_expiry(restored.as_slice()).unwrap() <= now);
            let path = kimi_credential_path(&stage, scope.region);
            aterm_secrets::fsx::write_atomic(&path, br#"{"access_token":"rotated","refresh_token":"rotated-refresh","token_type":"Bearer","expires_at":4000000001}"#, 0o600).unwrap();
            Ok(Some(kimi_pending_proof_fixture(&stage, scope.region, &scope.expected, false)))
        }).unwrap().unwrap();
        assert!(journal.resumed);
        assert_ne!(std::fs::read(f.record()).unwrap(), old_record);
        pending
            .complete(&req, &outcome(), deadline(), Some(&journal))
            .unwrap();
        assert!(
            admission
                .kimi_principal(&stage, Region::Global, "person@example.com")
                .unwrap()
                .is_some()
        );
    }
    assert!(
        crate::harness::browser_window::ProfileLock::claim(&stage).is_err(),
        "publication capability must retain the native claim after the driver returns"
    );
    aterm_secrets::fsx::write_atomic(
        &kimi_credential_path(&stage, Region::Global),
        b"another-writer",
        0o600,
    )
    .unwrap();
    assert!(
        admission
            .kimi_principal(&stage, Region::Global, "person@example.com")
            .is_err()
    );
    drop(admission);
    assert!(crate::harness::browser_window::ProfileLock::claim(&stage).is_ok());
}

#[test]
fn pooled_expired_recovery_preserves_rotation_across_outage_and_never_restores_revoked_grants() {
    for rejected in [false, true] {
        let root = aterm_tempfile::tempdir().unwrap();
        let f = Fixture::at(root.path());
        let first = f.stage("first");
        f.save_expired_pool(&first);
        let before = std::fs::read(f.record()).unwrap();
        std::fs::remove_dir_all(first).unwrap();
        let admission =
            login_drive::RecoveryAdmission::pool(login_drive::RecoveryKey([4; 32]), |_| Ok(()));
        let stage = f.stage("resumed");
        let rotated = br#"{"access_token":"rotated-access","refresh_token":"rotated-refresh","token_type":"Bearer","expires_at":4000000001}"#;
        {
            let req = login_drive::Request {
                admission: Some(&admission),
                ..f.request(&stage, 4)
            };
            let mut journal = Journal::open(&req, &f.home).unwrap().unwrap();
            struct Recover<'a> {
                journal: &'a mut Journal,
                home: &'a Path,
                rotated: &'a [u8],
                rejected: bool,
                calls: usize,
            }
            impl login_drive::Approver for Recover<'_> {
                fn resume(
                    &mut self,
                    req: &login_drive::Request<'_>,
                    deadline: login_drive::Deadline<'_>,
                ) -> Result<login_drive::Resume, String> {
                    self.journal.resume_using(req, self.home, deadline, |scope, now| {
                        self.calls += 1;
                        let (_, restored) = kimi_pending_files(req.staging, scope.region).unwrap();
                        assert!(kimi_pending_expiry(restored.as_slice()).unwrap() <= now);
                        aterm_secrets::fsx::write_atomic(
                            &kimi_credential_path(req.staging, scope.region),
                            if self.rejected {
                                br#"{"access_token":"","refresh_token":"","token_type":"Bearer","expires_at":0,"expires_in":0}"#
                            } else {
                                self.rotated
                            },
                            0o600,
                        ).unwrap();
                        Err(if self.rejected { KimiNativeError::RefreshRejected } else { KimiNativeError::Temporary })
                    })?;
                    panic!("unverified renewal was treated as a native login seed")
                }
                fn begin(&mut self, _: &login_drive::Request<'_>) -> Result<(), String> {
                    panic!("renewal failure repeated native authentication")
                }
                fn approve(&mut self, _: &Handoff<'_>) -> Approval {
                    panic!("renewal failure requested browser approval")
                }
            }
            let mut recover = Recover {
                journal: &mut journal,
                home: &f.home,
                rotated,
                rejected,
                calls: 0,
            };
            let result = login_drive::sign_in_with(
                &req,
                (Path::new("/missing-native-kimi"), &[]),
                None,
                None,
                &|_| None,
                &mut recover,
                Duration::from_secs(60),
            );
            assert!(matches!(
                result.outcome,
                login_drive::Outcome::Unverified(_)
            ));
            assert_eq!(recover.calls, 1);
            assert!(!journal.resumed);
            assert!(
                admission
                    .kimi_principal(&stage, Region::Global, "person@example.com")
                    .unwrap()
                    .is_none()
            );
        }
        assert_ne!(std::fs::read(f.record()).unwrap(), before);
        f.no_cookies();
        // A new process may have a new staging directory. Its only surviving
        // refresh authority is the encrypted checkpoint, never the old seed.
        std::fs::remove_dir_all(&stage).unwrap();
        let stage = f.stage("after-restart");
        let req = login_drive::Request {
            admission: Some(&admission),
            ..f.request(&stage, 4)
        };
        let mut journal = Journal::open(&req, &f.home).unwrap().unwrap();
        let pending = journal
            .resume_using(&req, &f.home, deadline(), |scope, _| {
                assert!(
                    !rejected,
                    "a rejected grant must never reach another exchange"
                );
                assert_eq!(
                    std::fs::read(kimi_credential_path(&stage, scope.region)).unwrap(),
                    rotated
                );
                Ok(Some(kimi_pending_proof_fixture(
                    &stage,
                    scope.region,
                    &scope.expected,
                    false,
                )))
            })
            .unwrap();
        if rejected {
            assert!(pending.is_none());
            assert!(!kimi_credential_path(&stage, Region::Global).exists());
            f.no_cookies();
        } else {
            pending
                .unwrap()
                .complete(&req, &outcome(), deadline(), Some(&journal))
                .unwrap();
            assert!(
                admission
                    .kimi_principal(&stage, Region::Global, "person@example.com")
                    .unwrap()
                    .is_some()
            );
            let identity = f.identity();
            let claim = identity.claim(&f.state).unwrap();
            assert_eq!(
                google_browser_store::load(&f.home, &identity, &claim)
                    .unwrap()
                    .unwrap()
                    .as_slice(),
                COOKIES
            );
        }
    }
}

#[test]
fn pooled_capability_cannot_read_a_shared_root_completion() {
    let root = aterm_tempfile::tempdir().unwrap();
    let f = Fixture::at(root.path());
    f.save_root(&f.stage("root"));
    let retained = std::fs::read(f.record()).unwrap();
    let stage = f.stage("pool");
    let admission =
        login_drive::RecoveryAdmission::pool(login_drive::RecoveryKey([4; 32]), |_| Ok(()));
    let req = login_drive::Request {
        admission: Some(&admission),
        ..f.request(&stage, 4)
    };
    let mut journal = Journal::open(&req, &f.home).unwrap().unwrap();
    assert!(
        journal
            .resume_using(&req, &f.home, deadline(), |_, _| panic!(
                "shared-root completion crossed into pooled recovery"
            ))
            .unwrap()
            .is_none()
    );
    assert_eq!(std::fs::read(f.record()).unwrap(), retained);
    assert!(!kimi_credential_path(&stage, Region::Global).exists());
    f.no_cookies();
}

#[test]
fn root_renewal_restores_only_its_exact_predecessor_and_rechecks_canonical_identity() {
    use crate::harness::cli::shared_provider::ObservationError;
    for case in 0..6 {
        let root = aterm_tempfile::tempdir().unwrap();
        let f = Fixture::at(root.path());
        let stage = f.stage("retained");
        f.save_root(&stage);
        let credential = kimi_credential_path(&stage, Region::Global);
        let predecessor = std::fs::read(&credential).unwrap();
        let admission =
            login_drive::RecoveryAdmission::new(login_drive::RecoveryKey([4; 32]), false, |_| {
                Ok(())
            });
        let req = login_drive::Request {
            admission: Some(&admission),
            ..f.request(&stage, 4)
        };
        let renewed = br#"{"access_token":"renewed-access","refresh_token":"renewed-refresh","token_type":"Bearer","expires_at":4000000001}"#;
        {
            let mut journal = Journal::open(&req, &f.home).unwrap().unwrap();
            aterm_secrets::fsx::write_atomic(&credential, renewed, 0o600).unwrap();
            let pending = f.pending(&journal);
            journal.capture(&req, Some(&pending), &f.home).unwrap();
        }
        // Simulate a native credential rename lost at the crash boundary after
        // the encrypted replacement and its predecessor digest became durable.
        aterm_secrets::fsx::write_atomic(&credential, &predecessor, 0o600).unwrap();
        if case == 2 {
            aterm_secrets::fsx::write_atomic(&credential, b"unrelated-writer", 0o600).unwrap();
        }
        if case == 3 {
            aterm_secrets::fsx::write_atomic(
                &stage.join("config.toml"),
                b"different-config",
                0o600,
            )
            .unwrap();
        }
        let live = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let authority = live.clone();
        let checked = login_drive::RecoveryAdmission::new(
            login_drive::RecoveryKey([if case == 5 { 5 } else { 4 }; 32]),
            false,
            move |_| {
                if !authority.load(std::sync::atomic::Ordering::SeqCst) {
                    Err("authority withdrawn".into())
                } else {
                    Ok(())
                }
            },
        );
        let req = login_drive::Request {
            admission: Some(&checked),
            recovery: Some(login_drive::RecoveryKey(
                [if case == 5 { 5 } else { 4 }; 32],
            )),
            ..req
        };
        let mut journal = Journal::open(&req, &f.home).unwrap().unwrap();
        if case == 4 {
            live.store(false, std::sync::atomic::Ordering::SeqCst);
        }
        let calls = Cell::new(0);
        let observed = journal.observe_existing_using(&req, &f.home, deadline(), |scope, _| {
            calls.set(calls.get() + 1);
            assert_eq!(std::fs::read(&credential).unwrap(), renewed);
            if case == 1 {
                Err(KimiNativeError::Temporary)
            } else {
                Ok(Some(kimi_pending_proof_fixture(
                    &stage,
                    scope.region,
                    &scope.expected,
                    false,
                )))
            }
        });
        match case {
            0 => {
                observed.unwrap().unwrap().check_current().unwrap();
            }
            1 => assert!(matches!(observed, Err(ObservationError::Unavailable(_)))),
            4 => assert!(matches!(observed, Err(ObservationError::Unavailable(_)))),
            _ => assert!(observed.unwrap().is_none()),
        }
        assert_eq!(calls.get(), i32::from(case < 2));
        if case != 0 {
            f.no_cookies();
        } else {
            let identity = f.identity();
            let claim = identity.claim(&f.state).unwrap();
            assert_eq!(
                google_browser_store::load(&f.home, &identity, &claim)
                    .unwrap()
                    .unwrap()
                    .as_slice(),
                COOKIES
            );
        }
        if case >= 3 {
            assert_eq!(std::fs::read(&credential).unwrap(), predecessor);
        }
        if case == 2 {
            assert_eq!(std::fs::read(&credential).unwrap(), b"unrelated-writer");
        }
    }
}

#[test]
fn legacy_completion_has_no_authority_to_replace_an_existing_credential() {
    let scope = [8; 32];
    let key = login_drive::RecoveryKey([4; 32]);
    let mut bytes = PREFIX.to_vec();
    bytes.extend_from_slice(&scope);
    bytes.extend_from_slice(&key.0);
    bytes.push(0);
    for field in [b"config".as_slice(), b"credential", b"cookies"] {
        bytes.extend_from_slice(&(field.len() as u32).to_be_bytes());
        bytes.extend_from_slice(field);
    }
    let candidate = decode(&bytes, scope, key).unwrap().unwrap();
    assert!(candidate.predecessor.is_none());
    bytes.push(1);
    assert!(decode(&bytes, scope, key).is_err());
}

#[test]
fn renewed_grant_and_quarantined_cookies_survive_outage_but_revocation_never_restores_old_grant() {
    use crate::harness::cli::shared_provider::ObservationError;
    for case in 0..3 {
        let root = aterm_tempfile::tempdir().unwrap();
        let f = Fixture::at(root.path());
        let stage = f.stage("retained");
        f.save_root(&stage);
        let admission =
            login_drive::RecoveryAdmission::new(login_drive::RecoveryKey([4; 32]), false, |_| {
                Ok(())
            });
        let req = login_drive::Request {
            admission: Some(&admission),
            ..f.request(&stage, 4)
        };
        let credential = kimi_credential_path(&stage, Region::Global);
        let rotated = br#"{"access_token":"rotated-access","refresh_token":"rotated-refresh","token_type":"Bearer","expires_at":4000000001}"#;
        let mut journal = Journal::open(&req, &f.home).unwrap().unwrap();
        let result = journal.observe_existing_using(&req, &f.home, deadline(), |scope, _| {
            if case == 2 {
                aterm_secrets::fsx::write_atomic(&credential, br#"{"access_token":"","refresh_token":"","token_type":"Bearer","expires_at":0,"expires_in":0,"scope":""}"#, 0o600).unwrap();
                return Err(KimiNativeError::RefreshRejected);
            }
            aterm_secrets::fsx::write_atomic(&credential, rotated, 0o600).unwrap();
            if case == 1 { Err(KimiNativeError::Temporary) }
            else { Ok(Some(kimi_pending_proof_fixture(&stage, scope.region, &scope.expected, false))) }
        });
        match case {
            0 => {
                result.unwrap().unwrap().check_current().unwrap();
            }
            1 => assert!(matches!(result, Err(ObservationError::Unavailable(_)))),
            _ => assert!(matches!(result, Err(ObservationError::RecoveryRequired(_)))),
        }
        drop(journal);
        if case != 0 {
            f.no_cookies();
        }
        std::fs::remove_file(&credential).unwrap();
        let mut journal = Journal::open(&req, &f.home).unwrap().unwrap();
        let proof = journal
            .observe_existing_using(&req, &f.home, deadline(), |scope, _| {
                assert_ne!(
                    case, 2,
                    "a terminal checkpoint must never restore the old native seed"
                );
                assert_eq!(std::fs::read(&credential).unwrap(), rotated);
                Ok(Some(kimi_pending_proof_fixture(
                    &stage,
                    scope.region,
                    &scope.expected,
                    false,
                )))
            })
            .unwrap();
        if case == 2 {
            assert!(proof.is_none());
            assert!(!credential.exists());
            f.no_cookies();
        } else {
            proof.unwrap().check_current().unwrap();
            let identity = f.identity();
            let claim = identity.claim(&f.state).unwrap();
            assert_eq!(
                google_browser_store::load(&f.home, &identity, &claim)
                    .unwrap()
                    .unwrap()
                    .as_slice(),
                COOKIES
            );
        }
    }
}

#[test]
fn kimi_pending_completion_revalidates_without_native_auth_and_commits_only_after_proof() {
    let root = aterm_tempfile::tempdir().unwrap();
    let f = Fixture::at(root.path());
    let first = f.stage("first");
    f.save(&first);
    f.no_cookies();
    let before = std::fs::read(f.record()).unwrap();
    for secret in [b"fixture-refresh".as_slice(), b"pending-kimi-cookie"] {
        assert!(!before.windows(secret.len()).any(|bytes| bytes == secret));
    }
    std::fs::remove_dir_all(first).unwrap();
    let stage = f.stage("resumed");
    let req = f.request(&stage, 4);
    let mut journal = Journal::open(&req, &f.home).unwrap().unwrap();
    let calls = Cell::new(0);
    let pending = journal
        .resume_using(&req, &f.home, deadline(), |scope, _| {
            calls.set(calls.get() + 1);
            f.no_cookies();
            Ok(Some(kimi_pending_proof_fixture(
                &stage,
                scope.region,
                &scope.expected,
                false,
            )))
        })
        .unwrap()
        .unwrap();
    assert_eq!(calls.get(), 1);
    journal.capture(&req, Some(&pending), &f.home).unwrap();
    pending
        .complete(&req, &outcome(), deadline(), Some(&journal))
        .unwrap();
    assert_eq!(
        std::fs::read(f.record()).unwrap(),
        before,
        "verification must retain the completion until pool publication"
    );
    let identity = f.identity();
    let claim = identity.claim(&f.state).unwrap();
    assert_eq!(
        google_browser_store::load(&f.home, &identity, &claim)
            .unwrap()
            .unwrap()
            .as_slice(),
        COOKIES
    );
}

#[test]
fn kimi_recovery_refuses_changed_context_files_and_ciphertext_without_cookie_publication() {
    for case in 0..9 {
        let root = aterm_tempfile::tempdir().unwrap();
        let f = Fixture::at(root.path());
        f.save(&f.stage("first"));
        let record = f.record();
        let before = std::fs::read(&record).unwrap();
        let stage = f.stage("resumed");
        let req = f.request(&stage, if case == 0 { 9 } else { 4 });
        if case == 1 {
            aterm_secrets::fsx::write_atomic(&stage.join("config.toml"), b"another-owner", 0o600)
                .unwrap();
        }
        if case == 2 {
            let path = f.state.join(REGISTRY_FILE);
            let text = std::str::from_utf8(REGISTRY).unwrap().replace(
                "email='person@example.com'\n[[bindings]]",
                "email='person@example.com'\ncredential_ref='another-key'\n[[bindings]]",
            );
            aterm_secrets::fsx::write_atomic(&path, text.as_bytes(), 0o600).unwrap();
        }
        let mut journal = Journal::open(&req, &f.home).unwrap().unwrap();
        let result = journal.resume_using(
            &req,
            &f.home,
            if case == 3 {
                login_drive::Deadline::after(Duration::ZERO)
            } else {
                deadline()
            },
            |scope, _| {
                assert!(
                    case >= 4,
                    "unavailable, stale or nonempty staging contacted Kimi"
                );
                if case == 4 {
                    aterm_secrets::fsx::write_atomic(&record, b"changed", 0o600).unwrap();
                }
                if case == 5 {
                    aterm_secrets::fsx::write_atomic(&stage.join("config.toml"), b"changed", 0o600)
                        .unwrap();
                    return Err(KimiNativeError::Temporary);
                }
                if case == 6 {
                    return Err(KimiNativeError::Temporary);
                }
                if case == 8 {
                    return Err(KimiNativeError::Refused(
                        "canonical account mismatch".into(),
                    ));
                }
                Ok(Some(kimi_pending_proof_fixture(
                    &stage,
                    scope.region,
                    if case == 7 {
                        "other@example.com"
                    } else {
                        &scope.expected
                    },
                    false,
                )))
            },
        );
        if case == 0 || case == 2 {
            assert!(matches!(result, Ok(None)));
            assert!(!stage.join("config.toml").exists());
        } else {
            assert!(result.is_err(), "case {case}");
        }
        f.no_cookies();
        if case == 8 {
            assert!(!record.exists());
            continue;
        }
        assert_eq!(
            std::fs::read(&record).unwrap(),
            if case == 4 {
                b"changed".as_slice()
            } else {
                before.as_slice()
            }
        );
    }
}

#[test]
fn kimi_rejected_or_expired_pending_token_keeps_refresh_for_the_native_client() {
    for expired in [false, true] {
        let root = aterm_tempfile::tempdir().unwrap();
        let f = Fixture::at(root.path());
        let stage = f.stage("first");
        let req = f.request(&stage, 4);
        let mut journal = Journal::open(&req, &f.home).unwrap().unwrap();
        drop(kimi_pending_proof_fixture(
            &stage,
            Region::Global,
            "person@example.com",
            true,
        ));
        if expired {
            expire_kimi_pending(&stage, Region::Global).unwrap();
        }
        let pending = f.pending(&journal);
        journal.capture(&req, Some(&pending), &f.home).unwrap();
        drop(pending);
        drop(journal);
        let before = std::fs::read(f.record()).unwrap();
        let next = f.stage("next");
        let req = f.request(&next, 4);
        let mut journal = Journal::open(&req, &f.home).unwrap().unwrap();
        let result = journal
            .resume_using(&req, &f.home, deadline(), |_, _| {
                assert!(!expired);
                Err(KimiNativeError::Rejected)
            })
            .unwrap();
        assert!(result.is_none());
        assert!(journal.has_native_seed());
        let bytes = std::fs::read(kimi_credential_path(&next, Region::Global)).unwrap();
        let token: Value = aterm_json::from_slice(&bytes).unwrap();
        assert_eq!(token["refresh_token"].as_str(), Some("fixture-refresh"));
        assert_eq!(token["expires_at"].as_i64(), Some(0));
        assert_eq!(std::fs::read(f.record()).unwrap(), before);
        f.no_cookies();
        // Native refresh may finish without a new browser handoff. Its canonical
        // proof can still authorize the quarantined Google cookies from this scope.
        let native = kimi_pending_proof_fixture(&next, Region::Global, "person@example.com", true);
        journal.capture(&req, None, &f.home).unwrap();
        let pending = journal.pending(&f.home).unwrap();
        pending
            .complete_checked(&req, &outcome(), deadline(), Some(&journal), |_| Ok(native))
            .unwrap();
        let identity = f.identity();
        let claim = identity.claim(&f.state).unwrap();
        assert_eq!(
            google_browser_store::load(&f.home, &identity, &claim)
                .unwrap()
                .unwrap()
                .as_slice(),
            COOKIES
        );
    }
}

#[test]
fn kimi_journal_rejects_unpinned_native_config_before_persisting_a_refresh_seed() {
    let root = aterm_tempfile::tempdir().unwrap();
    let f = Fixture::at(root.path());
    let stage = f.stage("first");
    let req = f.request(&stage, 4);
    let mut journal = Journal::open(&req, &f.home).unwrap().unwrap();
    drop(kimi_pending_proof_fixture(
        &stage,
        Region::Global,
        "person@example.com",
        true,
    ));
    let config = std::fs::read_to_string(stage.join("config.toml"))
        .unwrap()
        .replace(
            "https://api.kimi.ai/coding/v1",
            "https://untrusted.invalid/coding/v1",
        );
    aterm_secrets::fsx::write_atomic(&stage.join("config.toml"), config.as_bytes(), 0o600).unwrap();
    assert!(journal.capture(&req, None, &f.home).is_err());
    assert!(!f.record().exists());
}

#[test]
fn kimi_capture_cannot_rebind_an_older_google_attempt_to_a_new_admitted_parent() {
    let root = aterm_tempfile::tempdir().unwrap();
    let f = Fixture::at(root.path());
    let stage = f.stage("first");
    let req = f.request(&stage, 4);
    let mut journal = Journal::open(&req, &f.home).unwrap().unwrap();
    drop(kimi_pending_proof_fixture(
        &stage,
        Region::Global,
        "person@example.com",
        true,
    ));
    let mut pending = f.pending(&journal);
    pending.scope.attempt.recovery = Some(login_drive::RecoveryKey([8; 32]));
    assert!(journal.capture(&req, Some(&pending), &f.home).is_err());
    assert!(!f.record().exists());
    drop(pending);
    f.no_cookies();
}

#[test]
fn kimi_pending_crash_stand_in() {
    let Some(root) = std::env::var_os("ATERM_TEST_KIMI_PENDING_ROOT").map(PathBuf::from) else {
        return;
    };
    let f = Fixture::at(&root);
    let stage = f.stage("first");
    let req = f.request(&stage, 4);
    let mut journal = Journal::open(&req, &f.home).unwrap().unwrap();
    drop(kimi_pending_proof_fixture(
        &stage,
        Region::Global,
        "person@example.com",
        true,
    ));
    let pending = f.pending(&journal);
    journal.capture(&req, Some(&pending), &f.home).unwrap();
    std::fs::write(root.join("completed"), b"ready").unwrap();
    aterm_uds::process::kill_self_now();
}

#[test]
fn kimi_pending_survives_process_death_and_resumes_before_native_program_requirements() {
    use std::os::unix::process::ExitStatusExt;
    let root = aterm_tempfile::tempdir().unwrap();
    let f = Fixture::at(root.path());
    let mut child = aterm_uds::spawnfd::command(std::env::current_exe().unwrap());
    child
        .args([
            "harness::google_federation::recovery::tests::kimi_pending_crash_stand_in",
            "--exact",
            "--nocapture",
        ])
        .env("ATERM_TEST_KIMI_PENDING_ROOT", root.path());
    let output = aterm_uds::deadline::output_within(&mut child, Duration::from_secs(60)).unwrap();
    assert_eq!(
        output.status.signal(),
        Some(libc::SIGKILL),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(root.path().join("completed").is_file());
    std::fs::remove_dir_all(f.state.join("first")).unwrap();
    let stage = f.stage("resumed");
    let req = f.request(&stage, 4);
    struct Recover<'a> {
        f: &'a Fixture,
        journal: Option<Journal>,
        pending: Option<PendingFederation>,
    }
    impl login_drive::Approver for Recover<'_> {
        fn resume(
            &mut self,
            req: &login_drive::Request<'_>,
            deadline: login_drive::Deadline<'_>,
        ) -> Result<login_drive::Resume, String> {
            let mut journal = Journal::open(req, &self.f.home)?.unwrap();
            self.pending = journal.resume_using(req, &self.f.home, deadline, |scope, _| {
                Ok(Some(kimi_pending_proof_fixture(
                    req.staging,
                    scope.region,
                    &scope.expected,
                    false,
                )))
            })?;
            assert!(self.pending.is_some());
            self.journal = Some(journal);
            Ok(login_drive::Resume::Completed(outcome()))
        }
        fn begin(&mut self, _: &login_drive::Request<'_>) -> Result<(), String> {
            panic!("restart repeated native authentication")
        }
        fn approve(&mut self, _: &Handoff<'_>) -> Approval {
            panic!("restart requested browser approval")
        }
        fn complete(
            &mut self,
            req: &login_drive::Request<'_>,
            outcome: &login_drive::Outcome,
            deadline: login_drive::Deadline<'_>,
        ) -> Result<(), String> {
            self.pending
                .take()
                .unwrap()
                .complete(req, outcome, deadline, self.journal.as_ref())
        }
    }
    let mut recover = Recover {
        f: &f,
        journal: None,
        pending: None,
    };
    let result = login_drive::sign_in_with(
        &req,
        (Path::new("/missing-native-kimi"), &[]),
        None,
        None,
        &|_| None,
        &mut recover,
        Duration::from_secs(60),
    );
    assert!(matches!(
        result.outcome,
        login_drive::Outcome::SignedIn { .. }
    ));
    assert!(f.record().is_file());
}

#[cfg(target_os = "macos")]
#[test]
#[ignore = "contacts managed Kimi anonymously under NO_DESK; ATERM_TEST_KIMI names its pinned binary"]
fn expired_kimi_journal_seed_reaches_native_device_auth_without_browser_approval() {
    let program = std::env::var_os("ATERM_TEST_KIMI")
        .map(PathBuf::from)
        .unwrap();
    let root = aterm_tempfile::tempdir().unwrap();
    let f = Fixture::at(root.path());
    let first = f.stage("first");
    let req = f.request(&first, 4);
    let mut journal = Journal::open(&req, &f.home).unwrap().unwrap();
    drop(kimi_pending_proof_fixture(
        &first,
        Region::Global,
        "person@example.com",
        true,
    ));
    expire_kimi_pending(&first, Region::Global).unwrap();
    journal.capture(&req, None, &f.home).unwrap();
    drop(journal);
    let before = std::fs::read(f.record()).unwrap();
    let next = f.stage("next");
    let req = f.request(&next, 4);
    let mut journal = Journal::open(&req, &f.home).unwrap().unwrap();
    assert!(
        journal
            .resume_using(&req, &f.home, deadline(), |_, _| panic!(
                "expired token reached canonical proof"
            ))
            .unwrap()
            .is_none()
    );
    struct Stop {
        handed: bool,
    }
    impl login_drive::Approver for Stop {
        fn approve(&mut self, handoff: &Handoff<'_>) -> Approval {
            assert!(handoff.code.is_some());
            assert!(login_drive::pinned(handoff.url, handoff.hosts));
            self.handed = true;
            Approval::NotRun("anonymous expired-seed probe stops before approval".into())
        }
    }
    let mut stop = Stop { handed: false };
    let result = login_drive::sign_in_with(
        &req,
        (&program, &[]),
        None,
        Some(Path::new(login_drive::SANDBOX_EXEC)),
        &|_| None,
        &mut stop,
        Duration::from_secs(60),
    );
    assert!(
        stop.handed,
        "native Kimi did not reach device authorization: {:?}",
        result.outcome
    );
    assert!(matches!(
        result.outcome,
        login_drive::Outcome::Unverified(_)
    ));
    assert_eq!(std::fs::read(f.record()).unwrap(), before);
    f.no_cookies();
}

#[test]
fn root_retained_kimi_observation_uses_exact_files_and_live_authority_before_cookies() {
    use crate::harness::cli::shared_provider::ObservationError;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    for case in 0..=6 {
        let root = aterm_tempfile::tempdir().unwrap();
        let f = Fixture::at(root.path());
        let stage = f.stage("retained");
        f.save_root(&stage);
        let record = std::fs::read(f.record()).unwrap();
        let credential_path = kimi_credential_path(&stage, Region::Global);
        let credential = std::fs::read(&credential_path).unwrap();
        let config = std::fs::read(stage.join("config.toml")).unwrap();
        let live = Arc::new(AtomicBool::new(true));
        let checked = live.clone();
        let key = if case == 5 { 5 } else { 4 };
        let admission = login_drive::RecoveryAdmission::new(
            login_drive::RecoveryKey([key; 32]),
            false,
            move |principal| {
                if !checked.load(Ordering::SeqCst) || (case == 3 && principal.is_some()) {
                    Err("root admission no longer permits this native result".into())
                } else {
                    Ok(())
                }
            },
        );
        let req = login_drive::Request {
            admission: Some(&admission),
            ..f.request(&stage, key)
        };
        // A temporary canonical outage must survive dropping and reopening the journal.
        {
            let mut journal = Journal::open(&req, &f.home).unwrap().unwrap();
            if case != 5 {
                assert!(matches!(
                    journal.observe_existing_using(&req, &f.home, deadline(), |_, _| Err(
                        KimiNativeError::Temporary
                    )),
                    Err(ObservationError::Unavailable(_))
                ));
            }
        }
        assert_eq!(std::fs::read(f.record()).unwrap(), record);
        assert_eq!(std::fs::read(&credential_path).unwrap(), credential);
        f.no_cookies();
        let newer = br#"[{"name":"fixture","value":"newer-session","domain":".google.com","path":"/","secure":true}]"#;
        if case == 6 {
            let identity = f.identity();
            let claim = identity.claim(&f.state).unwrap();
            google_browser_store::save(&f.home, &identity, &claim, newer).unwrap();
        }
        let mut journal = Journal::open(&req, &f.home).unwrap().unwrap();
        if case == 1 {
            live.store(false, Ordering::SeqCst);
        }
        let calls = Cell::new(0);
        let result = journal.observe_existing_using(&req, &f.home, deadline(), |scope, _| {
            calls.set(calls.get() + 1);
            let proof = kimi_pending_proof_fixture(&stage, scope.region, &scope.expected, false);
            if case == 2 {
                live.store(false, Ordering::SeqCst);
            }
            if case == 4 {
                aterm_secrets::fsx::write_atomic(
                    &credential_path,
                    b"changed-native-credential",
                    0o600,
                )
                .unwrap();
            }
            Ok(Some(proof))
        });
        if case == 0 || case == 6 {
            result.unwrap().unwrap().check_current().unwrap();
            let identity = f.identity();
            let claim = identity.claim(&f.state).unwrap();
            let cookies = google_browser_store::load(&f.home, &identity, &claim)
                .unwrap()
                .unwrap();
            assert_eq!(
                cookies.as_slice(),
                if case == 6 { newer.as_slice() } else { COOKIES }
            );
        } else if case == 5 {
            assert!(result.unwrap().is_none());
            f.no_cookies();
        } else {
            assert!(result.is_err());
            f.no_cookies();
        }
        assert_eq!(calls.get(), if case == 1 || case == 5 { 0 } else { 1 });
        assert_eq!(std::fs::read(f.record()).unwrap(), record);
        assert_eq!(std::fs::read(stage.join("config.toml")).unwrap(), config);
        if case != 4 {
            assert_eq!(std::fs::read(&credential_path).unwrap(), credential);
        }
    }
}

#[test]
fn root_recovery_key_must_match_the_live_admission() {
    let root = aterm_tempfile::tempdir().unwrap();
    let f = Fixture::at(root.path());
    let stage = f.stage("retained");
    f.save_root(&stage);
    let record = std::fs::read(f.record()).unwrap();
    let admission =
        login_drive::RecoveryAdmission::new(login_drive::RecoveryKey([5; 32]), false, |_| Ok(()));
    let req = login_drive::Request {
        admission: Some(&admission),
        ..f.request(&stage, 4)
    };
    assert!(Journal::open(&req, &f.home).is_err());
    assert_eq!(std::fs::read(f.record()).unwrap(), record);
    f.no_cookies();
}

#[test]
fn root_retained_recovery_restores_only_missing_native_files_without_replacement() {
    for case in 0..5 {
        let root = aterm_tempfile::tempdir().unwrap();
        let f = Fixture::at(root.path());
        let stage = f.stage("retained");
        f.save_root(&stage);
        let record = std::fs::read(f.record()).unwrap();
        let config = stage.join("config.toml");
        let credential = kimi_credential_path(&stage, Region::Global);
        let config_before = std::fs::read(&config).unwrap();
        let credential_before = std::fs::read(&credential).unwrap();
        if case == 0 || case == 1 {
            std::fs::remove_file(&config).unwrap();
        }
        if case == 0 || case == 2 {
            std::fs::remove_file(&credential).unwrap();
        }
        if case == 3 {
            aterm_secrets::fsx::write_atomic(&config, b"another-native-config", 0o600).unwrap();
        }
        if case == 4 {
            aterm_secrets::fsx::write_atomic(&credential, b"another-native-credential", 0o600)
                .unwrap();
        }
        let admission =
            login_drive::RecoveryAdmission::new(login_drive::RecoveryKey([4; 32]), false, |_| {
                Ok(())
            });
        let req = login_drive::Request {
            admission: Some(&admission),
            ..f.request(&stage, 4)
        };
        let mut journal = Journal::open(&req, &f.home).unwrap().unwrap();
        let calls = Cell::new(0);
        let result = journal
            .observe_existing_using(&req, &f.home, deadline(), |scope, _| {
                calls.set(calls.get() + 1);
                Ok(Some(kimi_pending_proof_fixture(
                    &stage,
                    scope.region,
                    &scope.expected,
                    false,
                )))
            })
            .unwrap();
        assert_eq!(calls.get(), i32::from(case < 3));
        assert_eq!(result.is_some(), case < 3);
        assert_eq!(std::fs::read(f.record()).unwrap(), record);
        assert_eq!(
            std::fs::read(&config).unwrap().as_slice(),
            if case == 3 {
                b"another-native-config"
            } else {
                config_before.as_slice()
            }
        );
        assert_eq!(
            std::fs::read(&credential).unwrap().as_slice(),
            if case == 4 {
                b"another-native-credential"
            } else {
                credential_before.as_slice()
            }
        );
        if case >= 3 {
            f.no_cookies();
        }
    }
}

#[test]
fn root_completion_cannot_be_recovered_as_an_unguarded_pool_completion() {
    let root = aterm_tempfile::tempdir().unwrap();
    let f = Fixture::at(root.path());
    let stage = f.stage("retained");
    f.save_root(&stage);
    let record = std::fs::read(f.record()).unwrap();
    let unguarded = f.request(&stage, 4);
    {
        let mut journal = Journal::open(&unguarded, &f.home).unwrap().unwrap();
        assert!(
            journal
                .resume_using(&unguarded, &f.home, deadline(), |_, _| panic!(
                    "root completion escaped its admission"
                ))
                .unwrap()
                .is_none()
        );
    }
    assert_eq!(std::fs::read(f.record()).unwrap(), record);
    f.no_cookies();
    let admission =
        login_drive::RecoveryAdmission::new(login_drive::RecoveryKey([4; 32]), false, |_| Ok(()));
    let guarded = login_drive::Request {
        admission: Some(&admission),
        ..unguarded
    };
    let mut journal = Journal::open(&guarded, &f.home).unwrap().unwrap();
    assert!(
        journal
            .observe_existing_using(&guarded, &f.home, deadline(), |scope, _| Ok(Some(
                kimi_pending_proof_fixture(&stage, scope.region, &scope.expected, false)
            )))
            .unwrap()
            .is_some()
    );
}

#[test]
fn durable_rejection_before_native_write_prevents_seed_restoration_after_profile_loss() {
    for shared_root in [false, true] {
        let root = aterm_tempfile::tempdir().unwrap();
        let f = Fixture::at(root.path());
        let stage = f.stage("retained");
        f.save_for(&stage, shared_root);
        let key = login_drive::RecoveryKey([4; 32]);
        let admission = if shared_root {
            login_drive::RecoveryAdmission::new(key, false, |_| Ok(()))
        } else {
            login_drive::RecoveryAdmission::pool(key, |_| Ok(()))
        };
        let req = login_drive::Request {
            admission: Some(&admission),
            ..f.request(&stage, 4)
        };
        let config = stage.join("config.toml");
        let credential = kimi_credential_path(&stage, Region::Global);
        let before = std::fs::read(&credential).unwrap();
        {
            let mut journal = Journal::open(&req, &f.home).unwrap().unwrap();
            let result =
                journal.observe_existing_with(&req, &f.home, deadline(), |journal, _, _| {
                    journal.checkpoint_rejected(&req).unwrap();
                    assert_eq!(std::fs::read(&credential).unwrap(), before);
                    let mut rewritten = before.clone();
                    rewritten.push(b'\n');
                    aterm_secrets::fsx::write_atomic(&credential, &rewritten, 0o600).unwrap();
                    // The process can stop after the encrypted record commits but
                    // before publishing the native tombstone or returning rejection.
                    Err(KimiNativeError::Temporary)
                });
            assert!(result.is_err());
        }
        std::fs::remove_file(config).unwrap();
        std::fs::remove_file(&credential).unwrap();
        let mut journal = Journal::open(&req, &f.home).unwrap().unwrap();
        assert!(
            journal
                .observe_existing_using(&req, &f.home, deadline(), |_, _| {
                    panic!("a confirmed rejected seed was restored for another exchange")
                })
                .unwrap()
                .is_none()
        );
        assert!(!credential.exists());
        f.no_cookies();
    }
}
