// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Current machine-wide native Code-owner/Main-artifact recovery controls.
//! Loopback final transport is synthetic; all admission/provenance and custody
//! checks run before that test-only send substitution.
use super::*;
use crate::harness::{
    account_work::Lifetime, codex_app_login, codex_enrollment as enrollment,
    codex_grant_owner as owner,
};

struct CurrentNativeFixture<'a> {
    env: &'a Env,
    store: accounts::Store,
    request: enrollment::Request,
    stage: PathBuf,
    source: PathBuf,
    reference: owner::GrantReference,
    lifetime: Lifetime,
}
impl CurrentNativeFixture<'_> {
    fn with(
        cut: codex_app_login::EnrollmentCut,
        predecessor: bool,
        run: impl FnOnce(&CurrentNativeFixture<'_>),
    ) {
        owner::with_global_code_fixture(|env, request, auth, turn, prepared, artifact| {
            let store = env.accounts_store_for(Cli::Codex).unwrap();
            let roster = env.accounts_roster_for(Cli::Codex).unwrap();
            let records = store.read_account_records(Some(&roster), env.now).unwrap();
            let source = PathBuf::from(
                records
                    .raw_roster
                    .row(&request.label)
                    .unwrap()
                    .dir
                    .as_ref()
                    .unwrap(),
            );
            aterm_uds::private_file::write_private(
                &source,
                "fixture-sentinel",
                b"isolated original source remains unchanged",
            )
            .unwrap();
            let lifetime = Lifetime::default();
            let scope = lifetime.enter().unwrap();
            let (work, control) = enrollment::owned_work(request.clone(), scope.clone());
            let outcome = codex_app_login::tests::current_global_code_fixture(
                &artifact,
                prepared,
                &scope,
                Some(cut),
                true,
                predecessor,
                env,
            );
            let owner::CaptureOutcome::Verified(retained) = &outcome else {
                panic!("actual native sender must retain its supported-bundle response");
            };
            let reference = retained.reference();
            work.fixture_capture_after_native_admission(env, outcome);
            drop(turn);
            drop(work);
            drop(scope);
            control.settle_custody(env).unwrap();
            control.retired().unwrap().finish(env).unwrap();
            let fixture = CurrentNativeFixture {
                env,
                store,
                request: request.clone(),
                stage: auth.parent().unwrap().to_path_buf(),
                source,
                reference,
                lifetime,
            };
            run(&fixture);
        });
    }
    fn owner_cipher(&self) -> PathBuf {
        let retained = owner::recover(self.env, &self.request, &self.reference).unwrap();
        owner::fixture_owner_cipher(self.env, &self.request, &retained).unwrap()
    }
    fn recover(&self) -> enrollment::Status {
        self.try_recover().unwrap()
    }
    fn try_recover(&self) -> Result<enrollment::Status, String> {
        let before = self.store.load_logins(self.env.now).unwrap().0;
        assert!(
            before
                .enrollment
                .requires_global_catalogue(&self.request)
                .unwrap()
        );
        assert!(
            before
                .enrollment
                .catalogue_staging(&self.request)
                .unwrap()
                .native_row
                .is_some()
        );
        let host = NoReplay {
            now: self.env.now + 2,
        };
        let (work, _) =
            enrollment::owned_work(self.request.clone(), self.lifetime.enter().unwrap());
        work.run_native_with_host(self.env, &host)
    }
}

struct NoReplay {
    now: i64,
}
impl AccountsHost for NoReplay {
    fn stdin_is_terminal(&self) -> bool {
        false
    }
    fn unaskable(&self) -> Option<String> {
        None
    }
    fn auth_status(&self, _: &str) -> Result<Captured, String> {
        panic!("recovery must not probe a provider")
    }
    fn login(&self, _: Option<&str>) -> Result<i32, String> {
        panic!("recovery must not log in again")
    }
    fn now(&self) -> i64 {
        self.now
    }
    fn vendor_sign_in(
        &self,
        _: &crate::harness::login_drive::Request<'_>,
    ) -> crate::harness::login_drive::Drove {
        panic!("a retained code response must never dispatch another POST")
    }
}

