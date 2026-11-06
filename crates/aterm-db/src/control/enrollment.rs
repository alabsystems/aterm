// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A bounded first-binding exchange on the existing control channel. The
//! authority's journal is the publication record: retain before replying.

use super::*;
use aterm_secrets::{
    client,
    commons::{
        binding::{MAX_CHALLENGE, Pending, RESPONSE_LEN},
        bootstrap::Bootstrap,
    },
};

pub(super) fn client(
    endpoint: &Endpoint,
    stream: &mut (impl Read + Write),
    proof: Option<&NodeProof>,
    repair: &mut Repair,
) -> Result<Report> {
    endpoint.require_legacy_records()?;
    let paths = endpoint
        .paths
        .as_ref()
        .ok_or_else(|| Error::new("pending device has no custody"))?;
    let bootstrap = Bootstrap::cached_until(paths, Instant::now() + Duration::from_secs(5))
        .map_err(failure)?
        .ok_or_else(|| Error::new("pending device lost its authenticated root"))?;
    let request = bootstrap.request().map_err(failure)?;
    if bootstrap.trust() != endpoint.trust
        || request.binding().actor() != endpoint.actor
        || endpoint.hello[84..] != *request.bytes()
    {
        return Err(Error::new("pending device custody changed"));
    }
    endpoint.require_legacy_records()?;
    send_frame(stream, &endpoint.hello)?;
    let hello = repair.receive_answer(stream, Side::Initiator)?;
    let peer = authenticate(endpoint.trust, endpoint.actor, proof, &hello)?;
    let actor = peer.actor;
    let pass = verify_peer_against_local_log(
        endpoint.custody(),
        endpoint.trust,
        &peer.request,
        peer.log.as_deref(),
    )?;
    let authenticated_at = Instant::now();
    let challenge = receive_frame(stream, MAX_CHALLENGE)?;
    // Answering commits this device to the member its check passed.
    pass.spend();
    let response = bootstrap
        .device()
        .answer(&request, &challenge)
        .map_err(failure)?;
    send_frame(stream, &response)?;
    let wire = receive_frame(stream, MAX_ACX2_HELLO)?;
    if wire.starts_with(b"ACW2") {
        let witness_log = check_log_witness(endpoint.trust, &request, &wire[4..])?;
        if witness_log.audience_left_out(
            &aterm_secrets::membership_log::EpochAudience::Commons,
            &request.binding().actor(),
        ) || witness_log.audience_left_out(
            &aterm_secrets::membership_log::EpochAudience::Everyone,
            &request.binding().actor(),
        ) {
            return Err(Error::new("pending device refused membership witness"));
        }
        let records = aterm_secrets::membership_log::decode_records(&wire[4..]).map_err(failure)?;
        let wires: Vec<Vec<u8>> = records.into_iter().map(|r| r.bytes().to_vec()).collect();
        let acks = client::retain_records_for_commons(
            paths,
            endpoint.trust.network,
            endpoint.trust.authority,
            &wires,
        )
        .map_err(failure)?;
        if acks.iter().any(|ok| !*ok) {
            return Err(Error::new("pending device refused membership witness"));
        }
    } else {
        if wire.len() > MAX_NODE {
            return Err(Error::new("Commons enrollment witness exceeds its bound"));
        }
        check_witness(endpoint.trust, &request, &wire)?;
        // An orphan may establish a historical Bind witness. It grants no data
        // access; regular authenticated repair must first assemble its ancestry.
        let mut journal = Journal::open_existing(&endpoint.dir, endpoint.trust).map_err(failure)?;
        journal.ingest(&wire).map_err(failure)?;
    }
    repair.prepare(endpoint.trust, actor);
    repair.authenticated = Some(authenticated_at);
    Ok(Report {
        received: 1,
        ..Report::default()
    })
}

