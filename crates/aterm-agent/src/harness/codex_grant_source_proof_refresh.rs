// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Constructor-bound synthetic received-response boundary for the joined
//! Source protocol proof. No provider function, Slot or fake caller is used.
use super::*;

pub(in crate::harness) fn advance(
    proof: &crate::harness::codex_enrollment::SourceProofLaunch,
    env: &Env,
) -> (Option<u64>, Result<u64, OwnerRefreshError>) {
    let expected_generation = std::cell::Cell::new(None);
    // This String callback performs admission only. No mutable response or
    // owned retention can be erased by its final physical/cancellation check.
    let admitted =
        proof.with_refresh_fixture(env, |binding, scope| Ok((binding.clone(), scope.clone())));
    let result = (|| {
        let (binding, scope) = admitted.map_err(|_| OwnerRefreshError::Unavailable)?;
        transaction::with_refresh_work_scoped(env, &scope, None, |work| {
            let lineage = work
                .send_lineage(env, &binding)
                .map_err(|_| OwnerRefreshError::Unavailable)?;
            let mut retained_error = None;
            let admitted = super::with_maintenance_owner(env, &binding, &scope, |mut grant| {
                let witness = grant.anchor().read()?.ok_or("proof witness missing")?;
                let mut claimed =
                    Claimed::open(env, &binding, false)?.ok_or("proof journal missing")?;
                let current = claimed.view(&witness)?;
                let expected = current
                    .revision
                    .generation
                    .checked_add(1)
                    .ok_or("proof revision exhausted")?;
                expected_generation.set(Some(expected));
                let token =
                    codex_app_login::owned_refresh_token_digest(current.document.as_slice())?;
                let id =
                    aterm_uds::rand::hex_token::<16>().map_err(|_| "proof nonce unavailable")?;
                let operation = actor::NativeQuotaOperation {
                    id: id.clone(),
                    expected_digest: current.revision.digest,
                    credential_generation: expected - 1,
                    native_quota: actor::NativeQuotaContext {
                        schema: 1,
                        read_context_digest: aterm_codec::hex::encode_lower(
                            &aterm_digest::Sha256::digest(id.as_bytes()),
                        ),
                    },
                };
                let journal = claimed.journal.as_mut().ok_or("proof journal missing")?;
                if journal.pending.is_some() {
                    return Err("proof refresh already pending".into());
                }
                journal.schema = 4;
                journal.pending = Some(Pending {
                    operation: actor::JournalOperation::NativeQuota(operation),
                    phase: Phase::Prepared,
                    quarantine_reason: None,
                    base_generation: journal.current.generation,
                    base_digest: journal.current.digest.clone(),
                    token_digest: Some(token),
                    response: None,
                });
                journal.transition = JournalTransition {
                    expected: Some(witness.clone()),
                    kind: TransitionKind::Attempt,
                };
                // Each closure returns owned retention rather than erasing a
                // completed response. The outer typed result carries it out.
                let written = (|| -> Result<u64, OwnerRefreshError> {
                    claimed = claimed.persist().map_err(OwnerRefreshError::Retaining)?;
                    grant
                        .anchor()
                        .compare_exchange(
                            &claimed
                                .transition()
                                .map_err(|_| OwnerRefreshError::Unavailable)?,
                        )
                        .map_err(|_| OwnerRefreshError::Unavailable)?;
                    let witness = grant
                        .anchor()
                        .read()
                        .map_err(|_| OwnerRefreshError::Unavailable)?
                        .ok_or(OwnerRefreshError::Unavailable)?;
                    claimed = claimed
                        .arm_for_send(witness)
                        .map_err(OwnerRefreshError::Retaining)?;
                    grant
                        .anchor()
                        .compare_exchange(
                            &claimed
                                .transition()
                                .map_err(|_| OwnerRefreshError::Unavailable)?,
                        )
                        .map_err(|_| OwnerRefreshError::Unavailable)?;
                    let witness = grant
                        .anchor()
                        .read()
                        .map_err(|_| OwnerRefreshError::Unavailable)?
                        .ok_or(OwnerRefreshError::Unavailable)?;
                    let journal = claimed
                        .journal
                        .as_mut()
                        .ok_or(OwnerRefreshError::Unavailable)?;
                    let pending = journal
                        .pending
                        .as_mut()
                        .ok_or(OwnerRefreshError::Unavailable)?;
                    pending.phase = Phase::Responded;
                    pending.response = Some(Response {
                        status: 200,
                        received_at: Some(
                            u64::try_from(env.now).map_err(|_| OwnerRefreshError::Unavailable)?,
                        ),
                        body: aterm_codec::base64::encode(
                            &aterm_json::to_vec(&aterm_json::json!({
                                "access_token":"isolated-source-proof-access",
                                "refresh_token":format!("isolated-source-proof-refresh-{id}")
                            }))
                            .map_err(|_| OwnerRefreshError::Unavailable)?,
                        )
                        .map_err(|_| OwnerRefreshError::Unavailable)?,
                    });
                    journal.transition = JournalTransition {
                        expected: Some(witness),
                        kind: TransitionKind::Response,
                    };
                    claimed = claimed.persist().map_err(OwnerRefreshError::Retaining)?;
                    grant
                        .anchor()
                        .compare_exchange(
                            &claimed
                                .transition()
                                .map_err(|_| OwnerRefreshError::Unavailable)?,
                        )
                        .map_err(|_| OwnerRefreshError::Unavailable)?;
                    Ok(expected)
                })();
                match written {
                    Ok(generation) => Ok(Some(generation)),
                    Err(error) => {
                        retained_error = Some(error);
                        Ok(None)
                    }
                }
            });
            if let Some(error) = retained_error {
                return Err(error);
            }
            let expected = admitted
                .map_err(|_| OwnerRefreshError::Unavailable)?
                .ok_or(OwnerRefreshError::Unavailable)?;
            drop(lineage);
            transaction::reconcile_retained_held_work(env, &binding, &scope, work)?;
            let generation = super::with_maintenance_owner(env, &binding, &scope, |mut grant| {
                Ok(grant
                    .anchor()
                    .read()?
                    .ok_or("proof witness missing")?
                    .revision
                    .generation)
            })
            .map_err(|_| OwnerRefreshError::Unavailable)?;
            if generation != expected {
                return Err(OwnerRefreshError::Unavailable);
            }
            Ok(generation)
        })
    })();
    (expected_generation.get(), result)
}
