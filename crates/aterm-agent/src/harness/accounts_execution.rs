// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Exact account intent for a retained command. Admission never selects another row.

use super::*;

/// Non-secret account attribution captured when a command is constructed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExecutionIntent {
    /// A positively observed custom home, credential override or provider route.
    External {
        /// Only non-secret home/login exports or removals carried by the command.
        context: std::collections::BTreeMap<String, Option<String>>,
        /// What the replaced agent's own exec environment recorded of its shell's, for an agent
        /// on its stored login ([`accounts::LaunchEnv`]): the base [`Self::external_environment`]
        /// reads where the shell's own environment reads empty.
        launch: Option<accounts::LaunchEnv>,
    },
    /// A stored-login route in one exact local account namespace.
    Stored {
        /// Fingerprint of provider and local account roots, never a peer-supplied path.
        namespace: String,
        /// The selector the retained command carries; absent means the default login.
        login: Option<String>,
        /// The managed row, or an unowned stored login.
        owner: Option<String>,
        /// Credential generation and sign-in provenance, including for unowned logins.
        generation: String,
    },
    /// A vault-token route (`docs/DESIGN-claude-setup-token-2026-11-05.md` §5.2) to one
    /// account's token entry, in one exact local account namespace. Admission checks the row's
    /// custody and claims, the token's standing here and its record's email, never a stored
    /// login: a token row may have none on this Mac.
    Token {
        /// Fingerprint of provider and local account roots, never a peer-supplied path.
        namespace: String,
        /// The selector the retained command carries; absent means the default login.
        login: Option<String>,
        /// The managed row, or `None` for an account no row names (its login record's).
        owner: Option<String>,
        /// The token entry the wrapper serves.
        entry: accounts::TokenEntry,
    },
}

/// Holds the account writer through the caller's execution-grant decision.
/// Dropping it ends admission; retaining only the intent is never a live grant.
#[derive(Debug)]
pub struct ExecutionGuard {
    _held: Option<accounts::Lock>,
}

impl ExecutionIntent {
    /// An observed external route that inherits its environment.
    #[must_use]
    pub fn external() -> Self {
        Self::External {
            context: Default::default(),
            launch: None,
        }
    }

    /// Bind only the non-secret directory environment selected for this command, and the
    /// launch environment its agent recorded ([`accounts::LaunchEnv`], `None` where none was).
    pub fn bind_external_environment(
        &mut self,
        env: &[(String, String)],
        unset: Option<&[String]>,
        recorded: Option<&accounts::LaunchEnv>,
    ) {
        let Self::External { context, launch } = self else {
            return;
        };
        *launch = recorded.cloned();
        context.clear();
        for (name, value) in env {
            if directory_variable(name) {
                context.insert(name.clone(), Some(value.clone()));
            }
        }
        for name in unset.into_iter().flatten() {
            if directory_variable(name) {
                context.insert(name.clone(), None);
            }
        }
    }

    /// Apply the recorded directory exports to a live shell environment. Secret
    /// values remain transient input; none are copied into the intent. Where `inherited` is
    /// empty (macOS gives no other process the environment of an Apple binary such as
    /// `/bin/zsh`), the base is the launch environment the agent recorded, which is what that
    /// shell passed it.
    #[must_use]
    pub fn external_environment(&self, inherited: &[String]) -> Option<Vec<String>> {
        let Self::External { context, launch } = self else {
            return None;
        };
        let base = match launch {
            Some(recorded) if inherited.is_empty() => recorded.entries(),
            _ => inherited,
        };
        let mut env = base
            .iter()
            .filter(|entry| {
                !entry
                    .split_once('=')
                    .is_some_and(|(name, _)| context.contains_key(name))
            })
            .cloned()
            .collect::<Vec<_>>();
        env.extend(
            context
                .iter()
                .filter_map(|(name, value)| value.as_ref().map(|value| format!("{name}={value}"))),
        );
        Some(env)
    }
    /// The token entry of the account this intent names, where the caller can resolve it:
    /// a token intent's own, a stored one's owner row's in `roster` ([`accounts::TokenEntry::of_row`]),
    /// none for an external route or an unowned login.
    #[must_use]
    pub fn owner_entry(&self, roster: &Roster) -> Option<accounts::TokenEntry> {
        match self {
            Self::External { .. } => None,
            Self::Stored { owner, .. } => owner
                .as_deref()
                .and_then(|label| roster.row(label))
                .and_then(accounts::TokenEntry::of_row),
            Self::Token { entry, .. } => Some(*entry),
        }
    }

