// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Synthetic issuer input with actual native resource roles, envelope bridge,
//! canonical writer and production final CLI adapter. No browser/provider,
//! independent lineage or SourceV1 qualification is claimed here.

use super::*;

struct LocalOnly(i64);
impl AccountsHost for LocalOnly {
    fn stdin_is_terminal(&self) -> bool {
        true
    }
    fn auth_status(&self, _: &str) -> Result<Captured, String> {
        panic!("no provider request")
    }
    fn unaskable(&self) -> Option<String> {
        None
    }
    fn login(&self, _: Option<&str>) -> Result<i32, String> {
        panic!("no login replay")
    }
    fn now(&self) -> i64 {
        self.0
    }
}

#[test]
fn both_native_fenced_rows_publish_through_cli_final_adapter() {
    let _registered_ports =
        crate::harness::native_enrollment_admission::tests::callback_test_turn();
    crate::harness::codex_grant_owner::with_two_native_rows_fixture(|env, store, roster, rows| {
        let deadline =
            crate::harness::wake_deadline::WakeDeadline::after(crate::harness::login_drive::HANG);
        let lifetime = crate::harness::account_work::Lifetime::default();
        let scopes = [lifetime.enter().unwrap(), lifetime.enter().unwrap()];
        // Only the authenticated owner→Main bridge reconstructs envelope and
        // exact bytes. No raw file or plaintext predecessor supplies authority.
        let signer = store.signin_now().unwrap().unwrap();
        for (request, staging, fresh, artifact) in &rows {
            assert_eq!(&staging.request, request);
            crate::harness::codex_app_login::reconcile_retained_enrollment(
                env,
                staging,
                fresh,
                deadline,
                Some(&scopes[0]),
                store,
                &signer,
            )
            .unwrap();
            artifact.as_ref().unwrap().bind_deadline(deadline).unwrap();
        }
        drop(signer);
        aterm_uds::private_file::check_private_dir(&env.home.as_ref().unwrap().join(".aterm"))
            .unwrap();
        let records = store.read_account_records(Some(roster), env.now).unwrap();
        let turns: Vec<_> = rows
            .iter()
            .map(|(request, _, _, _)| {
                let row = records.raw_roster.row(&request.label).unwrap();
                let turn = crate::harness::native_enrollment_admission::NativeBrowserTurn::acquire(
                    store, request, row, env.now,
                )
                .unwrap();
                turn.bind_source_home(env.home.as_deref().unwrap()).unwrap();
                turn
            })
            .collect();
        for (((request, staging, fresh, artifact), turn), scope) in
            rows.into_iter().zip(turns).zip(scopes)
        {
            let row = records.roster.row(&request.label).unwrap();
            let begun = crate::harness::sign_in_ladder::asked_fixture(Cli::Codex, &request.label)
                .begin_native(&request.label, turn.clone(), store, env.now)
                .unwrap();
            crate::harness::codex_grant_owner::admit_native_publication_within(
                env, &request, &fresh, &turn, &scope, deadline,
            )
            .unwrap();
            drop(turn);
            let route = Cli::Codex
                .accounts()
                .signin
                .as_ref()
                .unwrap()
                .route(row.flow(Cli::Codex))
                .unwrap();
            let plan = SignInPlan {
                native_env: Some(env.clone()),
                native_row_occurrence: None,
                native_row_admission: None,
                native_required: true,
                native_grant: Some(fresh),
                publication: crate::harness::codex_app_login::PublicationCustody::default(),
                enrollment: Some(request.clone()),
                artifact,
                recovered: None,
                route,
                hosts: route.hosts.of(row.vendor.region).unwrap(),
                staging: staging.directory.clone(),
                staging_text: staging.directory.to_str().unwrap().into(),
                parent: staging.parent,
                workflow: accounts::current_workflow(
                    records.raw_roster.row(&request.label).unwrap(),
                    &records.logins,
                )
                .unwrap(),
                #[cfg(unix)]
                native_google: None,
                native_recovery: None,
                native_admission: None,
                #[cfg(unix)]
                native_parent: None,
                #[cfg(unix)]
                reauthentication: None,
                _begun: begun,
            };
            let expected_digest = crate::harness::codex_grant_owner::with_retained_response(
                env,
                &request,
                plan.native_grant.as_ref().unwrap(),
                |_, document, _, _, _| {
                    Ok(aterm_codec::hex::encode_lower(
                        &aterm_digest::Sha256::digest(document),
                    ))
                },
            )
            .unwrap();
            let mut error = Vec::new();
            let result = bind_signed_in(
                Cli::Codex,
                env.now,
                &LocalOnly(env.now),
                Some(env),
                store,
                (row, roster),
                records
                    .raw_roster
                    .row(&request.label)
                    .unwrap()
                    .dir
                    .as_deref(),
                &plan,
                accounts::Proof::Status,
                "renew",
                &mut Vec::new(),
                &mut error,
            );
            assert_eq!(
                result,
                Renewal::Renewed,
                "{}",
                String::from_utf8_lossy(&error)
            );
            let (published, _) = store.load_logins(env.now).unwrap();
            let generation = published
                .get(&request.label)
                .unwrap()
                .last_success_generation;
            let binding = published
                .enrollment
                .published_grant(&request.label, generation)
                .unwrap();
            let witness = published
                .enrollment
                .owner_witness(&binding)
                .unwrap()
                .expect("a published row must initialize its actual owner revision");
            assert_eq!(witness.revision.generation, 1);
            assert_eq!(witness.revision.digest, expected_digest);
            assert!(witness.pending.is_none());
            drop(plan); // actual profile/callback/S/E resources retire before B.
            drop(scope);
        }
        let (complete, _) = store.load_logins(env.now).unwrap();
        assert_eq!(complete.generation_clock, 2);
        for label in ["alpha", "beta"] {
            let generation = complete.get(label).unwrap().last_success_generation;
            assert!(
                complete
                    .enrollment
                    .published_grant(label, generation)
                    .is_some()
            );
        }
    });
}
