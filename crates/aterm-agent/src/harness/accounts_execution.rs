// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Exact account intent for a retained command. Admission never selects another row.
//!
//! Admission checks two things, by the intent's [`Purpose`]. Integrity, which the
//! `AccountExecution` laws need, holds for every intent: the namespace, the canonical selector,
//! one unambiguous owner row, the retained directory owner, the owner row's custody (another
//! Mac's hold) and claims (the lease verdict: another Mac's senior claim, an off clock, a
//! credential held for preparation only or failed to prepare here, an unrecordable non-home
//! claim), the token entry, the login's identity, the credential generation, the account writer
//! and the controls rechecked at the end. Custody and claims say whether this Mac may run the
//! credential at all, which a relaunch starts afresh whatever it replaces: a session they refuse
//! yields at its turn end (`accounts::row_available_here`), so no relaunch starts a new turn on
//! it. Selection policy (signed out or not enrolled, a usage limit, the owner's pin or a pending
//! one, and `accounts = "off"` naming another login) is checked only for a [`Purpose::Select`]
//! intent: a [`Purpose::SameLogin`] relaunch replaces the process already running on that exact
//! credential and chooses nothing, and a [`Purpose::Rebind`] relaunch moves that process's
//! account off a login its row left onto the row's current one, so only that new login's sign-in
//! here is asked of it.
//!
//! A login no row holds now that a row or binding retains (the login a renewal or a sign-in moved
//! its row off, or one a withdrawn account kept) identifies an account but never authorizes
//! running that superseded credential again ([`RefusalKind::Retained`]): a relaunch moves onto the
//! row's current login instead ([`relaunch_rebind`]), as a cold relaunch and a turn end's login
//! follow do.

use super::*;

/// What an execution intent's admission checks ([`capture_execution`], [`admit_execution`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Purpose {
    /// The command keeps the exact credential the process it replaces runs on (the same stored
    /// login under the same home, or the same token route): integrity alone.
    SameLogin,
    /// The command moves the process it replaces, on a login its account's row left for a newer
    /// one ([`Row::was`]), onto that row's current login ([`relaunch_rebind`]): integrity, and the
    /// row's current stored login signed in here, since the credential changes. Never a usage
    /// limit or a pin: the account is the one the process runs on, so their standing is what it
    /// was on the login it left, as for [`Self::SameLogin`]. Only a command that replaces a live
    /// process rebinds: a relaunch after its agent ended (a cold relaunch included) starts a new
    /// runner on the row's current login, and selects.
    Rebind,
    /// The command selects its login: integrity and the selection policy. A record that names no
    /// purpose reads as this one, the stricter.
    Select,
}

impl Purpose {
    /// The stored record's word ([`ExecutionIntent::to_value`]); `None` for [`Self::Select`], which
    /// a record states by naming none, so records written before purposes read back unchanged.
    const fn word(self) -> Option<&'static str> {
        match self {
            Self::SameLogin => Some("same-login"),
            Self::Rebind => Some("rebind"),
            Self::Select => None,
        }
    }

    /// The purpose whose [`Self::word`] a record names: `None` for any word no purpose writes.
    fn of_word(word: &str) -> Option<Self> {
        [Self::SameLogin, Self::Rebind]
            .into_iter()
            .find(|purpose| purpose.word() == Some(word))
    }
}

/// What lifts an account execution refusal ([`Refusal`]), named by its wait word
/// ([`Self::word`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefusalKind {
    /// The account writer is held: a change is under way, and the next look plans again.
    Lock,
    /// The account controls or the credential generation changed during admission, or since the
    /// intent was captured: the next look plans again.
    Changing,
    /// The command's authentication route is not positively attributed (a legacy record, a
    /// custom home with no directory, a shell whose environment cannot be read or does not carry
    /// the route its agent runs on, as where an rc file or a typed `export` set it).
    Unattributed,
    /// The account records, the login's files or the selection configuration cannot be read.
    Unread,
    /// The login's ownership or identity does not hold: an ambiguous owner, a login that differs
    /// from a row's only in letter case (never shown to be one directory), a token entry or an
    /// identity of another account (a retained login whose own file names another account than
    /// the row that left it included), a custom home over managed storage.
    Integrity,
    /// The login is one no row holds now that a row or binding retains: the login its account's
    /// row left for a newer one, or one an account no longer listed here kept. A superseded
    /// credential is never run again; a relaunch moves onto the row's current login instead
    /// ([`relaunch_rebind`]), so what is left waits on a person: the account listed again, or the
    /// session moved.
    Retained,
    /// The vault token the route names is not in use on this Mac (none, ended, rejected,
    /// mismatched, unreadable here, unverified with windows that differ here, or unverified off
    /// the Mac that minted it at a person's terminal), or this platform has no token route: a new
    /// token, or a move off the route, lifts it.
    Token,
    /// Selection policy: the account is signed out or has no login here.
    SignedOut,
    /// Selection policy: the account is at a usage limit.
    Limited,
    /// Selection policy: the owner's pin, a pending pin or `accounts = "off"` names another
    /// account.
    Pin,
    /// Claims, checked for every purpose: the lease verdict refuses this Mac (another Mac's
    /// claim, an off clock, a credential held for preparation only or failed to prepare here, a
    /// non-home claim this Mac cannot record), the pinned row's included.
    Lease,
    /// Custody, checked for every purpose: the account is assigned to another Mac.
    Held,
}

impl RefusalKind {
    /// Every kind, in declaration order.
    pub const ALL: [Self; 12] = [
        Self::Lock,
        Self::Changing,
        Self::Unattributed,
        Self::Unread,
        Self::Integrity,
        Self::Retained,
        Self::Token,
        Self::SignedOut,
        Self::Limited,
        Self::Pin,
        Self::Lease,
        Self::Held,
    ];