    /// Match independently observed execution-environment attribution. A route
    /// formerly external cannot fall back to a pooled login, and a stored or token route
    /// must still select exactly the retained login under the same user home, on either of the
    /// row's routes: its stored login, or its vault token where the marker names `owner_entry`,
    /// the intent's account's entry ([`Self::owner_entry`]; a token intent names its own). So a
    /// wrapper's fallback to the stored login, or the twin's own judgment at exec, never reads
    /// as a launch on another account.
    #[must_use]
    pub fn matches_process(
        &self,
        who: &accounts::ProcessLogin,
        same_user_home: bool,
        owner_entry: Option<accounts::TokenEntry>,
    ) -> bool {
        let (login, entry) = match self {
            Self::External { .. } => {
                return !same_user_home
                    || !who.default_home
                    || match who.route {
                        accounts::AuthRoute::Stored | accounts::AuthRoute::VaultToken { .. } => {
                            false
                        }
                        accounts::AuthRoute::Outside => true,
                    };
            }
            Self::Stored { login, .. } => (login, owner_entry),
            Self::Token { login, entry, .. } => (login, Some(*entry)),
        };
        let on_row = match who.route {
            accounts::AuthRoute::Stored => true,
            accounts::AuthRoute::VaultToken { entry: named } => Some(named) == entry,
            accounts::AuthRoute::Outside => false,
        };
        same_user_home
            && who.default_home
            && on_row
            && match (
                who.login.as_deref().filter(|s| !s.is_empty()),
                login.as_deref(),
            ) {
                (None, None) => true,
                (Some(actual), Some(expected)) => accounts::canonical_directory(Path::new(actual))
                    .ok()
                    .zip(accounts::canonical_directory(Path::new(expected)).ok())
                    .is_some_and(|(actual, expected)| actual == expected),
                _ => false,
            }
    }

    /// Canonical storage containing no credential values or file contents.
    #[must_use]
    pub fn to_value(&self) -> aterm_json::Value {
        match self {
            Self::External { context, launch } => {
                let mut value =
                    aterm_json::json!({"v": 1, "route": "external", "context": context});
                if let (Some(recorded), Some(fields)) = (launch, value.as_object_mut()) {
                    fields.insert("launch".into(), aterm_json::json!(recorded.entries()));
                }
                value
            }
            Self::Stored {
                namespace,
                login,
                owner,
                generation,
            } => aterm_json::json!({
                "v": 1, "route": "stored", "namespace": namespace,
                "login": login, "owner": owner, "generation": generation,
            }),
            Self::Token {
                namespace,
                login,
                owner,
                entry,
            } => aterm_json::json!({
                "v": 1, "route": "token", "namespace": namespace,
                "login": login, "owner": owner, "entry": entry.name(),
            }),
        }
    }

