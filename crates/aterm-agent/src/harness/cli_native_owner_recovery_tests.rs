// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::harness::{account_work::Lifetime, codex_app_login, codex_enrollment as enrollment};

struct NativeFixture {
    bed: Bed,
    request: enrollment::Request,
    stage: PathBuf,
    lifetime: Lifetime,
}

impl NativeFixture {
    fn new(cut: codex_app_login::EnrollmentCut, predecessor: bool) -> Self {
        let bed = Bed::new(Cli::Codex);
        let home = bed.env.home.as_deref().unwrap();
        aterm_secrets::client::machine_identity_as(
            &aterm_secrets::client::Paths::locate_for_home(home),
            aterm_secrets::Making {
                test_binary: true,
                ..Default::default()
            },
        )
        .unwrap();
        let request = enrollment_request(&bed);
        let lifetime = Lifetime::default();
        let (work, control) = enrollment::owned_work(request.clone(), lifetime.enter().unwrap());
        bed.store
            .transact_logins(bed.env.now, &mut None, |_, logins| {
                logins.enrollment.admit_native(&request)?;
                Ok(Vec::new())
            })
            .unwrap();
        let signer = bed.store.signin_now().unwrap().unwrap();
        let stage = bed
            .store
            .claim_staging_dir(&request.label)
            .unwrap()
            .canonicalize()
            .unwrap();
        if predecessor {
            aterm_uds::private_file::write_private(
                &stage,
                "auth.json",
                &std::fs::read(bed.dirs[0].join("auth.json")).unwrap(),
            )
            .unwrap();
        }
        bed.store
            .transact_pool_logins_within(
                &accounts::LOCK_LADDER,
                &bed.path,
                bed.env.now,
                &mut None,
                |_, _, logins| {
                    accounts::admit_workflow(logins, &request.label)?;
                    Ok(Vec::new())
                },
            )
            .unwrap();
        let artifact = RefCell::new(None);
        bed.store
            .transact_roster_logins_within(
                &accounts::LOCK_LADDER,
                &bed.path,
                bed.env.now,
                &mut None,
                |raw, state, logins| {
                    let row = raw.row(&request.label).unwrap();
                    let staging = enrollment::Staging {
                        native_row: None,
                        request: request.clone(),
                        store: bed.store.dir.canonicalize().unwrap(),
                        directory: stage.clone(),
                        store_identity: Some(enrollment::DirectoryIdentity::capture(
                            &bed.store.dir,
                        )?),
                        directory_identity: Some(enrollment::DirectoryIdentity::capture(&stage)?),
                        parent: accounts::RenewalParent::capture(logins, &request.label),
                        workflow: accounts::current_workflow(row, logins).unwrap().value(),
                        policy: accounts::pool_authority_revision(raw, logins)?,
                        selection: state
                            .pending_pin
                            .as_ref()
                            .map(accounts::PendingSelection::identity),
                    };
                    *artifact.borrow_mut() = Some(logins.enrollment.bind_staging(staging)?);
                    Ok(Vec::new())
                },
            )
            .unwrap();
        let artifact = artifact.into_inner().unwrap();
        let prepared = crate::harness::codex_grant_owner::prepare(
            &bed.env,
            &request,
            &stage.join("auth.json"),
            &artifact.staging().policy,
        )
        .unwrap();
        let (_drove, outcome) = codex_app_login::tests::native_enrollment_owner_fixture(
            &artifact,
            Some(cut),
            bed.env.now + 1,
            prepared,
        );
        assert!(matches!(
            &outcome,
            crate::harness::codex_grant_owner::CaptureOutcome::Verified(_)
        ));
        work.fixture_capture_after_native_admission(&bed.env, outcome);
        drop(signer);
        drop(work);
        control.settle_custody(&bed.env).unwrap();
        control.retired().unwrap().finish(&bed.env).unwrap();
        Self {
            bed,
            request,
            stage,
            lifetime,
        }
    }