    /// The wait word a plan or an admission refused for this kind records (`wait:<word>`).
    /// [`Self::Lease`] and [`Self::Held`] keep the `account-policy-` words records and the footer
    /// read, though every purpose checks them: the bare `account-held` is the account move's word
    /// (`accounts_pool::MOVE_HELD`).
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Lock => "account-lock",
            Self::Changing => "account-changing",
            Self::Unattributed => "account-unattributed",
            Self::Unread => "account-unread",
            Self::Integrity => "account-integrity",
            Self::Retained => "account-retained",
            Self::Token => "account-token",
            Self::SignedOut => "account-policy-signed-out",
            Self::Limited => "account-policy-limited",
            Self::Pin => "account-policy-pin",
            Self::Lease => "account-policy-lease",
            Self::Held => "account-policy-held",
        }
    }

    /// The kind whose [`Self::word`] is `word`.
    #[must_use]
    pub fn of_word(word: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.word() == word)
    }

    /// Whether this kind is the selection policy's: a [`Purpose::Select`] admission holds its
    /// login's row to each one, a [`Purpose::Rebind`] to its new login's sign-in alone
    /// ([`Self::SignedOut`]), and a [`Purpose::SameLogin`] to none.
    #[must_use]
    pub const fn policy(self) -> bool {
        match self {
            Self::Lock
            | Self::Changing
            | Self::Unattributed
            | Self::Unread
            | Self::Integrity
            | Self::Retained
            | Self::Token
            | Self::Lease
            | Self::Held => false,
            Self::SignedOut | Self::Limited | Self::Pin => true,
        }
    }
}

/// An account execution refusal: what lifts it and the whole reason, which names no credential
/// value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refusal {
    /// What lifts it.
    pub kind: RefusalKind,
    /// The reason in full.
    pub why: String,
}

impl Refusal {
    /// A refusal of `kind` for `why`.
    pub fn new(kind: RefusalKind, why: impl Into<String>) -> Self {
        Self {
            kind,
            why: why.into(),
        }
    }

    /// A step that returned a plain reason, as `kind`.
    pub fn of(kind: RefusalKind) -> impl FnOnce(String) -> Self {
        move |why| Self::new(kind, why)
    }
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.why)
    }
}

impl From<Refusal> for String {
    fn from(refusal: Refusal) -> Self {
        refusal.why
    }
}

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
        /// What its admission checks.
        purpose: Purpose,
    },
    /// A vault-token route (`docs/DESIGN-claude-setup-token-2026-11-05.md` §5.2) to one
    /// account's token entry, in one exact local account namespace. Admission checks the row's
    /// custody and claims, the token's standing here and its record's email, whatever its
    /// purpose, never a stored login: a token row may have none on this Mac.
    Token {
        /// Fingerprint of provider and local account roots, never a peer-supplied path.
        namespace: String,
        /// The selector the retained command carries; absent means the default login.
        login: Option<String>,
        /// The managed row, or `None` for an account no row names (its login record's).
        owner: Option<String>,
        /// The token entry the wrapper serves.
        entry: accounts::TokenEntry,
        /// What its admission checks.
        purpose: Purpose,
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

    /// Whether this intent may stand for a command that replaces the live process `who` (its exec
    /// environment, independently observed): [`Self::matches_process`], with the owner's token
    /// entry read from `roster` where `who` runs on a vault token; or, for a [`Purpose::Rebind`]
    /// intent, `who` on its own account's credential (its stored login, or the vault token of the
    /// intent's owner row) under the same user home, on a login that row left ([`Row::was`] of
    /// `roster`, bound by this Mac's login records, [`accounts::bound`]; a directory shown to be
    /// one it lists, never one spelling but for case alone, [`accounts::left_directory`]): the
    /// command moves that very account onto its row's current login. `None` for `roster` names no
    /// owner row, so it admits only what [`Self::matches_process`] admits with no token entry.
    #[must_use]
    pub fn replaces_process(
        &self,
        who: &accounts::ProcessLogin,
        same_user_home: bool,
        roster: Option<&Roster>,
    ) -> bool {
        let owner_entry = match who.route {
            accounts::AuthRoute::VaultToken { .. } => {
                roster.and_then(|roster| self.owner_entry(roster))
            }
            accounts::AuthRoute::Stored | accounts::AuthRoute::Outside => None,
        };
        if self.matches_process(who, same_user_home, owner_entry) {
            return true;
        }
        let (Self::Stored {
            owner,
            purpose: Purpose::Rebind,
            ..
        }
        | Self::Token {
            owner,
            purpose: Purpose::Rebind,
            ..
        }) = self
        else {
            return false;
        };
        let Some(row) = owner
            .as_deref()
            .and_then(|label| roster.and_then(|roster| roster.row(label)))
        else {
            return false;
        };
        let on_row = match who.route {
            accounts::AuthRoute::Stored => true,
            accounts::AuthRoute::VaultToken { entry } => {
                Some(entry) == accounts::TokenEntry::of_row(row)
            }
            accounts::AuthRoute::Outside => false,
        };
        same_user_home && who.default_home && on_row && left_login(row, who.login.as_deref())
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
                purpose,
            } => with_purpose(
                aterm_json::json!({
                    "v": 1, "route": "stored", "namespace": namespace,
                    "login": login, "owner": owner, "generation": generation,
                }),
                *purpose,
            ),
            Self::Token {
                namespace,
                login,
                owner,
                entry,
                purpose,
            } => with_purpose(
                aterm_json::json!({
                    "v": 1, "route": "token", "namespace": namespace,
                    "login": login, "owner": owner, "entry": entry.name(),
                }),
                *purpose,
            ),
        }
    }

    /// What this intent's admission checks: `None` for an external route, whose admission reads
    /// the shell's environment instead.
    #[must_use]
    pub const fn purpose(&self) -> Option<Purpose> {
        match self {
            Self::External { .. } => None,
            Self::Stored { purpose, .. } | Self::Token { purpose, .. } => Some(*purpose),
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
                // A record that names no purpose (every one written before purposes) is a
                // selection's, the stricter; a named one is only the word `with_purpose` writes.
                let purpose = match value.get("purpose") {
                    None => Purpose::Select,
                    Some(word) => Purpose::of_word(word.as_str()?)?,
                };
                if route == "token" {
                    Self::Token {
                        namespace: digest(value.get("namespace")?)?,
                        login,
                        owner,
                        entry: accounts::TokenEntry::parse(value.get("entry")?.as_str()?)?,
                        purpose,
                    }
                } else {
                    Self::Stored {
                        namespace: digest(value.get("namespace")?)?,
                        login,
                        owner,
                        generation: digest(value.get("generation")?)?,
                        purpose,
                    }
                }
            }
            _ => return None,
        };
        (intent.to_value() == *value).then_some(intent)
    }
}