    /// Accept only the exact known schema; malformed and extended records convey no authority.
    #[must_use]
    pub fn from_value(value: &aterm_json::Value) -> Option<Self> {
        fn digest(value: &aterm_json::Value) -> Option<String> {
            let word = value.as_str()?;
            (word.len() == 64
                && word
                    .bytes()
                    .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()))
            .then(|| word.to_owned())
        }
        let intent = match value.get("route")?.as_str()? {
            "external" => {
                let mut context = std::collections::BTreeMap::new();
                for (name, value) in value.get("context")?.as_object()? {
                    if !directory_variable(name) {
                        return None;
                    }
                    let value = if value.is_null() {
                        None
                    } else {
                        let word = value.as_str()?;
                        if word.len() > super::super::native_act::MAX_PATH
                            || word.contains(['\0', '\n', '\r'])
                        {
                            return None;
                        }
                        Some(word.to_owned())
                    };
                    context.insert(name.clone(), value);
                }
                // An absent record is an agent no launch environment was recorded for; a
                // present one must read back as a stored login's ([`accounts::LaunchEnv`]).
                let launch = match value.get("launch") {
                    None => None,
                    Some(words) => {
                        let words = words
                            .as_array()?
                            .iter()
                            .map(|word| {
                                let word = word.as_str()?;
                                (word.len() <= super::super::native_act::MAX_PATH
                                    && !word.contains(['\0', '\n', '\r']))
                                .then(|| word.to_owned())
                            })
                            .collect::<Option<Vec<_>>>()?;
                        Some(accounts::LaunchEnv::of_record(&words)?)
                    }
                };
                Self::External { context, launch }
            }
            route @ ("stored" | "token") => {
                let login = match value.get("login")? {
                    aterm_json::Value::Null => None,
                    value => {
                        let login = value.as_str()?;
                        if login.is_empty()
                            || login.len() > super::super::native_act::MAX_PATH
                            || login.contains(['\0', '\n', '\r'])
                        {
                            return None;
                        }
                        Some(login.to_owned())
                    }
                };
                let owner = match value.get("owner")? {
                    aterm_json::Value::Null => None,
                    value => {
                        let label = value.as_str()?;
                        if label.is_empty()
                            || label.len() > 256
                            || label.contains(['\0', '\n', '\r'])
                        {
                            return None;
                        }
                        Some(label.to_owned())
                    }
                };
                if route == "token" {
                    Self::Token {
                        namespace: digest(value.get("namespace")?)?,
                        login,
                        owner,
                        entry: accounts::TokenEntry::parse(value.get("entry")?.as_str()?)?,
                    }
                } else {
                    Self::Stored {
                        namespace: digest(value.get("namespace")?)?,
                        login,
                        owner,
                        generation: digest(value.get("generation")?)?,
                    }
                }
            }
            _ => return None,
        };
        (intent.to_value() == *value).then_some(intent)
    }
}

fn directory_variable(name: &str) -> bool {
    [
        accounts::HOME_ENV,
        accounts::HOMEDIR_ENV,
        accounts::LOGIN_ENV,
    ]
    .contains(&name)
}

fn namespace(seat: &Seat<'_>, cli: Cli) -> Result<String, String> {
    let mut digest = aterm_digest::Sha256::new();
    digest.update(b"aterm.account-execution-namespace.v1\0");
    for field in std::iter::once(cli.id().as_bytes().to_vec()).chain(
        [seat.config_dir, seat.state, seat.home]
            .into_iter()
            .map(accounts::canonical_directory)
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .map(|path| path.as_os_str().as_encoded_bytes().to_vec()),
    ) {
        digest.update((field.len() as u64).to_be_bytes());
        digest.update(field);
    }
    Ok(aterm_codec::hex::encode_lower(&digest.finalize()))
}

// Same captured profile bytes, with unknown evidence kept distinct from absence.
fn bind_identity_evidence(
    digest: &mut aterm_digest::Sha256,
    seat: &Seat<'_>,
    cli: Cli,
    login: Option<&str>,
    row: Option<&Row>,
) -> Result<(), String> {
    if cli != Cli::Claude {
        return Ok(());
    }
    let email = row.and_then(Row::expected_identity_email).unwrap_or("");
    let evidence = match login.filter(|path| !path.is_empty()) {
        Some(path) => super::super::profile::identity_evidence_in_home(Path::new(path), email),
        None => super::super::profile::default_identity_evidence(seat.home, email),
    }
    .map_err(|error| error.to_string())?;
    digest.update(b"aterm.account-execution-local-identity.v1\0");
    match evidence {
        super::super::profile::IdentityEvidence::Missing => digest.update([0]),
        super::super::profile::IdentityEvidence::Present {
            observation,
            email_digest,
        } => {
            digest.update([1]);
            digest.update([match observation.email_matches {
                None => 0,
                Some(false) => 1,
                Some(true) => 2,
            }]);
            for known in [observation.principal, email_digest] {
                match known {
                    None => digest.update([0]),
                    Some(value) => {
                        digest.update([1]);
                        digest.update(value);
                    }
                }
            }
        }
    }
    Ok(())
}