#[cfg(test)]
mod preflight_tests;
#[cfg(test)]
pub(crate) mod tests;

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbEnrollmentCommit",
        action = "Enroll",
        project = "aterm_db::control::enrollment::tests::enrollment_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbEnrollmentCommit",
        action = "LostReply",
        project = "aterm_db::control::enrollment::tests::enrollment_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbEnrollmentCommit",
        action = "Retry",
        project = "aterm_db::control::enrollment::tests::enrollment_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbEnrollmentCommit",
        action = "NoEvidence",
        project = "aterm_db::control::enrollment::tests::enrollment_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbEnrollmentCommit",
        action = "Excluded",
        project = "aterm_db::control::enrollment::tests::enrollment_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbEnrollmentCommit",
        action = "Retired",
        project = "aterm_db::control::enrollment::tests::enrollment_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbEnrollmentCommit",
        action = "Fork",
        project = "aterm_db::control::enrollment::tests::enrollment_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbEnrollmentGate",
        action = "Eligible",
        project = "aterm_db::control::enrollment::preflight_tests::gate_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbEnrollmentGate",
        action = "NoEvidence",
        project = "aterm_db::control::enrollment::preflight_tests::gate_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbEnrollmentGate",
        action = "ExcludedBefore",
        project = "aterm_db::control::enrollment::preflight_tests::gate_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbEnrollmentGate",
        action = "RetiredBefore",
        project = "aterm_db::control::enrollment::preflight_tests::gate_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbEnrollmentGate",
        action = "ForkBefore",
        project = "aterm_db::control::enrollment::preflight_tests::gate_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbEnrollmentGate",
        action = "ExcludedDuring",
        project = "aterm_db::control::enrollment::preflight_tests::gate_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbEnrollmentGate",
        action = "RetiredDuring",
        project = "aterm_db::control::enrollment::preflight_tests::gate_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbEnrollmentGate",
        action = "ForkDuring",
        project = "aterm_db::control::enrollment::preflight_tests::gate_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbEnrollmentGate",
        action = "InvalidProof",
        project = "aterm_db::control::enrollment::preflight_tests::gate_transition"
    )
)]
pub(super) fn serve(
    endpoint: &Endpoint,
    stream: &mut (impl Read + Write),
    proof: Option<&NodeProof>,
    hello: &[u8],
    repair: &mut Repair,
) -> Result<Report> {
    endpoint.require_legacy_records()?;
    if hello.len() != HELLO_FIXED
        || &hello[..4] != b"ACE1"
        || hello[4..20] != endpoint.trust.network
        || hello[20..52] != endpoint.trust.authority
        || hello[52..84] != endpoint.trust.genesis
    {
        return Err(Error::new("invalid Commons enrollment context"));
    }
    let request = Request::decode(&hello[84..]).map_err(failure)?;
    let actor = authenticate_identity(endpoint.actor, proof, &request)?;
    let authenticated_at = Instant::now();
    let paths = endpoint
        .paths
        .as_ref()
        .ok_or_else(|| Error::new("this peer does not serve enrollment"))?;
    let log_backed = {
        let bootstrap = Bootstrap::cached_until(paths, Instant::now() + Duration::from_secs(5))
            .map_err(failure)?
            .ok_or_else(|| Error::new("enrollment peer lost its local root"))?;
        if bootstrap.trust() != endpoint.trust
            || bootstrap.request().map_err(failure)?.binding().actor() != endpoint.actor
        {
            return Err(Error::new("enrollment peer custody changed"));
        }
        bootstrap.is_log_backed().map_err(failure)?
    };
    // A challenge is work for an eligible candidate, never an admission grant: the check runs
    // before the lease, and again before the member authors the device's seat.
    let check = || self::check(endpoint, paths, &request, log_backed);
    repair.phase = Phase::PeerAdmission;
    let mut lease = endpoint.lease(check()?, actor, repair)?;
    lease.begin_round();
    lease.phase = Phase::Membership;
    if log_backed {
        endpoint.require_legacy_records()?;
        send_frame(stream, &endpoint.hello)?;
        let (mut pending, challenge) =
            Pending::issue(&request, endpoint.trust.network, endpoint.trust.authority)
                .map_err(failure)?;
        send_frame(stream, &challenge)?;
        let response = receive_frame(stream, RESPONSE_LEN)?;
        let possession = pending.verify(&response).map_err(failure)?;
        {
            let bootstrap = Bootstrap::cached_until(paths, Instant::now() + Duration::from_secs(5))
                .map_err(failure)?
                .ok_or_else(|| Error::new("enrollment peer lost its local root"))?;
            if bootstrap.trust() != endpoint.trust
                || bootstrap.request().map_err(failure)?.binding().actor() != endpoint.actor
            {
                return Err(Error::new("enrollment peer custody changed"));
            }
        }
        check()?.spend();
        let chain =
            client::enroll_commons_member_in_log(paths, endpoint.trust, request, possession)
                .map_err(failure)?;
        let encoded_records = aterm_secrets::membership_log::encode_records(&chain);
        let mut wire = Vec::with_capacity(4 + encoded_records.len());
        wire.extend_from_slice(b"ACW2");
        wire.extend_from_slice(&encoded_records);
        endpoint.require_legacy_records()?;
        send_frame(stream, &wire)?;
        lease.prepare(endpoint.trust, actor);
        lease.authenticated = Some(authenticated_at);
        return Ok(Report {
            sent: 1,
            ..Report::default()
        });
    }
    // The check above dropped all its local observations and locks before this interacts with
    // the peer.
    endpoint.require_legacy_records()?;
    send_frame(stream, &endpoint.hello)?;
    let (mut pending, challenge) =
        Pending::issue(&request, endpoint.trust.network, endpoint.trust.authority)
            .map_err(failure)?;
    send_frame(stream, &challenge)?;
    let response = receive_frame(stream, RESPONSE_LEN)?;
    let possession = pending.verify(&response).map_err(failure)?;
    // All cache/state/custody reads finish before taking the journal lock.
    // No vault-server request or peer I/O occurs while that lock is held.
    check()?.spend();
    let (admin, bootstrap) = local_authority(endpoint, paths)?;
    let wire = {
        let mut journal = Journal::open_existing(&endpoint.dir, endpoint.trust).map_err(failure)?;
        admin
            .enroll_commons_member(&bootstrap, &mut journal, possession)
            .map_err(failure)?
            .bytes()
            .to_vec()
    };
    endpoint.require_legacy_records()?;
    send_frame(stream, &wire)?;
    lease.prepare(endpoint.trust, actor);
    lease.authenticated = Some(authenticated_at);
    Ok(Report {
        sent: 1,
        ..Report::default()
    })
}

