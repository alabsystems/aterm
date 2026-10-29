// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One explicit, root-checked renewal of an existing local Claude row. Root
//! resolution is supplied by the composing executable, not by provider I/O.

use super::*;
use accounts::shared::{self, Source};

/// Terms supplied only after the composing adapter has checked its root record
/// and owned configuration. These fields alone are not authority.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Grant {
    pub source: Source,
    pub decision: [u8; 32],
    pub generation: u64,
    pub configuration: [u8; 32],
    pub identity: String,
    pub expected_principal: Option<[u8; 32]>,
    /// A hard profile requirement from the approved configuration, distinct
    /// from an optional local preference. It must match the selected route.
    pub required_browser_profile: Option<String>,
}

/// The root/application check, independent of provider and filesystem I/O.
pub trait Authority {
    fn prepare(&self, row: &accounts::Row) -> Result<Grant, String>;
    fn current(&self, grant: &Grant, row: &accounts::Row) -> Result<(), String>;
}

pub(super) enum Guard<'a> {
    Local,
    Shared {
        authority: &'a dyn Authority,
        request: [u8; 16],
        grant: Option<Box<Grant>>,
        token: Option<shared::Token>,
    },
}

impl Guard<'_> {
    pub(super) fn retains_staging(&self) -> bool {
        matches!(self, Self::Shared { .. })
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AccountSharedBrowser",
            action = "OptionalDefault",
            project = "aterm_agent::harness::cli::tests::shared_renewal_tests::browser_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AccountSharedBrowser",
            action = "RequiredDefault",
            project = "aterm_agent::harness::cli::tests::shared_renewal_tests::browser_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AccountSharedBrowser",
            action = "OptionalRemote",
            project = "aterm_agent::harness::cli::tests::shared_renewal_tests::browser_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AccountSharedBrowser",
            action = "RequiredRemote",
            project = "aterm_agent::harness::cli::tests::shared_renewal_tests::browser_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AccountSharedBrowser",
            action = "OptionalProfile",
            project = "aterm_agent::harness::cli::tests::shared_renewal_tests::browser_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AccountSharedBrowser",
            action = "WrongProfile",
            project = "aterm_agent::harness::cli::tests::shared_renewal_tests::browser_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AccountSharedBrowser",
            action = "RequiredProfile",
            project = "aterm_agent::harness::cli::tests::shared_renewal_tests::browser_transition"
        )
    )]
    pub(super) fn preflight(
        &mut self,
        row: &accounts::Row,
        logins: &accounts::Logins,
        browser: &RenewBrowser,
    ) -> Result<(), String> {
        match self {
            Self::Local => unmanaged(logins, row, None, false),
            Self::Shared {
                authority, grant, ..
            } => {
                let approved = authority.prepare(row)?;
                let expected = row.expected_identity_email().ok_or_else(|| {
                    "shared renewal requires an explicit provider identity".to_owned()
                })?;
                if !accounts::email_matches(expected, &approved.identity)
                    || logins
                        .shared
                        .required(row)?
                        .is_some_and(|source| source != approved.source)
                {
                    return Err("shared renewal identity differs from its local custody".into());
                }
                logins.shared.check_preparation(
                    approved.source,
                    approved.generation,
                    approved.expected_principal,
                )?;
                if let Some(required) = approved.required_browser_profile.as_deref() {
                    match browser {
                        RenewBrowser::Profile { profile, .. }
                            if !required.trim().is_empty() && profile == required => {}
                        RenewBrowser::Default => return Err("shared renewal requires a local browser profile, but its opener is unavailable".into()),
                        RenewBrowser::Remote { .. } => return Err("shared renewal cannot enforce the required browser profile over a remote route".into()),
                        RenewBrowser::Profile { .. } => return Err("shared renewal selected a different browser profile than the approved requirement".into()),
                    }
                }
                *grant = Some(Box::new(approved));
                Ok(())
            }
        }
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AccountSharedBegin",
            action = "Current",
            project = "aterm_agent::harness::cli::tests::shared_renewal_tests::begin_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AccountSharedBegin",
            action = "Stopped",
            project = "aterm_agent::harness::cli::tests::shared_renewal_tests::begin_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AccountSharedBegin",
            action = "ChangedHead",
            project = "aterm_agent::harness::cli::tests::shared_renewal_tests::begin_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AccountSharedBegin",
            action = "Unavailable",
            project = "aterm_agent::harness::cli::tests::shared_renewal_tests::begin_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AccountSharedBegin",
            action = "Unrelated",
            project = "aterm_agent::harness::cli::tests::shared_renewal_tests::begin_transition"
        )
    )]
    pub(super) fn begin(
        &mut self,
        store: &accounts::Store,
        policy: accounts::RenewalPolicy<'_>,
        staging: &str,
        now: i64,
    ) -> Result<accounts::Logins, String> {
        self.begin_with(store, policy, staging, now, || {})
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AccountRenewalSnapshot",
            action = "Begin",
            project = "aterm_agent::harness::cli::tests::shared_renewal_tests::snapshot_transition"
        )
    )]
    pub(super) fn begin_with(
        &mut self,
        store: &accounts::Store,
        policy: accounts::RenewalPolicy<'_>,
        staging: &str,
        now: i64,
        after_commit: impl FnOnce(),
    ) -> Result<accounts::Logins, String> {
        if matches!(self, Self::Shared { grant: None, .. }) {
            return Err("shared renewal was not checked".to_owned());
        }
        // Keep the committed generation through the returned read. A later writer
        // must not become this attempt's parent while its grant stays unchanged.
        let held = store
            .acquire_within(&accounts::LOCK_LADDER)
            .map_err(|why| why.to_string())?
            .ok_or_else(|| accounts::LOCK_HELD.to_owned())?;
        match self {
            Self::Local => {
                store.transact_renewal_held(&held, now, &mut None, policy, |_, logins| {
                    unmanaged(logins, policy.row, None, false)?;
                    Ok(Vec::new())
                })?;
            }
            Self::Shared {
                authority,
                request,
                grant,
                token,
            } => {
                let approved = grant
                    .as_ref()
                    .ok_or_else(|| "shared renewal was not checked".to_owned())?;
                let mut begun = None;
                store.transact_renewal_held(&held, now, &mut None, policy, |_, logins| {
                    // Check after the account writer wait, before consuming the grant.
                    authority.current(approved, policy.row)?;
                    begun = Some(logins.shared.begin(
                        shared::Intent {
                            source: approved.source,
                            decision: approved.decision,
                            generation: approved.generation,
                            configuration: approved.configuration,
                            request: *request,
                            label: policy.row.label.clone(),
                            identity: approved.identity.clone(),
                            staging: staging.to_owned(),
                            before_generation:
                                logins.get(&policy.row.label).map(|b| b.renewal_generation),
                            begun_at: now,
                        },
                        approved.expected_principal,
                    )?);
                    Ok(vec![
                        accounts::Event::new(now, accounts::Kind::SignIn, &policy.row.label)
                            .because("shared preparation begun"),
                    ])
                })?;
                *token = begun;
            }
        }
        after_commit();
        store.load_logins(now).map(|(logins, _)| logins)
    }

    pub(super) fn check(&self, row: &accounts::Row) -> Result<(), String> {
        if let Self::Shared {
            authority, grant, ..
        } = self
        {
            authority.current(
                grant
                    .as_ref()
                    .ok_or_else(|| "shared preparation was not begun".to_owned())?,
                row,
            )?;
        }
        Ok(())
    }

    /// Observe the captured row, parent and authority after waiting for the writer.
    pub(super) fn check_policy(
        &self,
        store: &accounts::Store,
        policy: accounts::RenewalPolicy<'_>,
        now: i64,
    ) -> Result<(), String> {
        store
            .transact_renewal_within(&accounts::LOCK_LADDER, now, &mut None, policy, |_, _| {
                self.check(policy.row)?;
                Ok(Vec::new())
            })
            .map(|_| ())
    }

    pub(super) fn publish(
        &mut self,
        logins: &mut accounts::Logins,
        row: &accounts::Row,
        principal: Option<[u8; 32]>,
        now: i64,
    ) -> Result<(), String> {
        match self {
            Self::Local => unmanaged(logins, row, principal, true),
            Self::Shared {
                authority,
                grant,
                token,
                ..
            } => {
                let grant = grant
                    .as_ref()
                    .ok_or_else(|| "shared preparation was not begun".to_owned())?;
                // Recheck after acquiring the account writer as well: waiting
                // for that lock must not reuse a pre-wait authority observation.
                authority.current(grant, row)?;
                let principal = principal
                    .ok_or_else(|| "shared renewal has no full provider identity".to_owned())?;
                if grant
                    .expected_principal
                    .is_some_and(|expected| expected != principal)
                {
                    return Err("shared renewal did not prove the approved provider account".into());
                }
                let generation = logins
                    .generation_clock
                    .checked_add(1)
                    .ok_or_else(|| "local binding generation exhausted".to_owned())?;
                logins.shared.finish(
                    token
                        .take()
                        .ok_or_else(|| "shared preparation was already completed".to_owned())?,
                    shared::Outcome::Published { generation },
                    Some((&grant.identity, principal)),
                    now,
                )
            }
        }
    }

    fn failed(&mut self, store: &accounts::Store, now: i64, err: &mut dyn Write) {
        if let Self::Shared { token, .. } = self
            && let Some(token) = token.take()
            && let Err(why) =
                store.transact_logins_within(&accounts::LOCK_LADDER, now, &mut None, |_, logins| {
                    logins
                        .shared
                        .finish(token, shared::Outcome::Unknown, None, now)?;
                    Ok(Vec::new())
                })
        {
            let _ = writeln!(err, "shared renewal outcome remains unknown: {why}");
        }
    }
}