// The caller retains this writer. Unlike snapshot_held, this never replays a publication.
fn execution_snapshot_readonly(
    seat: &Seat<'_>,
    cli: Cli,
    store: &Store,
    _held: &accounts::Lock,
) -> Result<PoolSnapshot, String> {
    let receipt = store.sync_read_receipt()?;
    let raw = seat.roster(cli)?;
    let (logins, _) = store.load_logins(seat.now)?;
    let (mut state, _) = store.load(seat.now)?;
    let roster = accounts::bound(&raw, &logins);
    store.hydrate_auth_contexts(&mut state, &roster);
    store.check_sync_read_receipt(&receipt)?;
    Ok(PoolSnapshot {
        roster,
        logins,
        state,
    })
}

fn binding(
    seat: &Seat<'_>,
    cli: Cli,
    snapshot: &PoolSnapshot,
    row: &Row,
) -> Result<String, String> {
    let home = accounts::canonical_directory(seat.home)?;
    let canonical = Seat {
        config_dir: seat.config_dir,
        state: seat.state,
        home: &home,
        now: seat.now,
    };
    let mut digest = aterm_digest::Sha256::new();
    digest.update(b"aterm.account-execution-binding.v1\0");
    digest.update(
        sample_context(&canonical, cli, row, &snapshot.logins)
            .0
            .as_bytes(),
    );
    // An absent/legacy generation cannot identify a unique credential parent.
    if snapshot
        .logins
        .get(&row.label)
        .is_none_or(|binding| binding.renewal_generation == 0)
    {
        digest.update(snapshot.logins.generation_clock.to_be_bytes());
    }
    digest.update(
        snapshot
            .logins
            .sign_in_receipts
            .get(&row.label)
            .copied()
            .unwrap_or(0)
            .to_be_bytes(),
    );
    bind_identity_evidence(&mut digest, seat, cli, row.dir.as_deref(), Some(row))?;
    Ok(aterm_codec::hex::encode_lower(&digest.finalize()))
}

fn unowned_generation(
    seat: &Seat<'_>,
    cli: Cli,
    snapshot: &PoolSnapshot,
    login: Option<&str>,
) -> Result<String, String> {
    let mut digest = aterm_digest::Sha256::new();
    digest.update(b"aterm.account-execution-unowned.v1\0");
    digest.update(snapshot.logins.generation_clock.to_be_bytes());
    digest.update(snapshot.logins.sign_in_clock.to_be_bytes());
    bind_identity_evidence(&mut digest, seat, cli, login, None)?;
    Ok(aterm_codec::hex::encode_lower(&digest.finalize()))
}