/// Check `request`, a pending device asking this member to enroll it
/// ([`super::peer_check::check`]): the roster, every record this member holds (at an endpoint
/// built from its log), then its own enrollment judgement, on its acknowledged log
/// ([`client::check_log_commons_enrollment`]) where it is `log_backed`, and on its journal
/// otherwise. The pass the enrollment's next grant takes.
///
/// # Errors
/// The kernel refused the device: the roster or a record held removes it, a view the kernel
/// requires did not read, or the member's judgement refused it.
pub(super) fn check(
    endpoint: &Endpoint,
    paths: &client::Paths,
    request: &Request,
    log_backed: bool,
) -> Result<super::Pass> {
    let acknowledged = || -> Result<()> {
        if log_backed {
            client::check_log_commons_enrollment(paths, endpoint.trust, request.binding())
                .map_err(failure)
        } else {
            let (admin, bootstrap) = local_authority(endpoint, paths)?;
            let journal = Journal::read_existing(&endpoint.dir, endpoint.trust).map_err(failure)?;
            admin
                .check_commons_enrollment(&bootstrap, &journal, request.binding())
                .map_err(failure)
        }
    };
    super::peer_check::check(
        endpoint.custody(),
        endpoint.trust,
        &super::peer_check::Judged::Enrollment {
            binding: request.binding(),
            acknowledged: &acknowledged,
        },
    )
}

pub(super) fn local_authority(
    endpoint: &Endpoint,
    paths: &client::Paths,
) -> Result<(client::Admin, Bootstrap)> {
    let admin = client::admin_cached(paths).map_err(failure)?;
    let bootstrap = Bootstrap::cached(paths)
        .map_err(failure)?
        .ok_or_else(|| Error::new("enrollment authority lost its local root"))?;
    if bootstrap.trust() != endpoint.trust
        || bootstrap.request().map_err(failure)?.binding().actor() != endpoint.actor
    {
        return Err(Error::new("enrollment authority custody changed"));
    }
    Ok((admin, bootstrap))
}