pub(in crate::harness) fn unmanaged(
    logins: &accounts::Logins,
    row: &accounts::Row,
    principal: Option<[u8; 32]>,
    publishing: bool,
) -> Result<(), String> {
    if logins.shared.required(row)?.is_some()
        || principal.is_some_and(|p| logins.shared.principal_source(p).is_some())
    {
        return Err("this account requires a fresh root-authorized shared renewal".into());
    }
    if publishing && !logins.shared.is_empty() && principal.is_none() {
        return Err(
            "provider identity is unavailable in a store with shared account custody".into(),
        );
    }
    Ok(())
}

/// Run exactly one renewal. No fallback or retry re-enters a consumed grant.
/// The provider's own sign-in may still require a person at the terminal.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountRenewalIdentity",
        action = "Replay",
        project = "aterm_agent::harness::cli::tests::renewal_identity::renewal_identity_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSharedCheckpoint",
        action = "Fail",
        project = "aterm_agent::harness::cli::tests::shared_renewal_tests::checkpoint_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSharedCheckpoint",
        action = "Supersede",
        project = "aterm_agent::harness::cli::tests::shared_renewal_tests::checkpoint_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSharedRenewal",
        action = "Good",
        project = "aterm_agent::harness::cli::tests::shared_renewal_tests::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSharedRenewal",
        action = "StoppedBefore",
        project = "aterm_agent::harness::cli::tests::shared_renewal_tests::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSharedRenewal",
        action = "StoppedDuring",
        project = "aterm_agent::harness::cli::tests::shared_renewal_tests::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSharedRenewal",
        action = "WrongPrincipal",
        project = "aterm_agent::harness::cli::tests::shared_renewal_tests::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSharedRenewal",
        action = "ChangedRow",
        project = "aterm_agent::harness::cli::tests::shared_renewal_tests::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSharedRenewal",
        action = "Crash",
        project = "aterm_agent::harness::cli::tests::shared_renewal_tests::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSharedRenewal",
        action = "ChangedHead",
        project = "aterm_agent::harness::cli::tests::shared_renewal_tests::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSharedRenewal",
        action = "MissingPrincipal",
        project = "aterm_agent::harness::cli::tests::shared_renewal_tests::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSharedRenewal",
        action = "Replay",
        project = "aterm_agent::harness::cli::tests::shared_renewal_tests::transition"
    )
)]
pub fn run(
    env: &Env,
    host: &dyn AccountsHost,
    label: &str,
    request: [u8; 16],
    authority: &dyn Authority,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> ExitCode {
    if request == [0; 16] {
        let _ = writeln!(err, "shared renewal requires a nonzero request id");
        return ExitCode::from(2);
    }
    let Some(path) = accounts_roster_path_for(Cli::Claude, env, err) else {
        return ExitCode::from(1);
    };
    let store = env.accounts_store();
    let Some(records) = read_renewal_records(Cli::Claude, &store, &path, env.now, false, err)
    else {
        return ExitCode::from(1);
    };
    let Some(row) = records.roster.row(label) else {
        let _ = writeln!(err, "shared renewal requires an existing local row");
        return ExitCode::from(1);
    };
    if row.email.is_none() || !host.stdin_is_terminal() {
        let _ = writeln!(
            err,
            "shared renewal requires a named local row and an interactive terminal"
        );
        return ExitCode::from(1);
    }
    if let Some(why) = host.unaskable() {
        let _ = writeln!(err, "shared renewal cannot check its login: {why}");
        return ExitCode::from(1);
    }
    let lock = match signin_guard(&store, label, "shared-renew", out, err) {
        Ok(lock) => lock,
        Err(_) => return ExitCode::from(1),
    };
    let opener = opener_link(&store, host);
    let browser = if host.under_ssh() {
        RenewBrowser::Remote { opener }
    } else {
        match (row.browser.as_ref(), opener) {
            (Some(profile), Some(opener)) => RenewBrowser::Profile {
                opener,
                profile: profile.clone(),
            },
            _ => RenewBrowser::Default,
        }
    };
    let policy = accounts::RenewalPolicy {
        roster_path: &path,
        roster: &records.raw_roster,
        row,
        logins: &records.logins,
    };
    let own_dir = records.raw_roster.row(label).and_then(|r| r.dir.as_deref());
    let mut guard = Guard::Shared {
        authority,
        request,
        grant: None,
        token: None,
    };
    let result = renew_one_guarded(
        env, host, &store, &lock, policy, own_dir, &browser, out, err, &mut guard,
    );
    if result == Renewal::Renewed {
        ExitCode::SUCCESS
    } else {
        guard.failed(&store, host.now().max(env.now), err);
        ExitCode::from(1)
    }
}

/// The upkeep already holds sign-in admission and has selected an unattended
/// route. It uses the same grant consumption and publication as explicit renewal.
pub(in crate::harness) fn background(
    env: &Env,
    host: &dyn AccountsHost,
    store: &accounts::Store,
    signin: &accounts::Lock,
    policy: accounts::RenewalPolicy<'_>,
    browser: &RenewBrowser,
    authority: &dyn Authority,
) -> Renewal {
    let mut request = [0; 16];
    if aterm_uds::rand::fill(&mut request).is_err() || request == [0; 16] {
        return Renewal::Failed;
    }
    let mut guard = Guard::Shared {
        authority,
        request,
        grant: None,
        token: None,
    };
    let own_dir = policy
        .roster
        .row(&policy.row.label)
        .and_then(|row| row.dir.as_deref());
    let result = renew_one_guarded(
        env,
        host,
        store,
        signin,
        policy,
        own_dir,
        browser,
        &mut io::sink(),
        &mut io::sink(),
        &mut guard,
    );
    if result != Renewal::Renewed {
        guard.failed(store, host.now().max(env.now), &mut io::sink());
    }
    result
}