pub(super) fn capture_held(
    seat: &Seat<'_>,
    cli: Cli,
    who: &accounts::ProcessLogin,
    snapshot: &PoolSnapshot,
) -> Result<ExecutionIntent, String> {
    let token = match who.route {
        accounts::AuthRoute::Stored => None,
        accounts::AuthRoute::VaultToken { entry } => Some(entry),
        accounts::AuthRoute::Outside => {
            return Err("execution route is not the account's own credential".into());
        }
    };
    let login = who
        .login
        .as_deref()
        .filter(|login| !login.is_empty())
        .map(|login| {
            accounts::canonical_directory(Path::new(login))?
                .into_os_string()
                .into_string()
                .map_err(|_| "execution login is not UTF-8".to_owned())
        })
        .transpose()?;
    let default = accounts::canonical_directory(&seat.home.join(cli.accounts().default_home))?;
    let directory = login.as_deref().map(Path::new).unwrap_or(&default);
    let (config, issues) = crate::supervise::config::SupervisorConfig::from_path(Some(
        &seat.config_dir.join("aterm.toml"),
    ));
    if !issues.is_empty() {
        return Err("execution selection configuration is unavailable or invalid".into());
    }
    let mode = config.accounts_for(cli.program());
    let enrolled = accounts::enrolled_in(cli, &snapshot.roster, seat.home);
    let pin = launch_pin(cli, &snapshot.roster, &snapshot.state, &enrolled, seat.now)?;
    let mut label = None;
    for row in &snapshot.roster.rows {
        let row_directory =
            accounts::canonical_directory(row.dir.as_deref().map(Path::new).unwrap_or(&default))?;
        if row_directory == directory && label.replace(row.label.as_str()).is_some() {
            return Err("execution login has ambiguous account ownership".into());
        }
    }
    if pin.is_some_and(|pin| Some(pin.label.as_str()) != label)
        || snapshot
            .state
            .pending_pin
            .as_ref()
            .is_some_and(|selection| {
                selection
                    .account()
                    .is_some_and(|pin| Some(pin.label()) != label)
            })
        || (mode == Mode::Off && pin.is_none() && login.is_some())
    {
        return Err("execution no longer matches the selected account policy".into());
    }
    let (owner, generation) = match label {
        None => {
            if accounts::retained_directory_owner_at(
                &snapshot.logins,
                &snapshot.roster.rows,
                "",
                directory.to_str().ok_or("execution login is not UTF-8")?,
                Some(&default),
            )?
            .is_some()
            {
                return Err("execution login has no current account binding".into());
            }
            match token {
                None => (
                    None,
                    Some(unowned_generation(seat, cli, snapshot, login.as_deref())?),
                ),
                // An account no row names: the marker must name the entry of the account the
                // session's own login record names (§5.2).
                Some(entry) => {
                    let named = super::super::profile::login_record(seat.home, login.as_deref())
                        .and_then(|record| accounts::TokenEntry::of_email(&record.email));
                    if named != Some(entry) {
                        return Err("execution token names another account's entry".into());
                    }
                    (None, None)
                }
            }
        }
        Some(label) => {
            let row = snapshot
                .roster
                .row(label)
                .ok_or("execution account disappeared")?;
            if row.held_by.is_some() {
                return Err("execution account is held by another machine".into());
            }
            snapshot
                .state
                .lease_verdict(label, seat.now)
                .map_err(|why| format!("execution account: {}", why.as_str()))?;
            let condition =
                accounts::condition(row, &snapshot.state, enrolled.contains(label), seat.now);
            if matches!(condition, accounts::Condition::Limited { .. })
                && mode != Mode::Off
                && pin.is_none()
            {
                return Err("execution account is at its limit".into());
            }
            match token {
                None => {
                    if matches!(
                        condition,
                        accounts::Condition::NotEnrolled | accounts::Condition::SignedOut
                    ) {
                        return Err("execution account is not signed in".into());
                    }
                    check_login_identity(cli, row, seat.home).map_err(|why| why.to_string())?;
                    (
                        Some(label.to_owned()),
                        Some(binding(seat, cli, snapshot, row)?),
                    )
                }
                // No signed-out check: a token row may have no stored login on this Mac.
                Some(entry) => {
                    if accounts::TokenEntry::of_row(row) != Some(entry) {
                        return Err("execution token names another account's entry".into());
                    }
                    (Some(label.to_owned()), None)
                }
            }
        }
    };
    let (final_config, final_issues) = crate::supervise::config::SupervisorConfig::from_path(Some(
        &seat.config_dir.join("aterm.toml"),
    ));
    if !final_issues.is_empty()
        || final_config.accounts_for(cli.program()) != mode
        || seat.keyring_held(cli)
    {
        return Err("execution account controls changed during admission".into());
    }
    let namespace = namespace(seat, cli)?;
    Ok(match (token, generation) {
        (Some(entry), _) => ExecutionIntent::Token {
            namespace,
            login,
            owner,
            entry,
        },
        (None, Some(generation)) => ExecutionIntent::Stored {
            namespace,
            login,
            owner,
            generation,
        },
        (None, None) => return Err("execution stored login has no generation".into()),
    })
}