#[test]
fn current_global_required_native_recovery_joins_actual_code_response_across_publication_cuts() {
    for (cut, predecessor) in [
        (codex_app_login::EnrollmentCut::BeforeSeal, false),
        (codex_app_login::EnrollmentCut::BeforeSeal, true),
        (codex_app_login::EnrollmentCut::AfterSeal, false),
        (codex_app_login::EnrollmentCut::AfterSeal, true),
        (codex_app_login::EnrollmentCut::AfterAuthWrite, true),
    ] {
        CurrentNativeFixture::with(cut, predecessor, |fixture| {
            let original = std::fs::read(fixture.source.join("fixture-sentinel")).unwrap();
            if cut == codex_app_login::EnrollmentCut::AfterSeal && !predecessor {
                let owner = fixture.owner_cipher();
                let held = owner.with_extension("held");
                let main_before = std::fs::read(fixture.stage.join("auth.json.refreshed")).unwrap();
                let auth_before = std::fs::read(fixture.stage.join("auth.json")).ok();
                std::fs::rename(&owner, &held).unwrap();
                assert_eq!(fixture.recover().phase, enrollment::Phase::Running);
                assert_eq!(
                    std::fs::read(fixture.stage.join("auth.json")).ok(),
                    auth_before
                );
                assert_eq!(
                    std::fs::read(fixture.stage.join("auth.json.refreshed")).unwrap(),
                    main_before
                );
                std::fs::rename(held, owner).unwrap();
            }
            let result = fixture.recover();
            assert_eq!(result.phase, enrollment::Phase::Applied);
            let logins = fixture.store.load_logins(fixture.env.now).unwrap().0;
            let binding = logins
                .enrollment
                .published_grant(&fixture.request.label, result.generation.unwrap())
                .unwrap();
            assert!(logins.enrollment.authenticates_published_grant(&binding));
            let witness = logins
                .enrollment
                .owner_witness(&binding)
                .unwrap()
                .expect("native publication must install its canonical owner witness");
            assert!(witness.valid());
            let grant = crate::harness::codex_grant_owner::recover(
                fixture.env,
                &fixture.request,
                &binding.reference,
            )
            .unwrap();
            crate::harness::codex_grant_owner::with_retained_response(
                fixture.env,
                &fixture.request,
                &grant,
                |_, bytes, _, _, _| {
                    assert_eq!(
                        std::fs::read(fixture.stage.join("auth.json")).unwrap(),
                        bytes
                    );
                    Ok(())
                },
            )
            .unwrap();
            assert_eq!(
                std::fs::read(fixture.source.join("fixture-sentinel")).unwrap(),
                original
            );
            assert!(!fixture.stage.join("auth.json.refreshed").exists());
        });
    }
}

#[test]
fn current_global_required_native_recovery_refuses_different_candidate_without_changing_predecessor_or_response()
 {
    for cut in [
        codex_app_login::EnrollmentCut::AfterSeal,
        codex_app_login::EnrollmentCut::AfterAuthWrite,
    ] {
        CurrentNativeFixture::with(cut, true, |fixture| {
            let path = fixture.stage.join("auth.json.refreshed");
            let mut journal: aterm_json::Value =
                aterm_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            let mut candidate: aterm_json::Value =
                aterm_json::from_str(journal["candidate"].as_str().unwrap()).unwrap();
            candidate
                .as_object_mut()
                .unwrap()
                .insert("different_document".into(), aterm_json::json!(true));
            let candidate = aterm_json::to_string(&candidate).unwrap();
            let digest =
                aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(candidate.as_bytes()));
            journal
                .as_object_mut()
                .unwrap()
                .insert("candidate".into(), aterm_json::json!(candidate));
            journal
                .as_object_mut()
                .unwrap()
                .insert("digest".into(), aterm_json::json!(digest));
            aterm_uds::private_file::write_private(
                &fixture.stage,
                "auth.json.refreshed",
                &aterm_json::to_vec(&journal).unwrap(),
            )
            .unwrap();
            let before = std::fs::read(fixture.stage.join("auth.json")).unwrap();
            let journal_before = std::fs::read(&path).unwrap();
            assert_eq!(
                fixture.try_recover().err().as_deref(),
                Some("retained enrollment response differs")
            );
            assert_eq!(
                std::fs::read(fixture.stage.join("auth.json")).unwrap(),
                before
            );
            assert_eq!(std::fs::read(path).unwrap(), journal_before);
        });
    }
}