/// `value`, a stored or token record, with `purpose`'s word where it names one ([`Purpose::word`]).
fn with_purpose(mut value: aterm_json::Value, purpose: Purpose) -> aterm_json::Value {
    if let (Some(word), Some(fields)) = (purpose.word(), value.as_object_mut()) {
        fields.insert("purpose".into(), aterm_json::json!(word));
    }
    value
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
    store
        .check_sync_read_receipt(&receipt)
        .map_err(crate::harness::account_sync_error::SyncError::into_message)?;
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

/// The selection policy a [`Purpose::Select`] admission holds the login's owner row to: what the
/// pool would choose a new run onto, which a [`Purpose::SameLogin`] relaunch never asks. Custody
/// and claims are not here: every purpose holds the row to them ([`owner_may_run_here`]).
struct Selection {
    /// The rows with a login made here ([`accounts::enrolled_in`]).
    enrolled: std::collections::BTreeSet<String>,
    /// The owner's pin stands ([`launch_pin`]).
    pinned: bool,
    /// The configured rotation mode.
    mode: Mode,
}

impl Selection {
    /// Read the pin and hold `label`, the login's owner row (`None`: no row names it), and
    /// `named`, whether the command names a login of its own, to it and to a pending pin.
    fn read(
        seat: &Seat<'_>,
        cli: Cli,
        snapshot: &PoolSnapshot,
        mode: Mode,
        label: Option<&str>,
        named: bool,
    ) -> Result<Self, Refusal> {
        let enrolled = accounts::enrolled_in(cli, &snapshot.roster, seat.home);
        // The pinned row's own standing (its sign-in, what this Mac may run) is this login's only
        // where the pin names the login's row; a pin on any other row names another account,
        // whatever that row's standing.
        let pin = pin_of(cli, &snapshot.roster, &snapshot.state, &enrolled, seat.now).map_err(
            |refusal| {
                if refusal.kind == RefusalKind::Pin || snapshot.state.active.as_deref() == label {
                    refusal
                } else {
                    Refusal::new(
                        RefusalKind::Pin,
                        format!(
                            "execution account is not the pinned account, which refuses: {}",
                            refusal.why
                        ),
                    )
                }
            },
        )?;
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
            || (mode == Mode::Off && pin.is_none() && named)
        {
            return Err(Refusal::new(
                RefusalKind::Pin,
                "execution no longer matches the selected account policy",
            ));
        }
        Ok(Self {
            enrolled,
            pinned: pin.is_some(),
            mode,
        })
    }

    /// Hold `row`, the login's owner, to its limit and, on a stored login (`stored`), its
    /// sign-in. A stored-login route is judged by the stored login alone
    /// ([`accounts::stored_login_condition`]): a row that only a reading of its vault token keeps
    /// in rotation ([`accounts::LoginHere::OnToken`]) has a lapsed stored login here, so an intent
    /// that starts on that login is refused as signed out, never relaunched on it.
    fn admits(
        &self,
        row: &Row,
        state: &accounts::State,
        now: i64,
        stored: bool,
    ) -> Result<(), Refusal> {
        let enrolled = self.enrolled.contains(&row.label);
        let condition = if stored {
            accounts::stored_login_condition(row, state, enrolled, now)
        } else {
            accounts::condition(row, state, enrolled, now)
        };
        if matches!(condition, accounts::Condition::Limited { .. })
            && self.mode != Mode::Off
            && !self.pinned
        {
            return Err(Refusal::new(
                RefusalKind::Limited,
                "execution account is at its limit",
            ));
        }
        // No signed-out check on a token route: a token row may have no stored login on this Mac.
        if stored
            && matches!(
                condition,
                accounts::Condition::NotEnrolled | accounts::Condition::SignedOut
            )
        {
            return Err(Refusal::new(
                RefusalKind::SignedOut,
                "execution account is not signed in",
            ));
        }
        Ok(())
    }
}

/// Whether this Mac may run `row`'s credential at `now`, whatever a relaunch on it replaces: no
/// other Mac holds the row ([`Row::held_by`]) and the claims admit this Mac
/// ([`accounts::State::lease_verdict`], which holds the credential's execution verdict too). The
/// turn-end advice reads the same two facts as execution authority
/// (`accounts::row_available_here`): a session they refuse yields, and no relaunch starts a new
/// turn on it.
fn owner_may_run_here(row: &Row, state: &accounts::State, now: i64) -> Result<(), Refusal> {
    if row.held_by.is_some() {
        return Err(Refusal::new(
            RefusalKind::Held,
            "execution account is held by another machine",
        ));
    }
    state.lease_verdict(&row.label, now).map_err(|why| {
        Refusal::new(
            RefusalKind::Lease,
            format!("execution account: {}", why.as_str()),
        )
    })
}

/// Whether `row`'s current stored login is signed in here ([`accounts::stored_login_wants_sign_in`]
/// says it wants none: a login is made here and is not signed out, whatever its walls say): what a
/// [`Purpose::Rebind`] asks of the login it moves onto, and whether the row can take a relaunch off
/// a login it left ([`relaunch_rebind`]).
fn serves_stored(cli: Cli, seat: &Seat<'_>, snapshot: &PoolSnapshot, row: &Row) -> bool {
    let enrolled = accounts::enrolled_in(cli, &snapshot.roster, seat.home);
    !accounts::stored_login_wants_sign_in(
        row,
        &snapshot.state,
        enrolled.contains(&row.label),
        seat.now,
    )
}

/// Whether `row` left the login `login` (`None` or empty: the default home's) for a newer one: its
/// [`Row::was`] lists a directory shown to be it, never one spelling but for case alone
/// ([`accounts::left_directory`]). A directory that cannot be resolved is not shown to be one.
fn left_login(row: &Row, login: Option<&str>) -> bool {
    match login.filter(|login| !login.is_empty()) {
        None => row.was.contains(&None),
        Some(login) => accounts::left_directory(&row.was, login).unwrap_or(false),
    }
}

/// Whether `intent`'s owner row, as `snapshot` binds it, left the login the intent names for a
/// newer one ([`left_login`]) and does not hold it now ([`accounts::rows_at`], `default` the
/// native default home's canonical login): that login is the account's superseded credential, which
/// no admission grants again and no plan made now names, since a relaunch moves onto the row's
/// current login ([`relaunch_rebind`]). `false` for an intent that names no owner row, or an owner
/// the roster no longer lists.
fn owner_left(intent: &ExecutionIntent, snapshot: &PoolSnapshot, default: &Path) -> bool {
    let (ExecutionIntent::Stored { login, owner, .. }
    | ExecutionIntent::Token { login, owner, .. }) = intent
    else {
        return false;
    };
    let Some(row) = owner
        .as_deref()
        .and_then(|label| snapshot.roster.row(label))
    else {
        return false;
    };
    let Some(directory) = login.as_deref().or_else(|| default.to_str()) else {
        return false;
    };
    left_login(row, login.as_deref())
        && accounts::rows_at(std::slice::from_ref(row), directory, default)
            .is_ok_and(|at| at.same.is_empty())
}

/// Why [`execution_readmission`] stops an intent whose owner row left its login since the plan
/// ([`owner_left`]).
const OWNER_LEFT: &str = "execution login is one its account left for a newer login since its plan";

/// The refusal of `login` (`None`: the default home's), which no row holds now and `retainer`'s
/// row or binding retains ([`accounts::retained_directory_owner_at`]): never run again, and never
/// read as a fault of the records ([`RefusalKind::Retained`]) unless it contradicts them. Where
/// `retainer` is a row of `roster`, that row left the login for a newer one: the login is that
/// row's superseded credential, unless its own file names another account than the row's, an
/// identity contradiction ([`RefusalKind::Integrity`], as [`check_login_identity`] reads it, so a
/// login that names an account no row lists is never filed under the row that left it). Where no
/// row of `roster` is `retainer`, an account no longer listed here kept it.
fn retained(
    cli: Cli,
    home: &Path,
    roster: &Roster,
    retainer: &str,
    login: Option<&str>,
) -> Refusal {
    let Some(row) = roster.row(retainer) else {
        return Refusal::new(
            RefusalKind::Retained,
            "execution login belongs to an account no longer listed here",
        );
    };
    let at = Row {
        dir: login.map(str::to_owned),
        ..row.clone()
    };
    match check_login_identity(cli, &at, home) {
        Err(_) => Refusal::new(
            RefusalKind::Integrity,
            "execution login names another account than the one that left it",
        ),
        Ok(()) => Refusal::new(
            RefusalKind::Retained,
            "execution login is one its account left for a newer login",
        ),
    }
}

pub(super) fn capture_held(
    seat: &Seat<'_>,
    cli: Cli,
    who: &accounts::ProcessLogin,
    snapshot: &PoolSnapshot,
    purpose: Purpose,
) -> Result<ExecutionIntent, Refusal> {
    use RefusalKind::{Changing, Integrity, Unread};
    let token = match who.route {
        accounts::AuthRoute::Stored => None,
        accounts::AuthRoute::VaultToken { entry } => Some(entry),
        accounts::AuthRoute::Outside => {
            return Err(Refusal::new(
                Integrity,
                "execution route is not the account's own credential",
            ));
        }
    };
    let login = who
        .login
        .as_deref()
        .filter(|login| !login.is_empty())
        .map(|login| {
            accounts::canonical_directory(Path::new(login))
                .map_err(Refusal::of(Unread))?
                .into_os_string()
                .into_string()
                .map_err(|_| Refusal::new(Integrity, "execution login is not UTF-8"))
        })
        .transpose()?;
    let default = accounts::canonical_directory(&seat.home.join(cli.accounts().default_home))
        .map_err(Refusal::of(Unread))?;
    let directory = login.as_deref().map(Path::new).unwrap_or(&default);
    let (config, issues) = crate::supervise::config::SupervisorConfig::from_path(Some(
        &seat.config_dir.join("aterm.toml"),
    ));
    if !issues.is_empty() {
        return Err(Refusal::new(
            Unread,
            "execution selection configuration is unavailable or invalid",
        ));
    }
    let mode = config.accounts_for(cli.program());
    let directory = directory
        .to_str()
        .ok_or_else(|| Refusal::new(Integrity, "execution login is not UTF-8"))?;
    // The rows whose login this is, shown to be one directory ([`accounts::rows_at`]): a current
    // row's directory reached by another path is that row's. A row's directory that only one
    // spelling but for ASCII case relates to it may be another directory (a leaf not made yet, a
    // case-sensitive volume): this login is then neither that row's, whose identity and binding
    // say nothing of it, nor no account's, so it is refused, as the retained check below would.
    let at = accounts::rows_at(&snapshot.roster.rows, directory, &default)
        .map_err(Refusal::of(Unread))?;
    if !at.case_only.is_empty() {
        return Err(Refusal::new(
            Integrity,
            "execution login differs from an account's login only in letter case",
        ));
    }
    let label = match at.same.as_slice() {
        [] => None,
        [row] => Some(row.label.as_str()),
        [_, _, ..] => {
            return Err(Refusal::new(
                Integrity,
                "execution login has ambiguous account ownership",
            ));
        }
    };
    let selection = match purpose {
        Purpose::Select => Some(Selection::read(
            seat,
            cli,
            snapshot,
            mode,
            label,
            login.is_some(),
        )?),
        Purpose::SameLogin | Purpose::Rebind => None,
    };
    let (owner, generation) = match label {
        // A rebind moves onto its row's current login: one no row holds is no longer the login
        // its plan named. Before the signal the next look plans again; once its agent ended no
        // plan names the intent again, and its readmission stops where the owner row left the
        // login ([`owner_left`]).
        None if purpose == Purpose::Rebind => {
            return Err(Refusal::new(
                Changing,
                "execution account's current login moved before admission",
            ));
        }
        None => {
            if let Some(retainer) = accounts::retained_directory_owner_at(
                &snapshot.logins,
                &snapshot.roster.rows,
                "",
                directory,
                Some(&default),
            )
            .map_err(Refusal::of(Unread))?
            {
                return Err(retained(
                    cli,
                    seat.home,
                    &snapshot.roster,
                    retainer,
                    login.as_deref(),
                ));
            }
            match token {
                None => (
                    None,
                    Some(
                        unowned_generation(seat, cli, snapshot, login.as_deref())
                            .map_err(Refusal::of(Unread))?,
                    ),
                ),
                // An account no row names: the marker must name the entry of the account the
                // session's own login record names (§5.2).
                Some(entry) => {
                    let named = super::super::profile::login_record(seat.home, login.as_deref())
                        .and_then(|record| accounts::TokenEntry::of_email(&record.email));
                    if named != Some(entry) {
                        return Err(Refusal::new(
                            Integrity,
                            "execution token names another account's entry",
                        ));
                    }
                    (None, None)
                }
            }
        }
        Some(label) => {
            let row = snapshot
                .roster
                .row(label)
                .ok_or_else(|| Refusal::new(Changing, "execution account disappeared"))?;
            owner_may_run_here(row, &snapshot.state, seat.now)?;
            if let Some(selection) = &selection {
                selection.admits(row, &snapshot.state, seat.now, token.is_none())?;
            }
            // A rebind's credential changes: the row's current stored login must be signed in
            // here, as [`Selection::admits`] asks of a stored login, and nothing else of the
            // selection policy. A token route keeps the token it runs on, whatever the login.
            if purpose == Purpose::Rebind
                && token.is_none()
                && !serves_stored(cli, seat, snapshot, row)
            {
                return Err(Refusal::new(
                    RefusalKind::SignedOut,
                    "execution account's new login is not signed in here",
                ));
            }
            match token {
                None => {
                    check_login_identity(cli, row, seat.home)
                        .map_err(|why| Refusal::new(Integrity, why.to_string()))?;
                    (
                        Some(label.to_owned()),
                        Some(binding(seat, cli, snapshot, row).map_err(Refusal::of(Unread))?),
                    )
                }
                Some(entry) => {
                    if accounts::TokenEntry::of_row(row) != Some(entry) {
                        return Err(Refusal::new(
                            Integrity,
                            "execution token names another account's entry",
                        ));
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
        return Err(Refusal::new(
            Changing,
            "execution account controls changed during admission",
        ));
    }
    let namespace = namespace(seat, cli).map_err(Refusal::of(Unread))?;
    Ok(match (token, generation) {
        (Some(entry), _) => ExecutionIntent::Token {
            namespace,
            login,
            owner,
            entry,
            purpose,
        },
        (None, Some(generation)) => ExecutionIntent::Stored {
            namespace,
            login,
            owner,
            generation,
            purpose,
        },
        (None, None) => {
            return Err(Refusal::new(
                Integrity,
                "execution stored login has no generation",
            ));
        }
    })
}

/// The account writer under `store`, or the refusal that names why it is not held.
fn writer(store: &Store) -> Result<accounts::Lock, Refusal> {
    store
        .acquire_bounded()
        .map_err(|why| Refusal::new(RefusalKind::Unread, why.to_string()))?
        .ok_or_else(|| Refusal::new(RefusalKind::Lock, accounts::LOCK_HELD))
}

/// Capture an already selected login under the writer, without choosing a replacement, for
/// `purpose`, which the intent carries so that [`admit_execution`] checks exactly what this
/// capture checked. The caller supplies positively observed authentication attribution.
///
/// # Errors
/// The store, selected account, custody, credential identity or binding cannot be admitted, or
/// (for [`Purpose::Select`]) the selection policy refuses the account.
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
    purpose: Purpose,
) -> Result<ExecutionIntent, Refusal> {
    let stored = match who.route {
        accounts::AuthRoute::Stored => true,
        accounts::AuthRoute::VaultToken { .. } => false,
        accounts::AuthRoute::Outside => return Ok(ExecutionIntent::external()),
    };
    if !who.default_home {
        return Err(Refusal::new(
            RefusalKind::Unattributed,
            "custom stored-login home requires directory attribution",
        ));
    }
    // The sign-in lock is a stored login's service; a token route never takes it (§5.8).
    if stored && seat.keyring_held(cli) {
        return Err(Refusal::new(
            RefusalKind::Integrity,
            "execution stored-login route is unavailable",
        ));
    }
    let store = seat.store(cli);
    let held = writer(&store)?;
    let snapshot =
        snapshot_held(seat, cli, &store, &held).map_err(Refusal::of(RefusalKind::Unread))?;
    capture_held(seat, cli, who, &snapshot, purpose)
}

/// What an in-place relaunch starts on in place of the login its process runs on, where that is a
/// login its account's row left for a newer one ([`relaunch_rebind`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Rebind {
    /// The row's current login, which is signed in here (or the process runs on the row's vault
    /// token, which keeps serving it): the line's intent is a [`Purpose::Rebind`] where it
    /// replaces the live process on the login the row left.
    Row(LoginEnv),
    /// The pool's pick on the records, where the row's current login is not signed in here: the
    /// owner's pin first, then the row's current login on the row's own vault token, where only
    /// that token runs the row here and the line serves it, then [`accounts::launch_row`]'s pick
    /// of another row, each under the launch-time token guard as the line starts it. A move onto
    /// the row's vault token or onto another account, whose intent selects ([`Purpose::Select`]):
    /// either starts a new runner on a credential the live process does not run on.
    Pick(LoginEnv),
}

impl Rebind {
    /// The login the line carries.
    #[must_use]
    pub const fn login(&self) -> &LoginEnv {
        match self {
            Self::Row(login) | Self::Pick(login) => login,
        }
    }
}

/// What an in-place relaunch of a Claude Code process on `who` (an upgrade, a native upgrade, a
/// restart in place) starts on, where `who` runs on its own credential in the default home, on a
/// login no row holds now and a row is attributed (the row that left it, or the row its login
/// file names: [`rebound_login`], the one rule a cold relaunch follows too): that row's current
/// login where it is signed in here ([`Rebind::Row`]), else the pool's pick on the records, taking
/// no usage read and writing nothing ([`Rebind::Pick`]). The pick judges each row as the line
/// starts it: a row only its vault token keeps running here is picked only where `serves`, the
/// line's own token judgment of a start on that row, says the token serves it (never for a line
/// that cannot carry the wrapper), since the line otherwise starts on the row's lapsed stored
/// login, which its admission refuses on every pass while the records stand
/// ([`accounts::token_guarded`], the cold relaunch's guard). Past the owner's pin, the pick is the
/// row itself where only its vault token runs it here and the relaunch keeps it by the cold
/// relaunch's rule (no other Mac holds it, the claims admit this Mac, no limit holds it, and
/// `serves` says the token serves the line): its current login on that token, a move onto the
/// token that selects as every such move does. It never stops at the row itself while another
/// row serves. A retained login is never run again
/// ([`RefusalKind::Retained`]), so the relaunch moves off it, as a turn end's login follow and a
/// cold relaunch do. `Ok(None)` keeps the login: a row holds it now, no row is attributed (a login
/// its admission then judges: an unowned login, a retained one, or one whose file names an
/// account no row lists), the route is not the account's own, or the home is custom.
///
/// # Errors
/// [`RefusalKind::SignedOut`] where the row's current login is not signed in here, its vault token
/// does not carry the line, and no other row serves (rotation off, the pin's row unavailable, or
/// no row usable); [`RefusalKind::Integrity`]
/// where several rows left the login and nothing the process runs on names one of them, so the
/// records cannot say whose account it runs on; [`RefusalKind::Lock`] or [`RefusalKind::Unread`]
/// where the account records cannot be read under the writer.
pub fn relaunch_rebind(
    seat: &Seat<'_>,
    who: &accounts::ProcessLogin,
    serves: &mut dyn FnMut(&Row) -> bool,
) -> Result<Option<Rebind>, Refusal> {
    let cli = Cli::Claude;
    let own = match who.route {
        accounts::AuthRoute::Stored | accounts::AuthRoute::VaultToken { .. } => true,
        accounts::AuthRoute::Outside => false,
    };
    if !own || !who.default_home || !cli.accounts().measured || seat.keyring_held(cli) {
        return Ok(None);
    }
    let store = seat.store(cli);
    let held = writer(&store)?;
    let snapshot =
        snapshot_held(seat, cli, &store, &held).map_err(Refusal::of(RefusalKind::Unread))?;
    drop(held);
    let Some(row) = rebound_login(cli, &snapshot.roster, who, seat.home)
        .map_err(|why| Refusal::new(RefusalKind::Integrity, why))?
    else {
        return Ok(None);
    };
    let (var, value) = accounts::login_env_for(cli, row);
    let current = LoginEnv {
        label: row.label.clone(),
        var,
        value,
    };
    let token = matches!(who.route, accounts::AuthRoute::VaultToken { .. });
    if token || serves_stored(cli, seat, &snapshot, row) {
        return Ok(Some(Rebind::Row(current)));
    }
    let enrolled = accounts::enrolled_in(cli, &snapshot.roster, seat.home);
    (seat.mode_for(cli) != Mode::Off)
        .then(|| records_pick(seat, cli, &snapshot, &enrolled, row, serves))
        .flatten()
        .map(|pick| Some(Rebind::Pick(pick)))
        .ok_or_else(|| {
            Refusal::new(
                RefusalKind::SignedOut,
                format!(
                    "execution account {}'s new login is not signed in here, and no other account \
                     serves",
                    row.label
                ),
            )
        })
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
    external_admission(seat, cli, directories).map_err(String::from)
}

/// [`admit_external_execution`], its refusal typed by what lifts it.
/// # Errors
/// As [`admit_external_execution`].
pub fn external_admission(
    seat: &Seat<'_>,
    cli: Cli,
    directories: &[PathBuf],
) -> Result<ExecutionGuard, Refusal> {
    use RefusalKind::{Integrity, Unattributed, Unread};
    if directories.is_empty() {
        return Err(Refusal::new(
            Unattributed,
            "custom credential directory is unknown",
        ));
    }
    let store = seat.store(cli);
    let held = writer(&store)?;
    let snapshot =
        execution_snapshot_readonly(seat, cli, &store, &held).map_err(Refusal::of(Unread))?;
    let default = accounts::canonical_directory(&seat.home.join(cli.accounts().default_home))
        .map_err(Refusal::of(Unread))?;
    let managed =
        accounts::canonical_directory(&store.logins_dir()).map_err(Refusal::of(Unread))?;
    for directory in directories {
        let path = accounts::canonical_directory(directory).map_err(Refusal::of(Unread))?;
        if path == default
            || path.starts_with(&managed)
            || accounts::retained_directory_owner_at(
                &snapshot.logins,
                &snapshot.roster.rows,
                "",
                path.to_str().ok_or_else(|| {
                    Refusal::new(Integrity, "custom credential directory is not UTF-8")
                })?,
                Some(&default),
            )
            .map_err(Refusal::of(Unread))?
            .is_some()
        {
            return Err(Refusal::new(
                Integrity,
                "custom credential directory overlaps managed accounts",
            ));
        }
    }
    Ok(ExecutionGuard { _held: Some(held) })
}

/// Authorize exactly the retained intent and keep the local account writer held.
/// A successful result grants no replacement row and does not outlive its guard.
///
/// # Errors
/// As [`execution_admission`], in words.
pub fn admit_execution(
    seat: &Seat<'_>,
    cli: Cli,
    intent: &ExecutionIntent,
) -> Result<ExecutionGuard, String> {
    execution_admission(seat, cli, intent).map_err(String::from)
}

/// [`admit_execution`], its refusal typed by what lifts it. The intent's [`Purpose`] names what is
/// checked: exactly what its capture checked, so a [`Purpose::SameLogin`] intent's integrity
/// holds at exec as it held at the plan, and a [`Purpose::Select`] intent's selection policy too.
///
/// # Errors
/// The namespace, account owner or credential generation changed, the owner row's custody or
/// claims refuse this Mac, or (for [`Purpose::Select`]) current account policy does not admit
/// execution. A token intent is also refused, whatever its purpose, where its token is not in use
/// here or the vault or the token table cannot be read ([`token_in_use_here`]). Unknown and
/// malformed intents must never call this API.
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
pub fn execution_admission(
    seat: &Seat<'_>,
    cli: Cli,
    intent: &ExecutionIntent,
) -> Result<ExecutionGuard, Refusal> {
    admission(seat, cli, intent, false)
        .map(|(guard, _)| guard)
        .map_err(Unadmitted::into_refusal)
}

/// Why [`execution_readmission`] admitted nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Unadmitted {
    /// A refusal that may lift ([`execution_admission`]'s): the next look admits again.
    Refused(Refusal),
    /// The login is no longer filed as the retained intent names it: captured afresh for its
    /// purpose, it names another owner row or another namespace, or the intent's owner row left
    /// it for a newer login since ([`owner_left`]), so no admission of that intent can match
    /// again. Its kind is [`RefusalKind::Changing`].
    Moved(Refusal),
}

impl Unadmitted {
    /// The refusal, whichever it is.
    #[must_use]
    pub fn into_refusal(self) -> Refusal {
        match self {
            Self::Refused(refusal) | Self::Moved(refusal) => refusal,
        }
    }
}

/// [`execution_admission`] for a relaunch whose agent has already ended, where the retained
/// intent's plan cannot be made again: where only the stored login's credential generation moved
/// since its capture (a renewal or a sign-in of its row, a first usage reading creating the row's
/// binding, or, for a row with no binding of its own, any account's change), the intent captured
/// afresh for the same purpose, under the same writer, is admitted in its place and returned, for
/// the caller to keep before it executes. The fresh capture holds every check the purpose names,
/// and it names the same namespace, selector, owner row and route, so it grants the same login
/// that a plan made now would. Every other refusal is [`execution_admission`]'s, and a fresh
/// capture naming another owner row or namespace is [`Unadmitted::Moved`], as is a login the
/// intent's owner row left for a newer one since (a renewal or a sign-in of that row after the
/// signal): no plan names the intent again to move its line onto the row's current login.
///
/// # Errors
/// [`Unadmitted::Refused`] as [`execution_admission`] refuses; [`Unadmitted::Moved`] where the
/// login is filed under another owner row or namespace than the intent names, or its owner row
/// left it.
pub fn execution_readmission(
    seat: &Seat<'_>,
    cli: Cli,
    intent: &ExecutionIntent,
) -> Result<(ExecutionGuard, Option<ExecutionIntent>), Unadmitted> {
    admission(seat, cli, intent, true)
}

impl ExecutionIntent {
    /// Whether this intent, captured afresh for `retained`'s purpose, names the same login on the
    /// same route for the same owner row in the same namespace as `retained`: equal but for a
    /// stored login's credential generation. It is what [`execution_readmission`] admits in
    /// place of `retained`, and all a reader of that admission (a native restart's completion,
    /// which names the intent its Execute admitted) may take in its place: never another owner,
    /// namespace, selector, route or purpose.
    #[must_use]
    pub fn same_login(&self, retained: &Self) -> bool {
        match (self, retained) {
            (
                Self::Stored {
                    namespace,
                    login,
                    owner,
                    purpose,
                    generation: _,
                },
                Self::Stored {
                    namespace: was_namespace,
                    login: was_login,
                    owner: was_owner,
                    purpose: was_purpose,
                    generation: _,
                },
            ) => {
                namespace == was_namespace
                    && login == was_login
                    && owner == was_owner
                    && purpose == was_purpose
            }
            _ => self == retained,
        }
    }
}

/// [`execution_admission`], and with `renew`, [`execution_readmission`]: the guard, and the
/// intent captured afresh where it was admitted in place of `intent`.
fn admission(
    seat: &Seat<'_>,
    cli: Cli,
    intent: &ExecutionIntent,
    renew: bool,
) -> Result<(ExecutionGuard, Option<ExecutionIntent>), Unadmitted> {
    use RefusalKind::{Changing, Integrity, Unread};
    let (login, route, purpose) = match intent {
        ExecutionIntent::External { .. } => return Ok((ExecutionGuard { _held: None }, None)),
        ExecutionIntent::Stored { login, purpose, .. } => {
            (login, accounts::AuthRoute::Stored, *purpose)
        }
        ExecutionIntent::Token {
            login,
            entry,
            purpose,
            ..
        } => (
            login,
            accounts::AuthRoute::VaultToken { entry: *entry },
            *purpose,
        ),
    };
    if route == accounts::AuthRoute::Stored && seat.keyring_held(cli) {
        return Err(Unadmitted::Refused(Refusal::new(
            Integrity,
            "execution stored-login route is unavailable",
        )));
    }
    let store = seat.store(cli);
    let held = writer(&store).map_err(Unadmitted::Refused)?;
    let snapshot = execution_snapshot_readonly(seat, cli, &store, &held)
        .map_err(|why| Unadmitted::Refused(Refusal::new(Unread, why)))?;
    let current = match capture_held(
        seat,
        cli,
        &accounts::ProcessLogin {
            login: login.clone(),
            default_home: true,
            route,
        },
        &snapshot,
        purpose,
    ) {
        Ok(current) => current,
        // Once its agent ended nothing plans the intent again, so a login its owner row left
        // since the plan (a renewal or a sign-in after the signal) can never be admitted again:
        // the readmission stops rather than waits, whatever the capture's own refusal says.
        Err(_)
            if renew
                && accounts::canonical_directory(&seat.home.join(cli.accounts().default_home))
                    .is_ok_and(|default| owner_left(intent, &snapshot, &default)) =>
        {
            return Err(Unadmitted::Moved(Refusal::new(Changing, OWNER_LEFT)));
        }
        Err(refusal) => return Err(Unadmitted::Refused(refusal)),
    };
    // The whole intent, its purpose included: a capture for one purpose never stands for the
    // other's. A readmission takes a fresh capture of the same login in its place.
    let renewed = if current == *intent {
        None
    } else if !renew {
        return Err(Unadmitted::Refused(Refusal::new(
            Changing,
            "execution account namespace or credential generation changed",
        )));
    } else if current.same_login(intent) {
        Some(current)
    } else {
        return Err(Unadmitted::Moved(Refusal::new(
            Changing,
            "execution login is filed under another account or namespace than its plan named",
        )));
    };
    if let ExecutionIntent::Token { owner, entry, .. } = intent {
        let email = match owner
            .as_deref()
            .and_then(|label| snapshot.roster.row(label))
        {
            Some(row) => row.expected_identity_email().map(str::to_owned),
            None => super::super::profile::login_record(seat.home, login.as_deref())
                .map(|record| record.email),
        }
        .ok_or_else(|| {
            Unadmitted::Refused(Refusal::new(
                Integrity,
                "execution token's account names no email",
            ))
        })?;
        token_in_use_here(seat, &store, *entry, &email).map_err(Unadmitted::Refused)?;
    }
    Ok((ExecutionGuard { _held: Some(held) }, renewed))
}

/// Whether the token `entry` is in use on this Mac (its standing in this Mac's vault, under
/// `seat.home`, and the table's own facts of that token, `Table::row_for`: `active`, `soon`,
/// `exposed`, or `unverified` on every Mac where aterm's window or headless route minted it and
/// on its minting Mac where a person's terminal did, but never `unverified (windows differ)`
/// here, on which this Mac starts no session) and its record names `email` (§5.2). The hold of
/// a token awaiting its basis is the route's judgment (`token_route::standing_judged`), which the
/// plan and the wrapper make; this admits the intent they decided. The words name the state,
/// never a value.
///
/// # Errors
/// [`RefusalKind::Unread`] where the vault or the token table cannot be read (just now, or at
/// all; a check file that does not read, for a token minted here alone); [`RefusalKind::Token`]
/// where the token is not in use here; [`RefusalKind::Integrity`]
/// where the entry is not a token's or its record names another account.
#[cfg(unix)]
fn token_in_use_here(
    seat: &Seat<'_>,
    store: &Store,
    entry: accounts::TokenEntry,
    email: &str,
) -> Result<(), Refusal> {
    use super::super::accounts_tokens::TokenState;
    use aterm_secrets::claude_token::{self, Lookup};
    let key = claude_token::Key::parse(entry.key().as_str()).ok_or_else(|| {
        Refusal::new(
            RefusalKind::Integrity,
            "execution token entry is not a token's",
        )
    })?;
    let found = claude_token::standing(&aterm_secrets::client::Paths::for_home(seat.home), &key)
        .map_err(|why| Refusal::new(RefusalKind::Unread, format!("execution token: {why}")))?;
    // Judged as the route judges it: a check file that does not read holds only a token minted
    // here, whose verdict it holds.
    let table = super::super::accounts_tokens::read_table_for(store, &found).map_err(|why| {
        Refusal::new(RefusalKind::Unread, format!("execution token table: {why}"))
    })?;
    let now = u64::try_from(seat.now).unwrap_or(0);
    let state = TokenState::of(&found, table.row_for(key.as_str(), &found), now);
    if !state.in_use() {
        // A vault not read just now is a fact aterm cannot read yet, not a token out of use.
        let kind = match state {
            TokenState::Unknown => RefusalKind::Unread,
            _ => RefusalKind::Token,
        };
        return Err(Refusal::new(
            kind,
            format!("execution token is not in use here: {}", state.words()),
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
        return Err(Refusal::new(
            RefusalKind::Integrity,
            "execution token's record names another account",
        ));
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
) -> Result<(), Refusal> {
    Err(Refusal::new(
        RefusalKind::Token,
        "execution token route is unavailable on this platform",
    ))
}