/// Capture an already selected login under the writer, without choosing a replacement.
/// The caller supplies positively observed authentication attribution.
///
/// # Errors
/// The store, selected account, custody, credential identity or binding cannot be admitted.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountExecution",
        action = "Capture",
        project = "harness::accounts_pool::tests::execution::transition"
    )
)]
pub fn capture_execution(
    seat: &Seat<'_>,
    cli: Cli,
    who: &accounts::ProcessLogin,
) -> Result<ExecutionIntent, String> {
    let stored = match who.route {
        accounts::AuthRoute::Stored => true,
        accounts::AuthRoute::VaultToken { .. } => false,
        accounts::AuthRoute::Outside => return Ok(ExecutionIntent::external()),
    };
    if !who.default_home {
        return Err("custom stored-login home requires directory attribution".into());
    }
    // The sign-in lock is a stored login's service; a token route never takes it (§5.8).
    if stored && seat.keyring_held(cli) {
        return Err("execution stored-login route is unavailable".into());
    }
    let store = seat.store(cli);
    let held = store
        .acquire_bounded()
        .map_err(|why| why.to_string())?
        .ok_or(accounts::LOCK_HELD)?;
    let snapshot = snapshot_held(seat, cli, &store, &held)?;
    capture_held(seat, cli, who, &snapshot)
}

/// Admit a custom stored-login route only while its effective credential
/// directories remain outside this machine's managed account namespace.
/// # Errors
/// A directory is unknown, aliases the default login or a retained account,
/// lies inside managed storage, or the account writer cannot be acquired.
pub fn admit_external_execution(
    seat: &Seat<'_>,
    cli: Cli,
    directories: &[PathBuf],
) -> Result<ExecutionGuard, String> {
    if directories.is_empty() {
        return Err("custom credential directory is unknown".into());
    }
    let store = seat.store(cli);
    let held = store
        .acquire_bounded()
        .map_err(|why| why.to_string())?
        .ok_or(accounts::LOCK_HELD)?;
    let snapshot = execution_snapshot_readonly(seat, cli, &store, &held)?;
    let default = accounts::canonical_directory(&seat.home.join(cli.accounts().default_home))?;
    let managed = accounts::canonical_directory(&store.logins_dir())?;
    for directory in directories {
        let path = accounts::canonical_directory(directory)?;
        if path == default
            || path.starts_with(&managed)
            || accounts::retained_directory_owner_at(
                &snapshot.logins,
                &snapshot.roster.rows,
                "",
                path.to_str()
                    .ok_or("custom credential directory is not UTF-8")?,
                Some(&default),
            )?
            .is_some()
        {
            return Err("custom credential directory overlaps managed accounts".into());
        }
    }
    Ok(ExecutionGuard { _held: Some(held) })
}

