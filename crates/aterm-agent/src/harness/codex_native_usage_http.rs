// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! HTTP is reached only with the owner's actual natural-read capability.
use super::*;
use crate::harness::{
    account_work, accounts_pool, codex_grant_owner::NativeUsageGrant, provider_read,
};

pub(in crate::harness) struct NativeUsageProbe {
    pub(in crate::harness) proactive: bool,
    pub(in crate::harness) status: Option<u16>,
    pub(in crate::harness) observed: accounts_pool::UsageObservation,
    pub(in crate::harness) context_digest: String,
}
pub(in crate::harness) fn native_usage_route(home: &Path, machine_home: &Path) -> String {
    resolve_usage_url(home, machine_home)
}
pub(in crate::harness) fn native_usage_probe(
    grant: &NativeUsageGrant<'_>,
    allow_proactive: bool,
) -> Option<NativeUsageProbe> {
    grant.check().ok()?;
    let stored = stored_auth_from_doc(aterm_json::from_slice(grant.document()).ok()?)?;
    check_stored_identity(&stored, grant.expected_email()).ok()?;
    let token_url = format!("{DEFAULT_ISSUER}/oauth/token");
    let context = CredentialContext::for_login(&stored, grant.route(), &token_url);
    let context_digest = aterm_codec::hex::encode_lower(&context.0);
    if allow_proactive && needs_proactive_refresh(&stored, grant.now()) {
        return Some(NativeUsageProbe {
            proactive: true,
            status: None,
            observed: accounts_pool::UsageObservation::unread(),
            context_digest,
        });
    }
    grant.contact().ok()?;
    let (status, body) =
        get_usage_once(grant.route(), &stored, grant.left(), grant.expected_email())?;
    grant.check().ok()?;
    Some(NativeUsageProbe {
        proactive: false,
        status: Some(status),
        observed: accounts_pool::UsageObservation::from_native(
            usage_reading(status, &body, grant.now()),
            Some(context),
        ),
        context_digest,
    })
}

/// Explicitly unmanaged reads retain observational compatibility, but this
/// scoped doorway never renews an unowned token. Native/historical sources are
/// rejected by canonical classification before reaching it.
pub(in crate::harness) struct LegacyUsageRead<'a> {
    pub(in crate::harness) home: &'a Path,
    pub(in crate::harness) machine_home: &'a Path,
    pub(in crate::harness) wait: Duration,
    pub(in crate::harness) now: i64,
    pub(in crate::harness) expected_email: Option<&'a str>,
    pub(in crate::harness) permit: &'a provider_read::Permit,
    pub(in crate::harness) scope: &'a account_work::Scope,
    pub(in crate::harness) on_contact: &'a dyn Fn(),
}
pub(in crate::harness) fn native_legacy_observation(
    read: LegacyUsageRead<'_>,
) -> accounts_pool::UsageObservation {
    let LegacyUsageRead {
        home,
        machine_home,
        wait,
        now,
        expected_email,
        permit,
        scope,
        on_contact,
    } = read;
    if scope.cancelled()
        || !permit.matches(provider_read::Read::CodexEndpoint, home)
        || !legacy_reader_allowed(home)
    {
        return accounts_pool::UsageObservation::unread();
    }
    let Some(stored) = load_stored_auth(home) else {
        return accounts_pool::UsageObservation::unread();
    };
    if check_stored_identity(&stored, expected_email).is_err() {
        return accounts_pool::UsageObservation::unread();
    }
    let route = resolve_usage_url(home, machine_home);
    if scope.cancelled() || !legacy_reader_allowed(home) {
        return accounts_pool::UsageObservation::unread();
    }
    on_contact();
    permit.note_contact(now);
    let Some((status, body)) = get_usage_once(&route, &stored, wait, expected_email) else {
        return accounts_pool::UsageObservation::unread();
    };
    if scope.cancelled() || !legacy_reader_allowed(home) {
        return accounts_pool::UsageObservation::unread();
    }
    accounts_pool::UsageObservation::from_native(
        usage_reading(status, &body, now),
        Some(CredentialContext::for_login(
            &stored,
            &route,
            &format!("{DEFAULT_ISSUER}/oauth/token"),
        )),
    )
}