#[test]
fn current_global_owner_only_response_cannot_invent_a_predecessor_receipt() {
    CurrentNativeFixture::with(
        codex_app_login::EnrollmentCut::BeforeSeal,
        true,
        |fixture| {
            aterm_uds::private_file::write_private(
                &fixture.stage,
                "auth.json",
                b"unrelated current credential",
            )
            .unwrap();
            let before = std::fs::read(fixture.stage.join("auth.json")).unwrap();
            assert_eq!(
                fixture.try_recover().err().as_deref(),
                Some("retained enrollment predecessor differs")
            );
            assert_eq!(
                std::fs::read(fixture.stage.join("auth.json")).unwrap(),
                before
            );
            assert!(!fixture.stage.join("auth.json.refreshed").exists());
        },
    );
}

#[test]
fn current_global_plaintext_predecessor_rewrite_cannot_authorize_replacing_unrelated_stage() {
    CurrentNativeFixture::with(codex_app_login::EnrollmentCut::AfterSeal, true, |fixture| {
        let other = b"different credential that was never the pre-POST predecessor";
        aterm_uds::private_file::write_private(&fixture.stage, "auth.json", other).unwrap();
        let path = fixture.stage.join("auth.json.refreshed");
        let mut journal: aterm_json::Value =
            aterm_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let digest = aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(other));
        journal
            .as_object_mut()
            .unwrap()
            .insert("predecessor".into(), aterm_json::json!(digest));
        aterm_uds::private_file::write_private(
            &fixture.stage,
            "auth.json.refreshed",
            &aterm_json::to_vec(&journal).unwrap(),
        )
        .unwrap();
        let before = std::fs::read(&path).unwrap();
        assert_eq!(
            fixture.try_recover().err().as_deref(),
            Some("retained enrollment predecessor differs")
        );
        assert_eq!(
            std::fs::read(fixture.stage.join("auth.json")).unwrap(),
            other
        );
        assert_eq!(std::fs::read(path).unwrap(), before);
    });
}

#[test]
fn current_global_held_owner_or_auth_lock_defers_native_recovery_without_changing_custody() {
    CurrentNativeFixture::with(
        codex_app_login::EnrollmentCut::AfterSeal,
        false,
        |fixture| {
            let reference = fixture.reference.clone();
            let grant = crate::harness::codex_grant_owner::recover(
                fixture.env,
                &fixture.request,
                &reference,
            )
            .unwrap();
            let journal = std::fs::read(fixture.stage.join("auth.json.refreshed")).unwrap();
            crate::harness::codex_grant_owner::with_retained_response(
                fixture.env,
                &fixture.request,
                &grant,
                |_, _, _, _, _| {
                    assert_eq!(fixture.recover().phase, enrollment::Phase::Running);
                    Ok(())
                },
            )
            .unwrap();
            let auth = codex_app_login::AuthLock::try_take(&fixture.stage.join("auth.json"))
                .unwrap()
                .unwrap();
            assert_eq!(
                fixture.try_recover().err().as_deref(),
                Some("native publication stage custody busy")
            );
            assert_eq!(
                std::fs::read(fixture.stage.join("auth.json.refreshed")).unwrap(),
                journal
            );
            assert!(!fixture.stage.join("auth.json").exists());
            drop(auth);
            assert_eq!(fixture.recover().phase, enrollment::Phase::Applied);
        },
    );
}
