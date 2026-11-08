// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! AI CLI account roster admission, usage/wall tracking, rotation advice, and state store: Claude
//! Code's pool, and one per pooled CLI ([`Cli`]) through the same parser, store and advice.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};

use aterm_json::{Map, Value};
use aterm_phase::ResetKind;

pub use crate::ai::connector::Cli;
use crate::ai::connector::{Flow, Region, RowKeys, Seam};

use super::one_line;

#[path = "accounts_auth_route.rs"]
mod auth_route;
pub use auth_route::{AuthRoute, TokenEntry};

#[path = "accounts_signin_queue.rs"]
mod signin_queue;
pub use signin_queue::{
    ManualSigninCapability, SigninAttempt, SigninBlocker, SigninPosition, SigninTicket, SigninTurn,
    manual_signin_capability,
};

#[path = "accounts_auth_scope.rs"]
mod auth_scope;
pub(super) use auth_scope::note_auth_for_observed_row;
pub(super) use auth_scope::note_served_login_for_observed_row;
pub use auth_scope::{
    LifetimeScope, ManagedContexts, ManagedScope, note_auth_in_context, note_auth_on_default,
};

#[path = "accounts_workflow_scope.rs"]
mod workflow_scope;
pub use workflow_scope::{
    WorkflowContexts, WorkflowCustody, WorkflowToken, current_sign_in_attempt,
    current_sign_in_blocked, current_sign_in_task, current_workflow, workflow_event_matches,
    workflow_token_current,
};

/// Capture an explicit sign-in policy admission without changing untouched workflow records.
pub(in crate::harness) fn admit_workflow(logins: &mut Logins, label: &str) -> Result<(), String> {
    workflow_scope::admit(logins, label)
}

/// Read a captured descriptor; publication still checks it against the current raw policy.
pub(in crate::harness) fn captured_workflow(logins: &Logins, label: &str) -> Option<WorkflowToken> {
    workflow_scope::captured_workflow(logins, label)
}

/// Environment choices in the workflow conformance replay, linked by the source audit.
#[cfg(any(test, debug_assertions))]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountWorkflowScope",
        action = "Seed",
        reason = "The fixture seeds admitted task and attempt observations through real account transactions."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountWorkflowScope",
        action = "Legacy",
        reason = "The fixture selects persisted raw history without producer custody; this supplies no current authority."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountWorkflowScope",
        action = "AnotherProvider",
        reason = "Fixture selection repeats the replay for another supported provider."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountWorkflowScope",
        action = "AnotherEdit",
        reason = "Fixture selection chooses the policy or nonpolicy edit before replaying its transaction."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountWorkflowScope",
        action = "Activity",
        reason = "The fixture combines served-login, notice and authentication observations; no single workflow operation represents this input."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountWorkflowScope",
        action = "Back",
        reason = "The environment restores configured policy or a bound directory through existing account transactions."
    )
)]
#[doc(hidden)]
pub fn account_workflow_scope_environment() {}

#[cfg(test)]
pub(super) fn begin_workflow_projection_for_test(raw: &Roster, logins: &mut Logins) {
    workflow_scope::begin(Some(raw), logins);
}

#[cfg(test)]
pub(super) fn finish_workflow_projection_for_test(
    before: &Logins,
    raw: &Roster,
    logins: &mut Logins,
    events: &mut [Event],
) -> Result<bool, String> {
    workflow_scope::finish(before, Some(raw), logins, events)
}

#[path = "accounts_shared.rs"]
pub mod shared;

#[path = "accounts_native_publication.rs"]
mod native_publication;
pub(in crate::harness) use native_publication::{ActualPublicationPermit, ActualPublicationStamp};

#[path = "accounts_sync_commit.rs"]
mod sync_commit;
#[cfg(test)]
pub(super) use sync_commit::CUT as SYNC_COMMIT_CUT;
pub(super) use sync_commit::SyncTarget;
use sync_commit::usage_target;

#[path = "accounts_directory.rs"]
mod directory;
pub(super) use directory::canonical_directory;
pub(super) use directory::retained_directory_owner_at;
pub(super) use directory::{
    lists_directory, naming_row, retained_directory_owner, shared_staging_retained,
};

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountDirectoryAdmission",
        action = "AnotherCase",
        reason = "Model bookkeeping selects a filesystem and retained-owner fixture"
    )
)]
#[doc(hidden)]
pub fn directory_admission_environment() {}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountLifetimeScope",
        action = "AnotherTransform",
        reason = "Model bookkeeping selects the edit the fixture replays: a credential replacement or a non-credential edit"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountLifetimeScope",
        action = "WithAuth",
        reason = "Model bookkeeping selects a fixture whose login carries a current authentication word"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountLifetimeScope",
        action = "Seed",
        reason = "Model bookkeeping starts the walk: the fixture every replayed transition rebuilds writes the initial roster and lifetime observations through the store's own notes"
    )
)]
#[doc(hidden)]
pub fn lifetime_scope_environment() {}

/// The roster's file name, beside `aterm.toml`.
pub const ACCOUNTS_FILE: &str = "accounts.toml";

/// The variable a relaunch line carries to choose the login.
pub const LOGIN_ENV: &str = "CLAUDE_SECURESTORAGE_CONFIG_DIR";

/// The most peer names retained in a clock observation or machine alias list.
pub const MAX_PEER_NAMES: usize = 32;

/// The longest label.
pub const LABEL_MAX: usize = 64;

/// The one word of the label grammar that is no label: the harness command's help word.
pub const RESERVED_LABEL: &str = "help";

/// The longest `dir` a row may name, in bytes.
pub const DIR_MAX: usize = 512;

/// The longest `email` a row may carry, in bytes (RFC 5321's path bound).
pub const EMAIL_MAX: usize = 254;

/// The longest `browser` profile name a row may carry, in characters.
pub const BROWSER_MAX: usize = 64;

/// The most bytes of `accounts.toml` that are read.
pub const MAX_ROSTER_BYTES: u64 = 256 * 1024;

/// The most bytes of `state.json` that are read.
pub const MAX_STATE_BYTES: u64 = 1024 * 1024;

/// The state file's schema.
pub const SCHEMA: u64 = 1;
/// A state carrying preparation-only execution holds cannot be written by an
/// older reader which would discard those holds as unknown fields.
pub const PREPARED_STATE_SCHEMA: u64 = 2;

/// The most walls kept per account (the vendor names six limit types).
pub const MAX_WALLS: usize = 8;

/// The longest session id, machine id, window name and `held` word kept.
pub const WORD_MAX: usize = 64;

/// The longest reason a ledger row is given ([`Event::because`]), in bytes.
pub const WHY_MAX: usize = 200;

/// The longest record of the routes an automatic sign-in tried ([`SignInAttempt::routes`]), in
/// bytes: room for every route's words at their longest (the mailbox's masked login among
/// them), so none is cut off.
pub const ROUTES_MAX: usize = 432;

/// The longest reason a ledger row carries, in bytes: a [`Kind::SignIn`] row's `auto <outcome>:`
/// and its [`ROUTES_MAX`] of routes ([`note_sign_in_attempt`]), every other row's [`WHY_MAX`].
pub const LEDGER_WHY_MAX: usize = ROUTES_MAX + 32;

/// The longest ledger line that is parsed.
pub const MAX_EVENT_LINE_BYTES: usize = 4096;

/// The ledger is cut back to [`LEDGER_KEEP_ROWS`] once it passes this size.
pub const LEDGER_MAX_BYTES: u64 = 1024 * 1024;

/// The rows a cut ledger keeps.
pub const LEDGER_KEEP_ROWS: usize = 1024;

/// A percent above this is no figure (the vendor paints whole percents).
pub const PCT_MAX: u32 = 1000;

/// A wall that names its reset stands this long past it.
pub const RESET_GRACE_S: i64 = 150;

/// A wall observed after its own reset stands this long from the observation.
pub const LATE_WALL_S: [i64; 2] = [10 * 60, 30 * 60];

/// A wall that names no reset stands this long from the observation.
pub const NO_RESET_WALL_S: [i64; 3] = [30 * 60, 2 * 3600, 6 * 3600];

/// A wall seen again more than this long after it stopped standing is a new wall.
pub const WALL_EPISODE_S: i64 = 6 * 3600;

/// The session window's share at which an account is `low`.
pub const LOW_SESSION_PCT: u32 = 90;

/// The week's share at which an account is `low`.
pub const LOW_WEEK_PCT: u32 = 98;

/// A session is not moved early ([`Sample::leave_early`]) off an account whose low window resets
/// within this long.
pub const RESET_SOON_S: i64 = 15 * 60;

/// A `low` account is moved into only when no account becomes usable within this long.
pub const LOW_ONLY_AFTER_S: i64 = 15 * 60;

/// An account holding a reset offer that expires within this long is not left early.
pub const OFFER_EXPIRING_S: i64 = 24 * 3600;

/// A reset claim with no outcome is in flight this long ([`reset_in_flight`]) and no longer.
pub const RESET_CLAIM_S: i64 = 10 * 60;

/// How long before a process's start no other login may have run for its reset to be allowed
/// ([`reset_allowed`], [`Account::ran_at`]).
pub const RESET_PROFILE_S: i64 = 25 * 3600;

/// The probe is due no sooner than this after the last sample or claim.
pub const PROBE_MIN_S: i64 = 3 * 60;

/// No account is probed within this long of any account's claim.
pub const PROBE_APART_S: i64 = 90;

/// The probe is due no later than this after the last sample.
pub const PROBE_MAX_S: i64 = 20 * 60;

/// The probe's interval when no burn is known.
pub const PROBE_DEFAULT_S: i64 = 10 * 60;

/// The window the machine's rotation bound counts over.
pub const ROTATION_WINDOW_S: i64 = 3600;

/// How long the one session on an account that has not served a response yet holds every other
/// session back ([`StayWhy::Canary`]).
pub const CANARY_HOLD_S: i64 = 10 * 60;

/// The most rotations whose undo is kept ([`State::undo`]).
pub const MAX_UNDO: usize = 8;

/// How far ahead of the reader's clock an observation's `at` may stand.
pub const STAMP_SKEW_S: i64 = 5 * 60;

/// The session window's length (the vendor's `five_hour`, `windowSeconds: 18000`).
pub const SESSION_WINDOW_S: i64 = 5 * 3600;

/// The week's length.
pub const WEEK_WINDOW_S: i64 = 7 * 24 * 3600;

/// How far past its own row a wall's reset may stand and still be believed ([`note_wall`],
/// [`WEEK_WINDOW_S`]).
pub const RESET_BELIEVED_S: i64 = 8 * 24 * 3600;

/// How far past its own row a [`MONTH_LIMIT`] wall's reset may stand and still be believed
/// ([`note_wall`]): the longest month and a day.
pub const MONTH_RESET_BELIEVED_S: i64 = 32 * 24 * 3600;

/// A session's row older than this is dropped when the state is saved ([`drop_stale_sessions`]).
pub const SESSION_KEEP_S: i64 = 30 * 24 * 3600;

/// How often a session's loop says again what its screen says of usage while that shows unchanged.
pub const CREDITS_RETELL_S: i64 = 10 * 60;

/// How old an answer's row must be for the screen read now to have drawn its notices
/// ([`UsageSaid::at`]).
pub const NOTICES_DRAWN_S: i64 = 2;

/// Credits are taken as in use this long past the newest answer seen served on them
/// ([`CreditsUse::stands`], [`CREDITS_RETELL_S`]).
pub const CREDITS_STALE_S: i64 = 3 * CREDITS_RETELL_S;

/// The weekly window counts as nearly full from this figure ([`windows_full`]).
pub const WINDOW_FULL_PCT: u32 = 90;

/// The weekly window counts as nearly full only while its reset is more than this far off.
pub const WINDOW_FULL_AHEAD_S: i64 = 24 * 3600;

/// The pauses of a bounded wait for `accounts.lock` ([`Store::acquire_within`]).
pub const LOCK_LADDER: [std::time::Duration; 9] = {
    const fn ms(n: u64) -> std::time::Duration {
        std::time::Duration::from_millis(n)
    }
    [
        ms(10),
        ms(20),
        ms(40),
        ms(80),
        ms(150),
        ms(300),
        ms(400),
        ms(500),
        ms(500),
    ]
};

/// What a bounded take answers when the lock stayed held ([`Store::transact_within`],
/// [`Recorded::lock_held`]).
pub const LOCK_HELD: &str = "the accounts lock is held";

#[cfg(test)]
thread_local! {
    static TEST_LADDER: std::cell::Cell<Option<&'static [std::time::Duration]>> =
        const { std::cell::Cell::new(None) };
}

/// The ladder [`Store::at`] and [`Store::at_for`] give a store: [`LOCK_LADDER`], or the one a
/// test set on this thread ([`with_store_ladder`]).
fn store_ladder() -> &'static [std::time::Duration] {
    #[cfg(test)]
    {
        // A test takes its file system place as it opens a store, before it can hold the
        // store's lock: a wait for a place under that lock would hold up each of the test's
        // threads that takes it.
        aterm_spec::fs_lane::join();
        if let Some(ladder) = TEST_LADDER.get() {
            return ladder;
        }
    }
    &LOCK_LADDER
}

/// `f` with every store this thread opens by path ([`Store::at`], [`Store::at_for`]) bounded
/// by `ladder` ([`Store::bounded_by`]), for a verb that opens its own store from an `Env`: at a
/// take on the store's own ladder ([`Store::acquire_bounded`], [`Store::signin_within`],
/// [`Store::recover_records`], [`Store::recover_state_records`], a renewal admission),
/// a lock its test holds throughout is refused at once, not after the ladder's pauses. A take
/// that names [`LOCK_LADDER`] itself still waits that ladder out.
#[cfg(test)]
pub(crate) fn with_store_ladder<T>(
    ladder: &'static [std::time::Duration],
    f: impl FnOnce() -> T,
) -> T {
    struct Restore(Option<&'static [std::time::Duration]>);
    impl Drop for Restore {
        fn drop(&mut self) {
            TEST_LADDER.set(self.0);
        }
    }
    let _restore = Restore(TEST_LADDER.replace(Some(ladder)));
    f()
}

/// The window name a candidate that failed with no wall of its own is struck under
/// ([`Strike::Failed`]).
pub const STRUCK_WINDOW: &str = "struck";

/// The window name a wall is recorded under when its row names no limit type and its text names
/// none either ([`record_wall`]).
pub const UNNAMED_WINDOW: &str = "limit";

/// The limit type of the five-hour window ([`Account::warned_session`]).
pub const SESSION_LIMIT: &str = "five_hour";

/// The limit type of the week of all models ([`Account::warned_week`]).
pub const WEEK_LIMIT: &str = "seven_day";

/// The limit type of Kimi Code's monthly total ([`note_panel_window`]).
pub const MONTH_LIMIT: &str = "monthly";

/// The variable that moves a process's Claude home ([`LOGIN_ENV`]).
pub const HOME_ENV: &str = "CLAUDE_CONFIG_DIR";

/// The variable Claude Code's `os.homedir()` reads.
pub const HOMEDIR_ENV: &str = if cfg!(windows) { "USERPROFILE" } else { "HOME" };

/// The variables that give a process a credential or a provider of its own
/// ([`super::netwatch::ROUTE_KEYS`], [`ProcessLogin::of_env`]).
///
/// Its job is attribution: every variable that moves a Claude Code process off its stored login,
/// so a process that sets one non-empty is judged outside its row (the vault token's descriptor
/// apart, where the wrapper's marker names it: [`super::token_route`]). It is not the list a
/// pooled line removes, which is [`crate::ai::connector::Connector::credential_env`] less
/// `KEPT_CREDENTIALS` ([`super::relaunch::ahead_env`]): that one keeps the Bedrock and Vertex
/// switches, so a pooled line never moves a session off the provider its person chose, and
/// merging the two would change what pooled lines remove
/// (`docs/DESIGN-claude-setup-token-2026-11-05.md` §5.8). aterm's token wrapper unsets every
/// name here before it serves (`accounts_token_run`), where only empty ones can remain.
pub const CREDENTIAL_KEYS: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "CLAUDE_CODE_OAUTH_TOKEN",
    "CLAUDE_CODE_OAUTH_REFRESH_TOKEN",
    "CLAUDE_CODE_API_KEY_FILE_DESCRIPTOR",
    "CLAUDE_CODE_OAUTH_TOKEN_FILE_DESCRIPTOR",
    "CLAUDE_CODE_GATEWAY_TOKEN_FILE_DESCRIPTOR",
    "CLAUDE_CODE_USE_ANTHROPIC_AWS",
    "CLAUDE_CODE_USE_ANTHROPIC_GOOGLE_CLOUD",
    "CLAUDE_CODE_USE_MANTLE",
    "CLAUDE_CODE_USE_GATEWAY",
    "CLAUDE_CODE_PROVIDER_MANAGED_BY_HOST",
    "CLAUDE_CODE_HOST_AUTH_ENV_VAR",
];

/// The variables of the vendor's `profile` source.
pub const PROFILE_KEYS: &[&str] = &[
    "ANTHROPIC_PROFILE",
    "ANTHROPIC_FEDERATION_RULE_ID",
    "ANTHROPIC_ORGANIZATION_ID",
    "ANTHROPIC_IDENTITY_TOKEN",
    "ANTHROPIC_IDENTITY_TOKEN_FILE",
    "ANTHROPIC_CONFIG_DIR",
];

/// Where a row stands in this Mac's order (`docs/DESIGN-alab-lab-model-2026-11-01.md` §0): a
/// derived "preferred here", never a claim on the account. Any number of Macs may prefer one
/// account and run it, so nothing reads it as "no other Mac used this account": the claims
/// ring, not a class, keeps two Macs off one login. A sync derives it (from the lab's
/// preferences, or on a Mac the lab does not name from the sheet or pool it reads), and
/// `accounts.toml` keeps it as the `class` word older builds read ([`Class::legacy_word`]), so a
/// roster read between syncs keeps the order and the installed release that shares the file
/// still loads it. The word is a serialization only: no reader takes it as a claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum Class {
    /// This Mac prefers it: tried before the others.
    #[default]
    Preferred,
    /// This Mac does not prefer it (another Mac does, or none): tried after the preferred ones.
    Pool,
}

impl Class {
    /// The word a report shows for it: `preferred` or `pool`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Class::Preferred => "preferred",
            Class::Pool => "pool",
        }
    }

    /// The word files that older builds read carry for it (`dedicated` for [`Class::Preferred`],
    /// `shared` for [`Class::Pool`]): `accounts.toml`'s and a fleet file's `class`, and the
    /// `local-class` a shared login's approved configuration requires.
    #[must_use]
    pub const fn legacy_word(self) -> &'static str {
        match self {
            Class::Preferred => "dedicated",
            Class::Pool => "shared",
        }
    }

    /// The order a `class` word gives ([`Self::legacy_word`]): a fleet file's, or a roster's an older
    /// build wrote.
    #[must_use]
    pub fn from_written(word: &str) -> Option<Class> {
        match word {
            "dedicated" => Some(Class::Preferred),
            "shared" => Some(Class::Pool),
            _ => None,
        }
    }
}

/// Admitted `accounts.toml` row, optionally bound to a renewed login directory via [`bound`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Row {
    /// The only name the account has anywhere.
    pub label: String,
    /// Preferred here or not ([`Class`]): an order, never a claim.
    pub class: Class,
    /// The login's directory, as a process's [`LOGIN_ENV`] holds it; `None` is the default login.
    pub dir: Option<String>,
    /// The account's email ([`email_matches`]).
    pub email: Option<String>,
    /// Claude's or Codex's expected provider email, when its login address is an alias.
    /// This explicit expectation is not proof of a login or budget authority.
    pub identity_email: Option<String>,
    /// The browser profile that holds the account's claude.ai session.
    pub browser: Option<String>,
    /// The logins a renewal here bound this row away from ([`Binding::was`], [`bound`]).
    pub was: Vec<Option<String>>,
    /// Its vendor keys ([`RowKeys`]): none on a Claude Code row.
    pub vendor: Vendor,
    /// When `ai-pool.toml` no longer names this Mac in the account's warm set, the current holder
    /// (`"m22"` or `"pool"`). Non-warm rows stay in `accounts.toml` (`01e7d42708`) and stop being
    /// candidates or sign-in targets until warmed here again (`DESIGN-alab-swarm` §6.5, C14).
    pub held_by: Option<String>,
}

/// A roster row's vendor keys, each admitted only by the CLI that takes it ([`RowKeys`],
/// [`Roster::parse_for`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Vendor {
    /// `region` (Kimi Code): the account system the row's email belongs to.
    pub region: Option<Region>,
    /// `flow` (Codex, Grok): the sign-in flow the row names; `None` takes the CLI's default.
    pub flow: Option<Flow>,
}

impl Row {
    /// The email the provider must report for this row, independently of its login address.
    #[must_use]
    pub fn expected_identity_email(&self) -> Option<&str> {
        self.identity_email.as_deref().or(self.email.as_deref())
    }

    /// Whether `self` matches `email` by either its login `email` or its `identity_email`.
    #[must_use]
    pub fn matches_email(&self, email: &str) -> bool {
        self.email
            .as_deref()
            .is_some_and(|e| email_matches(e, email))
            || self
                .identity_email
                .as_deref()
                .is_some_and(|e| email_matches(e, email))
    }

    /// The sign-in flow of this `cli` row: its own `flow`, else the CLI's default, `None` for a
    /// CLI that takes no `flow`.
    #[must_use]
    pub fn flow(&self, cli: Cli) -> Option<Flow> {
        match cli.accounts().keys {
            RowKeys::Flow(default) => Some(self.vendor.flow.unwrap_or(default)),
            RowKeys::Plain | RowKeys::Region => None,
        }
    }
}

/// Whether two configurations name the same provider realm. Login methods do
/// not distinguish account identities; region-keyed providers do.
#[must_use]
pub(crate) fn same_provider_realm(cli: Cli, left: Option<Region>, right: Option<Region>) -> bool {
    !matches!(cli.accounts().keys, RowKeys::Region) || left == right
}

/// Admitted `accounts.toml` roster in file order.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Roster {
    /// The rows, in file order.
    pub rows: Vec<Row>,
}

/// Returns whether `label` matches `[a-z0-9][a-z0-9._-]{0,63}` and is not [`RESERVED_LABEL`].
#[must_use]
pub fn admissible_label(label: &str) -> bool {
    label_refusal(label).is_none()
}

/// Returns the refusal reason when `label` is not a valid account label.
#[must_use]
pub fn label_refusal(label: &str) -> Option<&'static str> {
    let bytes = label.as_bytes();
    let word = |b: &u8| b.is_ascii_lowercase() || b.is_ascii_digit();
    let in_grammar = bytes.len() <= LABEL_MAX
        && bytes.first().is_some_and(word)
        && bytes
            .iter()
            .all(|b| word(b) || matches!(b, b'.' | b'_' | b'-'));
    if !in_grammar {
        Some("is outside [a-z0-9][a-z0-9._-]{0,63}")
    } else if label == RESERVED_LABEL {
        Some("is the harness command's help word, which no account can be named by")
    } else {
        None
    }
}

fn dir_refusal(dir: &str) -> Option<String> {
    if dir
        .chars()
        .any(|c| c.is_control() || c == '\u{2028}' || c == '\u{2029}')
    {
        Some("holds a control character".to_string())
    } else if dir.len() > DIR_MAX {
        Some(format!("is longer than {DIR_MAX} bytes"))
    } else if !Path::new(dir).is_absolute() {
        Some(
            "is relative; the login is named by this exact string, which must not depend on \
             where a relaunch starts"
                .to_string(),
        )
    } else {
        None
    }
}

/// Returns the refusal reason when `email` is not a valid roster email address.
#[must_use]
pub fn email_refusal(email: &str) -> Option<&'static str> {
    if email.len() > EMAIL_MAX {
        Some("is longer than 254 bytes")
    } else if email
        .chars()
        .any(|c| c.is_whitespace() || c.is_control() || c == '\u{2028}' || c == '\u{2029}')
    {
        Some("holds a space or a control character")
    } else {
        match email.split_once('@') {
            Some((local, domain)) if !local.is_empty() && !domain.is_empty() => {
                (domain.contains('@')).then_some("holds more than one @")
            }
            _ => Some("is not one address (`name@domain`)"),
        }
    }
}

/// Returns the refusal reason when `browser` is not a valid browser profile name.
#[must_use]
pub fn browser_refusal(browser: &str) -> Option<&'static str> {
    if browser.trim().is_empty() {
        Some("is empty")
    } else if browser.chars().count() > BROWSER_MAX {
        Some("is longer than 64 characters")
    } else if browser
        .chars()
        .any(|c| c.is_control() || c == '\u{2028}' || c == '\u{2029}')
    {
        Some("holds a control character")
    } else {
        None
    }
}

/// Compares two email addresses for ASCII case-insensitive equality.
#[must_use]
pub fn email_matches(row: &str, answered: &str) -> bool {
    row.eq_ignore_ascii_case(answered.trim())
}

fn shown(text: &str) -> String {
    one_line(text, 80)
}

impl Roster {
    /// Parses and validates an `accounts.toml` document.
    /// # Errors
    ///
    /// Each names the row and the reason: the text is not TOML; an unknown top-level key or
    /// table; `account` is not `[[account]]` rows; text exceeding [`MAX_ROSTER_BYTES`]; a row with an
    /// unknown key, with no label, an inadmissible one ([`RESERVED_LABEL`] by name) or a repeated
    /// one, with no class or one that is neither `dedicated` nor `shared`, with a `dir` that is
    /// not a string, is relative, holds a control character, is longer than [`DIR_MAX`] bytes or
    /// repeats another row's; with an `email` that is not one address ([`email_refusal`]; never
    /// quoted) or repeats another row's, or a `browser` that is not a profile's name
    /// ([`browser_refusal`]); or more than one row with no `dir`.
    pub fn parse(text: &str) -> Result<Roster, String> {
        Roster::parse_for(Cli::Claude, text)
    }

    /// Parses and validates `cli`'s roster document as [`Roster::parse`] does, each refusal
    /// naming that CLI's file, with the vendor keys it takes ([`RowKeys`]) admitted.
    /// # Errors
    ///
    /// As [`Roster::parse`], and: a vendor key this CLI does not take (Claude Code takes none);
    /// a `region` other than `global` or `mainland-cn`, or a Kimi row with a declared identity and no
    /// `region`; a `flow` other than `browser` or `device`; `identity_email` outside
    /// Claude, Codex, Kimi or agy, an invalid identity address, or two rows expecting the same identity.
    /// Duplicate email addresses are scoped to one provider realm.
    pub fn parse_for(cli: Cli, text: &str) -> Result<Roster, String> {
        let file = cli.accounts().roster_file;
        if text.len() as u64 > MAX_ROSTER_BYTES {
            return Err(format!("{file}: roster exceeds {MAX_ROSTER_BYTES} bytes"));
        }
        let doc = aterm_toml::from_str::<aterm_toml::Value>(text).map_err(|e| {
            format!(
                "{file}: not valid TOML ({})",
                shown(e.to_string().lines().next().unwrap_or(""))
            )
        })?;
        let Some(top) = doc.as_table() else {
            return Err(format!("{file}: not a TOML document"));
        };
        for (key, value) in top {
            if key != "account" {
                let what = if value.as_table().is_some() {
                    "table"
                } else {
                    "key"
                };
                return Err(format!(
                    "{file}: unknown {what} `{}`; this file holds [[account]] rows and \
                     nothing else",
                    shown(key)
                ));
            }
        }
        let items = match top.get("account") {
            None => return Ok(Roster::default()),
            Some(aterm_toml::Value::Array(items)) => items,
            Some(other) => {
                return Err(format!(
                    "{file}: `account` is a TOML {}, not [[account]] rows",
                    other.type_str()
                ));
            }
        };
        let mut rows: Vec<Row> = Vec::with_capacity(items.len());
        for (i, item) in items.iter().enumerate() {
            let row = row_of(cli, item, i + 1)?;
            if let Some(twin) = rows.iter().position(|r| r.label == row.label) {
                return Err(format!(
                    "{file}: label `{}` is listed twice (rows {} and {})",
                    row.label,
                    twin + 1,
                    i + 1
                ));
            }
            if let Some(twin) = rows.iter().find(|r| {
                same_provider_realm(cli, r.vendor.region, row.vendor.region)
                    && r.email
                        .as_deref()
                        .zip(row.email.as_deref())
                        .is_some_and(|(a, b)| a.eq_ignore_ascii_case(b))
            }) {
                return Err(format!(
                    "{file}: rows `{}` and `{}` name one email in the same provider realm; an account is one row: {}",
                    twin.label,
                    row.label,
                    drop_one_of(twin, &row)
                ));
            }
            if matches!(cli, Cli::Claude | Cli::Codex | Cli::Kimi | Cli::Agy)
                && let Some(twin) = rows.iter().find(|r| {
                    same_provider_realm(cli, r.vendor.region, row.vendor.region)
                        && r.expected_identity_email()
                            .zip(row.expected_identity_email())
                            .is_some_and(|(a, b)| email_matches(a, b))
                })
            {
                return Err(format!(
                    "{file}: rows `{}` and `{}` expect one provider identity; an account is one row: {}",
                    twin.label,
                    row.label,
                    drop_one_of(twin, &row)
                ));
            }
            if let Some(twin) = rows.iter().find(|r| r.dir == row.dir) {
                return Err(match &row.dir {
                    Some(_) => format!(
                        "{file}: rows `{}` and `{}` name one dir; a login is one account",
                        twin.label, row.label
                    ),
                    None => format!(
                        "{file}: rows `{}` and `{}` both name no dir; only one row is \
                         this machine's own login",
                        twin.label, row.label
                    ),
                });
            }
            rows.push(row);
        }
        Ok(Roster { rows })
    }

    /// Loads the roster at `path`; an absent file is an empty roster.
    /// # Errors
    ///
    /// The file exists and cannot be read, is larger than [`MAX_ROSTER_BYTES`], is not UTF-8,
    /// or does not admit ([`Roster::parse`]).
    pub fn load(path: &Path) -> Result<Roster, String> {
        Roster::load_for(Cli::Claude, path)
    }

    /// Loads `cli`'s roster at `path` ([`Roster::load`], [`Roster::parse_for`]).
    /// # Errors
    ///
    /// As [`Roster::load`], the text parsed by [`Roster::parse_for`].
    pub fn load_for(cli: Cli, path: &Path) -> Result<Roster, String> {
        match roster_text(path)? {
            None => Ok(Roster::default()),
            Some(text) => Roster::parse_for(cli, &text),
        }
    }

    /// Looks up the roster row with `label`.
    #[must_use]
    pub fn row(&self, label: &str) -> Option<&Row> {
        self.rows.iter().find(|r| r.label == label)
    }

    /// Looks up the roster row matching a process's [`LOGIN_ENV`] value.
    #[must_use]
    pub fn row_for_login(&self, login: Option<&str>) -> Option<&Row> {
        match login {
            None | Some("") => self.rows.iter().find(|r| r.dir.is_none()),
            Some(dir) => self.rows.iter().find(|r| r.dir.as_deref() == Some(dir)),
        }
    }

    /// Returns whether the roster lists two or more accounts.
    #[must_use]
    pub const fn several(&self) -> bool {
        self.rows.len() >= 2
    }

    /// Returns the login directory of a single-row roster when it is non-default.
    #[must_use]
    pub fn lone_login(&self) -> Option<&str> {
        match self.rows.as_slice() {
            [row] => row.dir.as_deref().filter(|dir| !dir.is_empty()),
            _ => None,
        }
    }

    /// Synthetic single-row roster representing this machine's default login.
    #[must_use]
    pub fn own() -> Roster {
        Roster {
            rows: vec![Row {
                label: super::cli::ACCOUNTS_OWN_LABEL.to_owned(),
                ..Row::default()
            }],
        }
    }

    /// Finds the roster row whose login was moved away from `login` by a renewal on this machine.
    #[must_use]
    pub fn row_left(&self, login: Option<&str>) -> Option<&Row> {
        let login = login.filter(|l| !l.is_empty());
        if self.row_for_login(login).is_some() {
            return None;
        }
        self.rows
            .iter()
            .find(|row| row.was.iter().any(|was| was.as_deref() == login))
    }

    /// The rows a session on the login at `login` (`None`: the default one) belongs to: the row
    /// that holds it now, else each row that left it ([`Row::was`]). A row's `was` keeps naming what
    /// it left, so once another row takes that login (a default login reassigned,
    /// `accounts_sync::reassign_codex_default`), the row holding it is its account's.
    #[must_use]
    pub fn rows_of_login(&self, login: Option<&str>) -> Vec<&Row> {
        let holding: Vec<&Row> = self
            .rows
            .iter()
            .filter(|row| row.dir.as_deref() == login)
            .collect();
        if !holding.is_empty() {
            return holding;
        }
        self.rows
            .iter()
            .filter(|row| row.was.iter().any(|was| was.as_deref() == login))
            .collect()
    }
}

/// Reads the raw text of the roster file at `path` ([`MAX_ROSTER_BYTES`]).
/// # Errors
///
/// The file exists and cannot be read, is larger than [`MAX_ROSTER_BYTES`]
/// or is not UTF-8 — named with its path.
pub fn roster_text(path: &Path) -> Result<Option<String>, String> {
    read_bounded(path, MAX_ROSTER_BYTES).map_err(|e| format!("{}: {e}", path.display()))
}

/// Which of two rows naming one account a person removes to admit the roster: the row with a login
/// directory of its own beside the row on this machine's own login, which keeps the account on
/// the login this machine runs (a lab sync then gives that row the lab's label), else the later
/// row, which the first one already covers.
fn drop_one_of(first: &Row, later: &Row) -> String {
    let (keep, drop) = if later.dir.is_none() {
        (later, first)
    } else {
        (first, later)
    };
    format!("remove `{}` and keep `{}`", drop.label, keep.label)
}

fn row_of(cli: Cli, item: &aterm_toml::Value, n: usize) -> Result<Row, String> {
    let file = cli.accounts().roster_file;
    let Some(table) = item.as_table() else {
        return Err(format!(
            "{file}: row {n} is a TOML {}, not an [[account]] table",
            item.type_str()
        ));
    };
    let label = match table.get("label") {
        Some(aterm_toml::Value::String(label)) => label,
        Some(other) => {
            return Err(format!(
                "{file}: row {n}: its label is a TOML {}, not a string",
                other.type_str()
            ));
        }
        None => return Err(format!("{file}: row {n} has no label")),
    };
    if let Some(why) = label_refusal(label) {
        return Err(format!("{file}: row {n}: label `{}` {why}", shown(label)));
    }
    let keys = cli.accounts().keys;
    let vendor_key = match keys {
        RowKeys::Plain => None,
        RowKeys::Region => Some("region"),
        RowKeys::Flow(_) => Some("flow"),
    };
    if let Some(key) = table.keys().find(|k| {
        !matches!(
            k.as_str(),
            "label" | "class" | "dir" | "email" | "browser" | "held_by"
        ) && !(matches!(cli, Cli::Claude | Cli::Codex | Cli::Kimi | Cli::Agy)
            && k.as_str() == "identity_email")
            && Some(k.as_str()) != vendor_key
    }) {
        let holds = vendor_key.map_or_else(
            || "email, browser and held_by".to_owned(),
            |key| format!("email, browser, held_by and {key}"),
        );
        let holds = if matches!(cli, Cli::Claude | Cli::Codex | Cli::Kimi | Cli::Agy) {
            format!("{holds}, and identity_email")
        } else {
            holds
        };
        return Err(format!(
            "{file}: row `{label}` has an unknown key `{}`; a row holds label, class, \
             dir, {holds}",
            shown(key)
        ));
    }
    // The order is stored as the `class` word every build reads: a serialization, never a claim.
    let class = match table.get("class") {
        Some(aterm_toml::Value::String(word)) => Class::from_written(word).ok_or_else(|| {
            format!(
                "{file}: row `{label}`: class `{}` is neither `dedicated` nor `shared`",
                shown(word)
            )
        })?,
        Some(other) => {
            return Err(format!(
                "{file}: row `{label}`: its class is a TOML {}, not `dedicated` or `shared`",
                other.type_str()
            ));
        }
        None => {
            return Err(format!(
                "{file}: row `{label}` has no class (`dedicated` or `shared`)"
            ));
        }
    };
    let dir = match table.get("dir") {
        None => None,
        Some(aterm_toml::Value::String(dir)) => {
            if let Some(why) = dir_refusal(dir) {
                return Err(format!("{file}: row `{label}`: its dir {why}"));
            }
            Some(dir.clone())
        }
        Some(other) => {
            return Err(format!(
                "{file}: row `{label}`: its dir is a TOML {}, not a string",
                other.type_str()
            ));
        }
    };
    let email = match table.get("email") {
        None => None,
        Some(aterm_toml::Value::String(email)) => {
            if let Some(why) = email_refusal(email) {
                return Err(format!("{file}: row `{label}`: its email {why}"));
            }
            Some(email.clone())
        }
        Some(other) => {
            return Err(format!(
                "{file}: row `{label}`: its email is a TOML {}, not a string",
                other.type_str()
            ));
        }
    };
    let browser = match table.get("browser") {
        None => None,
        Some(aterm_toml::Value::String(browser)) => {
            if let Some(why) = browser_refusal(browser) {
                return Err(format!("{file}: row `{label}`: its browser {why}"));
            }
            Some(browser.clone())
        }
        Some(other) => {
            return Err(format!(
                "{file}: row `{label}`: its browser is a TOML {}, not a string",
                other.type_str()
            ));
        }
    };
    let identity_email = match table.get("identity_email") {
        None => None,
        Some(aterm_toml::Value::String(email)) => {
            if let Some(why) = email_refusal(email) {
                return Err(format!("{file}: row `{label}`: its identity_email {why}"));
            }
            Some(email.clone())
        }
        Some(_) => {
            return Err(format!(
                "{file}: row `{label}`: identity_email must be a string"
            ));
        }
    };
    let held_by = match table.get("held_by") {
        None => None,
        Some(aterm_toml::Value::String(holder)) => {
            if let Some(why) = label_refusal(holder) {
                return Err(format!("{file}: row `{label}`: its held_by {why}"));
            }
            Some(holder.clone())
        }
        Some(other) => {
            return Err(format!(
                "{file}: row `{label}`: its held_by is a TOML {}, not a string",
                other.type_str()
            ));
        }
    };
    let word = |key: &str| match table.get(key) {
        None => Ok(None),
        Some(aterm_toml::Value::String(word)) => Ok(Some(word.as_str())),
        Some(other) => Err(format!(
            "{file}: row `{label}`: its {key} is a TOML {}, not a string",
            other.type_str()
        )),
    };
    let vendor = match keys {
        RowKeys::Plain => Vendor::default(),
        RowKeys::Region => {
            let region = word("region")?
                .map(|w| {
                    Region::parse(w).ok_or_else(|| {
                        format!(
                            "{file}: row `{label}`: region `{}` is neither `global` (kimi.ai) \
                             nor `mainland-cn` (kimi.com)",
                            shown(w)
                        )
                    })
                })
                .transpose()?;
            if region.is_none() && (email.is_some() || identity_email.is_some()) {
                let address = if email.is_some() {
                    "an email"
                } else {
                    "an identity_email"
                };
                return Err(format!(
                    "{file}: row `{label}` has {address} and no region: name `global` (kimi.ai) \
                     or `mainland-cn` (kimi.com), since a sign-in on the other region's site can \
                     make a new account"
                ));
            }
            Vendor { region, flow: None }
        }
        RowKeys::Flow(_) => Vendor {
            region: None,
            flow: word("flow")?
                .map(|w| {
                    Flow::parse(w).ok_or_else(|| {
                        format!(
                            "{file}: row `{label}`: flow `{}` is neither `browser` nor `device`",
                            shown(w)
                        )
                    })
                })
                .transpose()?,
        },
    };
    Ok(Row {
        label: label.clone(),
        class,
        dir,
        email,
        identity_email,
        browser,
        was: Vec::new(),
        vendor,
        held_by,
    })
}

/// Returns the `(`[`LOGIN_ENV`]`, value)` pair exported on a relaunch line for `row`.
#[must_use]
pub fn login_env(row: &Row) -> (&'static str, String) {
    (LOGIN_ENV, row.dir.clone().unwrap_or_default())
}

/// What a launch of `cli` under `row` sets ([`Seam`]): the seam's variable, and its value, or
/// `None` for the machine's default login (no `dir`), whose launch unsets the variable. A
/// [`Seam::LoginFile`] value names the file in the row's directory, the others the directory.
#[must_use]
pub fn login_env_for(cli: Cli, row: &Row) -> (&'static str, Option<String>) {
    let seam = cli.accounts().seam;
    let value = row
        .dir
        .as_deref()
        .filter(|dir| !dir.is_empty())
        .map(|dir| match seam {
            Seam::LoginFile { file, .. } => {
                Path::new(dir).join(file).to_string_lossy().into_owned()
            }
            Seam::LoginDir { .. } | Seam::Overlay { .. } => dir.to_owned(),
        });
    (seam.var(), value)
}

fn row_refusal(row: &Row) -> Option<String> {
    let label = &row.label;
    if let Some(why) = label_refusal(label) {
        return Some(format!("label `{}` {why}", shown(label)));
    }
    if let Some(why) = row.dir.as_deref().and_then(dir_refusal) {
        return Some(format!("row `{label}`: its dir {why}"));
    }
    if let Some(why) = row.email.as_deref().and_then(email_refusal) {
        return Some(format!("row `{label}`: its email {why}"));
    }
    if let Some(why) = row.identity_email.as_deref().and_then(email_refusal) {
        return Some(format!("row `{label}`: its identity_email {why}"));
    }
    if let Some(why) = row.held_by.as_deref().and_then(label_refusal) {
        return Some(format!("row `{label}`: its held_by {why}"));
    }
    row.browser
        .as_deref()
        .and_then(browser_refusal)
        .map(|why| format!("row `{label}`: its browser {why}"))
}

/// Formats a complete [`Row`] (including login and identity emails) as an `[[account]]` TOML block.
#[must_use]
pub fn row_text(row: &Row) -> Option<String> {
    if row_refusal(row).is_some() {
        return None;
    }
    // The order is written as the `class` word older builds read, so the installed release
    // on a Mac a newer build synced keeps loading the roster it shares.
    let mut out = format!(
        "[[account]]\nlabel = \"{}\"\nclass = \"{}\"\n",
        row.label,
        row.class.legacy_word()
    );
    let mut put = |key: &str, value: Option<&str>| {
        if let Some(value) = value {
            out.push_str(&format!("{key} = {}\n", aterm_toml::Value::from(value)));
        }
    };
    put("dir", row.dir.as_deref());
    put("email", row.email.as_deref());
    put("identity_email", row.identity_email.as_deref());
    put("browser", row.browser.as_deref());
    put("held_by", row.held_by.as_deref());
    put("region", row.vendor.region.map(Region::as_str));
    put("flow", row.vendor.flow.map(Flow::as_str));
    Some(out)
}

/// Stable metadata witness for a pool view and later native admission. This is
/// not an account identity or a credential capability: the writer must recheck
/// its source, current policy and the selected row's provider identity.
/// # Errors
/// A raw roster row cannot be represented canonically.
pub fn pool_authority_revision(raw: &Roster, logins: &Logins) -> Result<String, String> {
    let rows = pool_authority_rows(raw)?;
    let bindings = logins
        .labels
        .iter()
        .map(|(label, binding)| pool_authority_binding(label, binding))
        .collect::<Vec<_>>();
    let bytes = aterm_json::to_vec(
        &aterm_json::json!({"domain": "aterm-codex-pool-authority-v1",
        "rows": rows, "generation_clock": logins.generation_clock, "bindings": bindings}),
    )
    .map_err(|_| "pool authority metadata cannot be encoded".to_string())?;
    Ok(aterm_codec::hex::encode_lower(
        &aterm_digest::Sha256::digest(bytes),
    ))
}

// Shared canonical encodings: scoped native witnesses must retain the exact
// same roster/binding policy fields as the original whole-pool witness.
pub(super) fn pool_authority_rows(raw: &Roster) -> Result<Vec<aterm_json::Value>, String> {
    raw.rows
        .iter()
        .map(|row| {
            row_text(row)
                .map(|text| aterm_json::json!({"row": text, "was": row.was}))
                .ok_or_else(|| "pool authority row cannot be represented".to_string())
        })
        .collect()
}
pub(super) fn pool_authority_binding(label: &str, binding: &Binding) -> aterm_json::Value {
    aterm_json::json!({"label": label, "dir": binding.dir,
        "renewal_generation": binding.renewal_generation,
        "last_success_generation": binding.last_success_generation,
        "proved_email": binding.proved_email, "proved_dir": binding.proved_dir,
        "proved_by": binding.proved_by.map(Proof::as_str),
        "ended_at": binding.ended_at, "wrong_at": binding.wrong_at, "was": binding.was})
}

/// Formats a single account row as an inline TOML table.
#[must_use]
pub fn row_inline(label: &str, class: Class, dir: Option<&str>) -> Option<String> {
    row_inline_of(&Row {
        label: label.to_owned(),
        class,
        dir: dir.map(str::to_owned),
        ..Row::default()
    })
}

/// What a row's `email` is written as in a row to add by hand ([`row_inline_of`]).
pub const EMAIL_BY_HAND: &str = "<the --email you gave>";

fn row_inline_of(row: &Row) -> Option<String> {
    if row_refusal(row).is_some() {
        return None;
    }
    let mut out = format!(
        "{{ label = \"{}\", class = \"{}\"",
        row.label,
        row.class.legacy_word()
    );
    if let Some(dir) = &row.dir {
        out.push_str(&format!(
            ", dir = {}",
            aterm_toml::Value::from(dir.as_str())
        ));
    }
    if row.email.is_some() {
        out.push_str(&format!(
            ", email = {}",
            aterm_toml::Value::from(EMAIL_BY_HAND)
        ));
    }
    if row.identity_email.is_some() {
        out.push_str(", identity_email = \"<the --identity-email you gave>\"");
    }
    if let Some(browser) = &row.browser {
        out.push_str(&format!(
            ", browser = {}",
            aterm_toml::Value::from(browser.as_str())
        ));
    }
    if let Some(held_by) = &row.held_by {
        out.push_str(&format!(
            ", held_by = {}",
            aterm_toml::Value::from(held_by.as_str())
        ));
    }
    for (key, word) in [
        ("region", row.vendor.region.map(Region::as_str)),
        ("flow", row.vendor.flow.map(Flow::as_str)),
    ] {
        if let Some(word) = word {
            out.push_str(&format!(", {key} = {}", aterm_toml::Value::from(word)));
        }
    }
    out.push_str(" }");
    Some(out)
}

/// Appends a new `(label, class, dir)` row to `existing` `accounts.toml` text and validates the
/// result ([`append_rows`]).
/// # Errors
///
/// As [`append_rows`], of which this is the one-row case.
pub fn append_row(
    existing: &str,
    label: &str,
    class: Class,
    dir: Option<&str>,
) -> Result<String, String> {
    append_rows(
        existing,
        &[Row {
            label: label.to_owned(),
            class,
            dir: dir.map(str::to_owned),
            ..Row::default()
        }],
    )
}

/// Appends `rows` in order to `existing` `accounts.toml` text and validates the result.
/// # Errors
///
/// A row would not admit, `existing` does not admit, or the whole does not (a repeated label,
/// `dir` or `email`, a second row with no `dir`, text exceeding [`MAX_ROSTER_BYTES`]), each as
/// [`Roster::parse`] names it, so nothing is written that the next read refuses. A roster that
/// defines `account` inline (`account = [ { … } ]`, or `account = []`) refuses too, since an
/// `[[account]]` table cannot extend that array: the refusal carries every row to add by hand
/// ([`row_inline`]), in order, an email written as [`EMAIL_BY_HAND`].
pub fn append_rows(existing: &str, rows: &[Row]) -> Result<String, String> {
    append_rows_for(Cli::Claude, existing, rows)
}

/// Appends `rows` in order to `cli`'s roster text and validates the result ([`append_rows`],
/// [`Roster::parse_for`]).
/// # Errors
///
/// As [`append_rows`], each refusal naming `cli`'s roster file and parsed by
/// [`Roster::parse_for`].
pub fn append_rows_for(cli: Cli, existing: &str, rows: &[Row]) -> Result<String, String> {
    let file = cli.accounts().roster_file;
    if let Some(why) = rows.iter().find_map(row_refusal) {
        return Err(why);
    }
    Roster::parse_for(cli, existing)?;
    let mut out = existing.to_string();
    for row in rows {
        let text = row_text(row).ok_or_else(|| format!("row `{}` does not admit", row.label))?;
        if !out.is_empty() {
            if !out.ends_with('\n') {
                out.push('\n');
            }
            out.push('\n');
        }
        out.push_str(&text);
    }
    if aterm_toml::from_str::<aterm_toml::Value>(&out).is_err() {
        let by_hand: Vec<String> = rows.iter().filter_map(row_inline_of).collect();
        let these = if by_hand.len() == 1 {
            "this row"
        } else {
            "these rows"
        };
        return Err(format!(
            "{file} defines `account` inline (`account = [ … ]`), which an [[account]] \
             table cannot extend; rewrite its rows as [[account]] tables, or add {these} to \
             that array by hand: {}",
            by_hand.join(", ")
        ));
    }
    Roster::parse_for(cli, &out)?;
    Ok(out)
}

/// Returns the set of roster labels whose login directories are enrolled on this machine.
#[must_use]
pub fn enrolled(roster: &Roster) -> BTreeSet<String> {
    roster
        .rows
        .iter()
        .filter(|r| r.dir.as_deref().is_none_or(|d| login_made(Path::new(d))))
        .map(|r| r.label.clone())
        .collect()
}

/// Returns the set of `cli`'s roster labels whose logins are made on this machine ([`enrolled`]
/// for Claude Code's): another CLI's row is enrolled once its directory holds the file the CLI
/// keeps its login in ([`crate::ai::connector::stored_login_file`]), since an overlay home holds
/// links and scratch before any sign-in.
#[must_use]
pub fn enrolled_for(cli: Cli, roster: &Roster) -> BTreeSet<String> {
    if cli == Cli::Claude {
        return enrolled(roster);
    }
    roster
        .rows
        .iter()
        .filter(|r| {
            r.dir.as_deref().is_none_or(|d| {
                crate::ai::connector::stored_login_file(cli.connector(), Path::new(d)).is_some()
            })
        })
        .map(|r| r.label.clone())
        .collect()
}

/// [`enrolled_for`] on the machine whose user home is `home`: a row of a CLI other than Claude
/// Code with no directory of its own is the CLI's default login, enrolled only while the CLI's
/// default home there holds its login file ([`default_login_made`]), as `aterm ai` reports it
/// (`ai::facts`). Claude Code's default login is enrolled as [`enrolled`] says.
#[must_use]
pub fn enrolled_in(cli: Cli, roster: &Roster, home: &Path) -> BTreeSet<String> {
    let mut enrolled = enrolled_for(cli, roster);
    if cli != Cli::Claude && !default_login_made(cli, home) {
        for row in roster.rows.iter().filter(|row| row.dir.is_none()) {
            enrolled.remove(&row.label);
        }
    }
    enrolled
}

/// The address `cli`'s default login under the user home `home` names, where aterm reads one
/// without asking the OS keyring or running the CLI: Codex's `auth.json` under a file store.
/// `None` for every other CLI, under a keyring store, and where the file names no address.
#[must_use]
pub fn default_login_address(cli: Cli, home: &Path) -> Option<String> {
    if cli != Cli::Codex {
        return None;
    }
    let dir = home.join(cli.accounts().default_home);
    if !crate::ai::facts::codex_file_store(&dir) {
        return None;
    }
    super::codex_app_login::read_account_email(&dir)
        .map(|email| email.trim().to_owned())
        .filter(|email| email_refusal(email).is_none())
}

/// The row on the default login whose address is not the one that login names (`login`,
/// [`default_login_address`]): that login moved off its account, so a launch or a handoff on the
/// row is refused ([`super::accounts_pool::check_launch_login_identity`]).
#[must_use]
pub fn default_login_moved_off<'a>(roster: &'a Roster, login: Option<&str>) -> Option<&'a Row> {
    let row = roster.row_for_login(None)?;
    let expected = row.expected_identity_email()?;
    (!email_matches(expected, login?)).then_some(row)
}

/// One account listed twice: the row on the default login that names no address, and a row of its
/// own whose address is the one that login names (`login`, [`default_login_address`]).
#[must_use]
pub fn default_login_twin<'a>(
    roster: &'a Roster,
    login: Option<&str>,
) -> Option<(&'a Row, &'a Row)> {
    let login = login?;
    let own = roster.row_for_login(None)?;
    if own.expected_identity_email().is_some() {
        return None;
    }
    let twin = roster.rows.iter().find(|row| {
        row.dir.is_some()
            && row
                .expected_identity_email()
                .is_some_and(|held| email_matches(held, login))
    })?;
    Some((own, twin))
}

/// Whether `cli`'s default home under the user home `home` holds the file the CLI keeps its
/// login in ([`crate::ai::connector::stored_login_file`]).
#[must_use]
pub fn default_login_made(cli: Cli, home: &Path) -> bool {
    let dir = home.join(cli.accounts().default_home);
    crate::ai::connector::stored_login_file(cli.connector(), &dir).is_some()
}

/// Returns whether `dir` contains an entry other than aterm's renewal bookkeeping.
/// This is an initialization footprint, not authentication: the provider's status and
/// identity checks decide whether the login works. A checkpoint or its interrupted
/// publication directory alone cannot enroll a login.
#[must_use]
pub fn login_made(dir: &Path) -> bool {
    login_made_checked(dir).unwrap_or(false)
}

/// The same footprint decision, retaining I/O uncertainty when no entry proves presence.
/// # Errors
/// The directory or an entry cannot be read and no known entry establishes a footprint.
pub(super) fn login_made_checked(dir: &Path) -> io::Result<bool> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    let mut unavailable = None;
    for entry in entries {
        match entry {
            Ok(entry) => {
                if !login_bookkeeping(&entry.file_name()) {
                    return Ok(true);
                }
            }
            Err(error) => unavailable = Some(error),
        }
    }
    unavailable.map_or(Ok(false), Err)
}

/// Whether `name` is a file aterm keeps beside a login, which holds nobody's: a renewal's checkpoint
/// or its stage, or the lock on a login's file, which every take of it leaves
/// ([`super::codex_app_login::AuthLock`]).
pub(super) fn login_bookkeeping(name: &std::ffi::OsStr) -> bool {
    name == RENEWAL_CHECKPOINT_FILE
        || name == super::codex_app_login::AUTH_LOCK_FILE
        || name
            .to_str()
            .is_some_and(|name| name.starts_with(RENEWAL_CHECKPOINT_STAGE_PREFIX))
}

/// Whether `row`'s login is here: the enrolled set is the one answer, a default-home row
/// included ([`enrolled`] and [`enrolled_for`] hold it; [`enrolled_in`] drops a missing one).
fn is_enrolled(row: &Row, enrolled: &BTreeSet<String>) -> bool {
    enrolled.contains(&row.label)
}

/// Recovers and reads the bound [`Roster`] used to attribute usage-tap readings at `now`.
/// Unavailable account publication never falls back to the default login. With no configured
/// path, only local record publication can recover; a pending roster-bound or damaged receipt
/// refuses that fallback.
#[must_use]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountTapRead",
        action = "Read",
        project = "aterm_agent::harness::accounts::tap_recovery_tests::tap_transition"
    )
)]
pub fn tap_roster(path: Option<&Path>, store: &Store, now: i64) -> Option<Roster> {
    let roster = store.read_roster(path, now).ok()?;
    if roster.rows.is_empty() {
        return Some(Roster::own());
    }
    Some(roster)
}

/// Returns [`Roster::lone_login`] for the bound roster at `path`.
#[must_use]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountTapRead",
        action = "Read",
        project = "aterm_agent::harness::accounts::tap_recovery_tests::tap_transition"
    )
)]
pub fn lone_login_at(path: Option<&Path>, store: &Store, now: i64) -> Option<String> {
    store
        .read_bound_roster(path?, now)
        .ok()
        .filter(|roster| roster.rows.len() == 1)?
        .lone_login()
        .map(str::to_owned)
}

/// Every login the roster at `path` names as `store` binds it at `now`: each row's `dir` and each
/// login a renewal bound a row away from ([`Row::was`]). These are the usage tap's logins where
/// the machine has no lone one ([`lone_login_at`]). Empty when the roster lists fewer than two
/// rows (one row follows only its current login) or the store's logins cannot be read, since
/// [`tap_roster`] and the footer's account gate refuse that roster too.
#[must_use]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountTapRead",
        action = "Read",
        project = "aterm_agent::harness::accounts::tap_recovery_tests::tap_transition"
    )
)]
pub fn listed_logins_at(path: Option<&Path>, store: &Store, now: i64) -> Vec<String> {
    let Some(roster) = path
        .and_then(|path| store.read_bound_roster(path, now).ok())
        .filter(Roster::several)
    else {
        return Vec::new();
    };
    roster
        .rows
        .iter()
        .flat_map(|row| row.dir.iter().chain(row.was.iter().flatten()))
        .filter(|dir| !dir.is_empty())
        .cloned()
        .collect()
}

/// Returns the `accounts.toml` path sibling to the given `aterm.toml` `config` path.
#[must_use]
pub fn roster_beside(config: &Path) -> Option<PathBuf> {
    Some(config.parent()?.join(ACCOUNTS_FILE))
}

/// Returns `cli`'s roster path ([`Cli`]) sibling to the given `aterm.toml` `config` path.
#[must_use]
pub fn roster_beside_for(config: &Path, cli: Cli) -> Option<PathBuf> {
    Some(config.parent()?.join(cli.accounts().roster_file))
}

/// Returns the default `accounts.toml` path beside the active `aterm.toml`.
#[must_use]
pub fn default_roster_path() -> Option<PathBuf> {
    roster_beside(&aterm_types::dirs::aterm_config_path()?)
}

/// Login directory and credential-override facts extracted from a process's environment and
/// settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessLogin {
    /// The value of [`LOGIN_ENV`] where it is set and not empty.
    pub login: Option<String>,
    /// Its Claude home is the default one ([`HOME_ENV`]).
    pub default_home: bool,
    /// How it authenticates ([`ProcessLogin::of_env`]), before a gate resolves its account
    /// ([`AuthRoute::for_account`]).
    pub route: AuthRoute,
}

/// Whether a set variable `name = value` moves a Claude Code process off its stored login: a
/// credential, profile or route variable ([`CREDENTIAL_KEYS`], [`PROFILE_KEYS`],
/// [`super::netwatch::ROUTE_KEYS`] and its prefixes), whatever its value, but the lab stub's
/// route its development seam admits ([`super::netwatch::lab_route`]; never in a shipped binary).
pub(super) fn names_another_login(name: &str, value: &str) -> bool {
    use super::netwatch::{ROUTE_KEYS, ROUTE_PREFIXES, lab_route};
    CREDENTIAL_KEYS.contains(&name)
        || PROFILE_KEYS.contains(&name)
        || ((ROUTE_KEYS.contains(&name) || ROUTE_PREFIXES.iter().any(|p| name.starts_with(p)))
            && !lab_route(name, value))
}

/// Returns the vendor profile store's `configs` directory for a process with environment `env`.
#[must_use]
pub fn profile_configs_dir<'a>(
    env: impl IntoIterator<Item = (&'a str, &'a str)>,
) -> Option<PathBuf> {
    let (mut named, mut xdg, mut home) = (None, None, None);
    for (name, value) in env {
        let slot = match name {
            "ANTHROPIC_CONFIG_DIR" => &mut named,
            "XDG_CONFIG_HOME" => &mut xdg,
            "HOME" => &mut home,
            _ => continue,
        };
        if !value.trim().is_empty() {
            slot.get_or_insert(value.trim());
        }
    }
    let store = named
        .map(PathBuf::from)
        .or_else(|| xdg.map(|dir| Path::new(dir).join("anthropic")))
        .or_else(|| home.map(|dir| Path::new(dir).join(".config").join("anthropic")))?;
    Some(store.join("configs"))
}

/// Returns whether the vendor profile `configs` directory contains any `.json` profile entry.
#[must_use]
pub fn profile_configured(configs: &Path) -> bool {
    match std::fs::read_dir(configs) {
        Ok(entries) => entries.into_iter().any(|entry| {
            entry.is_ok_and(|e| {
                Path::new(&e.file_name())
                    .extension()
                    .is_some_and(|x| x == "json")
            })
        }),
        Err(e) => e.kind() != io::ErrorKind::NotFound,
    }
}

/// What a Claude relaunch's account checks (a cold admission, an external route's recheck) read
/// of the environment the relaunch inherits, recorded from a Claude Code process's own kernel
/// exec environment while it ran: the environment its shell passed it, which a relaunch typed
/// into that shell inherits too. A shell's own kernel record holds only what the shell was
/// started with, and macOS omits even that for an Apple binary such as `/bin/zsh` or
/// `/bin/bash` (measured on macOS 26.7.2 and 27.0.1).
///
/// Only the entries those checks read are kept ([`Self::reads`]), and only for a process that
/// runs on its account's own credential ([`Self::admitted`]): such an environment sets no
/// credential, profile or route variable ([`ProcessLogin::of_env`]). A token session's own
/// descriptor variable and marker are not kept either: aterm's wrapper made them, not the shell,
/// and it makes them afresh for a relaunch ([`super::token_route::TOKEN_STATE_ENV`]). So a
/// record holds directories and selectors, never a secret, and reads as a stored login's.
#[derive(Clone, PartialEq, Eq)]
pub struct LaunchEnv(Vec<String>);

impl LaunchEnv {
    /// The record of `env`, a process's kernel exec environment (`KEY=value` entries): `None`
    /// for an empty one (an unread environment) or one [`Self::admitted`] refuses.
    #[must_use]
    pub fn of_exec_env(env: &[String]) -> Option<Self> {
        if env.is_empty() {
            return None;
        }
        let pairs = env.iter().filter_map(|entry| entry.split_once('='));
        let mut kept = Self::kept(env);
        match ProcessLogin::of_env(pairs).route {
            AuthRoute::VaultToken { .. } => kept.0.retain(|entry| {
                entry
                    .split_once('=')
                    .is_none_or(|(name, _)| !super::token_route::TOKEN_STATE_ENV.contains(&name))
            }),
            AuthRoute::Stored | AuthRoute::Outside => {}
        }
        kept.admitted()
    }

    /// A record read back from where it was kept (`KEY=value` words): only the entries
    /// [`Self::reads`] names, and `None` where [`Self::admitted`] refuses them, so a damaged or
    /// edited record never stands in for one.
    #[must_use]
    pub fn of_record(words: &[String]) -> Option<Self> {
        Self::kept(words).admitted()
    }

    /// The record where it runs on its account's own credential ([`ProcessLogin::of_env`]'s
    /// route; a token session's record, its descriptor and marker set aside, reads as a stored
    /// login's), and sets no credential, profile or route variable empty. An empty one may be what the agent's own
    /// command line set (`ANTHROPIC_API_KEY= claude`) over its shell's value, which a relaunch
    /// line, carrying no credential, would leave to the shell: such a record is no evidence of
    /// what the relaunch inherits.
    fn admitted(self) -> Option<Self> {
        let blanked = self
            .pairs()
            .any(|(name, value)| value.is_empty() && Self::routes(name));
        let admitted = match ProcessLogin::of_env(self.pairs()).route {
            // The token's wrapper sets the descriptor and the marker afresh, so a token session's
            // record holds the same directories and selectors a stored login's does.
            AuthRoute::Stored | AuthRoute::VaultToken { .. } => true,
            AuthRoute::Outside => false,
        };
        (!blanked && admitted).then_some(self)
    }

    /// Its `KEY=value` entries, in the process's order.
    #[must_use]
    pub fn entries(&self) -> &[String] {
        &self.0
    }

    /// Whether admission reads variable `name`: the login and home selectors, the directories
    /// the home check and [`profile_configs_dir`] read, and every variable
    /// [`ProcessLogin::of_env`] judges a login by.
    #[must_use]
    pub fn reads(name: &str) -> bool {
        [LOGIN_ENV, HOME_ENV, HOMEDIR_ENV, "XDG_CONFIG_HOME", "HOME"].contains(&name)
            || Self::routes(name)
    }

    /// Whether `name` is a credential, profile or route variable ([`names_another_login`]).
    fn routes(name: &str) -> bool {
        use super::netwatch::{ROUTE_KEYS, ROUTE_PREFIXES};
        CREDENTIAL_KEYS.contains(&name)
            || PROFILE_KEYS.contains(&name)
            || ROUTE_KEYS.contains(&name)
            || ROUTE_PREFIXES.iter().any(|p| name.starts_with(p))
    }

    fn kept(env: &[String]) -> Self {
        Self(
            env.iter()
                .filter(|entry| entry.split_once('=').is_some_and(|(k, _)| Self::reads(k)))
                .cloned()
                .collect(),
        )
    }

    fn pairs(&self) -> impl Iterator<Item = (&str, &str)> {
        self.0.iter().filter_map(|entry| entry.split_once('='))
    }
}

/// Names only, so no value reaches a log line.
impl std::fmt::Debug for LaunchEnv {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list()
            .entries(self.pairs().map(|(name, _)| name))
            .finish()
    }
}

fn login_file_dir<'a>(value: &'a str, file: &str) -> Option<&'a Path> {
    if value
        .chars()
        .any(|c| c.is_control() || c == '\u{2028}' || c == '\u{2029}')
    {
        return None;
    }
    let stem = value.strip_suffix(file)?;
    if !stem.ends_with('/') && !stem.ends_with(std::path::MAIN_SEPARATOR) {
        return None;
    }
    let path = Path::new(value);
    if !path.is_absolute() || path.file_name() != Some(std::ffi::OsStr::new(file)) {
        return None;
    }
    path.parent().filter(|dir| !dir.as_os_str().is_empty())
}

impl ProcessLogin {
    /// Extracts [`ProcessLogin`] facts from `(name, value)` environment pairs: its login is the
    /// first [`LOGIN_ENV`], its home the default without a non-empty [`HOME_ENV`], and its route:
    ///
    /// - [`AuthRoute::VaultToken`] where the token's descriptor variable
    ///   ([`super::token_route::DESCRIPTOR_ENV`]) and the wrapper's marker
    ///   ([`super::token_route::MARKER_ENV`], naming a token entry) are both set and no other
    ///   credential, profile or route variable is (`docs/DESIGN-claude-setup-token-2026-11-05.md`
    ///   §5.2). The shape alone, with no roster: a gate that resolves the account keeps it only
    ///   where the marker names that account's entry ([`AuthRoute::for_account`]);
    /// - [`AuthRoute::Outside`] where [`LOGIN_ENV`] is given twice differently, a credential,
    ///   profile or route variable is set non-empty (the lab stub's route its development seam
    ///   admits apart, [`super::netwatch::LAB_ROUTE_SEAM`]; never in a shipped binary), or the
    ///   descriptor or the marker is set without the other, or the marker names no token entry;
    /// - [`AuthRoute::Stored`] otherwise.
    #[must_use]
    pub fn of_env<'a>(env: impl IntoIterator<Item = (&'a str, &'a str)>) -> ProcessLogin {
        use super::token_route::{DESCRIPTOR_ENV, MARKER_ENV};
        let mut login: Option<&str> = None;
        let (mut descriptor, mut marker, mut marked_twice) = (false, None::<&str>, false);
        let mut out = ProcessLogin {
            login: None,
            default_home: true,
            route: AuthRoute::Stored,
        };
        for (name, value) in env {
            if name == LOGIN_ENV {
                match login {
                    None => login = Some(value),
                    Some(first) if first != value => out.route = AuthRoute::Outside,
                    Some(_) => {}
                }
            }
            if value.is_empty() {
                continue;
            }
            if name == HOME_ENV {
                out.default_home = false;
            }
            if name == DESCRIPTOR_ENV {
                descriptor = true;
            } else if name == MARKER_ENV {
                marked_twice |= marker.is_some_and(|first| first != value);
                marker.get_or_insert(value);
            } else if names_another_login(name, value) {
                out.route = AuthRoute::Outside;
            }
        }
        out.login = login.filter(|dir| !dir.is_empty()).map(str::to_owned);
        if out.route == AuthRoute::Outside {
            return out;
        }
        out.route = match (descriptor, marker.filter(|_| !marked_twice)) {
            (false, None) if !marked_twice => AuthRoute::Stored,
            (true, Some(marker)) => TokenEntry::parse(marker)
                .map_or(AuthRoute::Outside, |entry| AuthRoute::VaultToken { entry }),
            _ => AuthRoute::Outside,
        };
        out
    }

    /// Extracts a `cli` process's [`ProcessLogin`] facts from `(name, value)` environment pairs:
    /// Claude Code's are [`ProcessLogin::of_env`]. Another CLI's login is the directory its seam
    /// variable names; for a [`Seam::LoginFile`], the named file's directory, where the path is
    /// absolute, ends at the seam's name with no trailing separator or control character, and the
    /// file has the seam's name. A relative path, trailing separator, control character or another
    /// filename is retained as unbound: this reader has no process working directory to resolve it.
    /// Its home is the default unless a home variable of its own, other than the seam's, is set. Two different
    /// values of the seam variable, or a credential variable the CLI reads ahead of or in place
    /// of its stored login ([`crate::harness::relaunch::ahead_env`], the ones a pooled line
    /// removes), mean it runs on no stored login. Any non-empty value of such a variable judges
    /// so, even one the CLI ignores and runs on its stored login with (measured on Kimi Code
    /// 2.3.1: a `KIMI_MODEL_API_KEY` with no `KIMI_MODEL_NAME`, a name of only spaces, a
    /// `KIMI_CODE_CUSTOM_HEADERS` with no `Authorization` line), which holds a move rather than
    /// makes one. One read behind the stored login (grok's `XAI_API_KEY`) or that is no login of
    /// the CLI's (`OPENAI_API_KEY` for Codex) judges nothing, so the line that moves a session
    /// and the judge of where it runs agree.
    #[must_use]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "AccountLoginAttribution",
            action = "AnotherProvider",
            reason = "Select the Codex or Grok file-seam fixture."
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "AccountLoginAttribution",
            action = "AnotherCase",
            reason = "Select the native environment fixture before observing it."
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AccountLoginAttribution",
            action = "Observe",
            project = "aterm_agent::harness::accounts::tests::login_attribution::attribution_transition"
        )
    )]
    pub fn of_env_for<'a>(
        cli: Cli,
        env: impl IntoIterator<Item = (&'a str, &'a str)>,
    ) -> ProcessLogin {
        if cli == Cli::Claude {
            return ProcessLogin::of_env(env);
        }
        let connector = cli.connector();
        let seam = cli.accounts().seam;
        let home_var = connector.home.var.filter(|var| *var != seam.var());
        let ahead = super::relaunch::ahead_env(cli);
        let mut login: Option<&str> = None;
        let mut out = ProcessLogin {
            login: None,
            default_home: true,
            route: AuthRoute::Stored,
        };
        for (name, value) in env {
            if name == seam.var() {
                match login {
                    None => login = Some(value),
                    Some(first) if first != value => out.route = AuthRoute::Outside,
                    Some(_) => {}
                }
            }
            if value.is_empty() {
                continue;
            }
            if Some(name) == home_var {
                out.default_home = false;
            }
            if ahead.contains(&name) {
                out.route = AuthRoute::Outside;
            }
        }
        out.login = login.filter(|v| !v.is_empty()).map(|value| match seam {
            Seam::LoginFile { file, .. } => match login_file_dir(value, file) {
                Some(dir) => dir.to_string_lossy().into_owned(),
                None => {
                    out.route = AuthRoute::Outside;
                    value.to_owned()
                }
            },
            Seam::LoginDir { .. } | Seam::Overlay { .. } => value.to_owned(),
        });
        out
    }

    /// Extracts [`ProcessLogin`] facts from a kernel `KEY=value` exec environment slice.
    #[must_use]
    pub fn of_exec_env(env: &[String]) -> Option<ProcessLogin> {
        (!env.is_empty())
            .then(|| ProcessLogin::of_env(env.iter().filter_map(|kv| kv.split_once('='))))
    }

    /// Attaches settings-file credential overrides discovered for the process: an unreadable or
    /// unparseable document, an `apiKeyHelper`, or an `env` block naming [`LOGIN_ENV`] or setting
    /// [`HOME_ENV`] or a credential, profile or route variable non-empty (the lab stub's route
    /// its development seam admits apart, as in [`ProcessLogin::of_env`]) means no stored login.
    #[must_use]
    pub fn with_settings(mut self, settings: &[(String, super::netwatch::Source)]) -> ProcessLogin {
        use super::netwatch::{Source, settings_of};
        let another = |source: &Source| match source {
            Source::Missing => false,
            Source::Unreadable => true,
            Source::Text(text) => settings_of(text).is_none_or(|read| {
                read.api_key_helper
                    || read.env.iter().any(|(name, value)| {
                        name == LOGIN_ENV
                            || (!value.is_empty()
                                && (name == HOME_ENV || names_another_login(name, value)))
                    })
            }),
        };
        self.route = self
            .route
            .unless(settings.iter().any(|(_, source)| another(source)));
        self
    }

    /// Attaches whether the vendor profile store holds an active `.json` configuration.
    #[must_use]
    pub const fn with_profile_store(mut self, configured: bool) -> ProcessLogin {
        self.route = self.route.unless(configured);
        self
    }

    /// Resolves this process against `roster` into a [`Me`] descriptor for [`advise`].
    /// Its route is resolved against the row its login names ([`AuthRoute::for_account`]): a
    /// token route stays one only where its marker names that row's entry.
    #[must_use]
    pub fn me<'a>(&self, roster: &'a Roster) -> Me<'a> {
        let row = roster.row_for_login(self.login.as_deref());
        Me {
            account: row.map(|row| row.label.as_str()),
            left: roster
                .row_left(self.login.as_deref())
                .map(|row| row.label.as_str()),
            default_home: self.default_home,
            route: self.route.for_account(row.and_then(TokenEntry::of_row)),
        }
    }
}

/// Returns the roster account label under which `who` runs when no credential override is active.
#[must_use]
pub fn judged_label<'a>(roster: &'a Roster, who: &ProcessLogin) -> Option<&'a str> {
    let me = who.me(roster);
    me.label()
        .filter(|_| me.default_home && me.route.on_account())
}

/// Returns the account label shown in UI surfaces when `roster` lists multiple accounts.
#[must_use]
pub fn footer_label<'a>(roster: &'a Roster, who: &ProcessLogin) -> Option<&'a str> {
    judged_label(roster, who).filter(|_| roster.several())
}

/// The logins' file name, in the store beside `state.json` ([`State::to_json`]).
pub const LOGINS_FILE: &str = "logins.json";

/// [`LOGINS_FILE`]'s schema. Schemas 1 and 2 are read for migration; schema 3 prevents
/// older writers from moving or replacing bindings without advancing their generations.
pub const LOGINS_SCHEMA: u64 = 3;

/// The most bytes of [`LOGINS_FILE`] that are read.
pub const MAX_LOGINS_BYTES: u64 = 256 * 1024;

const DAY_S: i64 = 86_400;

/// A login's lifetime before anything was learned of it.
pub const LIFETIME_START_S: i64 = 21 * DAY_S;

/// The least a learned lifetime is taken as: a week ([`note_login_end`]).
pub const LIFETIME_MIN_S: i64 = 7 * DAY_S;

/// The most a learned lifetime is taken as: the client's own 30 days.
pub const LIFETIME_MAX_S: i64 = 30 * DAY_S;

/// A login whose forecast end is this near is due ([`LoginState::Due`]).
pub const DUE_S: i64 = 5 * DAY_S;

/// A login with a served turn on record this recently is `ok` ([`LoginState::Ok`]).
pub const OK_FRESH_S: i64 = 6 * 3600;

/// The most days the vendor's start-up notice `Your login expires in N days` names.
pub const NOTICE_DAYS_MAX: u32 = 3;

/// The most superseded logins a binding remembers ([`Binding::was`]).
pub const MAX_LEFT: usize = 4;

/// Within this long of now, a machine whose every renewable login is gone or forecast to end has
/// no working login soon ([`none_working_within`]).
pub const NONE_SOON_S: i64 = DAY_S;

const SUBSCRIPTION_MAX: usize = 32;

/// Private evidence left only after a fresh staging sign-in passed its check.
pub(super) const RENEWAL_CHECKPOINT_FILE: &str = ".aterm-renewal.json";
const RENEWAL_CHECKPOINT_STAGE_PREFIX: &str = ".renewal-check-";
const RENEWAL_CHECKPOINT_MAX: u64 = 4096;

/// The binding a completed sign-in is allowed to replace. An absent or legacy binding has
/// no unique generation, so any intervening renewal conservatively invalidates its proof.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(super) struct RenewalParent {
    generation: Option<u64>,
    clock: u64,
}

impl RenewalParent {
    /// Fence a pending native login to this exact row, parent and authentication facts.
    #[cfg(unix)]
    pub(super) fn recovery_scope(
        self,
        row: &Row,
        state: &State,
        logins: &Logins,
    ) -> Result<[u8; 32], String> {
        self.require_current(logins, &row.label)?;
        let row_text = row_text(row).ok_or("native recovery row is invalid")?;
        let mut selected = Logins::default();
        if let Some(binding) = logins.get(&row.label) {
            let mut binding = binding.clone();
            // Reporting an interrupted attempt must not revoke its retained completion.
            // These observations carry no account identity or parent authority.
            binding.sign_in_attempt = None;
            binding.sign_in_task = None;
            binding.served_at = None;
            // A first failed attempt may create a report-only default binding.
            // It carries the same identity facts as no binding at all.
            if binding != Binding::default() {
                selected.labels.insert(row.label.clone(), binding);
            }
        }
        // A versioned parent survives unrelated renewals; a legacy parent cannot.
        selected.generation_clock = self
            .generation
            .filter(|value| *value > 0)
            .unwrap_or(self.clock);
        let account = state.accounts.get(&row.label);
        let mut auth = aterm_json::json!({
            "auth": account.map_or(Auth::Unseen, |a| a.auth).as_str(),
            "auth_at": account.and_then(|a| a.auth_at),
            "was": row.was.clone(),
        });
        if !state.contextual_auth
            && account.is_some_and(|account| !account.managed_auth_is_current())
            && let Some(object) = auth.as_object_mut()
        {
            object.insert("auth_retired".into(), Value::Bool(true));
        }
        let auth = auth.to_string();
        let mut digest = aterm_digest::Sha256::new();
        digest.update(b"aterm.native-google-recovery.v1\0");
        for field in [row_text, selected.to_json(), auth] {
            digest.update((field.len() as u64).to_be_bytes());
            digest.update(field.as_bytes());
        }
        Ok(digest.finalize())
    }

    pub(super) fn capture(logins: &Logins, label: &str) -> Self {
        Self {
            generation: logins.get(label).map(|binding| binding.renewal_generation),
            clock: logins.generation_clock,
        }
    }

    pub(super) fn matches(self, logins: &Logins, label: &str) -> bool {
        let current = Self::capture(logins, label);
        self.generation == current.generation
            && if self.generation.is_some_and(|generation| generation > 0) {
                current.clock >= self.clock
            } else {
                current.clock == self.clock
            }
    }

    /// Reject a result whose captured parent is no longer current.
    /// # Errors
    /// The binding generation or the required legacy store counter changed.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AccountAttemptGeneration",
            action = "Commit",
            project = "aterm_agent::harness::cli::tests::attempt_generation::attempt_transition"
        )
    )]
    pub(super) fn require_current(self, logins: &Logins, label: &str) -> Result<(), String> {
        if self.matches(logins, label) {
            Ok(())
        } else {
            Err(format!(
                "account `{label}` login changed during sign-in; nothing recorded"
            ))
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RenewalCheckpoint {
    pub native_grant: Option<super::codex_grant_owner::GrantReference>,
    pub enrollment: Option<super::codex_enrollment::Request>,
    pub region: Option<Region>,
    pub label: String,
    pub email: String,
    pub dir: String,
    pub checked_at: i64,
    pub subscription: Option<String>,
    pub proved_by: Option<Proof>,
    pub google_principal: Option<[u8; 32]>,
    pub google_grant_digest: Option<[u8; 32]>,
    pub google_binding_scope: Option<[u8; 32]>,
    pub parent: RenewalParent,
}

impl RenewalCheckpoint {
    fn valid(&self) -> bool {
        admissible_label(&self.label)
            && email_refusal(&self.email).is_none()
            && dir_refusal(&self.dir).is_none()
            && self.checked_at > 0
            && self
                .parent
                .generation
                .is_none_or(|generation| generation <= self.parent.clock)
            && self.subscription.as_ref().is_none_or(|s| {
                !s.is_empty() && s.len() <= SUBSCRIPTION_MAX && one_line(s, SUBSCRIPTION_MAX) == *s
            })
    }
}

/// Saves the bounded, private result of a successful fresh sign-in check before binding it.
/// The caller holds the account lock and supplies its current, unedited bindings, so the
/// checkpoint identifies the parent of this renewal rather than its eventual replacement.
/// # Errors
/// The scope is invalid, staging cannot be canonicalized, the reserved name already exists, or
/// writing, publishing or flushing the private checkpoint fails.
pub(super) fn write_renewal_checkpoint(
    label: &str,
    email: &str,
    staging_text: &str,
    subscription: Option<&str>,
    checked_at: i64,
    logins: &Logins,
) -> io::Result<()> {
    write_renewal_checkpoint_for(
        Cli::Claude,
        None,
        label,
        email,
        staging_text,
        subscription,
        None,
        checked_at,
        logins,
    )
}

/// [`write_renewal_checkpoint`] for a renewal of `cli`'s pool: the checkpoint names the CLI's
/// program, so only that CLI's recovery reads it back ([`read_renewal_checkpoint_for`]).
/// Kimi also retains the region used by the captured sign-in row. Its schema 3 checkpoint
/// cannot be read by older clients that would compare only the email.
/// # Errors
/// As [`write_renewal_checkpoint`].
#[expect(
    clippy::too_many_arguments,
    reason = "checkpoint fields and the locked parent are explicit at each publication site"
)]
pub(super) fn write_renewal_checkpoint_for(
    cli: Cli,
    region: Option<Region>,
    label: &str,
    email: &str,
    staging_text: &str,
    subscription: Option<&str>,
    proved_by: Option<Proof>,
    checked_at: i64,
    logins: &Logins,
) -> io::Result<()> {
    write_checkpoint(
        cli,
        region,
        label,
        email,
        staging_text,
        subscription,
        proved_by,
        None,
        None,
        None,
        checked_at,
        logins,
        None,
    )
}

/// Codex enrollment completion remains bound to its durable operation during recovery.
pub(super) fn write_enrollment_checkpoint(
    request: &super::codex_enrollment::Request,
    email: &str,
    staging: &str,
    proof: Proof,
    now: i64,
    logins: &Logins,
) -> io::Result<()> {
    logins
        .enrollment
        .require_running(request)
        .map_err(io::Error::other)?;
    write_checkpoint(
        Cli::Codex,
        None,
        &request.label,
        email,
        staging,
        None,
        Some(proof),
        None,
        None,
        None,
        now,
        logins,
        Some(request),
    )
}

/// A new fenced response keeps the original issuer-bound parent even when
/// authenticated other-row publications advanced the global clock. Only the
/// held current-row proof can authorize this metadata writer; legacy callers
/// still capture and check the complete current parent.
pub(super) fn write_native_enrollment_checkpoint(
    current: &super::codex_native_row::CurrentNativeRow<'_>,
    email: &str,
    proof: Proof,
    checked_at: i64,
    logins: &Logins,
) -> io::Result<()> {
    let (admission, parent) = current
        .checkpoint_metadata(logins)
        .map_err(io::Error::other)?;
    write_checkpoint_with_parent(
        Cli::Codex,
        None,
        &admission.request.label,
        email,
        admission
            .directory
            .to_str()
            .ok_or_else(|| io::Error::other("native checkpoint path invalid"))?,
        None,
        Some(proof),
        None,
        None,
        None,
        checked_at,
        logins,
        Some(&admission.request),
        parent,
    )
}

/// A native Google checkpoint can only be made while its verified capability is current.
#[cfg(unix)]
pub(super) fn write_google_renewal_checkpoint(
    label: &str,
    staging: &str,
    grant: &super::agy_signin::VerifiedGoogleGrant,
    checked_at: i64,
    logins: &Logins,
) -> io::Result<()> {
    if grant.account_label() != label {
        return Err(io::Error::other(
            "Google proof describes another account label",
        ));
    }
    let path = grant.path().map_err(io::Error::other)?;
    if path != super::upgrade_agy::profile_grant(Path::new(staging)) {
        return Err(io::Error::other(
            "Google proof does not describe this staging home",
        ));
    }
    let profile = grant.profile();
    require_google_principal(logins, label, profile.principal).map_err(io::Error::other)?;
    write_checkpoint(
        Cli::Agy,
        None,
        label,
        &profile.email,
        staging,
        None,
        Some(Proof::Status),
        Some(profile.principal),
        Some(grant.digest().map_err(io::Error::other)?),
        Some(grant.checkpoint_scope().map_err(io::Error::other)?),
        checked_at,
        logins,
        None,
    )
}

#[expect(
    clippy::too_many_arguments,
    reason = "checkpoint fields and the locked parent are explicit at publication"
)]
fn write_checkpoint(
    cli: Cli,
    region: Option<Region>,
    label: &str,
    email: &str,
    staging_text: &str,
    subscription: Option<&str>,
    proved_by: Option<Proof>,
    google_principal: Option<[u8; 32]>,
    google_grant_digest: Option<[u8; 32]>,
    google_binding_scope: Option<[u8; 32]>,
    checked_at: i64,
    logins: &Logins,
    enrollment: Option<&super::codex_enrollment::Request>,
) -> io::Result<()> {
    write_checkpoint_with_parent(
        cli,
        region,
        label,
        email,
        staging_text,
        subscription,
        proved_by,
        google_principal,
        google_grant_digest,
        google_binding_scope,
        checked_at,
        logins,
        enrollment,
        RenewalParent::capture(logins, label),
    )
}

#[expect(
    clippy::too_many_arguments,
    reason = "existing explicit checkpoint fields plus its privately derived parent"
)]
fn write_checkpoint_with_parent(
    cli: Cli,
    region: Option<Region>,
    label: &str,
    email: &str,
    staging_text: &str,
    subscription: Option<&str>,
    proved_by: Option<Proof>,
    google_principal: Option<[u8; 32]>,
    google_grant_digest: Option<[u8; 32]>,
    google_binding_scope: Option<[u8; 32]>,
    checked_at: i64,
    logins: &Logins,
    enrollment: Option<&super::codex_enrollment::Request>,
    parent: RenewalParent,
) -> io::Result<()> {
    let staging = Path::new(staging_text);
    std::fs::canonicalize(staging)?;
    let native_grant = enrollment
        .map(|request| logins.enrollment.checkpoint_reference(request))
        .transpose()
        .map_err(io::Error::other)?
        .flatten();
    let checkpoint = RenewalCheckpoint {
        native_grant: native_grant.clone(),
        enrollment: enrollment.cloned(),
        region,
        label: label.to_owned(),
        email: email.to_owned(),
        dir: staging_text.to_owned(),
        checked_at,
        subscription: subscription.map(str::to_owned),
        proved_by,
        google_principal,
        google_grant_digest,
        google_binding_scope,
        parent,
    };
    if !checkpoint.valid()
        || (cli == Cli::Kimi) != region.is_some()
        || (cli == Cli::Agy) != google_principal.is_some()
        || (cli == Cli::Agy) != google_grant_digest.is_some()
        || (cli == Cli::Agy) != google_binding_scope.is_some()
        || (cli == Cli::Agy && proved_by != Some(Proof::Status))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid renewal checkpoint scope",
        ));
    }
    let mut doc = Map::new();
    doc.insert(
        "schema".into(),
        Value::from(if native_grant.is_some() {
            7
        } else if enrollment.is_some() {
            6
        } else if google_principal.is_some() {
            5
        } else if region.is_some() {
            3
        } else {
            2
        }),
    );
    if let Some(reference) = native_grant {
        doc.insert(
            "native_grant".into(),
            aterm_json::to_value(reference).map_err(io::Error::other)?,
        );
    }
    if let Some(request) = enrollment {
        doc.insert(
            "enrollment".into(),
            aterm_json::to_value(request).map_err(io::Error::other)?,
        );
    }
    doc.insert("program".into(), Value::from(cli.id()));
    if let Some(region) = region {
        doc.insert("region".into(), Value::from(region.as_str()));
    }
    doc.insert("label".into(), Value::from(checkpoint.label));
    doc.insert("email".into(), Value::from(checkpoint.email));
    doc.insert("dir".into(), Value::from(checkpoint.dir));
    doc.insert("checked_at".into(), Value::from(checkpoint.checked_at));
    doc.insert(
        "parent_generation".into(),
        Value::from(checkpoint.parent.generation),
    );
    doc.insert("parent_clock".into(), Value::from(checkpoint.parent.clock));
    doc.insert(
        "subscription".into(),
        checkpoint.subscription.map_or(Value::Null, Value::from),
    );
    if let Some(proof) = checkpoint.proved_by {
        doc.insert("proved_by".into(), Value::from(proof.as_str()));
    }
    if let Some(principal) = checkpoint.google_principal {
        doc.insert(
            "google_principal".into(),
            Value::from(aterm_codec::hex::encode_lower(&principal)),
        );
    }
    if let Some(digest) = checkpoint.google_grant_digest {
        doc.insert(
            "google_grant_digest".into(),
            Value::from(aterm_codec::hex::encode_lower(&digest)),
        );
    }
    if let Some(scope) = checkpoint.google_binding_scope {
        doc.insert(
            "google_binding_scope".into(),
            Value::from(aterm_codec::hex::encode_lower(&scope)),
        );
    }
    let text = Value::Object(doc).to_string();
    if text.len() as u64 > RENEWAL_CHECKPOINT_MAX {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "renewal checkpoint exceeds the byte limit",
        ));
    }
    let stage = aterm_tempfile::Builder::new()
        .prefix(RENEWAL_CHECKPOINT_STAGE_PREFIX)
        .tempdir_in(staging)?;
    let candidate = stage.path().join("result");
    super::upgrade_models::write_atomic_mode(&candidate, &text, Some(0o600))?;
    std::fs::hard_link(&candidate, staging.join(RENEWAL_CHECKPOINT_FILE))?;
    aterm_durable::durable_dir(staging)
}

/// Reads a bounded regular checkpoint without following a link or waiting on a FIFO.
#[cfg(test)]
#[must_use]
pub(super) fn read_renewal_checkpoint(
    staging: &Path,
    actual_dir: &Path,
    now: i64,
) -> Option<RenewalCheckpoint> {
    read_renewal_checkpoint_for(Cli::Claude, staging, actual_dir, now)
}

/// `read_renewal_checkpoint` of a checkpoint `cli`'s renewal wrote: one naming another CLI's
/// program is not this pool's.
#[must_use]
pub(super) fn read_renewal_checkpoint_for(
    cli: Cli,
    staging: &Path,
    actual_dir: &Path,
    now: i64,
) -> Option<RenewalCheckpoint> {
    read_renewal_checkpoint_checked_for(cli, staging, actual_dir, now)
        .ok()
        .flatten()
}

/// Read local completion evidence while distinguishing unavailable I/O from absent or invalid data.
/// # Errors
/// A checkpoint, its declared directory or its already-required native grant cannot be read.
pub(super) fn read_renewal_checkpoint_checked_for(
    cli: Cli,
    staging: &Path,
    actual_dir: &Path,
    now: i64,
) -> io::Result<Option<RenewalCheckpoint>> {
    read_renewal_checkpoint_with(
        cli,
        staging,
        actual_dir,
        now,
        aterm_uds::regular_file::open,
        |path| std::fs::canonicalize(path),
        native_google_digest_checked,
    )
}

fn read_renewal_checkpoint_with(
    cli: Cli,
    staging: &Path,
    actual_dir: &Path,
    now: i64,
    open: impl FnOnce(&Path) -> io::Result<(std::fs::File, std::fs::Metadata)>,
    canonicalize: impl FnOnce(&Path) -> io::Result<PathBuf>,
    grant_digest: impl FnOnce(&Path) -> io::Result<Option<[u8; 32]>>,
) -> io::Result<Option<RenewalCheckpoint>> {
    use std::io::Read as _;
    let (file, metadata) = match open(&staging.join(RENEWAL_CHECKPOINT_FILE)) {
        Ok(file) => file,
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::InvalidData
            ) =>
        {
            return Ok(None);
        }
        Err(error) => return Err(error),
    };
    if !metadata.is_file() || metadata.len() > RENEWAL_CHECKPOINT_MAX {
        return Ok(None);
    }
    let mut bytes = Vec::with_capacity(
        usize::try_from(metadata.len())
            .unwrap_or(0)
            .saturating_add(1),
    );
    file.take(RENEWAL_CHECKPOINT_MAX + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > RENEWAL_CHECKPOINT_MAX {
        return Ok(None);
    }
    let Some(checkpoint) = decode_renewal_checkpoint(cli, &bytes) else {
        return Ok(None);
    };
    if checkpoint.checked_at > now {
        return Ok(None);
    }
    if let Some(digest) = checkpoint.google_grant_digest
        && grant_digest(staging)? != Some(digest)
    {
        return Ok(None);
    }
    let declared = match canonicalize(Path::new(&checkpoint.dir)) {
        Ok(path) => path,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    Ok((declared == actual_dir).then_some(checkpoint))
}

fn decode_renewal_checkpoint(cli: Cli, bytes: &[u8]) -> Option<RenewalCheckpoint> {
    let doc: Value = aterm_json::from_str(std::str::from_utf8(bytes).ok()?).ok()?;
    // Schema 1 has no parent evidence; Kimi's schema 2 has no region evidence. Preserve
    // either for inspection without inventing the missing scope from current policy.
    // Google's schema 4 likewise lacks the binding revision and state-root scope.
    if doc.get("program")?.as_str()? != cli.id() {
        return None;
    }
    let region = match (doc.get("schema")?.as_u64()?, cli) {
        (2, cli) if !matches!(cli, Cli::Kimi | Cli::Agy) => None,
        (3, Cli::Kimi) => Some(Region::parse(doc.get("region")?.as_str()?)?),
        (5, Cli::Agy) => None,
        (6 | 7, Cli::Codex) => None,
        _ => return None,
    };
    let checkpoint = RenewalCheckpoint {
        native_grant: if doc.get("schema")?.as_u64()? == 7 {
            Some(aterm_json::from_value(doc.get("native_grant")?.clone()).ok()?)
        } else {
            if doc.get("native_grant").is_some() {
                return None;
            }
            None
        },
        enrollment: if matches!(doc.get("schema")?.as_u64()?, 6 | 7) {
            let request: super::codex_enrollment::Request =
                aterm_json::from_value(doc.get("enrollment")?.clone()).ok()?;
            if !request.valid() || request.label != doc.get("label")?.as_str()? {
                return None;
            }
            Some(request)
        } else {
            if doc.get("enrollment").is_some() {
                return None;
            }
            None
        },
        region,
        label: doc.get("label")?.as_str()?.to_owned(),
        email: doc.get("email")?.as_str()?.to_owned(),
        dir: doc.get("dir")?.as_str()?.to_owned(),
        checked_at: doc.get("checked_at")?.as_i64()?,
        subscription: match doc.get("subscription")? {
            Value::Null => None,
            value => Some(value.as_str()?.to_owned()),
        },
        proved_by: match doc.get("proved_by") {
            None | Some(Value::Null) => None,
            Some(value) => Some(Proof::parse(value.as_str()?)?),
        },
        google_principal: if cli == Cli::Agy {
            Some(google_principal(doc.get("google_principal")?)?)
        } else {
            None
        },
        google_grant_digest: if cli == Cli::Agy {
            Some(google_principal(doc.get("google_grant_digest")?)?)
        } else {
            None
        },
        google_binding_scope: if cli == Cli::Agy {
            Some(google_principal(doc.get("google_binding_scope")?)?)
        } else {
            None
        },
        parent: RenewalParent {
            generation: match doc.get("parent_generation")? {
                Value::Null => None,
                value => Some(value.as_u64()?),
            },
            clock: doc.get("parent_clock")?.as_u64()?,
        },
    };
    (checkpoint.valid() && (cli != Cli::Agy || checkpoint.proved_by == Some(Proof::Status)))
        .then_some(checkpoint)
}

/// Whether the captured parent and later authentication facts still permit recovery.
/// A temporary quota wall defers publication without invalidating this completion.
pub(super) fn checkpoint_current(
    row: &Row,
    state: &State,
    logins: &Logins,
    proof: &RenewalCheckpoint,
    google_state: Option<&Path>,
) -> bool {
    use auth_scope::Clock;

    let account = state.accounts.get(&row.label);
    proof.parent.matches(logins, &row.label)
        && proof
            .enrollment
            .as_ref()
            .is_none_or(|request| logins.enrollment.require_running(request).is_ok())
        && proof.google_binding_scope.is_none_or(|scope| {
            #[cfg(unix)]
            {
                google_state.is_some_and(|root| {
                    super::agy_signin::current_checkpoint_scope(root, &proof.label, &proof.email)
                        == Some(scope)
                })
            }
            #[cfg(not(unix))]
            {
                let _ = (scope, google_state);
                false
            }
        })
        && proof
            .google_principal
            .is_none_or(|principal| require_google_principal(logins, &row.label, principal).is_ok())
        && proof
            .google_grant_digest
            .is_none_or(|digest| native_google_digest(Path::new(&proof.dir)) == Some(digest))
        && !account.is_some_and(|a| {
            (state.contextual_auth || a.managed_auth_is_current())
                && a.auth_at.is_some_and(|at| at >= proof.checked_at)
        })
        && !logins.labels.get(&row.label).is_some_and(|binding| {
            let current = |clock| {
                state.contextual_auth || auth_scope::lifetime_current_for_row(row, logins, clock)
            };
            let later = |clock, at: Option<i64>| {
                current(clock) && at.is_some_and(|at| at >= proof.checked_at)
            };
            later(Clock::Renewal, binding.renewed_at)
                || later(Clock::Ended, binding.ended_at)
                || binding.wrong_at.is_some_and(|at| at >= proof.checked_at)
                || later(Clock::Served, binding.served_at)
                // An old checkpoint must not clear an undated login-end notice.
                || (current(Clock::Notice) && binding.notice_end.is_some())
        })
}

pub(super) fn checkpoint_identity_matches(cli: Cli, row: &Row, proof: &RenewalCheckpoint) -> bool {
    let expected = if matches!(cli, Cli::Claude | Cli::Codex | Cli::Kimi | Cli::Agy) {
        row.expected_identity_email()
    } else {
        row.email.as_deref()
    };
    row.vendor.region == proof.region
        && (cli != Cli::Codex || proof.proved_by == Some(Proof::Status))
        && (cli != Cli::Agy
            || (proof.proved_by == Some(Proof::Status)
                && proof.google_principal.is_some()
                && proof.google_grant_digest.is_some()
                && proof.google_binding_scope.is_some()))
        && expected.is_some_and(|email| email_matches(email, &proof.email))
}

pub(super) fn checkpoint_unreferenced(
    roster: &Roster,
    logins: &Logins,
    dir: &Path,
    actual: &Path,
) -> bool {
    !roster
        .rows
        .iter()
        .flat_map(|row| row.dir.iter().chain(row.was.iter().flatten()))
        .chain(
            logins
                .labels
                .values()
                .flat_map(|entry| entry.dir.iter().chain(entry.was.iter().flatten())),
        )
        .any(|held| {
            Path::new(held) == dir || std::fs::canonicalize(held).is_ok_and(|path| path == actual)
        })
}

/// Machine-local login binding and learned renewal lifetime for one roster account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding {
    /// The login directory a renewal bound the row to, verbatim.
    pub dir: Option<String>,
    /// When the bound login was renewed (its staging check answered).
    pub renewed_at: Option<i64>,
    /// Generation allocated by the local store when renewed or its binding identity changes.
    /// Unlike `renewed_at`, this distinguishes replacements even when the clock does not advance.
    /// It is not a provider identity, and zero also admits a legacy binding.
    pub renewal_generation: u64,
    /// Generation of the last committed renewal of this binding. Zero means no
    /// retained publication evidence. Other identity changes invalidate it; this
    /// is a local observation marker, not proof that credentials still work.
    pub last_success_generation: u64,
    /// The lifetime a renewal is forecast by, as learned ([`Binding::lifetime`],
    /// [`LIFETIME_MIN_S`], [`LIFETIME_MAX_S`]).
    pub lifetime_s: i64,
    /// The latest end the vendor's notice allows, for the bound login ([`note_login_notice`]).
    pub notice_end: Option<i64>,
    /// When the bound login was first seen ended ([`note_login_end`]).
    pub ended_at: Option<i64>,
    /// When a turn was last served under the bound login ([`note_served_login`]).
    pub served_at: Option<i64>,
    /// Applicability of retained managed renewal, notice, end and served observations.
    /// Codex uses its credential-context metadata authority instead.
    pub lifetime_scope: LifetimeScope,
    /// The `subscriptionType` its renewal's check answered (`max`, `pro`).
    pub subscription: Option<String>,
    /// When a renewal last answered another account than the row's ([`note_wrong_account`]).
    pub wrong_at: Option<i64>,
    /// At most [`MAX_LEFT`] retained login directories. Renewals put previous logins newest
    /// first; a roster departure keeps the last current login at the end for return recovery.
    pub was: Vec<Option<String>>,
    /// What proved the bound login is the row's account, where a sign-in of another CLI's pool
    /// bound it ([`note_proof`]); a Claude Code renewal's proof is its status check, never
    /// written.
    pub proved_by: Option<Proof>,
    /// The checked provider email, scoped to `proved_dir`, not a mutable row label.
    pub proved_email: Option<String>,
    /// The login directory whose identity was checked.
    pub proved_dir: Option<String>,
    /// Canonical Google subject digest checked for this exact email and login directory.
    pub google_principal: Option<[u8; 32]>,
    /// The remaining human step of a renewal, cleared by a successful sign-in.
    pub sign_in_task: Option<SignInTask>,
    /// How the row's last automatic sign-in ended and the routes it tried
    /// ([`note_sign_in_attempt`]); a failed one is cleared by a new login, a renewal's
    /// ([`note_renewed`]) or one signed back in where it stands ([`note_signed_back`]).
    pub sign_in_attempt: Option<SignInAttempt>,
}

/// How an automatic sign-in of a row ended ([`SignInAttempt::outcome`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AttemptOutcome {
    /// No route could sign the row in on this machine: every one tried is named.
    NoRoute,
    /// A route ran and did not bind a login.
    Failed,
    /// A route ran and its page stopped it (Cloudflare's bot check or block page,
    /// [`super::open_url::Met::stopped`]), which aterm never tries to pass: the row needs a
    /// person's own sign-in, in a browser that clears it.
    Blocked,
    /// A route signed the row in and its login was bound.
    SignedIn,
}

impl AttemptOutcome {
    /// The stored and shown word.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NoRoute => "no-route",
            Self::Failed => "failed",
            Self::Blocked => "blocked",
            Self::SignedIn => "signed-in",
        }
    }

    fn parse(word: &str) -> Option<Self> {
        [Self::NoRoute, Self::Failed, Self::Blocked, Self::SignedIn]
            .into_iter()
            .find(|outcome| outcome.as_str() == word)
    }

    /// Whether the row is still without a login it can be run on.
    #[must_use]
    pub const fn failing(self) -> bool {
        matches!(self, Self::NoRoute | Self::Failed | Self::Blocked)
    }
}

/// A row's last automatic sign-in: how it ended, the routes its ladder tried (one bounded line
/// that never holds a URL, code, password or email), and since when that has been so.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignInAttempt {
    /// How it ended.
    pub outcome: AttemptOutcome,
    /// The routes tried and what each found, at most [`ROUTES_MAX`] bytes.
    pub routes: String,
    /// When this outcome over these routes was first recorded (Unix seconds).
    pub since: i64,
    /// When a pass last met this page (Unix seconds), for [`AttemptOutcome::Blocked`], whose wait
    /// counts from it: an unchanged attempt keeps `since` and writes no ledger line. Any other
    /// outcome repeats with no write at all, so here this stays `since`.
    pub tried_at: i64,
}

/// The human step a vendor renewal could not finish, without its page or error text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SignInAction {
    /// Sign the row's browser profile in once, then retry its renewal.
    Browser,
    /// Approve the CLI's own sign-in on a terminal.
    Login,
}

impl SignInAction {
    /// The stored action word.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Browser => "browser",
            Self::Login => "login",
        }
    }

    fn parse(word: &str) -> Option<Self> {
        match word {
            "browser" => Some(Self::Browser),
            "login" => Some(Self::Login),
            _ => None,
        }
    }

    /// The next step for this action on `label` in `cli`'s pool: a browser profile signed in
    /// once is a step only where a renewal can then approve the page in it
    /// ([`Cli::renews_headlessly`]), so a task of one stored for a CLI whose renewals stop for a
    /// person names its sign-in on a terminal ([`Cli::sign_in_verb`]).
    #[must_use]
    pub fn next_step(self, cli: Cli, label: &str) -> String {
        use super::accounts_step::Step;
        match self {
            Self::Browser if cli.renews_headlessly() => format!(
                "Run {}, then {}",
                Step::Browser(cli, label.to_owned()),
                Step::Renew(cli, label.to_owned())
            ),
            Self::Browser | Self::Login => {
                format!("Run {}", Step::Login(cli, label.to_owned()))
            }
        }
    }
}

/// A durable renewal task. Its timestamp is evidence, never a login identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SignInTask {
    /// The remaining step.
    pub action: SignInAction,
    /// When the renewal stopped for a person, in Unix seconds.
    pub at: i64,
}

/// What proved a sign-in made the row's account ([`Binding::proved_by`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Proof {
    /// The native CLI or its canonical account endpoint named the row's email
    /// (Codex's `account/read`, Grok's `Signed in as`, or Kimi's verified `/me`).
    Status,
    /// The sign-in page showed the row's email.
    Page,
    /// Nothing named the account: the person approved the page.
    Unchecked,
}

impl Proof {
    /// Its `logins.json` word.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Proof::Status => "status",
            Proof::Page => "page",
            Proof::Unchecked => "unchecked",
        }
    }

    /// The proof its `logins.json` `word` names.
    #[must_use]
    pub fn parse(word: &str) -> Option<Proof> {
        [Proof::Status, Proof::Page, Proof::Unchecked]
            .into_iter()
            .find(|p| p.as_str() == word)
    }
}

impl Default for Binding {
    fn default() -> Binding {
        Binding {
            dir: None,
            renewed_at: None,
            renewal_generation: 0,
            last_success_generation: 0,
            lifetime_s: LIFETIME_START_S,
            notice_end: None,
            ended_at: None,
            served_at: None,
            lifetime_scope: LifetimeScope::default(),
            subscription: None,
            wrong_at: None,
            was: Vec::new(),
            proved_by: None,
            proved_email: None,
            proved_dir: None,
            google_principal: None,
            sign_in_task: None,
            sign_in_attempt: None,
        }
    }
}

impl Binding {
    /// A recorded CLI identity check still matching this effective row from
    /// [`bound`] and this binding's current login directory.
    /// This local historical evidence grants no provider budget authority.
    #[must_use]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AccountIdentity",
            action = "ChangedIdentity",
            project = "aterm_agent::harness::cli::tests::identity::identity_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AccountIdentity",
            action = "ChangedDirectory",
            project = "aterm_agent::harness::cli::tests::identity::identity_transition"
        )
    )]
    pub fn identity_proof_for(&self, row: &Row) -> Option<Proof> {
        let proof = self.proved_by.filter(|proof| *proof == Proof::Status)?;
        (self.proved_dir.as_deref() == self.dir.as_deref()
            && self.proved_dir.as_deref() == row.dir.as_deref()
            && self.proved_dir.is_some()
            && self
                .proved_email
                .as_deref()
                .zip(row.expected_identity_email())
                .is_some_and(|(seen, expected)| email_matches(expected, seen)))
        .then_some(proof)
    }

    /// Ends a failed automatic sign-in's record once the row has a login again: what it tried is
    /// no news of the new login, whose own end, due date or failed renewal is told afresh.
    fn clear_failed_sign_in(&mut self) {
        if self
            .sign_in_attempt
            .as_ref()
            .is_some_and(|attempt| attempt.outcome.failing())
        {
            self.sign_in_attempt = None;
        }
    }

    /// Clamped login lifetime in seconds used to forecast expiration.
    #[must_use]
    pub fn lifetime(&self) -> i64 {
        self.lifetime_s.clamp(LIFETIME_MIN_S, LIFETIME_MAX_S)
    }

    fn lower_lifetime(&mut self, end: i64) {
        if let Some(renewed) = self.renewed_at
            && end > renewed
        {
            let implied = end.saturating_sub(renewed);
            self.lifetime_s = self
                .lifetime_s
                .min(implied)
                .clamp(LIFETIME_MIN_S, LIFETIME_MAX_S);
        }
    }
}

/// Transaction-local permission to forget one terminal native receipt. The
/// receipt writer constructs this only after validating durable terminal state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeRotationRetirement {
    id: String,
    binding: String,
}

impl NativeRotationRetirement {
    pub(super) fn new(id: &str, binding: &str) -> Self {
        Self {
            id: id.into(),
            binding: binding.into(),
        }
    }

    fn matches(&self, id: &str, binding: &str) -> bool {
        self.id == id && self.binding == binding
    }
}

/// Machine-local `logins.json` map from account label to [`Binding`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Logins {
    /// Durable native enrollment operation custody; never an alternate credential store.
    pub enrollment: super::codex_enrollment::Operations,
    /// Last allocated renewal generation. Retained even when labels move or disappear;
    /// only the store transaction allocates from this machine-local counter.
    pub generation_clock: u64,
    /// Last allocated workflow identity, independent of credential and success generations.
    pub workflow_clock: u64,
    /// Workflow custody, including tombstones that prevent label reuse reviving old evidence.
    pub workflows: BTreeMap<String, WorkflowCustody>,
    /// Last allocated in-place sign-in receipt, independent of credential fences.
    pub sign_in_clock: u64,
    /// Successful in-place sign-ins by label, including unbound default logins.
    /// The transaction owns these receipts; identity edits invalidate them.
    pub sign_in_receipts: BTreeMap<String, u64>,
    /// Transaction-local receipt revocations, consumed before serialization.
    /// Forgetting and reusing a label in one transaction must still revoke it.
    #[doc(hidden)]
    pub pending_sign_in_invalidations: BTreeSet<String>,
    /// The bindings, by label.
    pub labels: BTreeMap<String, Binding>,
    /// Retained shared authority and consumed preparation generations. Ordinary
    /// metadata edits cannot remove these pins or lower their high-water marks.
    pub shared: shared::State,
    /// Native handoffs already charged to the pool, keyed by their private
    /// receipt. The digest binds the receipt to its source and destination.
    pub native_rotations: BTreeMap<String, String>,
    /// Consumed by the transaction guard, never serialized.
    #[doc(hidden)]
    pub pending_native_retirement: Option<NativeRotationRetirement>,
    /// Captured entry contexts for observations created during an admitted publication.
    #[doc(hidden)]
    pub auth_contexts: ManagedContexts,
    /// Captured workflow contexts for all CLI providers; never serialized.
    #[doc(hidden)]
    pub workflow_contexts: WorkflowContexts,
}

impl Logins {
    /// Looks up the [`Binding`] for `label`.
    #[must_use]
    pub fn get(&self, label: &str) -> Option<&Binding> {
        self.labels.get(label)
    }

    fn entry(&mut self, label: &str) -> Option<&mut Binding> {
        if !admissible_label(label) {
            return None;
        }
        Some(self.labels.entry(label.to_owned()).or_default())
    }

    /// Serializes `logins.json` as a single-line sorted JSON object.
    #[must_use]
    pub fn to_json(&self) -> String {
        let mut o = Map::new();
        o.insert(
            "schema".to_owned(),
            Value::from(if self.enrollment.has_native_rows() {
                10
            } else if self.enrollment.has_owner_witness() || self.enrollment.has_artifacts() {
                9
            } else if self.enrollment.requires_extended() {
                8
            } else if !self.enrollment.is_empty() {
                7
            } else if self
                .labels
                .values()
                .any(|binding| binding.google_principal.is_some())
            {
                6
            } else if !self.native_rotations.is_empty() {
                5
            } else if self.shared.is_empty() {
                LOGINS_SCHEMA
            } else {
                4
            }),
        );
        if !self.enrollment.is_empty() {
            o.insert("enrollment".into(), self.enrollment.encode());
        }
        if !self.shared.is_empty() {
            o.insert("shared".into(), self.shared.to_json());
        }
        if !self.native_rotations.is_empty() {
            o.insert(
                "native_rotations".into(),
                Value::Object(
                    self.native_rotations
                        .iter()
                        .map(|(id, binding)| (id.clone(), Value::from(binding.clone())))
                        .collect(),
                ),
            );
        }
        o.insert(
            "generation_clock".to_owned(),
            Value::from(self.generation_clock),
        );
        if self.sign_in_clock != 0 {
            o.insert("sign_in_clock".into(), Value::from(self.sign_in_clock));
            o.insert(
                "sign_in_receipts".into(),
                Value::Object(
                    self.sign_in_receipts
                        .iter()
                        .map(|(label, receipt)| (label.clone(), Value::from(*receipt)))
                        .collect(),
                ),
            );
        }
        workflow_scope::write_json(self, &mut o);
        let labels = self
            .labels
            .iter()
            .map(|(label, b)| {
                let mut m = Map::new();
                m.insert("dir".to_owned(), Value::from(b.dir.clone()));
                m.insert("renewed_at".to_owned(), Value::from(b.renewed_at));
                m.insert(
                    "renewal_generation".to_owned(),
                    Value::from(b.renewal_generation),
                );
                if b.last_success_generation != 0 {
                    m.insert(
                        "last_success_generation".to_owned(),
                        Value::from(b.last_success_generation),
                    );
                }
                m.insert("lifetime_s".to_owned(), Value::from(b.lifetime_s));
                m.insert("notice_end".to_owned(), Value::from(b.notice_end));
                m.insert("ended_at".to_owned(), Value::from(b.ended_at));
                m.insert("served_at".to_owned(), Value::from(b.served_at));
                b.lifetime_scope.write_json(&mut m);
                m.insert(
                    "subscription".to_owned(),
                    Value::from(b.subscription.clone()),
                );
                m.insert("wrong_at".to_owned(), Value::from(b.wrong_at));
                m.insert(
                    "was".to_owned(),
                    Value::Array(b.was.iter().map(|w| Value::from(w.clone())).collect()),
                );
                if let Some(proof) = b.proved_by {
                    m.insert("proved_by".to_owned(), Value::from(proof.as_str()));
                }
                if let Some(email) = &b.proved_email {
                    m.insert("proved_email".into(), Value::from(email.clone()));
                }
                if let Some(dir) = &b.proved_dir {
                    m.insert("proved_dir".into(), Value::from(dir.clone()));
                }
                if let Some(principal) = b.google_principal {
                    m.insert(
                        "google_principal".into(),
                        Value::from(aterm_codec::hex::encode_lower(&principal)),
                    );
                }
                if let Some(task) = b.sign_in_task {
                    let mut pending = Map::new();
                    pending.insert("action".into(), Value::from(task.action.as_str()));
                    pending.insert("at".into(), Value::from(task.at));
                    m.insert("sign_in_task".into(), Value::Object(pending));
                }
                if let Some(attempt) = &b.sign_in_attempt {
                    let mut tried = Map::new();
                    tried.insert("outcome".into(), Value::from(attempt.outcome.as_str()));
                    tried.insert("routes".into(), Value::from(attempt.routes.clone()));
                    tried.insert("since".into(), Value::from(attempt.since));
                    tried.insert("tried_at".into(), Value::from(attempt.tried_at));
                    m.insert("sign_in_attempt".into(), Value::Object(tried));
                }
                (label.clone(), Value::Object(m))
            })
            .collect();
        o.insert("labels".to_owned(), Value::Object(labels));
        aterm_json::to_string(&Value::Object(o)).unwrap_or_else(|_| String::from("{}"))
    }

    /// Deserializes and validates `logins.json` against `now`.
    /// # Errors
    ///
    /// As [`State::from_json`]: [`Unread::Torn`] for text that is not JSON,
    /// [`Unread::NotOurs`] for another schema or invalid identity structure.
    pub fn from_json(text: &str, now: i64) -> Result<Logins, Unread> {
        let doc: Value = aterm_json::from_str(text)
            .map_err(|e| Unread::Torn(format!("not JSON ({})", shown(&e.to_string()))))?;
        if !doc.is_object() {
            return Err(Unread::NotOurs("not a JSON object".to_string()));
        }
        let schema = doc.get("schema").and_then(Value::as_u64);
        let legacy = match schema {
            Some(4..=10) => false,
            Some(2 | LOGINS_SCHEMA) => false,
            Some(1) => true,
            Some(other) => {
                return Err(Unread::NotOurs(format!(
                    "schema {other}; this build reads schemas 1, 2, {LOGINS_SCHEMA}, 4, 5, 6, 7, 8, 9 and 10"
                )));
            }
            None => return Err(Unread::NotOurs("names no schema".to_string())),
        };
        let shared = if schema == Some(4)
            || (matches!(schema, Some(5..=10)) && doc.get("shared").is_some())
        {
            shared::State::from_json(&doc["shared"]).map_err(Unread::NotOurs)?
        } else {
            if doc.get("shared").is_some() {
                return Err(Unread::NotOurs(
                    "shared pins require their guarded schema".into(),
                ));
            }
            shared::State::default()
        };
        let mut native_rotations = BTreeMap::new();
        if schema == Some(5)
            || (matches!(schema, Some(6..=10)) && doc.get("native_rotations").is_some())
        {
            let entries = doc
                .get("native_rotations")
                .and_then(Value::as_object)
                .filter(|entries| !entries.is_empty())
                .ok_or_else(|| Unread::NotOurs("missing native rotation receipts".into()))?;
            for (id, value) in entries {
                let binding = value
                    .as_str()
                    .filter(|binding| native_rotation_key(id, binding))
                    .ok_or_else(|| Unread::NotOurs("invalid native rotation receipt".into()))?;
                native_rotations.insert(id.clone(), binding.to_owned());
            }
        } else if doc.get("native_rotations").is_some() {
            return Err(Unread::NotOurs(
                "native rotation receipts require their guarded schema".into(),
            ));
        }
        let entries = doc
            .get("labels")
            .and_then(Value::as_object)
            .ok_or_else(|| Unread::NotOurs("labels is not an object".to_string()))?;
        let mut labels = BTreeMap::new();
        for (label, value) in entries {
            if !admissible_label(label) || !value.is_object() {
                return Err(Unread::NotOurs(
                    "invalid binding label or object".to_string(),
                ));
            }
            let dir_of = |value: &Value| match value {
                Value::Null => Ok(None),
                Value::String(dir) if dir_refusal(dir).is_none() => Ok(Some(dir.clone())),
                _ => Err(Unread::NotOurs(format!(
                    "binding `{label}` has an invalid login directory"
                ))),
            };
            let dir = dir_of(value.get("dir").ok_or_else(|| {
                Unread::NotOurs(format!("binding `{label}` names no login directory"))
            })?)?;
            let was = match value.get("was") {
                Some(Value::Array(was)) if was.len() <= MAX_LEFT => {
                    was.iter().map(dir_of).collect::<Result<Vec<_>, _>>()?
                }
                _ => {
                    return Err(Unread::NotOurs(format!(
                        "binding `{label}` has invalid superseded logins"
                    )));
                }
            };
            let v = value;
            let sign_in_task = match v.get("sign_in_task") {
                None | Some(Value::Null) => None,
                Some(task) => {
                    let action = task
                        .get("action")
                        .and_then(Value::as_str)
                        .and_then(SignInAction::parse)
                        .ok_or_else(|| {
                            Unread::NotOurs(format!(
                                "binding `{label}` has an unknown sign-in task"
                            ))
                        })?;
                    let at = stamp(task.get("at"), now).ok_or_else(|| {
                        Unread::NotOurs(format!("binding `{label}` has no sign-in task timestamp"))
                    })?;
                    Some(SignInTask { action, at })
                }
            };
            // An attempt this build cannot read is dropped, not refused: it is a record of the
            // ladder's last pass, which the next pass writes again.
            let sign_in_attempt = v.get("sign_in_attempt").and_then(|tried| {
                let since = stamp(tried.get("since"), now)?;
                Some(SignInAttempt {
                    outcome: tried
                        .get("outcome")
                        .and_then(Value::as_str)
                        .and_then(AttemptOutcome::parse)?,
                    routes: one_line(tried.get("routes")?.as_str()?, ROUTES_MAX),
                    since,
                    tried_at: stamp(tried.get("tried_at"), now).unwrap_or(since),
                })
            });
            let binding = Binding {
                dir,
                renewed_at: stamp(v.get("renewed_at"), now),
                renewal_generation: match v.get("renewal_generation") {
                    None if legacy => 0,
                    value => value.and_then(Value::as_u64).ok_or_else(|| {
                        Unread::NotOurs(format!(
                            "binding `{label}` has an invalid renewal generation"
                        ))
                    })?,
                },
                last_success_generation: match v.get("last_success_generation") {
                    None => 0,
                    Some(value) => value.as_u64().ok_or_else(|| {
                        Unread::NotOurs(format!(
                            "binding `{label}` has an invalid successful publication generation"
                        ))
                    })?,
                },
                lifetime_s: int(v.get("lifetime_s"))
                    .unwrap_or(LIFETIME_START_S)
                    .clamp(LIFETIME_MIN_S, LIFETIME_MAX_S),
                notice_end: int(v.get("notice_end")),
                ended_at: stamp(v.get("ended_at"), now),
                served_at: stamp(v.get("served_at"), now),
                lifetime_scope: LifetimeScope::from_json(v),
                subscription: word(v.get("subscription"), SUBSCRIPTION_MAX),
                wrong_at: stamp(v.get("wrong_at"), now),
                was,
                proved_by: v
                    .get("proved_by")
                    .and_then(Value::as_str)
                    .and_then(Proof::parse),
                proved_email: v
                    .get("proved_email")
                    .and_then(Value::as_str)
                    .filter(|email| email_refusal(email).is_none())
                    .map(str::to_owned),
                proved_dir: v
                    .get("proved_dir")
                    .and_then(Value::as_str)
                    .filter(|dir| dir_refusal(dir).is_none())
                    .map(str::to_owned),
                google_principal: match v.get("google_principal") {
                    None | Some(Value::Null) => None,
                    Some(value) => Some(google_principal(value).ok_or_else(|| {
                        Unread::NotOurs("invalid Google principal digest".into())
                    })?),
                },
                sign_in_task,
                sign_in_attempt,
            };
            if binding.last_success_generation > binding.renewal_generation {
                return Err(Unread::NotOurs(format!(
                    "binding `{label}` successful publication exceeds its generation"
                )));
            }
            if binding.google_principal.is_some()
                && (!matches!(schema, Some(6..=10))
                    || binding.proved_by != Some(Proof::Status)
                    || binding.proved_email.is_none()
                    || binding.proved_dir.is_none()
                    || binding.proved_dir != binding.dir)
            {
                return Err(Unread::NotOurs(
                    "Google principal has no guarded scoped identity".into(),
                ));
            }
            labels.insert(label.clone(), binding);
        }
        let highest = labels
            .values()
            .map(|binding| binding.renewal_generation)
            .max()
            .unwrap_or(0);
        let generation_clock = match doc.get("generation_clock") {
            None if legacy => highest,
            value => value
                .and_then(Value::as_u64)
                .filter(|clock| *clock >= highest)
                .ok_or_else(|| Unread::NotOurs("invalid renewal generation clock".into()))?,
        };
        if !shared.fits_clock(generation_clock) {
            return Err(Unread::NotOurs(
                "shared preparation exceeds its retained generation clock".into(),
            ));
        }
        let sign_in_clock = match doc.get("sign_in_clock") {
            None => 0,
            Some(value) => value
                .as_u64()
                .ok_or_else(|| Unread::NotOurs("invalid sign-in receipt clock".into()))?,
        };
        let mut sign_in_receipts = BTreeMap::new();
        if let Some(value) = doc.get("sign_in_receipts") {
            let entries = value
                .as_object()
                .ok_or_else(|| Unread::NotOurs("invalid sign-in receipts".into()))?;
            for (label, value) in entries {
                let receipt = value
                    .as_u64()
                    .filter(|receipt| *receipt != 0 && *receipt <= sign_in_clock)
                    .filter(|_| admissible_label(label))
                    .ok_or_else(|| Unread::NotOurs("invalid sign-in receipt".into()))?;
                sign_in_receipts.insert(label.clone(), receipt);
            }
        }
        let enrollment = if matches!(schema, Some(7..=10)) {
            let book = super::codex_enrollment::Operations::decode(&doc["enrollment"])
                .map_err(Unread::NotOurs)?;
            if schema != Some(10) && book.has_native_rows() {
                return Err(Unread::NotOurs(
                    "native row admission requires schema 10".into(),
                ));
            }

            if !matches!(schema, Some(9 | 10)) && book.has_owner_witness() {
                return Err(Unread::NotOurs(
                    "owner freshness witnesses require schema 9".into(),
                ));
            }
            if schema == Some(7) && book.requires_extended() {
                return Err(Unread::NotOurs(
                    "native enrollment custody requires schema 8".into(),
                ));
            }
            if !matches!(schema, Some(9 | 10)) && book.has_artifacts() {
                return Err(Unread::NotOurs(
                    "native artifact custody requires schema 9".into(),
                ));
            }
            book
        } else {
            if doc.get("enrollment").is_some() {
                return Err(Unread::NotOurs(
                    "enrollment custody requires schema 7 or 8".into(),
                ));
            }
            super::codex_enrollment::Operations::default()
        };
        let (workflow_clock, workflows) = workflow_scope::from_json(&doc)?;
        Ok(Logins {
            enrollment,
            labels,
            generation_clock,
            workflow_clock,
            workflows,
            sign_in_clock,
            sign_in_receipts,
            pending_sign_in_invalidations: BTreeSet::new(),
            shared,
            native_rotations,
            pending_native_retirement: None,
            auth_contexts: ManagedContexts::default(),
            workflow_contexts: WorkflowContexts::default(),
        })
    }
}

/// Private handoff identifiers and their binding digests use canonical hex.
pub(super) fn native_rotation_key(id: &str, binding: &str) -> bool {
    let hex = |value: &str, len| {
        value.len() == len
            && value
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    };
    hex(id, 32) && hex(binding, 64)
}

/// Allocate at most one new generation per affected label in this publication. Renewals,
/// insertion, removal and changes to location or proved identity invalidate old parents.
/// Removal consumes a generation even though no binding remains to hold it. Multiple edits
/// expose only the final binding. Exhaustion refuses before either account file is saved.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountSharedRenewal",
        action = "MetadataEdit",
        project = "aterm_agent::harness::cli::tests::shared_renewal_tests::transition"
    )
)]
fn advance_renewal_generations(
    before: &Logins,
    after: &mut Logins,
    events: &[Event],
) -> Result<(), String> {
    let retirement = after.pending_native_retirement.take();
    if before.native_rotations.iter().any(|(id, binding)| {
        after.native_rotations.get(id) != Some(binding)
            && !(!after.native_rotations.contains_key(id)
                && retirement
                    .as_ref()
                    .is_some_and(|permit| permit.matches(id, binding)))
    }) || after
        .native_rotations
        .iter()
        .any(|(id, binding)| !native_rotation_key(id, binding))
    {
        return Err("account edit would erase or alter a native rotation receipt".into());
    }
    if !after.shared.extends(&before.shared) {
        return Err(
            "account edit would erase shared custody or consumed preparation history".into(),
        );
    }
    let renewed: BTreeSet<_> = events
        .iter()
        .filter(|event| event.kind == Kind::Renewed)
        .map(|event| event.account.clone())
        .collect();
    // Publication evidence is derived here, never supplied by an ordinary edit.
    for (label, binding) in &mut after.labels {
        binding.last_success_generation = before
            .labels
            .get(label)
            .map_or(0, |old| old.last_success_generation);
    }
    let mut changed = renewed.clone();
    for label in &changed {
        if !after
            .labels
            .get(label)
            .is_some_and(|binding| binding.dir.is_some())
        {
            return Err(format!(
                "account `{label}` renewal has no binding; account records unchanged"
            ));
        }
    }
    for (label, current_target) in after.shared.new_attempt_labels(&before.shared) {
        if !after.labels.contains_key(&label) {
            // A missing historical binding already invalidates its checkpoints;
            // do not recreate removed labels merely to advance their fences.
            if !current_target {
                continue;
            }
            after.labels.insert(label.clone(), Binding::default());
        }
        changed.insert(label);
    }
    changed.extend(
        before
            .labels
            .keys()
            .chain(after.labels.keys())
            .filter(
                |label| match (before.labels.get(*label), after.labels.get(*label)) {
                    (Some(a), Some(b)) => {
                        a.dir != b.dir
                            || a.proved_by != b.proved_by
                            || a.proved_email != b.proved_email
                            || a.proved_dir != b.proved_dir
                            || a.google_principal != b.google_principal
                            || a.renewal_generation != b.renewal_generation
                    }
                    (None, None) => false,
                    _ => true,
                },
            )
            .cloned(),
    );
    // Forgetting or relabelling an account revokes in-flight observations even
    // when no binding exists, or its directory and identity stayed unchanged.
    changed.extend(after.pending_sign_in_invalidations.iter().cloned());
    let mut clock = before.generation_clock;
    after.sign_in_receipts = before.sign_in_receipts.clone();
    for label in changed {
        // Usage can first create an empty binding for an existing default
        // login. That adds a credential fence but does not replace the login
        // whose successful sign-back the receipt describes.
        let default_metadata = !before.labels.contains_key(&label)
            && after.labels.get(&label).is_some_and(|binding| {
                binding.dir.is_none()
                    && binding.proved_by.is_none()
                    && binding.proved_email.is_none()
                    && binding.proved_dir.is_none()
                    && binding.google_principal.is_none()
                    && binding.renewal_generation == 0
            });
        let next = clock.checked_add(1).ok_or_else(|| {
            format!("account `{label}` renewal generation is exhausted; account records unchanged")
        })?;
        if let Some(binding) = after.labels.get_mut(&label) {
            binding.renewal_generation = next;
            binding.last_success_generation = if renewed.contains(&label) { next } else { 0 };
        }
        if !default_metadata {
            after.sign_in_receipts.remove(&label);
        }
        clock = next;
    }
    after
        .enrollment
        .commit(&before.enrollment, &renewed, &after.labels)?;
    after.generation_clock = clock;
    after.sign_in_clock = before.sign_in_clock;
    after
        .shared
        .fence_new_attempts(&before.shared, &after.labels)?;
    if !after.shared.fits_clock(clock)
        || !after.shared.published_into(&before.shared, &after.labels)
    {
        return Err("shared preparation does not match the committed binding generation".into());
    }
    Ok(())
}

/// Derive in-place receipts after credential fences have been normalized. Prune
/// explicitly removed account records before recording new successes.
fn advance_sign_in_receipts(
    before: &Logins,
    after: &mut Logins,
    state: &State,
    events: &[Event],
) -> Result<(), String> {
    let invalidated = std::mem::take(&mut after.pending_sign_in_invalidations);
    for label in &invalidated {
        if let Some(binding) = after.labels.get_mut(label) {
            binding.last_success_generation = 0;
        }
    }
    after.sign_in_receipts.retain(|label, _| {
        state
            .accounts
            .get(label)
            .is_some_and(|account| state.contextual_auth || account.managed_auth_is_current())
            && !invalidated.contains(label)
    });
    let signed_in: BTreeSet<_> = events
        .iter()
        .filter(|event| event.kind == Kind::SignedIn)
        .filter(|event| {
            state.accounts.get(&event.account).is_some_and(|account| {
                state.contextual_auth
                    || (account.managed_auth_is_current() && account.managed_auth() == Auth::Ok)
            })
        })
        .filter(|event| !invalidated.contains(&event.account))
        // A result for the old login cannot certify a replacement made by
        // another edit in this transaction. A first empty metadata binding
        // still describes the same unbound default login.
        .filter(
            |event| match (before.get(&event.account), after.get(&event.account)) {
                (None, None) => true,
                (None, Some(binding)) => {
                    binding.dir.is_none()
                        && binding.proved_by.is_none()
                        && binding.proved_email.is_none()
                        && binding.proved_dir.is_none()
                        && binding.google_principal.is_none()
                }
                (Some(old), Some(new)) => old.renewal_generation == new.renewal_generation,
                (Some(_), None) => false,
            },
        )
        .map(|event| event.account.clone())
        .collect();
    for label in signed_in {
        if !admissible_label(&label) {
            return Err("sign-in receipt has an invalid account label".into());
        }
        after.sign_in_clock = after
            .sign_in_clock
            .checked_add(1)
            .ok_or("sign-in receipt clock is exhausted; account records unchanged")?;
        after.sign_in_receipts.insert(label, after.sign_in_clock);
    }
    Ok(())
}

/// Applies machine-local renewal bindings in `logins` to `roster`.
#[must_use]
pub fn bound(roster: &Roster, logins: &Logins) -> Roster {
    let rows = roster
        .rows
        .iter()
        .map(|row| match logins.get(&row.label) {
            Some(b) if b.dir.is_some() => {
                let mut was = b.was.clone();
                if row.dir.is_none() && !was.contains(&None) {
                    was.push(None);
                }
                Row {
                    dir: b.dir.clone(),
                    was,
                    ..row.clone()
                }
            }
            // A row on the login its roster row names, whose binding keeps what it left (the
            // default login's row after a reassignment): a session still there is this row's.
            Some(b) if !b.was.is_empty() => Row {
                was: b.was.clone(),
                ..row.clone()
            },
            _ => row.clone(),
        })
        .collect();
    Roster { rows }
}

/// Derived renewal state of a roster account's login on this machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginState {
    /// It is signed out here ([`signed_out_here`]): its login's last word on record is
    /// [`Auth::SignedOut`] (one word for a login wall, a displacement by the vendor's own sign-in
    /// and a check that found no login), or its own credential file here shows it lapsed
    /// ([`State::lapsed_logins`]).
    Expired,
    /// Its forecast end ([`forecast`]) is within [`DUE_S`].
    Due {
        /// The forecast end.
        by: i64,
    },
    /// A turn was served under it within [`OK_FRESH_S`].
    Ok,
    /// None of the above: not a fault.
    Idle,
}

impl LoginState {
    /// Short status word for reports and CLI output.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            LoginState::Expired => "expired",
            LoginState::Due { .. } => "due",
            LoginState::Ok => "ok",
            LoginState::Idle => "idle",
        }
    }
}

/// Retrospective estimate from all retained dates, without checking their applicability.
/// Use [`login_forecast`] for the current login's status, scheduling and reports.
#[must_use]
pub fn forecast(binding: &Binding) -> Option<i64> {
    let by_life = binding
        .renewed_at
        .map(|at| at.saturating_add(binding.lifetime()));
    [by_life, binding.notice_end].into_iter().flatten().min()
}

/// The expiration estimate supported by observations applicable to this row's current login.
/// Retained dates that describe another managed login remain available through [`forecast`].
#[must_use]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountLifetimeScope",
        action = "Read",
        project = "aterm_agent::harness::accounts::lifetime_scope_tests::lifetime_transition"
    )
)]
pub fn login_forecast(row: &Row, logins: &Logins, state: &State) -> Option<i64> {
    if state.contextual_auth {
        let account = state.accounts.get(&row.label)?;
        return (account.current_auth(row) != Auth::Unseen && account.renewal_metadata)
            .then(|| logins.get(&row.label).and_then(forecast))
            .flatten();
    }
    auth_scope::lifetime_forecast(row, logins)
}

/// The last served observation applicable to the current login, not retained history.
#[must_use]
pub fn login_served_at(row: &Row, logins: &Logins, state: &State) -> Option<i64> {
    if state.contextual_auth {
        let account = state.accounts.get(&row.label)?;
        return (account.current_auth(row) != Auth::Unseen && account.served_metadata)
            .then(|| logins.get(&row.label)?.served_at)
            .flatten();
    }
    auth_scope::lifetime_served(row, logins)
}

/// The retained end observation only when it applies to this managed login.
/// Contextual stores keep their existing credential-authentication authority.
#[must_use]
pub fn login_ended_at(row: &Row, logins: &Logins, state: &State) -> Option<i64> {
    if state.contextual_auth {
        return logins.get(&row.label)?.ended_at;
    }
    auth_scope::lifetime_ended(row, logins)
}

/// Whether `row`'s own credential file here shows its login lapsed ([`State::lapsed_logins`],
/// which `Store::hydrate_auth_contexts` fills from [`crate::ai::connector::stored_login_lapsed`]):
/// read from that file alone, never from a process, the network, a browser or the keychain. A
/// state nothing hydrated (a plain [`Store::load`]) holds no lapse, and nothing records one: it is
/// a runtime reading, not an observation.
#[must_use]
pub fn lapsed_here(row: &Row, state: &State) -> bool {
    state.lapsed_logins.contains(&row.label)
}

/// THE ONE RULE for whether `row`'s login is signed out on this machine, which [`condition`]
/// reads as [`Condition::SignedOut`] (for a row enrolled here) and [`login_state`] as
/// [`LoginState::Expired`]: its own credential file here shows it lapsed ([`lapsed_here`]), or
/// its login's last word on record is [`Auth::SignedOut`] (one word for a wall a session or a
/// read met, [`strike`] and [`note_login_end`], a displacement by the vendor's own sign-in,
/// [`note_displaced`], and a check that found no login). A keychain-held default login is never
/// lapsed here (its store names no file for it, [`Store::default_login_home`]), so it reads by
/// its record alone.
#[must_use]
pub fn signed_out_here(row: &Row, state: &State) -> bool {
    lapsed_here(row, state)
        || state
            .accounts
            .get(&row.label)
            .is_some_and(|account| account.current_auth(row) == Auth::SignedOut)
}

/// Computes the [`LoginState`] of `row` at `now` from `logins` and `state`: [`LoginState::Expired`]
/// exactly where [`signed_out_here`] holds, whatever its records forecast, and otherwise what its
/// records say (`recorded_login_state`).
#[must_use]
pub fn login_state(row: &Row, logins: &Logins, state: &State, now: i64) -> LoginState {
    if signed_out_here(row, state) {
        return LoginState::Expired;
    }
    recorded_login_state(row, logins, state, now)
}

/// The [`LoginState`] of `row` by its records alone, blind to a lapse its credential file shows
/// ([`lapsed_here`]): what [`login_state`] answers of a row not signed out here, and the only
/// reading the renewal door takes ([`renewal_reading`]).
fn recorded_login_state(row: &Row, logins: &Logins, state: &State, now: i64) -> LoginState {
    let account = state.accounts.get(&row.label);
    let auth = account.map_or(Auth::Unseen, |account| account.current_auth(row));
    if auth == Auth::SignedOut {
        return LoginState::Expired;
    }
    if state.contextual_auth && auth == Auth::Unseen {
        return LoginState::Idle;
    }
    if let Some(by) = login_forecast(row, logins, state)
        && by.saturating_sub(now) <= DUE_S
    {
        return LoginState::Due { by };
    }
    if login_served_at(row, logins, state).is_some_and(|at| recent(at, now, OK_FRESH_S)) {
        LoginState::Ok
    } else {
        LoginState::Idle
    }
}

/// How long a login keeps being renewed on this Mac after a session here last ran on it
/// ([`Account::ran_at`], [`used_here`]): a week (`DESIGN-alab-one-mesh-2026-11-05.md` §4.15
/// rule 4).
pub const USED_WITHIN_S: i64 = WEEK_WINDOW_S;

/// Whether this Mac uses `label`'s login, so its upkeep may renew it (§4.15 rule 4): a session
/// here ran on it within [`USED_WITHIN_S`] ([`Account::ran_at`], which only a session noted on
/// this Mac sets), and this Mac holds the account's senior live claim or no Mac claims it
/// ([`State::leases`], as the claims ring settles it).
#[must_use]
pub fn used_here(state: &State, label: &str, now: i64) -> bool {
    #[cfg(any(test, debug_assertions))]
    if USE_CONDITION_OFF.with(std::cell::Cell::get) {
        return true;
    }
    ran_here(state, label, now) && claim_ours_or_none(state, label, now)
}

/// Whether a session here ran on `label` within [`USED_WITHIN_S`] ([`Account::ran_at`]): the
/// first half of [`used_here`].
#[must_use]
pub fn ran_here(state: &State, label: &str, now: i64) -> bool {
    state
        .accounts
        .get(label)
        .and_then(|account| account.ran_at)
        .is_some_and(|at| recent(at, now, USED_WITHIN_S))
}

/// Whether a session on this Mac ever ran on `label` ([`Account::ran_at`], which only a session
/// noted here sets): the proof that a login whose directory has gone was made here once, however
/// long ago.
#[must_use]
pub fn ran_here_ever(state: &State, label: &str) -> bool {
    state
        .accounts
        .get(label)
        .is_some_and(|account| account.ran_at.is_some())
}

/// Whether this Mac holds `label`'s senior live claim or no Mac claims it ([`State::leases`], as
/// the claims ring settles it): the second half of [`used_here`].
#[must_use]
pub fn claim_ours_or_none(state: &State, label: &str, now: i64) -> bool {
    state
        .leases
        .get(label)
        .filter(|lease| lease.until > now)
        .is_none_or(|lease| state.owns_swarm_label(&lease.holder))
}

#[cfg(any(test, debug_assertions))]
thread_local! {
    static USE_CONDITION_OFF: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// NEGATIVE CONTROL seam: runs `body` with [`used_here`] true for every login on this thread, as
/// the upkeep was before it renewed only logins this Mac uses, so a test can show its own
/// assertion fails without the use condition, or observe a login's lifetime apart from it.
#[cfg(any(test, debug_assertions))]
pub fn without_use_condition<R>(body: impl FnOnce() -> R) -> R {
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            USE_CONDITION_OFF.with(|off| off.set(self.0));
        }
    }
    let _restore = Restore(USE_CONDITION_OFF.with(|off| off.replace(true)));
    body()
}

/// The rows of a roster with no login on this Mac that its upkeep leaves for a person, by what
/// their source says of them or could not say (`DESIGN-alab-one-mesh-2026-11-05` §4.15 rule 3,
/// [`super::accounts_sync::rows_of_other_macs`]). A row with a login here is never one of them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OtherMacs {
    /// Each row whose account the source gives to other Macs alone, with the first Mac it names.
    pub named: BTreeMap<String, String>,
    /// Each row whose source this Mac cannot compare with its own names: one only the network
    /// could read, one that does not parse or a lab that cannot be read, or a Mac with no name or
    /// an unreadable pass roster. Its account may be another Mac's, so no door signs it in unasked.
    pub unread: BTreeSet<String>,
    /// Why `unread` could not be compared, one cause for the whole read; `None` where none is.
    pub unread_why: Option<UnreadWhy>,
    /// Each row that runs on its vault token on this Mac, with the token's end (unix seconds):
    /// renewal is off for it, so no door signs its stored login in with nobody asking, and a
    /// person's `accounts renew <label>` says so (`docs/DESIGN-claude-setup-token-2026-11-05.md`
    /// §5.8; [`super::accounts_tokens::rows_on_token`]).
    pub on_token: BTreeMap<String, u64>,
}

/// Why this Mac could not compare its source with its own names ([`OtherMacs::unread`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnreadWhy {
    /// The source is only on the network, which nothing fetches to judge whose an account is.
    Network,
    /// The source takes no name of this Mac's: it has none, or its pool refuses the one it has.
    NoName,
    /// The source, the lab or this Mac's swarm roster cannot be read or parsed here.
    Source,
}

impl UnreadWhy {
    /// The cause in a report's words, as [`super::sign_in_ladder::upkeep_waits_because`] gives it.
    #[must_use]
    pub const fn words(self) -> &'static str {
        match self {
            Self::Network => {
                "its source is only on the network, which aterm does not fetch to tell whose an \
                 account is"
            }
            Self::NoName => {
                "its source takes no name of this Mac's (`[fleet] machine` in aterm.toml names \
                 this Mac)"
            }
            Self::Source => {
                "this Mac cannot read its source, its lab or its swarm roster here, so it cannot \
                 tell whose the account is"
            }
        }
    }
}

impl OtherMacs {
    /// No row is another Mac's: nothing limits who runs an account, or every row has a login here.
    pub const NONE: Self = Self {
        named: BTreeMap::new(),
        unread: BTreeSet::new(),
        unread_why: None,
        on_token: BTreeMap::new(),
    };

    /// Whether `label` is one of them, named, unread or on its vault token: no door signs it in
    /// with nobody asking.
    #[must_use]
    pub fn holds(&self, label: &str) -> bool {
        self.named.contains_key(label)
            || self.unread.contains(label)
            || self.on_token.contains_key(label)
    }
}

/// Whether `row`'s login is one this Mac's upkeep renews by itself: the row names an `email`, no
/// other host holds it, it is not one of `others` (the rows with no login here whose account the
/// source gives to other Macs alone, or whose source this Mac cannot compare, [`OtherMacs`]:
/// §4.15 rule 3), this Mac uses it ([`used_here`]), and its records say its login has ended or
/// comes due ([`LoginState::Expired`], [`LoginState::Due`] by `recorded_login_state`). A login
/// whose directory has gone since a session here ran on it counts: that session's run is the
/// proof the login was made here, and the sync records a login it finds gone signed out, which is
/// an ended one. A login no session here has run on for a week, or that another Mac holds the
/// senior claim of, is not: it stays as it is until a person signs it in, or a session here runs
/// on it and loses its login (`DESIGN-alab-one-mesh-2026-11-05.md` §4.15 rule 4). Nor is a login
/// whose credential file alone shows it lapsed ([`lapsed_here`]): it reads expired
/// ([`login_state`]) and waits for a person's sign-in or a session here that meets its wall.
#[must_use]
pub fn renewal_due(
    row: &Row,
    logins: &Logins,
    state: &State,
    others: &OtherMacs,
    now: i64,
) -> bool {
    row.held_by.is_none()
        && row.email.is_some()
        && !others.holds(&row.label)
        && used_here(state, &row.label, now)
        && matches!(
            renewal_reading(row, logins, state, now),
            LoginState::Expired | LoginState::Due { .. }
        )
}

/// The [`LoginState`] the renewal door ([`renewal_due`]) opens on: the records'
/// ([`recorded_login_state`]), not [`login_state`]'s. What the store records opens it: a sign-out
/// on record (a wall a session or a read met, a displacement by the vendor's own sign-in, or a
/// used login's directory the sync found gone, `DESIGN-alab-one-mesh-2026-11-05.md` §4.15 rule 4)
/// or a forecast end a renewal or a notice set. A lapse its credential file alone shows
/// ([`lapsed_here`]) is on no record: the upkeep reads that file for every row on its timer, and
/// the owner's rule for one-mesh AC1 is that the upkeep starts no renewal or sign-in on its own
/// for a lapsed file login (sign-ins are local, never by a process that polls, and never raise a
/// prompt). So a lapse adds no renewal and takes none away; the test
/// `the_upkeep_starts_nothing_for_a_lapsed_login` (`accounts_no_sweep_tests.rs`) holds it.
fn renewal_reading(row: &Row, logins: &Logins, state: &State, now: i64) -> LoginState {
    #[cfg(any(test, debug_assertions))]
    if RENEWAL_ON_LOGIN_STATE.with(std::cell::Cell::get) {
        return login_state(row, logins, state, now);
    }
    recorded_login_state(row, logins, state, now)
}

#[cfg(any(test, debug_assertions))]
thread_local! {
    static RENEWAL_ON_LOGIN_STATE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// NEGATIVE CONTROL seam: runs `body` with [`renewal_due`] reading [`login_state`] on this thread,
/// as a renewal door that a credential file's lapse opens would, so a test can show its own
/// assertion that the upkeep starts nothing for a lapsed login fails without the records' reading.
#[cfg(any(test, debug_assertions))]
pub fn with_renewal_on_login_state<R>(body: impl FnOnce() -> R) -> R {
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            RENEWAL_ON_LOGIN_STATE.with(|on| on.set(self.0));
        }
    }
    let _restore = Restore(RENEWAL_ON_LOGIN_STATE.with(|on| on.replace(true)));
    body()
}

/// The labels of `roster`'s rows whose logins this Mac's upkeep renews by itself, in roster order
/// ([`renewal_due`], which `others`, the rows whose account may be another Mac's, never pass).
#[must_use]
pub fn renew_due(
    roster: &Roster,
    logins: &Logins,
    state: &State,
    others: &OtherMacs,
    now: i64,
) -> Vec<String> {
    roster
        .rows
        .iter()
        .filter(|row| renewal_due(row, logins, state, others, now))
        .map(|row| row.label.clone())
        .collect()
}

/// The logins still on this Mac that [`renew_due`] lists, in roster order: a due or ended login
/// this Mac holds (`enrolled`), which the upkeep renews by itself. A row whose login has gone is a
/// sign-in rather than a renewal, which a report names with its routes once it fails
/// ([`failing_sign_ins`]). `enrolled` is the set [`super::accounts_sync::rows_of_other_macs`]
/// reads, [`enrolled_in`] this Mac's home, so no enrolled row is one of [`renewal_due`]'s
/// `others`, which have no login here, and none is read.
#[must_use]
pub fn due_logins_here(
    roster: &Roster,
    logins: &Logins,
    state: &State,
    enrolled: &BTreeSet<String>,
    now: i64,
) -> Vec<String> {
    roster
        .rows
        .iter()
        .filter(|row| {
            is_enrolled(row, enrolled) && renewal_due(row, logins, state, &OtherMacs::NONE, now)
        })
        .map(|row| row.label.clone())
        .collect()
}

/// Returns whether the roster has renewable accounts enrolled here and none remain working past
/// `now + within`.
#[must_use]
pub fn none_working_within(
    roster: &Roster,
    logins: &Logins,
    state: &State,
    enrolled: &BTreeSet<String>,
    now: i64,
    within: i64,
) -> bool {
    let mut rows = roster
        .rows
        .iter()
        .filter(|row| row.held_by.is_none() && row.email.is_some() && is_enrolled(row, enrolled))
        .peekable();
    rows.peek().is_some()
        && rows.all(|row| match login_state(row, logins, state, now) {
            LoginState::Expired => true,
            _ => login_forecast(row, logins, state)
                .is_some_and(|by| by.saturating_sub(now) <= within),
        })
}

/// Returns the labels in roster order whose last renewal authenticated the wrong account.
#[must_use]
pub fn wrong_accounts(roster: &Roster, logins: &Logins) -> Vec<String> {
    roster
        .rows
        .iter()
        .filter(|row| {
            row.held_by.is_none() && logins.get(&row.label).is_some_and(|b| b.wrong_at.is_some())
        })
        .map(|row| row.label.clone())
        .collect()
}

/// The configured raw roster's current human sign-in actions, in roster order.
#[must_use]
pub fn sign_in_tasks(roster: &Roster, logins: &Logins) -> Vec<(String, SignInAction)> {
    roster
        .rows
        .iter()
        .filter_map(|row| {
            let task = current_sign_in_task(row, logins)?;
            Some((row.label.clone(), task.action))
        })
        .collect()
}

/// Records a renewal's remaining human action without changing its login binding.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountWorkflowScope",
        action = "FreshTask",
        project = "aterm_agent::harness::accounts::workflow_scope_tests::workflow_transition"
    )
)]
pub fn note_sign_in_task(logins: &mut Logins, label: &str, action: SignInAction, at: i64) {
    if let Some(binding) = logins.entry(label) {
        binding.sign_in_task = Some(SignInTask { action, at });
        workflow_scope::stamp(logins, label, workflow_scope::Guidance::Task);
    }
}

/// The `why` prefix of the [`Kind::SignIn`] event an automatic sign-in writes
/// ([`note_sign_in_attempt`]).
pub const AUTO_SIGN_IN: &str = "auto";

/// Records how `label`'s automatic sign-in ended at `at` and the `routes` its ladder tried,
/// without changing its login binding: one [`Kind::SignIn`] event (`auto <outcome>: <routes>`)
/// when the workflow, outcome or routes differ from the row's last attempt. An unchanged
/// failure within the same workflow is written once however often it is tried again.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountWorkflowScope",
        action = "FreshAttempt",
        project = "aterm_agent::harness::accounts::workflow_scope_tests::workflow_transition"
    )
)]
pub fn note_sign_in_attempt(
    logins: &mut Logins,
    label: &str,
    outcome: AttemptOutcome,
    routes: &str,
    at: i64,
) -> Vec<Event> {
    let routes = one_line(routes, ROUTES_MAX);
    if !admissible_label(label) {
        return Vec::new();
    }
    let current = workflow_scope::attempt_current(logins, label);
    // An identical summary still observes this workflow and must fence later replacement.
    workflow_scope::stamp(logins, label, workflow_scope::Guidance::Attempt);
    let Some(binding) = logins.entry(label) else {
        return Vec::new();
    };
    if current
        && let Some(last) = binding.sign_in_attempt.as_mut()
        && last.outcome == outcome
        && last.routes == routes
    {
        if outcome == AttemptOutcome::Blocked {
            last.tried_at = at;
        }
        return Vec::new();
    }
    let mut event = Event::new(at, Kind::SignIn, label);
    event.why = one_line(
        &format!("{AUTO_SIGN_IN} {}: {routes}", outcome.as_str()),
        LEDGER_WHY_MAX,
    );
    binding.sign_in_attempt = Some(SignInAttempt {
        outcome,
        routes,
        since: at,
        tried_at: at,
    });
    vec![event]
}

/// Whether `row` still needs a sign-in on this machine: it has no login it can be run on here
/// (not enrolled, or signed out), or it has an email and its login is due for renewal
/// ([`login_state`]). A failed automatic sign-in of such a row is still news
/// ([`failing_sign_ins`]); one of a row whose login works and is not due is not.
#[must_use]
pub fn needs_sign_in(row: &Row, state: &State, logins: &Logins, enrolled: bool, now: i64) -> bool {
    row.held_by.is_none()
        && (matches!(
            condition(row, state, enrolled, now),
            Condition::NotEnrolled | Condition::SignedOut
        ) || (row.email.is_some()
            && enrolled
            && matches!(login_state(row, logins, state, now), LoginState::Due { .. })))
}

/// The bound rows whose current automatic sign-in failed and which still need one
/// ([`needs_sign_in`]: not enrolled, signed out, or due for renewal), in roster order, with that
/// attempt. `raw_roster` supplies each workflow's configured policy.
#[must_use]
pub fn failing_sign_ins(
    roster: &Roster,
    raw_roster: &Roster,
    state: &State,
    logins: &Logins,
    now: i64,
) -> Vec<(String, SignInAttempt)> {
    let enrolled = enrolled(roster);
    roster
        .rows
        .iter()
        .filter(|row| needs_sign_in(row, state, logins, is_enrolled(row, &enrolled), now))
        .filter_map(|row| {
            let attempt = current_sign_in_attempt(raw_roster.row(&row.label)?, logins)?.clone();
            attempt
                .outcome
                .failing()
                .then(|| (row.label.clone(), attempt))
        })
        .collect()
}

/// Whether some Claude Code login works here, so a row's failed sign-in leaves nothing broken: a
/// row on a login of its own that a session was seen on and that is not signed out, or the
/// default login (`default_signed_in`) where no row on it is recorded signed out.
#[must_use]
pub fn claude_serves(
    roster: &Roster,
    state: &State,
    enrolled: &BTreeSet<String>,
    default_signed_in: bool,
    now: i64,
) -> bool {
    let of = |row: &Row| condition(row, state, is_enrolled(row, enrolled), now);
    roster.rows.iter().any(|row| {
        row.dir.is_some()
            && row.held_by.is_none()
            && matches!(
                of(row),
                Condition::Usable | Condition::Low | Condition::Limited { .. }
            )
    }) || (default_signed_in
        && !roster
            .rows
            .iter()
            .any(|row| row.dir.is_none() && of(row) == Condition::SignedOut))
}

/// Records a login wall hit at `at` on `label`'s bound login and updates learned lifetime.
pub fn note_login_end(state: &mut State, logins: &mut Logins, label: &str, at: i64) -> Vec<Event> {
    let mut out = if state.contextual_auth && state.account(label).auth == Auth::SignedOut {
        Vec::new()
    } else {
        note_auth_in_context(state, logins, label, Auth::SignedOut, at)
    };
    auth_scope::prepare_lifetime(logins, label);
    let ended_current = auth_scope::clock_current(logins, label, auth_scope::Clock::Ended);
    let renewal_current = auth_scope::clock_current(logins, label, auth_scope::Clock::Renewal);
    let Some(binding) = logins.entry(label) else {
        return out;
    };
    let first = !ended_current
        || binding.ended_at.is_none_or(|ended| {
            renewal_current && binding.renewed_at.is_some_and(|renewed| ended < renewed)
        });
    if first {
        binding.ended_at = Some(at);
        if renewal_current
            && binding
                .renewed_at
                .is_some_and(|renewed| at.saturating_sub(renewed) >= LIFETIME_MIN_S)
        {
            binding.lower_lifetime(at);
        }
        out.push(Event::new(at, Kind::LoginEnded, label));
    }
    auth_scope::stamp_lifetime(logins, label, auth_scope::Clock::Ended, None);
    out
}

/// Records a vendor `Your login expires in N days` notice read at `at` on `label`.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountLifetimeScope",
        action = "Notice",
        project = "aterm_agent::harness::accounts::lifetime_scope_tests::lifetime_transition"
    )
)]
pub fn note_login_notice(logins: &mut Logins, label: &str, days: u32, at: i64) -> bool {
    if days == 0 || days > NOTICE_DAYS_MAX {
        return false;
    }
    auth_scope::prepare_lifetime(logins, label);
    let notice_current = auth_scope::clock_current(logins, label, auth_scope::Clock::Notice);
    let renewal_current = auth_scope::clock_current(logins, label, auth_scope::Clock::Renewal);
    let Some(binding) = logins.entry(label) else {
        return false;
    };
    let end = at.saturating_add(i64::from(days).saturating_mul(DAY_S));
    let before = binding.clone();
    binding.notice_end = Some(if notice_current {
        binding.notice_end.map_or(end, |held| held.min(end))
    } else {
        end
    });
    if renewal_current {
        binding.lower_lifetime(end);
    }
    auth_scope::stamp_lifetime(logins, label, auth_scope::Clock::Notice, None);
    logins.get(label).is_some_and(|binding| *binding != before)
}

/// Records that `label`'s bound login served a turn at `at`.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountLifetimeScope",
        action = "Served",
        project = "aterm_agent::harness::accounts::lifetime_scope_tests::lifetime_transition"
    )
)]
pub fn note_served_login(logins: &mut Logins, label: &str, at: i64) -> bool {
    auth_scope::prepare_lifetime(logins, label);
    let current = auth_scope::clock_current(logins, label, auth_scope::Clock::Served);
    let Some(binding) = logins.entry(label) else {
        return false;
    };
    let served = if current {
        binding.served_at.map_or(at, |held| held.max(at))
    } else {
        at
    };
    let changed = binding.served_at != Some(served) || !current;
    binding.served_at = Some(served);
    auth_scope::stamp_lifetime(logins, label, auth_scope::Clock::Served, None);
    changed
}

/// Records that `label`'s bound login served a turn at `at` after being marked signed out.
/// Return its events from the enclosing transaction so the success receipt is committed.
pub fn note_signed_back(
    state: &mut State,
    logins: &mut Logins,
    label: &str,
    at: i64,
) -> Vec<Event> {
    if state.accounts.get(label).is_none_or(|a| {
        a.auth != Auth::SignedOut && (state.contextual_auth || a.managed_auth_is_current())
    }) {
        return Vec::new();
    }
    note_checked_sign_in(state, logins, label, at)
}

/// Record a verified sign-in, including refreshed metadata on an existing binding.
/// A future authentication observation keeps its authority until time catches up.
/// Return these events from the account transaction to advance its success receipt.
pub(super) fn note_checked_sign_in(
    state: &mut State,
    logins: &mut Logins,
    label: &str,
    at: i64,
) -> Vec<Event> {
    auth_scope::retire_changed_context(state, logins, label, false);
    if state.accounts.get(label).is_some_and(|account| {
        (state.contextual_auth || account.managed_auth_is_current())
            && account.auth_at.is_some_and(|held| held > at)
    }) {
        return Vec::new();
    }
    let mut out = if state.contextual_auth {
        let out = note_auth(state, label, Auth::Ok, at);
        if state
            .accounts
            .get(label)
            .is_none_or(|account| account.auth != Auth::Ok)
        {
            return out;
        }
        out
    } else {
        if account_mut(state, label).is_none() {
            return Vec::new();
        }
        Vec::new()
    };
    workflow_scope::replace(logins, label);
    if !state.contextual_auth {
        auth_scope::replace_lifetime(logins, label);
    }
    if let Some(binding) = logins.labels.get_mut(label) {
        binding.renewed_at = Some(at);
        binding.notice_end = None;
        binding.served_at = Some(if state.contextual_auth {
            binding.served_at.map_or(at, |held| held.max(at))
        } else {
            at
        });
        binding.sign_in_task = None;
        binding.clear_failed_sign_in();
    }
    workflow_scope::clear_absent(logins, label);
    if !state.contextual_auth {
        auth_scope::clear_absent_lifetime(logins, label);
        auth_scope::stamp_lifetime(logins, label, auth_scope::Clock::Renewal, None);
        auth_scope::stamp_lifetime(logins, label, auth_scope::Clock::Served, None);
        // The verified legacy result describes the metadata just refreshed above.
        out = note_auth_in_context(state, logins, label, Auth::Ok, at);
        if state
            .accounts
            .get(label)
            .is_none_or(|account| account.managed_auth() != Auth::Ok || account.auth_at != Some(at))
        {
            return out;
        }
    }
    out.push(Event::new(at, Kind::SignedIn, label));
    out
}

/// Binds `label` to the newly authenticated staging directory `dir` after a verified renewal.
/// The store advances its publication generation when committing the returned renewal event.
/// A persistent caller must return these events from its enclosing login transaction.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountLifetimeScope",
        action = "Renewed",
        project = "aterm_agent::harness::accounts::lifetime_scope_tests::lifetime_transition"
    )
)]
pub fn note_renewed(
    state: &mut State,
    logins: &mut Logins,
    label: &str,
    own_dir: Option<&str>,
    dir: &str,
    subscription: Option<&str>,
    at: i64,
) -> Vec<Event> {
    let mut out = Vec::new();
    workflow_scope::replace(logins, label);
    if !state.contextual_auth {
        auth_scope::replace_lifetime(logins, label);
    }
    let Some(binding) = logins.entry(label) else {
        return out;
    };
    let replaced = binding.dir.clone().or_else(|| own_dir.map(str::to_owned));
    binding
        .was
        .retain(|was| *was != replaced && was.as_deref() != Some(dir));
    binding.was.insert(0, replaced);
    binding.was.truncate(MAX_LEFT);
    binding.dir = Some(dir.to_owned());
    binding.renewed_at = Some(at);
    binding.notice_end = None;
    binding.ended_at = None;
    binding.wrong_at = None;
    binding.proved_by = None;
    binding.proved_email = None;
    binding.proved_dir = None;
    binding.google_principal = None;
    binding.sign_in_task = None;
    binding.clear_failed_sign_in();
    binding.subscription = subscription
        .map(|s| one_line(s, SUBSCRIPTION_MAX))
        .filter(|s| !s.is_empty());
    workflow_scope::clear_absent(logins, label);
    if !state.contextual_auth {
        auth_scope::clear_absent_lifetime(logins, label);
        auth_scope::stamp_lifetime(logins, label, auth_scope::Clock::Renewal, None);
    }
    // A verified replacement is a new credential observation, even at the same
    // path and timestamp as the previous login.
    if !state.contextual_auth
        && let Some(account) = state.accounts.get_mut(label)
    {
        account.auth_scope.retire();
    }
    out.extend(note_auth_in_context(state, logins, label, Auth::Ok, at));
    out.push(Event::new(at, Kind::Renewed, label));
    out
}

/// Records what proved `label`'s newly bound login is its account ([`Binding::proved_by`]),
/// after [`note_renewed`] in the same transaction.
pub fn note_proof(logins: &mut Logins, label: &str, proof: Proof) {
    if let Some(binding) = logins.labels.get_mut(label) {
        binding.proved_by = Some(proof);
        binding.proved_email = None;
        binding.proved_dir = None;
        binding.google_principal = None;
    }
}

/// Retain the checked identity with the exact login it describes. Legacy proof
/// flags with no identity or directory are never upgraded by this function.
pub fn note_identity(logins: &mut Logins, label: &str, proof: Proof, email: Option<&str>) {
    note_proof(logins, label, proof);
    if proof == Proof::Status
        && let Some(email) = email.filter(|email| email_refusal(email).is_none())
        && let Some(binding) = logins.labels.get_mut(label)
        && binding.dir.is_some()
    {
        binding.proved_email = Some(email.to_owned());
        binding.proved_dir = binding.dir.clone();
    }
}

fn google_principal(value: &Value) -> Option<[u8; 32]> {
    let text = value.as_str()?;
    let mut digest = [0; 32];
    (text.len() == 64 && aterm_codec::hex::decode_into(text.as_bytes(), &mut digest))
        .then_some(digest)
}

#[cfg(unix)]
fn native_google_digest(staging: &Path) -> Option<[u8; 32]> {
    native_google_digest_checked(staging).ok().flatten()
}

#[cfg(unix)]
fn native_google_digest_checked(staging: &Path) -> io::Result<Option<[u8; 32]>> {
    let path = super::upgrade_agy::profile_grant(staging);
    let Some(bytes) = aterm_secrets::fsx::read_private(&path, 64 * 1024)
        .map_err(|_| io::Error::other("native grant could not be read"))?
    else {
        return Ok(None);
    };
    if bytes.is_empty() {
        return Ok(None);
    }
    let mut digest = aterm_digest::Sha256::new();
    digest.update(bytes.as_slice());
    Ok(Some(digest.finalize()))
}

#[cfg(not(unix))]
fn native_google_digest(_staging: &Path) -> Option<[u8; 32]> {
    None
}

#[cfg(not(unix))]
fn native_google_digest_checked(_staging: &Path) -> io::Result<Option<[u8; 32]>> {
    Ok(None)
}

/// Refuse replacing a retained Google subject with another canonical subject.
pub(super) fn require_google_principal(
    logins: &Logins,
    label: &str,
    principal: [u8; 32],
) -> Result<(), String> {
    if logins
        .labels
        .get(label)
        .and_then(|binding| binding.google_principal)
        .is_some_and(|old| old != principal)
    {
        return Err("canonical Google subject changed for this account".into());
    }
    Ok(())
}

/// Retain the canonical Google subject with the same scoped identity evidence.
pub(super) fn note_google_identity(
    logins: &mut Logins,
    label: &str,
    email: &str,
    principal: [u8; 32],
) {
    note_identity(logins, label, Proof::Status, Some(email));
    if let Some(binding) = logins.labels.get_mut(label)
        && binding.proved_email.as_deref() == Some(email)
        && binding.proved_dir.is_some()
    {
        binding.google_principal = Some(principal);
    }
}

/// Records that a renewal attempt for `label` at `at` authenticated a different account.
pub fn note_wrong_account(logins: &mut Logins, label: &str, at: i64) -> Vec<Event> {
    match logins.entry(label) {
        Some(binding) => {
            binding.wrong_at = Some(at);
            binding.sign_in_task = None;
            workflow_scope::clear_absent(logins, label);
            vec![Event::new(at, Kind::WrongAccount, label)]
        }
        None => Vec::new(),
    }
}

/// Records at `at` that the vendor's own sign-in on the default login replaced `label`'s account
/// there with `now_default`'s, so `label` has no login on this machine: its auth word is signed
/// out, a pin on it ends ([`Kind::Unpin`]), its binding's `served_at` (turns served under a login
/// it no longer holds) is cleared, `active` where it named `label` names `now_default`
/// ([`repoint_active`]), and one [`Kind::Displaced`] event says so. A later auth word already on
/// record is kept. The word is of the row's final login, so an earlier retired word does not hide
/// it, and neither does this transaction's edit ([`auth_scope::finish`]): a row that read `usable`
/// here could be rotated onto with no login.
pub fn note_displaced(
    state: &mut State,
    logins: &mut Logins,
    label: &str,
    now_default: &str,
    at: i64,
) -> Vec<Event> {
    let contextual = state.contextual_auth;
    let Some(record) = account_mut(state, label) else {
        return Vec::new();
    };
    if (!contextual && !record.managed_auth_is_current())
        || record.auth_at.is_none_or(|held| held <= at)
    {
        record.clear_auth_context();
        record.auth = Auth::SignedOut;
        record.auth_at = Some(at);
        record.auth_scope.reinstate();
        // Metadata loss is not a provider observation on the replacement. Preserve the
        // entry witness, including any actual result already captured in this transaction.
    }
    let mut out = vec![
        Event::new(at, Kind::Displaced, label)
            .with_to(now_default)
            .because(&format!("default login now {now_default}")),
    ];
    out.extend(repoint_active(state, label, now_default, at));
    if let Some(binding) = logins.labels.get_mut(label) {
        binding.served_at = None;
    }
    out
}

/// The default login moved from `was` to `now` at `at`: where `active` (the one account this home
/// runs on) named `was`, a pin on it ends ([`Kind::Unpin`], `displaced`) and it names `now`,
/// proven, with no canary and no rotation left to go back from ([`Undo`]): no rotation chose it
/// (the vendor's sign-in did, on the login the home's sessions already run on), so there is no
/// canary for any session to answer, and an unproven `active` would hold its own tab's move ahead
/// of a wall ([`StayWhy::Canary`]) with nothing ever to prove it. `previous` where it named
/// `now`, which is `active` itself now, is cleared. Any other `active` is left as it is.
pub fn repoint_active(state: &mut State, was: &str, now: &str, at: i64) -> Vec<Event> {
    let mut out = Vec::new();
    if was == now || state.active.as_deref() != Some(was) {
        return out;
    }
    end_pin(state, was, "displaced", at, &mut out);
    state.active = Some(now.to_owned());
    state.proven = true;
    state.canary = None;
    state.undo.clear();
    if state.previous.as_deref() == Some(now) {
        state.previous = None;
        state.previous_proven = false;
    }
    out
}

/// The sessions on record under `was`, each of which ran on the default login, are `label`'s
/// at `at`: the default login holds `label`'s account now. `active`, where it named `was`,
/// follows them ([`repoint_active`]).
pub fn move_default_sessions(state: &mut State, label: &str, was: &str, at: i64) -> Vec<Event> {
    if was == label {
        return Vec::new();
    }
    for session in state.sessions.values_mut() {
        if session.account == was {
            label.clone_into(&mut session.account);
        }
    }
    repoint_active(state, was, label, at)
}

/// Records at `at` that the vendor's own sign-in just made `label`'s login the machine's default
/// login (the default login newly names its row, the row `was` held it before): that login is
/// new, so `label`'s auth word is Ok and its binding served at `at`, with what its earlier login
/// left on record cleared (the end it was seen to meet, the vendor's expiry notice, a renewal's
/// remaining human step and a failed automatic sign-in), and every session on record under
/// `was` (each ran on the default login, which `label` holds now) is `label`'s, `active` with
/// them ([`move_default_sessions`]). A later auth word already on record is kept, as
/// [`note_auth`] keeps it. A row whose default login merely still matches is not this: its login
/// was not made now, and a sign-out on record stands.
pub fn note_default_login_made(
    state: &mut State,
    logins: &mut Logins,
    label: &str,
    was: Option<&str>,
    at: i64,
) -> Vec<Event> {
    let out = note_auth_on_default(state, logins, label, Auth::Ok, at);
    finish_default_login_made(state, logins, label, was, at, out, None)
}

/// The caller inspected this row's expected identity on the native default login. Retain
/// that observed policy rather than attributing the result to the transaction's entry row.
pub(super) fn note_default_login_made_observed(
    state: &mut State,
    logins: &mut Logins,
    row: &Row,
    was: Option<&str>,
    at: i64,
) -> Vec<Event> {
    let out = note_auth_for_observed_row(state, logins, row, Auth::Ok, at);
    finish_default_login_made(state, logins, &row.label, was, at, out, Some(row))
}

fn finish_default_login_made(
    state: &mut State,
    logins: &mut Logins,
    label: &str,
    was: Option<&str>,
    at: i64,
    mut out: Vec<Event>,
    observed: Option<&Row>,
) -> Vec<Event> {
    if !state.accounts.get(label).is_some_and(|account| {
        account.auth == Auth::Ok
            && (state.contextual_auth || account.managed_auth_is_current())
            && account.auth_at == Some(at)
    }) {
        return out;
    }
    workflow_scope::replace(logins, label);
    if !state.contextual_auth {
        auth_scope::replace_lifetime(logins, label);
    }
    if let Some(binding) = logins.entry(label) {
        binding.ended_at = None;
        binding.notice_end = None;
        binding.sign_in_task = None;
        binding.clear_failed_sign_in();
    }
    workflow_scope::clear_absent(logins, label);
    if !state.contextual_auth {
        auth_scope::clear_absent_lifetime(logins, label);
    }
    if let Some(row) = observed {
        note_served_login_for_observed_row(logins, row, at);
    } else {
        note_served_login(logins, label, at);
    }
    if let Some(was) = was {
        out.extend(move_default_sessions(state, label, was, at));
    }
    out.push(Event::new(at, Kind::SignedIn, label));
    out
}

/// Relabels one account's observations, binding, selections, sessions, claims and rollback
/// references. The caller establishes that both labels denote the same account and persists
/// the change under the writer lock. A move to itself preserves every field.
/// # Errors
/// Either label is invalid, or the destination retains any local account reference. Refusal
/// changes neither snapshot. A successful move leaves generation allocation to the transaction.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountRename",
        action = "Move",
        project = "aterm_agent::harness::accounts::rename_tests::rename_transition"
    )
)]
pub fn move_account(
    state: &mut State,
    logins: &mut Logins,
    from: &str,
    to: &str,
) -> Result<(), String> {
    move_accounts(state, logins, &[(from, to)])
}

/// Relabels several accounts at once, each as [`move_account`] relabels one: every pair's
/// account leaves its first label for its second together, so the accounts of one sync can trade
/// labels (a swap, or any cycle) with no free label between them. A pair whose labels are equal
/// is left as it is.
/// # Errors
/// A label is invalid, two pairs leave one label or take one label, or a destination that no
/// pair leaves retains any local account reference. Refusal changes neither snapshot. A
/// successful move leaves generation allocation to the transaction.
pub fn move_accounts(
    state: &mut State,
    logins: &mut Logins,
    moves: &[(&str, &str)],
) -> Result<(), String> {
    for label in moves.iter().flat_map(|(from, to)| [*from, *to]) {
        if let Some(why) = label_refusal(label) {
            return Err(format!("cannot relabel `{label}`: {why}"));
        }
    }
    let mut map: BTreeMap<&str, &str> = BTreeMap::new();
    let mut taken = BTreeSet::new();
    for &(from, to) in moves.iter().filter(|(from, to)| from != to) {
        if map.insert(from, to).is_some() || !taken.insert(to) {
            return Err(format!(
                "cannot move `{from}` to `{to}`: another move leaves or takes the same label; nothing was written"
            ));
        }
    }
    for (from, to) in &map {
        if !map.contains_key(to) && label_is_retained(state, logins, to) {
            return Err(format!(
                "cannot move `{from}` to `{to}`: the destination retains local login or account state; choose an unused label; nothing was written"
            ));
        }
    }
    for (from, to) in &map {
        for label in [*from, *to] {
            logins.sign_in_receipts.remove(label);
            logins
                .pending_sign_in_invalidations
                .insert(label.to_owned());
        }
    }
    auth_scope::move_contexts(logins, moves);
    workflow_scope::move_contexts(logins, moves);
    let moved: Vec<_> = map
        .iter()
        .map(|(from, to)| {
            (
                *to,
                state.accounts.remove(*from),
                state.leases.remove(*from),
                state.home_accounts.remove(*from),
                state.prepared_accounts.remove(*from),
                logins.labels.remove(*from),
                state.claimed_at.remove(*from),
            )
        })
        .collect();
    for (to, record, lease, home, prepared, binding, claimed) in moved {
        if let Some(at) = claimed {
            state.claimed_at.insert(to.to_owned(), at);
        }
        if let Some(record) = record {
            state.accounts.insert(to.to_owned(), record);
        }
        if let Some(lease) = lease {
            state.leases.insert(to.to_owned(), lease);
        }
        if home {
            state.home_accounts.insert(to.to_owned());
        }
        if prepared {
            state.prepared_accounts.insert(to.to_owned());
        }
        if let Some(binding) = binding {
            logins.labels.insert(to.to_owned(), binding);
        }
    }
    for named in state
        .sessions
        .values_mut()
        .map(|session| &mut session.account)
        .chain(state.active.as_mut())
        .chain(state.previous.as_mut())
        .chain(
            state
                .pending_pin
                .iter_mut()
                .filter_map(|request| match request {
                    PendingSelection::Account(request) => Some(&mut request.label),
                    PendingSelection::Next(_) => None,
                }),
        )
        .chain(state.probe.iter_mut().map(|claim| &mut claim.account))
        .chain(state.reset.iter_mut().map(|claim| &mut claim.account))
        .chain(
            state
                .undo
                .iter_mut()
                .filter_map(|undo| undo.previous.as_mut()),
        )
    {
        if let Some(to) = map.get(named.as_str()) {
            (*to).clone_into(named);
        }
    }
    Ok(())
}

/// Whether either snapshot still references a label, including pending work.
pub(super) fn label_is_retained(state: &State, logins: &Logins, label: &str) -> bool {
    logins.labels.contains_key(label)
        || logins.sign_in_receipts.contains_key(label)
        || state.accounts.contains_key(label)
        || state.prepared_accounts.contains(label)
        || state.active.as_deref() == Some(label)
        || state.previous.as_deref() == Some(label)
        || state
            .pending_pin
            .as_ref()
            .and_then(PendingSelection::account)
            .is_some_and(|pin| pin.label() == label)
        || state
            .sessions
            .values()
            .any(|session| session.account == label)
        || state
            .probe
            .as_ref()
            .is_some_and(|claim| claim.account == label)
        || state
            .reset
            .as_ref()
            .is_some_and(|claim| claim.account == label)
        || state
            .undo
            .iter()
            .any(|undo| undo.previous.as_deref() == Some(label))
}

/// Drops what is on record of the account the row `label` held, where a fleet gave the label to
/// another account: its observations (walls, samples, auth word) and its binding's turns served.
/// The login directory the label is bound to is kept: what it holds is checked where it is run.
pub fn forget_account(state: &mut State, logins: &mut Logins, label: &str) {
    workflow_scope::replace(logins, label);
    state
        .pending_auth
        .retain(|intent| intent.raw.label != label);
    logins.sign_in_receipts.remove(label);
    logins
        .pending_sign_in_invalidations
        .insert(label.to_owned());
    if let Some(account) = state.accounts.remove(label)
        && let Some(pk) = account.pk.clone()
    {
        remember_forgotten(state, pk, account);
    }
    state.leases.remove(label);
    state.home_accounts.remove(label);
    if let Some(binding) = logins.labels.get_mut(label) {
        binding.served_at = None;
    }
}

/// Keep what this Mac observed of a forgotten row under its account key ([`State::forgotten`]),
/// its login word left behind (it was this row's login directory's). The snapshot's byte budget
/// bounds publication without silently dropping an account's quota observations.
pub(crate) fn remember_forgotten(state: &mut State, pk: String, mut account: Account) {
    account.auth = Auth::Unseen;
    account.auth_at = None;
    account.auth_scope = ManagedScope::default();
    account.auth_context = None;
    account.renewal_metadata = false;
    account.served_metadata = false;
    account.auth_projection = AuthProjection::Unknown;
    account.pk = Some(pk.clone());
    if let Some(held) = state.forgotten.remove(&pk) {
        fill_observations(&mut account, held);
    }
    state.forgotten.insert(pk, account);
}

/// Move every observation [`fill_observations`] carries out of `account` into a row of their own,
/// leaving `account` its key and its login's machine-local word (`auth`, `auth_at`), which are
/// the login directory's, not the account's.
pub(crate) fn take_observations(account: &mut Account) -> Account {
    let observed = std::mem::take(account);
    account.pk.clone_from(&observed.pk);
    account.auth = observed.auth;
    account.auth_at = observed.auth_at;
    account.auth_scope.clone_from(&observed.auth_scope);
    account.auth_context.clone_from(&observed.auth_context);
    account.renewal_metadata = observed.renewal_metadata;
    account.served_metadata = observed.served_metadata;
    account.auth_projection = observed.auth_projection.clone();
    observed
}

/// Give `into` every observation it lacks from `from`: a field it already holds stays, since it
/// was observed of this row; each wall and window it has none of is taken.
pub(crate) fn fill_observations(into: &mut Account, from: Account) {
    fn fill<T>(slot: &mut Option<T>, from: Option<T>) {
        if slot.is_none() {
            *slot = from;
        }
    }
    for (window, wall) in from.walls {
        into.walls.entry(window).or_insert(wall);
    }
    for (window, fig) in from.windows {
        into.windows.entry(window).or_insert(fig);
    }
    for (bucket, said) in from.weeks_said {
        into.weeks_said.entry(bucket).or_insert(said);
    }
    fill(&mut into.sample, from.sample);
    fill(&mut into.sample_context, from.sample_context);
    fill(&mut into.before, from.before);
    fill(&mut into.offer, from.offer);
    fill(&mut into.cleared_at, from.cleared_at);
    fill(&mut into.room_at, from.room_at);
    fill(&mut into.ran_at, from.ran_at);
    fill(&mut into.credits, from.credits);
    fill(&mut into.credits_day, from.credits_day);
    fill(&mut into.warned_session, from.warned_session);
    fill(&mut into.warned_week, from.warned_week);
    fill(&mut into.key, from.key);
    fill(&mut into.keyed_at, from.keyed_at);
}

/// Newest observation or scoped clearing of a single vendor quota-limit type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Wall {
    /// The reset the wall named (Unix seconds), or `None` when it named none.
    pub until: Option<i64>,
    /// When it was observed.
    pub at: i64,
    /// The machine that observed it.
    pub by: String,
    /// How many times it was found standing with no reset ahead of it.
    pub attempts: u32,
    /// This window was cleared at `at`, rather than observed standing.
    pub cleared: bool,
}

fn rung(ladder: &[i64], attempts: u32) -> i64 {
    let i = usize::try_from(attempts.saturating_sub(1)).unwrap_or(usize::MAX);
    ladder
        .get(i)
        .or_else(|| ladder.last())
        .copied()
        .unwrap_or(0)
}

impl Wall {
    /// Returns the Unix timestamp until which this observed wall remains standing.
    #[must_use]
    pub fn stands_until(&self) -> Option<i64> {
        if self.cleared {
            return None;
        }
        Some(match self.until {
            Some(until) if self.at <= until => until.saturating_add(RESET_GRACE_S),
            Some(_) => self.at.saturating_add(rung(&LATE_WALL_S, self.attempts)),
            None => self
                .at
                .saturating_add(rung(&NO_RESET_WALL_S, self.attempts)),
        })
    }
}

/// An account's usage sample: a settled `/usage` panel, or the figures held by its tap.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Sample {
    /// `Current session`, whole percent used.
    pub session_pct: Option<u32>,
    /// When the session window resets.
    pub session_reset: Option<i64>,
    /// `Current week (all models)`, whole percent used.
    pub week_pct: Option<u32>,
    /// When the week resets.
    pub week_reset: Option<i64>,
    /// When the session figure was observed. Absent in older records: use [`Self::at`].
    pub session_read_at: Option<i64>,
    /// When the weekly figure was observed: a tap heartbeat keeps the window's time, a settled
    /// `/usage` panel its own read time. Absent in older records: use [`Self::at`].
    pub week_read_at: Option<i64>,
    /// When the sample was published, for heartbeat and probe scheduling. A tap sample's
    /// individual figures can have been observed earlier.
    pub at: i64,
    /// The machine that read it ([`note_sample`] stamps it).
    pub by: String,
}

fn figure(
    pct: Option<u32>,
    reset: Option<i64>,
    at: i64,
    window_s: i64,
    now: i64,
) -> Option<(u32, i64)> {
    let end = reset.unwrap_or_else(|| at.saturating_add(window_s));
    pct.filter(|_| now < end).map(|pct| (pct, end))
}

impl Sample {
    /// Returns the 5-hour session utilization percentage and reset timestamp while the window is
    /// active.
    #[must_use]
    pub fn session(&self, now: i64) -> Option<(u32, i64)> {
        figure(
            self.session_pct,
            self.session_reset,
            self.at,
            SESSION_WINDOW_S,
            now,
        )
    }

    /// Returns the 7-day weekly utilization percentage and reset timestamp while the window is
    /// active.
    #[must_use]
    pub fn week(&self, now: i64) -> Option<(u32, i64)> {
        figure(self.week_pct, self.week_reset, self.at, WEEK_WINDOW_S, now)
    }

    /// Returns the latest timestamp until which any running window in this sample is at or above
    /// its low threshold.
    #[must_use]
    pub fn low_until(&self, now: i64) -> Option<i64> {
        let at = |figure: Option<(u32, i64)>, threshold: u32| {
            figure
                .filter(|(pct, _)| *pct >= threshold)
                .map(|(_, end)| end)
        };
        at(self.session(now), LOW_SESSION_PCT).max(at(self.week(now), LOW_WEEK_PCT))
    }

    /// When `window`'s limit resets, where this sample shows that limit reached. A read short of the
    /// limit names when the window turns over, which says nothing of when a wall there ends, so a
    /// wall it stands beside keeps the rungs of one that names no reset ([`NO_RESET_WALL_S`]).
    #[must_use]
    pub fn reached_reset(&self, window: &str) -> Option<i64> {
        let (used, reset) = match window {
            "seven_day" => (self.week_pct, self.week_reset),
            "five_hour" => (self.session_pct, self.session_reset),
            _ => return None,
        };
        reset.filter(|_| used.is_some_and(|used| used >= 100))
    }

    /// Returns whether any running window in this sample is at or above its low threshold at `now`.
    #[must_use]
    pub fn low(&self, now: i64) -> bool {
        self.low_until(now).is_some()
    }

    /// Returns whether a session on this account should rotate early before hitting a wall.
    #[must_use]
    pub fn leave_early(&self, now: i64) -> bool {
        self.low_until(now)
            .is_some_and(|end| end.saturating_sub(now) > RESET_SOON_S)
    }
}

/// Limit-reset offer observed at a usage wall.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Offer {
    /// The kind `left` counts, where one was read.
    pub kind: Option<ResetKind>,
    /// Resets left (of `kind`, where one is named).
    pub left: u32,
    /// The grant's deadline, where one was read.
    pub use_by: Option<i64>,
    /// When it was seen.
    pub at: i64,
    /// The machine that saw it.
    pub by: String,
}

/// Newest observation of whether usage credits are carrying an account (`InUse` or `Off`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Credits {
    /// Seen carrying the account's sessions past a limit.
    InUse(CreditsUse),
    /// Off, as a limit row said (`quotaLimits.overageStatus: rejected`).
    Off(CreditsOff),
    /// On, as a limit row said (`allowed`, `allowed_warning`), at this instant.
    On(i64),
}

impl Credits {
    /// Timestamp when this credits observation was recorded.
    #[must_use]
    pub const fn at(&self) -> i64 {
        match self {
            Credits::InUse(c) => c.seen,
            Credits::Off(off) => off.at,
            Credits::On(at) => *at,
        }
    }
}

/// Active usage-credits episode carrying an account's sessions past a limit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreditsUse {
    /// The first answer seen served on credits, this episode.
    pub since: i64,
    /// The newest.
    pub seen: i64,
    /// The reset of the window credits carried past, where the notice they started on named it.
    pub until: Option<i64>,
    /// This episode raised the day's line ([`credits_noted`]).
    pub told: bool,
}

impl CreditsUse {
    /// Returns whether this credits episode is still considered active at `now`.
    #[must_use]
    pub fn stands(&self, now: i64) -> bool {
        now < self.until_at()
    }

    /// Timestamp until which this credits episode is considered active.
    #[must_use]
    pub fn until_at(&self) -> i64 {
        let reset = self
            .until
            .unwrap_or_else(|| self.since.saturating_add(SESSION_WINDOW_S));
        reset.max(self.seen.saturating_add(CREDITS_STALE_S))
    }
}

/// Observation that an account's usage credits are disabled, with the vendor's reason word.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreditsOff {
    /// The vendor's word for why.
    pub reason: String,
    /// When the row was written.
    pub at: i64,
    /// When they come back (`overageResetsAt`), where the row named it.
    pub until: Option<i64>,
}

/// Warning utilization figure and reset timestamp read from a session screen outside `/usage`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowSaid {
    /// Whole percent used, as of [`Self::at`].
    pub pct: Option<u32>,
    /// When the window resets, where the warning named it ([`reset_outcome`]).
    pub reset: Option<i64>,
    /// When it was true.
    pub at: i64,
}

impl WindowSaid {
    /// Returns the utilization percentage and window end timestamp while the warning window is
    /// active.
    #[must_use]
    pub fn figure(&self, window_s: i64, now: i64) -> Option<(u32, i64)> {
        figure(self.pct, self.reset, self.at, window_s, now)
    }
}

/// A model's week as Claude Code last said it on a session's screen, its wall or its warning
/// ([`Account::weeks_said`], [`record_usage`]), or a limit reset on that week since: stats only,
/// read by `accounts stats` and by no decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WeekSaid {
    /// Whole percent used: 100 at its wall. `None` for a limit reset on the week at [`Self::at`]
    /// ([`reset_outcome`]): used (with no [`Self::reset`]), or lifted by itself (with the reset of
    /// the wall it lifted), with no figure since.
    pub pct: Option<u32>,
    /// The week's reset, where the notice named it.
    pub reset: Option<i64>,
    /// When it was true.
    pub at: i64,
    /// The machine that read it.
    pub by: String,
    /// It was the model's wall, not its warning.
    pub wall: bool,
    /// Where it named no reset: the reset an earlier read of the same week named after it, so the
    /// week it was read in ends by then ([`note_week`]).
    pub ends_by: Option<i64>,
}

/// The bucket id ([`super::profile::Bucket::id`]) of the week a session's screen names for
/// `model` ([`Account::weeks_said`]): Fable's is `seven_day_overage_included`, the limit type a
/// Fable 429 names; Sonnet's `seven_day_sonnet`.
#[must_use]
pub const fn week_bucket(model: aterm_phase::WeekModel) -> &'static str {
    match model {
        aterm_phase::WeekModel::Fable => super::profile::Bucket::Fable.id(),
        aterm_phase::WeekModel::Sonnet => super::profile::Bucket::SevenDaySonnet.id(),
    }
}

/// The model whose week `bucket` is ([`week_bucket`]), or `None` for a bucket no screen names.
fn week_model(bucket: &str) -> Option<aterm_phase::WeekModel> {
    [
        aterm_phase::WeekModel::Fable,
        aterm_phase::WeekModel::Sonnet,
    ]
    .into_iter()
    .find(|model| week_bucket(*model) == bucket)
}

/// [`WindowFig::source`] of a usage tap's reading ([`note_reading`]).
pub const SOURCE_TAP: &str = "tap";

/// [`WindowFig::source`] of a settled `/usage` panel figure from the account's [`Sample`]
/// ([`note_sample`]).
pub const SOURCE_SAMPLE: &str = "sample";

/// The most windows one account keeps a [`WindowFig`] for: the vendor's two, its per-model weeks
/// and its spend limit, with room.
pub const MAX_WINDOWS: usize = 8;

/// A drop smaller than this, in tenths of a percent, is no refill ([`WindowRead::refilled`]).
pub const REFILL_MIN_X10: u32 = 10;

/// Single quota-window utilization reading held on an [`Account`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowFig {
    /// Tenths of a percent used (`425` is 42.5%), at most `10 *` [`PCT_MAX`].
    pub pct_x10: u32,
    /// The window's reset, the vendor's instant ([`merge_fig`]).
    pub reset: i64,
    /// When the reading was served, as near as its source knows ([`TapRead::at`]).
    pub at: i64,
    /// The machine that read it.
    pub by: String,
    /// Where it came from: in the state, [`SOURCE_TAP`] or [`SOURCE_SAMPLE`].
    pub source: String,
    /// A limit reset used inside this window ([`WindowRead::refilled`], [`reset_outcome`]).
    pub epoch_at: Option<i64>,
}

/// The windows a sample read with no reset named ([`Account::unnamed_windows`]): each a share used
/// and the latest its window can end.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UnnamedWindows {
    /// The five-hour window.
    pub session: Option<(u32, i64)>,
    /// The week.
    pub week: Option<(u32, i64)>,
}

impl WindowFig {
    /// Returns whether this window reading's reset timestamp is still in the future at `now`.
    #[must_use]
    pub const fn current(&self, now: i64) -> bool {
        now < self.reset
    }

    /// Returns whether this reading was served before an in-window limit reset.
    #[must_use]
    pub fn cut(&self) -> bool {
        !self.admits_reading(self.at)
    }

    /// Whether a reading at `at` is on or after this window's retained refill barrier.
    #[must_use]
    pub fn admits_reading(&self, at: i64) -> bool {
        self.epoch_at.is_none_or(|epoch| at >= epoch)
    }

    /// Utilization percentage rounded half-up to a whole integer.
    #[must_use]
    pub const fn whole(&self) -> u32 {
        self.pct_x10.saturating_add(5) / 10
    }
}

/// Merges a stored [`WindowFig`] with an incoming reading for the same quota window.
#[must_use]
pub fn merge_fig(
    held: Option<&WindowFig>,
    read: Option<&WindowFig>,
    now: i64,
) -> Option<WindowFig> {
    let held = held.filter(|h| h.current(now));
    let (h, r) = match (held, read) {
        (None, r) => return r.cloned(),
        (h, None) => return h.cloned(),
        (Some(h), Some(r)) => (h, r),
    };
    if r.reset != h.reset {
        return Some(if r.reset > h.reset { r } else { h }.clone());
    }
    let epoch_at = h.epoch_at.max(r.epoch_at);
    let live = |f: &WindowFig| epoch_at.is_none_or(|epoch| f.at >= epoch);
    let pick = match (live(h), live(r)) {
        (true, true) if (r.pct_x10, r.at) > (h.pct_x10, h.at) => r,
        (false, false) if r.at > h.at => r,
        (true, true) | (true, false) | (false, false) => h,
        (false, true) => r,
    };
    Some(WindowFig {
        epoch_at,
        ..pick.clone()
    })
}

/// Single quota window from a usage-tap reading.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowRead {
    /// Tenths of a percent used, as the vendor sent it (`used_percentage` carries one decimal).
    pub pct_x10: u32,
    /// The window's reset.
    pub reset: i64,
    /// The figure the vendor refilled the window from, where it did ([`REFILL_MIN_X10`],
    /// [`TapRead::seen_before`]).
    pub refilled: Option<u32>,
}

/// Fresh usage-tap reading passed to [`note_reading`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TapRead {
    /// The five-hour window, where the reading carried it.
    pub five_hour: Option<WindowRead>,
    /// The week.
    pub seven_day: Option<WindowRead>,
    /// When its figures were served.
    pub at: i64,
    /// The host read this process's tap before and the process was served since.
    pub seen_before: bool,
    /// When the process began (its kernel start, Unix seconds), where the host read it.
    pub born: Option<i64>,
    /// The account's key.
    pub key: Option<super::profile::AccountKey>,
}

/// Machine-local authentication status last observed for an account's login.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Auth {
    /// Nothing is on record.
    #[default]
    Unseen,
    /// The check after a sign-in, or a served response, said the login works.
    Ok,
    /// A login wall was seen on it ([`strike`], [`note_login_end`]), a first login's flow
    /// failed, or the check after a sign-in said it is signed out.
    SignedOut,
}

impl Auth {
    /// Canonical status word for `state.json` and reports.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Auth::Unseen => "unseen",
            Auth::Ok => "ok",
            Auth::SignedOut => "signed-out",
        }
    }

    /// Parses an [`Auth`] status from its state-file word ([`Auth::Unseen`]).
    #[must_use]
    pub fn parse(word: &str) -> Auth {
        match word {
            "ok" => Auth::Ok,
            "signed-out" => Auth::SignedOut,
            _ => Auth::Unseen,
        }
    }
}

/// A local pooled sample's SHA-256 cache key, with no credential material in its input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SampleContext(pub(super) String);

impl SampleContext {
    fn parse(text: &str) -> Option<Self> {
        aterm_codec::hex::is_lower(text, 64).then(|| Self(text.to_owned()))
    }
}

/// Per-account observations persisted in `state.json`.
#[derive(Debug, Clone, Default)]
pub struct Account {
    /// Observation: the walls, by the vendor's limit type.
    pub walls: BTreeMap<String, Wall>,
    /// Observation: the newest sample.
    pub sample: Option<Sample>,
    /// The pooled reader's configuration and login fingerprint for `sample`. This is a local
    /// cache key, never evidence of a provider identity or permission to clear a limit.
    pub sample_context: Option<SampleContext>,
    /// Observation: the sample before it (for the burn rate).
    pub before: Option<Sample>,
    /// Observation: the reset offer last seen.
    pub offer: Option<Offer>,
    /// Machine-local: the login's last known word.
    pub auth: Auth,
    /// Machine-local: when that word was learned.
    pub auth_at: Option<i64>,
    /// Machine-local applicability of a legacy harness's auth word to its managed login.
    pub auth_scope: ManagedScope,
    /// Machine-local credential and routing context of the retained auth observation.
    pub auth_context: Option<AuthContext>,
    /// Local renewal metadata was established in `auth_context`, rather than inherited.
    pub renewal_metadata: bool,
    /// A served turn was observed in `auth_context`, rather than inherited.
    pub served_metadata: bool,
    /// Read-only applicability projection; never serialized or compared as persisted state.
    pub auth_projection: AuthProjection,
    /// Machine-local: the latest account-wide clearing ([`pin`], [`note_reading`]).
    pub cleared_at: Option<i64>,
    /// Machine-local: the newest read naming a window with none of them full ([`note_sample`]),
    /// kept past a later read that finds one full ([`Account::room_since`]).
    pub room_at: Option<i64>,
    /// Machine-local: when a session was last read running on it ([`note_session`],
    /// [`reset_allowed`]).
    pub ran_at: Option<i64>,
    /// Observation: the newest word on its usage credits ([`Credits`]).
    pub credits: Option<Credits>,
    /// Machine-local: the local day the last credits episode that raised the day's line began on.
    pub credits_day: Option<i64>,
    /// Observation: the newest word on its five-hour window apart from a `/usage` sample
    /// ([`WindowSaid`]).
    pub warned_session: Option<WindowSaid>,
    /// Observation: the same of its week ([`windows_full`], [`Self::week_low_until`]).
    pub warned_week: Option<WindowSaid>,
    /// Stats only: each model's week as a session's screen last said it, by bucket id
    /// ([`week_bucket`], [`WeekSaid`]), or a limit reset on that week since ([`reset_outcome`]).
    /// No decision reads it: it is in neither [`Self::observed`], [`condition`],
    /// [`Self::limited_until`], [`Self::low`] nor [`windows_full`] (the stats design's §8.2), and
    /// a change to it alone is [`Recorded::Stats`].
    pub weeks_said: BTreeMap<String, WeekSaid>,
    /// Observation: the account this label's readings were last read of, by its key.
    pub key: Option<super::profile::AccountKey>,
    /// Machine-local: when a reading under another key than [`Self::key`] held was recorded
    /// ([`note_reading`], [`Kind::Epoch`]).
    pub keyed_at: Option<i64>,
    /// Observation: each window's figure ([`WindowFig`], [`Self::sample`]).
    pub windows: BTreeMap<String, WindowFig>,
    /// The lab model's canonical account key (`ac-<16hex>`), learned from the roster row a
    /// transaction read ([`super::journal::learn`]): what the journal and the ledger name this
    /// account by beside its label, so its history survives a relabel or a forgotten row.
    pub pk: Option<String>,
}

impl Eq for Account {}

impl PartialEq for Account {
    fn eq(&self, other: &Self) -> bool {
        self.walls == other.walls
            && self.sample == other.sample
            && self.sample_context == other.sample_context
            && self.before == other.before
            && self.offer == other.offer
            && self.auth == other.auth
            && self.auth_at == other.auth_at
            && self.auth_scope == other.auth_scope
            && self.auth_context == other.auth_context
            && self.renewal_metadata == other.renewal_metadata
            && self.served_metadata == other.served_metadata
            && self.cleared_at == other.cleared_at
            && self.room_at == other.room_at
            && self.ran_at == other.ran_at
            && self.credits == other.credits
            && self.credits_day == other.credits_day
            && self.warned_session == other.warned_session
            && self.warned_week == other.warned_week
            && self.weeks_said == other.weeks_said
            && self.key == other.key
            && self.keyed_at == other.keyed_at
            && self.windows == other.windows
            && self.pk == other.pk
    }
}

/// A local auth observation's exact credentials and effective routing policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthContext {
    credential: super::codex_app_login::CredentialContext,
    login: Option<String>,
    expected_email: Option<String>,
}

impl AuthContext {
    pub(crate) fn uses(&self, credential: &super::codex_app_login::CredentialContext) -> bool {
        &self.credential == credential
    }
    pub(crate) fn new(row: &Row, credential: super::codex_app_login::CredentialContext) -> Self {
        Self {
            credential,
            login: row.dir.clone(),
            expected_email: row.expected_identity_email().map(str::to_owned),
        }
    }

    fn matches_row(&self, row: &Row) -> bool {
        self.login == row.dir && self.expected_email.as_deref() == row.expected_identity_email()
    }

    fn to_json(&self) -> Value {
        aterm_json::json!({"credential": self.credential.to_private_wire(), "login": self.login,
            "expected_email": self.expected_email})
    }

    fn from_json(value: &Value) -> Option<Self> {
        let optional = |name| match value.get(name)? {
            Value::Null => Some(None),
            Value::String(text) if text.len() <= MAX_STATE_BYTES as usize => {
                Some(Some(text.clone()))
            }
            _ => None,
        };
        Some(Self {
            credential: super::codex_app_login::CredentialContext::from_private_wire(
                value.get("credential")?.as_str()?,
            )?,
            login: optional("login")?,
            expected_email: optional("expected_email")?,
        })
    }
}

/// Runtime-only evidence available to pure account consumers.
#[derive(Debug, Clone, Default)]
pub enum AuthProjection {
    /// Existing non-Codex observations have no provider snapshot interface.
    #[default]
    Legacy,
    /// No current credential snapshot was observed.
    Unknown,
    /// A read-only snapshot of the effective login.
    Current(AuthContext),
}

impl Account {
    fn clear_auth_context(&mut self) {
        self.auth_context = None;
        self.renewal_metadata = false;
        self.served_metadata = false;
        if !matches!(self.auth_projection, AuthProjection::Legacy) {
            self.auth_projection = AuthProjection::Unknown;
        }
    }

    /// The retained auth word only while its local credential context still applies.
    #[must_use]
    pub fn current_auth(&self, row: &Row) -> Auth {
        match &self.auth_projection {
            AuthProjection::Legacy => self.managed_auth(),
            AuthProjection::Current(current)
                if current.matches_row(row) && self.auth_context.as_ref() == Some(current) =>
            {
                self.auth
            }
            AuthProjection::Unknown | AuthProjection::Current(_) => Auth::Unseen,
        }
    }

    /// Returns whether a reading at `at` comes before the end of the [`KEY_ADOPT_S`] grace that
    /// follows a reading under another key ([`Self::keyed_at`]).
    #[must_use]
    pub fn adopting(&self, at: i64) -> bool {
        self.keyed_at
            .is_some_and(|keyed| at < keyed.saturating_add(KEY_ADOPT_S))
    }

    /// Returns whether any wall, sample, offer, applicable auth word, credits word, warning or
    /// window figure is on record.
    #[must_use]
    pub fn observed(&self) -> bool {
        !self.walls.is_empty()
            || self.sample.is_some()
            || self.offer.is_some()
            || match self.auth_projection {
                AuthProjection::Legacy => self.managed_auth() != Auth::Unseen,
                AuthProjection::Unknown | AuthProjection::Current(_) => self.auth != Auth::Unseen,
            }
            || self.credits.is_some()
            || self.warned_session.is_some()
            || self.warned_week.is_some()
            || !self.windows.is_empty()
    }

    /// Returns whether this account's recorded [`super::profile::AccountKey`] differs from `key`.
    #[must_use]
    pub fn of_another(&self, key: Option<super::profile::AccountKey>) -> bool {
        matches!((self.key, key), (Some(held), Some(key)) if held != key)
    }

    /// When this row last had room on someone's word: the owner's clearing ([`pin`]), or the newest
    /// read that named a window and found none full ([`note_sample`]). Nothing rewrites a rollout,
    /// and a session stopped at a wall writes no later reading, so a full reading older than this
    /// no longer speaks for the row ([`crate::ai::codex_limits::login_look`]). A later read that
    /// finds one full leaves this where it was: it speaks for itself, and only from then on.
    #[must_use]
    pub fn room_since(&self) -> Option<i64> {
        self.cleared_at.max(self.room_at)
    }

    /// When its newest wall on record was observed, whether or not it still stands. A lapsed wall
    /// stays in the record until a read lifts it ([`note_room`]), a clearing or another account's
    /// reading removes it, or a newer wall takes its place ([`MAX_WALLS`]), so this outlasts the wall.
    /// A lifted wall ([`Wall::cleared`]) counts for nothing.
    #[must_use]
    pub fn walled_at(&self) -> Option<i64> {
        self.walls
            .values()
            .filter(|wall| !wall.cleared)
            .map(|wall| wall.at)
            .max()
    }

    /// What its [`Self::sample`] read of the five-hour window and of the week where that read named
    /// no reset, each with the latest it can end ([`Sample::session`], [`Sample::week`]). No
    /// [`WindowFig`] holds such a window ([`sample_fig`]). A word that a reset was used since the
    /// sample, or a clearing since it, leaves nothing of it.
    #[must_use]
    pub fn unnamed_windows(&self, now: i64) -> UnnamedWindows {
        let Some(sample) = self
            .sample
            .as_ref()
            .filter(|sample| self.cleared_at.is_none_or(|cleared| sample.at > cleared))
        else {
            return UnnamedWindows::default();
        };
        let unnamed =
            |reset: Option<i64>, said: Option<&WindowSaid>, figure: Option<(u32, i64)>| {
                let refilled = said.is_some_and(|w| w.pct.is_none() && w.at >= sample.at);
                figure.filter(|_| reset.is_none() && !refilled)
            };
        UnnamedWindows {
            session: unnamed(
                sample.session_reset,
                self.warned_session.as_ref(),
                sample.session(now),
            ),
            week: unnamed(
                sample.week_reset,
                self.warned_week.as_ref(),
                sample.week(now),
            ),
        }
    }

    /// Returns the latest timestamp until which a standing wall blocks this account at `now`.
    #[must_use]
    pub fn limited_until(&self, now: i64) -> Option<i64> {
        self.walls
            .values()
            .filter_map(Wall::stands_until)
            .filter(|end| now < *end)
            .max()
    }

    /// Returns the timestamp until which the 5-hour session window marks this account `low` at
    /// `now`.
    #[must_use]
    pub fn session_low_until(&self, now: i64) -> Option<i64> {
        let refilled = self
            .warned_session
            .as_ref()
            .is_some_and(|w| w.pct.is_none() && self.sample.as_ref().is_none_or(|s| w.at >= s.at));
        let sample = (!refilled)
            .then(|| self.sample.as_ref()?.session(now))
            .flatten()
            .filter(|(pct, _)| *pct >= LOW_SESSION_PCT)
            .map(|(_, end)| end);
        let warned = self
            .warned_session
            .as_ref()
            .and_then(|w| w.figure(SESSION_WINDOW_S, now))
            .filter(|(pct, _)| *pct >= LOW_SESSION_PCT)
            .map(|(_, end)| end);
        sample.max(warned)
    }

    /// Returns the timestamp until which the 7-day weekly window marks this account `low` at `now`.
    #[must_use]
    pub fn week_low_until(&self, now: i64) -> Option<i64> {
        let refilled = self
            .warned_week
            .as_ref()
            .is_some_and(|w| w.pct.is_none() && self.sample.as_ref().is_none_or(|s| w.at >= s.at));
        let sample = (!refilled)
            .then(|| self.sample.as_ref()?.week(now))
            .flatten()
            .filter(|(pct, _)| *pct >= LOW_WEEK_PCT)
            .map(|(_, end)| end);
        let warned = self
            .warned_week
            .as_ref()
            .and_then(|w| w.figure(WEEK_WINDOW_S, now))
            .filter(|(pct, _)| *pct >= LOW_WEEK_PCT)
            .map(|(_, end)| end);
        sample.max(warned)
    }

    /// Returns the timestamp until which active usage credits mark this account `low` at `now`.
    #[must_use]
    pub fn credits_until(&self, now: i64) -> Option<i64> {
        match &self.credits {
            Some(Credits::InUse(c)) if c.stands(now) => Some(c.until_at()),
            _ => None,
        }
    }

    /// Returns the latest timestamp until which this account is `low` at `now`.
    #[must_use]
    pub fn low_until(&self, now: i64) -> Option<i64> {
        [
            self.session_low_until(now),
            self.week_low_until(now),
            self.credits_until(now),
        ]
        .into_iter()
        .flatten()
        .max()
    }

    /// Returns whether this account is `low` at `now`.
    #[must_use]
    pub fn low(&self, now: i64) -> bool {
        self.low_until(now).is_some()
    }

    /// Returns whether Claude Code's own warning that its five-hour or weekly window is filling
    /// ([`Self::warned_session`], [`Self::warned_week`]) still stands at `now`, at any percent:
    /// the first sign a session on it will need another account, well before it is [`Self::low`].
    #[must_use]
    pub fn warned(&self, now: i64) -> bool {
        self.warned_session
            .as_ref()
            .and_then(|w| w.figure(SESSION_WINDOW_S, now))
            .is_some()
            || self
                .warned_week
                .as_ref()
                .and_then(|w| w.figure(WEEK_WINDOW_S, now))
                .is_some()
    }

    /// Returns whether a session on this account should proactively rotate off before hitting a
    /// wall.
    #[must_use]
    pub fn leave_early(&self, now: i64) -> bool {
        self.credits_until(now).is_some()
            || self
                .low_until(now)
                .is_some_and(|end| end.saturating_sub(now) > RESET_SOON_S)
    }

    /// Returns the 7-day weekly reset timestamp for this account if known and in the future at
    /// `now`.
    #[must_use]
    pub fn week_reset(&self, now: i64) -> Option<i64> {
        let refilled = self
            .warned_week
            .as_ref()
            .is_some_and(|w| w.pct.is_none() && self.sample.as_ref().is_none_or(|s| w.at >= s.at));
        let sample = (!refilled)
            .then(|| self.sample.as_ref()?.week_reset)
            .flatten()
            .filter(|reset| *reset > now);
        let warned = self
            .warned_week
            .as_ref()
            .and_then(|w| w.figure(WEEK_WINDOW_S, now))
            .and_then(|(_, _)| self.warned_week.as_ref()?.reset)
            .filter(|reset| *reset > now);
        sample.or(warned)
    }

    /// Returns the timestamp when `window` was cleared after `at` by a scoped reset or operator
    /// pin.
    #[must_use]
    pub fn cleared_since(&self, window: &str, at: i64, now: i64) -> Option<i64> {
        self.cleared_at
            .into_iter()
            .chain(
                self.walls
                    .get(window)
                    .filter(|wall| wall.cleared)
                    .map(|wall| wall.at),
            )
            .map(|cleared| cleared.min(now))
            .filter(|cleared| at < *cleared)
            .max()
    }
}

/// Machine-local record of which account a supervised session is running on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRow {
    /// The account its process's own environment names.
    pub account: String,
    /// When that was last read.
    pub at: i64,
    /// Why it could not move, where a move was refused.
    pub held: Option<String>,
}

/// One stretch of a Claude Code conversation under a login of a row's own directory
/// ([`LoginDirSpans`]), in Unix seconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DirSpan {
    /// From when: a move or a relaunch onto such a login began, or such a process of it started.
    pub since: i64,
    /// Until when, exclusive: the second after the one a process of it on the default login
    /// started in (`off_dir_from`), whose rows may be the process before's, or its record's lapse
    /// ([`LoginDirSpans::lapse`]) where an open stretch lapsed before a later note (`spans_at`).
    /// `None` while a host sees it run under such a login, to its record's lapse.
    pub until: Option<i64>,
}

/// How long an open stretch of a conversation under a login of a row's own directory runs on
/// past its last sighting ([`LoginDirSpans::lapse`]): its process seen there by a host (at its
/// account points, at a move's landing) or found still running by an accounts sync, which saves
/// that before its transcript scan reads the record ([`LoginDirSpans::seen_running`]). One whose
/// process is gone unseen (aterm quit, a crash, a resume in another terminal) is no longer taken
/// to run there an hour on.
pub const LOGIN_DIR_NOTED_S: i64 = 3600;

/// The longest [`NotedProcess::start`] a record keeps.
const NOTED_START_MAX: usize = 64;

/// The process a host saw run a conversation under a login of a row's own directory, as its
/// relaunch snapshot names it (`relaunch::Snapshot`'s `pid` and `start`, the kernel's start in
/// `ps -o lstart=` spelling), so a check that it still runs is not fooled by a reused pid
/// (`restart_stamp::alive_as`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotedProcess {
    pub pid: u32,
    pub start: String,
}

/// Machine-local record of the stretches a Claude Code conversation ran under a login of a row's
/// own directory ([`State::login_dir_spans`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoginDirSpans {
    /// Its stretches, oldest first, each ending before the next begins.
    pub spans: Vec<DirSpan>,
    /// When it was last seen: noted by a host, or its process found running by a sync.
    pub at: i64,
    /// The process a host last saw run its open stretch; `None` where it has none, or where a
    /// launch onto such a login is still to start one.
    pub process: Option<NotedProcess>,
}

impl LoginDirSpans {
    /// Its process found still running at `now` (`alive`), as an accounts sync looks, and saves,
    /// as its transcript scan reads the record ([`LOGIN_DIR_NOTED_S`]): a sighting, so the open stretch
    /// of a turn that runs on for hours with no account point does not lapse under it.
    pub fn seen_running(&mut self, now: i64, alive: impl FnOnce(&NotedProcess) -> bool) {
        if self.spans.iter().any(|span| span.until.is_none())
            && self.process.as_ref().is_some_and(alive)
        {
            self.at = self.at.max(now);
        }
    }

    /// Whether a transcript row written at `at` (Unix seconds) falls in one of its stretches.
    #[must_use]
    pub fn covers(&self, at: i64) -> bool {
        let lapse = self.lapse();
        self.spans
            .iter()
            .any(|span| span.since <= at && at < span.until.unwrap_or(lapse))
    }

    /// Where its open stretch ends unless a host notes it again ([`LOGIN_DIR_NOTED_S`]).
    #[must_use]
    pub fn lapse(&self) -> i64 {
        self.at.saturating_add(LOGIN_DIR_NOTED_S)
    }

    /// Its stretches as they stand at `t`: an open one that lapsed before then ended at its
    /// lapse, so a note at `t` neither joins it nor stretches its end over the hours no host saw.
    fn spans_at(&self, t: i64) -> Vec<DirSpan> {
        let lapse = self.lapse();
        self.spans
            .iter()
            .map(|span| DirSpan {
                since: span.since,
                until: span.until.or((lapse < t).then_some(lapse)),
            })
            .filter(|span| span.until.is_none_or(|until| span.since < until))
            .collect()
    }

    /// What of it still matters at `now`: a wall written in a stretch stands a week at most
    /// ([`WEEK_WINDOW_S`]), so a stretch that ended that long ago cuts no standing wall, and a
    /// record no host noted in that long is of no process that runs. `None` where nothing is left.
    fn kept_at(&self, now: i64) -> Option<Self> {
        if self.at.saturating_add(WEEK_WINDOW_S) <= now {
            return None;
        }
        let spans: Vec<DirSpan> = self
            .spans
            .iter()
            .filter(|span| {
                span.until
                    .is_none_or(|until| until.saturating_add(WEEK_WINDOW_S) > now)
            })
            .copied()
            .collect();
        let open = spans.iter().any(|span| span.until.is_none());
        (!spans.is_empty()).then(|| Self {
            spans,
            at: self.at,
            process: self.process.clone().filter(|_| open),
        })
    }
}

/// `spans` with the stretch from `since` on added: every stretch that has not ended before
/// `since` joins it, and it runs on (`until: None`).
fn spans_opened(spans: &[DirSpan], since: i64) -> Vec<DirSpan> {
    let (mut kept, joined): (Vec<DirSpan>, Vec<DirSpan>) = spans
        .iter()
        .partition(|span| span.until.is_some_and(|until| until < since));
    let since = joined.iter().map(|span| span.since).fold(since, i64::min);
    kept.push(DirSpan { since, until: None });
    kept
}

/// `state`'s conversation records with what no longer matters at `now` gone
/// ([`LoginDirSpans::kept_at`]).
fn login_dir_spans_kept(state: &mut State, now: i64) {
    let spans = std::mem::take(&mut state.login_dir_spans);
    state.login_dir_spans = spans
        .into_iter()
        .filter_map(|(conversation, noted)| Some((conversation, noted.kept_at(now)?)))
        .collect();
}

/// `spans` with nothing from `from` on: a stretch that began by then ends there at the latest,
/// one that began later is dropped.
fn spans_cut(spans: &[DirSpan], from: i64) -> Vec<DirSpan> {
    spans
        .iter()
        .filter(|span| span.since < from)
        .map(|span| DirSpan {
            since: span.since,
            until: Some(span.until.map_or(from, |until| until.min(from))),
        })
        .collect()
}

/// Machine-local claim for an in-flight or recently completed `/usage` probe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeClaim {
    /// The account probed.
    pub account: String,
    /// The session that types it.
    pub sid: String,
    /// When it was claimed.
    pub at: i64,
}

/// Machine-local canary record holding other sessions back until a newly rotated account serves a
/// response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Canary {
    /// The session that moved alone.
    pub sid: String,
    /// When it did: the rotation's time, or the time it took a silent canary's place.
    pub at: i64,
    /// It took the place of a canary that did not answer ([`take_canary`], [`Strike::NotMoved`]).
    pub took: bool,
}

/// Rollback snapshot saved by [`begin_rotation`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Undo {
    /// [`State::previous`] as the rotation found it.
    pub previous: Option<String>,
    /// [`State::previous_proven`] as the rotation found it.
    pub previous_proven: bool,
    /// [`State::canary`] as the rotation found it.
    pub canary: Option<Canary>,
}

/// Machine-local claim for a single limit-reset attempt on a standing wall.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResetClaim {
    /// The account at the wall.
    pub account: String,
    /// The wall's own reset.
    pub until: Option<i64>,
    /// The vendor's raw limit type observed at this claim.
    pub window: Option<String>,
    /// The session that takes the row.
    pub sid: String,
    /// When it was claimed.
    pub at: i64,
    /// The pid of the process the claim was made for.
    pub pid: u32,
    /// That process's kernel start, as the host reads it.
    pub start: String,
    /// The outcome's word once it was recorded ([`reset_outcome`], [`ResetDone::word`]).
    pub done: Option<String>,
}

/// An explicit account selection awaiting a verified login on this machine.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PinRequest {
    label: String,
    nonce: String,
}

/// An explicit request to choose the next eligible account, identified independently of its row.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct NextRequest {
    nonce: String,
}

/// The single pending manual selection; a newer choice replaces either kind.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PendingSelection {
    /// A specific account selected by the person.
    Account(PinRequest),
    /// The next eligible account, selected after admission.
    Next(NextRequest),
}

impl PendingSelection {
    pub(super) fn identity(&self) -> String {
        match self {
            Self::Account(request) => format!("account:{}:{}", request.label, request.nonce),
            Self::Next(request) => format!("next:{}", request.nonce),
        }
    }
    /// The specific-account request, if this selection names one.
    #[must_use]
    pub const fn account(&self) -> Option<&PinRequest> {
        match self {
            Self::Account(request) => Some(request),
            Self::Next(_) => None,
        }
    }

    /// The next-account request, if this selection asks for one.
    #[must_use]
    pub const fn next(&self) -> Option<&NextRequest> {
        match self {
            Self::Next(request) => Some(request),
            Self::Account(_) => None,
        }
    }

    fn from_json(value: &Value) -> Result<Self, Unread> {
        let invalid = || Unread::NotOurs("invalid pending account selection".to_owned());
        let object = value.as_object().ok_or_else(invalid)?;
        let nonce = object
            .get("nonce")
            .and_then(Value::as_str)
            .ok_or_else(invalid)?;
        if !aterm_codec::hex::is_hex(nonce, 32) {
            return Err(invalid());
        }
        if object.len() == 2 && object.get("kind").and_then(Value::as_str) == Some("next") {
            return Ok(Self::Next(NextRequest {
                nonce: nonce.to_owned(),
            }));
        }
        if object.len() == 2
            && let Some(label) = object.get("label").and_then(Value::as_str)
        {
            return PinRequest::new(label, nonce.to_owned())
                .map(Self::Account)
                .ok_or_else(invalid);
        }
        Err(invalid())
    }
}

impl From<PinRequest> for PendingSelection {
    fn from(request: PinRequest) -> Self {
        Self::Account(request)
    }
}

impl PinRequest {
    fn new(label: &str, nonce: String) -> Option<Self> {
        (label_refusal(label).is_none() && aterm_codec::hex::is_hex(&nonce, 32)).then(|| Self {
            label: label.to_owned(),
            nonce,
        })
    }

    /// The canonical account label selected before its login worker started.
    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }
}

/// What this Mac's peers say of its clock, where it limits its claims
/// (`docs/DESIGN-fluid-swarm-2026-10-28.md` F22): a peer's clock that agrees asks nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ClockStanding {
    /// No strict majority of the peers it reads agrees with its clock, and none says it is off
    /// one way: it uses and claims its home accounts only.
    Split,
    /// A strict majority says its clock is off, ahead or behind: it writes no claim row, uses its
    /// home accounts whatever its own view says, and no other account.
    Off {
        /// Its clock reads ahead of theirs.
        ahead: bool,
    },
}

impl ClockStanding {
    /// Its word in the store's `claims_clock` record.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Split => "split",
            Self::Off { .. } => "off",
        }
    }
}

/// This Mac's clock as its last claims read judged it (`claims::touch`, F22), kept until `until`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Clock {
    /// Split or Off.
    pub standing: ClockStanding,
    /// Under Off, at least how far off (seconds); otherwise zero.
    pub by_s: i64,
    /// The decisive readings against its clock.
    pub against: u32,
    /// The decisive readings counted.
    pub of: u32,
    /// The labels of the Macs whose clocks disagree with this one's (Split), for a person to check.
    pub peers: Vec<String>,
    /// Past this instant (Unix seconds) the record says nothing.
    pub until: i64,
}

/// The Mac that holds a roster account, as the account claims settle it
/// (`docs/DESIGN-fluid-swarm-2026-10-28.md` F19, `claims::settle`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountLease {
    /// The holder's `m<N>` label, this Mac's own label or machine id for its own claim, or
    /// `claim:<machine>` for another Mac with no label of its own.
    pub holder: String,
    /// When the holder's claim lapses (Unix seconds).
    pub until: i64,
    /// When the holder's uninterrupted claim began (Unix seconds), which ranks it.
    pub at: i64,
}

/// Persisted accounts state (`state.json`).
#[derive(Debug, Clone)]
pub struct State {
    /// [`SCHEMA`].
    pub schema: u64,
    /// Machine-local: this machine's id ([`Store::machine_id`]).
    pub machine: String,
    /// The accounts, by label.
    pub accounts: BTreeMap<String, Account>,
    /// Transient scoped observations, drained before any publication is compared or saved.
    #[doc(hidden)]
    pub pending_auth: Vec<PendingAuth>,
    /// Runtime store scope; Codex accounts cannot inherit context-free auth authority.
    #[doc(hidden)]
    pub contextual_auth: bool,
    /// Operation-local failed preparations; not authority, history or persisted policy.
    #[doc(hidden)]
    pub excluded_accounts: BTreeSet<String>,
    /// Runtime local read: the rows whose own credential file here shows their login lapsed
    /// ([`Store::hydrate_auth_contexts`], [`crate::ai::connector::stored_login_lapsed`]), which
    /// [`signed_out_here`] reads, so [`condition`] says [`Condition::SignedOut`] and
    /// [`login_state`] [`LoginState::Expired`]; not an observation, history or policy.
    #[doc(hidden)]
    pub lapsed_logins: BTreeSet<String>,
    /// Machine-local: the one account this home runs on.
    pub active: Option<String>,
    /// Machine-local: the account `active` was before the last rotation or pin ([`strike`]).
    pub previous: Option<String>,
    /// Machine-local: whether `previous` had served a response when it was left.
    pub previous_proven: bool,
    /// Machine-local: `active` has served a response since it was chosen.
    pub proven: bool,
    /// Machine-local: the session on an `active` that is not proven yet.
    pub canary: Option<Canary>,
    /// Machine-local: what each rotation that can still be gone back from wrote over, newest last
    /// ([`Undo`], [`MAX_UNDO`]).
    pub undo: Vec<Undo>,
    /// Machine-local: `active` is the owner's word.
    pub pinned: bool,
    /// Machine-local: the one explicit account or next-account selection still awaiting admission.
    pub pending_pin: Option<PendingSelection>,
    /// Machine-local: the supervised sessions, by session id.
    pub sessions: BTreeMap<String, SessionRow>,
    /// Machine-local: when each Claude Code conversation ran under a login of a row's own
    /// directory, by conversation id ([`note_on_login_dir`], [`note_off_login_dir`]). Such a
    /// process keeps the default profile, so the `session_context` of its transcript names the
    /// default login's address whatever login served its turns (measured on Claude Code
    /// 2.1.317, 2026-11-03: a conversation moved onto `lab-c`'s login by `--resume` went on
    /// naming `lab-a`'s): the transcript scan books none of its rows written in those stretches
    /// (`accounts_sync::find_transcript_walls_by_email`).
    pub login_dir_spans: BTreeMap<String, LoginDirSpans>,
    /// Machine-local: when `active` was rotated, the last hour's.
    pub rotations: Vec<i64>,
    /// Machine-local: the probe's claim.
    pub probe: Option<ProbeClaim>,
    /// Machine-local: the reset's claim.
    pub reset: Option<ResetClaim>,
    /// Who holds each account, by label, as the account claims settle it
    /// (`docs/DESIGN-fluid-swarm-2026-10-28.md` F19, `claims::settle`).
    pub leases: BTreeMap<String, AccountLease>,
    /// Account labels for which this Mac is the home Mac (`warm[0]` in `ai-pool.toml`).
    pub home_accounts: BTreeSet<String>,
    /// Verified local credentials whose root grant authorized preparation only.
    /// They cannot dispatch until a separate execution policy releases them.
    pub prepared_accounts: BTreeSet<String>,
    /// Canonical `m<N>` swarm label resolved for this Mac, when known.
    pub swarm_label: Option<String>,
    /// Legacy `m<N>` aliases resolved for this Mac from `aterm-swarm-roster`.
    pub swarm_aliases: Vec<String>,
    /// Machine-local: true while this Mac cannot record its account claims (no Commons writer, the
    /// claims table full or unreadable; `docs/DESIGN-fluid-swarm-2026-10-28.md` F19): it keeps to
    /// its home accounts and the claims it holds.
    pub unrecorded: bool,
    /// Machine-local: when this Mac last claimed each account, by label (a launch, or a running
    /// session's usage tap), within `claims::IDLE_S`: its use, which keeps its claim
    /// (`claims::in_use`).
    pub claimed_at: BTreeMap<String, i64>,
    /// Machine-local: this Mac's clock where its peers read it Split or Off at its last claims
    /// read (`docs/DESIGN-fluid-swarm-2026-10-28.md` F22), until the record's `until`.
    pub clock: Option<Clock>,
    /// Observation: what this Mac had observed of each row it forgot ([`forget_account`]), by
    /// the account's lab key (`ac-<16hex>`), so a row that comes back for the same account under
    /// any label takes its walls, readings and last session again ([`super::journal::learn_with`])
    /// rather than starting blind. The snapshot's byte budget bounds publication.
    pub forgotten: BTreeMap<String, Account>,
}

impl Eq for State {}

/// Every field but the runtime scope (`contextual_auth`, `excluded_accounts`, `lapsed_logins`):
/// a transaction saves its state only when this says it changed. The pattern names each field,
/// so a new one is compared or skipped by a decision here, never left out of a save unseen.
impl PartialEq for State {
    fn eq(&self, other: &Self) -> bool {
        let Self {
            schema,
            machine,
            accounts,
            pending_auth,
            contextual_auth: _,
            excluded_accounts: _,
            lapsed_logins: _,
            active,
            previous,
            previous_proven,
            proven,
            canary,
            undo,
            pinned,
            pending_pin,
            sessions,
            login_dir_spans,
            rotations,
            probe,
            reset,
            leases,
            home_accounts,
            prepared_accounts,
            swarm_label,
            swarm_aliases,
            unrecorded,
            claimed_at,
            clock,
            forgotten,
        } = self;
        *schema == other.schema
            && *machine == other.machine
            && *accounts == other.accounts
            && *pending_auth == other.pending_auth
            && *active == other.active
            && *previous == other.previous
            && *previous_proven == other.previous_proven
            && *proven == other.proven
            && *canary == other.canary
            && *undo == other.undo
            && *pinned == other.pinned
            && *pending_pin == other.pending_pin
            && *sessions == other.sessions
            && *login_dir_spans == other.login_dir_spans
            && *rotations == other.rotations
            && *probe == other.probe
            && *reset == other.reset
            && *leases == other.leases
            && *home_accounts == other.home_accounts
            && *prepared_accounts == other.prepared_accounts
            && *swarm_label == other.swarm_label
            && *swarm_aliases == other.swarm_aliases
            && *unrecorded == other.unrecorded
            && *claimed_at == other.claimed_at
            && *clock == other.clock
            && *forgotten == other.forgotten
    }
}

impl Default for State {
    fn default() -> State {
        State {
            schema: SCHEMA,
            machine: String::new(),
            accounts: BTreeMap::new(),
            pending_auth: Vec::new(),
            contextual_auth: false,
            excluded_accounts: BTreeSet::new(),
            lapsed_logins: BTreeSet::new(),
            active: None,
            previous: None,
            previous_proven: false,
            proven: false,
            canary: None,
            undo: Vec::new(),
            pinned: false,
            pending_pin: None,
            sessions: BTreeMap::new(),
            login_dir_spans: BTreeMap::new(),
            rotations: Vec::new(),
            probe: None,
            reset: None,
            leases: BTreeMap::new(),
            home_accounts: BTreeSet::new(),
            prepared_accounts: BTreeSet::new(),
            swarm_label: None,
            swarm_aliases: Vec::new(),
            unrecorded: false,
            claimed_at: BTreeMap::new(),
            clock: None,
            forgotten: BTreeMap::new(),
        }
    }
}

/// Reason `state.json` or `logins.json` failed deserialization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unread {
    /// It is not JSON: a file cut short.
    Torn(String),
    /// It is JSON and it is not this build's.
    NotOurs(String),
}

impl Unread {
    /// Human-readable description of the parse or validation failure.
    #[must_use]
    pub fn why(&self) -> &str {
        match self {
            Unread::Torn(why) | Unread::NotOurs(why) => why,
        }
    }
}

fn recent(t: i64, now: i64, window: i64) -> bool {
    (0..window).contains(&now.saturating_sub(t))
}

fn int(v: Option<&Value>) -> Option<i64> {
    v.and_then(Value::as_i64)
}

const fn stamp_cap(now: i64) -> i64 {
    now.saturating_add(STAMP_SKEW_S)
}

fn stamp(v: Option<&Value>, cap: i64) -> Option<i64> {
    int(v).map(|at| at.min(cap))
}

fn count(v: Option<&Value>) -> Option<u32> {
    v.and_then(Value::as_u64)
        .map(|n| u32::try_from(n).unwrap_or(u32::MAX))
}

fn pct(v: Option<&Value>) -> Option<u32> {
    count(v).filter(|p| *p <= PCT_MAX)
}

fn word(v: Option<&Value>, max: usize) -> Option<String> {
    v.and_then(Value::as_str)
        .map(|s| one_line(s, max))
        .filter(|s| !s.is_empty())
}

fn label(v: Option<&Value>) -> Option<String> {
    v.and_then(Value::as_str)
        .filter(|s| admissible_label(s))
        .map(str::to_owned)
}

fn wall_json(w: &Wall) -> Value {
    let mut o = Map::new();
    o.insert("until".to_owned(), Value::from(w.until));
    o.insert("at".to_owned(), Value::from(w.at));
    o.insert("by".to_owned(), Value::from(w.by.as_str()));
    o.insert("attempts".to_owned(), Value::from(w.attempts));
    o.insert("cleared".to_owned(), Value::from(w.cleared));
    Value::Object(o)
}

fn wall_of(v: &Value, cap: i64) -> Option<Wall> {
    Some(Wall {
        until: int(v.get("until")),
        at: stamp(v.get("at"), cap)?,
        by: word(v.get("by"), WORD_MAX).unwrap_or_default(),
        attempts: count(v.get("attempts")).unwrap_or(0),
        cleared: v.get("cleared").and_then(Value::as_bool).unwrap_or(false),
    })
}

fn sample_json(s: &Sample) -> Value {
    let mut o = Map::new();
    o.insert("session_pct".to_owned(), Value::from(s.session_pct));
    o.insert("session_reset".to_owned(), Value::from(s.session_reset));
    o.insert("week_pct".to_owned(), Value::from(s.week_pct));
    o.insert("week_reset".to_owned(), Value::from(s.week_reset));
    if let Some(at) = s.session_read_at {
        o.insert("session_read_at".to_owned(), Value::from(at));
    }
    if let Some(at) = s.week_read_at {
        o.insert("week_read_at".to_owned(), Value::from(at));
    }
    o.insert("at".to_owned(), Value::from(s.at));
    o.insert("by".to_owned(), Value::from(s.by.as_str()));
    Value::Object(o)
}

fn sample_of(v: &Value, cap: i64) -> Option<Sample> {
    let at = stamp(v.get("at"), cap)?;
    Some(Sample {
        session_pct: pct(v.get("session_pct")),
        session_reset: int(v.get("session_reset")),
        week_pct: pct(v.get("week_pct")),
        week_reset: int(v.get("week_reset")),
        session_read_at: stamp(v.get("session_read_at"), cap),
        week_read_at: stamp(v.get("week_read_at"), cap),
        at,
        by: word(v.get("by"), WORD_MAX).unwrap_or_default(),
    })
}

fn offer_json(o: &Offer) -> Value {
    let mut m = Map::new();
    if let Some(kind) = o.kind {
        m.insert("kind".to_owned(), Value::from(kind.word()));
    }
    m.insert("left".to_owned(), Value::from(o.left));
    m.insert("use_by".to_owned(), Value::from(o.use_by));
    m.insert("at".to_owned(), Value::from(o.at));
    m.insert("by".to_owned(), Value::from(o.by.as_str()));
    Value::Object(m)
}

fn offer_of(v: &Value, cap: i64) -> Option<Offer> {
    Some(Offer {
        kind: v
            .get("kind")
            .and_then(Value::as_str)
            .and_then(ResetKind::of_word),
        left: count(v.get("left"))?,
        use_by: int(v.get("use_by")),
        at: stamp(v.get("at"), cap)?,
        by: word(v.get("by"), WORD_MAX).unwrap_or_default(),
    })
}

fn canary_json(c: &Canary) -> Value {
    let mut m = Map::new();
    m.insert("sid".to_owned(), Value::from(c.sid.as_str()));
    m.insert("at".to_owned(), Value::from(c.at));
    m.insert("took".to_owned(), Value::from(c.took));
    Value::Object(m)
}

fn canary_of(v: &Value) -> Option<Canary> {
    Some(Canary {
        sid: word(v.get("sid"), WORD_MAX).unwrap_or_default(),
        at: int(v.get("at"))?,
        took: v.get("took").and_then(Value::as_bool).unwrap_or(false),
    })
}

fn undo_json(u: &Undo) -> Value {
    let mut m = Map::new();
    m.insert("previous".to_owned(), Value::from(u.previous.clone()));
    m.insert("previous_proven".to_owned(), Value::from(u.previous_proven));
    m.insert(
        "canary".to_owned(),
        u.canary.as_ref().map_or(Value::Null, canary_json),
    );
    Value::Object(m)
}

fn undo_of(v: &Value) -> Option<Undo> {
    v.is_object().then(|| Undo {
        previous: label(v.get("previous")),
        previous_proven: v
            .get("previous_proven")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        canary: v.get("canary").and_then(canary_of),
    })
}

fn account_json(a: &Account) -> Value {
    let mut o = Map::new();
    o.insert(
        "walls".to_owned(),
        Value::Object(
            a.walls
                .iter()
                .map(|(k, w)| (k.clone(), wall_json(w)))
                .collect(),
        ),
    );
    let sample = |s: &Option<Sample>| s.as_ref().map_or(Value::Null, sample_json);
    o.insert("sample".to_owned(), sample(&a.sample));
    o.insert(
        "sample_context".to_owned(),
        Value::from(a.sample_context.as_ref().map(|context| context.0.as_str())),
    );
    o.insert("before".to_owned(), sample(&a.before));
    o.insert(
        "offer".to_owned(),
        a.offer.as_ref().map_or(Value::Null, offer_json),
    );
    o.insert("auth".to_owned(), Value::from(a.auth.as_str()));
    o.insert("auth_at".to_owned(), Value::from(a.auth_at));
    if a.auth_scope.is_retired() {
        o.insert("auth_retired".to_owned(), Value::Bool(true));
    }
    if let Some(context) = &a.auth_context {
        o.insert("auth_context".to_owned(), context.to_json());
        o.insert(
            "renewal_metadata".to_owned(),
            Value::from(a.renewal_metadata),
        );
        o.insert("served_metadata".to_owned(), Value::from(a.served_metadata));
    }
    o.insert("cleared_at".to_owned(), Value::from(a.cleared_at));
    o.insert("room_at".to_owned(), Value::from(a.room_at));
    o.insert("ran_at".to_owned(), Value::from(a.ran_at));
    o.insert(
        "credits".to_owned(),
        a.credits.as_ref().map_or(Value::Null, credits_json),
    );
    o.insert("credits_day".to_owned(), Value::from(a.credits_day));
    o.insert(
        "warned_session".to_owned(),
        a.warned_session.as_ref().map_or(Value::Null, window_json),
    );
    o.insert(
        "warned_week".to_owned(),
        a.warned_week.as_ref().map_or(Value::Null, window_json),
    );
    if !a.weeks_said.is_empty() {
        o.insert(
            "weeks_said".to_owned(),
            Value::Object(
                a.weeks_said
                    .iter()
                    .map(|(k, w)| (k.clone(), week_said_json(w)))
                    .collect(),
            ),
        );
    }
    o.insert("key".to_owned(), Value::from(a.key.map(|k| k.to_string())));
    o.insert("keyed_at".to_owned(), Value::from(a.keyed_at));
    if let Some(pk) = &a.pk {
        o.insert("pk".to_owned(), Value::from(pk.as_str()));
    }
    o.insert(
        "windows".to_owned(),
        Value::Object(
            a.windows
                .iter()
                .map(|(k, w)| (k.clone(), fig_json(w)))
                .collect(),
        ),
    );
    Value::Object(o)
}

fn fig_json(w: &WindowFig) -> Value {
    let mut m = Map::new();
    m.insert("pct_x10".to_owned(), Value::from(w.pct_x10));
    m.insert("reset".to_owned(), Value::from(w.reset));
    m.insert("at".to_owned(), Value::from(w.at));
    m.insert("by".to_owned(), Value::from(w.by.as_str()));
    m.insert("source".to_owned(), Value::from(w.source.as_str()));
    m.insert("epoch_at".to_owned(), Value::from(w.epoch_at));
    Value::Object(m)
}

fn fig_of(v: &Value, cap: i64) -> Option<WindowFig> {
    Some(WindowFig {
        pct_x10: count(v.get("pct_x10")).filter(|p| *p <= 10 * PCT_MAX)?,
        reset: int(v.get("reset"))?,
        at: stamp(v.get("at"), cap)?,
        by: word(v.get("by"), WORD_MAX).unwrap_or_default(),
        source: word(v.get("source"), WORD_MAX).unwrap_or_default(),
        epoch_at: stamp(v.get("epoch_at"), cap),
    })
}

fn credits_json(c: &Credits) -> Value {
    let mut m = Map::new();
    match c {
        Credits::InUse(c) => {
            m.insert("word".to_owned(), Value::from("in-use"));
            m.insert("since".to_owned(), Value::from(c.since));
            m.insert("seen".to_owned(), Value::from(c.seen));
            m.insert("until".to_owned(), Value::from(c.until));
            m.insert("told".to_owned(), Value::from(c.told));
        }
        Credits::Off(off) => {
            m.insert("word".to_owned(), Value::from("off"));
            m.insert("reason".to_owned(), Value::from(off.reason.as_str()));
            m.insert("at".to_owned(), Value::from(off.at));
            m.insert("until".to_owned(), Value::from(off.until));
        }
        Credits::On(at) => {
            m.insert("word".to_owned(), Value::from("on"));
            m.insert("at".to_owned(), Value::from(*at));
        }
    }
    Value::Object(m)
}

fn credits_of(v: &Value, cap: i64) -> Option<Credits> {
    match v.get("word").and_then(Value::as_str)? {
        "in-use" => Some(Credits::InUse(CreditsUse {
            since: stamp(v.get("since"), cap)?,
            seen: stamp(v.get("seen"), cap)?,
            until: int(v.get("until")),
            told: v.get("told").and_then(Value::as_bool).unwrap_or(false),
        })),
        "off" => Some(Credits::Off(CreditsOff {
            reason: word(v.get("reason"), WORD_MAX).unwrap_or_default(),
            at: stamp(v.get("at"), cap)?,
            until: int(v.get("until")),
        })),
        "on" => Some(Credits::On(stamp(v.get("at"), cap)?)),
        _ => None,
    }
}

fn window_json(w: &WindowSaid) -> Value {
    let mut m = Map::new();
    m.insert("pct".to_owned(), Value::from(w.pct));
    m.insert("reset".to_owned(), Value::from(w.reset));
    m.insert("at".to_owned(), Value::from(w.at));
    Value::Object(m)
}

fn window_of(v: &Value, cap: i64) -> Option<WindowSaid> {
    Some(WindowSaid {
        pct: pct(v.get("pct")),
        reset: int(v.get("reset")),
        at: stamp(v.get("at"), cap)?,
    })
}

fn week_said_json(w: &WeekSaid) -> Value {
    let mut m = Map::new();
    m.insert("pct".to_owned(), Value::from(w.pct));
    m.insert("reset".to_owned(), Value::from(w.reset));
    m.insert("at".to_owned(), Value::from(w.at));
    m.insert("by".to_owned(), Value::from(w.by.as_str()));
    m.insert("wall".to_owned(), Value::from(w.wall));
    m.insert("ends_by".to_owned(), Value::from(w.ends_by));
    Value::Object(m)
}

/// A [`WeekSaid`] as [`week_said_json`] wrote it; a figure out of range is no entry, where a
/// `null` one is a limit reset's.
fn week_said_of(v: &Value, cap: i64) -> Option<WeekSaid> {
    let pct = match v.get("pct") {
        None | Some(Value::Null) => None,
        figure => Some(pct(figure)?),
    };
    Some(WeekSaid {
        pct,
        reset: int(v.get("reset")),
        at: stamp(v.get("at"), cap)?,
        by: word(v.get("by"), WORD_MAX).unwrap_or_default(),
        wall: v.get("wall").and_then(Value::as_bool).unwrap_or(false),
        ends_by: int(v.get("ends_by")),
    })
}

fn account_of(v: &Value, now: i64) -> Account {
    let cap = stamp_cap(now);
    let mut walls: Vec<(String, Wall)> = v
        .get("walls")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .filter_map(|(k, w)| {
            Some((
                Some(one_line(k, WORD_MAX)).filter(|k| !k.is_empty())?,
                wall_of(w, cap)?,
            ))
        })
        .collect();
    walls.sort_by_key(|(_, w)| std::cmp::Reverse(w.at));
    walls.truncate(MAX_WALLS);
    let mut windows: Vec<(String, WindowFig)> = v
        .get("windows")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .filter_map(|(k, w)| {
            Some((
                Some(one_line(k, WORD_MAX)).filter(|k| !k.is_empty())?,
                fig_of(w, cap)?,
            ))
        })
        .collect();
    windows.sort_by_key(|(_, w)| std::cmp::Reverse(w.at));
    windows.truncate(MAX_WINDOWS);
    let auth_context = v.get("auth_context").and_then(AuthContext::from_json);
    let renewal_metadata = auth_context.is_some()
        && v.get("renewal_metadata")
            .and_then(Value::as_bool)
            .unwrap_or(false);
    let served_metadata = auth_context.is_some()
        && v.get("served_metadata")
            .and_then(Value::as_bool)
            .unwrap_or(false);
    Account {
        key: v
            .get("key")
            .and_then(Value::as_str)
            .and_then(super::profile::AccountKey::parse),
        keyed_at: stamp(v.get("keyed_at"), now),
        pk: v
            .get("pk")
            .and_then(Value::as_str)
            .filter(|pk| super::journal::Key::new(pk).is_some())
            .map(str::to_owned),
        windows: windows.into_iter().collect(),
        walls: walls.into_iter().collect(),
        sample: v.get("sample").and_then(|s| sample_of(s, cap)),
        sample_context: v
            .get("sample_context")
            .and_then(Value::as_str)
            .and_then(SampleContext::parse),
        before: v.get("before").and_then(|s| sample_of(s, cap)),
        offer: v.get("offer").and_then(|o| offer_of(o, cap)),
        auth: v
            .get("auth")
            .and_then(Value::as_str)
            .map_or(Auth::Unseen, Auth::parse),
        auth_at: stamp(v.get("auth_at"), now),
        auth_scope: ManagedScope::from_retired(
            v.get("auth_retired")
                .is_some_and(|value| value.as_bool() != Some(false)),
        ),
        auth_context,
        renewal_metadata,
        served_metadata,
        auth_projection: AuthProjection::Legacy,
        cleared_at: stamp(v.get("cleared_at"), now),
        room_at: stamp(v.get("room_at"), now),
        ran_at: stamp(v.get("ran_at"), now),
        credits: v.get("credits").and_then(|c| credits_of(c, cap)),
        credits_day: int(v.get("credits_day")),
        warned_session: v.get("warned_session").and_then(|w| window_of(w, cap)),
        warned_week: v.get("warned_week").and_then(|w| window_of(w, cap)),
        weeks_said: v
            .get("weeks_said")
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
            .filter(|(k, _)| week_model(k).is_some())
            .filter_map(|(k, w)| Some((k.clone(), week_said_of(w, cap)?)))
            .collect(),
    }
}

impl State {
    /// Serializes the state as a single-line sorted JSON object.
    #[must_use]
    pub fn to_json(&self) -> String {
        let mut o = Map::new();
        o.insert(
            "schema".to_owned(),
            Value::from(
                if self.schema == SCHEMA && !self.prepared_accounts.is_empty() {
                    PREPARED_STATE_SCHEMA
                } else {
                    self.schema
                },
            ),
        );
        o.insert("machine".to_owned(), Value::from(self.machine.as_str()));
        o.insert(
            "accounts".to_owned(),
            Value::Object(
                self.accounts
                    .iter()
                    .map(|(k, a)| (k.clone(), account_json(a)))
                    .collect(),
            ),
        );
        o.insert("active".to_owned(), Value::from(self.active.clone()));
        o.insert("previous".to_owned(), Value::from(self.previous.clone()));
        o.insert(
            "previous_proven".to_owned(),
            Value::from(self.previous_proven),
        );
        o.insert("proven".to_owned(), Value::from(self.proven));
        o.insert(
            "canary".to_owned(),
            self.canary.as_ref().map_or(Value::Null, canary_json),
        );
        o.insert(
            "undo".to_owned(),
            Value::Array(self.undo.iter().map(undo_json).collect()),
        );
        o.insert("pinned".to_owned(), Value::from(self.pinned));
        if let Some(request) = &self.pending_pin {
            let mut pending = Map::new();
            let nonce = match request {
                PendingSelection::Account(request) => {
                    pending.insert("label".to_owned(), Value::from(request.label.as_str()));
                    &request.nonce
                }
                PendingSelection::Next(request) => {
                    pending.insert("kind".to_owned(), Value::from("next"));
                    &request.nonce
                }
            };
            pending.insert("nonce".to_owned(), Value::from(nonce.as_str()));
            o.insert("pending_pin".to_owned(), Value::Object(pending));
        }
        o.insert(
            "sessions".to_owned(),
            Value::Object(
                self.sessions
                    .iter()
                    .map(|(sid, s)| {
                        let mut row = Map::new();
                        row.insert("account".to_owned(), Value::from(s.account.as_str()));
                        row.insert("at".to_owned(), Value::from(s.at));
                        row.insert("held".to_owned(), Value::from(s.held.clone()));
                        (sid.clone(), Value::Object(row))
                    })
                    .collect(),
            ),
        );
        if !self.login_dir_spans.is_empty() {
            o.insert(
                "login_dir_spans".to_owned(),
                Value::Object(
                    self.login_dir_spans
                        .iter()
                        .map(|(conversation, noted)| {
                            let spans = noted
                                .spans
                                .iter()
                                .map(|span| {
                                    let mut row = Map::new();
                                    row.insert("since".to_owned(), Value::from(span.since));
                                    row.insert(
                                        "until".to_owned(),
                                        span.until.map_or(Value::Null, Value::from),
                                    );
                                    Value::Object(row)
                                })
                                .collect();
                            let mut row = Map::new();
                            row.insert("spans".to_owned(), Value::Array(spans));
                            row.insert("at".to_owned(), Value::from(noted.at));
                            if let Some(process) = &noted.process {
                                let mut seen = Map::new();
                                seen.insert("pid".to_owned(), Value::from(process.pid));
                                seen.insert("start".to_owned(), Value::from(process.start.clone()));
                                row.insert("process".to_owned(), Value::Object(seen));
                            }
                            (conversation.clone(), Value::Object(row))
                        })
                        .collect(),
                ),
            );
        }
        o.insert(
            "rotations".to_owned(),
            Value::Array(self.rotations.iter().map(|t| Value::from(*t)).collect()),
        );
        o.insert(
            "probe".to_owned(),
            self.probe.as_ref().map_or(Value::Null, |c| {
                let mut m = Map::new();
                m.insert("account".to_owned(), Value::from(c.account.as_str()));
                m.insert("sid".to_owned(), Value::from(c.sid.as_str()));
                m.insert("at".to_owned(), Value::from(c.at));
                Value::Object(m)
            }),
        );
        o.insert(
            "reset".to_owned(),
            self.reset.as_ref().map_or(Value::Null, |c| {
                let mut m = Map::new();
                m.insert("account".to_owned(), Value::from(c.account.as_str()));
                m.insert("until".to_owned(), Value::from(c.until));
                m.insert("window".to_owned(), Value::from(c.window.clone()));
                m.insert("sid".to_owned(), Value::from(c.sid.as_str()));
                m.insert("at".to_owned(), Value::from(c.at));
                m.insert("pid".to_owned(), Value::from(c.pid));
                m.insert("start".to_owned(), Value::from(c.start.as_str()));
                m.insert("done".to_owned(), Value::from(c.done.clone()));
                Value::Object(m)
            }),
        );
        if !self.leases.is_empty() {
            o.insert(
                "leases".to_owned(),
                Value::Object(
                    self.leases
                        .iter()
                        .map(|(label, lease)| {
                            let mut m = Map::new();
                            m.insert("holder".to_owned(), Value::from(lease.holder.as_str()));
                            m.insert("until".to_owned(), Value::from(lease.until));
                            m.insert("at".to_owned(), Value::from(lease.at));
                            (label.clone(), Value::Object(m))
                        })
                        .collect(),
                ),
            );
        }
        if !self.home_accounts.is_empty() {
            o.insert(
                "home_accounts".to_owned(),
                Value::Array(
                    self.home_accounts
                        .iter()
                        .map(|l| Value::from(l.as_str()))
                        .collect(),
                ),
            );
        }
        if !self.prepared_accounts.is_empty() {
            o.insert(
                "prepared_accounts".into(),
                Value::Array(
                    self.prepared_accounts
                        .iter()
                        .map(|label| Value::from(label.as_str()))
                        .collect(),
                ),
            );
        }
        if let Some(swarm_label) = &self.swarm_label {
            o.insert("swarm_label".to_owned(), Value::from(swarm_label.as_str()));
        }
        if !self.swarm_aliases.is_empty() {
            o.insert(
                "swarm_aliases".to_owned(),
                Value::Array(
                    self.swarm_aliases
                        .iter()
                        .map(|a| Value::from(a.as_str()))
                        .collect(),
                ),
            );
        }
        if self.unrecorded {
            o.insert("claims_unrecorded".to_owned(), Value::from(true));
        }
        if !self.claimed_at.is_empty() {
            o.insert(
                "claimed_at".to_owned(),
                Value::Object(
                    self.claimed_at
                        .iter()
                        .map(|(label, at)| (label.clone(), Value::from(*at)))
                        .collect(),
                ),
            );
        }
        if let Some(clock) = &self.clock {
            let mut c = Map::new();
            c.insert("standing".to_owned(), Value::from(clock.standing.word()));
            if let ClockStanding::Off { ahead } = clock.standing {
                c.insert("ahead".to_owned(), Value::from(ahead));
            }
            c.insert("by_s".to_owned(), Value::from(clock.by_s));
            c.insert("against".to_owned(), Value::from(clock.against));
            c.insert("of".to_owned(), Value::from(clock.of));
            c.insert(
                "peers".to_owned(),
                Value::Array(
                    clock
                        .peers
                        .iter()
                        .map(|p| Value::from(p.as_str()))
                        .collect(),
                ),
            );
            c.insert("until".to_owned(), Value::from(clock.until));
            o.insert("claims_clock".to_owned(), Value::Object(c));
        }
        if !self.forgotten.is_empty() {
            o.insert(
                "forgotten".to_owned(),
                Value::Object(
                    self.forgotten
                        .iter()
                        .map(|(pk, a)| (pk.clone(), account_json(a)))
                        .collect(),
                ),
            );
        }
        aterm_json::to_string(&Value::Object(o)).unwrap_or_else(|_| String::from("{}"))
    }

    /// Deserializes and sanitizes `state.json` against `now`.
    /// # Errors
    ///
    /// [`Unread::Torn`]: not JSON, including a file cut short.
    /// [`Unread::NotOurs`]: JSON but not an object, or naming no schema or one other than
    /// [`SCHEMA`] or [`PREPARED_STATE_SCHEMA`], or a malformed or unknown pending selection:
    /// a file this build must not guess at or write over.
    pub fn from_json(text: &str, now: i64) -> Result<State, Unread> {
        let doc: Value = aterm_json::from_str(text)
            .map_err(|e| Unread::Torn(format!("not JSON ({})", shown(&e.to_string()))))?;
        if !doc.is_object() {
            return Err(Unread::NotOurs("not a JSON object".to_string()));
        }
        let stored_schema = doc.get("schema").and_then(Value::as_u64);
        match stored_schema {
            Some(SCHEMA | PREPARED_STATE_SCHEMA) => {}
            Some(other) => {
                return Err(Unread::NotOurs(format!(
                    "schema {other}; this build reads schemas {SCHEMA} and {PREPARED_STATE_SCHEMA}"
                )));
            }
            None => return Err(Unread::NotOurs("names no schema".to_string())),
        }
        let accounts: BTreeMap<String, Account> = doc
            .get("accounts")
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
            .filter(|(k, v)| admissible_label(k) && v.is_object())
            .map(|(k, v)| (k.clone(), account_of(v, now)))
            .collect();
        let sessions: Vec<(String, SessionRow)> = doc
            .get("sessions")
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
            .filter_map(|(sid, v)| {
                Some((
                    Some(one_line(sid, WORD_MAX)).filter(|sid| !sid.is_empty())?,
                    SessionRow {
                        account: label(v.get("account"))?,
                        at: int(v.get("at"))?,
                        held: word(v.get("held"), WORD_MAX),
                    },
                ))
            })
            .collect();
        let login_dir_spans: Vec<(String, LoginDirSpans)> = doc
            .get("login_dir_spans")
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
            .filter(|(conversation, _)| super::upgrade::is_session_id(conversation))
            .filter_map(|(conversation, v)| {
                let mut spans = Vec::new();
                for span in v.get("spans")?.as_array()? {
                    spans = spans_opened(&spans, int(span.get("since"))?);
                    if let Some(until) = span.get("until").filter(|until| !until.is_null()) {
                        spans = spans_cut(&spans, int(Some(until))?);
                    }
                }
                let process = v.get("process").and_then(|seen| {
                    Some(NotedProcess {
                        pid: u32::try_from(int(seen.get("pid"))?).ok()?,
                        start: seen
                            .get("start")?
                            .as_str()
                            .filter(|start| {
                                !start.is_empty()
                                    && start.len() <= NOTED_START_MAX
                                    && !start.chars().any(char::is_control)
                            })?
                            .to_owned(),
                    })
                });
                let noted = LoginDirSpans {
                    spans,
                    at: int(v.get("at"))?,
                    process,
                };
                Some((conversation.clone(), noted.kept_at(now)?))
            })
            .collect();
        let mut rotations: Vec<i64> = doc
            .get("rotations")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_i64)
            .collect();
        rotations.sort_unstable();
        let probe = doc.get("probe").and_then(|c| {
            Some(ProbeClaim {
                account: label(c.get("account"))?,
                sid: word(c.get("sid"), WORD_MAX)?,
                at: int(c.get("at"))?,
            })
        });
        let reset = doc.get("reset").and_then(|c| {
            Some(ResetClaim {
                account: label(c.get("account"))?,
                until: int(c.get("until")),
                window: word(c.get("window"), WORD_MAX),
                sid: word(c.get("sid"), WORD_MAX)?,
                at: int(c.get("at"))?,
                pid: count(c.get("pid")).unwrap_or(0),
                start: word(c.get("start"), WORD_MAX).unwrap_or_default(),
                done: word(c.get("done"), WORD_MAX),
            })
        });
        let canary = doc.get("canary").and_then(canary_of);
        let made = canary.as_ref().is_some_and(|c| !c.took);
        let mut undo: Vec<Undo> = doc
            .get("undo")
            .and_then(Value::as_array)
            .filter(|_| made)
            .into_iter()
            .flatten()
            .filter_map(undo_of)
            .collect();
        let extra = undo.len().saturating_sub(MAX_UNDO);
        undo.drain(..extra);
        let leases: BTreeMap<String, AccountLease> = doc
            .get("leases")
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
            .filter(|(k, v)| admissible_label(k) && v.is_object())
            .filter_map(|(k, v)| {
                Some((
                    k.clone(),
                    AccountLease {
                        holder: word(v.get("holder"), WORD_MAX)?,
                        until: int(v.get("until"))?,
                        at: int(v.get("at"))?,
                    },
                ))
            })
            .collect();
        let home_accounts: BTreeSet<String> = doc
            .get("home_accounts")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .filter(|s| admissible_label(s))
            .map(str::to_owned)
            .collect();
        let swarm_label = word(doc.get("swarm_label"), WORD_MAX);
        let mut prepared_accounts = BTreeSet::new();
        if let Some(value) = doc.get("prepared_accounts") {
            let labels = value.as_array().ok_or_else(|| {
                Unread::NotOurs("invalid prepared-account execution holds".into())
            })?;
            for value in labels {
                let label = value
                    .as_str()
                    .filter(|label| admissible_label(label))
                    .ok_or_else(|| Unread::NotOurs("invalid prepared-account label".into()))?;
                if !prepared_accounts.insert(label.to_owned()) {
                    return Err(Unread::NotOurs("duplicate prepared-account hold".into()));
                }
            }
        }
        if (!prepared_accounts.is_empty()) != (stored_schema == Some(PREPARED_STATE_SCHEMA)) {
            return Err(Unread::NotOurs(
                "prepared-account holds require their guarded state schema".into(),
            ));
        }
        let mut swarm_aliases: Vec<String> = doc
            .get("swarm_aliases")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|v| word(Some(v), WORD_MAX))
            .take(MAX_PEER_NAMES)
            .collect();
        swarm_aliases.sort();
        swarm_aliases.dedup();
        let unrecorded = doc
            .get("claims_unrecorded")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let claimed_at: BTreeMap<String, i64> = doc
            .get("claimed_at")
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
            .filter(|(label, _)| admissible_label(label))
            .filter_map(|(label, at)| Some((label.clone(), at.as_i64()?)))
            .collect();
        // A record past its `until` says nothing, so it is not kept.
        let clock = doc.get("claims_clock").and_then(|c| {
            let standing = match c.get("standing")?.as_str()? {
                "split" => ClockStanding::Split,
                "off" => ClockStanding::Off {
                    ahead: c.get("ahead")?.as_bool()?,
                },
                _ => return None,
            };
            Some(Clock {
                standing,
                by_s: int(c.get("by_s"))?.max(0),
                against: u32::try_from(c.get("against")?.as_u64()?).ok()?,
                of: u32::try_from(c.get("of")?.as_u64()?).ok()?,
                peers: c
                    .get("peers")?
                    .as_array()?
                    .iter()
                    .filter_map(|p| word(Some(p), WORD_MAX))
                    .take(MAX_PEER_NAMES)
                    .collect(),
                until: int(c.get("until"))?,
            })
            .filter(|clock| clock.until > now)
        });
        let forgotten: BTreeMap<String, Account> = doc
            .get("forgotten")
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
            .filter(|(pk, v)| super::journal::Key::new(pk).is_some() && v.is_object())
            .map(|(pk, v)| {
                let mut account = account_of(v, now);
                account.pk = Some(pk.clone());
                (pk.clone(), account)
            })
            .collect();
        Ok(State {
            schema: SCHEMA,
            machine: doc
                .get("machine")
                .and_then(Value::as_str)
                .filter(|m| {
                    !m.is_empty() && m.len() <= WORD_MAX && aterm_codec::hex::is_hex(m, m.len())
                })
                .map(str::to_owned)
                .unwrap_or_default(),
            accounts,
            pending_auth: Vec::new(),
            contextual_auth: false,
            excluded_accounts: BTreeSet::new(),
            lapsed_logins: BTreeSet::new(),
            active: label(doc.get("active")),
            previous: label(doc.get("previous")),
            previous_proven: doc
                .get("previous_proven")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            proven: doc.get("proven").and_then(Value::as_bool).unwrap_or(false),
            canary,
            undo,
            pinned: doc.get("pinned").and_then(Value::as_bool).unwrap_or(false),
            pending_pin: doc
                .get("pending_pin")
                .map(PendingSelection::from_json)
                .transpose()?,
            sessions: sessions.into_iter().collect(),
            login_dir_spans: login_dir_spans.into_iter().collect(),
            rotations,
            probe,
            reset,
            leases,
            home_accounts,
            prepared_accounts,
            swarm_label,
            swarm_aliases,
            unrecorded,
            claimed_at,
            clock,
            forgotten,
        })
    }

    /// True when `candidate` names this Mac's swarm label, one of its roster aliases, or its
    /// local store `machine` id.
    #[must_use]
    pub fn owns_swarm_label(&self, candidate: &str) -> bool {
        let trimmed = candidate.trim_ascii();
        if trimmed.is_empty() {
            return false;
        }
        if !self.machine.is_empty() && trimmed == self.machine {
            return true;
        }
        if let Some((_, canon)) = super::ai_pool::leading_m_label(trimmed) {
            if self.swarm_label.as_deref() == Some(canon.as_str()) {
                return true;
            }
            if self.swarm_aliases.iter().any(|a| a == &canon) {
                return true;
            }
        }
        self.swarm_label
            .as_deref()
            .is_some_and(|lbl| trimmed.eq_ignore_ascii_case(lbl))
            || self
                .swarm_aliases
                .iter()
                .any(|a| trimmed.eq_ignore_ascii_case(a))
    }

    /// Checks preparation-only authority and operation-local preparation failures on `label`.
    /// Other admission rules, including leases, are evaluated separately.
    ///
    /// # Errors
    ///
    /// [`WhyNot::PreparationOnly`] when the credential is still held for preparation only,
    /// or [`WhyNot::PreparationFailed`] when this operation already failed to prepare it.
    pub fn execution_verdict(&self, label: &str) -> Result<(), WhyNot> {
        if self.prepared_accounts.contains(label) {
            return Err(WhyNot::PreparationOnly);
        }
        if self.excluded_accounts.contains(label) {
            return Err(WhyNot::PreparationFailed);
        }
        Ok(())
    }

    /// Evaluates whether this Mac may use `label` at `now`, as the account claims settle it
    /// (`docs/DESIGN-fluid-swarm-2026-10-28.md` F19).
    ///
    /// # Errors
    ///
    /// - [`WhyNot::PreparationOnly`] when a shared credential has no separate execution grant.
    /// - [`WhyNot::PreparationFailed`] when this operation could not prepare the account.
    /// - [`WhyNot::LeasedElsewhere`] when another Mac's live claim on `label` is the senior one.
    /// - [`WhyNot::ClockOff`] when this Mac's peers read its clock Off and `label` is not one of
    ///   its home accounts, or Split and `label` is not one of its home accounts
    ///   (`docs/DESIGN-fluid-swarm-2026-10-28.md` F22). Under Off a home account is `Ok` whatever
    ///   the claims this Mac sees: each peer whose reading counted it Off holds its home rows for
    ///   it, so none of those takes one anew; a peer with no direct round, one whose clock sits
    ///   with this Mac's wrong one, and a lease taken before it went Off (until that peer's next
    ///   turn end) are not covered (the design's §4.2, F22 items 4, 6 and 8). Under Split a home
    ///   account follows the rules below.
    /// - [`WhyNot::Unclaimed`] when this Mac cannot record its claims (`self.unrecorded`) and
    ///   neither holds a live claim on `label` nor is `label`'s home Mac (`warm[0]`).
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AccountClaimClock",
            action = "Verdict",
            project = "aterm_agent::harness::claims::tests::claim_clock_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AccountClaimClock",
            action = "Launch",
            project = "aterm_agent::harness::claims::tests::claim_clock_transition"
        )
    )]
    pub fn lease_verdict(&self, label: &str, now: i64) -> Result<(), WhyNot> {
        self.execution_verdict(label)?;
        if let Some(clock) = self.clock_at(now) {
            let home = self.home_accounts.contains(label);
            match clock.standing {
                ClockStanding::Off { .. } if home => return Ok(()),
                ClockStanding::Off { .. } => return Err(WhyNot::ClockOff),
                ClockStanding::Split if !home => return Err(WhyNot::ClockOff),
                ClockStanding::Split => {}
            }
        }
        if let Some(lease) = self.leases.get(label)
            && lease.until > now
        {
            return if self.owns_swarm_label(&lease.holder) {
                Ok(())
            } else {
                Err(WhyNot::LeasedElsewhere { until: lease.until })
            };
        }
        if self.unrecorded && !self.home_accounts.contains(label) {
            return Err(WhyNot::Unclaimed);
        }
        Ok(())
    }

    /// This Mac's clock record where it still counts at `now` ([`State::clock`]).
    #[must_use]
    pub fn clock_at(&self, now: i64) -> Option<&Clock> {
        self.clock.as_ref().filter(|clock| clock.until > now)
    }

    /// Records or renews a lease on `label` held by `holder` until `until`.
    pub fn note_lease(&mut self, label: &str, holder: &str, until: i64, at: i64) {
        if !admissible_label(label) || holder.trim_ascii().is_empty() {
            return;
        }
        self.leases.insert(
            label.to_owned(),
            AccountLease {
                holder: holder.trim_ascii().to_owned(),
                until,
                at,
            },
        );
    }

    /// Removes any recorded lease for `label`.
    pub fn clear_lease(&mut self, label: &str) {
        self.leases.remove(label);
    }

    /// Returns the [`Account`] record for `label`, or a default empty record if unseen.
    #[must_use]
    pub fn account(&self, label: &str) -> Account {
        self.accounts.get(label).cloned().unwrap_or_default()
    }

    /// Returns the timestamp when the active canary hold began.
    #[must_use]
    pub fn canary_since(&self) -> Option<i64> {
        (self.active.is_some() && !self.proven)
            .then(|| self.canary.as_ref().map(|c| c.at))
            .flatten()
    }

    /// Returns whether an unproven canary hold is currently active at `now`.
    #[must_use]
    pub fn canary_holds(&self, now: i64) -> bool {
        self.canary_since()
            .is_some_and(|at| recent(at, now, CANARY_HOLD_S))
    }

    /// Returns the timestamp when the active canary hold expires at `now`.
    #[must_use]
    pub fn canary_until(&self, now: i64) -> Option<i64> {
        self.canary_since()
            .filter(|_| self.canary_holds(now))
            .map(|since| since.saturating_add(CANARY_HOLD_S))
    }

    /// Returns the session IDs currently recorded on `label` in sorted order.
    #[must_use]
    pub fn sessions_on(&self, label: &str) -> Vec<&str> {
        self.sessions
            .iter()
            .filter(|(_, s)| s.account == label)
            .map(|(sid, _)| sid.as_str())
            .collect()
    }
}

/// Generates a fresh 16-hex-char machine identifier from OS entropy.
/// # Errors
///
/// No OS entropy source answered.
pub fn mint_machine() -> io::Result<String> {
    aterm_uds::rand::hex_token::<8>()
}

/// Derived operational condition of a roster account.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Condition {
    /// No login was made for it on this machine ([`login_made`]).
    NotEnrolled,
    /// It is signed out here ([`signed_out_here`]): its login's last word on record is
    /// [`Auth::SignedOut`] ([`strike`]), or its own credential file here shows it lapsed
    /// ([`State::lapsed_logins`]).
    SignedOut,
    /// A wall stands, until this instant.
    Limited {
        /// When its last standing wall ends.
        until: i64,
    },
    /// A window still running is at its threshold, however near its reset.
    Low,
    /// Observed, and none of the above.
    Usable,
    /// Never observed.
    Unseen,
}

impl Condition {
    /// Canonical condition word for reports and CLI output.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Condition::NotEnrolled => "not-enrolled",
            Condition::SignedOut => "signed-out",
            Condition::Limited { .. } => "limited",
            Condition::Low => "low",
            Condition::Usable => "usable",
            Condition::Unseen => "unseen",
        }
    }

    /// Returns whether the account has fresh budget (`Usable` or `Unseen`).
    #[must_use]
    pub const fn fresh(self) -> bool {
        matches!(self, Condition::Usable | Condition::Unseen)
    }

    /// Returns whether a session can run on the account now (`Usable`, `Unseen` or `Low`).
    #[must_use]
    pub fn runs(self) -> bool {
        self.fresh() || self == Condition::Low
    }
}

/// Computes the [`Condition`] of `row` at `now` given `state` and whether its login is `enrolled`:
/// an enrolled row signed out here ([`signed_out_here`], the rule [`login_state`] reads as
/// [`LoginState::Expired`]) is [`Condition::SignedOut`], a lapsed credential file whatever its
/// records say, so no launch picks it.
#[must_use]
pub fn condition(row: &Row, state: &State, enrolled: bool, now: i64) -> Condition {
    if !enrolled {
        return Condition::NotEnrolled;
    }
    if signed_out_here(row, state) {
        return Condition::SignedOut;
    }
    let Some(account) = state.accounts.get(&row.label) else {
        return Condition::Unseen;
    };
    if let Some(until) = account.limited_until(now) {
        return Condition::Limited { until };
    }
    if account.low(now) {
        return Condition::Low;
    }
    if !account.walls.is_empty()
        || account.sample.is_some()
        || account.offer.is_some()
        || account.current_auth(row) != Auth::Unseen
        || account.credits.is_some()
        || account.warned_session.is_some()
        || account.warned_week.is_some()
        || !account.windows.is_empty()
    {
        Condition::Usable
    } else {
        Condition::Unseen
    }
}

/// Returns the labels of accounts in [`Condition::SignedOut`] in roster order.
#[must_use]
pub fn signed_out(
    roster: &Roster,
    state: &State,
    enrolled: &BTreeSet<String>,
    now: i64,
) -> Vec<String> {
    roster
        .rows
        .iter()
        .filter(|row| {
            row.held_by.is_none()
                && row.email.is_none()
                && condition(row, state, is_enrolled(row, enrolled), now) == Condition::SignedOut
        })
        .map(|row| row.label.clone())
        .collect()
}

/// Returns the first signed-out roster row that would give a session on `me` a usable target if
/// signed in.
#[must_use]
pub fn sign_in_next(
    roster: &Roster,
    state: &State,
    enrolled: &BTreeSet<String>,
    me: Option<&str>,
    now: i64,
) -> Option<(String, bool)> {
    roster
        .rows
        .iter()
        .filter(|row| {
            row.held_by.is_none()
                && state.execution_verdict(&row.label).is_ok()
                && me != Some(row.label.as_str())
        })
        .find(|row| {
            matches!(
                condition(row, state, is_enrolled(row, enrolled), now),
                Condition::NotEnrolled | Condition::SignedOut
            ) && !state
                .accounts
                .get(&row.label)
                .is_some_and(|a| a.limited_until(now).is_some())
        })
        .map(|row| (row.label.clone(), row.email.is_some()))
}

/// Returns the first signed-out or un-enrolled roster row with an email (`Preferred` before
/// `Pool`: this Mac's preferred rows first, an order and no limit) that would give a session on `me` a usable target if renewed.
#[must_use]
pub fn renew_next(
    roster: &Roster,
    state: &State,
    enrolled: &BTreeSet<String>,
    me: Option<&str>,
    now: i64,
) -> Option<String> {
    for class in [Class::Preferred, Class::Pool] {
        for row in roster.rows.iter().filter(|r| {
            r.held_by.is_none() && state.execution_verdict(&r.label).is_ok() && r.class == class
        }) {
            if me == Some(row.label.as_str()) || row.email.is_none() {
                continue;
            }
            if matches!(
                condition(row, state, is_enrolled(row, enrolled), now),
                Condition::NotEnrolled | Condition::SignedOut
            ) && !state
                .accounts
                .get(&row.label)
                .is_some_and(|a| a.limited_until(now).is_some())
            {
                return Some(row.label.clone());
            }
        }
    }
    None
}

/// `[harness] accounts` rotation policy mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Move just before the wall, and at it.
    Early,
    /// Move only at the wall.
    Wall,
    /// Nothing rotates.
    Off,
}

impl Mode {
    /// Canonical `aterm.toml` string for this mode.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Mode::Early => "early",
            Mode::Wall => "wall",
            Mode::Off => "off",
        }
    }

    /// Parses a [`Mode`] from its `aterm.toml` string.
    #[must_use]
    pub fn parse(word: &str) -> Option<Mode> {
        match word {
            "early" => Some(Mode::Early),
            "wall" => Some(Mode::Wall),
            "off" => Some(Mode::Off),
            _ => None,
        }
    }
}

/// Trigger point at which a session asks [`advise`] for account guidance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Point {
    /// The turn ended on a usage limit this process hit ([`note_wall`], [`begin_rotation`]).
    Wall {
        /// The vendor's limit type (`five_hour`, `seven_day`, …).
        window: String,
        /// When the limit's row was written ([`Advice::Resume`]).
        at: Option<i64>,
    },
    /// A lost login right after the session's own move ([`advise`], [`strike`]).
    LostLogin {
        /// When the host first met this process's lost login (Unix seconds, its own clock).
        seen: i64,
    },
    /// A lost login with no move behind it ([`note_login_end`]).
    LoginEnded,
    /// A turn that ended with none.
    TurnEnd,
}

/// Session identity and login facts passed to [`advise`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Me<'a> {
    /// The label its process's own environment names ([`Roster::row_for_login`]).
    pub account: Option<&'a str>,
    /// The label whose login a renewal on this machine moved away from the one its process holds
    /// ([`Roster::row_left`], [`bound`]).
    pub left: Option<&'a str>,
    /// Its Claude home is the default one.
    pub default_home: bool,
    /// How its process authenticates, resolved against [`Self::account`]'s row
    /// ([`AuthRoute::for_account`]).
    pub route: AuthRoute,
}

impl<'a> Me<'a> {
    /// Returns the roster label for the session's current or superseded login.
    #[must_use]
    pub fn label(&self) -> Option<&'a str> {
        self.account.or(self.left)
    }
}

/// Reason [`advise`] recommends staying on the current account.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StayWhy {
    /// `accounts = "off"`.
    Off,
    /// No roster.
    NoRoster,
    /// The session's login is no row of the roster.
    NotListed,
    /// The session's home is not the default.
    OtherHome,
    /// The process authenticates by neither its account's stored login nor its vault token
    /// ([`AuthRoute::Outside`]).
    NotAStoredLogin,
    /// `active` has not served a response yet: nobody follows it, and nobody rotates past it.
    Canary,
    /// At a wall with no candidate and no instant of the roster's to wait for: the wall is waited
    /// out as it would be with no roster.
    NoCandidate,
    /// The machine's rotation bound is spent.
    Breaker {
        /// When the bound has room again ([`breaker_until`]).
        until: i64,
    },
    /// An account to leave ahead of its wall, and the owner's pin holds.
    Pinned,
    /// An account to leave, and it holds a reset offer that expires within a day.
    OfferExpiring,
    /// An account to leave, and no fresh account to move to.
    NoFresh,
    /// Nothing is due.
    NothingDue,
    /// A move is due, and no lab pass has measured this CLI's seam ([`advise_for`]), or its walls
    /// for a running session ([`advise_session`]).
    Unmeasured,
    /// The session answers its own walls (a proven self-claim, `codex-alab@<pid>`): aterm moves
    /// nothing under it ([`advise_session`]).
    OtherSupervisor,
    /// The CLI keeps its login in the OS keyring here (Codex's `cli_auth_credentials_store`),
    /// where no row's own login file is read, so no row is picked or moved to.
    KeyringStore,
    /// A move is due, and the relaunch line onto the row could not be written (its overlay home
    /// could not be made, or holds entries of its own), or the Codex build the session runs keeps
    /// no row's login its own ([`super::codex_alab::auth_path_unread`]): nothing was committed.
    NoLine,
}

impl StayWhy {
    /// Short reason word for logs and reports.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            StayWhy::Off => "off",
            StayWhy::NoRoster => "no-roster",
            StayWhy::NotListed => "not-listed",
            StayWhy::OtherHome => "other-home",
            StayWhy::NotAStoredLogin => "not-a-stored-login",
            StayWhy::Canary => "canary",
            StayWhy::NoCandidate => "no-candidate",
            StayWhy::Breaker { .. } => "breaker",
            StayWhy::Pinned => "pinned",
            StayWhy::OfferExpiring => "offer-expiring",
            StayWhy::NoFresh => "no-fresh",
            StayWhy::NothingDue => "nothing-due",
            StayWhy::Unmeasured => "unmeasured",
            StayWhy::OtherSupervisor => "other-supervisor",
            StayWhy::KeyringStore => "keyring-store",
            StayWhy::NoLine => "no-line",
        }
    }
}

/// Action recommended by [`advise`] at a session turn boundary or wall.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Advice {
    /// Do what would be done with no roster.
    Stay(StayWhy),
    /// Read `/usage` (after [`claim_probe`]).
    Probe,
    /// Relaunch under another login.
    Move {
        /// The label to move to.
        to: String,
        /// Ahead of the wall.
        ahead: bool,
        /// This move changes `active` and needs [`begin_rotation`] to win.
        rotates: bool,
        /// `active` has not served a response and the session that moved onto it has been silent
        /// for [`CANARY_HOLD_S`] ([`take_canary`]).
        takes_canary: bool,
        /// The session yields its own account by this move, another Mac's senior live claim
        /// holding it (`docs/DESIGN-fluid-swarm-2026-10-28.md` F19): until when a turn end that
        /// does not make the move holds, as one with no row to move to does ([`Advice::Wait`]).
        yields: Option<i64>,
    },
    /// No account is usable before this instant: at a wall, the session's own is walled; at a
    /// turn end, another Mac's senior live claim holds it (`docs/DESIGN-fluid-swarm-2026-10-28.md`
    /// F19), and the turn end holds rather than go on using it.
    Wait {
        /// When to decide again.
        until: i64,
    },
    /// The recorded wall expired or was lifted, and the current account can run.
    Resume {
        /// When the wall expired or was cleared, as the state was read.
        cleared_at: i64,
    },
}

impl Advice {
    /// Short action word logged by the host.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Advice::Stay(_) => "stay",
            Advice::Probe => "probe",
            Advice::Move { .. } => "move",
            Advice::Wait { .. } => "wait",
            Advice::Resume { .. } => "resume",
        }
    }
}

/// Sort key ordering candidate target accounts (lower ranks first).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rank {
    /// `low`: admitted only at a wall.
    pub low: bool,
    /// The row's class.
    pub class: Class,
    /// The week's reset from the newest sample, while it is still ahead.
    pub week_reset: Option<i64>,
}

impl Rank {
    fn key<'a>(&self, label: &'a str) -> (bool, Class, bool, i64, &'a str) {
        (
            self.low,
            self.class,
            self.week_reset.is_none(),
            self.week_reset.unwrap_or(i64::MAX),
            label,
        )
    }
}

/// Reason a roster row is excluded from candidate rotation targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WhyNot {
    /// Its credential is prepared, but no separate execution policy admits use.
    PreparationOnly,
    /// Preparation failed in this selection attempt; no new authority is implied.
    PreparationFailed,
    /// It is the session's own account.
    Me,
    /// Assigned only to other Macs' warm sets in `ai-pool.toml` (`Row::held_by`).
    HeldElsewhere,
    /// No login was made for it on this machine ([`login_made`]).
    NotEnrolled,
    /// Its login is known signed out.
    SignedOut,
    /// A wall stands.
    Limited {
        /// Until this instant.
        until: i64,
    },
    /// Another Mac's live claim on this account is the senior one
    /// (`docs/DESIGN-fluid-swarm-2026-10-28.md` F19).
    LeasedElsewhere {
        /// When that claim lapses.
        until: i64,
    },
    /// This Mac cannot record a claim, holds none on this account, and is not its home Mac.
    Unclaimed,
    /// This Mac's peers read its clock as off, or are split over it, and this is not one of its
    /// home accounts (`docs/DESIGN-fluid-swarm-2026-10-28.md` F22).
    ClockOff,
    /// `low`, and the point is not a wall.
    LowAheadOfAWall,
    /// `low`, and an account becomes usable within [`LOW_ONLY_AFTER_S`].
    LowWhileOneIsNear {
        /// When that account is usable.
        at: i64,
    },
}

impl WhyNot {
    /// Human-readable exclusion reason for reports.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            WhyNot::PreparationOnly => "prepared: awaiting execution authority",
            WhyNot::PreparationFailed => "preparation failed in this selection",
            WhyNot::Me => "this session's own",
            WhyNot::HeldElsewhere => "held-elsewhere",
            WhyNot::NotEnrolled => "not-enrolled",
            WhyNot::SignedOut => "signed-out",
            WhyNot::Limited { .. } => "limited",
            WhyNot::LeasedElsewhere { .. } => "leased-elsewhere",
            WhyNot::Unclaimed => "unclaimed",
            WhyNot::ClockOff => "clock-off: its peers disagree with this Mac's clock",
            WhyNot::LowAheadOfAWall => "low: used only at a wall",
            WhyNot::LowWhileOneIsNear { .. } => "low: an account is usable within 15 minutes",
        }
    }
}

fn usable_at(row: &Row, state: &State, cond: Condition, now: i64) -> Option<i64> {
    match cond {
        Condition::Limited { until } => Some(until),
        Condition::Usable | Condition::Unseen => Some(now),
        Condition::Low => state
            .accounts
            .get(&row.label)
            .and_then(|a| a.low_until(now)),
        Condition::SignedOut | Condition::NotEnrolled => None,
    }
}

fn effective_usable_at(row: &Row, state: &State, cond: Condition, now: i64) -> Option<i64> {
    if row.held_by.is_some() {
        return None;
    }
    let base = usable_at(row, state, cond, now)?;
    match state.lease_verdict(&row.label, now) {
        Ok(()) => Some(base),
        Err(WhyNot::LeasedElsewhere { until }) => Some(base.max(until)),
        Err(_) => None,
    }
}

pub(super) fn row_available_here(row: &Row, state: &State, now: i64) -> bool {
    row.held_by.is_none() && state.lease_verdict(&row.label, now).is_ok()
}

/// The trigger used to rank replacements for this session. A turn end that must
/// yield its current account admits low-budget replacements as a wall does.
#[must_use]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AccountClaimClock",
        action = "TurnEnd",
        project = "aterm_agent::harness::claims::tests::claim_clock_transition"
    )
)]
pub fn effective_ranking_point<'a>(
    roster: &Roster,
    state: &State,
    me: &Me<'_>,
    point: &'a Point,
    now: i64,
) -> Cow<'a, Point> {
    if *point == Point::TurnEnd
        && me.default_home
        && me.route.on_account()
        && let Some(mine) = me.label().and_then(|label| roster.row(label))
        && !row_available_here(mine, state, now)
    {
        Cow::Owned(Point::Wall {
            window: String::new(),
            at: None,
        })
    } else {
        Cow::Borrowed(point)
    }
}

/// Ranks all roster rows as rotation candidates.
#[must_use]
pub fn candidates(
    roster: &Roster,
    state: &State,
    enrolled: &BTreeSet<String>,
    me: Option<&str>,
    point: &Point,
    now: i64,
) -> Vec<(String, Result<Rank, WhyNot>)> {
    let conditions: Vec<(&Row, Condition)> = roster
        .rows
        .iter()
        .map(|r| (r, condition(r, state, is_enrolled(r, enrolled), now)))
        .collect();
    let near = conditions
        .iter()
        .filter_map(|(row, c)| effective_usable_at(row, state, *c, now))
        .filter(|at| *at > now && at.saturating_sub(now) <= LOW_ONLY_AFTER_S)
        .min();
    let at_wall = matches!(
        point,
        Point::Wall { .. } | Point::LostLogin { .. } | Point::LoginEnded
    );
    let mut out: Vec<(String, Result<Rank, WhyNot>)> = conditions
        .iter()
        .map(|(row, cond)| {
            let rank = |low: bool| Rank {
                low,
                class: row.class,
                week_reset: state
                    .accounts
                    .get(&row.label)
                    .and_then(|a| a.week_reset(now)),
            };
            let verdict = if me == Some(row.label.as_str()) {
                Err(WhyNot::Me)
            } else if row.held_by.is_some() {
                Err(WhyNot::HeldElsewhere)
            } else {
                match *cond {
                    Condition::NotEnrolled => Err(WhyNot::NotEnrolled),
                    Condition::SignedOut => Err(WhyNot::SignedOut),
                    Condition::Limited { until } => Err(WhyNot::Limited { until }),
                    Condition::Low => match state.lease_verdict(&row.label, now) {
                        Err(why) => Err(why),
                        Ok(()) if !at_wall => Err(WhyNot::LowAheadOfAWall),
                        Ok(()) => match near {
                            Some(at) => Err(WhyNot::LowWhileOneIsNear { at }),
                            None => Ok(rank(true)),
                        },
                    },
                    Condition::Usable | Condition::Unseen => {
                        state.lease_verdict(&row.label, now).map(|()| rank(false))
                    }
                }
            };
            (row.label.clone(), verdict)
        })
        .collect();
    out.sort_by(|a, b| match (&a.1, &b.1) {
        (Ok(x), Ok(y)) => x.key(&a.0).cmp(&y.key(&b.0)),
        (Ok(_), Err(_)) => std::cmp::Ordering::Less,
        (Err(_), Ok(_)) => std::cmp::Ordering::Greater,
        (Err(_), Err(_)) => std::cmp::Ordering::Equal,
    });
    out
}

/// Earliest timestamp at which any roster account becomes usable. A preference orders a move and
/// never holds one back (`docs/DESIGN-alab-lab-model-2026-11-01.md` §0), so a wait is never past
/// a pool row's instant for a preferred row's later one.
#[must_use]
pub fn next_usable(
    roster: &Roster,
    state: &State,
    enrolled: &BTreeSet<String>,
    now: i64,
) -> Option<i64> {
    wait_instant(roster, state, enrolled, Instants::All, now)
}

/// Earliest timestamp after `now` at which a currently blocked roster account becomes usable.
#[must_use]
pub fn next_opening(
    roster: &Roster,
    state: &State,
    enrolled: &BTreeSet<String>,
    now: i64,
) -> Option<i64> {
    wait_instant(roster, state, enrolled, Instants::Ahead, now)
}

/// Timestamp at which a session held at a wall should re-evaluate [`advise`].
#[must_use]
pub fn hold_until(why: StayWhy, state: &State, opening: Option<i64>, now: i64) -> Option<i64> {
    let end = match why {
        StayWhy::Canary => state.canary_until(now)?,
        StayWhy::Breaker { until } => until,
        _ => return None,
    };
    Some(opening.filter(|at| *at > now).map_or(end, |at| end.min(at)))
}

/// Which of the roster's instants [`wait_instant`] counts.
#[derive(Debug, Clone, Copy)]
enum Instants<'a> {
    /// Every account's.
    All,
    /// Every account's, but only one ahead of `now` for this one.
    WalledOn(&'a str),
    /// Only the ones ahead of `now` ([`next_opening`]).
    Ahead,
}

fn wait_instant(
    roster: &Roster,
    state: &State,
    enrolled: &BTreeSet<String>,
    which: Instants<'_>,
    now: i64,
) -> Option<i64> {
    roster
        .rows
        .iter()
        .filter_map(|r| {
            let cond = condition(r, state, is_enrolled(r, enrolled), now);
            let at = effective_usable_at(r, state, cond, now)?;
            let counted = match which {
                Instants::All => true,
                Instants::WalledOn(walled) => walled != r.label || at > now,
                Instants::Ahead => at > now,
            };
            counted.then_some(at)
        })
        .min()
}

/// Computes the seconds after its newest sample when `account`'s next `/usage` probe becomes due.
#[must_use]
pub fn probe_interval(account: &Account) -> i64 {
    let (Some(new), Some(old)) = (&account.sample, &account.before) else {
        return PROBE_DEFAULT_S;
    };
    let span = new.at.saturating_sub(old.at);
    if span <= 0 {
        return PROBE_DEFAULT_S;
    }
    let half_to = |new_pct: Option<u32>, old_pct: Option<u32>, same: bool, threshold: u32| {
        let (new_pct, old_pct) = (new_pct?, old_pct?);
        if !same {
            return None;
        }
        if new_pct >= threshold {
            return Some(0);
        }
        if new_pct <= old_pct {
            return Some(i64::MAX);
        }
        let left = i128::from(threshold - new_pct);
        let gained = i128::from(new_pct - old_pct);
        let half = left * i128::from(span) / (2 * gained);
        Some(i64::try_from(half).unwrap_or(i64::MAX))
    };
    let known = [
        half_to(
            new.session_pct,
            old.session_pct,
            new.session_reset == old.session_reset,
            LOW_SESSION_PCT,
        ),
        half_to(
            new.week_pct,
            old.week_pct,
            new.week_reset == old.week_reset,
            LOW_WEEK_PCT,
        ),
    ];
    known
        .into_iter()
        .flatten()
        .min()
        .map_or(PROBE_DEFAULT_S, |s| s.clamp(PROBE_MIN_S, PROBE_MAX_S))
}

/// Returns whether a `/usage` probe is due for `account` at `now`.
#[must_use]
pub fn probe_due(account: &Account, now: i64) -> bool {
    account
        .sample
        .as_ref()
        .is_none_or(|s| !recent(s.at, now, probe_interval(account)))
}

fn offer_expiring(account: &Account, now: i64) -> bool {
    account.offer.as_ref().is_some_and(|o| {
        o.left > 0
            && o.use_by
                .is_some_and(|by| by > now && by.saturating_sub(now) <= OFFER_EXPIRING_S)
    })
}

/// Pure decision function returning the [`Advice`] for session `me` at `point` and `now`.
#[must_use]
pub fn advise(
    roster: &Roster,
    state: &State,
    enrolled: &BTreeSet<String>,
    me: &Me<'_>,
    point: &Point,
    mode: Mode,
    now: i64,
) -> Advice {
    advise_with(roster, state, enrolled, me, point, mode, now, true)
}

/// Computes the advice for `cli`'s pool: [`advise`]'s for Claude Code, else [`advise_past_probe`]'s
/// (another CLI has no `/usage` probe). A move between the logins of a CLI whose seam no lab pass
/// has measured ([`crate::ai::connector::Accounts::measured`]) becomes [`StayWhy::Unmeasured`],
/// or a wait when the session must yield, since an unmeasured relaunch can lose the session.
#[allow(clippy::too_many_arguments)]
#[must_use]
pub fn advise_for(
    cli: Cli,
    roster: &Roster,
    state: &State,
    enrolled: &BTreeSet<String>,
    me: &Me<'_>,
    point: &Point,
    mode: Mode,
    now: i64,
) -> Advice {
    let advice = if cli == Cli::Claude {
        advise(roster, state, enrolled, me, point, mode, now)
    } else {
        advise_past_probe(roster, state, enrolled, me, point, mode, now)
    };
    held_unless_measured(advice, cli.accounts().measured)
}

/// Hold an unmeasured move, retaining a wait when its session cannot keep executing.
fn held_unless_measured(advice: Advice, measured: bool) -> Advice {
    match advice {
        Advice::Move {
            yields: Some(until),
            ..
        } if !measured => Advice::Wait { until },
        Advice::Move { .. } if !measured => Advice::Stay(StayWhy::Unmeasured),
        advice => advice,
    }
}

/// Who answers a running session's walls ([`advise_session`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Supervisor {
    /// aterm's harness: the roster's advice is its to act on.
    Aterm,
    /// The session's own program, by a proven self-claim (`codex-alab@<pid>`).
    Own,
}

/// Computes [`advise_for`] for a running session of `cli` that `supervisor` answers. A session
/// that answers its own walls stays ([`StayWhy::OtherSupervisor`]), and so does a move of a
/// session of a CLI whose walls no lab pass has measured
/// ([`crate::ai::connector::Accounts::walls_measured_on`], [`StayWhy::Unmeasured`]): that CLI
/// moves only at a launch ([`launch_row`]).
#[allow(clippy::too_many_arguments)]
#[must_use]
pub fn advise_session(
    cli: Cli,
    supervisor: Supervisor,
    roster: &Roster,
    state: &State,
    enrolled: &BTreeSet<String>,
    me: &Me<'_>,
    point: &Point,
    mode: Mode,
    now: i64,
) -> Advice {
    if supervisor == Supervisor::Own {
        return Advice::Stay(StayWhy::OtherSupervisor);
    }
    match advise_for(cli, roster, state, enrolled, me, point, mode, now) {
        Advice::Move {
            yields: Some(until),
            ..
        } if cli.accounts().walls_measured_on.is_none() => Advice::Wait { until },
        Advice::Move { .. } if cli.accounts().walls_measured_on.is_none() => {
            Advice::Stay(StayWhy::Unmeasured)
        }
        advice => advice,
    }
}

/// Claude Code's rotation target, and the row every other CLI's launch weighs: `active` while the
/// owner's pin holds it (a row whose login is here and not signed out) or while it is fresh, else
/// the first candidate a wall would move to, else `active` while it still runs (a low row, every
/// other one walled); `None` when no row is usable. A launch of a CLI other than Claude Code
/// passes this through [`super::accounts_pool::launch_now`], which decides that pool's `next:` in
/// `aterm accounts`.
#[must_use]
pub fn launch_row<'a>(
    roster: &'a Roster,
    state: &State,
    enrolled: &BTreeSet<String>,
    now: i64,
) -> Option<&'a Row> {
    let available_here = |row: &Row| row_available_here(row, state, now);
    let active = state.active.as_deref().and_then(|label| roster.row(label));
    if let Some(row) = active
        && available_here(row)
    {
        let cond = condition(row, state, is_enrolled(row, enrolled), now);
        let here = !matches!(cond, Condition::NotEnrolled | Condition::SignedOut);
        if (state.pinned && here) || cond.fresh() {
            return Some(row);
        }
    }
    let point = Point::Wall {
        window: String::new(),
        at: None,
    };
    let me = active.map(|row| row.label.as_str());
    candidates(roster, state, enrolled, me, &point, now)
        .into_iter()
        .find(|(_, verdict)| verdict.is_ok())
        .and_then(|(label, _)| roster.row(&label))
        .or_else(|| {
            active.filter(|row| {
                available_here(row) && condition(row, state, is_enrolled(row, enrolled), now).runs()
            })
        })
}

/// Whether a Claude Code worker whose starter runs as `who` may leave its starter's row: rotation
/// is on, and `who` is this machine's own stored login or its row's vault token. The launcher asks this before it reads
/// any record, so a launch under `Off` reads none.
#[must_use]
pub fn claude_worker_may_move(mode: Mode, who: &ProcessLogin) -> bool {
    // A starter on its row's token moves as one on its stored login does (`judged_label` credits
    // only the row its marker names).
    let own = match who.route {
        AuthRoute::Stored | AuthRoute::VaultToken { .. } => true,
        AuthRoute::Outside => false,
    };
    mode != Mode::Off && who.default_home && own
}

/// The starter's row and the row a Claude Code worker whose starter runs as `who` starts on, where
/// it moves at all: [`claude_worker_may_move`], the roster lists several, and [`worker_row`] moves
/// it. This is the one rule the task launcher follows and a survey reports, so what a survey says
/// a launch would start on cannot drift from what the launch does.
#[must_use]
pub fn claude_worker_move<'a>(
    roster: &'a Roster,
    state: &State,
    enrolled: &BTreeSet<String>,
    who: &ProcessLogin,
    mode: Mode,
    now: i64,
) -> Option<(&'a str, &'a Row)> {
    if !claude_worker_may_move(mode, who) || !roster.several() {
        return None;
    }
    let from = judged_label(roster, who)?;
    worker_row(roster, state, enrolled, from, mode, now).map(|row| (from, row))
}

/// The row a Claude Code worker spawned from a tab on the row `from` starts under, where that is
/// another row: the one the host would move that tab to at its next point, by the host's own
/// rule ([`advise_past_probe`] under `mode`), at a wall where `from` cannot run now (walled,
/// signed out, not enrolled here) and at a turn end where it can. So the worker follows `active`
/// where it is another row and fresh, or pinned and still running; a canary holds it where the
/// host would hold the tab; a spent rotation bound keeps it; under [`Mode::Wall`] a row that
/// still runs keeps it, and under [`Mode::Early`] a low row it should leave early moves it.
/// `None` keeps the worker on `from`: a stay, a wait, a resume, `from` no row of `roster`, or
/// a move that takes the canary, which is the host's to take with the worker's own session.
/// A launch records nothing: the move is the host's to record, as for any launch
/// ([`launch_row`]).
#[must_use]
pub fn worker_row<'a>(
    roster: &'a Roster,
    state: &State,
    enrolled: &BTreeSet<String>,
    from: &str,
    mode: Mode,
    now: i64,
) -> Option<&'a Row> {
    let mine = roster.row(from)?;
    let point = if condition(mine, state, is_enrolled(mine, enrolled), now).runs() {
        Point::TurnEnd
    } else {
        Point::Wall {
            window: String::new(),
            at: None,
        }
    };
    let me = Me {
        account: Some(from),
        left: None,
        default_home: true,
        route: AuthRoute::Stored,
    };
    match advise_past_probe(roster, state, enrolled, &me, &point, mode, now) {
        Advice::Move {
            to,
            takes_canary: false,
            ..
        } if to != from => roster.row(&to),
        _ => None,
    }
}

/// Computes [`advise`] with the `/usage` probe arm skipped.
#[must_use]
pub fn advise_past_probe(
    roster: &Roster,
    state: &State,
    enrolled: &BTreeSet<String>,
    me: &Me<'_>,
    point: &Point,
    mode: Mode,
    now: i64,
) -> Advice {
    advise_with(roster, state, enrolled, me, point, mode, now, false)
}

/// [`advise`] (`probes`) and [`advise_past_probe`] (not): one body.
#[allow(clippy::too_many_arguments)]
fn advise_with(
    roster: &Roster,
    state: &State,
    enrolled: &BTreeSet<String>,
    me: &Me<'_>,
    point: &Point,
    mode: Mode,
    now: i64,
    probes: bool,
) -> Advice {
    let login_point =
        *point == Point::LoginEnded || (me.left.is_some() && !matches!(point, Point::Wall { .. }));
    if mode == Mode::Off
        && !login_point
        && me
            .label()
            .is_none_or(|label| state.execution_verdict(label).is_ok())
    {
        return Advice::Stay(StayWhy::Off);
    }
    if roster.rows.is_empty() {
        return Advice::Stay(StayWhy::NoRoster);
    }
    let Some(mine) = me.label().and_then(|label| roster.row(label)) else {
        return Advice::Stay(StayWhy::NotListed);
    };
    if !me.default_home {
        return Advice::Stay(StayWhy::OtherHome);
    }
    // A token session goes through every rule after this one, as a stored login does: the
    // yield, the walls, `LeasedElsewhere` and `ClockOff` (§5.2). Only a process outside its
    // account's own credential stays.
    match me.route {
        AuthRoute::Stored | AuthRoute::VaultToken { .. } => {}
        AuthRoute::Outside => return Advice::Stay(StayWhy::NotAStoredLogin),
    }
    let at_wall = matches!(
        point,
        Point::Wall { .. } | Point::LostLogin { .. } | Point::LoginEnded
    );
    let cond = |row: &Row| condition(row, state, is_enrolled(row, enrolled), now);
    let available_here = |row: &Row| row_available_here(row, state, now);
    if *point == Point::LoginEnded {
        return if available_here(mine) {
            login_follow(mine, me, point, cond(mine))
        } else {
            Advice::Stay(StayWhy::NoCandidate)
        };
    }
    if login_point && available_here(mine) {
        return login_follow(mine, me, point, cond(mine));
    }
    let unavailable = !available_here(mine);
    let retry = now.saturating_add(super::claims::RECHECK_S);
    if mode == Mode::Off {
        return if !at_wall && state.execution_verdict(&mine.label).is_err() {
            Advice::Wait { until: retry }
        } else {
            Advice::Stay(StayWhy::Off)
        };
    }
    // A session without execution authority yields at its next turn end. Preparation holds,
    // senior claims and unrecordable non-home claims all forbid continuing on the credential.
    // Rank replacements as at a wall, including low-budget accounts.
    let ranked_at = effective_ranking_point(roster, state, me, point, now);
    let yielding = ranked_at.as_ref() != point;
    // Where it yields, a turn end no move takes it from holds until a row opens, the breaker has
    // room, the `active`'s canary hold ends, or the claims are read again, whichever comes first:
    // so too one that does not make its move ([`Advice::Move`]'s `yields`).
    let yields = yielding.then(|| {
        let opening = wait_instant(roster, state, enrolled, Instants::Ahead, now);
        [
            opening,
            breaker_until(state, roster, now),
            state.canary_until(now),
        ]
        .into_iter()
        .flatten()
        .filter(|at| *at > now)
        .fold(retry, i64::min)
    });
    let admitted = |to: &str| {
        // Use the same wall reducer as the recorder: a new wall retires its pin, but a
        // stale observation of a cleared wall must preserve it.
        let mut current = state.clone();
        if let Point::Wall { window, at } = point {
            note_wall(
                &mut current,
                &mine.label,
                window,
                None,
                at.unwrap_or(now),
                now,
            );
        }
        let mover = Mover {
            sid: "advice-admission",
            account: &mine.label,
        };
        begin_rotation(
            &mut current,
            roster,
            enrolled,
            state.active.as_deref(),
            mover,
            to,
            now,
        )
        .is_ok()
    };
    let onto_active = |active: &Row, takes_canary: bool| Advice::Move {
        to: active.label.clone(),
        ahead: !at_wall,
        rotates: false,
        takes_canary,
        yields,
    };
    let mut low_active = None;
    if let Some(active) = state.active.as_deref().and_then(|l| roster.row(l))
        && active.label != mine.label
        && available_here(active)
    {
        let theirs = cond(active);
        if theirs.runs() && !state.proven {
            let walled = at_wall || yielding;
            if !state.canary_holds(now) && (walled || theirs.fresh()) {
                return onto_active(active, true);
            }
            if !yielding {
                return Advice::Stay(StayWhy::Canary);
            }
        } else if theirs.fresh() || (theirs.runs() && state.pinned) {
            return onto_active(active, false);
        } else if theirs.runs() {
            low_active = Some(active);
        }
    }

    if let Point::Wall {
        window,
        at: Some(written),
    } = point
        && available_here(mine)
        && let Some(record) = state.accounts.get(&mine.label)
        && let Some(cleared_at) = record.cleared_since(window, *written, now).or_else(|| {
            // A timed wall is also released at its reset (including its grace), or
            // at the end of its retry backoff. An observation made at or after
            // that instant is a new wall, not evidence that the old one ended.
            record
                .walls
                .get(window)?
                .stands_until()
                .filter(|end| *written < *end && *end <= now)
        })
        && cond(mine).runs()
    {
        return Advice::Resume {
            cleared_at: cleared_at.min(now),
        };
    }

    if let Point::LostLogin { seen } = *point
        && available_here(mine)
        && cond(mine).runs()
        && state.accounts.get(&mine.label).is_some_and(|a| {
            a.current_auth(mine) == Auth::Ok && a.auth_at.is_some_and(|signed_in| signed_in > seen)
        })
    {
        let unanswered = !state.proven && state.active.as_deref() == Some(mine.label.as_str());
        if unanswered && state.canary_holds(now) {
            return Advice::Stay(StayWhy::Canary);
        }
        return Advice::Move {
            to: mine.label.clone(),
            ahead: false,
            rotates: false,
            takes_canary: unanswered,
            yields: None,
        };
    }

    let breaker = breaker_until(state, roster, now);
    let listed = candidates(
        roster,
        state,
        enrolled,
        Some(&mine.label),
        ranked_at.as_ref(),
        now,
    );
    let low_active =
        low_active.filter(|a| listed.iter().any(|(l, rank)| *l == a.label && rank.is_ok()));
    let next = listed
        .into_iter()
        .find_map(|(label, verdict)| verdict.ok().map(|rank| (label, rank)));

    if at_wall {
        return match (next, low_active, breaker) {
            (Some((_, rank)), Some(active), spent) if rank.low || spent.is_some() => {
                onto_active(active, false)
            }
            (Some(_), _, Some(until)) => Advice::Stay(StayWhy::Breaker { until }),
            (Some((to, _)), _, None) if admitted(&to) => Advice::Move {
                to,
                ahead: false,
                rotates: true,
                takes_canary: false,
                yields: None,
            },
            (None, ..) => {
                let walled = Instants::WalledOn(&mine.label);
                match wait_instant(roster, state, enrolled, walled, now) {
                    Some(until) if until > now => Advice::Wait { until },
                    _ if unavailable => Advice::Wait { until: retry },
                    _ => Advice::Stay(StayWhy::NoCandidate),
                }
            }
            (Some(_), _, None) => Advice::Wait { until: retry },
        };
    }

    if let Some(until) = yields {
        // A move goes only where its rotation would be admitted ([`begin_rotation`]: not off a
        // pinned `active` that still runs, or an unanswered one this Mac may use); else the turn
        // end waits, so the session stops using the account, until the yield's instant.
        return match (next, low_active, breaker) {
            (Some((_, rank)), Some(active), spent) if rank.low || spent.is_some() => {
                onto_active(active, false)
            }
            (Some((to, _)), _, None) if admitted(&to) => Advice::Move {
                to,
                ahead: true,
                rotates: true,
                takes_canary: false,
                yields,
            },
            _ => Advice::Wait { until },
        };
    }

    if mode != Mode::Early {
        return Advice::Stay(StayWhy::NothingDue);
    }
    let account = state.account(&mine.label);
    let own = cond(mine);
    let yielded = !available_here(mine);
    let limited = matches!(own, Condition::Limited { .. });
    let leave = yielded || limited || (own == Condition::Low && account.leave_early(now));
    if state.pinned && !yielded {
        return Advice::Stay(if leave {
            StayWhy::Pinned
        } else {
            StayWhy::NothingDue
        });
    }
    let unanswered = !state.proven
        && state.active.as_deref() == Some(mine.label.as_str())
        && own.runs()
        && !yielded;
    if probes
        && roster.rows.len() > 1
        && !limited
        && !yielded
        && !unanswered
        && !probe_held(state, &mine.label, now)
        && probe_due(&account, now)
    {
        return Advice::Probe;
    }
    if !leave {
        return Advice::Stay(StayWhy::NothingDue);
    }
    if !yielded && offer_expiring(&account, now) {
        return Advice::Stay(StayWhy::OfferExpiring);
    }
    match (next, breaker) {
        (None, _) => Advice::Stay(StayWhy::NoFresh),
        (Some(_), _) if unanswered => Advice::Stay(StayWhy::Canary),
        (Some(_), Some(until)) => Advice::Stay(StayWhy::Breaker { until }),
        (Some((to, _)), None) if admitted(&to) => Advice::Move {
            to,
            ahead: true,
            rotates: true,
            takes_canary: false,
            yields: None,
        },
        (Some(_), None) => Advice::Wait { until: retry },
    }
}

fn login_follow(mine: &Row, me: &Me<'_>, point: &Point, cond: Condition) -> Advice {
    let at_wall = !matches!(point, Point::TurnEnd);
    let signed_in = !matches!(cond, Condition::SignedOut | Condition::NotEnrolled);
    if me.left.is_some() && signed_in {
        return Advice::Move {
            to: mine.label.clone(),
            ahead: !at_wall,
            rotates: false,
            takes_canary: false,
            yields: None,
        };
    }
    Advice::Stay(if at_wall {
        StayWhy::NoCandidate
    } else {
        StayWhy::NothingDue
    })
}

/// Event kind recorded in `ledger.jsonl`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A wall was recorded for an account.
    Wall,
    /// A `/usage` sample was recorded.
    Sample,
    /// `active` was rotated ([`begin_rotation`]), or a canary took a silent one's place
    /// ([`take_canary`]).
    Rotate,
    /// The rotated-to account served a response.
    Proven,
    /// A candidate was struck and `active` went back.
    Struck,
    /// A session was read on another account than before.
    Move,
    /// A `/usage` probe was claimed.
    Probe,
    /// A limit reset was used.
    ResetUsed,
    /// A limit reset was offered and kept.
    ResetKept,
    /// A limit reset failed.
    ResetFailed,
    /// A login's word changed.
    Auth,
    /// The owner pinned an account.
    Use,
    /// The pin ended.
    Unpin,
    /// A tapped session's usage tap went silent (`super::tap::Silence`).
    NoTap,
    /// Usage credits began carrying an account's sessions ([`record_usage`]).
    Credits,
    /// Claude Code warned that a window is filling ([`record_usage`]).
    Warned,
    /// A login was renewed ([`note_renewed`]).
    Renewed,
    /// An in-place login succeeded ([`note_signed_back`], [`note_default_login_made`]).
    SignedIn,
    /// A bound login was seen ended, at a login wall in a session on it ([`note_login_end`]).
    LoginEnded,
    /// A renewal's check answered another account than the row's ([`note_wrong_account`]).
    WrongAccount,
    /// A limit reset was used inside a window ([`note_reading`], [`WindowRead::refilled`]).
    Reset,
    /// Another account signed in to the home ([`note_reading`]).
    Epoch,
    /// A sign-in that bound no login, `accounts browser`'s window included: its `why` names the
    /// page's host, the flow (`device`, `browser`, `window`) and how it ended, never a URL, code
    /// or email.
    SignIn,
    /// The vendor's own sign-in on the default login (Claude Code's `/login`) replaced the
    /// account the row held there, which leaves the row no login on this machine
    /// ([`note_displaced`]); `to` names the row the default login holds now.
    Displaced,
}

impl Kind {
    /// Every kind.
    pub const ALL: [Kind; 24] = [
        Kind::Wall,
        Kind::Sample,
        Kind::Rotate,
        Kind::Proven,
        Kind::Struck,
        Kind::Move,
        Kind::Probe,
        Kind::ResetUsed,
        Kind::ResetKept,
        Kind::ResetFailed,
        Kind::Auth,
        Kind::Use,
        Kind::Unpin,
        Kind::NoTap,
        Kind::Credits,
        Kind::Warned,
        Kind::Renewed,
        Kind::SignedIn,
        Kind::LoginEnded,
        Kind::WrongAccount,
        Kind::Reset,
        Kind::Epoch,
        Kind::SignIn,
        Kind::Displaced,
    ];

    /// Canonical string for this event kind in `ledger.jsonl`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Kind::Wall => "wall",
            Kind::Sample => "sample",
            Kind::Rotate => "rotate",
            Kind::Proven => "proven",
            Kind::Struck => "struck",
            Kind::Move => "move",
            Kind::Probe => "probe",
            Kind::ResetUsed => "reset-used",
            Kind::ResetKept => "reset-kept",
            Kind::ResetFailed => "reset-failed",
            Kind::Auth => "auth",
            Kind::Use => "use",
            Kind::Unpin => "unpin",
            Kind::NoTap => "no-tap",
            Kind::Credits => "credits",
            Kind::Warned => "warned",
            Kind::Renewed => "renewed",
            Kind::SignedIn => "signed-in",
            Kind::LoginEnded => "login-ended",
            Kind::WrongAccount => "wrong-account",
            Kind::Reset => "reset",
            Kind::Epoch => "epoch",
            Kind::SignIn => "sign-in",
            Kind::Displaced => "displaced",
        }
    }

    /// Parses a [`Kind`] from its `ledger.jsonl` string.
    #[must_use]
    pub fn parse(word: &str) -> Option<Kind> {
        Kind::ALL.into_iter().find(|k| k.as_str() == word)
    }
}

/// Single append-only event row in `ledger.jsonl`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    /// When (Unix seconds).
    pub t: i64,
    /// What.
    pub kind: Kind,
    /// The account it is about.
    pub account: String,
    /// The account moved or rotated to.
    pub to: Option<String>,
    /// The session, where one is concerned.
    pub sid: Option<String>,
    /// The reason, one bounded line.
    pub why: String,
    /// The account's lab key (`ac-<16hex>`), set when the row is published from the account's
    /// learned key ([`Account::pk`], [`super::journal::observe`]).
    pub pk: Option<String>,
    /// Captured sign-in workflow custody; an explicit legacy zero is never retargeted.
    pub workflow: Option<WorkflowToken>,
}

impl Event {
    /// Constructs a ledger event at timestamp `t` for `account`.
    #[must_use]
    pub fn new(t: i64, kind: Kind, account: &str) -> Event {
        Event {
            t,
            kind,
            account: one_line(account, LABEL_MAX),
            to: None,
            sid: None,
            why: String::new(),
            pk: None,
            workflow: None,
        }
    }

    /// Sets the target account label (`to`) on this event.
    #[must_use]
    pub fn with_to(mut self, label: &str) -> Event {
        self.to = Some(one_line(label, LABEL_MAX)).filter(|l| !l.is_empty());
        self
    }

    /// Sets the session ID (`sid`) on this event.
    #[must_use]
    pub fn with_sid(mut self, sid: &str) -> Event {
        self.sid = Some(one_line(sid, WORD_MAX)).filter(|s| !s.is_empty());
        self
    }

    /// Sets the reason string (`why`) on this event.
    #[must_use]
    pub fn because(mut self, why: &str) -> Event {
        self.why = one_line(why, WHY_MAX);
        self
    }

    /// Returns whether this event wakes sessions waiting on an account hold.
    #[must_use]
    pub fn wakes(&self) -> bool {
        match self.kind {
            Kind::Use
            | Kind::Unpin
            | Kind::Proven
            | Kind::Struck
            | Kind::ResetUsed
            | Kind::ResetKept
            | Kind::ResetFailed
            | Kind::Renewed
            | Kind::SignedIn => true,
            Kind::Auth => self.why == Auth::Ok.as_str(),
            Kind::Wall
            | Kind::Sample
            | Kind::Rotate
            | Kind::Move
            | Kind::Probe
            | Kind::NoTap
            | Kind::Credits
            | Kind::Warned
            | Kind::LoginEnded
            | Kind::WrongAccount
            | Kind::Reset
            | Kind::Epoch
            | Kind::SignIn
            | Kind::Displaced => false,
        }
    }

    /// Serializes the event as a single-line bounded JSON object.
    #[must_use]
    pub fn to_json_line(&self) -> String {
        let mut o = Map::new();
        o.insert("t".to_owned(), Value::from(self.t));
        o.insert("kind".to_owned(), Value::from(self.kind.as_str()));
        o.insert(
            "account".to_owned(),
            Value::from(one_line(&self.account, LABEL_MAX)),
        );
        o.insert(
            "to".to_owned(),
            Value::from(self.to.as_deref().map(|l| one_line(l, LABEL_MAX))),
        );
        o.insert(
            "sid".to_owned(),
            Value::from(self.sid.as_deref().map(|s| one_line(s, WORD_MAX))),
        );
        o.insert(
            "why".to_owned(),
            Value::from(one_line(&self.why, LEDGER_WHY_MAX)),
        );
        if let Some(pk) = self
            .pk
            .as_deref()
            .filter(|pk| super::journal::Key::new(pk).is_some())
        {
            o.insert("pk".to_owned(), Value::from(pk));
        }
        if let Some(token) = self.workflow {
            o.insert("workflow".into(), Value::from(token.value()));
        }
        aterm_json::to_string(&Value::Object(o)).unwrap_or_else(|_| String::from("{}"))
    }

    /// Parses a single `ledger.jsonl` line into an [`Event`].
    #[must_use]
    pub fn of_json_line(line: &str) -> Option<Event> {
        if line.len() > MAX_EVENT_LINE_BYTES {
            return None;
        }
        let doc: Value = aterm_json::from_str(line.trim()).ok()?;
        Some(Event {
            t: int(doc.get("t"))?,
            kind: Kind::parse(doc.get("kind")?.as_str()?)?,
            account: word(doc.get("account"), LABEL_MAX)?,
            to: word(doc.get("to"), LABEL_MAX),
            sid: word(doc.get("sid"), WORD_MAX),
            why: word(doc.get("why"), LEDGER_WHY_MAX).unwrap_or_default(),
            pk: doc
                .get("pk")
                .and_then(Value::as_str)
                .filter(|pk| super::journal::Key::new(pk).is_some())
                .map(str::to_owned),
            workflow: match doc.get("workflow") {
                None => None,
                Some(value) => Some(WorkflowToken::from_value(value.as_u64()?)),
            },
        })
    }
}

fn account_mut<'a>(state: &'a mut State, label: &str) -> Option<&'a mut Account> {
    if !admissible_label(label) {
        return None;
    }
    let account = state.accounts.entry(label.to_owned()).or_default();
    if state.contextual_auth && matches!(account.auth_projection, AuthProjection::Legacy) {
        account.auth_projection = AuthProjection::Unknown;
    }
    Some(account)
}

fn end_pin(state: &mut State, account: &str, why: &str, t: i64, out: &mut Vec<Event>) {
    if state.pinned && state.active.as_deref() == Some(account) {
        state.pinned = false;
        out.push(Event::new(t, Kind::Unpin, account).because(why));
    }
}

fn most_lapsed(walls: &BTreeMap<String, Wall>) -> Option<String> {
    walls
        .iter()
        .min_by_key(|(_, w)| (w.stands_until(), w.at))
        .map(|(k, _)| k.clone())
}

/// Filters a wall's reported reset timestamp `until`, discarding timestamps more than
/// [`RESET_BELIEVED_S`] past `at`.
#[must_use]
pub fn believed_reset(until: Option<i64>, at: i64) -> Option<i64> {
    until.filter(|until| until.saturating_sub(at) <= RESET_BELIEVED_S)
}

/// Records a quota wall hit by `account` and returns any resulting ledger events. Its reset
/// `until` is believed as [`believed_reset`] says, or up to [`MONTH_RESET_BELIEVED_S`] past `at`
/// for a [`MONTH_LIMIT`] wall.
pub fn note_wall(
    state: &mut State,
    account: &str,
    window: &str,
    until: Option<i64>,
    at: i64,
    now: i64,
) -> Vec<Event> {
    let window = one_line(window, WORD_MAX);
    let until = if window == MONTH_LIMIT {
        until.filter(|until| until.saturating_sub(at) <= MONTH_RESET_BELIEVED_S)
    } else {
        believed_reset(until, at)
    };
    let by = state.machine.clone();
    let mut out = Vec::new();
    if window.is_empty() {
        return out;
    }
    let Some(record) = account_mut(state, account) else {
        return out;
    };
    if record.cleared_since(&window, at, now).is_some() {
        return out;
    }
    // A wall that names no reset says nothing of one a read named: it stands until then.
    let until = until.or_else(|| {
        record
            .walls
            .get(&window)
            .and_then(|old| old.until)
            .filter(|known| *known > at)
    });
    let late = u32::from(until.is_none_or(|u| at > u));
    if let Some(old) = record.walls.get_mut(&window) {
        old.at = old.at.min(now);
    }
    let attempts = match record.walls.get(&window) {
        Some(old) if at < old.at => None,
        Some(old) if old.until == until => old.stands_until().map_or(Some(late), |end| {
            if at < end {
                None
            } else if at.saturating_sub(end) > WALL_EPISODE_S {
                Some(late)
            } else {
                Some(old.attempts.saturating_add(1))
            }
        }),
        _ => Some(late),
    };
    if let Some(attempts) = attempts {
        if !record.walls.contains_key(&window)
            && record.walls.len() >= MAX_WALLS
            && let Some(lapsed) = most_lapsed(&record.walls)
        {
            record.walls.remove(&lapsed);
        }
        record.walls.insert(
            window.clone(),
            Wall {
                until,
                at,
                by,
                attempts,
                cleared: false,
            },
        );
        let why = match until {
            Some(until) => format!("{window} until {until}"),
            None => format!("{window}, no reset named"),
        };
        out.push(Event::new(at, Kind::Wall, account).because(&why));
    }
    end_pin(state, account, "wall", at, &mut out);
    out
}

/// Lifts the walls a read of `account` shows gone: `sample`, read after each of them and naming no
/// wall of its own, has a figure for the wall's window below full. A wall written after the read
/// stands, and so does one of a window it has no figure for (a month, a window its plan does not
/// show). The caller holds a read that named a wall back, since a server can refuse use with no
/// window full. As a reset's clearing does ([`reset_outcome`]), the mark keeps an older observer
/// from raising it again.
pub fn note_room(state: &mut State, account: &str, sample: &Sample, now: i64) -> Vec<Event> {
    let by = state.machine.clone();
    let Some(record) = account_mut(state, account) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (window, wall) in &mut record.walls {
        let pct = match window.as_str() {
            SESSION_LIMIT => sample.session_pct,
            WEEK_LIMIT => sample.week_pct,
            _ => None,
        };
        if !wall.cleared && wall.at < sample.at && pct.is_some_and(|pct| pct < 100) {
            wall.cleared = true;
            wall.at = now;
            wall.by = by.clone();
            wall.attempts = 0;
            out.push(
                Event::new(now, Kind::Wall, account)
                    .because(&format!("{window} lifted: a read shows room")),
            );
        }
    }
    out
}

/// Records a settled `/usage` panel [`Sample`] for `account` at `now`.
pub fn note_sample(state: &mut State, account: &str, sample: Sample, now: i64) -> Vec<Event> {
    let by = state.machine.clone();
    let sample = Sample {
        session_pct: sample.session_pct.filter(|p| *p <= PCT_MAX),
        week_pct: sample.week_pct.filter(|p| *p <= PCT_MAX),
        session_read_at: sample
            .session_pct
            .filter(|p| *p <= PCT_MAX)
            .map(|_| sample.at),
        week_read_at: sample.week_pct.filter(|p| *p <= PCT_MAX).map(|_| sample.at),
        by,
        ..sample
    };
    if sample.session_pct.is_none() && sample.week_pct.is_none() {
        return Vec::new();
    }
    let Some(record) = account_mut(state, account) else {
        return Vec::new();
    };
    if record
        .sample
        .as_ref()
        .is_some_and(|held| sample.at < held.at.min(now) || sample.at == held.at)
    {
        return Vec::new();
    }
    if [sample.session_pct, sample.week_pct]
        .iter()
        .flatten()
        .all(|pct| *pct < 100)
    {
        record.room_at = record.room_at.max(Some(sample.at.min(now)));
    }
    // A row the panel lacked leads with `–` and says so, never a bare `?`: a narrow pane may cut
    // a row, so it is not Claude Code reporting no window ([`tap_shown`]).
    let shown = |pct: Option<u32>| {
        pct.map_or_else(|| "– (not on the panel)".to_owned(), |p| format!("{p}%"))
    };
    let event = Event::new(sample.at, Kind::Sample, account).because(&format!(
        "session {} week {}",
        shown(sample.session_pct),
        shown(sample.week_pct)
    ));
    record.before = record.sample.replace(sample);
    record.sample_context = None;
    vec![event]
}

/// How long a usage tap's unchanged figure is held before it is written again ([`note_reading`]).
pub const READING_HEARTBEAT_S: i64 = 15 * 60;

/// How long after another account signed in to the home a running Claude Code may still be served
/// on the login it held ([`note_reading`], [`Account::keyed_at`]).
pub const KEY_ADOPT_S: i64 = 60;

/// One window of a sample as a figure. A read that named no reset gives none: a figure's reset is
/// the vendor's instant, and [`merge_fig`] orders figures by it, so a reset made up here would
/// outrank a real one. [`Account::unnamed_windows`] reads such a window.
fn sample_fig(pct: Option<u32>, reset: Option<i64>, at: i64, sample: &Sample) -> Option<WindowFig> {
    Some(WindowFig {
        pct_x10: pct.filter(|p| *p <= PCT_MAX)?.saturating_mul(10),
        reset: reset?,
        at,
        by: sample.by.clone(),
        source: SOURCE_SAMPLE.to_owned(),
        epoch_at: None,
    })
}

/// Converts a `/usage` [`Sample`] bucket ([`SESSION_LIMIT`] or [`WEEK_LIMIT`]) into a figure;
/// `None` when it has no figure there or `stored` already holds that figure as recently.
#[must_use]
pub fn sample_window(
    sample: Option<&Sample>,
    bucket: &str,
    stored: Option<&WindowFig>,
) -> Option<WindowFig> {
    let s = sample?;
    let f = match bucket {
        SESSION_LIMIT => sample_fig(
            s.session_pct,
            s.session_reset,
            s.session_read_at.unwrap_or(s.at),
            s,
        ),
        WEEK_LIMIT => sample_fig(s.week_pct, s.week_reset, s.week_read_at.unwrap_or(s.at), s),
        _ => None,
    }?;
    stored
        .is_none_or(|held| {
            (held.whole() * 10, held.reset) != (f.pct_x10, f.reset) || f.at > held.at
        })
        .then_some(f)
}

/// Records an additional settled `/usage` window, such as Kimi Code's monthly total, which is
/// distinct from the session and legacy weekly buckets in [`Sample`].
pub fn note_panel_window(
    state: &mut State,
    account: &str,
    bucket: &str,
    pct: u32,
    reset: Option<i64>,
    at: i64,
) -> Vec<Event> {
    if pct > PCT_MAX || bucket.is_empty() {
        return Vec::new();
    }
    let Some(reset) = reset else {
        return Vec::new();
    };
    let by = state.machine.clone();
    let Some(record) = account_mut(state, account) else {
        return Vec::new();
    };
    if record.windows.get(bucket).is_some_and(|held| held.at >= at) {
        return Vec::new();
    }
    record.windows.insert(
        bucket.to_owned(),
        WindowFig {
            pct_x10: pct.saturating_mul(10),
            reset,
            at,
            by,
            source: SOURCE_SAMPLE.to_owned(),
            epoch_at: None,
        },
    );
    vec![Event::new(at, Kind::Sample, account).because(&format!("{bucket} {pct}% resets {reset}"))]
}

fn fig_news(f: Option<&WindowFig>) -> Option<(u32, i64, Option<i64>)> {
    f.map(|f| (f.pct_x10, f.reset, f.epoch_at))
}

/// A window's figure in a usage tap's ledger row (`tap: session 42% week 1%`): the figure the
/// record holds after the reading (`held`, its percentage and reset) while its window is open at
/// `now`, else, never a bare `?` and never a closed window's figure, why there is none. The
/// reading did not carry the window (`carried`), as Claude Code 2.1.316's statusLine leaves out a
/// window the API reply did not report, and nothing open is held from another session or a panel;
/// or what it carried was held back or cut as read before an in-window reset
/// ([`WindowFig::cut`]), or its window has closed. The figure then leads with `–`, so no reader
/// takes a number off it.
fn tap_shown(held: Option<(u32, i64)>, carried: bool, now: i64) -> String {
    match held {
        Some((pct, reset)) if reset > now => format!("{pct}%"),
        _ if !carried => "– (not reported by Claude Code)".to_owned(),
        Some(_) => "– (window reset)".to_owned(),
        None => "– (read before a reset)".to_owned(),
    }
}

/// Records a fresh usage-tap [`TapRead`] for `account` at `now`.
pub fn note_reading(state: &mut State, account: &str, read: &TapRead, now: i64) -> Vec<Event> {
    note_reading_held(state, account, read, now).0
}

/// Records a usage-tap [`TapRead`] and returns whether any part was held back during login
/// adoption.
pub fn note_reading_held(
    state: &mut State,
    account: &str,
    read: &TapRead,
    now: i64,
) -> (Vec<Event>, bool) {
    let valid = |w: Option<WindowRead>| w.filter(|w| w.pct_x10 <= 10 * PCT_MAX);
    let reads = [
        (SESSION_LIMIT, valid(read.five_hour)),
        (WEEK_LIMIT, valid(read.seven_day)),
    ];
    if reads.iter().all(|(_, w)| w.is_none()) {
        return (Vec::new(), false);
    }
    let by = state.machine.clone();
    let Some(record) = account_mut(state, account) else {
        return (Vec::new(), false);
    };
    let at = read.at.min(now);
    let mut events = Vec::new();
    if record.of_another(read.key) {
        let Account {
            walls: _,
            sample: _,
            sample_context: _,
            before: _,
            offer: _,
            auth,
            auth_at,
            auth_scope,
            auth_context,
            renewal_metadata,
            served_metadata,
            auth_projection,
            cleared_at: _,
            room_at: _,
            ran_at,
            credits: _,
            credits_day,
            warned_session: _,
            warned_week: _,
            weeks_said: _,
            key: _,
            keyed_at: _,
            windows: _,
            pk,
        } = std::mem::take(record);
        *record = Account {
            auth,
            auth_at,
            auth_scope,
            auth_context,
            renewal_metadata,
            served_metadata,
            auth_projection,
            cleared_at: Some(now),
            keyed_at: Some(now),
            ran_at,
            credits_day,
            pk,
            ..Account::default()
        };
        events.push(
            Event::new(now, Kind::Epoch, account).because("another account signed in to the home"),
        );
    }
    let mut changed = read.key.is_some() && record.key != read.key;
    if read.key.is_some() {
        record.key = read.key;
    }
    if record.adopting(at) {
        return (events, true);
    }
    let mut held_back = false;
    let sample = record.sample.clone();
    let mut figures: [Option<WindowFig>; 2] = [None, None];
    for (slot, (bucket, w)) in reads.into_iter().enumerate() {
        let stored = record.windows.get(bucket).cloned();
        let from_sample = sample_window(sample.as_ref(), bucket, stored.as_ref());
        let held = merge_fig(stored.as_ref(), from_sample.as_ref(), now);
        let anchored = |w: &WindowRead| held.as_ref().filter(|h| h.reset == w.reset);
        let fig = w.and_then(|w| {
            let epoch = anchored(&w).and_then(|h| h.epoch_at);
            if !read.seen_before && epoch.is_some_and(|e| read.born.is_none_or(|b| b < e)) {
                held_back = true;
                return None;
            }
            let refill = read.seen_before
                && w.refilled.is_some_and(|from| {
                    held.as_ref().is_some_and(|h| {
                        h.reset == w.reset
                            && h.epoch_at.is_none_or(|_| !h.cut() && from <= h.pct_x10)
                    })
                });
            Some(WindowFig {
                pct_x10: w.pct_x10,
                reset: w.reset,
                at,
                by: by.clone(),
                source: SOURCE_TAP.to_owned(),
                epoch_at: refill.then_some(at),
            })
        });
        let merged = merge_fig(held.as_ref(), fig.as_ref(), now);
        let opened = fig.as_ref().and_then(|f| f.epoch_at).is_some_and(|epoch| {
            merged.as_ref().and_then(|m| m.epoch_at) == Some(epoch)
                && w.and_then(|w| anchored(&w)).and_then(|h| h.epoch_at) < Some(epoch)
        });
        if opened {
            events.push(Event::new(now, Kind::Reset, account).because(&format!(
                "{bucket} refilled inside its window (resets {})",
                super::usage::rfc3339_utc(merged.as_ref().map_or(0, |m| m.reset))
            )));
        }
        if merged.is_some() {
            changed |= fig_news(merged.as_ref()) != fig_news(stored.as_ref());
        }
        figures[slot] = merged.or(stored);
    }
    let beat = sample
        .as_ref()
        .is_none_or(|s| now.saturating_sub(s.at) >= READING_HEARTBEAT_S);
    if !changed && !beat {
        return (events, held_back);
    }
    for ((bucket, _), fig) in reads.iter().zip(&figures) {
        if let Some(fig) = fig {
            record.windows.insert((*bucket).to_owned(), fig.clone());
        }
    }
    let live = |f: &Option<WindowFig>| {
        f.as_ref()
            .filter(|f| !f.cut())
            .map(|f| (f.whole(), f.reset))
    };
    let (session, week) = (live(&figures[0]), live(&figures[1]));
    events.push(Event::new(now, Kind::Sample, account).because(&format!(
        "tap: session {} week {}",
        tap_shown(session, reads[0].1.is_some(), now),
        tap_shown(week, reads[1].1.is_some(), now)
    )));
    // A heartbeat publishes now, but does not observe the held windows again. Keep a
    // newer matching panel observation too: equal values do not make it a heartbeat.
    let observed = |slot: usize, prior: Option<(u32, i64, i64)>| {
        let fig = figures[slot].as_ref().filter(|fig| !fig.cut())?;
        Some(
            prior
                .filter(|(pct, reset, _)| (*pct, *reset) == (fig.whole(), fig.reset))
                .map_or(fig.at, |(_, _, read_at)| fig.at.max(read_at)),
        )
    };
    let session_read_at = observed(
        0,
        sample.as_ref().and_then(|s| {
            Some((
                s.session_pct?,
                s.session_reset?,
                s.session_read_at.unwrap_or(s.at),
            ))
        }),
    );
    let week_read_at = observed(
        1,
        sample
            .as_ref()
            .and_then(|s| Some((s.week_pct?, s.week_reset?, s.week_read_at.unwrap_or(s.at)))),
    );
    let next = Sample {
        session_pct: session.map(|(p, _)| p),
        session_reset: session.map(|(_, r)| r),
        week_pct: week.map(|(p, _)| p),
        week_reset: week.map(|(_, r)| r),
        session_read_at,
        week_read_at,
        at: now,
        by,
    };
    record.before = record.sample.take();
    record.sample = Some(next);
    record.sample_context = None;
    (events, held_back)
}

/// Records a limit-reset [`Offer`] observed at a wall on `account`.
pub fn note_offer(
    state: &mut State,
    account: &str,
    kind: Option<ResetKind>,
    left: u32,
    use_by: Option<i64>,
    at: i64,
    now: i64,
) -> bool {
    let by = state.machine.clone();
    let Some(record) = account_mut(state, account) else {
        return false;
    };
    if record.offer.as_ref().is_some_and(|o| at < o.at.min(now)) {
        return false;
    }
    record.offer = Some(Offer {
        kind,
        left,
        use_by,
        at,
        by,
    });
    true
}

/// Records an [`Auth`] observation for `account` at `at`. In a contextual Codex store this
/// retains history only; a scoped observation must validate current credentials before
/// affecting a pin or supplying current auth authority.
pub fn note_auth(state: &mut State, account: &str, auth: Auth, at: i64) -> Vec<Event> {
    let mut out = Vec::new();
    if auth == Auth::Unseen {
        return out;
    }
    let contextual = state.contextual_auth;
    let Some(record) = account_mut(state, account) else {
        return out;
    };
    if (contextual || record.managed_auth_is_current())
        && record.auth_at.is_some_and(|held| at < held)
    {
        return out;
    }
    record.clear_auth_context();
    let changed = record.auth != auth;
    let reactivated = !contextual && !record.managed_auth_is_current();
    record.auth = auth;
    record.auth_at = Some(at);
    if !contextual {
        record.auth_scope.observe_entry();
    }
    if changed {
        out.push(Event::new(at, Kind::Auth, account).because(auth.as_str()));
    }
    if auth == Auth::SignedOut && !contextual && (changed || reactivated) {
        end_pin(state, account, "signed-out", at, &mut out);
    }
    out
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PendingAuthKind {
    Word(Auth),
    LoginEnd,
    SignedBack,
    CheckedSignIn,
    Check,
}

/// A transient observation whose authority is checked against the final publication.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingAuth {
    raw: Row,
    context: AuthContext,
    kind: PendingAuthKind,
    at: i64,
}

fn stage_auth_kind(
    state: &mut State,
    raw: &Row,
    _logins: &Logins,
    context: AuthContext,
    kind: PendingAuthKind,
    at: i64,
) {
    state.pending_auth.push(PendingAuth {
        raw: raw.clone(),
        context,
        kind,
        at,
    });
}

/// Stages an explicit auth result without applying pin or lifecycle effects yet.
pub(crate) fn stage_auth(
    state: &mut State,
    raw: &Row,
    logins: &Logins,
    context: AuthContext,
    word: Auth,
    at: i64,
) {
    stage_auth_kind(state, raw, logins, context, PendingAuthKind::Word(word), at);
}

/// Stages a current-login failure including its learned lifetime metadata.
pub(crate) fn stage_login_end(
    state: &mut State,
    raw: &Row,
    logins: &Logins,
    context: AuthContext,
    at: i64,
) {
    stage_auth_kind(state, raw, logins, context, PendingAuthKind::LoginEnd, at);
}

/// Stages a served login's recovery, preserving the existing signed-back semantics.
pub(crate) fn stage_signed_back(
    state: &mut State,
    raw: &Row,
    logins: &Logins,
    context: AuthContext,
    at: i64,
) {
    stage_auth_kind(state, raw, logins, context, PendingAuthKind::SignedBack, at);
}

/// Stages verified sign-in metadata for the final login.
pub(crate) fn stage_checked_sign_in(
    state: &mut State,
    raw: &Row,
    logins: &Logins,
    context: AuthContext,
    at: i64,
) {
    stage_auth_kind(
        state,
        raw,
        logins,
        context,
        PendingAuthKind::CheckedSignIn,
        at,
    );
}

/// Requires a current context for other effects without recording an auth result.
pub(crate) fn stage_auth_check(
    state: &mut State,
    raw: &Row,
    logins: &Logins,
    context: AuthContext,
    at: i64,
) {
    stage_auth_kind(state, raw, logins, context, PendingAuthKind::Check, at);
}

/// Records the account `sid` is running on and any hold reason.
pub fn note_session(
    state: &mut State,
    sid: &str,
    account: &str,
    held: Option<&str>,
    now: i64,
) -> Vec<Event> {
    let sid = one_line(sid, WORD_MAX);
    if sid.is_empty() || !admissible_label(account) {
        return Vec::new();
    }
    let out = state
        .sessions
        .get(&sid)
        .filter(|old| old.account != account)
        .map(|old| {
            Event::new(now, Kind::Move, &old.account)
                .with_to(account)
                .with_sid(&sid)
        })
        .into_iter()
        .collect();
    if let Some(record) = account_mut(state, account) {
        record.ran_at = Some(record.ran_at.map_or(now, |held| held.max(now)));
    }
    state.sessions.insert(
        sid,
        SessionRow {
            account: account.to_owned(),
            at: now,
            held: held
                .map(|h| one_line(h, WORD_MAX))
                .filter(|h| !h.is_empty()),
        },
    );
    out
}

/// Notes that Claude Code conversation `conversation` runs under a login of a row's own
/// directory from `since` on ([`State::login_dir_spans`]), by a launch onto such a login whose
/// process is still to start (a move, a cold relaunch): a stretch that has not ended by then (nor
/// lapsed, [`LoginDirSpans::lapse`]) joins it, the process it was last seen in is forgotten, and
/// `now` is when it was noted. What no record holds any more at `now` goes
/// ([`LoginDirSpans::kept_at`]). Returns whether the record changed; a word that is not a
/// conversation id ([`super::upgrade::is_session_id`]) changes nothing.
pub fn note_on_login_dir(state: &mut State, conversation: &str, since: i64, now: i64) -> bool {
    note_on(state, conversation, since, None, now)
}

/// [`note_on_login_dir`] for `process`, seen running `conversation` under such a login since
/// `since`, its start: the process its open stretch runs in ([`LoginDirSpans::seen_running`]).
pub fn note_seen_on_login_dir(
    state: &mut State,
    conversation: &str,
    process: &NotedProcess,
    since: i64,
    now: i64,
) -> bool {
    note_on(state, conversation, since, Some(process), now)
}

fn note_on(
    state: &mut State,
    conversation: &str,
    since: i64,
    process: Option<&NotedProcess>,
    now: i64,
) -> bool {
    if !super::upgrade::is_session_id(conversation) {
        return false;
    }
    let before = state.login_dir_spans.clone();
    let was = state.login_dir_spans.get(conversation);
    let noted = LoginDirSpans {
        spans: spans_opened(&was.map_or_else(Vec::new, |was| was.spans_at(since)), since),
        at: was.map_or(now, |was| was.at.max(now)),
        process: process
            .filter(|process| process.start.len() <= NOTED_START_MAX)
            .cloned(),
    };
    state.login_dir_spans.insert(conversation.to_owned(), noted);
    login_dir_spans_kept(state, now);
    state.login_dir_spans != before
}

/// Notes that conversation `conversation` runs on the default login from `from` on: a stretch
/// of it under a row's own directory ends there at the latest (at its lapse, where that came
/// first), and one that began later never ran (a move onto such a login that was not made), so
/// it goes; a record left with none goes, as does what no record holds any more at `now`.
/// `from` is a process's start in whole seconds (`lstart`), as the transcript's row times are,
/// so its own second stays the stretch's ([`off_dir_from`]). Returns whether the record changed.
pub fn note_off_login_dir(state: &mut State, conversation: &str, from: i64, now: i64) -> bool {
    let before = state.login_dir_spans.clone();
    let from = off_dir_from(from);
    if let Some(was) = state.login_dir_spans.get(conversation) {
        let spans = spans_cut(&was.spans_at(from), from);
        if spans != was.spans {
            let at = was.at.max(now);
            let process = None;
            state.login_dir_spans.insert(
                conversation.to_owned(),
                LoginDirSpans { spans, at, process },
            );
        }
    }
    login_dir_spans_kept(state, now);
    state.login_dir_spans != before
}

/// Whether noting `conversation` as running under a row's own directory (`on_dir`) or on the
/// default login from `from` on ([`note_on_login_dir`], [`note_off_login_dir`]) would leave its
/// stretches as they are.
#[must_use]
pub fn login_dir_spans_hold(state: &State, conversation: &str, on_dir: bool, from: i64) -> bool {
    let Some(noted) = state.login_dir_spans.get(conversation) else {
        return !on_dir;
    };
    if on_dir {
        spans_opened(&noted.spans_at(from), from) == noted.spans
    } else {
        let from = off_dir_from(from);
        spans_cut(&noted.spans_at(from), from) == noted.spans
    }
}

/// Where a stretch under a row's own login ends for a process seen on the default login since
/// `started`, a whole second: after that second. A row the transcript times in it (whole seconds
/// too) may be the process before's, still on the row's login: its wall, met in the second the
/// relaunch onto the default login started, belongs to the row it ran on, and a new process
/// meets no wall in its first second.
fn off_dir_from(started: i64) -> i64 {
    started.saturating_add(1)
}

/// Removes `sid` from the session table and clears its canary hold if it was the canary.
pub fn forget_session(state: &mut State, sid: &str) -> bool {
    if sid.is_empty() {
        return false;
    }
    let named = |c: &Option<Canary>| c.as_ref().is_some_and(|c| c.sid == sid);
    let canary = named(&state.canary);
    let passed = state.undo.iter().rposition(|over| named(&over.canary));
    if canary {
        state.canary = None;
        state.undo.clear();
    } else if let Some(k) = passed {
        state.undo.drain(..k);
        if let Some(over) = state.undo.first_mut() {
            over.canary = None;
        }
    }
    state.sessions.remove(sid).is_some() || canary || passed.is_some()
}

/// Removes session rows not updated within [`SESSION_KEEP_S`] of `now`.
pub fn drop_stale_sessions(state: &mut State, now: i64) -> usize {
    let before = state.sessions.len();
    state
        .sessions
        .retain(|_, row| now.saturating_sub(row.at) <= SESSION_KEEP_S);
    before - state.sessions.len()
}

/// Returns whether an existing probe claim prevents claiming a new `/usage` probe for `account` at
/// `now`.
#[must_use]
pub fn probe_held(state: &State, account: &str, now: i64) -> bool {
    state.probe.as_ref().is_some_and(|c| {
        let within = if c.account == account {
            PROBE_MIN_S
        } else {
            PROBE_APART_S
        };
        recent(c.at, now, within)
    })
}

/// Claims the `/usage` probe slot for `account` and `sid` at `now` if unheld.
pub fn claim_probe(state: &mut State, account: &str, sid: &str, now: i64) -> Option<Event> {
    let sid = one_line(sid, WORD_MAX);
    if sid.is_empty() || !admissible_label(account) {
        return None;
    }
    if probe_held(state, account, now) {
        return None;
    }
    state.probe = Some(ProbeClaim {
        account: account.to_owned(),
        sid: sid.clone(),
        at: now,
    });
    Some(Event::new(now, Kind::Probe, account).with_sid(&sid))
}

/// Outcome of attempting to claim a limit-reset slot via [`claim_reset`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResetClaimed {
    /// The slot is this session's from now.
    Taken,
    /// It already was: this session and this process claimed this wall.
    Mine,
    /// A claim is in flight that is not this session's own for this wall ([`RESET_CLAIM_S`]).
    Held {
        /// When that claim lapses.
        until: i64,
    },
    /// This wall was attempted already and is not attempted again.
    Attempted,
}

/// Attempts to claim the single limit-reset slot for `account`'s standing wall.
pub fn claim_reset(
    state: &mut State,
    account: &str,
    wall: (&str, i64),
    sid: &str,
    pid: u32,
    start: &str,
    now: i64,
) -> ResetClaimed {
    let (window, until) = wall;
    let sid = one_line(sid, WORD_MAX);
    let start = one_line(start, WORD_MAX);
    let window = one_line(window, WORD_MAX);
    if sid.is_empty()
        || start.is_empty()
        || window.is_empty()
        || pid == 0
        || !admissible_label(account)
    {
        return ResetClaimed::Attempted;
    }
    if let Some(held) = &state.reset {
        let flying = held.done.is_none() && recent(held.at, now, RESET_CLAIM_S);
        let this_wall = held.until == Some(until);
        let mine = held.sid == sid
            && held.pid == pid
            && held.start == start
            && held.window.as_deref() == Some(window.as_str());
        let stands_past = held
            .until
            .is_some_and(|theirs| until < theirs && now < theirs);
        if flying && this_wall && mine {
            return ResetClaimed::Mine;
        }
        if flying {
            return ResetClaimed::Held {
                until: held.at.saturating_add(RESET_CLAIM_S),
            };
        }
        if this_wall || stands_past {
            return ResetClaimed::Attempted;
        }
    }
    state.reset = Some(ResetClaim {
        account: account.to_owned(),
        until: Some(until),
        window: Some(window),
        sid,
        at: now,
        pid,
        start,
        done: None,
    });
    ResetClaimed::Taken
}

/// Returns the claim expiration timestamp if a limit reset is currently in flight on `account` at
/// `now`.
#[must_use]
pub fn reset_in_flight(state: &State, account: &str, now: i64) -> Option<i64> {
    state
        .reset
        .as_ref()
        .filter(|c| c.account == account && c.done.is_none() && recent(c.at, now, RESET_CLAIM_S))
        .map(|c| c.at.saturating_add(RESET_CLAIM_S))
}

/// Returns whether a process on `label` started at `started` is eligible to claim a limit reset.
/// # Errors
///
/// The first check that fails, by its word.
pub fn reset_allowed(
    roster: &Roster,
    state: &State,
    label: &str,
    started: i64,
) -> Result<(), &'static str> {
    let Some(row) = roster.row(label) else {
        return Err("not-listed");
    };
    if row.dir.is_some() {
        return Err("not-default");
    }
    if state
        .active
        .as_deref()
        .is_some_and(|active| active != label)
    {
        return Err("active-other");
    }
    let floor = started.saturating_sub(RESET_PROFILE_S);
    let ran = state
        .accounts
        .iter()
        .filter_map(|(other, record)| record.ran_at.map(|at| (other.as_str(), at)))
        .chain(
            state
                .sessions
                .values()
                .map(|session| (session.account.as_str(), session.at)),
        );
    for (other, at) in ran {
        if other != label && at > floor {
            return Err("other-login");
        }
    }
    Ok(())
}

/// Outcome of a claimed limit-reset attempt passed to [`reset_outcome`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResetDone<'a> {
    /// The vendor's own line said the limits were reset.
    Used {
        /// The kind spent.
        kind: ResetKind,
        /// Resets of that kind left after this one.
        left: Option<u32>,
    },
    /// The vendor's own line said a reset was spent and the walled limit still stands.
    Walled {
        /// The kind spent.
        kind: ResetKind,
        /// Resets of that kind left after this one.
        left: u32,
    },
    /// The vendor said the limit had lifted by itself.
    Lifted,
    /// The reset was kept, for this reason (one word of the caller's).
    Kept(&'a str),
    /// The attempt failed or its end was not read, for this reason.
    Failed(&'a str),
}

impl ResetDone<'_> {
    /// Canonical status word stored in [`ResetClaim::done`].
    #[must_use]
    pub const fn word(&self) -> &'static str {
        match self {
            ResetDone::Used { .. } | ResetDone::Walled { .. } => "used",
            ResetDone::Lifted => "lifted",
            ResetDone::Kept(_) => "kept",
            ResetDone::Failed(_) => "failed",
        }
    }
}

fn reset_reason(why: &str) -> String {
    let line = one_line(why, WORD_MAX);
    let head = line.split(" · ").next().unwrap_or_default();
    let end = head.find(['/', '\\', '@', '~']).unwrap_or(head.len());
    head.get(..end).unwrap_or_default().trim().to_owned()
}

/// Records the outcome of a claimed limit-reset attempt on `account` at `now`.
pub fn reset_outcome(
    state: &mut State,
    account: &str,
    until: i64,
    sid: &str,
    done: &ResetDone<'_>,
    now: i64,
) -> Vec<Event> {
    let sid = one_line(sid, WORD_MAX);
    let Some(claim) = state.reset.as_mut().filter(|c| {
        c.account == account && c.until == Some(until) && c.sid == sid && c.done.is_none()
    }) else {
        return Vec::new();
    };
    claim.done = Some(done.word().to_owned());
    let claimed = claim.at;
    let window = claim.window.clone();
    let machine = state.machine.clone();
    let (kind, why) = match done {
        ResetDone::Used { kind, left } => (
            Kind::ResetUsed,
            left.map_or_else(
                || kind.name().to_owned(),
                |n| format!("{} left", kind.resets(n)),
            ),
        ),
        ResetDone::Walled { kind, left } => (
            Kind::ResetUsed,
            format!("{} left, still walled", kind.resets(*left)),
        ),
        ResetDone::Lifted => (Kind::ResetKept, ResetDone::Lifted.word().to_owned()),
        ResetDone::Kept(why) => (Kind::ResetKept, reset_reason(why)),
        ResetDone::Failed(why) => (Kind::ResetFailed, reset_reason(why)),
    };
    if matches!(done, ResetDone::Used { .. } | ResetDone::Lifted)
        && let Some(window) = window
        && let Some(record) = account_mut(state, account)
        && let Some(wall) = record.walls.get_mut(&window)
        && wall.until == Some(until)
        && wall.at <= claimed
        && !wall.cleared
    {
        let used = matches!(done, ResetDone::Used { .. });
        let stood = wall.stands_until().is_some_and(|end| claimed < end);
        let record_by = machine.clone();
        wall.cleared = true;
        wall.at = now;
        wall.by = machine;
        wall.attempts = 0;
        let sample = record.sample.as_ref();
        let bucket = match window.as_str() {
            WEEK_LIMIT => Some((
                &mut record.warned_week,
                sample.filter(|s| s.week_pct.is_some()).map(|s| s.at),
            )),
            SESSION_LIMIT => Some((
                &mut record.warned_session,
                sample.filter(|s| s.session_pct.is_some()).map(|s| s.at),
            )),
            _ => None,
        };
        if stood
            && let Some((warned, sampled)) = bucket
            && sampled.is_none_or(|at| at <= claimed)
            && warned.as_ref().is_none_or(|w| w.at <= claimed)
        {
            *warned = Some(WindowSaid {
                pct: None,
                reset: (!used).then_some(until),
                at: now,
            });
        }
        if used
            && stood
            && let Some(fig) = record.windows.get_mut(window.as_str())
        {
            fig.epoch_at = fig.epoch_at.max(Some(now));
        }
        if stood
            && week_model(&window).is_some()
            && record
                .weeks_said
                .get(window.as_str())
                .is_none_or(|said| said.at <= claimed)
        {
            record.weeks_said.insert(
                window.clone(),
                WeekSaid {
                    pct: None,
                    reset: (!used).then_some(until),
                    at: now,
                    by: record_by,
                    wall: false,
                    ends_by: None,
                },
            );
        }
    }
    vec![Event::new(now, kind, account).with_sid(&sid).because(&why)]
}

/// Returns whether the machine's hourly account-rotation circuit breaker is spent at `now`.
#[must_use]
pub fn rotations_spent(state: &State, roster: &Roster, now: i64) -> bool {
    let counted = state
        .rotations
        .iter()
        .filter(|t| recent(**t, now, ROTATION_WINDOW_S))
        .count();
    counted >= roster.rows.len().saturating_mul(2)
}

/// Returns the timestamp when a spent rotation circuit breaker reopens.
#[must_use]
pub fn breaker_until(state: &State, roster: &Roster, now: i64) -> Option<i64> {
    let bound = roster.rows.len().saturating_mul(2);
    if bound == 0 {
        return None;
    }
    let mut counted: Vec<i64> = state
        .rotations
        .iter()
        .copied()
        .filter(|t| recent(*t, now, ROTATION_WINDOW_S))
        .collect();
    counted.sort_unstable();
    let over = counted.len().checked_sub(bound)?;
    counted
        .get(over)
        .map(|t| t.saturating_add(ROTATION_WINDOW_S))
}

/// Reason a compare-and-set rotation in [`begin_rotation`] was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lost {
    /// `active` is not what the caller read.
    Raced {
        /// What `active` is.
        active: Option<String>,
    },
    /// `active` as read can be run on, this Mac may use it or the owner pinned it, and it is not
    /// rotated past.
    Held {
        /// That account.
        active: String,
    },
    /// The owner's pin holds.
    Pinned,
    /// `to` is not a row, is the session's own or the active account, or cannot be run on now.
    NotACandidate,
    /// The machine's rotation bound is spent.
    Spent,
    /// The mover has no session id.
    NoSession,
}

/// Session and expected state snapshot for [`begin_rotation`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mover<'a> {
    /// Its session id: the canary's, once the rotation is committed.
    pub sid: &'a str,
    /// The account its process runs on.
    pub account: &'a str,
}

/// Commits an atomic compare-and-set rotation moving `mover` onto candidate account `to`
/// ([`Lost`]).
/// # Errors
///
/// A [`Lost`], with nothing changed.
pub fn begin_rotation(
    state: &mut State,
    roster: &Roster,
    enrolled: &BTreeSet<String>,
    expect_active: Option<&str>,
    mover: Mover<'_>,
    to: &str,
    now: i64,
) -> Result<Event, Lost> {
    let sid = one_line(mover.sid, WORD_MAX);
    if sid.is_empty() {
        return Err(Lost::NoSession);
    }
    if state.active.as_deref() != expect_active {
        return Err(Lost::Raced {
            active: state.active.clone(),
        });
    }
    let me = mover.account;
    let cond = |label: &str| {
        roster
            .row(label)
            .filter(|r| r.held_by.is_none())
            .map(|r| condition(r, state, is_enrolled(r, enrolled), now))
    };
    let runs = |label: &str| cond(label).is_some_and(Condition::runs);
    let fresh = |label: &str| cond(label).is_some_and(Condition::fresh);
    // An `active` this Mac may not use (another Mac's senior claim, one it cannot record) holds
    // nothing, as [`advise`] moves no session onto it; where another Mac holds it, every session
    // here on it yields, so none answers its canary. A pin holds whatever the claim: only
    // `accounts use --auto` ends it, and a rotation would clear it unannounced.
    let active_runs = expect_active.is_some_and(|active| {
        runs(active) && (state.pinned || state.lease_verdict(active, now).is_ok())
    });
    if let Some(active) = expect_active
        && active_runs
    {
        let held = Err(Lost::Held {
            active: active.to_owned(),
        });
        if !state.proven {
            return held;
        }
        if state.pinned {
            return if active == me {
                Err(Lost::Pinned)
            } else {
                held
            };
        }
        if active != me && (fresh(active) || !fresh(to)) {
            return held;
        }
    }
    if to == me || expect_active == Some(to) || !runs(to) || state.execution_verdict(to).is_err() {
        return Err(Lost::NotACandidate);
    }
    if rotations_spent(state, roster, now) {
        return Err(Lost::Spent);
    }
    let from = expect_active.unwrap_or(me).to_owned();
    let proven = state.active.is_some() && state.proven;
    let over = Undo {
        previous: std::mem::replace(&mut state.previous, state.active.take()),
        previous_proven: std::mem::replace(&mut state.previous_proven, proven),
        canary: state.canary.replace(Canary {
            sid: sid.clone(),
            at: now,
            took: false,
        }),
    };
    state.undo.push(over);
    let extra = state.undo.len().saturating_sub(MAX_UNDO);
    state.undo.drain(..extra);
    state.active = Some(to.to_owned());
    state.proven = false;
    state.pinned = false;
    state
        .rotations
        .retain(|t| recent(*t, now, ROTATION_WINDOW_S));
    state.rotations.push(now);
    Ok(Event::new(now, Kind::Rotate, &from)
        .with_to(to)
        .with_sid(&sid))
}

/// Replaces the timed-out canary `expect` on `active` with `sid`, by compare-and-set.
pub fn take_canary(
    state: &mut State,
    expect: Option<&Canary>,
    sid: &str,
    now: i64,
) -> Option<Event> {
    let sid = one_line(sid, WORD_MAX);
    let active = state.active.clone()?;
    if sid.is_empty() || state.proven || state.canary.as_ref() != expect || state.canary_holds(now)
    {
        return None;
    }
    let silent = state.canary.replace(Canary {
        sid: sid.clone(),
        at: now,
        took: true,
    });
    state.undo.clear();
    let why = match silent {
        Some(silent) if !silent.sid.is_empty() => {
            format!("canary: in place of {}, which did not answer", silent.sid)
        }
        _ => "canary: no session held it".to_string(),
    };
    Some(
        Event::new(now, Kind::Rotate, &active)
            .with_to(&active)
            .with_sid(&sid)
            .because(&why),
    )
}

/// Marks `account` as proven when it serves a response, releasing any canary hold.
pub fn prove(state: &mut State, account: &str, now: i64) -> Option<Event> {
    if state.active.as_deref() != Some(account) || state.proven {
        return None;
    }
    state.proven = true;
    state.canary = None;
    state.undo.clear();
    let contextual = state.contextual_auth;
    if let Some(record) = account_mut(state, account) {
        record.clear_auth_context();
        record.auth = Auth::Ok;
        record.auth_at = Some(now);
        if !contextual {
            record.auth_scope.observe_entry();
        }
    }
    Some(Event::new(now, Kind::Proven, account))
}

/// Record a response admitted against the current login and bind its proof to that context.
pub fn prove_in_context(
    state: &mut State,
    logins: &Logins,
    account: &str,
    now: i64,
) -> Option<Event> {
    let event = prove(state, account, now);
    if event.is_some() {
        auth_scope::bind_observation(state, logins, account, false);
    }
    event
}

/// Failure reason when a canary session on a newly rotated account fails to serve a response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Strike {
    /// It answered with a usage wall of its own.
    Wall {
        /// The vendor's limit type.
        window: String,
        /// The reset it named.
        until: Option<i64>,
    },
    /// It answered with a login wall.
    SignedOut,
    /// The move failed with no word about the account.
    Failed(String),
    /// The move was never made.
    NotMoved(String),
}

/// Strikes candidate `account` on behalf of `sid`. A strike of the unproven `active` rolls it
/// back to [`State::previous`] ([`State::undo`]), except a [`Strike::NotMoved`] of a canary that
/// took a silent one's place; a [`Strike::NotMoved`] counts only from that account's own canary.
pub fn strike(state: &mut State, account: &str, sid: &str, why: &Strike, now: i64) -> Vec<Event> {
    if !admissible_label(account) {
        return Vec::new();
    }
    let sid = one_line(sid, WORD_MAX);
    let canary = state.active.as_deref() == Some(account) && !state.proven;
    let not_moved = matches!(why, Strike::NotMoved(_));
    let its_own = !sid.is_empty() && state.canary.as_ref().is_some_and(|c| c.sid == sid);
    if not_moved && !(canary && its_own) {
        return Vec::new();
    }
    let word = match why {
        Strike::Wall { window, .. } => format!("wall: {window}"),
        Strike::SignedOut => "signed-out".to_string(),
        Strike::Failed(what) => format!("failed: {what}"),
        Strike::NotMoved(what) => format!("not moved: {what}"),
    };
    let mut struck = Event::new(now, Kind::Struck, account)
        .with_sid(&sid)
        .because(&word);
    if canary {
        let held = state.canary.take();
        let took = held.as_ref().is_some_and(|c| c.took);
        let undoes = not_moved && !took;
        if undoes || !not_moved {
            state.active = state.previous.take();
            state.proven = state.active.is_some() && state.previous_proven;
            state.previous_proven = false;
            state.pinned = false;
            struck.to.clone_from(&state.active);
            if let Some(over) = state.undo.pop() {
                state.previous = over.previous;
                state.previous_proven = over.previous_proven;
                state.canary = over.canary;
            }
        }
        if undoes {
            let own = held.and_then(|c| state.rotations.iter().rposition(|t| *t == c.at));
            match own {
                Some(k) => {
                    state.rotations.remove(k);
                }
                None => {
                    state.rotations.pop();
                }
            }
        }
    }
    let mut out = vec![struck];
    out.extend(match why {
        Strike::Wall { window, until } => note_wall(state, account, window, *until, now, now),
        Strike::SignedOut => note_auth(state, account, Auth::SignedOut, now),
        Strike::Failed(_) => note_wall(state, account, STRUCK_WINDOW, None, now, now),
        Strike::NotMoved(_) => Vec::new(),
    });
    out
}

/// Why [`pin`] refuses a label ([`pin_refusal`]), which `accounts use` names the way past by.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PinRefusal {
    /// The roster lists no row of that label.
    NotListed,
    /// The row is held for preparation only, with no execution authority here.
    PreparationOnly,
    /// This selection already failed to prepare the row.
    PreparationFailed,
    /// No login of the row is made on this machine.
    NotEnrolled,
    /// The row's login is known signed out.
    SignedOut,
}

impl PinRefusal {
    fn message(self, label: &str) -> String {
        match self {
            Self::NotListed => format!("`{}` is not in the roster", shown(label)),
            Self::PreparationOnly => {
                format!("`{label}` is prepared but has no execution authority")
            }
            Self::PreparationFailed => format!("`{label}` preparation failed in this selection"),
            Self::NotEnrolled => format!(
                "`{label}` is not-enrolled: no login was made for it on this machine (its login \
                 directory is missing or empty)"
            ),
            Self::SignedOut => format!("`{label}` is signed-out"),
        }
    }
}

/// Why [`pin`] would refuse `label` in `state` (with the rows whose login is here, `enrolled`),
/// `None` where it pins it.
pub(crate) fn pin_refusal(
    state: &State,
    roster: &Roster,
    enrolled: &BTreeSet<String>,
    label: &str,
) -> Option<PinRefusal> {
    let Some(row) = roster.row(label) else {
        return Some(PinRefusal::NotListed);
    };
    if state.prepared_accounts.contains(label) {
        return Some(PinRefusal::PreparationOnly);
    }
    if state.excluded_accounts.contains(label) {
        return Some(PinRefusal::PreparationFailed);
    }
    if !is_enrolled(row, enrolled) {
        return Some(PinRefusal::NotEnrolled);
    }
    state
        .accounts
        .get(label)
        .is_some_and(|a| a.current_auth(row) == Auth::SignedOut)
        .then_some(PinRefusal::SignedOut)
}

/// What an explicit account selection saved under the store lock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PinSelection {
    /// The account is eligible and is now pinned.
    Pinned,
    /// The account needs sign-in; this exact selection is saved for its completion.
    Renew(PinRequest),
}

/// Pins `label` as the active account (`accounts use <label>`) and clears its standing walls.
/// # Errors
///
/// By name: a label the roster does not list, one not enrolled on this
/// machine, one known signed out, one held for preparation only, or one whose current
/// preparation failed. Nothing is changed.
pub fn pin(
    state: &mut State,
    roster: &Roster,
    enrolled: &BTreeSet<String>,
    label: &str,
    now: i64,
) -> Result<Event, String> {
    if let Some(why) = pin_refusal(state, roster, enrolled, label) {
        return Err(why.message(label));
    }
    state.pending_pin = None;
    if let Some(record) = account_mut(state, label) {
        record.walls.clear();
        record.cleared_at = Some(now);
    }
    if state.active.as_deref() != Some(label) {
        state.previous_proven = state.active.is_some() && state.proven;
        state.previous = state.active.take();
        state.active = Some(label.to_owned());
    }
    state.proven = true;
    state.canary = None;
    state.undo.clear();
    state.pinned = true;
    Ok(Event::new(now, Kind::Use, label))
}

/// Clears an operator account pin (`accounts use --auto`).
pub fn unpin(state: &mut State, why: &str, now: i64) -> Option<Event> {
    state.pending_pin = None;
    if !state.pinned {
        return None;
    }
    state.pinned = false;
    let account = state.active.clone().unwrap_or_default();
    Some(Event::new(now, Kind::Unpin, &account).because(why))
}

/// Completes only the explicit selection that still owns the pending request.
pub(super) fn complete_requested_pin(
    state: &mut State,
    roster: &Roster,
    enrolled: &BTreeSet<String>,
    request: &PinRequest,
    now: i64,
) -> Option<Event> {
    if state
        .pending_pin
        .as_ref()
        .and_then(PendingSelection::account)
        != Some(request)
    {
        return None;
    }
    pin(state, roster, enrolled, request.label(), now).ok()
}

const LOCK_FILE: &str = "accounts.lock";

const STATE_FILE: &str = "state.json";

const STATE_WITNESS_FILE: &str = "state.seen";

const LOGINS_WITNESS_FILE: &str = "logins.seen";

const LEDGER_FILE: &str = "ledger.jsonl";

const WAKE_FILE: &str = "wake";

const MACHINE_FILE: &str = "machine";

fn is_machine_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= WORD_MAX && aterm_codec::hex::is_hex(id, id.len())
}

fn absent_at_open(look: io::Result<std::fs::Metadata>) -> bool {
    match look {
        Err(e) => e.kind() == io::ErrorKind::NotFound,
        Ok(meta) => !meta.file_type().is_symlink(),
    }
}

/// Reads at most `max` bytes from a regular file at `path`, returning `Ok(None)` if absent.
pub(crate) fn read_bounded(path: &Path, max: u64) -> io::Result<Option<String>> {
    use std::io::Read as _;
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC);
    }
    let file = match options.open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return if absent_at_open(std::fs::symlink_metadata(path)) {
                Ok(None)
            } else {
                Err(e)
            };
        }
        Err(e) => return Err(e),
    };
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a regular file",
        ));
    }
    if metadata.len() > max {
        return Err(io::Error::new(
            io::ErrorKind::FileTooLarge,
            format!("larger than {max} bytes"),
        ));
    }
    let mut bytes = Vec::with_capacity(
        usize::try_from(metadata.len())
            .unwrap_or(0)
            .saturating_add(1),
    );
    file.take(max.saturating_add(1)).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max {
        return Err(io::Error::new(
            io::ErrorKind::FileTooLarge,
            format!("larger than {max} bytes"),
        ));
    }
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "not UTF-8"))
}

/// On-disk store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Store {
    /// The directory (0700).
    pub dir: PathBuf,
    /// The root of its rows' login directories ([`Store::logins_dir`]).
    logins: PathBuf,
    /// The pauses of its bounded lock takes ([`LOCK_LADDER`], [`Store::signin_within`]).
    ladder: &'static [std::time::Duration],
    /// The CLI whose pool it holds ([`Store::at_for`]).
    cli: Cli,
    /// The user home a Claude Code store opened by [`Store::at_for`] reads its default login
    /// under ([`Store::default_login_home`]); `None` for a [`Store::at`] store and every other
    /// CLI's, whose user home is [`Store::user_home`].
    claude_user_home: Option<PathBuf>,
}

/// The account a process observation was read against. The row is from a bound roster;
/// recording checks it against the raw roster and current bindings again under the lock.
#[derive(Debug, Clone, Copy)]
pub struct LoginObservation<'a> {
    /// The raw roster file, whose current contents must still describe this account.
    pub roster_path: &'a Path,
    /// The complete effective row at observation time, including its login and identity.
    pub row: &'a Row,
    /// The observed process's login and credential-routing facts.
    pub who: &'a ProcessLogin,
}

/// A usage reader's account policy and its source. The implicit default account is
/// distinguished from an explicitly configured row, even when their fields match.
#[derive(Debug, Clone)]
pub struct UsageRoster {
    roster: Roster,
    path: Option<PathBuf>,
    target: Option<PathBuf>,
    store: Store,
    implicit: bool,
}

impl UsageRoster {
    /// The effective rows captured by the reader.
    #[must_use]
    pub fn roster(&self) -> &Roster {
        &self.roster
    }

    /// Whether this is the default account used in the absence of configured rows.
    #[must_use]
    pub const fn implicit(&self) -> bool {
        self.implicit
    }
}

impl std::ops::Deref for UsageRoster {
    type Target = Roster;
    fn deref(&self) -> &Self::Target {
        self.roster()
    }
}

/// Core recorder tests supply their policy directly; materialize it so they exercise
/// the same writer admission as runtime observations without requiring a read first.
#[cfg(test)]
pub(super) fn usage_fixture(
    store: &Store,
    roster: &Roster,
) -> (aterm_tempfile::TempDir, UsageRoster) {
    let directory = aterm_tempfile::TempDir::new().unwrap();
    let implicit = *roster == Roster::own();
    let path = (!implicit).then(|| directory.path().join("accounts.toml"));
    if let Some(path) = &path {
        let text: String = roster
            .rows
            .iter()
            .map(|row| row_text(row).unwrap())
            .collect();
        if std::fs::read_to_string(path).ok().as_deref() != Some(&text) {
            std::fs::write(path, text).unwrap();
        }
    }
    let captured = UsageRoster {
        roster: roster.clone(),
        target: usage_target(path.as_deref()).unwrap(),
        path,
        store: store.clone(),
        implicit,
    };
    (directory, captured)
}

/// Why a bounded read of the current roster and its local bindings could not complete.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RosterReadError {
    /// The roster path or its contents could not be read as a roster.
    Roster(String),
    /// The writer, receipt, state or bindings were unavailable, or publication spanned the read.
    Records(String),
}

/// An account observation's roster, bindings and state read within one completed publication.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AccountRecords {
    /// The configured roster before local bindings, retained for later policy admission.
    pub raw_roster: Roster,
    /// The effective roster after applying local login bindings.
    pub roster: Roster,
    /// Whether the supplied roster path held a file, including an empty roster.
    pub roster_present: bool,
    /// The account state from the same publication, with [`Store::load`]'s local machine identity.
    pub state: State,
    /// The login bindings used to form the effective roster.
    pub logins: Logins,
    /// Nonfatal state and binding diagnostics, in report order.
    pub notes: Vec<String>,
}

impl LoginObservation<'_> {
    /// Whether this process still belongs to the same account, including retained logins.
    fn matches_account(&self, current: &Roster) -> bool {
        account_matches(self.row, self.who, current)
    }

    /// Whether this observation still describes the current row in an effective roster.
    pub(super) fn matches_current(&self, current: &Roster) -> bool {
        let me = self.who.me(current);
        me.default_home
            && me.route.on_account()
            && me.account == Some(self.row.label.as_str())
            && current.row(&self.row.label) == Some(self.row)
    }
}

fn account_matches(captured: &Row, who: &ProcessLogin, current: &Roster) -> bool {
    let Some(row) = current.row(&captured.label) else {
        return false;
    };
    if judged_label(current, who) != Some(captured.label.as_str()) {
        return false;
    }
    let mut captured = captured.clone();
    captured.dir.clone_from(&row.dir);
    captured.was.clone_from(&row.was);
    captured == *row
}

/// The raw roster, effective target and bindings captured before a renewal starts.
#[derive(Clone, Copy)]
pub(super) struct RenewalPolicy<'a> {
    pub roster_path: &'a Path,
    pub roster: &'a Roster,
    pub row: &'a Row,
    pub logins: &'a Logins,
}

impl RenewalPolicy<'_> {
    /// Require the captured roster, effective row and login parent under the writer.
    ///
    /// # Errors
    /// Refuses when the captured roster, effective row or login parent changed.
    pub(super) fn require_current(self, raw: &Roster, logins: &Logins) -> Result<(), String> {
        if raw != self.roster || bound(raw, logins).row(&self.row.label) != Some(self.row) {
            return Err(format!(
                "account `{}` configuration or login changed during sign-in; nothing recorded",
                self.row.label
            ));
        }
        RenewalParent::capture(self.logins, &self.row.label)
            .require_current(logins, &self.row.label)
    }
}

/// The machine-wide sign-in lock's file, in Claude Code's store ([`Store::signin_within`]).
pub const SIGNIN_LOCK_FILE: &str = "signin.lock";

/// What a sign-in that finds [`SIGNIN_LOCK_FILE`] held past its ladder reports.
pub const SIGNIN_HELD: &str = "another sign-in holds the machine's sign-in lock";

/// Exclusive `flock(2)` guard on a store lock file (`accounts.lock`, [`SIGNIN_LOCK_FILE`]) that
/// unlocks on drop.
#[derive(Debug)]
pub struct Lock(std::fs::File);

// Inspect the opened handle before using it as a lock or publication witness. Unix opens
// must neither follow a link nor wait on a special file before the caller can reject it.
fn open_retained_file(path: &Path, kind: &str) -> io::Result<std::fs::File> {
    open_retained_file_by(path, kind, false)
}

#[cfg(unix)]
fn create_retained_file(path: &Path, kind: &str) -> io::Result<std::fs::File> {
    open_retained_file_by(path, kind, true)
}

fn open_retained_file_by(path: &Path, kind: &str, exclusive: bool) -> io::Result<std::fs::File> {
    let not_regular = || {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{}: {kind} is not a regular file", path.display()),
        )
    };
    #[cfg(not(any(unix, windows)))]
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if !metadata.is_file() => return Err(not_regular()),
        Err(why) if why.kind() != io::ErrorKind::NotFound => return Err(why),
        _ => {}
    }
    let mut open = std::fs::OpenOptions::new();
    open.create(!exclusive)
        .create_new(exclusive)
        .truncate(false)
        .write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        open.mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        open.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let file = aterm_uds::kept_out(|| open.open(path))?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(not_regular());
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt as _;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(not_regular());
        }
    }
    Ok(file)
}

// Stock anchor attributes emit associated constants, so release lives outside the trait impl.
impl Lock {
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AccountSigninLock",
            action = "ReleaseA",
            project = "aterm_agent::harness::cli::tests::signin_lock::lock_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AccountSigninLock",
            action = "ReleaseB",
            project = "aterm_agent::harness::cli::tests::signin_lock::lock_transition"
        )
    )]
    fn release(&mut self) {
        let _ = self.0.unlock();
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        self.release();
    }
}

/// Takes the `flock(2)` of the lock file at `path` (made, mode 0600, where it is absent; never one
/// a link names) now and after each pause of `ladder`; `None` while it stays held. Claude Code's
/// setup-token verb takes its per-account lock and `tokens.lock` so
/// (`super::accounts_tokens`), neither of which is `accounts.lock` or [`SIGNIN_LOCK_FILE`].
/// # Errors
/// The file cannot be opened, is not a regular file, or the lock call fails.
pub fn lock_at_within(path: &Path, ladder: &[std::time::Duration]) -> io::Result<Option<Lock>> {
    let mut file = match Store::try_take(open_retained_file(path, "lock")?)? {
        Ok(lock) => return Ok(Some(lock)),
        Err(file) => file,
    };
    for pause in ladder {
        std::thread::sleep(*pause);
        file = match Store::try_take(file)? {
            Ok(lock) => return Ok(Some(lock)),
            Err(file) => file,
        };
    }
    Ok(None)
}

/// A pure snapshot pinned to the actual account writer. No constructor from
/// caller-supplied records; native admission must consume it while held.
pub(in crate::harness) struct NativeRowSnapshot<'a> {
    store: &'a Store,
    roster: PathBuf,
    held: &'a Lock,
    records: AccountRecords,
}
/// Read-only canonical custody pinned to the actual writer. Cancellation
/// retirement needs no roster, provider policy, credential, or stage file read.
pub(in crate::harness) struct NativeRetirementSnapshot<'a> {
    store: &'a Store,
    held: &'a Lock,
    logins: Logins,
}
impl NativeRetirementSnapshot<'_> {
    pub(in crate::harness) fn current(&self) -> Result<(), String> {
        if !self.store.native_writer_matches(self.held) {
            return Err("native retirement writer changed".into());
        }
        Ok(())
    }
    pub(in crate::harness) fn retained_image(&self) -> &Logins {
        &self.logins
    }
}

impl NativeRowSnapshot<'_> {
    pub(in crate::harness) fn current(&self) -> Result<(), String> {
        if !self.store.native_writer_matches(self.held) {
            return Err("native row writer changed".into());
        }
        Ok(())
    }
    pub(in crate::harness) fn roster(&self) -> &Path {
        &self.roster
    }
    pub(in crate::harness) fn account_image(&self) -> &AccountRecords {
        &self.records
    }
    pub(in crate::harness) fn store(&self) -> &Store {
        self.store
    }
}

impl Store {
    /// Opens the accounts [`Store`] rooted under `harness_state`.
    #[must_use]
    pub fn at(harness_state: &Path) -> Store {
        let dir = harness_state.join("accounts");
        Store {
            logins: dir.join("logins"),
            dir,
            ladder: store_ladder(),
            cli: Cli::Claude,
            claude_user_home: None,
        }
    }

    /// Opens `cli`'s [`Store`] ([`Cli`]): Claude Code's is [`Store::at`] that reads its default
    /// login under `user_home` ([`Store::default_login_home`]); another CLI's is
    /// `<harness_state>/<store_dir>`, its rows' logins under `<user_home>/.aterm/logins/<cli>`,
    /// a path that names the CLI and that its own sandbox lets it read.
    #[must_use]
    pub fn at_for(harness_state: &Path, cli: Cli, user_home: &Path) -> Store {
        match cli {
            Cli::Claude => Store {
                claude_user_home: Some(user_home.to_path_buf()),
                ..Store::at(harness_state)
            },
            _ => Store {
                dir: harness_state.join(cli.accounts().store_dir),
                logins: user_home.join(".aterm").join("logins").join(cli.id()),
                ladder: store_ladder(),
                cli,
                claude_user_home: None,
            },
        }
    }

    /// The CLI whose pool it holds.
    #[must_use]
    pub const fn cli(&self) -> Cli {
        self.cli
    }

    /// The CLI's own default home in the user's home that holds this store's logins
    /// (`<home>/.aterm/logins/<cli>`, [`Store::at_for`]); `None` for Claude Code's store.
    #[must_use]
    pub fn default_home(&self) -> Option<PathBuf> {
        Some(self.user_home()?.join(self.cli.accounts().default_home))
    }

    /// The directory whose credential file holds the CLI's default login (a row with no `dir`),
    /// read for its lapse ([`Store::hydrate_lapsed`]): another CLI's [`Store::default_home`];
    /// for Claude Code's store opened under a user home ([`Store::at_for`]), `<home>/.claude`,
    /// the default home a process judged on the default login has (no `CLAUDE_CONFIG_DIR`,
    /// [`judged_label`]), where the platform keeps that login in its `.credentials.json`
    /// ([`crate::ai::connector::claude_default_login_in_file`]). `None` for Claude Code's on
    /// macOS, whose default login is in the keychain, and for a [`Store::at`] store, which knows
    /// no user home: that row is then read by its records alone.
    #[must_use]
    pub fn default_login_home(&self) -> Option<PathBuf> {
        if self.cli != Cli::Claude {
            return self.default_home();
        }
        let home = self.claude_user_home.as_deref()?;
        crate::ai::connector::claude_default_login_in_file()
            .then(|| home.join(self.cli.accounts().default_home))
    }

    /// Captures local credentials and routing without refreshing or changing the login.
    pub(crate) fn capture_auth_context(&self, row: &Row) -> Option<AuthContext> {
        if self.cli != Cli::Codex {
            return None;
        }
        let user_home = self.user_home()?;
        let default = self.default_home()?;
        let home = row
            .dir
            .as_deref()
            .filter(|dir| !dir.is_empty())
            .map_or(default.as_path(), Path::new);
        let credential = super::codex_app_login::current_context(home, user_home)?;
        Some(AuthContext::new(row, credential))
    }

    /// Records `row`'s login signed out under its current credential context, as a read that met
    /// the login's wall does (`accounts_pool`'s `Reading::SignedOut`): how a lab world lays out a
    /// login that lapsed here (one-mesh E4, `aterm-lab local-signins`).
    /// # Errors
    /// The row's credential context cannot be read, or the store cannot be written.
    #[cfg(any(test, debug_assertions))]
    pub fn record_lapse_for_lab(&self, row: &Row, now: i64) -> Result<(), String> {
        let context = self
            .capture_auth_context(row)
            .ok_or("the row's credential context cannot be read")?;
        let mut note = None;
        self.transact_logins(now, &mut note, |state, logins| {
            stage_auth(state, row, logins, context, Auth::SignedOut, now);
            Ok(Vec::new())
        })
        .map(|_| ())
    }

    /// [`Store::load`] with `roster`'s runtime applicability hydrated
    /// ([`Store::hydrate_auth_contexts`]): the state a pick judges rows by, so a row whose own
    /// credential file shows its login lapsed is signed out to it as it is to the report.
    /// # Errors
    /// [`Store::load`]'s.
    pub fn load_judged(
        &self,
        now: i64,
        roster: &Roster,
    ) -> Result<(State, Option<String>), String> {
        let (mut state, note) = self.load(now)?;
        self.hydrate_auth_contexts(&mut state, roster);
        Ok((state, note))
    }

    /// Hydrates only runtime applicability; it creates neither observations nor writes. Every
    /// CLI's [`State::lapsed_logins`] is the rows of `roster` whose own credential file shows its
    /// login lapsed ([`crate::ai::connector::stored_login_lapsed`]): a row's own directory, or
    /// for the default login the directory [`Store::default_login_home`] names (where it names
    /// none, as for Claude Code's on macOS, the default row is read by its records alone).
    pub(crate) fn hydrate_auth_contexts(&self, state: &mut State, roster: &Roster) {
        self.hydrate_lapsed(state, roster);
        if self.cli != Cli::Codex {
            return;
        }
        self.hydrate_codex_contexts(state, roster);
    }

    /// [`State::lapsed_logins`] alone ([`Store::hydrate_auth_contexts`]): the runtime set a
    /// transaction's edit judges rows by, so an edit that picks a row (a restart's advice) skips
    /// a lapsed one as the report and the launch do.
    pub(crate) fn hydrate_lapsed(&self, state: &mut State, roster: &Roster) {
        let default = self.default_login_home();
        state.lapsed_logins = roster
            .rows
            .iter()
            .filter(|row| {
                let home = match row.dir.as_deref().filter(|dir| !dir.is_empty()) {
                    Some(dir) => Path::new(dir),
                    None => match default.as_deref() {
                        Some(home) => home,
                        None => return false,
                    },
                };
                crate::ai::connector::stored_login_lapsed(self.cli.connector(), home)
            })
            .map(|row| row.label.clone())
            .collect();
    }

    fn hydrate_codex_contexts(&self, state: &mut State, roster: &Roster) {
        state.contextual_auth = true;
        for account in state.accounts.values_mut() {
            account.auth_projection = AuthProjection::Unknown;
        }
        for row in &roster.rows {
            if let Some(account) = state.accounts.get_mut(&row.label) {
                account.auth_projection = self
                    .capture_auth_context(row)
                    .map_or(AuthProjection::Unknown, AuthProjection::Current);
            }
        }
    }

    fn finalize_auth(
        &self,
        state: &mut State,
        logins: &mut Logins,
        final_raw: Option<&Roster>,
    ) -> Result<Vec<Event>, String> {
        let pending = std::mem::take(&mut state.pending_auth);
        // Validate the entire publication before any auth, pin or lifecycle effect runs.
        for intent in &pending {
            let raw = match final_raw {
                Some(roster) => roster
                    .row(&intent.raw.label)
                    .ok_or("auth account is absent from the final roster")?,
                None => &intent.raw,
            };
            let effective = bound(
                &Roster {
                    rows: vec![raw.clone()],
                },
                logins,
            );
            let row = &effective.rows[0];
            if !intent.context.matches_row(row)
                || self.capture_auth_context(row).as_ref() != Some(&intent.context)
            {
                return Err("login credentials or routing changed before auth publication; nothing recorded".into());
            }
        }
        let mut events = Vec::new();
        for intent in pending {
            if matches!(intent.kind, PendingAuthKind::Check) {
                continue;
            }
            if state
                .accounts
                .get(&intent.raw.label)
                .is_some_and(|account| account.auth_at.is_some_and(|at| at > intent.at))
            {
                continue;
            }
            let label = &intent.raw.label;
            let old = state.accounts.get(label);
            let same_context =
                old.is_some_and(|old| old.auth_context.as_ref() == Some(&intent.context));
            let renewal_metadata = (same_context && old.is_some_and(|old| old.renewal_metadata))
                || matches!(intent.kind, PendingAuthKind::CheckedSignIn)
                || (matches!(intent.kind, PendingAuthKind::SignedBack)
                    && old.is_some_and(|old| old.auth == Auth::SignedOut));
            let served_metadata = (same_context && old.is_some_and(|old| old.served_metadata))
                || matches!(
                    intent.kind,
                    PendingAuthKind::CheckedSignIn | PendingAuthKind::SignedBack
                );
            let word = match intent.kind {
                PendingAuthKind::Word(word) => {
                    events.extend(note_auth(state, label, word, intent.at));
                    word
                }
                PendingAuthKind::LoginEnd => {
                    events.extend(note_auth(state, label, Auth::SignedOut, intent.at));
                    events.extend(note_login_end(state, logins, label, intent.at));
                    Auth::SignedOut
                }
                PendingAuthKind::SignedBack => {
                    events.extend(note_signed_back(state, logins, label, intent.at));
                    events.extend(note_auth(state, label, Auth::Ok, intent.at));
                    note_served_login(logins, label, intent.at);
                    Auth::Ok
                }
                PendingAuthKind::CheckedSignIn => {
                    events.extend(note_checked_sign_in(state, logins, label, intent.at));
                    Auth::Ok
                }
                PendingAuthKind::Check => continue,
            };
            if word == Auth::SignedOut {
                end_pin(state, label, "signed-out", intent.at, &mut events);
            }
            if word != Auth::Unseen
                && let Some(account) = account_mut(state, label)
            {
                account.auth_context = Some(intent.context.clone());
                account.renewal_metadata = renewal_metadata;
                account.served_metadata = served_metadata;
                account.auth_projection = AuthProjection::Current(intent.context);
            }
        }
        Ok(events)
    }

    /// The user home whose `.aterm/logins/<cli>` holds this store's logins ([`Store::at_for`]);
    /// `None` for Claude Code's store.
    pub(super) fn user_home(&self) -> Option<&Path> {
        if self.cli == Cli::Claude {
            return None;
        }
        self.logins.parent()?.parent()?.parent()
    }

    /// The rows of `roster` whose login is made on this machine: [`enrolled_in`] in the user home
    /// this store's logins are under, so a default row is enrolled only while the CLI's default
    /// home there holds its login file; [`enrolled`] for Claude Code's store.
    #[must_use]
    pub fn enrolled_here(&self, roster: &Roster) -> BTreeSet<String> {
        match self.user_home() {
            Some(home) => enrolled_in(self.cli, roster, home),
            None => enrolled_for(self.cli, roster),
        }
    }

    /// Claude Code's store directory beside it (the same directory for Claude Code's own): the
    /// one root every CLI's sign-ins share, for the browser profiles (one per email for the
    /// vendors, Claude Code's own shared where it is keyed by that exact email,
    /// [`super::headless_oauth::vendor_profile_dir`], so one account's mailbox session serves
    /// every vendor until a window's tidy drops its identity providers' cookies, a Microsoft
    /// webmail's among them, [`super::browser_window::tidy_profile`]) and the sign-in lock
    /// ([`Store::signin_within`]).
    #[must_use]
    pub fn shared_dir(&self) -> PathBuf {
        match self.cli {
            Cli::Claude => self.dir.clone(),
            _ => self
                .dir
                .parent()
                .map_or_else(|| self.dir.clone(), |state| state.join("accounts")),
        }
    }

    /// Takes the machine-wide sign-in lock ([`SIGNIN_LOCK_FILE`] in [`Store::shared_dir`]) on
    /// this store's ladder: one sign-in at a time on this machine, for every CLI, since a
    /// headless browser's profile admits one process and Codex's login listens on one fixed
    /// port. On Unix, live manual FIFO owners also hold background admission.
    /// `None` when either admission stayed held past the ladder. Other platforms
    /// retain kernel exclusion; the private manual FIFO is separately unavailable.
    /// # Errors
    /// The directory or lock file cannot be made, the opened path is not a regular file,
    /// the platform refuses a link, or the lock failed.
    pub fn signin_within(&self) -> io::Result<Option<SigninTurn>> {
        self.signin_on(self.ladder)
    }

    /// [`Store::signin_within`] tried once, never waited for: `None` while another sign-in
    /// holds it.
    /// # Errors
    /// As [`Store::signin_within`].
    pub fn signin_now(&self) -> io::Result<Option<SigninTurn>> {
        self.signin_on(&[])
    }

    fn signin_on(&self, ladder: &[std::time::Duration]) -> io::Result<Option<SigninTurn>> {
        if let Some(lock) = self.try_background_signin()? {
            return Ok(Some(lock));
        }
        for pause in ladder {
            std::thread::sleep(*pause);
            if let Some(lock) = self.try_background_signin()? {
                return Ok(Some(lock));
            }
        }
        Ok(None)
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AccountSigninLock",
            action = "TakeA",
            project = "aterm_agent::harness::cli::tests::signin_lock::lock_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AccountSigninLock",
            action = "TakeB",
            project = "aterm_agent::harness::cli::tests::signin_lock::lock_transition"
        )
    )]
    fn signin_kernel_now(&self) -> io::Result<Option<Lock>> {
        let shared = self.shared_dir();
        aterm_types::fs_restricted::ensure_private_dir(&shared)?;
        Ok(Self::try_take(open_retained_file(&shared.join(SIGNIN_LOCK_FILE), "lock")?)?.ok())
    }

    /// Overrides the ladder of this store's bounded lock takes: a seat's own ladder, as the
    /// window's roster read passes it in every build, and a test's shorter one.
    #[must_use]
    pub fn bounded_by(self, ladder: &'static [std::time::Duration]) -> Store {
        Store { ladder, ..self }
    }

    /// Ensures the store directory exists with mode `0700` owned by the current user.
    /// # Errors
    /// It cannot be made, or it is not this user's private directory.
    pub fn ensure(&self) -> io::Result<()> {
        aterm_types::fs_restricted::ensure_private_dir(&self.dir)
    }

    fn lock_file(&self) -> io::Result<std::fs::File> {
        self.ensure()?;
        open_retained_file(&self.dir.join(LOCK_FILE), "lock")
    }

    fn finish_account_lock(&self, held: Lock) -> io::Result<Lock> {
        self.sync_writer_barrier()?;
        Ok(held)
    }

    /// Acquires `accounts.lock`, blocking until the lock is available.
    /// # Errors
    /// The directory or lock file cannot be made, the opened path is not a regular file,
    /// the platform refuses a link, the lock failed, or the sync receipt is unreadable or
    /// its directory barrier fails.
    pub fn acquire(&self) -> io::Result<Lock> {
        let file: std::fs::File = self.lock_file()?;
        loop {
            #[expect(
                clippy::disallowed_methods,
                reason = "the guard's own take: its Drop releases by LOCK_UN"
            )]
            let taken = file.lock();
            match taken {
                Ok(()) => return self.finish_account_lock(Lock(file)),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
    }

    /// Attempts a non-blocking acquire of `accounts.lock` ([`Store::acquire`]).
    /// # Errors
    /// As [`Store::acquire`].
    pub fn try_acquire(&self) -> io::Result<Option<Lock>> {
        Self::try_take(self.lock_file()?)?
            .ok()
            .map(|held| self.finish_account_lock(held))
            .transpose()
    }

    fn try_take(file: std::fs::File) -> io::Result<Result<Lock, std::fs::File>> {
        #[expect(
            clippy::disallowed_methods,
            reason = "the guard's own take: its Drop releases by LOCK_UN"
        )]
        let taken = file.try_lock();
        match taken {
            Ok(()) => Ok(Ok(Lock(file))),
            Err(std::fs::TryLockError::WouldBlock) => Ok(Err(file)),
            Err(std::fs::TryLockError::Error(e)) => Err(e),
        }
    }

    /// [`Store::acquire_within`] on this store's own ladder ([`Store::bounded_by`]).
    /// # Errors
    /// As [`Store::acquire`].
    pub fn acquire_bounded(&self) -> io::Result<Option<Lock>> {
        self.acquire_within(self.ladder)
    }

    /// Tries `accounts.lock` now and after each pause of `ladder`; `None` when it stayed held.
    /// # Errors
    /// As [`Store::acquire`].
    pub fn acquire_within(&self, ladder: &[std::time::Duration]) -> io::Result<Option<Lock>> {
        let mut file = match Self::try_take(self.lock_file()?)? {
            Ok(lock) => return self.finish_account_lock(lock).map(Some),
            Err(file) => file,
        };
        for pause in ladder {
            std::thread::sleep(*pause);
            file = match Self::try_take(file)? {
                Ok(lock) => return self.finish_account_lock(lock).map(Some),
                Err(file) => file,
            };
        }
        Ok(None)
    }

    /// Path to `state.json`.
    #[must_use]
    pub fn state_file(&self) -> PathBuf {
        self.dir.join(STATE_FILE)
    }

    /// The retained evidence that a state snapshot has been prepared for publication.
    #[must_use]
    pub fn state_witness_file(&self) -> PathBuf {
        self.dir.join(STATE_WITNESS_FILE)
    }

    fn absent_state(&self) -> Result<State, String> {
        for path in [self.state_witness_file(), self.ledger_path()] {
            match std::fs::symlink_metadata(&path) {
                Ok(_) => {
                    return Err(format!(
                        "{STATE_FILE} is missing but {} remains; account state is unknown",
                        path.display()
                    ));
                }
                Err(why) if why.kind() == io::ErrorKind::NotFound => {}
                Err(why) => return Err(format!("{}: {why}", path.display())),
            }
        }
        Ok(State::default())
    }

    /// Path to `ledger.jsonl`.
    #[must_use]
    pub fn ledger_path(&self) -> PathBuf {
        self.dir.join(LEDGER_FILE)
    }

    /// Path to the directory holding per-account login directories: `logins/` in the store, or a
    /// CLI's own root ([`Store::at_for`]).
    #[must_use]
    pub fn logins_dir(&self) -> PathBuf {
        self.logins.clone()
    }

    /// Path to the login directory for `label`, or `None` if `label` is not admissible.
    #[must_use]
    pub fn login_dir(&self, label: &str) -> Option<PathBuf> {
        admissible_label(label).then(|| self.logins_dir().join(label))
    }

    /// Path to the `wake` marker file.
    #[must_use]
    pub fn wake_path(&self) -> PathBuf {
        self.dir.join(WAKE_FILE)
    }

    /// Path to `logins.json`.
    #[must_use]
    pub fn logins_file(&self) -> PathBuf {
        self.dir.join(LOGINS_FILE)
    }

    /// Retained evidence that a login binding snapshot was prepared for publication.
    #[must_use]
    pub fn logins_witness_file(&self) -> PathBuf {
        self.dir.join(LOGINS_WITNESS_FILE)
    }

    fn absent_logins(&self) -> Result<(Logins, Option<String>), String> {
        let path = self.logins_witness_file();
        match std::fs::symlink_metadata(&path) {
            Ok(_) => Err(format!(
                "{LOGINS_FILE} is missing but {} remains; account login bindings are unknown",
                path.display()
            )),
            Err(why) if why.kind() == io::ErrorKind::NotFound => Ok((Logins::default(), None)),
            Err(why) => Err(format!("{}: {why}", path.display())),
        }
    }

    /// Creates a fresh `0700` staging directory `<label>+<n>` in the logins directory.
    /// # Errors
    /// A label no roster could carry; the logins directory or the staging
    /// directory cannot be made; every name up to `<label>+9999` is taken.
    pub fn claim_staging_dir(&self, label: &str) -> io::Result<PathBuf> {
        if !admissible_label(label) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "not a label a roster could carry",
            ));
        }
        let logins = self.logins_dir();
        aterm_types::fs_restricted::ensure_private_dir(&logins)?;
        for n in 1..=9999 {
            let dir = logins.join(format!("{label}+{n}"));
            #[cfg(unix)]
            let made = {
                use std::os::unix::fs::DirBuilderExt;
                std::fs::DirBuilder::new().mode(0o700).create(&dir)
            };
            #[cfg(not(unix))]
            let made = std::fs::create_dir(&dir);
            match made {
                Ok(()) => return Ok(dir),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(e),
            }
        }
        Err(io::Error::other("every staging name is taken"))
    }

    /// Loads `logins.json` as of `now`; an absent file holds no bindings only when its
    /// publication witness is also absent. A lost snapshot must never select an old roster
    /// login as though no renewal had happened.
    /// # Errors
    /// A file that cannot be read, is larger than [`MAX_LOGINS_BYTES`], is not UTF-8, or that
    /// [`Logins::from_json`] refuses (not JSON, or JSON of another schema or of none).
    /// Also refuses an absent snapshot whose witness remains or cannot be inspected.
    /// A pending, damaged or lost sync receipt, or a sync spanning this read, also refuses.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AccountBindingRetention",
            action = "Read",
            project = "aterm_agent::harness::cli::tests::binding_retention::binding_transition"
        )
    )]
    pub fn load_logins(&self, now: i64) -> Result<(Logins, Option<String>), String> {
        let receipt = self.sync_read_receipt()?;
        let result = self.logins_as_stored(now);
        self.check_sync_read_receipt(&receipt)?;
        result
    }

    /// [`Self::load_logins`] without its receipt check, for a caller whose own check spans this
    /// read ([`Self::recover_account_records_with`]) or who holds the writer lock past a recovery,
    /// so that no publication can begin or be pending ([`Self::transact_images_held`]).
    fn logins_as_stored(&self, now: i64) -> Result<(Logins, Option<String>), String> {
        match read_bounded(&self.logins_file(), MAX_LOGINS_BYTES) {
            Ok(None) => self.absent_logins(),
            Ok(Some(text)) => match Logins::from_json(&text, now) {
                Ok(logins) => Ok((logins, None)),
                Err(Unread::Torn(why) | Unread::NotOurs(why)) => {
                    Err(format!("{LOGINS_FILE}: {why}"))
                }
            },
            Err(e) => Err(format!("{LOGINS_FILE}: {e}")),
        }
    }

    /// Checks that local state and login bindings can be read before starting a sign-in.
    /// This read-only check grants no account rights; completion still needs a transaction
    /// against the state and bindings current when the new login is published.
    /// # Errors
    /// As [`Self::load`] or [`Self::load_logins`].
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AccountSigninAdmission",
            action = "Read",
            project = "aterm_agent::harness::cli::tests::admission::admission_transition"
        )
    )]
    pub fn check_signin_state(&self, now: i64) -> Result<(), String> {
        self.load(now)
            .map_err(|why| format!("{why} ({})", self.state_file().display()))?;
        self.load_logins(now)
            .map_err(|why| format!("{why} ({})", self.logins_file().display()))?;
        Ok(())
    }

    /// Completes an interrupted, admitted account-record publication at `roster_path`.
    /// Uses this store's bounded lock wait and replays exact saved images without running
    /// the original edit or contacting a provider. With no pending publication, it reads
    /// the receipt without taking the writer lock and returns `false`.
    /// # Errors
    /// An unreadable or damaged receipt, a lost receipt with retained evidence, unavailable
    /// writer, different recovery target, conflicting image, or failed durable publication.
    pub fn recover_records(&self, roster_path: &Path) -> Result<bool, String> {
        self.recover_sync_within(self.ladder, roster_path)
    }

    /// Completes an admitted state or local-login publication at fixed paths in this store.
    /// Ordinary reads create no storage and take no lock. A pending roster-bound write
    /// refuses because it needs a caller-supplied roster path.
    /// # Errors
    /// As [`Self::recover_records`], or a pending publication requires a roster path.
    pub fn recover_state_records(&self) -> Result<bool, String> {
        self.recover_state_within(self.ladder)
    }

    /// Recovers account publication before interpreting an optional roster path.
    /// With no path, only fixed-path records can recover; a validated receipt then
    /// permits an empty roster. This neither discovers an external path from the
    /// journal nor reads bindings that no roster row uses.
    /// # Errors
    /// As [`Self::read_bound_roster`], including a pending roster-bound publication
    /// when the caller supplies no path.
    pub fn read_roster(
        &self,
        roster_path: Option<&Path>,
        now: i64,
    ) -> Result<Roster, RosterReadError> {
        match roster_path {
            Some(path) => self.read_bound_roster(path, now),
            None => {
                self.recover_state_records()
                    .map_err(RosterReadError::Records)?;
                self.sync_read_receipt().map_err(RosterReadError::Records)?;
                Ok(Roster::default())
            }
        }
    }

    /// Recovers admitted publication and reads a report's entire account snapshot.
    /// Unlike [`Self::read_roster`], this validates state and bindings even for an
    /// empty or absent roster. A receipt check covers the roster's presence as well
    /// as all three records. External file editors are not serialized by this read.
    /// # Errors
    /// As [`Self::read_bound_roster`], plus unreadable account state. No partial
    /// snapshot is returned, including when a publication overlaps a failed read.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AccountReportRead",
            action = "Read",
            project = "aterm_agent::harness::accounts::report_read_tests::report_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AccountRenewalGeneration",
            action = "ReadAuth",
            project = "aterm_agent::harness::accounts::generation_tests::generation_transition"
        )
    )]
    pub fn read_account_records(
        &self,
        roster_path: Option<&Path>,
        now: i64,
    ) -> Result<AccountRecords, RosterReadError> {
        self.read_account_records_with(roster_path, now, || {})
    }

    fn native_writer_matches(&self, held: &Lock) -> bool {
        astream_sys::file_matches_path(&held.0, &self.dir.join(LOCK_FILE)).unwrap_or(false)
    }

    pub(in crate::harness) fn native_retirement_snapshot_held<'a>(
        &'a self,
        held: &'a Lock,
        now: i64,
    ) -> Result<NativeRetirementSnapshot<'a>, String> {
        if !self.native_writer_matches(held) {
            return Err("native retirement writer changed".into());
        }
        let (logins, note) = self.load_logins(now)?;
        if note.is_some() {
            return Err("native retirement records are uncertain".into());
        }
        let snapshot = NativeRetirementSnapshot {
            store: self,
            held,
            logins,
        };
        snapshot.current()?;
        Ok(snapshot)
    }

    /// Observe cached account metadata without recovering publication or reading
    /// credentials. A pending or overlapping publication fails closed.
    /// # Errors
    /// As [`Self::read_account_records`]; no interrupted publication is repaired.
    pub(in crate::harness) fn native_row_snapshot_held<'a>(
        &'a self,
        held: &'a Lock,
        roster: &Path,
        now: i64,
    ) -> Result<NativeRowSnapshot<'a>, String> {
        if !self.native_writer_matches(held) {
            return Err("native row writer changed".into());
        }
        let records = self
            .peek_account_records(roster, now)
            .map_err(|_| "native row records unavailable")?;
        if !records.roster_present || !records.notes.is_empty() {
            return Err("native row records are uncertain".into());
        }
        let snapshot = NativeRowSnapshot {
            store: self,
            roster: std::fs::canonicalize(roster).map_err(|_| "native row source unavailable")?,
            held,
            records,
        };
        snapshot.current()?;
        Ok(snapshot)
    }

    pub fn peek_account_records(
        &self,
        roster_path: &Path,
        now: i64,
    ) -> Result<AccountRecords, RosterReadError> {
        self.peek_account_records_with(roster_path, now, || {})
    }

    fn peek_account_records_with(
        &self,
        roster_path: &Path,
        now: i64,
        after_roster: impl FnOnce(),
    ) -> Result<AccountRecords, RosterReadError> {
        let receipt = self.sync_read_receipt().map_err(RosterReadError::Records)?;
        let before = roster_text(roster_path).map_err(RosterReadError::Roster)?;
        let raw = before
            .as_deref()
            .map_or(Ok(Roster::default()), |text| {
                Roster::parse_for(self.cli, text)
            })
            .map_err(RosterReadError::Roster)?;
        after_roster();
        let (logins, login_note) = self.load_logins(now).map_err(RosterReadError::Records)?;
        let (state, state_note) = self.load(now).map_err(RosterReadError::Records)?;
        let roster = bound(&raw, &logins);
        if roster_text(roster_path).map_err(RosterReadError::Roster)? != before {
            return Err(RosterReadError::Roster(
                "account roster changed during observation".into(),
            ));
        }
        self.check_sync_read_receipt(&receipt)
            .map_err(RosterReadError::Records)?;
        Ok(AccountRecords {
            raw_roster: raw,
            roster,
            roster_present: before.is_some(),
            state,
            logins,
            notes: state_note.into_iter().chain(login_note).collect(),
        })
    }

    /// Captures usage attribution, including the implicit default when no rows exist.
    /// Its source path and mode travel with the observation until writer admission.
    /// # Errors
    /// The same publication, roster and binding failures as [`Self::read_account_records`].
    pub fn read_usage_roster(
        &self,
        path: Option<&Path>,
        now: i64,
    ) -> Result<UsageRoster, RosterReadError> {
        let target = usage_target(path).map_err(RosterReadError::Roster)?;
        let records = self.read_account_records(path, now)?;
        if usage_target(path).map_err(RosterReadError::Roster)? != target {
            return Err(RosterReadError::Roster(
                "usage roster target changed during read".into(),
            ));
        }
        let implicit = records.roster.rows.is_empty();
        Ok(UsageRoster {
            roster: if implicit {
                Roster::own()
            } else {
                records.roster
            },
            path: path.map(Path::to_path_buf),
            target,
            store: self.clone(),
            implicit,
        })
    }

    fn read_account_records_with(
        &self,
        roster_path: Option<&Path>,
        now: i64,
        after_raw: impl FnOnce(),
    ) -> Result<AccountRecords, RosterReadError> {
        self.recover_account_records_with(roster_path, now, after_raw)
            .map(|(_, records)| records)
    }

    /// Captures the same image as [`Self::read_account_records`] and reports whether
    /// this read completed an interrupted publication. A sync uses that fact in its
    /// change report, including a journal admitted just before the read begins.
    /// # Errors
    /// As [`Self::read_account_records`].
    pub(super) fn recover_account_records(
        &self,
        roster_path: Option<&Path>,
        now: i64,
    ) -> Result<(bool, AccountRecords), RosterReadError> {
        self.recover_account_records_with(roster_path, now, || {})
    }

    fn recover_account_records_with(
        &self,
        roster_path: Option<&Path>,
        now: i64,
        after_raw: impl FnOnce(),
    ) -> Result<(bool, AccountRecords), RosterReadError> {
        let recovered = match roster_path {
            Some(path) => self.recover_records(path),
            None => self.recover_state_records(),
        }
        .map_err(RosterReadError::Records)?;
        let receipt = self.sync_read_receipt().map_err(RosterReadError::Records)?;
        let raw = roster_path.map_or(Ok(None), roster_text).and_then(|text| {
            let present = text.is_some();
            let roster = text.map_or(Ok(Roster::default()), |text| {
                Roster::parse_for(self.cli, &text)
            })?;
            Ok((roster, present))
        });
        after_raw();
        let result = (|| {
            let (raw, roster_present) = raw.map_err(RosterReadError::Roster)?;
            // The receipt read above and checked below spans both, so neither checks its own.
            let (logins, login_note) = self
                .logins_as_stored(now)
                .map_err(RosterReadError::Records)?;
            let (mut state, state_note, _) = self
                .state_as_stored(now)
                .map_err(RosterReadError::Records)?;
            let roster = bound(&raw, &logins);
            self.hydrate_auth_contexts(&mut state, &roster);
            Ok(AccountRecords {
                roster,
                raw_roster: raw,
                roster_present,
                state,
                logins,
                notes: state_note.into_iter().chain(login_note).collect(),
            })
        })();
        self.check_sync_read_receipt(&receipt)
            .map_err(RosterReadError::Records)?;
        result.map(|records| (recovered, records))
    }

    /// Recovers admitted publication and reads the raw roster with its local bindings.
    /// A receipt check across both reads refuses a cooperating publication that overlaps
    /// the snapshot. Ordinary reads create no storage and need no writer lock. An external
    /// editor can still change a file after its read; observations must recheck their
    /// captured row when they publish.
    /// # Errors
    /// [`RosterReadError::Roster`] for an unreadable roster or target path, or
    /// [`RosterReadError::Records`] for recovery or binding failures, or a publication
    /// overlapping the read.
    pub fn read_bound_roster(
        &self,
        roster_path: &Path,
        now: i64,
    ) -> Result<Roster, RosterReadError> {
        self.read_bound_roster_with(roster_path, now, || {})
    }

    // The hook lets native tests publish precisely between the raw and binding reads.
    fn read_bound_roster_with(
        &self,
        roster_path: &Path,
        now: i64,
        after_raw: impl FnOnce(),
    ) -> Result<Roster, RosterReadError> {
        self.recover_records(roster_path)
            .map_err(RosterReadError::Records)?;
        let receipt = self.sync_read_receipt().map_err(RosterReadError::Records)?;
        let result = (|| {
            let raw = Roster::load_for(self.cli, roster_path).map_err(RosterReadError::Roster)?;
            after_raw();
            if raw.rows.is_empty() {
                return Ok(raw);
            }
            self.logins_as_stored(now)
                .map(|(logins, _)| bound(&raw, &logins))
                .map_err(RosterReadError::Records)
        })();
        self.check_sync_read_receipt(&receipt)
            .map_err(RosterReadError::Records)?;
        result
    }

    /// Refuses new roster labels that would inherit retained login or account state.
    /// The caller holds the account writer lock from the current roster read through
    /// publication. This reads existing snapshots and does not clear old records.
    /// # Errors
    /// As [`Self::load`] or [`Self::load_logins`], or a label remains referenced by
    /// either snapshot, including pending selections, claims and rollback state.
    pub(super) fn check_new_labels(&self, labels: &[&str], now: i64) -> Result<(), String> {
        if let Some(label) = self.retained(labels, now)? {
            return Err(format!(
                "`{label}` still has retained local login or account state; choose an unused label; nothing was written"
            ));
        }
        Ok(())
    }

    /// The first of `labels` either snapshot still references ([`Self::check_new_labels`]),
    /// read without the writer lock by a caller that only decides whether to sync.
    /// # Errors
    /// As [`Self::load`] or [`Self::load_logins`].
    pub(super) fn retained<'a>(
        &self,
        labels: &[&'a str],
        now: i64,
    ) -> Result<Option<&'a str>, String> {
        let (state, _) = self
            .load(now)
            .map_err(|why| format!("{why} ({})", self.state_file().display()))?;
        let (logins, _) = self
            .load_logins(now)
            .map_err(|why| format!("{why} ({})", self.logins_file().display()))?;
        Ok(labels
            .iter()
            .copied()
            .find(|label| label_is_retained(&state, &logins, label)))
    }

    /// Loads `logins.json` and returns `roster` with machine-local renewal bindings applied.
    /// # Errors
    /// As [`Self::load_logins`].
    pub fn bound(&self, roster: &Roster, now: i64) -> Result<Roster, String> {
        self.load_logins(now)
            .map(|(logins, _)| bound(roster, &logins))
    }

    /// Prepares and durably flushes `logins.json` (`0600`), retains a publication witness,
    /// then replaces the snapshot and flushes its directory. Transactions load first; an
    /// explicit restore of known-good bindings may call this directly.
    /// # Errors
    /// The directory or witness cannot be opened, the witness is not a regular file, or a
    /// write, rename or flush failed. Failure after retaining the witness can leave an absent
    /// snapshot unreadable until an explicit restore succeeds.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AccountBindingRetention",
            action = "Save",
            project = "aterm_agent::harness::cli::tests::binding_retention::binding_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AccountBindingRetention",
            action = "InterruptedSave",
            project = "aterm_agent::harness::cli::tests::binding_retention::binding_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AccountBindingRetention",
            action = "Restore",
            project = "aterm_agent::harness::cli::tests::binding_retention::binding_transition"
        )
    )]
    pub fn save_logins(&self, logins: &Logins) -> io::Result<()> {
        let mut text = logins.to_json();
        text.push('\n');
        if text.len() as u64 > MAX_LOGINS_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "login snapshot exceeds its readable size bound",
            ));
        }
        self.ensure()?;
        super::upgrade_models::write_atomic_before_publish(&self.logins_file(), &text, || {
            self.retain_witness(&self.logins_witness_file())
        })?;
        aterm_durable::durable_dir(&self.dir)
    }

    /// Atomically replaces the `wake` marker file so watchers detect an inode change.
    /// # Errors
    /// The directory cannot be made or the write or rename failed.
    pub fn touch_wake(&self) -> io::Result<()> {
        self.ensure()?;
        super::upgrade_models::write_atomic_ordered(&self.wake_path(), "")
    }

    /// Loads the state snapshot as of now. An absent snapshot reads as empty only when neither
    /// its publication witness nor the legacy ledger remains.
    /// # Errors
    /// A file that is not ours to replace, named: one that could not be read (a permission, an
    /// I/O error, a directory in its place), one larger than [`MAX_STATE_BYTES`], non-UTF-8
    /// bytes, malformed JSON, or JSON of another schema or none ([`Unread::NotOurs`]). It may
    /// hold a pin, walls and signed-out words, so a transaction stops without saving and a
    /// read-only caller must not infer availability from a replacement empty state. An absent
    /// snapshot also refuses when retained publication evidence exists or cannot be inspected.
    /// A pending, damaged or lost sync receipt, or a sync spanning this read, also refuses.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AccountStateRetention",
            action = "Read",
            project = "aterm_agent::harness::cli::tests::retention::retention_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AccountStateRead",
            action = "Absent",
            project = "aterm_agent::harness::cli::tests::state_read::state_read_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AccountStateRead",
            action = "Valid",
            project = "aterm_agent::harness::cli::tests::state_read::state_read_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AccountStateRead",
            action = "Torn",
            project = "aterm_agent::harness::cli::tests::state_read::state_read_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AccountStateRead",
            action = "NonUtf8",
            project = "aterm_agent::harness::cli::tests::state_read::state_read_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AccountStateRead",
            action = "ForeignSchema",
            project = "aterm_agent::harness::cli::tests::state_read::state_read_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AccountStateRead",
            action = "Unreadable",
            project = "aterm_agent::harness::cli::tests::state_read::state_read_transition"
        )
    )]
    pub fn load(&self, now: i64) -> Result<(State, Option<String>), String> {
        let receipt = self.sync_read_receipt()?;
        let (state, note, _) = self.state_as_stored(now)?;
        self.check_sync_read_receipt(&receipt)?;
        Ok((state, note))
    }

    /// [`Self::load`] without its receipt check, as [`Self::logins_as_stored`] is
    /// [`Self::load_logins`], and whether [`Self::machine_file`] named the state's machine: that
    /// file, once linked, is never rewritten, so [`Self::machine_id`] would read the same id again.
    fn state_as_stored(&self, now: i64) -> Result<(State, Option<String>, bool), String> {
        let mut state = match read_bounded(&self.state_file(), MAX_STATE_BYTES) {
            Ok(None) => self.absent_state()?,
            Ok(Some(text)) => State::from_json(&text, now)
                .map_err(|why| format!("{STATE_FILE}: {}", why.why()))?,
            Err(e) => return Err(format!("{STATE_FILE}: {e}")),
        };
        let mut note = None;
        let mut kept = false;
        match self.kept_machine() {
            Ok(Some(id)) => {
                state.machine = id;
                kept = true;
            }
            Ok(None) if !state.machine.is_empty() => {}
            Ok(None) => match mint_machine() {
                Ok(id) => state.machine = id,
                Err(e) => {
                    note.get_or_insert_with(|| {
                        format!("no machine id could be minted ({e}); observations carry none")
                    });
                }
            },
            Err(e) => {
                state.machine.clear();
                note.get_or_insert_with(|| {
                    format!("no machine id could be read ({e}); observations carry none")
                });
            }
        }
        if self.cli == Cli::Codex {
            state.contextual_auth = true;
            for account in state
                .accounts
                .values_mut()
                .chain(state.forgotten.values_mut())
            {
                account.auth_projection = AuthProjection::Unknown;
            }
        }
        Ok((state, note, kept))
    }

    /// Path to the `<dir>/machine` identifier file.
    #[must_use]
    pub fn machine_file(&self) -> PathBuf {
        self.dir.join(MACHINE_FILE)
    }

    /// The id the store keeps ([`Self::machine_file`]), where it keeps one.
    /// # Errors
    /// The file is there and could not be read, or holds no id.
    fn kept_machine(&self) -> io::Result<Option<String>> {
        match read_bounded(&self.machine_file(), 64)? {
            None => Ok(None),
            Some(text) if is_machine_id(text.trim()) => Ok(Some(text.trim().to_owned())),
            Some(_) => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "not a machine id",
            )),
        }
    }

    /// Reads or mints and persists this machine's 16-hex-char identifier.
    /// # Errors
    /// The directory cannot be made, the file holds no id, or the write or
    /// the link failed.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AccountStateRetention",
            action = "StatsOnly",
            project = "aterm_agent::harness::cli::tests::retention::retention_transition"
        )
    )]
    pub fn machine_id(&self, held: &str) -> io::Result<String> {
        if let Some(id) = self.kept_machine()? {
            return Ok(id);
        }
        let id = if is_machine_id(held) {
            held.to_owned()
        } else {
            mint_machine()?
        };
        self.ensure()?;
        let stage = aterm_tempfile::Builder::new()
            .prefix(".machine-")
            .tempdir_in(&self.dir)?;
        self.keep_machine_at(&id, &stage.path().join("id"))?;
        self.kept_machine()?
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "kept, then gone"))
    }

    fn keep_machine_at(&self, id: &str, tmp: &Path) -> io::Result<()> {
        use std::io::Write as _;
        let mut open = std::fs::OpenOptions::new();
        open.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            open.mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
        }
        let mut file = open.open(tmp)?;
        let written = (|| {
            file.write_all(format!("{id}\n").as_bytes())?;
            aterm_durable::durable(&file)?;
            match std::fs::hard_link(tmp, self.machine_file()) {
                Ok(()) => aterm_durable::durable_dir(&self.dir),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(()),
                Err(e) => Err(e),
            }
        })();
        drop(file);
        let _ = std::fs::remove_file(tmp);
        written
    }

    /// Prepares the state snapshot (0600) in a private sibling temporary file, retains durable
    /// publication evidence, then atomically replaces the snapshot and flushes its directory.
    /// A missing snapshot cannot become a fresh state while that witness survives. Transaction
    /// callers must first load successfully; restoring known-good state may call this directly.
    /// # Errors
    /// The snapshot exceeds its readable byte bound, the directory or witness cannot be
    /// opened, the witness is not a regular file, or a write, rename or durability flush failed. A failure after preparation may leave the
    /// witness without a snapshot, which reads refuse until an explicit snapshot restore succeeds.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AccountStateRetention",
            action = "Save",
            project = "aterm_agent::harness::cli::tests::retention::retention_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AccountStateRetention",
            action = "InterruptedSave",
            project = "aterm_agent::harness::cli::tests::retention::retention_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AccountBindingRetention",
            action = "Fresh",
            project = "aterm_agent::harness::cli::tests::binding_retention::binding_transition"
        )
    )]
    pub fn save(&self, state: &State) -> io::Result<()> {
        let mut text = state.to_json();
        text.push('\n');
        if text.len() as u64 > MAX_STATE_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "account snapshot exceeds its readable size bound",
            ));
        }
        self.ensure()?;
        super::upgrade_models::write_atomic_before_publish(&self.state_file(), &text, || {
            self.retain_witness(&self.state_witness_file())
        })?;
        aterm_durable::durable_dir(&self.dir)
    }

    fn retain_witness(&self, path: &Path) -> io::Result<()> {
        let witness = open_retained_file(path, "publication witness")?;
        aterm_durable::durable(&witness)?;
        aterm_durable::durable_dir(&self.dir)
    }

    /// Appends `events` to `ledger.jsonl` under the account writer lock ([`LEDGER_MAX_BYTES`]).
    /// # Errors
    /// The directory or lock cannot be acquired, a pending or damaged sync receipt refuses,
    /// or the append or the cut failed.
    pub fn append(&self, events: &[Event]) -> io::Result<()> {
        if events.is_empty() {
            return Ok(());
        }
        let held = self.acquire()?;
        self.append_held(&held, events)
    }

    fn append_held(&self, _held: &Lock, events: &[Event]) -> io::Result<()> {
        use std::io::Write as _;
        if events.is_empty() {
            return Ok(());
        }
        self.sync_read_receipt().map_err(io::Error::other)?;
        self.ensure()?;
        let mut text = String::new();
        for event in events {
            text.push_str(&event.to_json_line());
            text.push('\n');
        }
        let path = self.ledger_path();
        let mut open = std::fs::OpenOptions::new();
        open.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            open.mode(0o600).custom_flags(libc::O_CLOEXEC);
        }
        let mut file = open.open(&path)?;
        file.write_all(text.as_bytes())?;
        drop(file);
        super::cli::bound_ledger(&path, LEDGER_MAX_BYTES, LEDGER_KEEP_ROWS).map(|_| ())
    }

    /// Reads the newest `max` events from `ledger.jsonl` in chronological order.
    /// # Errors
    /// The ledger exists and cannot be read, or a pending, damaged or overlapping
    /// publication refuses the read.
    pub fn tail(&self, max: usize) -> io::Result<Vec<Event>> {
        self.tail_with(max, || {})
    }

    /// Reads the newest `max` ledger rows only when each is a recognized event.
    /// Admission readers cannot ignore an invalid row that may supersede earlier evidence.
    /// # Errors
    /// As [`Self::tail`], or a row in the bounded tail is malformed or unrecognized.
    pub(crate) fn tail_validated(&self, max: usize) -> io::Result<Vec<Event>> {
        self.read_tail_with(
            max,
            || {},
            |lines| {
                lines
                    .iter()
                    .map(|line| {
                        let invalid = || {
                            io::Error::new(
                                io::ErrorKind::InvalidData,
                                "account ledger contains an invalid event",
                            )
                        };
                        let event = Event::of_json_line(line).ok_or_else(invalid)?;
                        let doc: Value = aterm_json::from_str(line).map_err(|_| invalid())?;
                        if doc.get("pk").is_some_and(|pk| {
                            pk.as_str().and_then(super::journal::Key::new).is_none()
                        }) {
                            return Err(invalid());
                        }
                        for (key, max) in [
                            ("account", LABEL_MAX),
                            ("to", LABEL_MAX),
                            ("sid", WORD_MAX),
                            ("why", LEDGER_WHY_MAX),
                        ] {
                            match doc.get(key) {
                                None | Some(Value::Null) if key != "account" && key != "why" => {}
                                None if key == "why" => {}
                                Some(Value::String(word)) if *word == one_line(word, max) => {}
                                _ => return Err(invalid()),
                            }
                        }
                        Ok(event)
                    })
                    .collect()
            },
        )
    }

    fn tail_with(&self, max: usize, after_read: impl FnOnce()) -> io::Result<Vec<Event>> {
        self.read_tail_with(max, after_read, |lines| {
            Ok(lines
                .iter()
                .filter_map(|line| Event::of_json_line(line))
                .collect())
        })
    }

    fn read_tail_with<T>(
        &self,
        max: usize,
        after_read: impl FnOnce(),
        decode: impl FnOnce(Vec<String>) -> io::Result<T>,
    ) -> io::Result<T> {
        let receipt = self.sync_read_receipt().map_err(io::Error::other)?;
        let decoded = decode(super::cli::tail_lines(&self.ledger_path(), max)?);
        after_read();
        self.check_sync_read_receipt(&receipt)
            .map_err(io::Error::other)?;
        decoded
    }

    /// Serializes a read-modify-write transaction on `state.json` and `ledger.jsonl` under
    /// `accounts.lock`.
    /// # Errors
    /// [`Store::load`]'s refusal, naming the file; `edit`'s own, with nothing written; or
    /// the file that could not be locked or written, by path. State, history and notification
    /// publish through a durable receipt. An interrupted state-only write recovers before
    /// the edit runs; a pending roster-bound write needs its caller-supplied recovery path.
    pub fn transact(
        &self,
        now: i64,
        note: &mut Option<String>,
        edit: impl FnOnce(&mut State) -> Result<Vec<Event>, String>,
    ) -> Result<Vec<Event>, String> {
        let held = self
            .acquire()
            .map_err(|e| format!("{}: {e}", self.dir.display()))?;
        self.transact_state_held(&held, now, note, edit)
    }

    /// Runs [`Store::transact`] with a bounded lock wait over `ladder` ([`LOCK_HELD`]).
    /// # Errors
    /// As [`Store::transact`]; and exactly [`LOCK_HELD`] when the lock stayed held past the
    /// ladder: nothing was read, `edit` did not run, and the caller asks again later.
    pub fn transact_within(
        &self,
        ladder: &[std::time::Duration],
        now: i64,
        note: &mut Option<String>,
        edit: impl FnOnce(&mut State) -> Result<Vec<Event>, String>,
    ) -> Result<Vec<Event>, String> {
        let held = self
            .acquire_within(ladder)
            .map_err(|e| format!("{}: {e}", self.dir.display()))?
            .ok_or_else(|| LOCK_HELD.to_string())?;
        self.transact_state_held(&held, now, note, edit)
    }

    /// Saves an explicit selection before its login worker starts.
    /// # Errors
    /// Invalid label, unavailable entropy, or a bounded state transaction refusal.
    pub fn request_pin(&self, label: &str, now: i64) -> Result<PinRequest, String> {
        if let Some(why) = label_refusal(label) {
            return Err(why.to_owned());
        }
        let nonce =
            aterm_uds::rand::hex_token::<16>().map_err(|e| format!("account selection: {e}"))?;
        let request = PinRequest::new(label, nonce)
            .ok_or_else(|| "invalid account selection request".to_owned())?;
        self.transact_within(&LOCK_LADDER, now, &mut None, |state| {
            state.pending_pin = Some(request.clone().into());
            Ok(Vec::new())
        })?;
        Ok(request)
    }

    /// Unpins the preferred account and atomically replaces the pending selection with a
    /// fresh request to choose the next eligible account after admission.
    /// # Errors
    /// Unavailable manual queue, unavailable entropy or a bounded state transaction error. This does not start provider work;
    /// a publication error retains the transaction's ordinary recovery semantics.
    pub fn request_next(&self, now: i64) -> Result<NextRequest, String> {
        if manual_signin_capability() == ManualSigninCapability::Unavailable {
            return Err(
                "manual sign-in queue is unavailable on this platform; nothing selected".into(),
            );
        }
        let nonce =
            aterm_uds::rand::hex_token::<16>().map_err(|e| format!("account selection: {e}"))?;
        let request = NextRequest { nonce };
        self.transact_within(&LOCK_LADDER, now, &mut None, |state| {
            let events = unpin(state, "manual-next", now).into_iter().collect();
            state.pending_pin = Some(PendingSelection::Next(request.clone()));
            Ok(events)
        })?;
        Ok(request)
    }

    /// Pins an eligible account, or saves an explicit sign-in request for an unenrolled or
    /// signed-out account. Re-reads the selected row and current login bindings under
    /// `accounts.lock`; the row must still equal its captured configuration. Publication uses
    /// a durable receipt, including its history and notification.
    /// # Errors
    /// An unknown label, a sign-in target without an email or available manual queue, unavailable entropy, or a bounded
    /// state/login transaction refusal, a changed row, or a different provider's store.
    /// A store error never becomes a sign-in request. An admitted interrupted write fences
    /// readers until its exact images recover.
    pub fn select_account(
        &self,
        file_roster: &Roster,
        roster_path: &Path,
        label: &str,
        now: i64,
    ) -> Result<PinSelection, String> {
        self.select_account_for(Cli::Claude, file_roster, roster_path, label, now)
    }

    /// [`Self::select_account`] for `cli`'s pool ([`enrolled_for`]).
    /// # Errors
    /// As [`Self::select_account`], when `cli` has no sign-in flow for an unenrolled or
    /// signed-out row, or a row needing sign-in requires explicit shared authorization.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AccountSelectionCommit",
            action = "Select",
            project = "aterm_agent::harness::accounts::selection_commit_tests::selection_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AccountSharedSelection",
            action = "Queue",
            project = "aterm_agent::harness::accounts_sync::shared_selection_tests::selection_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AccountSharedSelection",
            action = "TryShared",
            project = "aterm_agent::harness::accounts_sync::shared_selection_tests::selection_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AccountSharedSelection",
            action = "Replace",
            project = "aterm_agent::harness::accounts_sync::shared_selection_tests::selection_transition"
        )
    )]
    pub fn select_account_for(
        &self,
        cli: Cli,
        file_roster: &Roster,
        roster_path: &Path,
        label: &str,
        now: i64,
    ) -> Result<PinSelection, String> {
        if cli != self.cli {
            return Err("account selection targets another provider's store".into());
        }
        let mut selected = PinSelection::Pinned;
        self.transact_pool_logins_within(
            &LOCK_LADDER,
            roster_path,
            now,
            &mut None,
            |raw, state, logins| {
                if raw.row(label) != file_roster.row(label) {
                    return Err("selected account configuration changed; nothing selected".into());
                }
                let roster = bound(raw, logins);
                let enrolled = self.enrolled_here(&roster);
                self.hydrate_auth_contexts(state, &roster);
                match pin_refusal(state, &roster, &enrolled, label) {
                    None => pin(state, &roster, &enrolled, label, now).map(|event| vec![event]),
                    Some(why @ (PinRefusal::NotListed | PinRefusal::PreparationOnly | PinRefusal::PreparationFailed)) => {
                        Err(why.message(label))
                    }
                    Some(why @ (PinRefusal::NotEnrolled | PinRefusal::SignedOut)) => {
                        if manual_signin_capability() == ManualSigninCapability::Unavailable {
                            return Err("manual sign-in queue is unavailable on this platform; nothing selected".into());
                        }
                        if let Some(row) = roster.row(label)
                            && logins.shared.required(row)?.is_some()
                        {
                            return Err("this account requires a fresh root-authorized shared renewal; use `aterm harness accounts shared-renew`".into());
                        }
                        if cli != Cli::Claude && cli.accounts().signin.is_none() {
                            return Err(why.message(label));
                        }
                        if roster
                            .row(label)
                            .and_then(|row| row.email.as_ref())
                            .is_none()
                        {
                            return Err(format!("`{label}` has no sign-in email"));
                        }
                        let nonce = aterm_uds::rand::hex_token::<16>()
                            .map_err(|e| format!("account selection: {e}"))?;
                        let request = PinRequest::new(label, nonce)
                            .ok_or_else(|| "invalid account selection request".to_owned())?;
                        state.pending_pin = Some(request.clone().into());
                        selected = PinSelection::Renew(request);
                        Ok(Vec::new())
                    }
                }
            },
        )?;
        Ok(selected)
    }

    /// Admits authentication only while its captured policy and parent binding remain
    /// current, and a supplied selection still owns its nonce. The bounded writer lock
    /// covers recovery and all admission reads; it is
    /// released before authentication, so a later selection can supersede completion.
    /// This does not edit the request, budget, bindings or history. Earlier admitted writes
    /// may recover before the check.
    /// # Errors
    /// An unavailable writer, unreadable account records or roster, or recovery failure.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AccountQueuedSelection",
            action = "Check",
            project = "aterm_agent::harness::accounts_sync::request_admission_tests::admission_transition"
        )
    )]
    pub(in crate::harness) fn admit_renewal_attempt(
        &self,
        policy: RenewalPolicy<'_>,
        request: Option<&PinRequest>,
        now: i64,
    ) -> Result<bool, String> {
        let mut admitted = false;
        self.transact_renewal_within(self.ladder, now, &mut None, policy, |state, logins| {
            if logins.shared.required(policy.row)?.is_some() {
                return Err("this account requires a fresh root-authorized shared renewal".into());
            }
            admitted = request.is_none_or(|request| {
                request.label() == policy.row.label
                    && state
                        .pending_pin
                        .as_ref()
                        .and_then(PendingSelection::account)
                        == Some(request)
            });
            Ok(Vec::new())
        })?;
        Ok(admitted)
    }

    /// Rechecks the same renewal policy and exact manual intent after FIFO admission.
    /// Next supplies no account authority: the caller still provides the freshly
    /// selected row and its full configured/bound policy.
    /// # Errors
    /// As [`Self::admit_renewal_attempt`].
    pub(in crate::harness) fn admit_manual_renewal_attempt(
        &self,
        policy: RenewalPolicy<'_>,
        request: &PendingSelection,
        now: i64,
    ) -> Result<bool, String> {
        let mut admitted = false;
        self.transact_renewal_within(self.ladder, now, &mut None, policy, |state, logins| {
            if logins.shared.required(policy.row)?.is_some() {
                return Err("this account requires a fresh root-authorized shared renewal".into());
            }
            admitted = state.pending_pin.as_ref() == Some(request)
                && request
                    .account()
                    .is_none_or(|pin| pin.label() == policy.row.label);
            Ok(Vec::new())
        })?;
        Ok(admitted)
    }

    /// Completes a saved selection after sign-in, only while its nonce still owns it and
    /// its selected row still matches. An ineligible or changed account cancels only this
    /// request so a completed worker does not restart authentication indefinitely.
    /// # Errors
    /// A roster, lock or durable account commit failure. An admitted failure retains the
    /// receipt until recovery; it never dispatches a new login from a partial update.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AccountSelectionCommit",
            action = "Complete",
            project = "aterm_agent::harness::accounts::selection_commit_tests::selection_transition"
        )
    )]
    pub(in crate::harness) fn complete_requested_pin_at(
        &self,
        roster_path: &Path,
        expected: &Roster,
        request: &PinRequest,
        now: i64,
    ) -> Result<bool, String> {
        let mut pinned = false;
        self.transact_pool_logins_within(
            &LOCK_LADDER,
            roster_path,
            now,
            &mut None,
            |raw, state, logins| {
                if state
                    .pending_pin
                    .as_ref()
                    .and_then(PendingSelection::account)
                    != Some(request)
                {
                    return Ok(Vec::new());
                }
                let roster = bound(raw, logins);
                let enrolled = self.enrolled_here(&roster);
                if raw.row(request.label()) != expected.row(request.label())
                    || pin_refusal(state, &roster, &enrolled, request.label()).is_some()
                {
                    state.pending_pin = None;
                    return Ok(Vec::new());
                }
                let event = complete_requested_pin(state, &roster, &enrolled, request, now);
                pinned = event.is_some();
                Ok(event.into_iter().collect())
            },
        )?;
        Ok(pinned)
    }

    /// Cancels this request after startup or login failure, preserving a newer choice.
    /// # Errors
    /// A bounded state transaction refusal.
    pub fn cancel_pin_request(&self, request: &PinRequest, now: i64) -> Result<(), String> {
        self.transact_within(&LOCK_LADDER, now, &mut None, |state| {
            if state
                .pending_pin
                .as_ref()
                .and_then(PendingSelection::account)
                == Some(request)
            {
                state.pending_pin = None;
            }
            Ok(Vec::new())
        })?;
        Ok(())
    }

    /// Retires only this exact next-selection attempt, without changing a preferred pin.
    /// # Errors
    /// A bounded state transaction refusal. An admitted publication error can
    /// retain its durable receipt and must not be treated as proof of no edit.
    pub fn cancel_next_request(&self, request: &NextRequest, now: i64) -> Result<(), String> {
        self.transact_within(&LOCK_LADDER, now, &mut None, |state| {
            if state.pending_pin.as_ref().and_then(PendingSelection::next) == Some(request) {
                state.pending_pin = None;
            }
            Ok(Vec::new())
        })?;
        Ok(())
    }

    /// Publishes edits to `state.json`, `logins.json` and history through a durable receipt
    /// under `accounts.lock` ([`Store::transact`]).
    /// A renewal or change to a binding's location, proved identity or existence consumes
    /// a generation. Multiple changes to one label consume it once per transaction.
    /// # Errors
    /// As [`Store::transact`], for either file, or a renewed binding is missing or its
    /// generation is exhausted, or a sign-in receipt exceeds its capacity or clock.
    /// Interrupted local publication recovers before the edit;
    /// a pending roster-bound publication needs its caller-supplied path. The edit's own
    /// external effects are not rolled back.
    pub fn transact_logins(
        &self,
        now: i64,
        note: &mut Option<String>,
        edit: impl FnOnce(&mut State, &mut Logins) -> Result<Vec<Event>, String>,
    ) -> Result<Vec<Event>, String> {
        let held = self
            .acquire()
            .map_err(|e| format!("{}: {e}", self.dir.display()))?;
        self.transact_logins_held(&held, now, note, edit)
    }

    /// Runs [`Store::transact_logins`] with the bounded [`Store::transact_within`] lock wait.
    /// # Errors
    /// As [`Store::transact_within`].
    pub fn transact_logins_within(
        &self,
        ladder: &[std::time::Duration],
        now: i64,
        note: &mut Option<String>,
        edit: impl FnOnce(&mut State, &mut Logins) -> Result<Vec<Event>, String>,
    ) -> Result<Vec<Event>, String> {
        let held = self
            .acquire_within(ladder)
            .map_err(|e| format!("{}: {e}", self.dir.display()))?
            .ok_or_else(|| LOCK_HELD.to_string())?;
        self.transact_logins_held(&held, now, note, edit)
    }

    /// Records a decision only while every effective roster row still matches its input.
    /// Candidate selection depends on the whole roster, including competing accounts.
    /// This checks policy and bindings under the writer lock; the caller supplies any
    /// process evidence. The existing roster-bound receipt fences conflicting edits
    /// during publication and recovery.
    /// # Errors
    /// An unavailable writer, unreadable roster or records, changed effective roster,
    /// `edit`'s refusal or failed durable publication. Admission failure runs no edit.
    pub fn observe_roster_within(
        &self,
        ladder: &[std::time::Duration],
        now: i64,
        roster_path: &Path,
        captured: &Roster,
        edit: impl FnOnce(&mut State) -> Result<Vec<Event>, String>,
    ) -> Result<Vec<Event>, String> {
        self.transact_pool_logins_within(
            ladder,
            roster_path,
            now,
            &mut None,
            |raw, state, logins| {
                if bound(raw, logins) != *captured {
                    return Err("account roster changed since observation; nothing recorded".into());
                }
                edit(state)
            },
        )
    }

    /// Records a process observation only while its own current login still matches the
    /// observed account. Historical logins can attribute usage, but cannot prove or change
    /// the lifecycle of a replacement login. Reads the raw roster inside the same lock as
    /// the binding/state transaction, then compares the complete effective row.
    ///
    /// This validates local routing evidence, not the provider's identity or a process's
    /// in-memory token. The roster image checked here is also the publication's admitted
    /// image. A conflicting external roster edit refuses publication or recovery.
    /// # Errors
    /// An unavailable writer, unreadable roster or records, changed row, historical login,
    /// credential override, `edit`'s refusal or failed durable publication. A failed check runs
    /// no edit. An admitted partial publication fences readers until its exact state,
    /// bindings, history and event-dependent notification recover together.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AccountLoginObservation",
            action = "Record",
            project = "aterm_agent::harness::accounts::observation_tests::observation_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AccountLifecycleCommit",
            action = "Record",
            project = "aterm_agent::harness::accounts::lifecycle_commit_tests::lifecycle_transition"
        )
    )]
    pub fn observe_login_within(
        &self,
        ladder: &[std::time::Duration],
        now: i64,
        observation: LoginObservation<'_>,
        edit: impl FnOnce(&mut State, &mut Logins) -> Result<Vec<Event>, String>,
    ) -> Result<Vec<Event>, String> {
        self.transact_pool_logins_within(
            ladder,
            observation.roster_path,
            now,
            &mut None,
            |raw, state, logins| {
                let current = bound(raw, logins);
                if !observation.matches_current(&current) {
                    return Err(format!(
                        "account `{}` login changed since observation; nothing recorded",
                        observation.row.label
                    ));
                }
                edit(state, logins)
            },
        )
    }

    /// Records account-wide usage while the captured account metadata and process
    /// attribution still match under the writer lock. A credential renewal may change
    /// the effective directory and history; its retained old logins remain attributable.
    /// All other row fields must match, including an absent identity becoming populated.
    /// This checks local routing and declared policy, not provider identity or token bytes.
    ///
    /// # Errors
    /// An unavailable writer, unreadable roster or records, changed account metadata,
    /// lost process attribution, `edit` refusal or failed durable publication. The same
    /// roster-bound receipt and recovery rules as [`Self::observe_login_within`] apply.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AccountUsageObservation",
            action = "Record",
            project = "aterm_agent::harness::accounts::usage_observation_tests::usage_observation_transition"
        )
    )]
    pub fn observe_account_within(
        &self,
        ladder: &[std::time::Duration],
        now: i64,
        observation: LoginObservation<'_>,
        edit: impl FnOnce(&mut State, &mut Logins) -> Result<Vec<Event>, String>,
    ) -> Result<Vec<Event>, String> {
        self.transact_pool_logins_within(
            ladder,
            observation.roster_path,
            now,
            &mut None,
            |raw, state, logins| {
                if !observation.matches_account(&bound(raw, logins)) {
                    return Err(format!(
                        "account `{}` changed since observation; nothing recorded",
                        observation.row.label
                    ));
                }
                edit(state, logins)
            },
        )
    }

    /// Admits a usage observation against its captured source and account under the
    /// writer lock. A configured row cannot turn into the implicit default, or the
    /// default into a configured row. Retained logins may report across renewal.
    /// # Errors
    /// An unattributed process, changed policy, unreadable records, unavailable writer,
    /// edit refusal or failed publication. A missing source remains absent in recovery.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "UsageSource",
            action = "Record",
            project = "aterm_agent::harness::accounts::usage_source_tests::usage_source_transition"
        )
    )]
    pub fn observe_usage_within(
        &self,
        ladder: &[std::time::Duration],
        now: i64,
        captured: &UsageRoster,
        who: &ProcessLogin,
        edit: impl FnOnce(&mut State) -> Result<Vec<Event>, String>,
    ) -> Result<Vec<Event>, String> {
        if self.dir != captured.store.dir
            || self.logins != captured.store.logins
            || self.cli != captured.store.cli
        {
            return Err("usage store changed; nothing recorded".into());
        }
        let label = judged_label(&captured.roster, who).ok_or("usage has no account")?;
        let row = captured
            .roster
            .row(label)
            .ok_or("usage account is missing")?;
        self.transact_usage_within(ladder, captured, now, |raw, state, logins| {
            let implicit = raw.rows.is_empty();
            let current = if implicit {
                Roster::own()
            } else {
                bound(raw, logins)
            };
            if captured.implicit != implicit || !account_matches(row, who, &current) {
                return Err(format!(
                    "account `{label}` changed since usage was read; nothing recorded"
                ));
            }
            edit(state)
        })
    }

    /// Publishes a renewal only while its captured roster and effective target row still
    /// match, including the captured target generation. The roster read, binding check,
    /// completion checkpoint supplied by `edit`, and
    /// account edits share the account lock. A noncooperating filesystem writer can still
    /// change the roster after its read; this does not prove provider identity.
    /// # Errors
    /// An unreadable or changed roster, a changed target row or generation, lock contention,
    /// or a commit failure. A refused policy runs no edit. After admission, a durable receipt
    /// fences readers until the exact state, bindings and ledger recover together. A
    /// conflicting roster edit refuses recovery and preserves the pending receipt.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AccountRenewalPolicy",
            action = "Publish",
            project = "aterm_agent::harness::accounts::renewal_policy_tests::renewal_transition"
        )
    )]
    pub(super) fn transact_renewal_within(
        &self,
        ladder: &[std::time::Duration],
        now: i64,
        note: &mut Option<String>,
        policy: RenewalPolicy<'_>,
        edit: impl FnOnce(&mut State, &mut Logins) -> Result<Vec<Event>, String>,
    ) -> Result<Vec<Event>, String> {
        let held = self
            .acquire_within(ladder)
            .map_err(|why| why.to_string())?
            .ok_or_else(|| LOCK_HELD.to_owned())?;
        self.transact_renewal_held(&held, now, note, policy, edit)
    }

    /// Runs the same renewal admission while the caller retains the account writer.
    /// # Errors
    /// As [`Self::transact_renewal_within`], without acquiring another writer.
    pub(super) fn transact_renewal_held(
        &self,
        held: &Lock,
        now: i64,
        note: &mut Option<String>,
        policy: RenewalPolicy<'_>,
        edit: impl FnOnce(&mut State, &mut Logins) -> Result<Vec<Event>, String>,
    ) -> Result<Vec<Event>, String> {
        self.transact_roster_logins_held(
            held,
            policy.roster_path,
            now,
            note,
            |raw, state, logins| {
                policy.require_current(raw, logins)?;
                edit(state, logins)
            },
        )
    }

    /// The local logins' transaction while the caller retains the account writer lock.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AccountRenewalGeneration",
            action = "Commit",
            project = "aterm_agent::harness::accounts::generation_tests::generation_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "AccountBindingRetention",
            action = "Edit",
            project = "aterm_agent::harness::cli::tests::binding_retention::binding_transition"
        )
    )]
    pub(super) fn transact_logins_held(
        &self,
        held: &Lock,
        now: i64,
        note: &mut Option<String>,
        edit: impl FnOnce(&mut State, &mut Logins) -> Result<Vec<Event>, String>,
    ) -> Result<Vec<Event>, String> {
        self.transact_local_logins_held(held, now, note, edit)
    }
}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountSelectionCommit",
        action = "AnotherProvider",
        reason = "Select a native account provider fixture."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountSelectionCommit",
        action = "AnotherCase",
        reason = "Select a policy, supersession or write-failure fixture."
    )
)]
#[doc(hidden)]
pub fn account_selection_environment() {}

/// Outcome of a [`record_wall`] call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Recorded {
    /// The session is on record on `account`.
    Wall {
        /// The account the session runs on ([`Me::label`]).
        account: String,
        /// The window it is recorded under: the row's limit type, or [`UNNAMED_WINDOW`].
        window: String,
        /// The reset it names.
        until: Option<i64>,
        /// The ledger rows this call appended.
        events: usize,
    },
    /// No roster, or the process's login is no row's of it, nor one a renewal moved a row off
    /// ([`Me::label`]).
    NotListed,
    /// The process's home is not the default one, or it authenticates by neither its account's
    /// stored login nor its vault token ([`AuthRoute::Outside`]).
    NotStored,
    /// The row is older than the process.
    NotThisProcess,
    /// The row was written before the owner cleared this account's walls ([`pin`]).
    Cleared,
    /// What a session's screen said of the account's usage is on record now ([`record_usage`]).
    Usage {
        /// The account the session runs on ([`Me::label`]).
        account: String,
        events: usize,
    },
    /// What a session's screen said is on record for the stats alone ([`Account::weeks_said`]):
    /// nothing a decision reads moved, so no look at the accounts follows it.
    Stats {
        /// The account the session runs on ([`Me::label`]).
        account: String,
        events: usize,
    },
    /// Everything this call would write is on record.
    Unchanged,
    /// The state is not this build's to write, or a file could not be locked or written.
    Refused(String),
}

impl Recorded {
    /// Returns whether the recorder failed because `accounts.lock` remained held.
    #[must_use]
    pub fn lock_held(&self) -> bool {
        matches!(self, Recorded::Refused(why) if why == LOCK_HELD)
    }

    /// Single-word outcome code logged by the host.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Recorded::Wall { .. } => "wall",
            Recorded::Usage { .. } => "usage",
            Recorded::Stats { .. } => "stats",
            Recorded::NotListed => "not-listed",
            Recorded::NotStored => "not-stored",
            Recorded::NotThisProcess => "not-this-process",
            Recorded::Cleared => "cleared",
            Recorded::Unchanged => "unchanged",
            Recorded::Refused(_) => "refused",
        }
    }
}

/// Records a standing usage wall hit by a supervised session under a bounded store lock.
pub fn record_wall(
    store: &Store,
    captured: &UsageRoster,
    sid: &str,
    who: &ProcessLogin,
    wall: &super::upgrade::StandingWall,
    process_started: i64,
    now: i64,
) -> Recorded {
    let roster = captured.roster();
    let me = who.me(roster);
    let Some(account) = me.label() else {
        return Recorded::NotListed;
    };
    if !me.default_home || !me.route.on_account() {
        return Recorded::NotStored;
    }
    let written = i64::try_from(wall.at).unwrap_or(i64::MAX);
    if written < process_started {
        return Recorded::NotThisProcess;
    }
    let at = written.min(now);
    let until = believed_reset(wall.until.and_then(|until| i64::try_from(until).ok()), at);
    let window = wall
        .window
        .as_deref()
        .map(|w| one_line(w, WORD_MAX))
        .filter(|w| !w.is_empty())
        .unwrap_or_else(|| UNNAMED_WINDOW.to_string());
    let sid = one_line(sid, WORD_MAX);
    let mut changed = false;
    let mut cleared = false;
    let done = store.observe_usage_within(store.ladder, now, captured, who, |state| {
        let before = state.clone();
        let on_it = state
            .sessions
            .get(&sid)
            .is_some_and(|row| row.account == account);
        let mut events = if on_it {
            Vec::new()
        } else {
            note_session(state, &sid, account, None, now)
        };
        cleared = state
            .accounts
            .get(account)
            .is_some_and(|record| record.cleared_since(&window, at, now).is_some());
        events.extend(note_wall(state, account, &window, until, at, now));
        note_credits_word(state, account, wall.credits.as_ref(), at);
        changed = *state != before;
        Ok(events
            .into_iter()
            .map(|event| match event.sid {
                Some(_) => event,
                None => event.with_sid(&sid),
            })
            .collect())
    });
    match done {
        Err(why) => Recorded::Refused(why),
        Ok(_) if cleared => Recorded::Cleared,
        Ok(events) if events.is_empty() && !changed => Recorded::Unchanged,
        Ok(events) => Recorded::Wall {
            account: account.to_string(),
            window,
            until,
            events: events.len(),
        },
    }
}

fn note_credits_word(
    state: &mut State,
    account: &str,
    word: Option<&super::upgrade::CreditsWord>,
    at: i64,
) {
    use super::upgrade::CreditsWord;
    let Some(word) = word else {
        return;
    };
    let Some(record) = account_mut(state, account) else {
        return;
    };
    let held = record.credits.as_ref();
    if held.is_some_and(|held| at < held.at())
        || (*word == CreditsWord::On && matches!(held, Some(Credits::InUse(_))))
    {
        return;
    }
    record.credits = Some(match word {
        CreditsWord::On => Credits::On(at),
        CreditsWord::Off { reason, until } => Credits::Off(CreditsOff {
            reason: reason
                .as_deref()
                .map(|r| one_line(r, WORD_MAX))
                .unwrap_or_default(),
            at,
            until: until.and_then(|u| i64::try_from(u).ok()),
        }),
    });
}

/// Screen observation that usage credits are carrying the session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CreditsRead {
    /// The reset of the window they carry past.
    pub until: Option<i64>,
}

/// Screen observation of a Claude Code quota-window warning.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WarningRead {
    /// The window: the week (`true`) or the five-hour session window.
    pub weekly: bool,
    /// Whole percent used.
    pub pct: u32,
    /// Its reset, placed by the caller.
    pub reset: Option<i64>,
}

/// Screen observation of a model's week, its wall or its warning ([`Account::weeks_said`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WeekRead {
    /// The week's bucket id ([`week_bucket`]).
    pub bucket: &'static str,
    /// Whole percent used: 100 at its wall.
    pub pct: u32,
    /// Its reset, placed by the caller, where the notice named one.
    pub reset: Option<i64>,
    /// It was the model's wall.
    pub wall: bool,
}

/// Combined screen usage observations (`credits`, `warning` and `week`) passed to
/// [`record_usage`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UsageSaid {
    /// When it was true ([`NOTICES_DRAWN_S`]).
    pub at: i64,
    /// The local day of [`Self::at`].
    pub day: i64,
    /// Credits carry the session.
    pub credits: Option<CreditsRead>,
    /// The warning that a window is filling.
    pub warning: Option<WarningRead>,
    /// A model's week, for the stats alone ([`Account::weeks_said`]).
    pub week: Option<WeekRead>,
}

/// Records screen-observed usage credits, quota warnings and a model's week for a session under a
/// bounded store lock.
pub fn record_usage(
    store: &Store,
    captured: &UsageRoster,
    sid: &str,
    who: &ProcessLogin,
    said: &UsageSaid,
    now: i64,
) -> Recorded {
    let roster = captured.roster();
    let me = who.me(roster);
    let Some(account) = me.label() else {
        return Recorded::NotListed;
    };
    if !me.default_home || !me.route.on_account() {
        return Recorded::NotStored;
    }
    let at = said.at.min(now);
    let sid = one_line(sid, WORD_MAX);
    let (mut decided, mut stats) = (false, false);
    let done = store.observe_usage_within(store.ladder, now, captured, who, |state| {
        let by = state.machine.clone();
        let Some(record) = account_mut(state, account) else {
            return Ok(Vec::new());
        };
        let before = record.clone();
        let mut events = Vec::new();
        if let Some(credits) = said.credits {
            events.extend(note_in_use(record, account, credits, at, said.day, now));
        }
        if let Some(warning) = said.warning {
            events.extend(note_warning(record, account, warning, at, now));
        }
        decided = *record != before;
        if let Some(week) = said.week {
            events.extend(note_week(record, account, week, at, &by, now));
        }
        stats = record.weeks_said != before.weeks_said;
        Ok(events
            .into_iter()
            .map(|event| event.with_sid(&sid))
            .collect())
    });
    match done {
        Err(why) => Recorded::Refused(why),
        Ok(events) if decided => Recorded::Usage {
            account: account.to_string(),
            events: events.len(),
        },
        Ok(events) if stats => Recorded::Stats {
            account: account.to_string(),
            events: events.len(),
        },
        Ok(_) => Recorded::Unchanged,
    }
}

fn note_in_use(
    record: &mut Account,
    account: &str,
    read: CreditsRead,
    at: i64,
    day: i64,
    now: i64,
) -> Option<Event> {
    match record.credits.as_mut() {
        Some(held) if at < held.at() => return None,
        Some(Credits::InUse(c)) if c.since <= at && c.stands(at) => {
            c.seen = c.seen.max(at);
            c.until = c.until.max(read.until);
            return None;
        }
        Some(held) if at <= held.at() => return None,
        _ => {}
    }
    let told = record.credits_day != Some(day);
    if told {
        record.credits_day = Some(day);
    }
    record.credits = Some(Credits::InUse(CreditsUse {
        since: at,
        seen: at,
        until: read.until,
        told,
    }));
    Some(Event::new(now, Kind::Credits, account).because("in use"))
}

fn note_warning(
    record: &mut Account,
    account: &str,
    warning: WarningRead,
    at: i64,
    now: i64,
) -> Option<Event> {
    let (slot, window) = if warning.weekly {
        (&mut record.warned_week, "weekly")
    } else {
        (&mut record.warned_session, "session")
    };
    if slot.as_ref().is_some_and(|held| at < held.at) {
        return None;
    }
    let pct = warning.pct.min(PCT_MAX);
    let moved = slot
        .as_ref()
        .is_none_or(|held| (held.pct, held.reset) != (Some(pct), warning.reset));
    *slot = Some(WindowSaid {
        pct: Some(pct),
        reset: warning.reset,
        at,
    });
    moved.then(|| Event::new(now, Kind::Warned, account).because(&format!("{window} {pct}%")))
}

/// Keeps a model's week as a screen said it ([`Account::weeks_said`]): a read older than the one
/// held is dropped, and so is a warning read at the instant of a wall held (the footer keeps the
/// warning drawn before the wall, and a local command such as the `/model` the wall suggests ends
/// the turn with no newer answer); a bucket no screen names ([`week_model`]) is never kept; a read
/// that names no reset keeps the one an earlier read of the same week named after it
/// ([`WeekSaid::ends_by`]); and a move of its figure, reset or wall writes one [`Kind::Warned`]
/// row (`fable week 85%`, `fable wall`). It writes nothing else: the stats alone read it.
fn note_week(
    record: &mut Account,
    account: &str,
    week: WeekRead,
    at: i64,
    by: &str,
    now: i64,
) -> Option<Event> {
    let model = week_model(week.bucket)?;
    let held = record.weeks_said.get(week.bucket);
    if held.is_some_and(|held| at < held.at || (at == held.at && held.wall && !week.wall)) {
        return None;
    }
    let pct = week.pct.min(PCT_MAX);
    let moved = held
        .is_none_or(|held| (held.pct, held.reset, held.wall) != (Some(pct), week.reset, week.wall));
    let ends_by = held
        .filter(|held| week.reset.is_none() && held.pct.is_some())
        .and_then(|held| held.reset.or(held.ends_by))
        .filter(|end| *end > at);
    record.weeks_said.insert(
        week.bucket.to_owned(),
        WeekSaid {
            pct: Some(pct),
            reset: week.reset,
            at,
            by: by.to_owned(),
            wall: week.wall,
            ends_by,
        },
    );
    let model = model.name();
    let why = if week.wall {
        format!("{model} wall")
    } else {
        format!("{model} week {pct}%")
    };
    moved.then(|| Event::new(now, Kind::Warned, account).because(&why))
}

/// Returns the labels in roster order whose active usage-credits episode warrants a digest entry.
#[must_use]
pub fn credits_noted(roster: &Roster, state: &State, now: i64) -> Vec<String> {
    roster
        .rows
        .iter()
        .filter(|row| {
            matches!(
                state.accounts.get(&row.label).and_then(|a| a.credits.as_ref()),
                Some(Credits::InUse(c)) if c.told && c.stands(now)
            )
        })
        .map(|row| row.label.clone())
        .collect()
}

/// An account whose weekly window is nearly full with credits off ([`windows_full`]).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct WindowFull {
    /// The roster label.
    pub label: String,
    /// The reason the credits word gave for being off (`out_of_credits`, `org_level_disabled`).
    pub reason: String,
    /// The limit resets the account was last seen to have ([`Account::offer`]), while that
    /// record stands and only of a kind that gives the week back ([`gives_week_back`]): at least
    /// one left, and its use-by instant, where one was read, not yet passed. It is the record as
    /// last seen, dated by its `at`, never a count read now.
    pub grants: Option<Offer>,
}

/// Whether a reset of `kind` gives back use of the weekly window: a weekly reset refills it, a
/// full reset refills it with the five-hour window, and a Fable reset returns what Fable used of
/// it. A 5-hour reset refills the five-hour window alone, and an offer whose kind was never named
/// cannot be said to give the week anything.
const fn gives_week_back(kind: Option<ResetKind>) -> bool {
    matches!(
        kind,
        Some(ResetKind::Weekly | ResetKind::Full | ResetKind::Fable)
    )
}

/// THE ACCOUNTS WHOSE WEEKLY WINDOW IS NEARLY FULL WITH CREDITS OFF, in roster order: S5's warning
/// on the week at [`WINDOW_FULL_PCT`] or more with its reset over [`WINDOW_FULL_AHEAD_S`] off, and
/// the credits word off. Each carries the account's standing reset offer of a kind that gives the
/// week back ([`WindowFull::grants`]), which decides nothing here: an offer neither adds an account
/// nor drops one.
#[must_use]
pub fn windows_full(roster: &Roster, state: &State, now: i64) -> Vec<WindowFull> {
    roster
        .rows
        .iter()
        .filter_map(|row| {
            let account = state.accounts.get(&row.label)?;
            let Some(Credits::Off(off)) = &account.credits else {
                return None;
            };
            if off.until.is_some_and(|until| now >= until) {
                return None;
            }
            let (pct, end) = account.warned_week.as_ref()?.figure(WEEK_WINDOW_S, now)?;
            (pct >= WINDOW_FULL_PCT && end.saturating_sub(now) > WINDOW_FULL_AHEAD_S).then(|| {
                WindowFull {
                    label: row.label.clone(),
                    reason: off.reason.clone(),
                    grants: account.offer.clone().filter(|o| {
                        o.left > 0 && o.use_by.is_none_or(|by| by > now) && gives_week_back(o.kind)
                    }),
                }
            })
        })
        .collect()
}

/// Removes ended session `sid` from the store under a bounded lock wait, and touches `wake` when
/// that released a canary hold. Returns whether the state held anything of `sid`.
/// # Errors
///
/// As [`Store::transact_within`]: the state is not this build's to write, a file could not
/// be locked or written, or the lock stayed held (exactly [`LOCK_HELD`]); or `wake` could not
/// be touched.
pub fn record_gone(store: &Store, sid: &str, now: i64) -> Result<bool, String> {
    if !store.state_file().exists() {
        return Ok(false);
    }
    let sid = one_line(sid, WORD_MAX);
    let mut forgot = false;
    let mut held = false;
    let mut note = None;
    store.transact_within(store.ladder, now, &mut note, |state| {
        let holding = state.canary_holds(now);
        forgot = forget_session(state, &sid);
        held = holding && !state.canary_holds(now);
        Ok(Vec::new())
    })?;
    if held {
        store
            .touch_wake()
            .map_err(|e| format!("{}: {e}", store.wake_path().display()))?;
    }
    Ok(forgot)
}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountSigninAdmission",
        action = "Fresh",
        reason = "Bounded environment scenario: the conformance test makes no store before the product's Read"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountSigninAdmission",
        action = "Known",
        reason = "Bounded environment scenario: the conformance test seeds a readable default state and login bindings before the product's Read"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountSigninAdmission",
        action = "StateUnread",
        reason = "Bounded environment scenario: the conformance test seeds an unreadable state file before the product's Read"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountSigninAdmission",
        action = "BindingsUnread",
        reason = "Bounded environment scenario: the conformance test seeds unreadable login bindings before the product's Read"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountSigninAdmission",
        action = "StateLost",
        reason = "Bounded environment scenario: the conformance test seeds a state witness whose state file is gone before the product's Read"
    )
)]
#[doc(hidden)]
pub fn account_signin_admission_environment() {}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountSigninLock",
        action = "RetryA",
        reason = "Model bookkeeping: a refused contender may take the sign-in lock again; the retry is its next TakeA, which is bound"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountSigninLock",
        action = "RetryB",
        reason = "Model bookkeeping: a refused contender may take the sign-in lock again; the retry is its next TakeB, which is bound"
    )
)]
#[doc(hidden)]
pub fn account_signin_lock_environment() {}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountStateRetention",
        action = "Legacy",
        reason = "Bounded environment scenario: the store holds the snapshot and ledger an older aterm wrote, which the conformance test writes as bytes"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountStateRetention",
        action = "LoseSnapshot",
        reason = "Environment: the snapshot file vanishes from under the store (the conformance test renames it away)"
    )
)]
#[doc(hidden)]
pub fn account_state_retention_environment() {}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountBindingRetention",
        action = "Legacy",
        reason = "Bounded environment scenario: the store holds the bindings an older aterm wrote, which the conformance test writes as bytes"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountBindingRetention",
        action = "LoseBindings",
        reason = "Environment: the bindings file vanishes from under the store (the conformance test renames it away)"
    )
)]
#[doc(hidden)]
pub fn account_binding_retention_environment() {}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountLoginObservation",
        action = "AnotherCase",
        reason = "Model bookkeeping: the model steps through its fixed cases, and the conformance test stages each case's fixture for Record"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountLoginObservation",
        action = "Choose",
        reason = "Model bookkeeping: the model fixes the case it checks before the bound Record"
    )
)]
#[doc(hidden)]
pub fn account_login_observation_environment() {}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountUsageObservation",
        action = "AnotherCase",
        reason = "Select a captured-account, renewal or refusal fixture."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountUsageObservation",
        action = "Choose",
        reason = "Fix the native fixture before recording its observation."
    )
)]
#[doc(hidden)]
pub fn account_usage_observation_environment() {}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountLifecycleCommit",
        action = "AnotherProvider",
        reason = "Select a native provider fixture."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountLifecycleCommit",
        action = "AnotherOperation",
        reason = "Select a process lifecycle observation fixture."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountLifecycleCommit",
        action = "AnotherFault",
        reason = "Select a publication obstruction fixture."
    )
)]
#[doc(hidden)]
pub fn account_lifecycle_environment() {}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountRenewalPolicy",
        action = "AnotherCase",
        reason = "Model bookkeeping: the model steps through its fixed cases, and the conformance test stages each case's fixture for the bound Publish"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountRenewalPolicy",
        action = "Choose",
        reason = "Model bookkeeping: the model fixes the case it checks before the bound Publish"
    )
)]
#[doc(hidden)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountRenewalSnapshot",
        action = "AnotherMode",
        reason = "Select a local or shared renewal fixture."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountRenewalSnapshot",
        action = "AnotherCase",
        reason = "Select the competing writer or read-failure fixture."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountSignInPublication",
        action = "AnotherProvider",
        reason = "Select a file-backed provider fixture."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountSignInPublication",
        action = "PriorBinding",
        reason = "Select a first sign-in or an existing binding."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountSignInPublication",
        action = "AnotherFault",
        reason = "Select a retained, absent or replaced credential file."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountSharedCheckpoint",
        action = "PriorBinding",
        reason = "Select a first or replacement login fixture."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountSharedCheckpoint",
        action = "AfterCheckpoint",
        reason = "Select a failure before or after completed checkpoint creation."
    )
)]
pub fn account_renewal_policy_environment() {}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountRenewalGeneration",
        action = "AnotherCase",
        reason = "Model bookkeeping: the model steps through its fixed cases, and the conformance test stages each case's fixture for the bound Commit"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountRenewalGeneration",
        action = "Choose",
        reason = "Model bookkeeping: the model fixes the case it checks before the bound Commit"
    )
)]
#[doc(hidden)]
pub fn account_renewal_generation_environment() {}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountAttemptGeneration",
        action = "AnotherProvider",
        reason = "Model bookkeeping: select the provider whose real login and renewal paths the conformance fixture drives"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountAttemptGeneration",
        action = "AnotherMode",
        reason = "Model bookkeeping: select login or renewal before running the native fixture"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountAttemptGeneration",
        action = "AnotherCase",
        reason = "Model bookkeeping: select the concurrent binding change staged by the native fixture"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountAttemptGeneration",
        action = "Choose",
        reason = "Model bookkeeping: fix the case before the native Commit"
    )
)]
#[doc(hidden)]
pub fn account_attempt_generation_environment() {}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountSignInPolicy",
        action = "AnotherOutcome",
        reason = "Model bookkeeping: select successful authentication, wrong identity, a human step or failed manual login for the native fixture"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountSignInPolicy",
        action = "AnotherProvider",
        reason = "Model bookkeeping: select a vendor whose real sign-in paths the conformance fixture drives"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountSignInPolicy",
        action = "AnotherMode",
        reason = "Model bookkeeping: select manual login or renewal for the native fixture"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountSignInPolicy",
        action = "AnotherCase",
        reason = "Model bookkeeping: select the roster edit staged during authentication"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountSignInPolicy",
        action = "Choose",
        reason = "Model bookkeeping: fix the case before the native Publish"
    )
)]
#[doc(hidden)]
pub fn account_signin_policy_environment() {}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "UsageSource",
        action = "AnotherCase",
        reason = "Model bookkeeping: select the source mode, roster edit or store change staged before admission"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "UsageSource",
        action = "Choose",
        reason = "Model bookkeeping: fix the case before the native Record"
    )
)]
#[doc(hidden)]
pub fn account_usage_source_environment() {}

#[cfg(test)]
#[path = "accounts_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "accounts_usage_source_tests.rs"]
mod usage_source_tests;

#[cfg(test)]
#[path = "accounts_logins_tests.rs"]
mod logins_tests;

#[cfg(test)]
#[path = "accounts_binding_tests.rs"]
mod binding_tests;

#[cfg(test)]
#[path = "accounts_observation_tests.rs"]
mod observation_tests;

#[cfg(test)]
#[path = "accounts_usage_observation_tests.rs"]
mod usage_observation_tests;

#[cfg(test)]
#[path = "accounts_lifecycle_commit_tests.rs"]
mod lifecycle_commit_tests;

#[cfg(test)]
#[path = "accounts_report_read_tests.rs"]
mod report_read_tests;

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountReportRead",
        action = "AnotherProvider",
        reason = "Fixture selection chooses one of the four provider stores"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountReportRead",
        action = "AnotherShape",
        reason = "Fixture selection chooses an absent, empty, or populated roster"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountReportRead",
        action = "AnotherFault",
        reason = "Fixture selection installs a publication, read failure, or overlapping transaction"
    )
)]
#[doc(hidden)]
pub fn account_report_environment() {}

#[cfg(test)]
#[path = "accounts_tap_recovery_tests.rs"]
mod tap_recovery_tests;

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountTapRead",
        action = "AnotherProvider",
        reason = "Fixture selection chooses the provider whose local account store is read"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountTapRead",
        action = "AnotherReader",
        reason = "Fixture selection chooses the tap roster, lone login, or listed login projection"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountTapRead",
        action = "AnotherFault",
        reason = "Fixture selection installs a healthy, pending, locked, conflicting, or damaged publication"
    )
)]
#[doc(hidden)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountTapRead",
        action = "AnotherShape",
        reason = "Fixture selection chooses the empty, single-row, or multi-row pre-image"
    )
)]
pub fn account_tap_environment() {}

#[cfg(test)]
#[path = "accounts_renewal_policy_tests.rs"]
mod renewal_policy_tests;

#[cfg(test)]
#[path = "accounts_renewal_commit_tests.rs"]
mod renewal_commit_tests;

#[cfg(test)]
#[path = "accounts_selection_commit_tests.rs"]
mod selection_commit_tests;

#[cfg(test)]
#[path = "accounts_pending_selection_tests.rs"]
mod pending_selection_tests;

#[cfg(test)]
#[path = "accounts_generation_tests.rs"]
mod generation_tests;

#[cfg(test)]
#[path = "accounts_auth_scope_tests.rs"]
mod auth_scope_tests;

#[cfg(test)]
#[path = "accounts_auth_scope_codec_tests.rs"]
mod auth_scope_codec_tests;

#[cfg(test)]
#[path = "accounts_auth_scope_publication_tests.rs"]
mod auth_scope_publication_tests;

#[cfg(test)]
#[path = "accounts_lifetime_scope_tests.rs"]
mod lifetime_scope_tests;

#[cfg(test)]
#[path = "accounts_workflow_scope_tests.rs"]
mod workflow_scope_tests;

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "AccountRename",
        action = "AnotherCase",
        reason = "Model bookkeeping: the native transition stages each source and destination fixture before calling Move"
    )
)]
#[doc(hidden)]
pub fn account_rename_environment() {}

#[cfg(test)]
#[path = "accounts_rename_tests.rs"]
pub(super) mod rename_tests;

#[cfg(test)]
#[path = "accounts_lock_tests.rs"]
mod lock_tests;

#[cfg(test)]
#[path = "accounts_auth_context_tests.rs"]
mod auth_context_tests;

#[cfg(test)]
#[path = "accounts_checkpoint_read_tests.rs"]
mod checkpoint_read_tests;

#[cfg(test)]
#[path = "accounts_lapsed_rule_tests.rs"]
mod lapsed_rule_tests;