/// Authorize exactly the retained intent and keep the local account writer held.
/// A successful result grants no replacement row and does not outlive its guard.
///
/// # Errors
/// The namespace, account owner or credential generation changed, or current account policy
/// does not admit execution. Unknown and malformed intents must never call this API.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountExecution",
        action = "Hold",
        reason = "A cooperative account writer takes the same file lock; the conformance test attempts that writer while an execution guard exists"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountExecution",
        action = "Release",
        reason = "External account custody release under the account writer"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountExecution",
        action = "Renew",
        reason = "A cooperative credential-generation writer takes the same file lock"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountExecution",
        action = "Retry",
        reason = "Caller retries the unchanged retained intent after a refusal"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountExecution",
        action = "Finish",
        reason = "Caller drops the guard after making its execution-grant decision"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountExecution",
        action = "Admit",
        project = "harness::accounts_pool::tests::execution::transition"
    )
)]
pub fn admit_execution(
    seat: &Seat<'_>,
    cli: Cli,
    intent: &ExecutionIntent,
) -> Result<ExecutionGuard, String> {
    let (login, route) = match intent {
        ExecutionIntent::External { .. } => return Ok(ExecutionGuard { _held: None }),
        ExecutionIntent::Stored { login, .. } => (login, accounts::AuthRoute::Stored),
        ExecutionIntent::Token { login, entry, .. } => {
            (login, accounts::AuthRoute::VaultToken { entry: *entry })
        }
    };
    if route == accounts::AuthRoute::Stored && seat.keyring_held(cli) {
        return Err("execution stored-login route is unavailable".into());
    }
    let store = seat.store(cli);
    let held = store
        .acquire_bounded()
        .map_err(|why| why.to_string())?
        .ok_or(accounts::LOCK_HELD)?;
    let snapshot = execution_snapshot_readonly(seat, cli, &store, &held)?;
    let current = capture_held(
        seat,
        cli,
        &accounts::ProcessLogin {
            login: login.clone(),
            default_home: true,
            route,
        },
        &snapshot,
    )?;
    if current != *intent {
        return Err("execution account namespace or credential generation changed".into());
    }
    if let ExecutionIntent::Token { owner, entry, .. } = intent {
        let email = match owner
            .as_deref()
            .and_then(|label| snapshot.roster.row(label))
        {
            Some(row) => row.expected_identity_email().map(str::to_owned),
            None => super::super::profile::login_record(seat.home, login.as_deref())
                .map(|record| record.email),
        }
        .ok_or("execution token's account names no email")?;
        token_in_use_here(seat, &store, *entry, &email)?;
    }
    Ok(ExecutionGuard { _held: Some(held) })
}

/// Whether the token `entry` is in use on this Mac (its standing in this Mac's vault, under
/// `seat.home`, and the table's own facts: `active`, `soon`, `exposed`, or `unverified` on the
/// Mac that minted it) and its record names `email` (§5.2). The words name the state, never a
/// value.
#[cfg(unix)]
fn token_in_use_here(
    seat: &Seat<'_>,
    store: &Store,
    entry: accounts::TokenEntry,
    email: &str,
) -> Result<(), String> {
    use aterm_secrets::claude_token::{self, Lookup};
    let key = claude_token::Key::parse(entry.key().as_str())
        .ok_or("execution token entry is not a token's")?;
    let found = claude_token::standing(&aterm_secrets::client::Paths::for_home(seat.home), &key)
        .map_err(|why| format!("execution token: {why}"))?;
    let table = super::super::accounts_tokens::read_table(store)
        .map_err(|why| format!("execution token table: {why}"))?;
    let now = u64::try_from(seat.now).unwrap_or(0);
    let state =
        super::super::accounts_tokens::TokenState::of(&found, table.rows.get(key.as_str()), now);
    if !state.in_use() {
        return Err(format!(
            "execution token is not in use here: {}",
            state.words()
        ));
    }
    let named = match &found {
        Lookup::Found(standing) => standing
            .meta
            .as_ref()
            .is_some_and(|meta| accounts::email_matches(&meta.email, email)),
        Lookup::Absent | Lookup::Unknown => false,
    };
    if !named {
        return Err("execution token's record names another account".into());
    }
    Ok(())
}

/// The token route is macOS's for now (`spawnfd`'s exec keeps a named descriptor only there).
#[cfg(not(unix))]
fn token_in_use_here(
    _seat: &Seat<'_>,
    _store: &Store,
    _entry: accounts::TokenEntry,
    _email: &str,
) -> Result<(), String> {
    Err("execution token route is unavailable on this platform".into())
}