    fn recover(&self) -> enrollment::Status {
        let before = self.bed.store.load_logins(self.bed.env.now).unwrap().0;
        assert!(
            before
                .enrollment
                .requires_native_grant(&self.request)
                .unwrap()
        );
        let host = NoReplay {
            now: self.bed.env.now + 2,
        };
        let (work, _) =
            enrollment::owned_work(self.request.clone(), self.lifetime.enter().unwrap());
        work.run_with_host(&self.bed.env, &host).unwrap()
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
fn required_native_recovery_joins_actual_code_response_across_publication_cuts() {
    for (cut, predecessor) in [
        (codex_app_login::EnrollmentCut::BeforeSeal, false),
        (codex_app_login::EnrollmentCut::BeforeSeal, true),
        (codex_app_login::EnrollmentCut::AfterSeal, false),
        (codex_app_login::EnrollmentCut::AfterSeal, true),
        (codex_app_login::EnrollmentCut::AfterAuthWrite, true),
    ] {
        let fixture = NativeFixture::new(cut, predecessor);
        let original = std::fs::read(fixture.bed.dirs[0].join("auth.json")).unwrap();
        if cut == codex_app_login::EnrollmentCut::AfterSeal && !predecessor {
            let owner = fixture
                .bed
                .store
                .dir
                .join("credential-owners")
                .join(&fixture.request.id)
                .join("candidate.age");
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
        let logins = fixture
            .bed
            .store
            .load_logins(fixture.bed.env.now)
            .unwrap()
            .0;
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
            &fixture.bed.env,
            &fixture.request,
            &binding.reference,
        )
        .unwrap();
        crate::harness::codex_grant_owner::with_retained_response(
            &fixture.bed.env,
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
            std::fs::read(fixture.bed.dirs[0].join("auth.json")).unwrap(),
            original
        );
        assert!(!fixture.stage.join("auth.json.refreshed").exists());
    }
}

#[test]
fn required_native_recovery_refuses_different_candidate_without_changing_predecessor_or_response() {
    for cut in [
        codex_app_login::EnrollmentCut::AfterSeal,
        codex_app_login::EnrollmentCut::AfterAuthWrite,
    ] {
        let fixture = NativeFixture::new(cut, true);
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
        assert_eq!(fixture.recover().phase, enrollment::Phase::Running);
        assert_eq!(
            std::fs::read(fixture.stage.join("auth.json")).unwrap(),
            before
        );
        assert_eq!(std::fs::read(path).unwrap(), journal_before);
    }
}

#[test]
fn owner_only_response_cannot_invent_a_predecessor_receipt() {
    let fixture = NativeFixture::new(codex_app_login::EnrollmentCut::BeforeSeal, true);
    aterm_uds::private_file::write_private(
        &fixture.stage,
        "auth.json",
        b"unrelated current credential",
    )
    .unwrap();
    let before = std::fs::read(fixture.stage.join("auth.json")).unwrap();
    assert_eq!(fixture.recover().phase, enrollment::Phase::Running);
    assert_eq!(
        std::fs::read(fixture.stage.join("auth.json")).unwrap(),
        before
    );
    assert!(!fixture.stage.join("auth.json.refreshed").exists());
}

#[test]
fn plaintext_predecessor_rewrite_cannot_authorize_replacing_unrelated_stage() {
    let fixture = NativeFixture::new(codex_app_login::EnrollmentCut::AfterSeal, true);
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
    assert_eq!(fixture.recover().phase, enrollment::Phase::Running);
    assert_eq!(
        std::fs::read(fixture.stage.join("auth.json")).unwrap(),
        other
    );
    assert_eq!(std::fs::read(path).unwrap(), before);
}

#[test]
fn held_owner_or_auth_lock_defers_native_recovery_without_changing_custody() {
    let fixture = NativeFixture::new(codex_app_login::EnrollmentCut::AfterSeal, false);
    let logins = fixture
        .bed
        .store
        .load_logins(fixture.bed.env.now)
        .unwrap()
        .0;
    let reference = logins
        .enrollment
        .checkpoint_reference(&fixture.request)
        .unwrap()
        .unwrap();
    let grant =
        crate::harness::codex_grant_owner::recover(&fixture.bed.env, &fixture.request, &reference)
            .unwrap();
    let journal = std::fs::read(fixture.stage.join("auth.json.refreshed")).unwrap();
    crate::harness::codex_grant_owner::with_retained_response(
        &fixture.bed.env,
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
    assert_eq!(fixture.recover().phase, enrollment::Phase::Running);
    assert_eq!(
        std::fs::read(fixture.stage.join("auth.json.refreshed")).unwrap(),
        journal
    );
    assert!(!fixture.stage.join("auth.json").exists());
    drop(auth);
    assert_eq!(fixture.recover().phase, enrollment::Phase::Applied);
}
