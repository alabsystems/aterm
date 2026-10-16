// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! CLAUDE CODE ACCOUNTS, the pure core
//! (`docs/DESIGN-claude-accounts-2026-10-01.md` §4, §5, §6): the roster the
//! owner wrote, what this machine has seen of each account, the condition
//! that follows from it, and the advice a session gets at a turn end — plus
//! the small store that keeps the state file and its ledger.
//!
//! **No credential is read, stored, forwarded or renewed anywhere in this
//! module**, and no account has a name here but its LABEL. The one address
//! it holds is a roster row's own `email` (owner ruling R-2, v2 design
//! §4.5), read from the roster file to hint a renewal's sign-in and to be
//! compared in memory with what that renewal answers ([`email_matches`]):
//! no email, person or organization name is logged, printed or written
//! anywhere else. A login is a directory NAME ([`Row::dir`]), carried
//! verbatim onto a relaunch line as [`LOGIN_ENV`]; the vendor hashes that
//! string into its keychain item's name (READ in the 2.1.286 bundle, design
//! §1), so this module never canonicalizes, trims or rewrites it — a
//! renewal's new login is RECORDED as the string it was made under
//! ([`Binding::dir`], `logins.json`) and every reader takes the roster
//! through it ([`bound`]).
//!
//! **Nothing here touches a session.** Every judgment is a pure function of
//! what it is handed: the clock is the caller's `now` (Unix seconds), whether
//! a login was made in a row's directory is the caller's `enrolled` set, and
//! the transactions edit a [`State`] in memory and answer the ledger
//! [`Event`]s the caller appends. Only [`Store`], [`Roster::load`],
//! [`roster_text`], [`enrolled`] (through [`login_made`]),
//! [`profile_configured`] and the recorder ([`record_wall`],
//! [`record_gone`]) read or write a file, and only [`default_roster_path`]
//! reads this process's environment.
//!
//! The layers, in the order a caller meets them:
//!
//! * THE ROSTER ([`Roster`]): `accounts.toml` beside `aterm.toml`, rows and
//!   nothing else, admitted whole or refused by name.
//! * THE LOGINS ([`Logins`], `logins.json`): where a renewal on this machine
//!   bound each row's login ([`bound`]), the forecast of its end
//!   ([`forecast`]) and its state ([`login_state`]): derived from what is on
//!   record, never by running anything under a login.
//! * THE STATE ([`State`]): OBSERVATIONS, which are account-wide and merge by
//!   newest `at` ([`merge_observations`]: a wall, the two newest `/usage`
//!   samples, a reset offer), and MACHINE-LOCAL facts that never merge (the
//!   auth word, when a session last ran on an account, `active`, `proven`,
//!   `pinned`, the canary and what a rotation wrote over, the sessions, the
//!   rotation times, the claims).
//! * THE CONDITION ([`condition`]): derived at every read, never stored.
//! * THE ADVICE ([`advise`]): `Stay | Probe | Move | Wait | Resume`, design
//!   §6 rules 1–5 and the owner's clearing between rules 2 and 3, with
//!   [`candidates`] giving every row's rank or the reason it has none.
//! * THE TRANSACTIONS (`note_*`, [`begin_rotation`], [`take_canary`],
//!   [`prove`], [`strike`], [`pin`], [`unpin`], [`claim_reset`],
//!   [`reset_outcome`]): what the host commits under
//!   [`Store::acquire`] ([`Store::transact`]; [`Store::transact_within`]
//!   where the caller is a loop or a host thread, which must not block on
//!   the lock).
//! * THE RECORDER ([`record_wall`], [`record_gone`]): the one call a host
//!   makes with the facts of a session at a usage wall — its process's own
//!   environment ([`ProcessLogin`]) and its transcript's standing limit row
//!   (`upgrade::transcript_standing_wall`) — and the one for a session that
//!   ended. The lock is taken inside, with a BOUNDED wait ([`LOCK_LADDER`]):
//!   held past it, the call answers [`LOCK_HELD`] and its caller asks again
//!   at the session's next point.
//!
//! **What this module does not know, stated.** It has never seen two
//! accounts: the design's §1 was measured on a machine with ONE, so the
//! thresholds below are the design's constants and not tuned figures. It
//! does not know whether a login is live — [`Auth`] is what a caller last
//! observed, and an account with nothing on record ([`Auth::Unseen`]) IS
//! proposed by [`advise`]: the design gives a candidate `auth = ok`, and the
//! move onto an unseen login is its check — a login wall on it strikes it
//! signed-out ([`strike`]) — so only a login KNOWN signed out is skipped
//! here. And it cannot tell a model-scoped wall from an account-wide one:
//! every wall a caller records limits the account ([`condition`]).
//!
//! STATUS, three things and no more.
//!
//! * UNIT-TESTED (`accounts_tests.rs`), with the fixed clock and invented
//!   labels.
//! * BOUND TO THE MODEL AT TIER-1. Design §10's `SupervisorAccountRotation`
//!   (aterm-spec `supervisor_account_rotation_model`) is walked by
//!   `tests/supervise_conformance_account_rotation.rs`: [`advise`],
//!   [`condition`] and the transactions agree with the model at every state
//!   it reaches — and so does the turn-end policy's `decide_turn_end`, fed
//!   [`advise`]'s answer as a host hands it over — a bind over those states,
//!   with three accounts and two sessions, and not a proof about this file.
//!   What the model leaves out
//!   (the clock's rules, a login signed out, the probe, the reset offer and
//!   the reset's claim, which session is the canary, what a strike restores
//!   of the account before) is bound only by the unit tests.
//! * CALLED BY THE CLI AND BY THE WINDOW'S HOST. The CLI (`aterm harness
//!   accounts …`, `cli.rs`): the report reads the roster and the state
//!   ([`condition`], [`candidates`], [`next_usable`]), `add` appends a row,
//!   `login` records the word of the login it signs in ([`note_auth`]) and
//!   `use` pins and unpins. The window's harness host (aterm-gui
//!   `harness_host.rs`), for each hosted Claude Code session: it hands [`record_wall`] one whose
//!   loop opened a limit episode and [`record_gone`] one that ended; it
//!   ADVISES at a turn end ([`advise`], read without the lock) and, where
//!   the supervisor's policy asks for the move (`supervise::policy::
//!   turn_end`'s `Restart::Account`), COMMITS it in one transaction under the
//!   lock ([`begin_rotation`], [`take_canary`], [`note_session`]) before its
//!   relaunch (`relaunch::restart_under`, [`login_env`]); it takes a move
//!   that was not made back ([`Strike::NotMoved`]), strikes a candidate that
//!   answered with a wall or a lost login ([`strike`]) and proves one that
//!   served a turn ([`prove`]). THE MOVE AHEAD OF A LIMIT AND THE `/usage`
//!   PROBE (`[harness] accounts = "early"`;
//!   `docs/SPEC-claude-accounts-probe-2026-10-01.md`): this module's side of
//!   both is [`advise_past_probe`] (what a due probe stands in front of: the
//!   advice a host hands on beside [`Advice::Probe`], and the one its commit
//!   re-asks), the claim and its spacing ([`claim_probe`], [`probe_held`],
//!   [`PROBE_APART_S`]) and [`note_sample`]. WHICH OF IT A HOST CALLS is
//!   that host's module's to say (aterm-gui `harness_host.rs`) and the
//!   handoff's (`docs/HANDOFF-claude-accounts-2026-10-01.md`), not this
//!   one's. THE LIMIT RESET (`[harness] limit_reset`, off as shipped;
//!   `docs/SPEC-claude-accounts-resets-2026-10-02.md`): this module's side
//!   is who may claim one ([`reset_allowed`]: this machine's own login, and
//!   no other login on record as run), the one attempt a wall
//!   ([`claim_reset`], [`reset_in_flight`]) and its outcome
//!   ([`reset_outcome`], which ends the account's walls where a reset was
//!   used or the limit had lifted); [`Roster::own`] is the roster of a
//!   machine that lists none, for that alone; and [`note_offer`] records an
//!   offer only from a confirmation that was read or from the vendor's own
//!   reset line — the wall dialog's row names a day, which is no deadline.
//!   NOTHING OF IT HAS MET A REAL RESET: every text it stands on was read
//!   in the vendor's bundle, never on a screen. [`Advice::Resume`]
//!   commits nothing: the host hands it to the policy through the policy's
//!   own mapping (`AccountAdvice::of_roster_at`), and the policy types that
//!   wall's continuation, once a clearing. The same mapping takes the
//!   host's [`next_opening`] for a held session's wait ([`hold_until`]),
//!   and the host reads a process's settings side through
//!   [`ProcessLogin::with_settings`] and
//!   [`ProcessLogin::with_profile_store`]. ALL OF IT HAS RUN
//!   AGAINST STAND-INS ONLY: a stand-in process, a transcript file, a
//!   scripted relaunch. Nothing here has run against two real accounts.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};

use aterm_json::{Map, Value};

use super::one_line;

// ---------------------------------------------------------------------------
// Constants — every number of the design, by name
// ---------------------------------------------------------------------------

/// The roster's file name, beside `aterm.toml`.
pub const ACCOUNTS_FILE: &str = "accounts.toml";

/// The variable a relaunch line carries to choose the login. READ in the
/// vendor's bundle: set, its value names the keychain item (empty: the
/// default item) whatever the Claude home is.
pub const LOGIN_ENV: &str = "CLAUDE_SECURESTORAGE_CONFIG_DIR";

/// The most rows one roster carries, and the most accounts a state keeps.
pub const MAX_ACCOUNTS: usize = 32;

/// The longest label: `[a-z0-9][a-z0-9._-]{0,63}`.
pub const LABEL_MAX: usize = 64;

/// The one word of that grammar that is no label: the harness command's help
/// word, wherever it stands on its line (`accounts use help` asks for the
/// help). An account named so could be listed and never named.
pub const RESERVED_LABEL: &str = "help";

/// The longest `dir` a row may name, in bytes. A longer one is REFUSED, not
/// cut: the string is hashed as given, so a cut one names another login. The
/// relaunch line that must carry it is itself bounded near 1000 bytes.
pub const DIR_MAX: usize = 512;

/// The longest `email` a row may carry, in bytes (RFC 5321's path bound).
pub const EMAIL_MAX: usize = 254;

/// The longest `browser` profile name a row may carry, in characters.
pub const BROWSER_MAX: usize = 64;

/// The most bytes of `accounts.toml` that are read; a larger file is refused.
pub const MAX_ROSTER_BYTES: u64 = 256 * 1024;

/// The most bytes of `state.json` that are read; a larger file is not ours.
pub const MAX_STATE_BYTES: u64 = 1024 * 1024;

/// The state file's schema. A file of another one is refused.
pub const SCHEMA: u64 = 1;

/// The most walls kept per account (the vendor names six limit types).
pub const MAX_WALLS: usize = 8;

/// The most sessions kept, newest first.
pub const MAX_SESSIONS: usize = 256;

/// The most rotation times kept (the bound itself is at most twice
/// [`MAX_ACCOUNTS`] an hour).
pub const MAX_ROTATIONS: usize = 128;

/// The longest session id, machine id, window name and `held` word kept.
pub const WORD_MAX: usize = 64;

/// The longest reason a ledger row carries, in bytes.
pub const WHY_MAX: usize = 200;

/// The longest ledger line that is parsed.
pub const MAX_EVENT_LINE_BYTES: usize = 4096;

/// The ledger is cut back to [`LEDGER_KEEP_ROWS`] once it passes this size.
pub const LEDGER_MAX_BYTES: u64 = 1024 * 1024;

/// The rows a cut ledger keeps.
pub const LEDGER_KEEP_ROWS: usize = 1024;

/// A percent above this is no figure (the vendor paints whole percents).
pub const PCT_MAX: u32 = 1000;

/// A wall that names its reset stands this long past it (the supervisor's
/// own `reset_grace`: Claude Code's limits lift a little after the instant
/// they print).
pub const RESET_GRACE_S: i64 = 150;

/// A wall observed AFTER its own reset stands this long from the
/// observation: 10 minutes the first time, 30 from the second on.
pub const LATE_WALL_S: [i64; 2] = [10 * 60, 30 * 60];

/// A wall that names no reset stands this long from the observation: 30
/// minutes, then 2 hours, then 6 from the third observation on.
pub const NO_RESET_WALL_S: [i64; 3] = [30 * 60, 2 * 3600, 6 * 3600];

/// A wall seen again more than this long after it stopped standing is a new
/// wall, counted from one (the ladders' last rung).
pub const WALL_EPISODE_S: i64 = 6 * 3600;

/// The session window's share at which an account is `low`.
pub const LOW_SESSION_PCT: u32 = 90;

/// The week's share at which an account is `low`.
pub const LOW_WEEK_PCT: u32 = 97;

/// A session is not moved EARLY off an account whose low window resets
/// within this long: what is left of it is about to be replaced. The guard
/// is about leaving ([`Sample::leave_early`]); the account itself is `low`
/// until that window ends ([`Sample::low`]).
pub const RESET_SOON_S: i64 = 15 * 60;

/// A `low` account is moved into only when no account becomes usable within
/// this long.
pub const LOW_ONLY_AFTER_S: i64 = 15 * 60;

/// A wait is for the earliest DEDICATED instant when that is within this
/// long of the earliest instant of any account.
pub const DEDICATED_WAIT_S: i64 = 15 * 60;

/// An account holding a reset offer that expires within this long is not
/// left early: resets are offered only at a wall.
pub const OFFER_EXPIRING_S: i64 = 24 * 3600;

/// A reset claim with no outcome is IN FLIGHT this long ([`reset_in_flight`]),
/// and no longer: the vendor bounds its own claim at 35 s (READ in the 2.1.287
/// bundle), and the rest is the loop's waits and a person's grace. Past it
/// the claim is an attempt nobody finished ([`ResetClaimed::Attempted`]), and
/// it holds no other session ([`ResetClaimed::Held`]).
pub const RESET_CLAIM_S: i64 = 10 * 60;

/// How long before a process's start another login must not have run for
/// that process's reset to be allowed ([`reset_allowed`]): the vendor posts a
/// reset to the HOME PROFILE's organization, and the profile follows a
/// process that started when it was over 24 h old (READ in the 2.1.287
/// bundle), plus the hour a session's row may lag ([`Account::ran_at`]).
pub const RESET_PROFILE_S: i64 = 25 * 3600;

/// The probe is due no sooner than this after the last sample or claim.
pub const PROBE_MIN_S: i64 = 3 * 60;

/// No account is probed within this long of ANY account's claim: the
/// vendor answers a usage read from the home's snapshot for 60 s, and a
/// claim precedes its fetch by up to the probe's own waits.
///
/// READ in the 2.1.287 bundle (the probe's spec, §0): a usage read is
/// answered from the home's snapshot, with no marker, while the snapshot is
/// under 60 s old and the asking process has had no response since it was
/// fetched — and the snapshot is tagged with the HOME PROFILE's account,
/// whichever login fetched it. So two accounts probed inside one minute
/// could each be shown the other's figures. The figure is the vendor's 60
/// plus 30 for the probe's own waits (its echo wait, its open wait, a
/// margin), and it is HALF of [`PROBE_MIN_S`], which is what keeps the
/// same-account floor with the ONE claim field ([`State::probe`]): another
/// account's claim can overwrite this account's no sooner than 90 s after
/// it, and then holds this account off for 90 s more. NOT OBSERVED: this
/// machine has one account.
pub const PROBE_APART_S: i64 = 90;

/// The probe is due no later than this after the last sample.
pub const PROBE_MAX_S: i64 = 20 * 60;

/// The probe's interval when no burn is known.
pub const PROBE_DEFAULT_S: i64 = 10 * 60;

/// The window the machine's rotation bound counts over.
pub const ROTATION_WINDOW_S: i64 = 3600;

/// How long the one session on an account that has not served a response
/// yet holds every other session back ([`StayWhy::Canary`]). Past it the
/// next session takes its place ([`take_canary`]): a canary nobody answers
/// for is not a hold for ever.
pub const CANARY_HOLD_S: i64 = 10 * 60;

/// The most rotations whose undo is kept ([`State::undo`]): one for each
/// rotation in a row past an account that had not answered and could not be
/// run on. Past it the oldest is forgotten, and undoing that one puts back
/// `active` and its proof and nothing more.
pub const MAX_UNDO: usize = 8;

/// How far ahead of the reader's clock an observation's `at` may stand. One
/// stamped later is read as made then: a clock set back, or another
/// machine's running ahead, must not make an observation newer than
/// anything this machine will see.
pub const STAMP_SKEW_S: i64 = 5 * 60;

/// The session window's length (MEASURED in the bundle: `five_hour`,
/// `windowSeconds: 18000`): a figure whose reset was not read is void once
/// this long has passed since it was sampled.
pub const SESSION_WINDOW_S: i64 = 5 * 3600;

/// The week's length, for the same bound on a weekly figure.
pub const WEEK_WINDOW_S: i64 = 7 * 24 * 3600;

/// How far past its own row a wall's reset may stand and still be believed
/// ([`note_wall`]): eight days, a day past the longest window the vendor
/// names ([`WEEK_WINDOW_S`]). A reset further off than that is no window's,
/// and the wall is recorded as naming none ([`NO_RESET_WALL_S`]).
pub const RESET_BELIEVED_S: i64 = 8 * 24 * 3600;

/// A session's row older than this is dropped when the state is saved
/// ([`drop_stale_sessions`]): thirty days. The host rewrites a live
/// session's row within the hour, so one this old is a tab that ended with
/// no word.
pub const SESSION_KEEP_S: i64 = 30 * 24 * 3600;

/// How often a session's loop says again what its screen says of usage while
/// that shows unchanged (`supervise::run`'s usage watch): ten minutes. Each
/// telling is stamped afresh with the newest answer its screen was drawn
/// after ([`UsageSaid::at`]): a notice that stands over new answers is said
/// of them, one that stands over none says nothing new.
pub const CREDITS_RETELL_S: i64 = 10 * 60;

/// HOW OLD AN ANSWER'S ROW MUST BE AS A SCREEN IS READ for the screen's
/// usage notices to be stamped with it ([`UsageSaid::at`]): two whole
/// seconds of the transcript's, so one at least. The screen is read before
/// the transcript is, and Claude Code redraws its notices from an answer's
/// headers about when it writes that answer's row: stamped with an answer
/// written around the read, a footer still drawn from the answer before it
/// carried its figure past a limit reset between the two, and the redraw
/// that took the warning away said nothing to undo it (the review of S5,
/// round 5). Stamped with an answer older than its footer, a figure is only
/// older than it was: a refill since outranks it, and the next telling
/// stamps it again.
pub const NOTICES_DRAWN_S: i64 = 2;

/// Credits are taken as in use this long past the newest answer seen served
/// on them ([`CreditsUse::stands`]): three of [`CREDITS_RETELL_S`].
pub const CREDITS_STALE_S: i64 = 3 * CREDITS_RETELL_S;

/// The weekly window counts as NEARLY FULL from this figure ([`windows_full`]).
pub const WINDOW_FULL_PCT: u32 = 90;

/// ...while its reset is more than this far off: a day. A window that resets
/// sooner is a wait with an end, and no line.
pub const WINDOW_FULL_AHEAD_S: i64 = 24 * 3600;

/// THE BOUNDED WAIT FOR `accounts.lock` ([`Store::acquire_within`]): the
/// pauses between one try and the next, two seconds in all. What a loop or
/// a host thread waits for a lock that is held for a read-edit-write, and
/// no longer: a holder that is stopped (a suspended `aterm harness
/// accounts`) must not stop every session that looks at its wall.
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

/// What a bounded take answers when the lock stayed held
/// ([`Store::transact_within`], [`Recorded::Refused`], [`record_gone`]):
/// exactly this, so a caller can tell "ask again" from a state that is not
/// ours to write ([`Recorded::lock_held`]). It names no file.
pub const LOCK_HELD: &str = "the accounts lock is held";

/// The window name a candidate that failed with no wall of its own is
/// struck under ([`Strike::Failed`]): a wall naming no reset.
pub const STRUCK_WINDOW: &str = "struck";

/// The window name a wall is recorded under when its row names no limit
/// type and its text names none either ([`record_wall`]).
pub const UNNAMED_WINDOW: &str = "limit";

/// The limit type of the five-hour window (`quotaLimits.rateLimitType`), the
/// one Claude Code's `session limit` warning names ([`Account::warned_session`]).
pub const SESSION_LIMIT: &str = "five_hour";

/// The limit type of the week of all models, the one its `weekly limit`
/// warning names ([`Account::warned_week`]); a model's own week
/// (`seven_day_opus`, `seven_day_overage_included`: `Current week (Fable)`) is
/// another window.
pub const WEEK_LIMIT: &str = "seven_day";

/// The variable that moves a process's Claude home. READ in the vendor's
/// bundle: set, it is the home, and — with [`LOGIN_ENV`] unset — the
/// directory its keychain item is named by too.
pub const HOME_ENV: &str = "CLAUDE_CONFIG_DIR";

/// THE VARIABLES THAT GIVE A PROCESS A CREDENTIAL OR A PROVIDER OF ITS OWN,
/// so that it does not authenticate by its stored login — beside the ones
/// that give it a route ([`super::netwatch::ROUTE_KEYS`]: a base URL, the
/// three cloud providers, a unix socket, a proxy), which
/// [`ProcessLogin::of_env`] reads from that list and does not copy.
///
/// Each literal was READ in the 2.1.286 bundle's strings, and that is all
/// that is claimed of it:
///
/// * `ANTHROPIC_API_KEY`, `ANTHROPIC_AUTH_TOKEN`,
///   `CLAUDE_CODE_API_KEY_FILE_DESCRIPTOR` and `CLAUDE_CODE_OAUTH_TOKEN` are
///   named together in the vendor's own print of its auth state;
///   `CLAUDE_CODE_OAUTH_REFRESH_TOKEN` is read from the environment
///   (`process.env.…`).
/// * The three `…_FILE_DESCRIPTOR` names are of one list of four, and the
///   gateway's is read as a `gateway token`. (The fourth,
///   `CLAUDE_CODE_WEBSOCKET_AUTH_FILE_DESCRIPTOR`, is not here: what it
///   authenticates was not read.)
/// * The four `CLAUDE_CODE_USE_…` names head one list with
///   `CLAUDE_CODE_USE_BEDROCK`, `…_VERTEX` and `…_FOUNDRY` (route keys
///   already), and all but the gateway's are tested in one chain with
///   those three.
/// * `CLAUDE_CODE_PROVIDER_MANAGED_BY_HOST` and
///   `CLAUDE_CODE_HOST_AUTH_ENV_VAR` are tested in one predicate with
///   `ANTHROPIC_UNIX_SOCKET` (a route key already).
///
/// Which of them an interactive session acts on was NOT traced, and it does
/// not need to be: a name here that the vendor ignores costs a session whose
/// wall is recorded for nobody, never a wall on the wrong account. A name
/// MISSING here costs the other way, and the list is what was read, not a
/// proof that it is whole.
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

/// THE VARIABLES OF THE VENDOR'S `profile` SOURCE, which its token chain
/// asks AHEAD of the stored login (`… → apiKeyHelper → profile →
/// claude.ai`). READ in the 2.1.286 and the 2.1.287 bundles' strings, the
/// same code in both, and that is all that is claimed:
///
/// * `ANTHROPIC_PROFILE` names a profile outright
///   (`process.env.ANTHROPIC_PROFILE?.trim()`).
/// * `ANTHROPIC_FEDERATION_RULE_ID` and `ANTHROPIC_ORGANIZATION_ID` are the
///   vendor's own test for a federated identity given by the environment
///   (one list of the two, `every` one set).
/// * `ANTHROPIC_IDENTITY_TOKEN` and `ANTHROPIC_IDENTITY_TOKEN_FILE` stand in
///   one list with those two, `ANTHROPIC_PROFILE` and `ANTHROPIC_CONFIG_DIR`;
///   what reads them was not traced.
/// * `ANTHROPIC_CONFIG_DIR` moves the profile store itself. The host's look
///   follows it where the process's own environment sets it
///   ([`profile_configs_dir`]); it is counted all the same, because a
///   settings file's `env` block may set it where that look does not follow.
///
/// EACH COUNTS ALONE. The vendor asks for the two federation names
/// together; counting one errs to a session that is left alone (the rule of
/// [`CREDENTIAL_KEYS`]), and covers a pair split between the environment and
/// a settings file's `env` block, which this module reads one at a time.
/// The third way to that source is no variable: a config under the profile
/// store, which the host looks for ([`profile_configured`],
/// [`ProcessLogin::with_profile_store`]).
pub const PROFILE_KEYS: &[&str] = &[
    "ANTHROPIC_PROFILE",
    "ANTHROPIC_FEDERATION_RULE_ID",
    "ANTHROPIC_ORGANIZATION_ID",
    "ANTHROPIC_IDENTITY_TOKEN",
    "ANTHROPIC_IDENTITY_TOKEN_FILE",
    "ANTHROPIC_CONFIG_DIR",
];

// ---------------------------------------------------------------------------
// 1. The roster (design §4)
// ---------------------------------------------------------------------------

/// Whose budget an account is. A contention class, not a person class: a
/// `shared` account is the owner's own, which the owner's OTHER machines
/// also draw on (design §2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum Class {
    /// This machine's alone: tried first.
    #[default]
    Dedicated,
    /// Drawn on from elsewhere too: tried after every dedicated one.
    Shared,
}

impl Class {
    /// The roster's spelling.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Class::Dedicated => "dedicated",
            Class::Shared => "shared",
        }
    }

    /// One `class =` value; anything else is no class.
    #[must_use]
    pub fn parse(word: &str) -> Option<Class> {
        match word {
            "dedicated" => Some(Class::Dedicated),
            "shared" => Some(Class::Shared),
            _ => None,
        }
    }
}

/// One roster row, as `accounts.toml` carries it — and, read through
/// [`bound`], with this machine's own renewal of its login applied.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Row {
    /// The only name the account has anywhere: `[a-z0-9][a-z0-9._-]{0,63}`.
    pub label: String,
    /// Dedicated or shared.
    pub class: Class,
    /// The login's directory NAME, VERBATIM as the file gave it (a trailing
    /// slash and a space survive). `None` is the login this machine already
    /// has (the default keychain item). Read through [`bound`], the login a
    /// renewal bound in its place ([`Binding::dir`]).
    pub dir: Option<String>,
    /// THE ACCOUNT'S EMAIL (owner ruling R-2, design §4.5): the `--email`
    /// hint a renewal's sign-in is opened with and the address its check
    /// compares in memory ([`email_matches`]). It is read from this file
    /// and nowhere else, and never printed, logged or kept beside it. `None`:
    /// the row renews nothing, and behaves exactly as a row did before
    /// renewal existed.
    pub email: Option<String>,
    /// The browser profile that holds the account's claude.ai session, by
    /// its display name (`Claude m7-a`): where a renewal's page is opened.
    /// `None`: the system's default browser.
    pub browser: Option<String>,
    /// The logins a renewal bound this row AWAY FROM on this machine, newest
    /// first ([`Binding::was`]; `None` is the default login): filled by
    /// [`bound`] alone, and empty as the file is read. A process still on
    /// one of them is the row's account on a login that is no longer its own
    /// ([`Me::left`]).
    pub was: Vec<Option<String>>,
}

/// An admitted `accounts.toml`: the rows, in file order. Empty is the
/// packaged state, and with it every judgment here is inert.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Roster {
    /// The rows, in file order.
    pub rows: Vec<Row>,
}

/// Whether `label` is `[a-z0-9][a-z0-9._-]{0,63}` and not
/// [`RESERVED_LABEL`].
#[must_use]
pub fn admissible_label(label: &str) -> bool {
    label_refusal(label).is_none()
}

/// Why `label` can name no account, as the words after the label in a
/// refusal — or `None` when it can.
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

/// Why a `dir` cannot be a row's, or `None` when it can.
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

/// Why an `email` cannot be a row's, or `None` when it can: at most
/// [`EMAIL_MAX`] bytes, no whitespace or control character, and ONE `@` with
/// something either side of it. The refusal never quotes the address.
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

/// Why a `browser` profile name cannot be a row's, or `None` when it can:
/// not empty, at most [`BROWSER_MAX`] characters, none of them a control
/// character.
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

/// Whether the address a login answers to is the row's: equal ignoring
/// ASCII case, as an address's domain is (and as the vendor's own sign-in
/// treats the local part). COMPARED IN MEMORY: neither side is kept, logged
/// or printed by anything that calls this.
#[must_use]
pub fn email_matches(row: &str, answered: &str) -> bool {
    row.eq_ignore_ascii_case(answered.trim())
}

/// A caller's text as it is quoted into a refusal: one bounded line.
fn shown(text: &str) -> String {
    one_line(text, 80)
}

impl Roster {
    /// Read one `accounts.toml`, or name the first thing that breaks it.
    ///
    /// Admission is WHOLE-FILE and fail-closed: one bad row refuses the file
    /// rather than loading the rest, because a roster loaded minus a row
    /// rotates somewhere the owner did not write. An empty file is the empty
    /// roster.
    ///
    /// # Errors
    ///
    /// Each names the row and the reason: the text is not TOML; an unknown
    /// top-level key or table; `account` is not `[[account]]` rows; more than
    /// [`MAX_ACCOUNTS`] rows; a row with an unknown key, with no label, an
    /// inadmissible one ([`RESERVED_LABEL`] by name) or a repeated one, with
    /// no class or one that is neither `dedicated` nor `shared`, with a `dir`
    /// that is not a string, is relative, holds a control character, is
    /// longer than [`DIR_MAX`] bytes or repeats another row's; with an
    /// `email` that is not one address ([`email_refusal`]; never quoted) or
    /// repeats another row's, or a `browser` that is not a profile's name
    /// ([`browser_refusal`]); or more than one row with no `dir`.
    pub fn parse(text: &str) -> Result<Roster, String> {
        let doc = aterm_toml::from_str::<aterm_toml::Value>(text).map_err(|e| {
            format!(
                "{ACCOUNTS_FILE}: not valid TOML ({})",
                shown(e.to_string().lines().next().unwrap_or(""))
            )
        })?;
        let Some(top) = doc.as_table() else {
            return Err(format!("{ACCOUNTS_FILE}: not a TOML document"));
        };
        for (key, value) in top {
            if key != "account" {
                let what = if value.as_table().is_some() {
                    "table"
                } else {
                    "key"
                };
                return Err(format!(
                    "{ACCOUNTS_FILE}: unknown {what} `{}`; this file holds [[account]] rows and \
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
                    "{ACCOUNTS_FILE}: `account` is a TOML {}, not [[account]] rows",
                    other.type_str()
                ));
            }
        };
        if items.len() > MAX_ACCOUNTS {
            return Err(format!(
                "{ACCOUNTS_FILE}: {} rows; a roster holds at most {MAX_ACCOUNTS}",
                items.len()
            ));
        }
        let mut rows: Vec<Row> = Vec::with_capacity(items.len());
        for (i, item) in items.iter().enumerate() {
            let row = row_of(item, i + 1)?;
            if let Some(twin) = rows.iter().position(|r| r.label == row.label) {
                return Err(format!(
                    "{ACCOUNTS_FILE}: label `{}` is listed twice (rows {} and {})",
                    row.label,
                    twin + 1,
                    i + 1
                ));
            }
            if let Some(twin) = rows.iter().find(|r| {
                r.email
                    .as_deref()
                    .zip(row.email.as_deref())
                    .is_some_and(|(a, b)| a.eq_ignore_ascii_case(b))
            }) {
                return Err(format!(
                    "{ACCOUNTS_FILE}: rows `{}` and `{}` name one email; an account is one row",
                    twin.label, row.label
                ));
            }
            if let Some(twin) = rows.iter().find(|r| r.dir == row.dir) {
                return Err(match &row.dir {
                    Some(_) => format!(
                        "{ACCOUNTS_FILE}: rows `{}` and `{}` name one dir; a login is one account",
                        twin.label, row.label
                    ),
                    None => format!(
                        "{ACCOUNTS_FILE}: rows `{}` and `{}` both name no dir; only one row is \
                         this machine's own login",
                        twin.label, row.label
                    ),
                });
            }
            rows.push(row);
        }
        Ok(Roster { rows })
    }

    /// Read the roster at `path`. A file that is not there is the EMPTY
    /// roster: the normal state of a machine that has listed no account, and
    /// never an error.
    ///
    /// # Errors
    ///
    /// The file exists and cannot be read, is larger than
    /// [`MAX_ROSTER_BYTES`], is not UTF-8, or does not admit
    /// ([`Roster::parse`]).
    pub fn load(path: &Path) -> Result<Roster, String> {
        match roster_text(path)? {
            None => Ok(Roster::default()),
            Some(text) => Roster::parse(&text),
        }
    }

    /// The row with this label.
    #[must_use]
    pub fn row(&self, label: &str) -> Option<&Row> {
        self.rows.iter().find(|r| r.label == label)
    }

    /// The row a process runs under, from the value of its own
    /// [`LOGIN_ENV`]: unset or empty is the row with no `dir`; anything else
    /// is the row whose `dir` equals it BYTE FOR BYTE (the vendor hashes the
    /// string as given, so `/x` and `/x/` are two logins).
    #[must_use]
    pub fn row_for_login(&self, login: Option<&str>) -> Option<&Row> {
        match login {
            None | Some("") => self.rows.iter().find(|r| r.dir.is_none()),
            Some(dir) => self.rows.iter().find(|r| r.dir.as_deref() == Some(dir)),
        }
    }

    /// Whether it lists TWO OR MORE accounts: with one, or none, there is no
    /// account to tell from another, and nothing names one
    /// ([`footer_label`], [`several_listed`]).
    #[must_use]
    pub fn several(&self) -> bool {
        self.rows.len() >= 2
    }

    /// THE LOGIN OF THE MACHINE'S ONE ACCOUNT, where that is a directory of
    /// its own: the `dir` of a roster that lists ONE row — through this
    /// machine's logins ([`bound`]), the login a renewal bound it to. One
    /// account can move no session, so a relaunch line on it may carry the
    /// usage tap as a line on the default login does
    /// (`super::tap::Armed::lone_login`). `None` with no row, with several,
    /// and where the one row is the default login.
    #[must_use]
    pub fn lone_login(&self) -> Option<&str> {
        match self.rows.as_slice() {
            [row] => row.dir.as_deref().filter(|dir| !dir.is_empty()),
            _ => None,
        }
    }

    /// THE ROSTER OF A MACHINE THAT LISTS NO ACCOUNT, for a limit reset
    /// alone: one row, this machine's own login under the label the first
    /// `accounts add` gives it (`super::cli::ACCOUNTS_OWN_LABEL`), dedicated,
    /// with no `dir`. A reset needs no roster, and its claim
    /// ([`claim_reset`]), the clearing ([`reset_outcome`]) and the ledger's
    /// rows all need a label. A caller hands it over only where the roster
    /// file is MISSING or lists nothing: a roster that was refused stands
    /// for no roster at all, and resets nothing.
    #[must_use]
    pub fn own() -> Roster {
        Roster {
            rows: vec![Row {
                label: super::cli::ACCOUNTS_OWN_LABEL.to_owned(),
                ..Row::default()
            }],
        }
    }

    /// THE ROW WHOSE LOGIN A RENEWAL MOVED AWAY FROM `login` on this machine
    /// ([`Row::was`], filled by [`bound`]): the same account, on a login that
    /// is no longer its own. Byte for byte, as [`Roster::row_for_login`]
    /// reads a login: unset and empty are both the default login. `None`
    /// where the login is a row's own or no row's.
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
}

/// The text of the roster file at `path`, as [`Roster::load`] reads it and
/// `accounts add` appends to it ([`append_row`]): at most
/// [`MAX_ROSTER_BYTES`], and `None` when there is no file. It is NOT admitted
/// here: the text of a roster that refuses is still its text.
///
/// # Errors
///
/// The file exists and cannot be read, is larger than [`MAX_ROSTER_BYTES`]
/// or is not UTF-8 — named with its path.
pub fn roster_text(path: &Path) -> Result<Option<String>, String> {
    read_bounded(path, MAX_ROSTER_BYTES).map_err(|e| format!("{}: {e}", path.display()))
}

/// One `[[account]]` item as a row; `n` is its 1-based place in the file.
fn row_of(item: &aterm_toml::Value, n: usize) -> Result<Row, String> {
    let Some(table) = item.as_table() else {
        return Err(format!(
            "{ACCOUNTS_FILE}: row {n} is a TOML {}, not an [[account]] table",
            item.type_str()
        ));
    };
    let label = match table.get("label") {
        Some(aterm_toml::Value::String(label)) => label,
        Some(other) => {
            return Err(format!(
                "{ACCOUNTS_FILE}: row {n}: its label is a TOML {}, not a string",
                other.type_str()
            ));
        }
        None => return Err(format!("{ACCOUNTS_FILE}: row {n} has no label")),
    };
    if let Some(why) = label_refusal(label) {
        return Err(format!(
            "{ACCOUNTS_FILE}: row {n}: label `{}` {why}",
            shown(label)
        ));
    }
    if let Some(key) = table
        .keys()
        .find(|k| !matches!(k.as_str(), "label" | "class" | "dir" | "email" | "browser"))
    {
        return Err(format!(
            "{ACCOUNTS_FILE}: row `{label}` has an unknown key `{}`; a row holds label, class, \
             dir, email and browser",
            shown(key)
        ));
    }
    let class = match table.get("class") {
        Some(aterm_toml::Value::String(word)) => Class::parse(word).ok_or_else(|| {
            format!(
                "{ACCOUNTS_FILE}: row `{label}`: class `{}` is neither `dedicated` nor `shared`",
                shown(word)
            )
        })?,
        Some(other) => {
            return Err(format!(
                "{ACCOUNTS_FILE}: row `{label}`: its class is a TOML {}, not `dedicated` or \
                 `shared`",
                other.type_str()
            ));
        }
        None => {
            return Err(format!(
                "{ACCOUNTS_FILE}: row `{label}` has no class (`dedicated` or `shared`)"
            ));
        }
    };
    let dir = match table.get("dir") {
        None => None,
        Some(aterm_toml::Value::String(dir)) => {
            if let Some(why) = dir_refusal(dir) {
                return Err(format!("{ACCOUNTS_FILE}: row `{label}`: its dir {why}"));
            }
            Some(dir.clone())
        }
        Some(other) => {
            return Err(format!(
                "{ACCOUNTS_FILE}: row `{label}`: its dir is a TOML {}, not a string",
                other.type_str()
            ));
        }
    };
    // The address is never quoted back: the refusal names the row alone.
    let email = match table.get("email") {
        None => None,
        Some(aterm_toml::Value::String(email)) => {
            if let Some(why) = email_refusal(email) {
                return Err(format!("{ACCOUNTS_FILE}: row `{label}`: its email {why}"));
            }
            Some(email.clone())
        }
        Some(other) => {
            return Err(format!(
                "{ACCOUNTS_FILE}: row `{label}`: its email is a TOML {}, not a string",
                other.type_str()
            ));
        }
    };
    let browser = match table.get("browser") {
        None => None,
        Some(aterm_toml::Value::String(browser)) => {
            if let Some(why) = browser_refusal(browser) {
                return Err(format!("{ACCOUNTS_FILE}: row `{label}`: its browser {why}"));
            }
            Some(browser.clone())
        }
        Some(other) => {
            return Err(format!(
                "{ACCOUNTS_FILE}: row `{label}`: its browser is a TOML {}, not a string",
                other.type_str()
            ));
        }
    };
    Ok(Row {
        label: label.clone(),
        class,
        dir,
        email,
        browser,
        was: Vec::new(),
    })
}

/// The variable and value a relaunch line carries for `row`: [`LOGIN_ENV`]
/// and the row's `dir` verbatim, or the EMPTY value for the default login —
/// set and empty, which names the default keychain item whatever variable
/// the shell had before (READ in the bundle).
#[must_use]
pub fn login_env(row: &Row) -> (&'static str, String) {
    (LOGIN_ENV, row.dir.clone().unwrap_or_default())
}

/// Why `row` would not admit, as the reader names it — its label, its `dir`,
/// its `email` (never quoted) or its `browser` — or `None` when it would: the
/// reader's own checks, asked BEFORE a row is written, because a roster
/// refuses whole.
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
    row.browser
        .as_deref()
        .and_then(browser_refusal)
        .map(|why| format!("row `{label}`: its browser {why}"))
}

/// ONE `[[account]]` row as text, or `None` when the row would not admit
/// (the label, or the `dir`): the reader's own checks, asked BEFORE the row
/// is written, because a roster refuses whole.
#[must_use]
pub fn row_toml(label: &str, class: Class, dir: Option<&str>) -> Option<String> {
    row_text(&Row {
        label: label.to_owned(),
        class,
        dir: dir.map(str::to_owned),
        ..Row::default()
    })
}

/// [`row_toml`] of a whole row, its `email` and `browser` too: the text
/// `accounts add --email` and `accounts adopt-default` write.
#[must_use]
pub fn row_text(row: &Row) -> Option<String> {
    if row_refusal(row).is_some() {
        return None;
    }
    let mut out = format!(
        "[[account]]\nlabel = \"{}\"\nclass = \"{}\"\n",
        row.label,
        row.class.as_str()
    );
    // The parser's own canonical spelling of a string, so what is read back
    // is the string that was given.
    let mut put = |key: &str, value: Option<&str>| {
        if let Some(value) = value {
            out.push_str(&format!("{key} = {}\n", aterm_toml::Value::from(value)));
        }
    };
    put("dir", row.dir.as_deref());
    put("email", row.email.as_deref());
    put("browser", row.browser.as_deref());
    Some(out)
}

/// ONE row as an inline table, or `None` when the row would not admit: the
/// spelling of a row inside `account = [ … ]`, which is how a roster that
/// defines `account` inline is added to by hand ([`append_row`]).
#[must_use]
pub fn row_inline(label: &str, class: Class, dir: Option<&str>) -> Option<String> {
    row_inline_of(&Row {
        label: label.to_owned(),
        class,
        dir: dir.map(str::to_owned),
        ..Row::default()
    })
}

/// What a row's `email` is written as in a row to add by hand
/// ([`row_inline_of`]): a refusal is printed, and an address is never.
pub const EMAIL_BY_HAND: &str = "<the --email you gave>";

/// [`row_inline`] of a whole row. Its `email`, where it has one, is written
/// as [`EMAIL_BY_HAND`]: this text is printed in a refusal, never written to
/// a file, and no address is ever printed.
fn row_inline_of(row: &Row) -> Option<String> {
    if row_refusal(row).is_some() {
        return None;
    }
    let mut out = format!(
        "{{ label = \"{}\", class = \"{}\"",
        row.label,
        row.class.as_str()
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
    if let Some(browser) = &row.browser {
        out.push_str(&format!(
            ", browser = {}",
            aterm_toml::Value::from(browser.as_str())
        ));
    }
    out.push_str(" }");
    Some(out)
}

/// `existing` (the whole of `accounts.toml`, or empty when there is none)
/// with one row appended: the text `accounts add` writes.
///
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

/// `existing` with `rows` appended, in order: [`append_row`] for the first
/// `accounts add`, which lists this machine's own login before the row it
/// was asked for.
///
/// # Errors
///
/// A row would not admit, `existing` does not admit, or the whole together
/// does not (a repeated label, `dir` or `email`, a second row with no `dir`,
/// a 33rd row) — each as [`Roster::parse`] names it, so nothing is written
/// that the next read refuses. And a roster that admits but defines
/// `account` INLINE (`account = [ { … } ]`, or the empty `account = []`): an
/// `[[account]]` table cannot extend an array written that way, so the
/// refusal says so and carries EVERY row to add by hand ([`row_inline`]), in
/// order, an email written as [`EMAIL_BY_HAND`].
pub fn append_rows(existing: &str, rows: &[Row]) -> Result<String, String> {
    if let Some(why) = rows.iter().find_map(row_refusal) {
        return Err(why);
    }
    Roster::parse(existing)?;
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
    // `existing` admits and each row is TOML of its own, so text that is not
    // TOML once they are joined is the join's doing: the one way that
    // happens is an `account` array defined inline.
    if aterm_toml::from_str::<aterm_toml::Value>(&out).is_err() {
        let by_hand: Vec<String> = rows.iter().filter_map(row_inline_of).collect();
        let these = if by_hand.len() == 1 {
            "this row"
        } else {
            "these rows"
        };
        return Err(format!(
            "{ACCOUNTS_FILE} defines `account` inline (`account = [ … ]`), which an [[account]] \
             table cannot extend; rewrite its rows as [[account]] tables, or add {these} to \
             that array by hand: {}",
            by_hand.join(", ")
        ));
    }
    Roster::parse(&out)?;
    Ok(out)
}

/// The labels enrolled on this machine: a row with no `dir` (this machine's
/// own login), and a row in whose `dir` A LOGIN WAS MADE ([`login_made`]).
/// READS the filesystem, bounded: one `read_dir` per row that names a `dir`,
/// and its first entry alone. A login made says nothing of whether it is
/// still signed in ([`Auth`]).
///
/// A DIRECTORY THAT EXISTS IS NOT A LOGIN. `accounts add` makes the row's
/// directory before anybody has signed in under it; counted by its
/// existence, that row would read `unseen` (fresh) and be the report's
/// `next:` and a rotation's candidate with no login behind it. It is listed,
/// and not enrolled, until the vendor's flow has written its home there.
#[must_use]
pub fn enrolled(roster: &Roster) -> BTreeSet<String> {
    roster
        .rows
        .iter()
        .filter(|r| r.dir.as_deref().is_none_or(|d| login_made(Path::new(d))))
        .map(|r| r.label.clone())
        .collect()
}

/// Whether a login was made in the login directory `dir`: it is a directory
/// here and HOLDS AT LEAST ONE ENTRY. The vendor's login flow, run with the
/// directory as its home ([`HOME_ENV`]: `CLAUDE_CONFIG_DIR=<dir> claude auth
/// login`), writes its files there; one that holds nothing is a directory
/// `accounts add` made and no flow has run in. The program's start writes
/// files there before any subcommand (MEASURED on 2.1.287 with `auth status`
/// and `mcp list`; `auth login` starts the same way), so a flow that FAILS
/// can leave entries too: `accounts login` reads this before its flow and
/// after, and a failed flow that left entries in a directory EMPTY before it
/// is recorded `signed-out` with nothing more run under it, so the row does
/// not read `unseen`. Bounded: one `read_dir` and its first entry, never a
/// walk. A path that is absent, is no directory or cannot be read — and an
/// entry that cannot be read — is `false`: nothing is run under a login
/// nobody can show was made.
#[must_use]
pub fn login_made(dir: &Path) -> bool {
    std::fs::read_dir(dir)
        .is_ok_and(|mut entries| entries.next().is_some_and(|entry| entry.is_ok()))
}

/// Whether `row` is enrolled given the caller's set: the row with no `dir`
/// always is.
fn is_enrolled(row: &Row, enrolled: &BTreeSet<String>) -> bool {
    row.dir.is_none() || enrolled.contains(&row.label)
}

/// Whether the roster at `path` lists TWO OR MORE accounts: then one Claude
/// home serves several logins, and what that home keeps of ONE of them (its
/// model caches: `super::upgrade_drive::Opts::several_logins`) is not this
/// session's to be judged by. READS the file (one bounded read). No path, no
/// file and a roster that does not admit list none: `false`, and everything
/// is read as it was before accounts existed.
#[must_use]
pub fn several_listed(path: Option<&Path>) -> bool {
    path.and_then(|path| Roster::load(path).ok())
        .is_some_and(|roster| roster.several())
}

/// [`Roster::lone_login`] of the roster at `path` as this machine's logins
/// in `store` bind it at `now` ([`Store::bound`]): what a pass that types
/// relaunch lines hands the usage tap (`super::tap::Armed`). READS the
/// roster (one bounded read) and, where it lists one row, `logins.json`. No
/// path, no file, a roster that does not admit, or anything but one row:
/// `None`.
#[must_use]
pub fn lone_login_at(path: Option<&Path>, store: &Store, now: i64) -> Option<String> {
    let roster = Roster::load(path?).ok().filter(|r| r.rows.len() == 1)?;
    store
        .bound(&roster, now)
        .ok()?
        .lone_login()
        .map(str::to_owned)
}

/// The roster that belongs to the `aterm.toml` at `config`: [`ACCOUNTS_FILE`]
/// in the same directory. THE ONE RULE for where a roster is, which the CLI
/// (`--config` moves both files) and a host ([`default_roster_path`]) share.
/// `None` for a path with no directory to be beside.
#[must_use]
pub fn roster_beside(config: &Path) -> Option<PathBuf> {
    Some(config.parent()?.join(ACCOUNTS_FILE))
}

/// The roster a host reads: beside the `aterm.toml` the window loads
/// ([`aterm_types::dirs::aterm_config_path`], [`roster_beside`]). READS the
/// environment (the variables that rule consults) and no file. `None` when
/// no config path is known: then there is no roster.
#[must_use]
pub fn default_roster_path() -> Option<PathBuf> {
    roster_beside(&aterm_types::dirs::aterm_config_path()?)
}

/// WHAT A PROCESS'S OWN ENVIRONMENT SAYS OF ITS LOGIN (design §5, "What
/// counts as an observation"; §6 rule 1): which login it runs under, and
/// whether that is one this module's judgments are about at all. The three
/// facts [`Me`] carries, read in one place.
///
/// [`ProcessLogin::of_env`] is the ENVIRONMENT's half. The rest is the
/// host's to gather and hand over, and each builder only ever takes
/// `stored_login` away:
///
/// * THE SETTINGS FILES Claude Code reads for the process (the host's
///   gatherer, as `super::netwatch::RouteFacts::settings` carries them):
///   [`ProcessLogin::with_settings`] — a credential, a profile or a route
///   name in an `env` block, an `apiKeyHelper`, and a source that could not
///   be read.
/// * ITS ROUTE, where a host already keeps one
///   ([`super::netwatch::route_of`]): [`ProcessLogin::with_route_default`].
/// * THE VENDOR'S PROFILE STORE ([`profile_configs_dir`],
///   [`profile_configured`]): [`ProcessLogin::with_profile_store`].
///
/// NOT TRACED, and so read as it may be: which of these an interactive
/// session acts on, and in what order beside its stored login. A process
/// given another subscription's token by any of them would, read as on its
/// stored login, have its wall recorded on that account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessLogin {
    /// The value of [`LOGIN_ENV`] where it is set and not empty, VERBATIM
    /// (a space and a trailing slash survive: the vendor hashes the string
    /// as given). `None` — unset, or set and empty — is the default login.
    pub login: Option<String>,
    /// Its Claude home is the default one: [`HOME_ENV`] is unset or empty.
    pub default_home: bool,
    /// It authenticates by a stored login: nothing in its environment gives
    /// it a credential, a provider, a profile or a route of its own
    /// ([`CREDENTIAL_KEYS`], [`PROFILE_KEYS`],
    /// [`super::netwatch::ROUTE_KEYS`]), its login can be told (see
    /// [`ProcessLogin::of_env`]), and nothing the host handed over says
    /// otherwise.
    pub stored_login: bool,
}

/// Whether the variable `name`, set, gives a process a way to authenticate
/// that is not its stored login: a credential or a provider
/// ([`CREDENTIAL_KEYS`]), the vendor's profile source ([`PROFILE_KEYS`]), or
/// a route of its own ([`super::netwatch::ROUTE_KEYS`],
/// [`super::netwatch::ROUTE_PREFIXES`]). ONE list for a process's
/// environment and for a settings file's `env` block.
fn names_another_login(name: &str) -> bool {
    use super::netwatch::{ROUTE_KEYS, ROUTE_PREFIXES};
    CREDENTIAL_KEYS.contains(&name)
        || PROFILE_KEYS.contains(&name)
        || ROUTE_KEYS.contains(&name)
        || ROUTE_PREFIXES.iter().any(|p| name.starts_with(p))
}

/// WHERE THE VENDOR'S `profile` SOURCE KEEPS ITS CONFIGS, for a process
/// with this environment: `<store>/configs`, the store being
/// `ANTHROPIC_CONFIG_DIR`, else `$XDG_CONFIG_HOME/anthropic`, else
/// `$HOME/.config/anthropic` — the vendor's own order, a value of spaces
/// being unset as it is to the vendor (READ in the 2.1.286 and 2.1.287
/// bundles: the profile in use is `configs/<name>.json` there, `<name>` from
/// `ANTHROPIC_PROFILE`, else the file `active_config`, else `default`).
/// PURE: it reads nothing. `None` where none of the three is set: the
/// vendor has no store then.
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

/// Whether the vendor's profile store holds a config: `configs` (a
/// [`profile_configs_dir`]) lists a `.json` entry. READS the filesystem (one
/// directory listing, names only: no config and no credential is opened).
/// No such directory is `false`. One that cannot be listed is `true`: a
/// store this machine cannot see may hold a profile, and the session is
/// left alone ([`ProcessLogin::with_profile_store`]).
///
/// ANY `.json` there counts. The vendor uses one — the active profile's,
/// and only where its `authentication.type` is one of two words — and
/// reading which would mean opening the file; a store with a config in it
/// is a machine set up for that source, which is enough to stand aside.
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

impl ProcessLogin {
    /// Read one process's environment, as `(name, value)` pairs. PURE.
    ///
    /// * `login`: the value of [`LOGIN_ENV`] when set and not empty.
    /// * `default_home`: no [`HOME_ENV`] with a value.
    /// * `stored_login`: no [`CREDENTIAL_KEYS`] name, no [`PROFILE_KEYS`]
    ///   name and no route name ([`super::netwatch::ROUTE_KEYS`],
    ///   [`super::netwatch::ROUTE_PREFIXES`]) with a value.
    ///
    /// SET MEANS NOT EMPTY, and nothing else is asked of a value: one of
    /// spaces is set, and so is `CLAUDE_CODE_USE_BEDROCK=0`. The vendor reads
    /// some of these as a boolean and some as a string; reading every one as
    /// set errs to a session that is left alone.
    ///
    /// A NAME GIVEN TWICE with two values (an environment no shell makes):
    /// for the home and for a credential, either one counts. For
    /// [`LOGIN_ENV`] the login cannot be told — which of the two the vendor's
    /// runtime reads was not measured — so `login` is the first and
    /// `stored_login` is false: nothing is recorded for, or advised to, a
    /// process whose account is a guess.
    #[must_use]
    pub fn of_env<'a>(env: impl IntoIterator<Item = (&'a str, &'a str)>) -> ProcessLogin {
        let mut login: Option<&str> = None;
        let mut out = ProcessLogin {
            login: None,
            default_home: true,
            stored_login: true,
        };
        for (name, value) in env {
            if name == LOGIN_ENV {
                match login {
                    None => login = Some(value),
                    Some(first) if first != value => out.stored_login = false,
                    Some(_) => {}
                }
            }
            if value.is_empty() {
                continue;
            }
            if name == HOME_ENV {
                out.default_home = false;
            }
            if names_another_login(name) {
                out.stored_login = false;
            }
        }
        out.login = login.filter(|dir| !dir.is_empty()).map(str::to_owned);
        out
    }

    /// [`ProcessLogin::of_env`] over a process's exec environment as the
    /// kernel hands it back: `KEY=value` entries, each cut at its FIRST `=`.
    /// AN ENVIRONMENT THAT READS EMPTY WAS NOT READ, and is `None`: the
    /// kernel hands none back for a process it does not show, and read as
    /// "no variable set" that would be the default login. PURE.
    #[must_use]
    pub fn of_exec_env(env: &[String]) -> Option<ProcessLogin> {
        (!env.is_empty())
            .then(|| ProcessLogin::of_env(env.iter().filter_map(|kv| kv.split_once('='))))
    }

    /// With the host's half: whether the route the process's SETTINGS give
    /// it is the default one ([`super::netwatch::route_of`]). `false` makes
    /// it no stored login, whatever its environment said; `true` leaves what
    /// the environment said, and never makes a stored login of a process
    /// with a credential in its environment.
    #[must_use]
    pub fn with_route_default(mut self, default: bool) -> ProcessLogin {
        self.stored_login &= default;
        self
    }

    /// With THE SETTINGS FILES Claude Code reads for the process, as the
    /// host's gatherer read them (`super::netwatch::RouteFacts::settings`:
    /// the chain Claude Code reads, `claude_settings::sources_under`, and its
    /// managed preference domains), each through the ONE settings reader
    /// ([`super::netwatch::settings_of`]).
    /// PURE. It is no stored login where any source:
    ///
    /// * sets, in its `env` block, a name the environment is read for
    ///   ([`CREDENTIAL_KEYS`], [`PROFILE_KEYS`], a route name) — SET MEANS
    ///   NOT EMPTY, as in [`ProcessLogin::of_env`];
    /// * sets [`LOGIN_ENV`] there (to anything, the empty value included:
    ///   set and empty names the default login) or [`HOME_ENV`] to a value:
    ///   whether the vendor's keychain name reads a variable a settings
    ///   block sets was NOT traced, so the login the process's own
    ///   environment names is then a guess;
    /// * names an `apiKeyHelper` (READ in the 2.1.286 and 2.1.287 bundles:
    ///   the token chain asks it ahead of the profile source and the stored
    ///   login);
    /// * could not be read, or is not a settings file this reader takes: a
    ///   source nobody can see may hold any of the above.
    ///
    /// A source that is not there sets nothing. Nothing here makes a stored
    /// login of a process whose environment said otherwise.
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
                                && (name == HOME_ENV || names_another_login(name)))
                    })
            }),
        };
        self.stored_login &= !settings.iter().any(|(_, source)| another(source));
        self
    }

    /// With THE VENDOR'S PROFILE STORE: whether it holds a config
    /// ([`profile_configured`] of [`profile_configs_dir`], the host's one
    /// directory listing). `true` makes it no stored login: the vendor asks
    /// that source ahead of the stored one. `false` leaves what was read.
    #[must_use]
    pub fn with_profile_store(mut self, configured: bool) -> ProcessLogin {
        self.stored_login &= !configured;
        self
    }

    /// The session as [`advise`] asks about it: the roster row its login
    /// names ([`Roster::row_for_login`]: byte for byte, the row with no
    /// `dir` for the default login), and the two facts above.
    ///
    /// Through a [`bound`] roster, a process still on a login a renewal
    /// moved its row away from is no row's own ([`Me::account`] `None`) and
    /// that row's account all the same ([`Me::left`], [`Roster::row_left`]).
    #[must_use]
    pub fn me<'a>(&self, roster: &'a Roster) -> Me<'a> {
        Me {
            account: roster
                .row_for_login(self.login.as_deref())
                .map(|row| row.label.as_str()),
            left: roster
                .row_left(self.login.as_deref())
                .map(|row| row.label.as_str()),
            default_home: self.default_home,
            stored_login: self.stored_login,
        }
    }
}

/// THE ACCOUNT THE ROSTER'S JUDGMENTS ARE ABOUT, for the process `who`: the
/// label of the row its login names ([`Roster::row_for_login`]: byte for
/// byte, the row with no `dir` for the default login), where it runs on a
/// STORED login in the DEFAULT home — the recorder's rule ([`record_wall`])
/// and [`advise`]'s first. `None` for a login no row names, a home of its
/// own, and a credential or a route of its own: such a process's environment
/// may name no login, which reads as the row with no `dir`, and it is named
/// as no account rather than guessed at. PURE.
///
/// It judges what `who` carries. Read from the environment alone
/// ([`ProcessLogin::of_env`]) that is the environment's half: a process a
/// settings file or the vendor's profile store gives a credential is still
/// named, until the host hands its half over ([`ProcessLogin`]). And THE
/// DEFAULT HOME IS JUDGED BY [`HOME_ENV`] ALONE, as the recorder judges it: a
/// process started under another `HOME` is still named by the row with no
/// `dir`. Whether the vendor's stored login follows `HOME` was not traced.
///
/// A process on a login a renewal moved its row away from ([`Me::left`]) is
/// judged as that row's account: it is the same account, the login it holds
/// not renewed yet.
#[must_use]
pub fn judged_label<'a>(roster: &'a Roster, who: &ProcessLogin) -> Option<&'a str> {
    let me = who.me(roster);
    me.label().filter(|_| me.default_home && me.stored_login)
}

/// THE LABEL A SURFACE SHOWS FOR A PROCESS'S ACCOUNT — the window's footer
/// (`super::footer::FooterFacts::account`) and `aterm harness usage`'s `acct`:
/// [`judged_label`], where the roster lists TWO OR MORE ([`Roster::several`]).
/// With one account or none there is nothing to tell apart, and no surface
/// names one. PURE.
#[must_use]
pub fn footer_label<'a>(roster: &'a Roster, who: &ProcessLogin) -> Option<&'a str> {
    judged_label(roster, who).filter(|_| roster.several())
}

// ---------------------------------------------------------------------------
// 1b. Logins (design v2 §4.5, §5): where each row's login is bound on this
// machine, and what its renewal knows
// ---------------------------------------------------------------------------

/// The logins' file name, in the store beside `state.json`. Its own file:
/// an older build saves `state.json` with the keys it knows ([`State::to_json`]),
/// so a binding kept there would be lost at that build's first write.
pub const LOGINS_FILE: &str = "logins.json";

/// [`LOGINS_FILE`]'s schema. A file of another one is refused.
pub const LOGINS_SCHEMA: u64 = 1;

/// The most bytes of [`LOGINS_FILE`] that are read.
pub const MAX_LOGINS_BYTES: u64 = 256 * 1024;

/// One day, in seconds.
const DAY_S: i64 = 86_400;

/// A login's lifetime before anything was learned of it: three weeks. The
/// vendor's client default is 30 days (`nP=2592000000`, READ in the 2.1.300
/// bundle) and field reports say one to four weeks; the server's figure is
/// not on any record here until an end or a notice teaches it.
pub const LIFETIME_START_S: i64 = 21 * DAY_S;

/// The least a learned lifetime is taken as: a week. An end sooner than this
/// after a renewal is no lifetime (a revocation, a sign-out elsewhere) and
/// teaches nothing ([`note_login_end`]); a notice that implies less is read
/// as this.
pub const LIFETIME_MIN_S: i64 = 7 * DAY_S;

/// The most a learned lifetime is taken as: the client's own 30 days.
pub const LIFETIME_MAX_S: i64 = 30 * DAY_S;

/// A login whose forecast end is this near is DUE ([`LoginState::Due`]):
/// five days, so one sitting a few days apart renews every due login before
/// it ends.
pub const DUE_S: i64 = 5 * DAY_S;

/// A login with a served turn on record this recently is `ok`
/// ([`LoginState::Ok`]).
pub const OK_FRESH_S: i64 = 6 * 3600;

/// The most days the vendor's start-up notice `Your login expires in N days`
/// names (READ in the 2.1.300 bundle: it is shown only while the refresh
/// token's end is 3 days away or nearer, `Math.ceil` of the days): a reading
/// of more is no such notice.
pub const NOTICE_DAYS_MAX: u32 = 3;

/// The most superseded logins a binding remembers ([`Binding::was`]).
pub const MAX_LEFT: usize = 4;

/// Within this long of now, a machine whose every renewable login is gone or
/// forecast to end has no working login soon ([`none_working_within`]).
pub const NONE_SOON_S: i64 = DAY_S;

/// The longest subscription word kept.
const SUBSCRIPTION_MAX: usize = 32;

/// WHERE ONE ROW'S LOGIN IS BOUND ON THIS MACHINE, and what its renewals
/// learned ([`Logins`]). MACHINE-LOCAL: a login is a keychain item of this
/// machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding {
    /// The login directory a renewal bound the row to, VERBATIM: the
    /// keychain item is named by a hash of this exact string, so it is
    /// recorded and never rebuilt. `None`: no renewal has bound one, and the
    /// row's own `dir` (or the default login) stands.
    pub dir: Option<String>,
    /// When the bound login was renewed (its staging check answered).
    pub renewed_at: Option<i64>,
    /// The lifetime a renewal is forecast by, as learned ([`Binding::lifetime`]
    /// reads it clamped to [`LIFETIME_MIN_S`]..=[`LIFETIME_MAX_S`]).
    pub lifetime_s: i64,
    /// The latest end the vendor's notice allows, for the bound login
    /// ([`note_login_notice`]).
    pub notice_end: Option<i64>,
    /// When the bound login was first seen ended ([`note_login_end`]).
    pub ended_at: Option<i64>,
    /// When a turn was last served under the bound login ([`note_served_login`]).
    pub served_at: Option<i64>,
    /// The `subscriptionType` its renewal's check answered (`max`, `pro`).
    pub subscription: Option<String>,
    /// When a renewal last answered ANOTHER account than the row's
    /// ([`note_wrong_account`]): nothing live changed then. Cleared by the
    /// next renewal that matches.
    pub wrong_at: Option<i64>,
    /// The logins this binding replaced, newest first, at most
    /// [`MAX_LEFT`]; `None` is the default login. A session still on one is
    /// moved onto [`Binding::dir`] at its next idle turn end ([`Me::left`]).
    pub was: Vec<Option<String>>,
}

impl Default for Binding {
    fn default() -> Binding {
        Binding {
            dir: None,
            renewed_at: None,
            lifetime_s: LIFETIME_START_S,
            notice_end: None,
            ended_at: None,
            served_at: None,
            subscription: None,
            wrong_at: None,
            was: Vec::new(),
        }
    }
}

impl Binding {
    /// The lifetime the forecast uses: as learned, within its bounds.
    #[must_use]
    pub fn lifetime(&self) -> i64 {
        self.lifetime_s.clamp(LIFETIME_MIN_S, LIFETIME_MAX_S)
    }

    /// Learn that the bound login ends no later than `end`: the lifetime is
    /// LOWERED to what that end implies (never raised), within its bounds.
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

/// `logins.json`: every row's [`Binding`], by label.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Logins {
    /// The bindings, by label.
    pub labels: BTreeMap<String, Binding>,
}

impl Logins {
    /// The binding of `label`, where one is kept.
    #[must_use]
    pub fn get(&self, label: &str) -> Option<&Binding> {
        self.labels.get(label)
    }

    /// The binding of `label`, made if absent; `None` for a label no roster
    /// could carry, or a 33rd.
    fn entry(&mut self, label: &str) -> Option<&mut Binding> {
        if !admissible_label(label)
            || (!self.labels.contains_key(label) && self.labels.len() >= MAX_ACCOUNTS)
        {
            return None;
        }
        Some(self.labels.entry(label.to_owned()).or_default())
    }

    /// The file as one line of JSON, keys sorted; an unknown is `null`.
    #[must_use]
    pub fn to_json(&self) -> String {
        let mut o = Map::new();
        o.insert("schema".to_owned(), Value::from(LOGINS_SCHEMA));
        let labels = self
            .labels
            .iter()
            .map(|(label, b)| {
                let mut m = Map::new();
                m.insert("dir".to_owned(), Value::from(b.dir.clone()));
                m.insert("renewed_at".to_owned(), Value::from(b.renewed_at));
                m.insert("lifetime_s".to_owned(), Value::from(b.lifetime_s));
                m.insert("notice_end".to_owned(), Value::from(b.notice_end));
                m.insert("ended_at".to_owned(), Value::from(b.ended_at));
                m.insert("served_at".to_owned(), Value::from(b.served_at));
                m.insert(
                    "subscription".to_owned(),
                    Value::from(b.subscription.clone()),
                );
                m.insert("wrong_at".to_owned(), Value::from(b.wrong_at));
                m.insert(
                    "was".to_owned(),
                    Value::Array(b.was.iter().map(|w| Value::from(w.clone())).collect()),
                );
                (label.clone(), Value::Object(m))
            })
            .collect();
        o.insert("labels".to_owned(), Value::Object(labels));
        aterm_json::to_string(&Value::Object(o)).unwrap_or_else(|_| String::from("{}"))
    }

    /// Read one `logins.json`: at most [`MAX_ACCOUNTS`] admissible labels,
    /// [`MAX_LEFT`] superseded logins each, every stamp this machine's own
    /// and so no later than `now`. Unknown keys are ignored. Invalid labels,
    /// binding objects, current directories or superseded directories refuse
    /// the file whole: an unknown identity cannot stand for the roster's old
    /// login. Observation metadata retains [`State::from_json`]'s bounds.
    ///
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
        match doc.get("schema").and_then(Value::as_u64) {
            Some(LOGINS_SCHEMA) => {}
            Some(other) => {
                return Err(Unread::NotOurs(format!(
                    "schema {other}; this build reads schema {LOGINS_SCHEMA}"
                )));
            }
            None => return Err(Unread::NotOurs("names no schema".to_string())),
        }
        let entries = doc
            .get("labels")
            .and_then(Value::as_object)
            .ok_or_else(|| Unread::NotOurs("labels is not an object".to_string()))?;
        if entries.len() > MAX_ACCOUNTS {
            return Err(Unread::NotOurs("too many binding labels".to_string()));
        }
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
            let binding = Binding {
                dir,
                renewed_at: stamp(v.get("renewed_at"), now),
                lifetime_s: int(v.get("lifetime_s"))
                    .unwrap_or(LIFETIME_START_S)
                    .clamp(LIFETIME_MIN_S, LIFETIME_MAX_S),
                // An end is the future's, not a stamp: as written.
                notice_end: int(v.get("notice_end")),
                ended_at: stamp(v.get("ended_at"), now),
                served_at: stamp(v.get("served_at"), now),
                subscription: word(v.get("subscription"), SUBSCRIPTION_MAX),
                wrong_at: stamp(v.get("wrong_at"), now),
                was,
            };
            labels.insert(label.clone(), binding);
        }
        Ok(Logins { labels })
    }
}

/// THE ROSTER AS THIS MACHINE'S LOGINS HAVE IT: each row with a binding a
/// renewal made reads that login as its `dir` ([`Binding::dir`]), and names
/// the logins it replaced ([`Row::was`]). Everything that relaunches,
/// records or advises reads the roster through this, so the line a move is
/// made with ([`login_env`]) carries the renewed login and the old one is
/// never rebuilt. PURE: nothing of the file changes.
///
/// THE DEFAULT LOGIN STAYS THIS MACHINE'S OWN ROW'S (the row the file lists
/// with no `dir`) however many renewals ago it was replaced: every `claude`
/// started by hand runs on it, so a session there is that row's account on
/// a superseded login, followed onto the renewed one — never a login no row
/// names, at whose wall `/login` would be typed into the default browser
/// once [`MAX_LEFT`] renewals had pushed it out of [`Binding::was`].
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
            _ => row.clone(),
        })
        .collect();
    Roster { rows }
}

/// Where a renewable login stands (design v2 §5.2): derived at every read
/// from what is on record, never by running anything under it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginState {
    /// A login wall was seen on it ([`Auth::SignedOut`] on record): only a
    /// renewal restores it.
    Expired,
    /// Its forecast end ([`forecast`]) is within [`DUE_S`].
    Due {
        /// The forecast end.
        by: i64,
    },
    /// A turn was served under it within [`OK_FRESH_S`].
    Ok,
    /// None of the above: not a fault — the next turn under it is its check.
    Idle,
}

impl LoginState {
    /// The report's word.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            LoginState::Expired => "expired",
            LoginState::Due { .. } => "due",
            LoginState::Ok => "ok",
            LoginState::Idle => "idle",
        }
    }
}

/// THE FORECAST END of a binding's login: its renewal plus the lifetime
/// learned ([`Binding::lifetime`]), or the end the vendor's notice allows,
/// whichever is sooner. `None` where neither is known: a login no renewal
/// on this machine made and no notice has spoken of. A notice that is
/// ABSENT lengthens nothing.
#[must_use]
pub fn forecast(binding: &Binding) -> Option<i64> {
    let by_life = binding
        .renewed_at
        .map(|at| at.saturating_add(binding.lifetime()));
    [by_life, binding.notice_end].into_iter().flatten().min()
}

/// [`LoginState`] of `row`'s login at `now`, from the state's auth word and
/// the row's binding.
#[must_use]
pub fn login_state(row: &Row, logins: &Logins, state: &State, now: i64) -> LoginState {
    if state
        .accounts
        .get(&row.label)
        .is_some_and(|a| a.auth == Auth::SignedOut)
    {
        return LoginState::Expired;
    }
    let binding = logins.get(&row.label);
    if let Some(by) = binding.and_then(forecast)
        && by.saturating_sub(now) <= DUE_S
    {
        return LoginState::Due { by };
    }
    if binding
        .and_then(|b| b.served_at)
        .is_some_and(|at| recent(at, now, OK_FRESH_S))
    {
        LoginState::Ok
    } else {
        LoginState::Idle
    }
}

/// THE LOGINS `renew --due` RENEWS, in roster order: every row with an email
/// whose login is [`LoginState::Expired`] or [`LoginState::Due`]. A row
/// whose login was never made here (`enrolled`) is not due: `renew <label>`
/// signs it in by name.
#[must_use]
pub fn renew_due(
    roster: &Roster,
    logins: &Logins,
    state: &State,
    enrolled: &BTreeSet<String>,
    now: i64,
) -> Vec<String> {
    roster
        .rows
        .iter()
        .filter(|row| row.email.is_some() && is_enrolled(row, enrolled))
        .filter(|row| {
            matches!(
                login_state(row, logins, state, now),
                LoginState::Expired | LoginState::Due { .. }
            )
        })
        .map(|row| row.label.clone())
        .collect()
}

/// Whether NO RENEWABLE LOGIN IS LEFT WORKING within `within` of `now`: the
/// roster has a row with an email enrolled here, and every such login is
/// expired or forecast to end by then. `false` with no such row.
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
        .filter(|row| row.email.is_some() && is_enrolled(row, enrolled))
        .peekable();
    rows.peek().is_some()
        && rows.all(|row| match login_state(row, logins, state, now) {
            LoginState::Expired => true,
            _ => logins
                .get(&row.label)
                .and_then(forecast)
                .is_some_and(|by| by.saturating_sub(now) <= within),
        })
}

/// The labels whose last renewal answered another account
/// ([`Binding::wrong_at`]), in roster order.
#[must_use]
pub fn wrong_accounts(roster: &Roster, logins: &Logins) -> Vec<String> {
    roster
        .rows
        .iter()
        .filter(|row| logins.get(&row.label).is_some_and(|b| b.wrong_at.is_some()))
        .map(|row| row.label.clone())
        .collect()
}

/// A LOGIN WALL ON `label`'s BOUND LOGIN, seen at `at` in a session running
/// on that very login (the caller checks the process's login string is the
/// binding's): the login is recorded signed out ([`note_auth`]) and, the
/// first time this binding is seen ended, its end is kept and a
/// `login-ended` row written — an end a week or more after the renewal
/// lowers the learned lifetime ([`Binding::lower_lifetime`]); one sooner is
/// a revocation, not a lifetime, and teaches nothing. Asked again at the
/// same wall it writes nothing more.
pub fn note_login_end(state: &mut State, logins: &mut Logins, label: &str, at: i64) -> Vec<Event> {
    // Recorded once: asked again at the same wall, nothing is rewritten.
    let mut out = if state.account(label).auth == Auth::SignedOut {
        Vec::new()
    } else {
        note_auth(state, label, Auth::SignedOut, at)
    };
    let Some(binding) = logins.entry(label) else {
        return out;
    };
    let first = binding
        .ended_at
        .is_none_or(|ended| binding.renewed_at.is_some_and(|renewed| ended < renewed));
    if first {
        binding.ended_at = Some(at);
        if binding
            .renewed_at
            .is_some_and(|renewed| at.saturating_sub(renewed) >= LIFETIME_MIN_S)
        {
            binding.lower_lifetime(at);
        }
        out.push(Event::new(at, Kind::LoginEnded, label));
    }
    out
}

/// THE VENDOR'S NOTICE `Your login expires in N days`, read at `at` on a
/// session running on `label`'s bound login: the login ends no later than
/// `at + days` ([`Binding::notice_end`], only ever moved sooner), which
/// makes it due at once and lowers the learned lifetime. A `days` of 0 or
/// more than [`NOTICE_DAYS_MAX`] is no such notice. Whether anything
/// changed.
pub fn note_login_notice(logins: &mut Logins, label: &str, days: u32, at: i64) -> bool {
    if days == 0 || days > NOTICE_DAYS_MAX {
        return false;
    }
    let Some(binding) = logins.entry(label) else {
        return false;
    };
    let end = at.saturating_add(i64::from(days).saturating_mul(DAY_S));
    let before = binding.clone();
    binding.notice_end = Some(binding.notice_end.map_or(end, |held| held.min(end)));
    binding.lower_lifetime(end);
    *binding != before
}

/// A TURN SERVED under `label`'s bound login at `at`: what makes it `ok`
/// ([`LoginState::Ok`]). Whether anything changed.
pub fn note_served_login(logins: &mut Logins, label: &str, at: i64) -> bool {
    let Some(binding) = logins.entry(label) else {
        return false;
    };
    let served = binding.served_at.map_or(at, |held| held.max(at));
    let changed = binding.served_at != Some(served);
    binding.served_at = Some(served);
    changed
}

/// A RENEWAL WHOSE CHECK ANSWERED THE ROW'S ACCOUNT: `label` is bound to
/// `dir`, the staging directory its flow wrote, VERBATIM. The login it
/// replaces (the binding's, else the row's own `own_dir`, `None` the default
/// login) goes first in [`Binding::was`]; the forecast starts again from
/// `at`, what the old login's notice and end said is dropped with it, and
/// the login is recorded `ok` ([`note_auth`]: the wake marker follows, so a
/// session waiting on the old login decides again). The rows: `auth` where
/// the word changed, and `renewed`.
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
    binding.subscription = subscription
        .map(|s| one_line(s, SUBSCRIPTION_MAX))
        .filter(|s| !s.is_empty());
    out.extend(note_auth(state, label, Auth::Ok, at));
    out.push(Event::new(at, Kind::Renewed, label));
    out
}

/// A RENEWAL WHOSE CHECK ANSWERED ANOTHER ACCOUNT: nothing live changes;
/// the attempt is kept ([`Binding::wrong_at`]) and its row written, labels
/// only.
pub fn note_wrong_account(logins: &mut Logins, label: &str, at: i64) -> Vec<Event> {
    match logins.entry(label) {
        Some(binding) => {
            binding.wrong_at = Some(at);
            vec![Event::new(at, Kind::WrongAccount, label)]
        }
        None => Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// 2. The state (design §5), schema 1
// ---------------------------------------------------------------------------

/// A usage limit one of this account's sessions hit, by the vendor's own
/// limit type. An OBSERVATION: account-wide, merged by newest `at`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Wall {
    /// The reset the wall named (Unix seconds), or `None` when it named none.
    pub until: Option<i64>,
    /// When it was observed.
    pub at: i64,
    /// The machine that observed it.
    pub by: String,
    /// How many times it was found standing with no reset ahead of it: `0`
    /// for a wall observed before its own reset.
    pub attempts: u32,
}

/// The rung of `ladder` for the `attempts`th observation, the last one for
/// ever after.
fn rung(ladder: &[i64], attempts: u32) -> i64 {
    let i = usize::try_from(attempts.saturating_sub(1)).unwrap_or(usize::MAX);
    ladder
        .get(i)
        .or_else(|| ladder.last())
        .copied()
        .unwrap_or(0)
}

impl Wall {
    /// The instant this wall stops standing (design §5's table): its reset
    /// plus [`RESET_GRACE_S`]; observed after its own reset, the observation
    /// plus [`LATE_WALL_S`]; naming no reset, the observation plus
    /// [`NO_RESET_WALL_S`].
    #[must_use]
    pub fn stands_until(&self) -> i64 {
        match self.until {
            Some(until) if self.at <= until => until.saturating_add(RESET_GRACE_S),
            Some(_) => self.at.saturating_add(rung(&LATE_WALL_S, self.attempts)),
            None => self
                .at
                .saturating_add(rung(&NO_RESET_WALL_S, self.attempts)),
        }
    }
}

/// One settled `/usage` panel. An OBSERVATION. A figure nobody read is
/// `None`, never `0`.
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
    /// When it was read.
    pub at: i64,
    /// The machine that read it ([`note_sample`] stamps it).
    pub by: String,
}

/// A window's figure and the instant it stops counting, while that window
/// still runs. A window whose reset has passed VOIDS its figure; a figure
/// whose reset was not read is void one window length after its sample.
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
    /// The session figure and its window's end, while the window runs.
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

    /// The week's figure and its end, while the week runs.
    #[must_use]
    pub fn week(&self, now: i64) -> Option<(u32, i64)> {
        figure(self.week_pct, self.week_reset, self.at, WEEK_WINDOW_S, now)
    }

    /// Until when this sample shows the account `low`: the end of the last
    /// window still running at its threshold — the instant the account is
    /// fresh again as far as this sample knows. `None` when no running
    /// window is at its threshold.
    #[must_use]
    pub fn low_until(&self, now: i64) -> Option<i64> {
        let at = |figure: Option<(u32, i64)>, threshold: u32| {
            figure
                .filter(|(pct, _)| *pct >= threshold)
                .map(|(_, end)| end)
        };
        at(self.session(now), LOW_SESSION_PCT).max(at(self.week(now), LOW_WEEK_PCT))
    }

    /// Whether this sample shows the account `low`: a window still running
    /// at its threshold, HOWEVER NEAR its reset. An account at 100 % of a
    /// window ten minutes from its reset has nothing left until then, so it
    /// is never a fresh candidate.
    #[must_use]
    pub fn low(&self, now: i64) -> bool {
        self.low_until(now).is_some()
    }

    /// Whether a session on this account is worth moving off it ahead of
    /// its wall: it is `low` by a window that does not reset within
    /// [`RESET_SOON_S`]. A window about to reset is about to be replaced, so
    /// leaving it buys nothing.
    #[must_use]
    pub fn leave_early(&self, now: i64) -> bool {
        self.low_until(now)
            .is_some_and(|end| end.saturating_sub(now) > RESET_SOON_S)
    }
}

/// A limit-reset offer last seen at a wall. An OBSERVATION.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Offer {
    /// Resets left.
    pub left: u32,
    /// The grant's deadline, where one was read.
    pub use_by: Option<i64>,
    /// When it was seen.
    pub at: i64,
    /// The machine that saw it.
    pub by: String,
}

/// THE NEWEST WORD ON AN ACCOUNT'S USAGE CREDITS (accounts v2 §3.3): an
/// OBSERVATION, kept by one rule ([`record_usage`]). Each word is stamped
/// with when it was TRUE — a limit row's own time ([`record_wall`]); for
/// Claude Code's notice on a screen, the newest answer of that session's
/// transcript, which the notice was drawn after ([`UsageSaid::at`]) — and
/// never stamped again when it is read again. THE NEWEST WORD WINS: an older
/// one changes nothing, so a row read again, or a notice left standing on an
/// idle tab and said again, is a no-op; at the same second a limit row
/// outranks the screen (it is the answer that said so). Credits are work
/// going on, never a wall: this is a record, the day's
/// line of the digest ([`credits_noted`]) and what makes a nearly used week
/// one ([`windows_full`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Credits {
    /// Seen carrying the account's sessions past a limit.
    InUse(CreditsUse),
    /// Off, as a limit row said (`quotaLimits.overageStatus: rejected`).
    Off(CreditsOff),
    /// On, as a limit row said (`allowed`, `allowed_warning`), at this
    /// instant: no session was seen on them since.
    On(i64),
}

impl Credits {
    /// When the word was true: the newest answer seen on credits, or the row.
    #[must_use]
    pub fn at(&self) -> i64 {
        match self {
            Credits::InUse(c) => c.seen,
            Credits::Off(off) => off.at,
            Credits::On(at) => *at,
        }
    }
}

/// AN EPISODE OF USAGE CREDITS carrying an account's sessions ([`Credits`]):
/// the answers seen served on them, the first and the newest, with no lapse
/// between ([`Self::stands`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreditsUse {
    /// The first answer seen served on credits, this episode.
    pub since: i64,
    /// The newest.
    pub seen: i64,
    /// The reset of the window credits carried past, where the notice they
    /// started on named it: they are taken as in use until it at least.
    pub until: Option<i64>,
    /// This episode raised the day's line ([`credits_noted`]): the first
    /// episode of the local day did, a later one the same day does not.
    pub told: bool,
}

impl CreditsUse {
    /// Whether credits are taken as still in use at `now`: an answer was
    /// served on them within [`CREDITS_STALE_S`], or the window they carried
    /// past has not reset — its named reset, else a session window after they
    /// began. Judged at an answer, whether that answer is this episode's.
    #[must_use]
    pub fn stands(&self, now: i64) -> bool {
        let reset = self
            .until
            .unwrap_or_else(|| self.since.saturating_add(SESSION_WINDOW_S));
        now < self.seen.saturating_add(CREDITS_STALE_S) || now < reset
    }
}

/// THE ACCOUNT'S USAGE CREDITS ARE OFF, as a limit row of its sessions said
/// ([`Credits::Off`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreditsOff {
    /// The vendor's word for why (`overageDisabledReason`: `out_of_credits`,
    /// `org_level_disabled`, …), or empty where the row named none.
    pub reason: String,
    /// When the row was written.
    pub at: i64,
    /// When they come back (`overageResetsAt`), where the row named it.
    pub until: Option<i64>,
}

/// THE NEWEST WORD ON ONE OF AN ACCOUNT'S WINDOWS apart from a `/usage`
/// sample: Claude Code's warning that it is filling (`You've used 80% of your
/// weekly limit · resets …`), or the limit reset that refilled it
/// ([`reset_outcome`]). An OBSERVATION, kept by [`Credits`]' rule: stamped
/// with when it was true (the answer the warning was drawn after, the reset),
/// the newest wins and an older one changes nothing ([`record_usage`]); each
/// window has its own, and ends at its own reset ([`Self::figure`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowSaid {
    /// Whole percent used; `None`: refilled at [`Self::at`], and no figure
    /// said since.
    pub pct: Option<u32>,
    /// When the window resets, where the warning named it.
    pub reset: Option<i64>,
    /// When it was true.
    pub at: i64,
}

impl WindowSaid {
    /// The figure and the instant it stops counting, while its window runs
    /// (a reset not read: one `window_s` after it was said).
    #[must_use]
    pub fn figure(&self, window_s: i64, now: i64) -> Option<(u32, i64)> {
        figure(self.pct, self.reset, self.at, window_s, now)
    }
}

/// What this machine last learned of a login. MACHINE-LOCAL: a login is a
/// keychain item of this machine and says nothing about another's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Auth {
    /// Nothing is on record.
    #[default]
    Unseen,
    /// The check after a sign-in, or a served response, said the login
    /// works.
    Ok,
    /// A login wall right after a session was moved onto it (before the
    /// move served a turn: [`strike`]), a first login whose flow failed, or
    /// the check after a sign-in said it is signed out. A login wall in a
    /// session not just moved records nothing yet.
    SignedOut,
}

impl Auth {
    /// The state file's and the report's word.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Auth::Unseen => "unseen",
            Auth::Ok => "ok",
            Auth::SignedOut => "signed-out",
        }
    }

    /// One word back; anything outside the list is [`Auth::Unseen`], never a
    /// stronger answer than the word deserved.
    #[must_use]
    pub fn parse(word: &str) -> Auth {
        match word {
            "ok" => Auth::Ok,
            "signed-out" => Auth::SignedOut,
            _ => Auth::Unseen,
        }
    }
}

/// Everything kept about one account.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Account {
    /// OBSERVATION: the walls, by the vendor's limit type.
    pub walls: BTreeMap<String, Wall>,
    /// OBSERVATION: the newest sample.
    pub sample: Option<Sample>,
    /// OBSERVATION: the sample before it (for the burn rate).
    pub before: Option<Sample>,
    /// OBSERVATION: the reset offer last seen.
    pub offer: Option<Offer>,
    /// MACHINE-LOCAL: the login's last known word.
    pub auth: Auth,
    /// MACHINE-LOCAL: when that word was learned.
    pub auth_at: Option<i64>,
    /// MACHINE-LOCAL: when the owner's pin last cleared its walls ([`pin`]).
    /// A wall another machine observed at or before this instant is the one
    /// the owner cleared, and a merge does not bring it back; nor does this
    /// machine's own look at a row written before it
    /// ([`Account::cleared_since`]). A reset that ended this account's walls
    /// stamps it too ([`reset_outcome`]).
    pub cleared_at: Option<i64>,
    /// MACHINE-LOCAL: when a session was last read running on it
    /// ([`note_session`]). It outlives the session's own row, which goes
    /// with its agent ([`forget_session`]): what [`reset_allowed`] asks of
    /// every OTHER account.
    pub ran_at: Option<i64>,
    /// OBSERVATION: the newest word on its usage credits ([`Credits`]).
    pub credits: Option<Credits>,
    /// MACHINE-LOCAL: the local day (days since the epoch on this machine's
    /// clock and zone) the last credits episode that raised the day's line
    /// began on.
    pub credits_day: Option<i64>,
    /// OBSERVATION: the newest word on its five-hour window apart from a
    /// `/usage` sample ([`WindowSaid`]). Kept apart from [`Self::sample`]:
    /// nothing that moves a session reads it.
    pub warned_session: Option<WindowSaid>,
    /// OBSERVATION: the same of its week: what [`windows_full`] reads of it.
    pub warned_week: Option<WindowSaid>,
}

impl Account {
    /// Whether anything was ever observed of it.
    #[must_use]
    pub fn observed(&self) -> bool {
        !self.walls.is_empty()
            || self.sample.is_some()
            || self.offer.is_some()
            || self.auth != Auth::Unseen
            || self.credits.is_some()
            || self.warned_session.is_some()
            || self.warned_week.is_some()
    }

    /// The instant its last standing wall ends, while one stands at `now`.
    #[must_use]
    pub fn limited_until(&self, now: i64) -> Option<i64> {
        self.walls
            .values()
            .map(Wall::stands_until)
            .filter(|end| now < *end)
            .max()
    }

    /// Whether a wall observed at `at` is one the owner has cleared SINCE
    /// ([`pin`]): it was observed before the clearing — the clearing read as
    /// made no later than `now`, as every stamp of this machine's is. An
    /// observation of the clearing's own second is read as made after it,
    /// as [`note_wall`] reads every tie.
    fn cleared_since(&self, at: i64, now: i64) -> bool {
        self.cleared_at.is_some_and(|cleared| at < cleared.min(now))
    }

    /// When anything about it was last written (its eviction order).
    fn touched(&self) -> i64 {
        self.walls
            .values()
            .map(|w| w.at)
            .chain(self.sample.as_ref().map(|s| s.at))
            .chain(self.offer.as_ref().map(|o| o.at))
            .chain(self.auth_at)
            .chain(self.cleared_at)
            .chain(self.ran_at)
            .chain(self.credits.as_ref().map(Credits::at))
            .chain(self.warned_session.as_ref().map(|w| w.at))
            .chain(self.warned_week.as_ref().map(|w| w.at))
            .max()
            .unwrap_or(i64::MIN)
    }
}

/// Where one supervised session runs. MACHINE-LOCAL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRow {
    /// The account its process's own environment names.
    pub account: String,
    /// When that was last read.
    pub at: i64,
    /// Why it could not move, where a move was refused: `jobs`, `person`,
    /// `line`.
    pub held: Option<String>,
}

/// The one `/usage` probe in flight, or the last one asked. MACHINE-LOCAL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeClaim {
    /// The account probed.
    pub account: String,
    /// The session that types it.
    pub sid: String,
    /// When it was claimed.
    pub at: i64,
}

/// The one session on an `active` account that has not served a response
/// yet. MACHINE-LOCAL. Its hold on every other session lasts
/// [`CANARY_HOLD_S`] from `at`, and no longer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Canary {
    /// The session that moved alone.
    pub sid: String,
    /// When it did: the rotation's time, or the time it took a silent
    /// canary's place.
    pub at: i64,
    /// It took the place of a canary that did not answer ([`take_canary`]),
    /// rather than making the rotation itself: a relaunch it is refused
    /// gives up its hold and undoes no rotation ([`Strike::NotMoved`]).
    pub took: bool,
}

/// What one rotation wrote over. MACHINE-LOCAL. [`begin_rotation`] keeps it
/// beside what it writes, so the account the rotation left is `active` again
/// with the `previous`, the proof and the canary it had, whichever way the
/// rotation ends without a served response ([`strike`]): the relaunch the
/// host refused ([`Strike::NotMoved`]), which takes the rotation back
/// EXACTLY, and the candidate that answered badly, whose rotation stays
/// counted and whose word stays on record.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Undo {
    /// [`State::previous`] as the rotation found it.
    pub previous: Option<String>,
    /// [`State::previous_proven`] as the rotation found it.
    pub previous_proven: bool,
    /// [`State::canary`] as the rotation found it: the record of the session
    /// on the account left, where that account had not answered either.
    /// None once that session is forgotten ([`forget_session`]).
    pub canary: Option<Canary>,
}

/// The one limit-reset attempt for a wall ([`claim_reset`]). MACHINE-LOCAL.
/// It is kept after its outcome, and after it lapses, as the record that the
/// wall was attempted: one attempt per (account, wall), machine-wide.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResetClaim {
    /// The account at the wall.
    pub account: String,
    /// The wall's own reset: one attempt per (account, wall).
    pub until: Option<i64>,
    /// The session that takes the row.
    pub sid: String,
    /// When it was claimed.
    pub at: i64,
    /// The process the claim was made for: its pid, and its kernel start as
    /// the host reads it. `0` and empty in a state an older build wrote: no
    /// process's.
    pub pid: u32,
    /// See `pid`.
    pub start: String,
    /// The outcome's word once it was recorded ([`reset_outcome`],
    /// [`ResetDone::word`]); `None` while the claim is in flight, and for
    /// ever where nobody recorded one.
    pub done: Option<String>,
}

/// `state.json`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct State {
    /// [`SCHEMA`].
    pub schema: u64,
    /// MACHINE-LOCAL: this machine's id, minted once ([`mint_machine`]): the
    /// `by` of every observation made here.
    pub machine: String,
    /// The accounts, by label. Observations and the auth word.
    pub accounts: BTreeMap<String, Account>,
    /// MACHINE-LOCAL: the one account this home runs on.
    pub active: Option<String>,
    /// MACHINE-LOCAL: the account `active` was before the last rotation or
    /// pin, which a [`strike`] goes back to.
    pub previous: Option<String>,
    /// MACHINE-LOCAL: whether `previous` had served a response when it was
    /// left. The proof travels with the account: a [`strike`] restores
    /// exactly this, so an account left before it ever answered is not
    /// followed for having been gone back to.
    pub previous_proven: bool,
    /// MACHINE-LOCAL: `active` has served a response since it was chosen;
    /// only then is it followed.
    pub proven: bool,
    /// MACHINE-LOCAL: the session on an `active` that is not proven yet.
    pub canary: Option<Canary>,
    /// MACHINE-LOCAL: what each rotation that can still be gone back from
    /// wrote over, the newest last ([`Undo`], at most [`MAX_UNDO`]). The
    /// newest is the rotation `canary` made; one below it is a rotation that
    /// one went past. A [`strike`] that puts `active` back takes the newest
    /// with it and leaves the ones below: they are the account gone back
    /// to's own. Empty once the account answers or is pinned, the canary's
    /// agent ends, or another session takes the canary's place: nothing is
    /// left to go back from.
    pub undo: Vec<Undo>,
    /// MACHINE-LOCAL: `active` is the owner's word.
    pub pinned: bool,
    /// MACHINE-LOCAL: the supervised sessions, by session id.
    pub sessions: BTreeMap<String, SessionRow>,
    /// MACHINE-LOCAL: when `active` was rotated, the last hour's.
    pub rotations: Vec<i64>,
    /// MACHINE-LOCAL: the probe's claim.
    pub probe: Option<ProbeClaim>,
    /// MACHINE-LOCAL: the reset's claim.
    pub reset: Option<ResetClaim>,
}

impl Default for State {
    fn default() -> State {
        State {
            schema: SCHEMA,
            machine: String::new(),
            accounts: BTreeMap::new(),
            active: None,
            previous: None,
            previous_proven: false,
            proven: false,
            canary: None,
            undo: Vec::new(),
            pinned: false,
            sessions: BTreeMap::new(),
            rotations: Vec::new(),
            probe: None,
            reset: None,
        }
    }
}

/// Why a `state.json` was not read ([`State::from_json`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unread {
    /// It is not JSON: a file cut short. It holds nothing to lose, and the
    /// next write may replace it.
    Torn(String),
    /// It is JSON and it is not this build's: another schema, or none. It
    /// is never written over.
    NotOurs(String),
}

impl Unread {
    /// What was wrong with it.
    #[must_use]
    pub fn why(&self) -> &str {
        match self {
            Unread::Torn(why) | Unread::NotOurs(why) => why,
        }
    }
}

/// Whether the instant `t` is within the last `window` seconds at `now`. A
/// stamp AHEAD of the clock (a clock set back, another machine's running
/// fast) is not recent: read as one, it would stay recent for as long as
/// the clock is behind it.
fn recent(t: i64, now: i64, window: i64) -> bool {
    (0..window).contains(&now.saturating_sub(t))
}

fn int(v: Option<&Value>) -> Option<i64> {
    v.and_then(Value::as_i64)
}

/// The latest instant an observation read at `now` may be stamped
/// ([`STAMP_SKEW_S`]).
fn stamp_cap(now: i64) -> i64 {
    now.saturating_add(STAMP_SKEW_S)
}

/// A stamp as it is read: no later than `cap`.
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
    Value::Object(o)
}

fn wall_of(v: &Value, cap: i64) -> Option<Wall> {
    Some(Wall {
        until: int(v.get("until")),
        at: stamp(v.get("at"), cap)?,
        by: word(v.get("by"), WORD_MAX).unwrap_or_default(),
        attempts: count(v.get("attempts")).unwrap_or(0),
    })
}

fn sample_json(s: &Sample) -> Value {
    let mut o = Map::new();
    o.insert("session_pct".to_owned(), Value::from(s.session_pct));
    o.insert("session_reset".to_owned(), Value::from(s.session_reset));
    o.insert("week_pct".to_owned(), Value::from(s.week_pct));
    o.insert("week_reset".to_owned(), Value::from(s.week_reset));
    o.insert("at".to_owned(), Value::from(s.at));
    o.insert("by".to_owned(), Value::from(s.by.as_str()));
    Value::Object(o)
}

fn sample_of(v: &Value, cap: i64) -> Option<Sample> {
    Some(Sample {
        session_pct: pct(v.get("session_pct")),
        session_reset: int(v.get("session_reset")),
        week_pct: pct(v.get("week_pct")),
        week_reset: int(v.get("week_reset")),
        at: stamp(v.get("at"), cap)?,
        by: word(v.get("by"), WORD_MAX).unwrap_or_default(),
    })
}

fn offer_json(o: &Offer) -> Value {
    let mut m = Map::new();
    m.insert("left".to_owned(), Value::from(o.left));
    m.insert("use_by".to_owned(), Value::from(o.use_by));
    m.insert("at".to_owned(), Value::from(o.at));
    m.insert("by".to_owned(), Value::from(o.by.as_str()));
    Value::Object(m)
}

fn offer_of(v: &Value, cap: i64) -> Option<Offer> {
    Some(Offer {
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
    o.insert("before".to_owned(), sample(&a.before));
    o.insert(
        "offer".to_owned(),
        a.offer.as_ref().map_or(Value::Null, offer_json),
    );
    o.insert("auth".to_owned(), Value::from(a.auth.as_str()));
    o.insert("auth_at".to_owned(), Value::from(a.auth_at));
    o.insert("cleared_at".to_owned(), Value::from(a.cleared_at));
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
    Value::Object(o)
}

/// [`Credits`] as the state keeps it: its `word` (`in-use`, `off`, `on`) and
/// that word's own keys.
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

/// [`Credits`] back, its stamps no later than `cap`; `None` for a word this
/// build does not know.
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

/// [`WindowSaid`] back, its stamp no later than `cap`.
fn window_of(v: &Value, cap: i64) -> Option<WindowSaid> {
    Some(WindowSaid {
        pct: pct(v.get("pct")),
        reset: int(v.get("reset")),
        at: stamp(v.get("at"), cap)?,
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
    // The newest observations are the ones kept.
    walls.sort_by_key(|(_, w)| std::cmp::Reverse(w.at));
    walls.truncate(MAX_WALLS);
    Account {
        walls: walls.into_iter().collect(),
        sample: v.get("sample").and_then(|s| sample_of(s, cap)),
        before: v.get("before").and_then(|s| sample_of(s, cap)),
        offer: v.get("offer").and_then(|o| offer_of(o, cap)),
        auth: v
            .get("auth")
            .and_then(Value::as_str)
            .map_or(Auth::Unseen, Auth::parse),
        // This machine's own stamps: no other clock to allow for.
        auth_at: stamp(v.get("auth_at"), now),
        cleared_at: stamp(v.get("cleared_at"), now),
        ran_at: stamp(v.get("ran_at"), now),
        // Absent in a state an older build wrote: none observed.
        credits: v.get("credits").and_then(|c| credits_of(c, cap)),
        credits_day: int(v.get("credits_day")),
        warned_session: v.get("warned_session").and_then(|w| window_of(w, cap)),
        warned_week: v.get("warned_week").and_then(|w| window_of(w, cap)),
    }
}

impl State {
    /// The state as one line of JSON, keys sorted. An unknown is `null`,
    /// never `0`.
    #[must_use]
    pub fn to_json(&self) -> String {
        let mut o = Map::new();
        o.insert("schema".to_owned(), Value::from(self.schema));
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
                m.insert("sid".to_owned(), Value::from(c.sid.as_str()));
                m.insert("at".to_owned(), Value::from(c.at));
                m.insert("pid".to_owned(), Value::from(c.pid));
                m.insert("start".to_owned(), Value::from(c.start.as_str()));
                m.insert("done".to_owned(), Value::from(c.done.clone()));
                Value::Object(m)
            }),
        );
        aterm_json::to_string(&Value::Object(o)).unwrap_or_else(|_| String::from("{}"))
    }

    /// Read one `state.json`.
    ///
    /// Unknown keys are ignored, and a known key of the wrong shape is the
    /// unknown it stands for (`null`), so a newer writer's additions do not
    /// cost this reader the file. Everything is BOUNDED as it is read: at
    /// most [`MAX_ACCOUNTS`] accounts (inadmissible labels dropped),
    /// [`MAX_WALLS`] walls each, the [`MAX_SESSIONS`] newest sessions, the
    /// [`MAX_ROTATIONS`] newest rotation times, the [`MAX_UNDO`] newest
    /// undos (absent: nothing to take back), every string cut. And NO
    /// STAMP IS READ FROM THE FUTURE: an observation's `at` is no later than
    /// `now` plus [`STAMP_SKEW_S`] (another machine's clock may honestly run
    /// that far ahead), and this machine's own stamps — the auth word's, the
    /// pin's — no later than `now`. A stamp from a clock that ran ahead is
    /// not newer than everything this machine will ever see: left as it
    /// was, the next `accounts login` would record nothing until the clock
    /// caught up with it.
    ///
    /// # Errors
    ///
    /// [`Unread::Torn`]: the text is not JSON — a file cut short, which the
    /// next write may replace. [`Unread::NotOurs`]: it is JSON and is not an
    /// object, or names no schema or one other than [`SCHEMA`] — a file this
    /// build must not guess at, and must not write over.
    pub fn from_json(text: &str, now: i64) -> Result<State, Unread> {
        let doc: Value = aterm_json::from_str(text)
            .map_err(|e| Unread::Torn(format!("not JSON ({})", shown(&e.to_string()))))?;
        if !doc.is_object() {
            return Err(Unread::NotOurs("not a JSON object".to_string()));
        }
        match doc.get("schema").and_then(Value::as_u64) {
            Some(SCHEMA) => {}
            Some(other) => {
                return Err(Unread::NotOurs(format!(
                    "schema {other}; this build reads schema {SCHEMA}"
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
            .take(MAX_ACCOUNTS)
            .map(|(k, v)| (k.clone(), account_of(v, now)))
            .collect();
        let mut sessions: Vec<(String, SessionRow)> = doc
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
        sessions.sort_by(|a, b| b.1.at.cmp(&a.1.at).then_with(|| a.0.cmp(&b.0)));
        sessions.truncate(MAX_SESSIONS);
        let mut rotations: Vec<i64> = doc
            .get("rotations")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_i64)
            .collect();
        rotations.sort_unstable();
        let extra = rotations.len().saturating_sub(MAX_ROTATIONS);
        rotations.drain(..extra);
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
                sid: word(c.get("sid"), WORD_MAX)?,
                at: int(c.get("at"))?,
                // Absent in a state an older build wrote: a claim of no
                // process's, with no outcome, which lapses by its clock.
                pid: count(c.get("pid")).unwrap_or(0),
                start: word(c.get("start"), WORD_MAX).unwrap_or_default(),
                done: word(c.get("done"), WORD_MAX),
            })
        });
        let canary = doc.get("canary").and_then(canary_of);
        // What a rotation wrote over is its maker's to put back, so it is
        // read only beside a canary that made its rotation; with none, or
        // one that took another's place, there is nothing to take back.
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
            sessions: sessions.into_iter().collect(),
            rotations,
            probe,
            reset,
        })
    }

    /// What is kept about `label`, or the nothing kept about an account
    /// nobody has observed.
    #[must_use]
    pub fn account(&self, label: &str) -> Account {
        self.accounts.get(label).cloned().unwrap_or_default()
    }

    /// Since when the session on `active` has been holding the others back:
    /// when it moved there alone ([`Canary::at`]), while `active` has not
    /// served a response. `None` once it is proven, struck or pinned, and
    /// when no session holds it.
    #[must_use]
    pub fn canary_since(&self) -> Option<i64> {
        (self.active.is_some() && !self.proven)
            .then(|| self.canary.as_ref().map(|c| c.at))
            .flatten()
    }

    /// Whether the canary's hold stands at `now`: `active` has not served a
    /// response, and the session on it moved there less than
    /// [`CANARY_HOLD_S`] ago. A hold stamped ahead of the clock is not one.
    #[must_use]
    pub fn canary_holds(&self, now: i64) -> bool {
        self.canary_since()
            .is_some_and(|at| recent(at, now, CANARY_HOLD_S))
    }

    /// The instant the canary's hold ends, while it stands at `now`
    /// ([`State::canary_holds`]): [`CANARY_HOLD_S`] after it began. `None`
    /// with no hold standing.
    #[must_use]
    pub fn canary_until(&self, now: i64) -> Option<i64> {
        self.canary_since()
            .filter(|_| self.canary_holds(now))
            .map(|since| since.saturating_add(CANARY_HOLD_S))
    }

    /// The ids of the sessions last read on `label`, in id order.
    #[must_use]
    pub fn sessions_on(&self, label: &str) -> Vec<&str> {
        self.sessions
            .iter()
            .filter(|(_, s)| s.account == label)
            .map(|(sid, _)| sid.as_str())
            .collect()
    }
}

/// A fresh machine id: 8 bytes of the OS's entropy as 16 hex characters
/// (the workspace's one audited source, `aterm_uds::rand`). Not a secret: it
/// only says which machine made an observation.
///
/// # Errors
///
/// No OS entropy source answered.
pub fn mint_machine() -> io::Result<String> {
    aterm_uds::rand::hex_token::<8>()
}

// ---------------------------------------------------------------------------
// 3. The condition (design §5's table) — derived, never stored
// ---------------------------------------------------------------------------

/// The one word for an account, in the table's order: the first row that
/// applies is the answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Condition {
    /// No login was made for it on this machine: its login directory is
    /// absent, or holds nothing yet ([`login_made`]).
    NotEnrolled,
    /// Its login's last word on record is `signed-out` ([`Auth::SignedOut`]):
    /// a login wall right after a session was moved onto it ([`strike`]), a
    /// first login whose flow failed, or the check after a sign-in said so.
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
    /// The report's word.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Condition::NotEnrolled => "not-enrolled",
            Condition::SignedOut => "signed-out",
            Condition::Limited { .. } => "limited",
            Condition::Low => "low",
            Condition::Usable => "usable",
            Condition::Unseen => "unseen",
        }
    }

    /// Fresh: budget nobody has seen run down. `unseen` counts as fresh.
    #[must_use]
    pub fn fresh(self) -> bool {
        matches!(self, Condition::Usable | Condition::Unseen)
    }

    /// A session can run on it now: fresh or `low`.
    #[must_use]
    pub fn runs(self) -> bool {
        self.fresh() || self == Condition::Low
    }
}

/// The condition of `row`'s account at `now`. `enrolled` is whether a login
/// was made in its directory on this machine ([`enrolled`]; the row with no
/// `dir`: always).
#[must_use]
pub fn condition(row: &Row, state: &State, enrolled: bool, now: i64) -> Condition {
    if !enrolled {
        return Condition::NotEnrolled;
    }
    let Some(account) = state.accounts.get(&row.label) else {
        return Condition::Unseen;
    };
    if account.auth == Auth::SignedOut {
        return Condition::SignedOut;
    }
    if let Some(until) = account.limited_until(now) {
        return Condition::Limited { until };
    }
    if account.sample.as_ref().is_some_and(|s| s.low(now)) {
        return Condition::Low;
    }
    if account.observed() {
        Condition::Usable
    } else {
        Condition::Unseen
    }
}

/// The labels whose [`condition`] is [`Condition::SignedOut`], in ROSTER
/// order: what the window's digest names, and the first is the one its action
/// asks the owner to sign in (`accounts login`). A row not enrolled is left
/// out however its state reads: it is the normal state between `accounts add`
/// and `accounts login`, and `add` printed the line to run. So is a row with
/// an email: it signs in only through `accounts renew`, which `login`
/// refuses, and its ended login is the due line's ([`renew_due`]). PURE.
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
            row.email.is_none()
                && condition(row, state, is_enrolled(row, enrolled), now) == Condition::SignedOut
        })
        .map(|row| row.label.clone())
        .collect()
}

// ---------------------------------------------------------------------------
// 4. Advice (design §6)
// ---------------------------------------------------------------------------

/// `[harness] accounts`.
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
    /// The config's word.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Early => "early",
            Mode::Wall => "wall",
            Mode::Off => "off",
        }
    }

    /// One `accounts =` value; anything else is no mode.
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

/// A session's point: a turn end.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Point {
    /// The turn ended on a usage limit THIS process hit. From this instant
    /// the session's account is limited, WHETHER OR NOT THE WALL IS ON
    /// RECORD YET: [`advise`] reads it so. The host records it —
    /// [`note_wall`], or the canary's own [`strike`] — in the same
    /// transaction as the move it commits, and before it: [`begin_rotation`]
    /// reads the state, not the point.
    Wall {
        /// The vendor's limit type (`five_hour`, `seven_day`, …).
        window: String,
        /// When the limit's row was written (Unix seconds, never ahead of
        /// the caller's clock), as the host read it from the session's
        /// transcript. `None` where there is no row to date (a caller that
        /// asks for no session), and then nothing is said of the owner's
        /// clearing ([`Advice::Resume`]).
        at: Option<i64>,
    },
    /// A LOST LOGIN RIGHT AFTER THE SESSION'S OWN MOVE: a wall for the
    /// roster too — the session cannot run where it is — with no usage
    /// wall behind it and no row to date. Not a limit: what stops the
    /// session is the login, and what [`advise`] reads of it is the
    /// record — the account struck signed out ([`strike`]) where the host
    /// was told of the lost login, or the word it held before where it was
    /// not (a strike the store's lock kept out, a move answered by a wall
    /// before). Where the record says the account can be run on again, its
    /// login's word `ok` written AFTER `seen` — the owner signed it back in
    /// since — the session is relaunched under it.
    LostLogin {
        /// When the host first met THIS PROCESS'S lost login (Unix
        /// seconds, its own clock's): a word about the login older than
        /// it, or of its own second, is one the lost login has overruled.
        seen: i64,
    },
    /// A LOST LOGIN WITH NO MOVE BEHIND IT, in a session whose login is a
    /// row a renewal can restore (one with an email): the login's own end,
    /// recorded by the host before it asks ([`note_login_end`]). Never a
    /// rotation (between accounts: R-5, R-7): the advice is the session's own
    /// label, renewed (whatever the account's usage), or nothing.
    LoginEnded,
    /// A turn that ended with none.
    TurnEnd,
}

/// The session asking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Me<'a> {
    /// The label its process's own environment names
    /// ([`Roster::row_for_login`]), or `None` when no row does.
    pub account: Option<&'a str>,
    /// The label whose login a renewal on this machine moved AWAY FROM the
    /// one its process holds ([`Roster::row_left`], through a [`bound`]
    /// roster): the same account, on a superseded login. `None` with
    /// `account` set, and wherever no renewal superseded the login — or
    /// where a host asks about the session at a usage limit, as its account
    /// ([`login_follow`]).
    pub left: Option<&'a str>,
    /// Its Claude home is the default one.
    pub default_home: bool,
    /// Its process authenticates by a stored login: no API key or token in
    /// its environment, no provider, no custom route.
    pub stored_login: bool,
}

impl<'a> Me<'a> {
    /// The account the session runs on: its login's row ([`Me::account`]),
    /// or the row a renewal moved off that login ([`Me::left`]).
    #[must_use]
    pub fn label(&self) -> Option<&'a str> {
        self.account.or(self.left)
    }
}

/// Why the advice is to stay.
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
    /// The process does not authenticate by a stored login.
    NotAStoredLogin,
    /// `active` has not served a response yet: nobody follows it, and nobody
    /// rotates past it — the session that chose it included, which is not
    /// moved on from it ahead of a wall before it has answered. The hold on
    /// the other sessions lasts [`CANARY_HOLD_S`] ([`State::canary_holds`]);
    /// then the next one takes the canary's place.
    Canary,
    /// At a wall with no candidate and no instant of the roster's to wait
    /// for: the wall is waited out as it would be with no roster.
    NoCandidate,
    /// The machine's rotation bound is spent: the wall is waited out.
    Breaker {
        /// When the bound has room again ([`breaker_until`]): the instant to
        /// decide again, with a candidate still there.
        until: i64,
    },
    /// An account to leave ahead of its wall (`low`, or `limited` by a wall
    /// another tab recorded), and the owner's pin holds.
    Pinned,
    /// An account to leave, and it holds a reset offer that expires within
    /// a day: it is run to its wall, where the reset is offered.
    OfferExpiring,
    /// An account to leave, and no fresh account to move to.
    NoFresh,
    /// Nothing is due.
    NothingDue,
}

impl StayWhy {
    /// The report's word.
    #[must_use]
    pub fn as_str(self) -> &'static str {
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
        }
    }
}

/// What a session should do at its point. Advice only: the host commits a
/// move in one transaction under the lock ([`begin_rotation`]).
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
        /// Ahead of the wall (deferred like the model's return) rather than
        /// at it (held on the restart back-off).
        ahead: bool,
        /// This move changes `active` and needs [`begin_rotation`] to win;
        /// `false` moves onto the account that is already `active`.
        rotates: bool,
        /// `active` has not served a response and the session that moved
        /// onto it has been silent for [`CANARY_HOLD_S`]: this session takes
        /// its place, and needs [`take_canary`] to win. `false` with
        /// `rotates: false` follows a proven `active`, which needs nothing.
        takes_canary: bool,
    },
    /// No account is usable before this instant.
    Wait {
        /// When to decide again.
        until: i64,
    },
    /// THE OWNER LIFTED THIS WALL: continue here, now. The session stands
    /// at a limit row written BEFORE the owner cleared its account's walls
    /// (`accounts use`, [`pin`]), and nothing has been recorded against the
    /// account since. Nobody knows whether the limit is still there, and
    /// the continuation is the probe: a served turn answers it, and a NEW
    /// limit row (written after the clearing) is an ordinary wall — it is
    /// recorded, ends the pin (`unpin: wall`) and is rotated from. Nothing
    /// is committed for it: no rotation, no canary, no ledger row.
    Resume {
        /// When the owner cleared the account ([`Account::cleared_at`], as
        /// the state was read): the key of "once per clearing". A caller
        /// that has answered a `Resume` for this instant types no second
        /// continuation for it; the wall's own ladder takes over.
        cleared_at: i64,
    },
}

impl Advice {
    /// The one word a host logs for it.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Advice::Stay(_) => "stay",
            Advice::Probe => "probe",
            Advice::Move { .. } => "move",
            Advice::Wait { .. } => "wait",
            Advice::Resume { .. } => "resume",
        }
    }
}

/// A candidate's sort key. Lower sorts first: fresh before `low`, dedicated
/// before shared, the week that resets soonest (unknown last), the label.
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

/// Why a row is not a candidate. Printed, never inferred.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WhyNot {
    /// It is the session's own account.
    Me,
    /// No login was made for it on this machine ([`login_made`]).
    NotEnrolled,
    /// Its login is known signed out.
    SignedOut,
    /// A wall stands.
    Limited {
        /// Until this instant.
        until: i64,
    },
    /// `low`, and the point is not a wall.
    LowAheadOfAWall,
    /// `low`, and an account — this one included, when the window that
    /// makes it low is about to reset — becomes usable within
    /// [`LOW_ONLY_AFTER_S`].
    LowWhileOneIsNear {
        /// When that account is usable.
        at: i64,
    },
}

impl WhyNot {
    /// The report's words.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            WhyNot::Me => "this session's own",
            WhyNot::NotEnrolled => "not-enrolled",
            WhyNot::SignedOut => "signed-out",
            WhyNot::Limited { .. } => "limited",
            WhyNot::LowAheadOfAWall => "low: used only at a wall",
            WhyNot::LowWhileOneIsNear { .. } => "low: an account is usable within 15 minutes",
        }
    }
}

/// The instant an account in condition `cond` is usable: `now` for a fresh
/// one, the end of its last wall for a limited one, the end of the window
/// that makes it low for a `low` one. `None` for one signed out or not
/// enrolled, which no clock makes usable.
fn usable_at(row: &Row, state: &State, cond: Condition, now: i64) -> Option<i64> {
    match cond {
        Condition::Limited { until } => Some(until),
        Condition::Usable | Condition::Unseen => Some(now),
        Condition::Low => state
            .accounts
            .get(&row.label)
            .and_then(|a| a.sample.as_ref())
            .and_then(|s| s.low_until(now)),
        Condition::SignedOut | Condition::NotEnrolled => None,
    }
}

/// Every row with its rank, or the reason it has none: the candidates first,
/// best first, then the rest in roster order. `me` is the asking session's
/// label (`None` for the report, which asks for no session).
///
/// A candidate is enrolled, not known signed out, not limited and not `me`.
/// A `low` account is one only at a wall, and only when no account —
/// `me` included, and the low account itself — becomes usable within
/// [`LOW_ONLY_AFTER_S`]: a limited one by its wall ending, a low one by the
/// window that makes it low resetting ([`Sample::low_until`]).
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
        .filter_map(|(row, c)| usable_at(row, state, *c, now))
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
                    .and_then(|a| a.sample.as_ref())
                    .and_then(|s| s.week_reset)
                    .filter(|reset| *reset > now),
            };
            let verdict = if me == Some(row.label.as_str()) {
                Err(WhyNot::Me)
            } else {
                match *cond {
                    Condition::NotEnrolled => Err(WhyNot::NotEnrolled),
                    Condition::SignedOut => Err(WhyNot::SignedOut),
                    Condition::Limited { until } => Err(WhyNot::Limited { until }),
                    Condition::Low if !at_wall => Err(WhyNot::LowAheadOfAWall),
                    Condition::Low => match near {
                        Some(at) => Err(WhyNot::LowWhileOneIsNear { at }),
                        None => Ok(rank(true)),
                    },
                    Condition::Usable | Condition::Unseen => Ok(rank(false)),
                }
            };
            (row.label.clone(), verdict)
        })
        .collect();
    // A stable sort: the candidates by their key, every other row after
    // them in the order the roster lists it.
    out.sort_by(|a, b| match (&a.1, &b.1) {
        (Ok(x), Ok(y)) => x.key(&a.0).cmp(&y.key(&b.0)),
        (Ok(_), Err(_)) => std::cmp::Ordering::Less,
        (Err(_), Ok(_)) => std::cmp::Ordering::Greater,
        (Err(_), Err(_)) => std::cmp::Ordering::Equal,
    });
    out
}

/// The first candidate, where there is one.
#[must_use]
pub fn next_candidate(
    roster: &Roster,
    state: &State,
    enrolled: &BTreeSet<String>,
    me: Option<&str>,
    point: &Point,
    now: i64,
) -> Option<String> {
    candidates(roster, state, enrolled, me, point, now)
        .into_iter()
        .find(|(_, verdict)| verdict.is_ok())
        .map(|(label, _)| label)
}

/// The instant to wait for when nothing can be moved into: the earliest
/// instant any account is usable — and the earliest DEDICATED instant when
/// that is within [`DEDICATED_WAIT_S`] of it, so a herd is not relaunched
/// onto a shared account minutes before a dedicated one opens.
///
/// A fresh account's instant is `now`; a limited one's is the end of its
/// last wall; a `low` one's is the end of the window that makes it low, when
/// it is fresh again; one that is signed out or not enrolled has none.
/// `None` when no account has an instant.
#[must_use]
pub fn next_usable(
    roster: &Roster,
    state: &State,
    enrolled: &BTreeSet<String>,
    now: i64,
) -> Option<i64> {
    wait_instant(roster, state, enrolled, Instants::All, now)
}

/// THE ROSTER'S NEXT OPENING: the next instant AHEAD of `now` at which an
/// account becomes usable — a limited one by its last wall ending, a `low`
/// one by the window that makes it low resetting — by [`next_usable`]'s own
/// rule (the earliest, or the earliest dedicated one within
/// [`DEDICATED_WAIT_S`] of it). An account usable now has no instant here:
/// this is asked for a session that is HELD though accounts can be run on
/// (the canary's hold, the machine's rotation bound), and what ends its
/// wait early is something that CHANGES ([`hold_until`]). `None` when
/// nothing is due to open.
#[must_use]
pub fn next_opening(
    roster: &Roster,
    state: &State,
    enrolled: &BTreeSet<String>,
    now: i64,
) -> Option<i64> {
    wait_instant(roster, state, enrolled, Instants::Ahead, now)
}

/// WHEN A SESSION HELD AT ITS WALL DECIDES AGAIN: the end of what holds it,
/// or the roster's next opening where that comes first. The wait ends at
/// whatever comes first.
///
/// * [`StayWhy::Canary`]: the end of the canary's hold
///   ([`State::canary_until`]); `None` when no hold stands at `now`.
/// * [`StayWhy::Breaker`]: the instant the machine's bound has room again.
/// * Any other reason holds nothing: `None`.
///
/// `opening` is [`next_opening`] on the same state and clock (the caller's,
/// which has the roster); one not ahead of `now` is none. A reset that makes
/// the active account followable, or the session's own account usable, then
/// ends the wait, rather than the session sitting out the rest of ten
/// minutes or of the hour beside an account it could run on. PURE.
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
    /// Every account's: the report's question, which asks for no session.
    All,
    /// Every account's, but only one ahead of `now` for this one: a session
    /// at its wall on it. Its own account is limited whether or not that
    /// wall is on record yet, so it has no instant of `now`: the end of the
    /// wall once it is recorded, of the window a sample shows it low by
    /// until then, and none where neither is known — the wall's own reset is
    /// not this function's to guess.
    WalledOn(&'a str),
    /// Only the ones ahead of `now` ([`next_opening`]).
    Ahead,
}

/// [`next_usable`]'s rule over the instants `which` counts.
fn wait_instant(
    roster: &Roster,
    state: &State,
    enrolled: &BTreeSet<String>,
    which: Instants<'_>,
    now: i64,
) -> Option<i64> {
    let instants: Vec<(Class, i64)> = roster
        .rows
        .iter()
        .filter_map(|r| {
            let cond = condition(r, state, is_enrolled(r, enrolled), now);
            let at = usable_at(r, state, cond, now)?;
            let counted = match which {
                Instants::All => true,
                Instants::WalledOn(walled) => walled != r.label || at > now,
                Instants::Ahead => at > now,
            };
            counted.then_some((r.class, at))
        })
        .collect();
    let earliest = instants.iter().map(|(_, t)| *t).min()?;
    let dedicated = instants
        .iter()
        .filter(|(class, _)| *class == Class::Dedicated)
        .map(|(_, t)| *t)
        .min();
    Some(match dedicated {
        Some(t) if t.saturating_sub(earliest) <= DEDICATED_WAIT_S => t,
        _ => earliest,
    })
}

/// How long after its newest sample an account's next probe is due: half
/// the time to the threshold at the burn between the last two samples,
/// clamped to [`PROBE_MIN_S`]–[`PROBE_MAX_S`]; [`PROBE_DEFAULT_S`] when no
/// burn is known.
///
/// A window's burn is known when both samples carry its figure for the SAME
/// window (one reset) and the newer is later; a window that rolled over
/// between them says nothing. A burn of zero is known: the threshold is
/// never reached, and the clamp answers.
#[must_use]
pub fn probe_interval(account: &Account) -> i64 {
    let (Some(new), Some(old)) = (&account.sample, &account.before) else {
        return PROBE_DEFAULT_S;
    };
    let span = new.at.saturating_sub(old.at);
    if span <= 0 {
        return PROBE_DEFAULT_S;
    }
    // Whole seconds, in integers: half of (what is left ÷ the burn), where
    // the burn is the percent gained over `span` seconds.
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

/// Whether `account`'s `/usage` read is due at `now`: it has no sample, or
/// its newest is not within the last [`probe_interval`] — older than that,
/// or stamped ahead of the clock, which says nothing about now.
#[must_use]
pub fn probe_due(account: &Account, now: i64) -> bool {
    account
        .sample
        .as_ref()
        .is_none_or(|s| !recent(s.at, now, probe_interval(account)))
}

/// Whether `account` holds a reset offer that expires within
/// [`OFFER_EXPIRING_S`]: a reset left, and a deadline that is read, ahead
/// and that near. An offer whose deadline nobody read is not one.
fn offer_expiring(account: &Account, now: i64) -> bool {
    account.offer.as_ref().is_some_and(|o| {
        o.left > 0
            && o.use_by
                .is_some_and(|by| by > now && by.saturating_sub(now) <= OFFER_EXPIRING_S)
    })
}

/// What the session `me` should do at `point`. PURE: it writes nothing and
/// reads no clock but `now`.
///
/// AHEAD OF THEM, under every mode, THE LOGIN LIFECYCLE's points
/// ([`login_follow`]): a lost login with no move behind it
/// ([`Point::LoginEnded`]) is advised the session's own label once a renewal
/// superseded the login it holds ([`Me::left`]), and nothing else — never
/// another account; and a session on a superseded login is moved onto the
/// renewed one at any point but a usage wall, whatever the account's usage
/// (a host asks about a session at a usage limit as its account, with no
/// [`Me::left`]: see [`login_follow`]).
///
/// Design §6's rules, in order:
///
/// 1. **Stay** with the mode off, no roster, a `me` no row names, a home
///    that is not the default, or a process that does not authenticate by a
///    stored login.
/// 2. **Follow**: `active` is another account that can be run on (not
///    limited — and, stated beyond the design's words, not signed out or
///    unenrolled either), is PROVEN, and is fresh or the owner's pin: `Move`
///    to it, at any point. A proven `active` that is `low` and not pinned is
///    not followed for its own sake: the rules below decide, and rule 3
///    falls back on it only when no fresh account is left.
///
///    NOT PROVEN YET, the one session that chose it moves alone: every other
///    session gets `Stay` ([`StayWhy::Canary`]) while its hold stands
///    ([`State::canary_holds`], [`CANARY_HOLD_S`]). Once it has lapsed — the
///    canary's tab was closed, or sits idle with nothing to send — the next
///    session takes its place: `Move` to `active` with `takes_canary`, which
///    [`take_canary`] commits. `active` is never put back by the clock. At a
///    plain turn end an unproven `active` that is `low` is not moved onto
///    either: that waits for a session at a wall.
///
/// BETWEEN RULES 2 AND 3, **Resume** ([`Advice::Resume`]): THE OWNER LIFTED
/// THIS WALL. The point is a wall whose row is dated ([`Point::Wall`]'s
/// `at`), the owner cleared the session's account AFTER that row was
/// written ([`Account::cleared_at`]; the tie is [`note_wall`]'s — a row of
/// the clearing's own second is read as made after it), and the account can
/// be run on as the state has it: no wall has been recorded against it
/// since, by this machine or a merge, and its login is not known signed
/// out. Then the advice is to continue HERE, now: the continuation is the
/// probe. It comes AFTER rule 2, so the owner's later word for another
/// account (a pin moved on) and a proven fresh `active` are followed first,
/// and an account that has not answered yet keeps its canary's hold. It
/// comes BEFORE rule 3, so the wall the owner cleared is never rotated
/// from: with the pin standing that rotation is one the compare-and-set
/// refuses ([`Lost::Pinned`]) and the recorder drops
/// ([`Recorded::Cleared`]) — advised, it was asked again on every back-off
/// rung until the row's own reset, and the owner's word reached nothing.
/// The owner need not still be pinning: the clearing is what is asked, and
/// `use --auto` after it leaves it standing. A row with no date
/// (`at: None`) is never read as cleared.
///
/// It carries the clearing's instant, as read: a caller types ONE
/// continuation for it. Where the account answers that with a new limit,
/// the new row is dated after the clearing and rule 3 takes it — recorded
/// ([`note_wall`]: it ends the pin), then rotated from. A clearing stamped
/// AHEAD of the clock (a clock set back since `accounts use`) is read as
/// made now at every read, so its instant moves with the clock: the key is
/// then not one a caller can hold, and NOTHING HERE BOUNDS THAT — the
/// caller's own bound on what it types does.
///
/// AFTER IT, STILL BEFORE RULE 3, **the login back**: THE OWNER SIGNED THE
/// LOST LOGIN IN AGAIN. The point is a lost login ([`Point::LostLogin`]),
/// and the session's own account can be run on as the state has it, its
/// login's last word `ok` ([`Auth::Ok`]: the check after `accounts login`,
/// or a served response) and WRITTEN AFTER THE LOST LOGIN WAS SEEN
/// ([`Account::auth_at`] later than the point's `seen`, strictly). Then
/// `Move` to it — a follow: nothing rotates — and the host relaunches the
/// session under the very login it lost, where the sign-in landed. The
/// instant is what keeps this rule off a login nothing has said a word of
/// since it was lost: the host strikes the account where it is told of the
/// lost login ([`strike`]: `signed-out`), but not always before it asks —
/// the turn asked may still be awaited, the move may have been answered by
/// a wall already, the store's lock may keep the strike out — and the `ok`
/// left from the login's enrolment, or from before the move, is no sign-in.
/// Read so, the first point of a lost login is never this rule's; nor is a
/// login nobody has said a word of (`unseen`).
///
/// THE CANARY'S HOLD STANDS HERE TOO. Rule 2 asks of `active` only where it
/// is another account; where it is the session's OWN account, not proven
/// yet — another session rotated onto the login signed back in, or this
/// one's own rotation was never struck — the move is that canary's
/// follower: `Stay` ([`StayWhy::Canary`]) while the hold stands (the
/// session's own hold included: the advice is asked for no session id),
/// then `Move` with `takes_canary`, which [`take_canary`] commits. Rule 2
/// still comes first, so an account the owner pinned since and a proven
/// fresh `active` are followed instead.
///
/// 3. **At a wall**: the next candidate, as a rotation — past a proven `low`
///    `active` too, when the candidate is fresh. That `active` itself, as a
///    follow (it rotates nothing), when only `low` accounts are left or the
///    hour's rotations are spent. No candidate: `Wait` until
///    [`next_usable`], or `Stay` when the roster has no instant ahead.
///
///    A lost login is a wall here too ([`Point::LostLogin`]): its own
///    account is never a candidate, struck or not.
///
///    AT A WALL `me` IS LIMITED, on record or not: the point says so (a wall
///    the owner cleared since is not this rule's: see above). It is never
///    the account waited for as usable now, and a pin on it is not asked
///    about — a wall on the pinned account ends the pin ([`note_wall`]), so
///    the rotation is advised. THE HOST RECORDS THE WALL
///    IN THE SAME TRANSACTION, before it commits: [`note_wall`], or
///    [`strike`] when the session is the canary (which puts `active` back:
///    advise again). [`begin_rotation`] reads the state and not the point,
///    and on the state with the wall recorded it commits every rotation this
///    gives — the pin ended, the session's own account not one to run on.
///    The advice is the same asked before the wall is recorded or after,
///    but for what the wall's own end decides, which is known only once it
///    is recorded: the instant of a `Wait`, and whether an account is usable
///    within [`LOW_ONLY_AFTER_S`].
/// 4. **At a plain turn end, `early`, no pin**: `Probe` when due (another
///    row exists; no claim stands in its way — none for this account within
///    [`PROBE_MIN_S`], none for ANY account within [`PROBE_APART_S`]:
///    [`probe_held`], the test [`claim_probe`] makes too; no wall stands on
///    the account — a wall says more than a sample would; and the account
///    is not the active one still unanswered, which is never probed). Then
///    — and at once for a caller that asks past the probe
///    ([`advise_past_probe`]) — on an account to LEAVE — `limited` (a wall
///    another tab recorded), or `low` by a window that does not reset within
///    [`RESET_SOON_S`] ([`Sample::leave_early`]) — with a fresh candidate
///    and no reset offer about to expire: `Move` ahead of the wall.
/// 5. Otherwise **Stay**.
///
/// A move that rotates is `Stay` ([`StayWhy::Breaker`], with the instant the
/// bound has room again) once the machine's bound is spent
/// ([`rotations_spent`]), and `Stay` ([`StayWhy::Canary`]) ahead of a wall
/// while the session's own account is the active one, can be run on and has
/// not served a response yet: it is the canary, or stands where one stood.
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

/// [`advise`] with its probe arm skipped: never [`Advice::Probe`].
///
/// WHAT A DUE PROBE STANDS IN FRONT OF. Rule 4 answers `Probe` BEFORE it
/// asks whether the account is one to leave, so a session on a low account
/// whose sample is due is told to read `/usage` and nothing about the move.
/// A caller that hands the advice to a decider hands this beside it (the
/// policy's `AccountAdvice::Probe { otherwise }`): the move goes on whether
/// or not anybody makes the probe. And a host's commit asks THIS, at both of
/// its re-advices: a due probe is no reason to refuse a move it was asked
/// for. Everywhere [`advise`] is not `Probe` the two are the same answer
/// (`accounts_tests.rs` asks both at every fixture).
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

/// [`advise`] (`probes`) and [`advise_past_probe`] (not): ONE body, so the
/// two cannot drift apart anywhere but at the probe's arm.
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
    // The login lifecycle's points are the logins', whatever the rotation's
    // mode: a renewed login is followed, and an ended one waited at, under
    // `accounts = "off"` too ([`login_follow`]).
    let login_point =
        *point == Point::LoginEnded || (me.left.is_some() && !matches!(point, Point::Wall { .. }));
    if mode == Mode::Off && !login_point {
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
    if !me.stored_login {
        return Advice::Stay(StayWhy::NotAStoredLogin);
    }
    let at_wall = matches!(
        point,
        Point::Wall { .. } | Point::LostLogin { .. } | Point::LoginEnded
    );
    let cond = |row: &Row| condition(row, state, is_enrolled(row, enrolled), now);
    if login_point {
        return login_follow(mine, me, point, cond(mine));
    }
    let onto_active = |active: &Row, takes_canary: bool| Advice::Move {
        to: active.label.clone(),
        ahead: !at_wall,
        rotates: false,
        takes_canary,
    };

    // Rule 2, and the canary. `low_active` is a proven active account that
    // is low and not pinned: not followed here, and what rule 3 falls back
    // on.
    let mut low_active = None;
    if let Some(active) = state.active.as_deref().and_then(|l| roster.row(l))
        && active.label != mine.label
    {
        let theirs = cond(active);
        if theirs.runs() && !state.proven {
            let take = !state.canary_holds(now) && (at_wall || theirs.fresh());
            return if take {
                onto_active(active, true)
            } else {
                Advice::Stay(StayWhy::Canary)
            };
        }
        if theirs.fresh() || (theirs.runs() && state.pinned) {
            return onto_active(active, false);
        }
        if theirs.runs() {
            low_active = Some(active);
        }
    }

    // The owner's clearing, between rules 2 and 3: the row the session
    // stands at is the wall the owner lifted, and nothing has been recorded
    // against the account since.
    if let Point::Wall {
        at: Some(written), ..
    } = point
        && let Some(record) = state.accounts.get(&mine.label)
        && let Some(cleared_at) = record.cleared_at
        && record.cleared_since(*written, now)
        && cond(mine).runs()
    {
        return Advice::Resume {
            cleared_at: cleared_at.min(now),
        };
    }

    // The login the session lost, signed back in since it was lost:
    // relaunched under it — behind the canary of an `active` that is this
    // very account and has not answered yet.
    if let Point::LostLogin { seen } = *point
        && cond(mine).runs()
        && state.accounts.get(&mine.label).is_some_and(|a| {
            a.auth == Auth::Ok && a.auth_at.is_some_and(|signed_in| signed_in > seen)
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
        };
    }

    let breaker = breaker_until(state, roster, now);
    let listed = candidates(roster, state, enrolled, Some(&mine.label), point, now);
    // …and only where a low account is a candidate at all: at a wall, with
    // no account about to become usable.
    let low_active =
        low_active.filter(|a| listed.iter().any(|(l, rank)| *l == a.label && rank.is_ok()));
    let next = listed
        .into_iter()
        .find_map(|(label, verdict)| verdict.ok().map(|rank| (label, rank)));

    // Rule 3.
    if at_wall {
        return match (next, low_active, breaker) {
            // The active account, low as it is, is where the others already
            // are, and moving onto it rotates nothing: it is taken when only
            // low accounts are left, and — rather than wait a wall out —
            // when the hour's rotations are spent.
            (Some((_, rank)), Some(active), spent) if rank.low || spent.is_some() => {
                onto_active(active, false)
            }
            (Some(_), _, Some(until)) => Advice::Stay(StayWhy::Breaker { until }),
            (Some((to, _)), _, None) => Advice::Move {
                to,
                ahead: false,
                rotates: true,
                takes_canary: false,
            },
            (None, ..) => {
                let walled = Instants::WalledOn(&mine.label);
                match wait_instant(roster, state, enrolled, walled, now) {
                    Some(until) if until > now => Advice::Wait { until },
                    _ => Advice::Stay(StayWhy::NoCandidate),
                }
            }
        };
    }

    // Rules 4 and 5.
    if mode != Mode::Early {
        return Advice::Stay(StayWhy::NothingDue);
    }
    let account = state.account(&mine.label);
    let own = cond(mine);
    let limited = matches!(own, Condition::Limited { .. });
    // An account to leave ahead of its wall: one a wall already stands on,
    // or one low by a window that is not about to reset.
    let leave = limited
        || (own == Condition::Low && account.sample.as_ref().is_some_and(|s| s.leave_early(now)));
    if state.pinned {
        return Advice::Stay(if leave {
            StayWhy::Pinned
        } else {
            StayWhy::NothingDue
        });
    }
    // The session on the account a rotation chose, before that account has
    // answered: the canary, or a session standing where one stood. It is
    // not moved on ahead of a wall, and it is NOT PROBED either: its first
    // turn under the new login has not been served, so what a `/usage` read
    // would show there is not known to be that account's (the rotation's
    // model reads no sample of an unproven active account: its `Low…` steps
    // exclude it).
    let unanswered =
        !state.proven && state.active.as_deref() == Some(mine.label.as_str()) && own.runs();
    if probes
        && roster.rows.len() > 1
        && !limited
        && !unanswered
        && !probe_held(state, &mine.label, now)
        && probe_due(&account, now)
    {
        return Advice::Probe;
    }
    if !leave {
        return Advice::Stay(StayWhy::NothingDue);
    }
    if offer_expiring(&account, now) {
        return Advice::Stay(StayWhy::OfferExpiring);
    }
    match (next, breaker) {
        (None, _) => Advice::Stay(StayWhy::NoFresh),
        (Some(_), _) if unanswered => Advice::Stay(StayWhy::Canary),
        (Some(_), Some(until)) => Advice::Stay(StayWhy::Breaker { until }),
        (Some((to, _)), None) => Advice::Move {
            to,
            ahead: true,
            rotates: true,
            takes_canary: false,
        },
    }
}

/// THE LOGIN LIFECYCLE'S ADVICE (design v2 §5.3), whatever the rotation's
/// mode, at a point [`advise`] reads as the logins': a lost login with no
/// move behind it ([`Point::LoginEnded`]), and any point but a usage wall of
/// a session still on a login a renewal superseded ([`Me::left`]).
///
/// * ON A SUPERSEDED LOGIN, the row's renewed login signed in (`cond` is
///   neither `signed-out` nor `not-enrolled`): `Move` to the session's own
///   label — a follow, nothing rotates and no canary is taken — so it is
///   relaunched under the renewed login: at its lost login, with the wall's
///   resume after it; at an idle turn end ahead of any wall (the same
///   account and organization, so the prompt cache holds, and nothing is
///   typed after the relaunch). WHATEVER THE ACCOUNT'S USAGE: a limit is the
///   account's and the same under both logins, so it never holds the follow;
///   the superseded login only ends sooner. A wall that holds the session's
///   model (an account-wide window, or that model's own bucket) answers the
///   resume under the renewed login, as a usage wall its own rule takes;
///   another model's bucket holds nothing. NEVER AT A USAGE LIMIT: a host
///   asks about a limit that is on the screen alone (no row yet, or the
///   process before this one's) as a plain turn end, and asks about a
///   session at any usage limit as its account, not its login ([`Me::left`]
///   folded into [`Me::account`]) — a relaunch onto the renewed login there
///   would type the resume into the same limit — so the rules below reach
///   it as they reach any session on the account, and the follow is made at
///   the next idle turn end.
/// * ELSE, at a lost login: `Stay` with no candidate — never another
///   account (a rotation between accounts waits for R-5); only a renewal
///   ends it. At a turn end: nothing is due.
fn login_follow(mine: &Row, me: &Me<'_>, point: &Point, cond: Condition) -> Advice {
    let at_wall = !matches!(point, Point::TurnEnd);
    let signed_in = !matches!(cond, Condition::SignedOut | Condition::NotEnrolled);
    if me.left.is_some() && signed_in {
        return Advice::Move {
            to: mine.label.clone(),
            ahead: !at_wall,
            rotates: false,
            takes_canary: false,
        };
    }
    Advice::Stay(if at_wall {
        StayWhy::NoCandidate
    } else {
        StayWhy::NothingDue
    })
}

// ---------------------------------------------------------------------------
// 5. The ledger event
// ---------------------------------------------------------------------------

/// What a ledger row records.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A wall was recorded for an account.
    Wall,
    /// A `/usage` sample was recorded.
    Sample,
    /// `active` was rotated: one session moves alone. Also the row of a
    /// session taking a silent canary's place ([`take_canary`]): `to` is
    /// then the account itself, which did not change.
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
    /// A tapped session's usage tap went silent (`super::tap::Silence`): its
    /// process served a turn and wrote no reading. A record; nothing moves.
    NoTap,
    /// Usage credits began carrying an account's sessions ([`record_usage`]).
    Credits,
    /// Claude Code warned that a window is filling ([`record_usage`]).
    Warned,
    /// A login was renewed: its check answered the row's account, and the
    /// row is bound to the new login ([`note_renewed`]).
    Renewed,
    /// A bound login was seen ended, at a login wall in a session on it
    /// ([`note_login_end`]).
    LoginEnded,
    /// A renewal's check answered another account than the row's; nothing
    /// live changed ([`note_wrong_account`]).
    WrongAccount,
}

impl Kind {
    /// Every kind, in the design's order.
    pub const ALL: [Kind; 19] = [
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
        Kind::LoginEnded,
        Kind::WrongAccount,
    ];

    /// The ledger's word.
    #[must_use]
    pub fn as_str(self) -> &'static str {
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
            Kind::LoginEnded => "login-ended",
            Kind::WrongAccount => "wrong-account",
        }
    }

    /// One word back; anything else is no kind.
    #[must_use]
    pub fn parse(word: &str) -> Option<Kind> {
        Kind::ALL.into_iter().find(|k| k.as_str() == word)
    }
}

/// One row of `ledger.jsonl`.
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
    /// The reason, one bounded line. Empty when there is none.
    pub why: String,
}

impl Event {
    /// A row about `account`.
    #[must_use]
    pub fn new(t: i64, kind: Kind, account: &str) -> Event {
        Event {
            t,
            kind,
            account: one_line(account, LABEL_MAX),
            to: None,
            sid: None,
            why: String::new(),
        }
    }

    /// With the account moved or rotated to.
    #[must_use]
    pub fn with_to(mut self, label: &str) -> Event {
        self.to = Some(one_line(label, LABEL_MAX)).filter(|l| !l.is_empty());
        self
    }

    /// With the session concerned.
    #[must_use]
    pub fn with_sid(mut self, sid: &str) -> Event {
        self.sid = Some(one_line(sid, WORD_MAX)).filter(|s| !s.is_empty());
        self
    }

    /// With its reason.
    #[must_use]
    pub fn because(mut self, why: &str) -> Event {
        self.why = one_line(why, WHY_MAX);
        self
    }

    /// Whether this row makes an account usable sooner, or changes whom a
    /// waiting session should follow: the caller touches the wake marker
    /// ([`Store::touch_wake`]) so a tab waiting until Thursday decides again.
    /// EVERY reset outcome does: a reset used or a limit found lifted ends
    /// the account's walls, and one kept or failed ends the hold a reset in
    /// flight put on every move off the account ([`reset_in_flight`]). So
    /// does a renewal: a session stopped on the login it replaced moves now.
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
            | Kind::Renewed => true,
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
            | Kind::WrongAccount => false,
        }
    }

    /// The row as one line of JSON (no line break), keys sorted, every
    /// string bounded and folded onto one line.
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
        o.insert("why".to_owned(), Value::from(one_line(&self.why, WHY_MAX)));
        aterm_json::to_string(&Value::Object(o)).unwrap_or_else(|_| String::from("{}"))
    }

    /// One ledger line back. `None` for anything that is not a row of this
    /// ledger: a line longer than [`MAX_EVENT_LINE_BYTES`], text that is not
    /// a JSON object, no `t`, a kind outside the list, no account.
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
            why: word(doc.get("why"), WHY_MAX).unwrap_or_default(),
        })
    }
}

// ---------------------------------------------------------------------------
// 6. Transactions — pure edits of a `State`, answering the ledger's rows
// ---------------------------------------------------------------------------

/// The record of `label`, made if absent. `None` for a label no roster
/// could carry. A state already holding [`MAX_ACCOUNTS`] others forgets the
/// one written longest ago.
fn account_mut<'a>(state: &'a mut State, label: &str) -> Option<&'a mut Account> {
    if !admissible_label(label) {
        return None;
    }
    if !state.accounts.contains_key(label) && state.accounts.len() >= MAX_ACCOUNTS {
        let stalest = state
            .accounts
            .iter()
            .min_by_key(|(_, a)| a.touched())
            .map(|(k, _)| k.clone())?;
        state.accounts.remove(&stalest);
    }
    Some(state.accounts.entry(label.to_owned()).or_default())
}

/// End the pin when `account` is the pinned one, with its ledger row.
fn end_pin(state: &mut State, account: &str, why: &str, t: i64, out: &mut Vec<Event>) {
    if state.pinned && state.active.as_deref() == Some(account) {
        state.pinned = false;
        out.push(Event::new(t, Kind::Unpin, account).because(why));
    }
}

/// The wall of `walls` that stops standing first: the one a ninth wall
/// takes the place of.
fn most_lapsed(walls: &BTreeMap<String, Wall>) -> Option<String> {
    walls
        .iter()
        .min_by_key(|(_, w)| w.stands_until())
        .map(|(k, _)| k.clone())
}

/// The reset a wall observed at `at` is recorded with: the one it named,
/// unless that stands more than [`RESET_BELIEVED_S`] past the observation.
/// The longest window the vendor names is a week, so a reset further off is
/// no window's end — a row misread, or a clock that was wrong when it was
/// written — and believing it would read the account `limited` for as long
/// as it said. Such a wall names NO reset, and stands on that ladder
/// ([`NO_RESET_WALL_S`]). A reset behind its observation is kept: that is a
/// wall found standing late ([`LATE_WALL_S`]).
#[must_use]
pub fn believed_reset(until: Option<i64>, at: i64) -> Option<i64> {
    until.filter(|until| until.saturating_sub(at) <= RESET_BELIEVED_S)
}

/// Record a wall `account` hit: the vendor's limit type, the reset it
/// named, when it was observed, and the caller's clock.
///
/// The SAME wall seen again while it stands is one observation and answers
/// no `wall` row (ten tabs at one wall write one row). Seen again once it
/// should have lifted it is found standing: `attempts` climbs, and with it
/// the rung it stands on ([`Wall::stands_until`]). An observation older than
/// the one held is dropped — the one held being read as made no later than
/// `now`: a stamp ahead of the clock is brought back to it first, or it
/// would shadow every wall observed until the clock caught up.
///
/// A wall on the pinned account ends the pin, whichever of those it was:
/// seeing a wall there again says the same thing as seeing it first.
///
/// THE OWNER'S CLEARING STANDS, as in [`merge_observations`]: a wall
/// observed BEFORE the pin cleared the account's walls
/// ([`Account::cleared_at`]) is the wall the owner cleared. It is dropped
/// and ends no pin — a session left standing at that wall still has the row
/// in its transcript, and a look at it ([`record_wall`]) would otherwise
/// put the wall back and end the owner's pin with it.
///
/// A RESET TOO FAR OFF IS NOT BELIEVED ([`believed_reset`]): one more than
/// [`RESET_BELIEVED_S`] past the observation is recorded as no reset, here,
/// so that the recorder, the host's commit and a strike — which all hand
/// over the same row — hold one wall and not two that replace each other.
pub fn note_wall(
    state: &mut State,
    account: &str,
    window: &str,
    until: Option<i64>,
    at: i64,
    now: i64,
) -> Vec<Event> {
    let window = one_line(window, WORD_MAX);
    let until = believed_reset(until, at);
    let by = state.machine.clone();
    let mut out = Vec::new();
    if window.is_empty() {
        return out;
    }
    let Some(record) = account_mut(state, account) else {
        return out;
    };
    if record.cleared_since(at, now) {
        return out;
    }
    let late = u32::from(until.is_none_or(|u| at > u));
    if let Some(old) = record.walls.get_mut(&window) {
        old.at = old.at.min(now);
    }
    // `Some(attempts)`: a wall to write, with its row. `None`: one already
    // held, newer or still standing.
    let attempts = match record.walls.get(&window) {
        Some(old) if at < old.at => None,
        Some(old) if old.until == until => {
            let end = old.stands_until();
            if at < end {
                None
            } else if at.saturating_sub(end) > WALL_EPISODE_S {
                Some(late)
            } else {
                Some(old.attempts.saturating_add(1))
            }
        }
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

/// Record a settled `/usage` panel read on `account`, at the caller's clock
/// `now`. The sample held becomes the one before it; a sample no newer than
/// the one held, or one with neither figure, is dropped. `sample.by` is
/// stamped here.
///
/// "No newer" is against the one held read as made no later than `now`: a
/// sample stamped ahead of the clock (a clock set back) is older than
/// anything read now, or every real sample would be dropped for as long as
/// the clock is behind it.
pub fn note_sample(state: &mut State, account: &str, sample: Sample, now: i64) -> Vec<Event> {
    let by = state.machine.clone();
    let sample = Sample {
        session_pct: sample.session_pct.filter(|p| *p <= PCT_MAX),
        week_pct: sample.week_pct.filter(|p| *p <= PCT_MAX),
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
    let shown = |pct: Option<u32>| pct.map_or_else(|| "?".to_string(), |p| p.to_string());
    let event = Event::new(sample.at, Kind::Sample, account).because(&format!(
        "session {}% week {}%",
        shown(sample.session_pct),
        shown(sample.week_pct)
    ));
    record.before = record.sample.replace(sample);
    vec![event]
}

/// How long a usage tap's unchanged figure is held before it is written
/// again ([`note_reading`]): the heartbeat that keeps a login read as served
/// while its figures stand still, without a write per reply.
pub const READING_HEARTBEAT_S: i64 = 15 * 60;

/// One window of a [`note_reading`]: the figure and the reset it counts to,
/// both read, or none.
fn tap_window(pct: Option<u32>, reset: Option<i64>) -> Option<(u32, i64)> {
    pct.filter(|p| *p <= PCT_MAX).zip(reset)
}

/// THE ACCOUNT'S FIGURE FOR ONE WINDOW (the accounts design v2, §3.2): the
/// LARGEST reading that shares the newest reset. A held figure whose reset
/// has passed at `now` is void; a reading for an older window than the one
/// held is never taken; within one window the larger figure stands — usage
/// only grows there, so a process that was not served since cannot make a
/// spent account look fresh.
fn tap_merge(held: Option<(u32, i64)>, read: Option<(u32, i64)>, now: i64) -> Option<(u32, i64)> {
    match (held.filter(|(_, reset)| *reset > now), read) {
        (None, read) => read,
        (held, None) => held,
        (Some((pct, reset)), Some((r_pct, r_reset))) => Some(if r_reset > reset {
            (r_pct, r_reset)
        } else if r_reset == reset {
            (pct.max(r_pct), reset)
        } else {
            (pct, reset)
        }),
    }
}

/// Record a usage tap's FRESH reading of `account` (`super::tap`; the
/// accounts design v2, §3.2) at the caller's clock `now`, by the max rule
/// ([`tap_merge`]) against the sample held. The state changes — the sample
/// held becomes the one before it, and the ledger gets a `sample` row — ONLY
/// where a figure or a reset changed, or [`READING_HEARTBEAT_S`] after the
/// held sample was taken; anything else writes nothing, so a machine of
/// many sessions replying many times a minute rewrites neither its state
/// nor its ledger at each reply. A reading with neither window is dropped.
/// `sample.by` is stamped here and `sample.at` is `now`. The `/usage`
/// probe's [`note_sample`] is not this: its panel is the account's settled
/// figure, taken whole.
pub fn note_reading(state: &mut State, account: &str, sample: Sample, now: i64) -> Vec<Event> {
    let read_session = tap_window(sample.session_pct, sample.session_reset);
    let read_week = tap_window(sample.week_pct, sample.week_reset);
    if read_session.is_none() && read_week.is_none() {
        return Vec::new();
    }
    let by = state.machine.clone();
    let Some(record) = account_mut(state, account) else {
        return Vec::new();
    };
    let held = record.sample.as_ref();
    let held_session = held.and_then(|s| tap_window(s.session_pct, s.session_reset));
    let held_week = held.and_then(|s| tap_window(s.week_pct, s.week_reset));
    let session = tap_merge(held_session, read_session, now);
    let week = tap_merge(held_week, read_week, now);
    let changed = (session, week) != (held_session, held_week);
    let beat = held.is_none_or(|s| now.saturating_sub(s.at) >= READING_HEARTBEAT_S);
    if !changed && !beat {
        return Vec::new();
    }
    let shown = |w: Option<(u32, i64)>| w.map_or_else(|| "?".to_string(), |(p, _)| p.to_string());
    let event = Event::new(now, Kind::Sample, account).because(&format!(
        "tap: session {}% week {}%",
        shown(session),
        shown(week)
    ));
    let next = Sample {
        session_pct: session.map(|(p, _)| p),
        session_reset: session.map(|(_, r)| r),
        week_pct: week.map(|(p, _)| p),
        week_reset: week.map(|(_, r)| r),
        at: now,
        by,
    };
    record.before = record.sample.replace(next);
    vec![event]
}

/// Record a reset offer seen at a wall on `account`, at the caller's clock
/// `now`. Whether it was recorded: an offer older than the one held is not.
/// No ledger row: the ledger's reset rows are the outcomes.
///
/// WHAT A CALLER HANDS IT IS AN OFFER THAT WAS READ AS ONE: the count and
/// the deadline of a reset confirmation read exactly, or the count the
/// vendor's own `reset` line names. The wall dialog's row is not one — it
/// names a day, and a day is no deadline ([`OFFER_EXPIRING_S`] is asked of
/// an instant).
///
/// "Older" is against the one held read as made no later than `now`, as in
/// [`note_sample`] and [`note_wall`]: an offer stamped ahead of the clock
/// would otherwise shadow every offer seen until the clock caught up.
pub fn note_offer(
    state: &mut State,
    account: &str,
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
        left,
        use_by,
        at,
        by,
    });
    true
}

/// Record what the check after a sign-in, a failed first login, a served
/// response or a login wall right after a move onto it said of `account`'s
/// login. [`Auth::Unseen`] is not something to observe and is dropped, as is
/// a word older than the one held. A row only when the word CHANGED. A
/// pinned account found signed out ends the pin.
pub fn note_auth(state: &mut State, account: &str, auth: Auth, at: i64) -> Vec<Event> {
    let mut out = Vec::new();
    if auth == Auth::Unseen {
        return out;
    }
    let Some(record) = account_mut(state, account) else {
        return out;
    };
    if record.auth_at.is_some_and(|held| at < held) {
        return out;
    }
    let changed = record.auth != auth;
    record.auth = auth;
    record.auth_at = Some(at);
    if changed {
        out.push(Event::new(at, Kind::Auth, account).because(auth.as_str()));
        if auth == Auth::SignedOut {
            end_pin(state, account, "signed-out", at, &mut out);
        }
    }
    out
}

/// Record which account the session `sid` runs on (read from its process's
/// own environment), and why it could not move where a move was refused. A
/// `move` row when it is on another account than last read. The
/// [`MAX_SESSIONS`] newest are kept. The account is stamped as run on at
/// `now` ([`Account::ran_at`], never moved back): the one record of a login
/// having run that outlives the session's row.
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
    while state.sessions.len() > MAX_SESSIONS {
        let Some(oldest) = state
            .sessions
            .iter()
            .min_by(|a, b| a.1.at.cmp(&b.1.at).then_with(|| a.0.cmp(b.0)))
            .map(|(k, _)| k.clone())
        else {
            break;
        };
        state.sessions.remove(&oldest);
    }
    out
}

/// Forget the session `sid` (its agent ended). Whether anything was kept of
/// it. When it is the canary its hold ends at once: nobody is left to
/// answer for the account, and the next session takes its place
/// ([`take_canary`]). The rotation it made stands, with nobody left to take
/// it back — and where a later rotation went past its account, its record
/// is not what taking THAT one back restores ([`State::undo`]). No id is no
/// session: nothing is forgotten for it, a canary record that names none
/// included.
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
        // The undos below are of the rotations it made: nobody's now.
        state.undo.drain(..k);
        if let Some(over) = state.undo.first_mut() {
            over.canary = None;
        }
    }
    state.sessions.remove(sid).is_some() || canary || passed.is_some()
}

/// Drop every session row last written more than [`SESSION_KEEP_S`] before
/// `now`; how many were. A session's end forgets its row
/// ([`forget_session`]), and one that ended with no word — a crash, a
/// machine switched off — leaves it: the report would name that tab for
/// ever. A row stamped ahead of the clock is not old. Only the row goes: a
/// canary's record is the hold's, and lapses by its own clock. Called where
/// the state is saved ([`Store::transact`]), so reading a state drops
/// nothing and a transaction that changes nothing writes nothing.
pub fn drop_stale_sessions(state: &mut State, now: i64) -> usize {
    let before = state.sessions.len();
    state
        .sessions
        .retain(|_, row| now.saturating_sub(row.at) <= SESSION_KEEP_S);
    before - state.sessions.len()
}

/// Whether the standing claim holds `account`'s probe off at `now`: it is
/// this account's and younger than [`PROBE_MIN_S`], or ANOTHER account's and
/// younger than [`PROBE_APART_S`]. The one test [`advise`] and
/// [`claim_probe`] share, so the advice never says `Probe` where the claim
/// would be refused. A claim stamped ahead of the clock holds nothing off
/// (a clock set back must not silence the probe for as long as it was
/// behind).
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

/// Claim the one `/usage` probe for `account`: `Some` (its ledger row) when
/// no claim stands in its way ([`probe_held`]: none for that account
/// younger than [`PROBE_MIN_S`], none for any other younger than
/// [`PROBE_APART_S`]), `None` when a session holds it. The state keeps ONE
/// claim ([`State::probe`]): a claim taken for another account writes over
/// this one's, which [`PROBE_APART_S`] being half of [`PROBE_MIN_S`] makes
/// harmless (the account is then held off by the newer claim until its own
/// floor has passed).
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

/// What a reset claim was answered ([`claim_reset`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResetClaimed {
    /// The slot is this session's from now: it may take the row.
    Taken,
    /// It already was: this session and this process claimed this wall, and
    /// the claim is still in flight. Nothing was written.
    Mine,
    /// A claim is in flight that is not this session's own for this wall —
    /// another session's, another process's of this session, or one for
    /// another wall or account — until this instant at the latest
    /// ([`RESET_CLAIM_S`] after it was made). Nothing was written.
    Held {
        /// When that claim lapses.
        until: i64,
    },
    /// This wall was attempted already and is not attempted again: its claim
    /// has an outcome, or lapsed with none, or the machine's last attempt
    /// was at a wall that still stands past this one. Also the answer to a
    /// claim nobody could hold (no session id, no process, a label no roster
    /// carries). Nothing was written.
    Attempted,
}

/// CLAIM THE ONE LIMIT-RESET ATTEMPT for the wall `account` stands at (its
/// own reset, `until`), for the session `sid` and its process (`pid`, and
/// its kernel `start` as the host reads it). The host calls it LAST — after
/// the loop's gates, the person's hold and [`reset_allowed`], in the
/// transaction that precedes the row's first key — and only
/// [`ResetClaimed::Taken`] and [`ResetClaimed::Mine`] let a key be written.
///
/// The state keeps ONE slot ([`State::reset`]), and it is the record of the
/// attempt for as long as it stays: once a wall's claim has an outcome
/// ([`reset_outcome`]) or is older than [`RESET_CLAIM_S`], that wall is
/// [`ResetClaimed::Attempted`] for every session, the claimer included — a
/// kept, failed or unread attempt is final, and a wall is never tried twice.
///
/// A claim IN FLIGHT (no outcome, younger than [`RESET_CLAIM_S`]) is never
/// written over, whichever wall or account it names: its outcome is still to
/// be recorded against it, and two resets must not be in flight at once.
/// A claim that is done or lapsed is written over by a claim for ANOTHER
/// wall — except by one for a wall that ends BEFORE the attempted wall,
/// while that wall still stands by its own reset at `now`: with one slot, a
/// wall met beside the attempted one would otherwise take the record's place
/// and leave the first free to be tried again.
///
/// A WALL IS THE SAME WALL BY ITS RESET, WHATEVER LABEL THE CLAIM WAS MADE
/// UNDER. The slot is this machine's own login's — no other row may claim
/// ([`reset_allowed`]) — and that login's label is the roster's to change:
/// [`Roster::own`]'s where no roster is written, the owner's word once one
/// is. Asked by the label, a wall attempted under the first was free to be
/// attempted again under the second. [`ResetClaim::account`] is who the
/// outcome is recorded for ([`reset_outcome`]) and who is held meanwhile
/// ([`reset_in_flight`]).
///
/// A claim stamped ahead of the clock is not in flight (a clock set back
/// must not hold every session for as long as it was behind): it reads as
/// lapsed. No ledger row: the ledger's reset rows are the outcomes.
pub fn claim_reset(
    state: &mut State,
    account: &str,
    until: i64,
    sid: &str,
    pid: u32,
    start: &str,
    now: i64,
) -> ResetClaimed {
    let sid = one_line(sid, WORD_MAX);
    let start = one_line(start, WORD_MAX);
    if sid.is_empty() || start.is_empty() || pid == 0 || !admissible_label(account) {
        return ResetClaimed::Attempted;
    }
    if let Some(held) = &state.reset {
        let flying = held.done.is_none() && recent(held.at, now, RESET_CLAIM_S);
        let this_wall = held.until == Some(until);
        let mine = held.sid == sid && held.pid == pid && held.start == start;
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
        sid,
        at: now,
        pid,
        start,
        done: None,
    });
    ResetClaimed::Taken
}

/// Whether a reset is IN FLIGHT on `account` at `now`, as the instant its
/// claim lapses: the slot names the account, has no outcome and is younger
/// than [`RESET_CLAIM_S`]. While it is, nobody is moved off the account: the
/// reset may be about to end its walls for every session on it, and a
/// rotation made meanwhile would take the machine off an account just
/// refilled. `None` once the outcome is recorded or the claim has lapsed,
/// and for a claim stamped ahead of the clock.
#[must_use]
pub fn reset_in_flight(state: &State, account: &str, now: i64) -> Option<i64> {
    state
        .reset
        .as_ref()
        .filter(|c| c.account == account && c.done.is_none() && recent(c.at, now, RESET_CLAIM_S))
        .map(|c| c.at.saturating_add(RESET_CLAIM_S))
}

/// WHETHER A RESET MAY BE CLAIMED AT ALL for a process on the account
/// `label` that started at `started` (Unix seconds): only on THIS MACHINE'S
/// OWN LOGIN, and only where no other login can have moved the home's
/// profile. READ in the 2.1.287 bundle: the vendor posts the claim to the
/// HOME PROFILE's organization, whichever login the process holds, and the
/// profile follows a process that started when it was over 24 h old.
///
/// `Ok` needs ALL of:
///
/// * `label` is a row of `roster` (`not-listed`) with no `dir`
///   (`not-default`): the login this machine already has.
/// * [`State::active`] is that row or nothing (`active-other`): a rotation
///   or a pin has put the machine on another account.
/// * NO OTHER account was on record running later than
///   [`RESET_PROFILE_S`] before `started` (`other-login`): its
///   [`Account::ran_at`], and the row of any session still on record on it.
///   One that ran AFTER the start counts too — the profile may have turned
///   24 h old under this process. The accounts asked are the state's, not
///   the roster's: a row the owner has since removed ran all the same.
///
/// The refusal is one word for a log, as quoted above. PURE, and NOT a
/// proof: a login started by hand, or one whose session nobody read, is on
/// no record here.
///
/// # Errors
///
/// The first of those that fails, by its word.
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

/// How a claimed reset ended ([`reset_outcome`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResetDone<'a> {
    /// The vendor's own line said the limits were reset: a reset was SPENT.
    /// `left` is the count that line named, where it named one.
    Used {
        /// Resets left after this one.
        left: Option<u32>,
    },
    /// The vendor said the limit had lifted by itself: nothing was spent,
    /// and the wall is over all the same.
    Lifted,
    /// The reset was kept, for this reason (one word of the caller's).
    Kept(&'a str),
    /// The attempt failed or its end was not read, for this reason.
    Failed(&'a str),
}

impl ResetDone<'_> {
    /// The word [`ResetClaim::done`] keeps.
    #[must_use]
    pub fn word(&self) -> &'static str {
        match self {
            ResetDone::Used { .. } => "used",
            ResetDone::Lifted => "lifted",
            ResetDone::Kept(_) => "kept",
            ResetDone::Failed(_) => "failed",
        }
    }
}

/// The reason a kept or failed reset's row carries, from the caller's word:
/// one line of at most [`WORD_MAX`] bytes, cut at the first ` · ` (a vendor
/// line's head and no more) and at the first character a path or an address
/// is written with. A reason is a word about the attempt; this keeps a line
/// quoted from a screen or a transcript from carrying anything else into
/// the ledger.
fn reset_reason(why: &str) -> String {
    let line = one_line(why, WORD_MAX);
    let head = line.split(" · ").next().unwrap_or_default();
    let end = head.find(['/', '\\', '@', '~']).unwrap_or(head.len());
    head.get(..end).unwrap_or_default().trim().to_owned()
}

/// RECORD HOW THE CLAIMED RESET ENDED, at the caller's clock `now`: the one
/// ledger row of the attempt, with the session and one bounded reason — the
/// count left, `lifted`, or the caller's word ([`reset_reason`]); never a
/// path or a name.
///
/// NOTHING unless the slot is THIS claim — `account`, the wall's `until` and
/// `sid` — with no outcome yet: an outcome is recorded once, by the session
/// that holds the claim, and a session that does not hold it changes
/// nothing. Whether the process in the tab is still the one the claim was
/// made for ([`ResetClaim::pid`], [`ResetClaim::start`]) is the host's to
/// check before it calls. A claim older than [`RESET_CLAIM_S`] still takes
/// its outcome: lapsing frees the other sessions, not the record.
///
/// The claim keeps the outcome's word ([`ResetClaim::done`]) and stays, so
/// the wall is not attempted again ([`claim_reset`]). The row:
///
/// * `reset-used` for [`ResetDone::Used`];
/// * `reset-kept` for [`ResetDone::Lifted`] (reason `lifted`) and for
///   [`ResetDone::Kept`];
/// * `reset-failed` for [`ResetDone::Failed`].
///
/// USED AND LIFTED END THE ACCOUNT'S WALLS FOR EVERYONE, as the owner's pin
/// does ([`pin`]): the walls are cleared and the instant kept
/// ([`Account::cleared_at`]). So a row still standing in a transcript does
/// not put the wall back ([`note_wall`]), and a session standing at that row
/// is advised to continue where it is ([`Advice::Resume`]). EACH WALLED
/// WINDOW is said refilled at `now` ([`WindowSaid`], the newest word on it):
/// the refill names the limit it refills (`Refills your weekly limit now`),
/// and Lifted says that limit had reset on its own; kept, a week refilled
/// stayed `nearly used` in the digest until its old reset ([`windows_full`]),
/// and a warning drawn before the reset and said again changes nothing. A
/// window is walled when its wall stood as the reset was claimed
/// ([`ResetClaim::at`], [`Wall::stands_until`]) — the claimed wall always
/// did, though an outcome may be read after it lapsed. A lapsed wall stays
/// among the account's walls until they are cleared or a ninth takes its
/// place: one from last week walls nothing now. A window that was not walled
/// keeps its word. Kept and failed change no wall.
/// `active`, the pin and the proof are left as they are.
/// Every row wakes the waiting sessions ([`Event::wakes`]).
///
/// EVERY wall of the account, whichever limits the grant refilled: the
/// clearing is one instant an account, as the pin's is, and neither the
/// walled window nor the limits a confirmation named reach this function.
/// Where a limit the reset did not refill still stands, a session at it is
/// advised to continue once, or one on another account is moved here, and
/// the wall answers; it is then recorded again from its new row. That costs
/// one continuation or one relaunch, never a reset.
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
    let (kind, why) = match done {
        ResetDone::Used { left } => (
            Kind::ResetUsed,
            left.map(|n| format!("{n} left")).unwrap_or_default(),
        ),
        ResetDone::Lifted => (Kind::ResetKept, ResetDone::Lifted.word().to_owned()),
        ResetDone::Kept(why) => (Kind::ResetKept, reset_reason(why)),
        ResetDone::Failed(why) => (Kind::ResetFailed, reset_reason(why)),
    };
    if matches!(done, ResetDone::Used { .. } | ResetDone::Lifted)
        && let Some(record) = account_mut(state, account)
    {
        let refilled = Some(WindowSaid {
            pct: None,
            reset: None,
            at: now,
        });
        let walled = |limit: &str| {
            record
                .walls
                .get(limit)
                .is_some_and(|wall| claimed < wall.stands_until())
        };
        let (week, session) = (walled(WEEK_LIMIT), walled(SESSION_LIMIT));
        if week {
            record.warned_week = refilled.clone();
        }
        if session {
            record.warned_session = refilled;
        }
        record.walls.clear();
        record.cleared_at = Some(now);
    }
    vec![Event::new(now, kind, account).with_sid(&sid).because(&why)]
}

/// Whether the machine's rotation bound is spent: at least twice the roster
/// size rotations within the last [`ROTATION_WINDOW_S`]. With no roster it
/// is spent: nothing rotates.
#[must_use]
pub fn rotations_spent(state: &State, roster: &Roster, now: i64) -> bool {
    let counted = state
        .rotations
        .iter()
        .filter(|t| recent(**t, now, ROTATION_WINDOW_S))
        .count();
    counted >= roster.rows.len().saturating_mul(2)
}

/// When a spent rotation bound has room again: the oldest rotation it counts
/// plus [`ROTATION_WINDOW_S`] (with more counted than the bound, as after a
/// row is removed, the instant enough of them have lapsed). `None` while the
/// bound is not spent, and with no roster, which no clock changes.
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

/// Why a rotation was not begun.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lost {
    /// `active` is not what the caller read: another session got there
    /// first. Advise again on the state as it is now.
    Raced {
        /// What `active` is.
        active: Option<String>,
    },
    /// `active` as read can be run on and is not rotated past: it has not
    /// served a response yet (whoever asks), or it is another account that
    /// is fresh or pinned (it is followed), or low with no fresh account
    /// named to go to instead.
    Held {
        /// That account.
        active: String,
    },
    /// The owner's pin holds.
    Pinned,
    /// `to` is not a row, is the session's own or the active account, or
    /// cannot be run on now.
    NotACandidate,
    /// The machine's rotation bound is spent.
    Spent,
    /// The mover has no session id: it could be no canary, and nobody could
    /// answer, or be forgotten, for the account it chose.
    NoSession,
}

/// The session a rotation moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mover<'a> {
    /// Its session id: the canary's, once the rotation is committed.
    pub sid: &'a str,
    /// The account its process runs on.
    pub account: &'a str,
}

/// Begin a rotation: the compare-and-set of design §6. The session `mover`
/// moves ALONE to `to`: `active = to`, `proven = false`, the mover is the
/// canary ([`State::canary`]), and the others follow only once [`prove`]
/// says it served a response.
///
/// It is allowed when `active` is still `expect_active` (what the caller's
/// advice read), and that is nothing, an account that cannot be run on, the
/// mover's own once proven, or — a proven account being left for a better
/// one — another account that is `low` and not pinned while `to` is fresh
/// (a wall on the mover's account, or its own low turn end, with the active
/// account nearly spent too); when no pin holds on an account that can be
/// run on (a pin left on one that cannot ends with the rotation); when `to`
/// is a row that can be run on; and while the machine's bound is not spent,
/// toward which it counts.
///
/// Nobody rotates past an account that can be run on and has not served a
/// response yet, the session that chose it included: that account has one
/// session on it and no answer, and a second rotation would leave it with
/// neither. (What it leaves behind is safe either way: `previous` carries
/// whether it was proven, and [`strike`] restores exactly that.)
///
/// A rotation [`advise`] gave at a [`Point::Wall`] is committed on the state
/// WITH THAT WALL RECORDED ([`note_wall`], in the same transaction): this
/// reads the state, where a mover's own account that is the active one can
/// then not be run on, and a pin on it has ended with the wall.
///
/// What it writes over — `previous`, whether that one was proven, the
/// canary's record — is kept beside what it writes ([`State::undo`]), so a
/// relaunch the host refuses takes it back exactly ([`Strike::NotMoved`]),
/// and a candidate that answers badly leaves the account gone back to with
/// its own `previous`, proof and canary ([`strike`]).
///
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
            .map(|r| condition(r, state, is_enrolled(r, enrolled), now))
    };
    let runs = |label: &str| cond(label).is_some_and(Condition::runs);
    let fresh = |label: &str| cond(label).is_some_and(Condition::fresh);
    let active_runs = expect_active.is_some_and(runs);
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
    if to == me || expect_active == Some(to) || !runs(to) {
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
    let extra = state.rotations.len().saturating_sub(MAX_ROTATIONS);
    state.rotations.drain(..extra);
    Ok(Event::new(now, Kind::Rotate, &from)
        .with_to(to)
        .with_sid(&sid))
}

/// Take the place of a canary that did not answer: the compare-and-set on
/// the canary's record that [`Advice::Move`]'s `takes_canary` asks for. The
/// session `sid` becomes the canary of the account that is already `active`
/// — which does not change, so this is no rotation and counts toward no
/// bound — and its own hold starts at `now`.
///
/// `Some` (its ledger row: a `rotate` onto the active account itself) when
/// `active` has still not served a response, the canary on record is still
/// `expect` (what the caller's advice read) and its hold has lapsed
/// ([`State::canary_holds`]). `None`, with nothing changed, when any of
/// that moved — another session took it first, or the account answered:
/// advise again — and for a session with no id, which can hold no place.
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
    // The rotation was the silent canary's to take back, and it is gone.
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

/// `account` served a response. When it is the active account and not
/// proven yet, it is now — its row is the answer, and every other session
/// follows at its next point. The canary's work is done, whichever session
/// the response was served to. A served response also says the login works.
pub fn prove(state: &mut State, account: &str, now: i64) -> Option<Event> {
    if state.active.as_deref() != Some(account) || state.proven {
        return None;
    }
    state.proven = true;
    state.canary = None;
    state.undo.clear();
    if let Some(record) = account_mut(state, account) {
        record.auth = Auth::Ok;
        record.auth_at = Some(now);
    }
    Some(Event::new(now, Kind::Proven, account))
}

/// What became of a rotation's candidate instead of a served response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Strike {
    /// It answered with a usage wall of its own: it is `limited`.
    Wall {
        /// The vendor's limit type.
        window: String,
        /// The reset it named.
        until: Option<i64>,
    },
    /// It answered with a login wall: it is `signed-out`.
    SignedOut,
    /// The move failed with no word about the account (`failed:no-resume`,
    /// `failed:account-not-applied`): it is `limited` on the ladder of a
    /// wall naming no reset, so it is not chosen again at once.
    Failed(String),
    /// The move was never made: the host refused or deferred the relaunch
    /// (`wait:changed` for the agent's jobs, `refused:line`). Nothing is held
    /// against the account, and the rotation is not counted. It is the word
    /// of the one session that was to move: from any other it is no step.
    NotMoved(String),
}

/// Strike `account`, on the word of the session `sid`: record what became
/// of it, and — when it is the active account still unproven, the canary's —
/// put `active` back to the account it was rotated from, PROVEN OR NOT AS
/// THAT ACCOUNT WAS when it was left ([`State::previous_proven`]): one that
/// never served a response is not followed for having been gone back to.
/// Nobody else was relaunched.
///
/// THE ACCOUNT GONE BACK TO IS AS THE ROTATION FOUND IT. What the rotation
/// wrote over ([`State::undo`]: `previous`, whether that one was proven, the
/// canary's record) goes back with `active`, whatever the word was. So a
/// rotation made PAST an account that had not answered either (a wall
/// recorded on it with no strike), then struck, leaves that account with its
/// own `previous` and its own canary: a later strike of it goes back to the
/// account before it, proven as that one was, and the one session on it is
/// still the one that holds the others back for what is left of its ten
/// minutes. (A state written before the undo was kept, a rotation older
/// than [`MAX_UNDO`], and a canary that only took a silent one's place have
/// none to put back: `previous` and the canary's record are then none.)
///
/// A wall, a login wall and a failure are words about the ACCOUNT, whichever
/// session on it heard them: the rotation STAYS COUNTED against the
/// machine's bound, and the word stays on record ([`note_wall`],
/// [`note_auth`]). [`Strike::NotMoved`] is a word about ONE SESSION's
/// relaunch, and is acted on only from the session the canary's record names
/// ([`State::canary`]): from any other — another session took the place
/// since, the record is gone, `sid` is empty — and on an account that is not
/// the unproven active one, it is no step and answers no row.
///
/// From the session that made the rotation, [`Strike::NotMoved`] takes the
/// rotation back EXACTLY: all of the above, nothing held against the
/// account, and the rotation's time out of the machine's bound. Two things
/// it does not put back, and both are said: a pin the rotation ended, which
/// was left on an account that cannot be run on and is the owner's to give
/// again; and the rotation times it dropped as no longer this hour's, which
/// nothing counts. From a session that only took a silent canary's place
/// ([`take_canary`]) there is no rotation of its own to undo: it gives up
/// its hold, `active` stays, and the next session takes the place.
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
        // A taker that was not moved gives up its hold, and that is all:
        // the rotation was made by the session whose place it took.
        let held = state.canary.take();
        let took = held.as_ref().is_some_and(|c| c.took);
        let undoes = not_moved && !took;
        if undoes || !not_moved {
            state.active = state.previous.take();
            state.proven = state.active.is_some() && state.previous_proven;
            state.previous_proven = false;
            state.pinned = false;
            struck.to.clone_from(&state.active);
            // What the rotation wrote over is the account gone back to's
            // own, and goes back with it: where it had not answered either,
            // the account before it, that one's proof and its canary. (A
            // taker's place has none: the take dropped it.)
            if let Some(over) = state.undo.pop() {
                state.previous = over.previous;
                state.previous_proven = over.previous_proven;
                state.canary = over.canary;
            }
        }
        if undoes {
            // This rotation's own time comes out, by its canary's record
            // (the instant both were written). It is the newest — nothing
            // rotates past an unproven account that can be run on — unless
            // a rotation made PAST this account was struck since: a
            // rotation whose candidate ANSWERED, and badly, was made, and
            // its time, the newer one, stays counted.
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

/// The owner's word, `accounts use <label>`: the account's walls are
/// cleared — and the instant kept ([`Account::cleared_at`]), so neither a
/// merge nor a row still standing in a transcript ([`note_wall`]) brings
/// back the walls the owner cleared — and it is `active`, `proven` and
/// `pinned`: every session follows at its next point, and the early move
/// is off.
///
/// # Errors
///
/// By name: a label the roster does not list, one not enrolled on this
/// machine, one known signed out. Nothing is changed.
pub fn pin(
    state: &mut State,
    roster: &Roster,
    enrolled: &BTreeSet<String>,
    label: &str,
    now: i64,
) -> Result<Event, String> {
    let Some(row) = roster.row(label) else {
        return Err(format!("`{}` is not in the roster", shown(label)));
    };
    if !is_enrolled(row, enrolled) {
        return Err(format!(
            "`{label}` is not-enrolled: no login was made for it on this machine (its login \
             directory is missing or empty)"
        ));
    }
    if state
        .accounts
        .get(label)
        .is_some_and(|a| a.auth == Auth::SignedOut)
    {
        return Err(format!("`{label}` is signed-out"));
    }
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

/// End the pin (`use --auto`). Its ledger row when one held.
pub fn unpin(state: &mut State, why: &str, now: i64) -> Option<Event> {
    if !state.pinned {
        return None;
    }
    state.pinned = false;
    let account = state.active.clone().unwrap_or_default();
    Some(Event::new(now, Kind::Unpin, &account).because(why))
}

/// The newer of two observations by `(at, by)`, so a merge gives one answer
/// whichever side it is run from.
fn newer<T: Clone>(
    mine: Option<&T>,
    theirs: Option<&T>,
    key: impl Fn(&T) -> (i64, &str),
) -> Option<T> {
    match (mine, theirs) {
        (Some(a), Some(b)) => Some(if key(b) > key(a) { b } else { a }.clone()),
        (a, b) => a.or(b).cloned(),
    }
}

/// Take `from`'s OBSERVATIONS into `into`: per account and fact, the newer
/// `at` wins (a tie goes to the greater `by`). Walls merge per limit type;
/// the two newest distinct samples of the four are kept; the offer is the
/// newer one. MACHINE-LOCAL facts — the auth word, when a session last ran
/// on an account, `active`, `previous`, `proven`, `pinned`, the canary and
/// the undo, the sessions, the rotation times, the claims, the machine id —
/// are never read from `from`.
///
/// The merge keeps this module's own rules:
///
/// * ONLY THIS MACHINE'S ACCOUNTS: a label `into` already holds, or one
///   `roster` lists. Another machine's label is skipped, and none ever costs
///   this machine a record: with the state full, a listed label takes the
///   place of the stalest record the roster no longer lists.
/// * NO STAMP FROM THE FUTURE: each of `from`'s observations is read as made
///   no later than `now` plus [`STAMP_SKEW_S`], as [`State::from_json`]
///   reads this machine's own.
/// * THE OWNER'S CLEARING STANDS: a wall observed at or before the instant
///   the pin cleared that account's walls ([`Account::cleared_at`]) is the
///   one the owner cleared, and is not taken.
/// * A NINTH WALL takes the place of the one that stops standing first, as
///   in [`note_wall`].
/// * A WALL ON THE PINNED ACCOUNT ENDS THE PIN: the answer is that `unpin`
///   row, for the ledger, when a wall that stands at `now` was taken for it.
pub fn merge_observations(into: &mut State, from: &State, roster: &Roster, now: i64) -> Vec<Event> {
    let cap = stamp_cap(now);
    let mut out = Vec::new();
    for (label, theirs) in &from.accounts {
        if !admissible_label(label) {
            continue;
        }
        if !into.accounts.contains_key(label) {
            if roster.row(label).is_none() {
                continue;
            }
            if into.accounts.len() >= MAX_ACCOUNTS {
                let unlisted = into
                    .accounts
                    .iter()
                    .filter(|(held, _)| roster.row(held).is_none())
                    .min_by_key(|(_, a)| a.touched())
                    .map(|(held, _)| held.clone());
                let Some(unlisted) = unlisted else {
                    continue;
                };
                into.accounts.remove(&unlisted);
            }
        }
        let mine = into.accounts.entry(label.clone()).or_default();
        let mut walled = false;
        for (window, wall) in &theirs.walls {
            let wall = Wall {
                at: wall.at.min(cap),
                ..wall.clone()
            };
            let cleared = mine.cleared_at.is_some_and(|cleared| wall.at <= cleared);
            let wins = mine
                .walls
                .get(window)
                .is_none_or(|held| (wall.at, wall.by.as_str()) > (held.at, held.by.as_str()));
            if cleared || !wins {
                continue;
            }
            let stands = now < wall.stands_until();
            mine.walls.insert(window.clone(), wall);
            let lapsed = (mine.walls.len() > MAX_WALLS)
                .then(|| most_lapsed(&mine.walls))
                .flatten();
            if let Some(lapsed) = &lapsed {
                mine.walls.remove(lapsed);
            }
            walled |= stands && lapsed.as_ref() != Some(window);
        }
        let read = |s: &Option<Sample>| {
            s.as_ref().map(|s| Sample {
                at: s.at.min(cap),
                ..s.clone()
            })
        };
        let mut samples: Vec<Sample> = [
            mine.sample.clone(),
            mine.before.clone(),
            read(&theirs.sample),
            read(&theirs.before),
        ]
        .into_iter()
        .flatten()
        .collect();
        samples.sort_by(|a, b| (b.at, b.by.as_str()).cmp(&(a.at, a.by.as_str())));
        samples.dedup();
        let mut newest = samples.into_iter();
        mine.sample = newest.next();
        mine.before = newest.next();
        let offered = theirs.offer.as_ref().map(|o| Offer {
            at: o.at.min(cap),
            ..o.clone()
        });
        mine.offer = newer(mine.offer.as_ref(), offered.as_ref(), |o| {
            (o.at, o.by.as_str())
        });
        if walled {
            end_pin(into, label, "wall", now, &mut out);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// 7. The store
// ---------------------------------------------------------------------------

/// The lock's file name.
const LOCK_FILE: &str = "accounts.lock";

/// The state's file name.
const STATE_FILE: &str = "state.json";

/// The ledger's file name.
const LEDGER_FILE: &str = "ledger.jsonl";

/// The wake marker's file name.
const WAKE_FILE: &str = "wake";

/// At most `max` bytes of a regular file at `path` as text; `None` only for
/// a genuinely absent name. Unix opens do not wait on a FIFO. A larger file
/// is [`io::ErrorKind::FileTooLarge`]; bytes that are not text are
/// [`io::ErrorKind::InvalidData`].
pub(crate) fn read_bounded(path: &Path, max: u64) -> io::Result<Option<String>> {
    use std::io::Read as _;
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NONBLOCK);
    }
    let file = match options.open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return match std::fs::symlink_metadata(path) {
                Err(missing) if missing.kind() == io::ErrorKind::NotFound => Ok(None),
                _ => Err(e),
            };
        }
        Err(e) => return Err(e),
    };
    if !file.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a regular file",
        ));
    }
    let mut bytes = Vec::new();
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

/// `<harness state>/accounts/`: `state.json`, `ledger.jsonl`, the lock, the
/// wake marker and the login directories `accounts add` makes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Store {
    /// The directory (0700).
    pub dir: PathBuf,
}

/// A held [`Store::acquire`], released by `LOCK_UN` when it drops rather than
/// by the close.
///
/// The reason is [`super::upgrade_drive`]'s sweep lock's, measured there:
/// an `flock(2)` lives on the open file description, and while any thread
/// of this process is mid-spawn the child holds a copy of every descriptor
/// until its exec — so a lock released only by the close still reads as
/// held for that long. `LOCK_UN` strips it from the description itself.
#[derive(Debug)]
pub struct Lock(std::fs::File);

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

impl Store {
    /// The store under the harness state root
    /// (`<aterm state>/harness/claude-harness`).
    #[must_use]
    pub fn at(harness_state: &Path) -> Store {
        Store {
            dir: harness_state.join("accounts"),
        }
    }

    /// Make the directory, 0700, owned by this user and writable by nobody
    /// else — or say why it is not.
    ///
    /// # Errors
    ///
    /// It cannot be made, or it is not this user's private directory.
    pub fn ensure(&self) -> io::Result<()> {
        aterm_types::fs_restricted::ensure_private_dir(&self.dir)
    }

    fn lock_file(&self) -> io::Result<std::fs::File> {
        self.ensure()?;
        let mut open = std::fs::OpenOptions::new();
        open.create(true).truncate(false).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            open.mode(0o600);
        }
        open.open(self.dir.join(LOCK_FILE))
    }

    /// Take `accounts.lock`, WAITING for whoever holds it. Every read-edit-
    /// write of the state and every ledger append happens under it. It is a
    /// short lock: never held across a relaunch, a probe or any wait.
    ///
    /// Named `acquire`, not `lock`, on purpose: the lock-order census
    /// (`aterm-census --locks`) reads `<name>.lock()` as a mutex named
    /// `<name>`, and a file lock taken as `store.lock()` would be merged into
    /// every mutex a caller happens to call `store`.
    ///
    /// # Errors
    ///
    /// The directory or the lock file cannot be made, or the lock failed.
    pub fn acquire(&self) -> io::Result<Lock> {
        let file: std::fs::File = self.lock_file()?;
        loop {
            #[expect(
                clippy::disallowed_methods,
                reason = "the guard's own take: its Drop releases by LOCK_UN"
            )]
            let taken = file.lock();
            match taken {
                Ok(()) => return Ok(Lock(file)),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
    }

    /// Take `accounts.lock` without waiting: `None` while another holder
    /// has it.
    ///
    /// # Errors
    ///
    /// As [`Store::acquire`].
    pub fn try_acquire(&self) -> io::Result<Option<Lock>> {
        Ok(Self::try_take(self.lock_file()?)?.ok())
    }

    /// One try at the lock on `file`: the guard, or the file handed back
    /// while another holder has it.
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

    /// Take `accounts.lock` with a BOUNDED wait: one try, then one more
    /// after each pause of `ladder`, and `None` when the last one still
    /// found it held. The take of a caller that must not block — a
    /// session's loop, a host thread — with [`LOCK_LADDER`]; the CLI's own
    /// transactions wait ([`Store::acquire`]). An empty ladder is
    /// [`Store::try_acquire`].
    ///
    /// # Errors
    ///
    /// As [`Store::acquire`].
    pub fn acquire_within(&self, ladder: &[std::time::Duration]) -> io::Result<Option<Lock>> {
        let mut file = match Self::try_take(self.lock_file()?)? {
            Ok(lock) => return Ok(Some(lock)),
            Err(file) => file,
        };
        for pause in ladder {
            std::thread::sleep(*pause);
            file = match Self::try_take(file)? {
                Ok(lock) => return Ok(Some(lock)),
                Err(file) => file,
            };
        }
        Ok(None)
    }

    /// `state.json`.
    #[must_use]
    pub fn state_file(&self) -> PathBuf {
        self.dir.join(STATE_FILE)
    }

    /// `ledger.jsonl`.
    #[must_use]
    pub fn ledger_path(&self) -> PathBuf {
        self.dir.join(LEDGER_FILE)
    }

    /// Where `accounts add` makes login directories.
    #[must_use]
    pub fn logins_dir(&self) -> PathBuf {
        self.dir.join("logins")
    }

    /// The login directory `accounts add <label>` makes, or `None` for a
    /// label no roster could carry (so no label is ever a path of its own).
    #[must_use]
    pub fn login_dir(&self, label: &str) -> Option<PathBuf> {
        admissible_label(label).then(|| self.logins_dir().join(label))
    }

    /// The wake marker.
    #[must_use]
    pub fn wake_path(&self) -> PathBuf {
        self.dir.join(WAKE_FILE)
    }

    /// `logins.json` ([`Logins`]).
    #[must_use]
    pub fn logins_file(&self) -> PathBuf {
        self.dir.join(LOGINS_FILE)
    }

    /// THE STAGING DIRECTORY a renewal of `label` signs in to, MADE BY THIS
    /// CALL (0700) and so its alone: the first `<logins>/<label>+<n>`, `n`
    /// from 1, that it creates — a name already on disk is passed over, never
    /// entered. A new directory, so its login is a new keychain item that no
    /// session reads until the check has answered ([`note_renewed`]); never
    /// one a renewal before used, so a superseded login's item is never
    /// written over; never one another renewal running at the same moment
    /// made, so two renewals never sign in to one home. `+` is in no label
    /// ([`admissible_label`]), so no row's own login directory
    /// (`<logins>/<label>`, `accounts add`) is ever a staging home, nor a
    /// staging home any row's.
    ///
    /// # Errors
    ///
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
            // Not recursive: an existing name is an error, never a home.
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

    /// `logins.json` as of `now`. Only a genuinely absent file is no
    /// bindings. Damaged or unreadable bytes cannot authorize the roster's
    /// original identities, or a transaction over empty bindings.
    ///
    /// # Errors
    ///
    /// As [`Store::load`]: a file that cannot be read, is larger than
    /// [`MAX_LOGINS_BYTES`], or is JSON of another schema or of none.
    pub fn load_logins(&self, now: i64) -> Result<(Logins, Option<String>), String> {
        match read_bounded(&self.logins_file(), MAX_LOGINS_BYTES) {
            Ok(None) => Ok((Logins::default(), None)),
            Ok(Some(text)) => match Logins::from_json(&text, now) {
                Ok(logins) => Ok((logins, None)),
                Err(Unread::Torn(why) | Unread::NotOurs(why)) => {
                    Err(format!("{LOGINS_FILE}: {why}"))
                }
            },
            Err(e) => Err(format!("{LOGINS_FILE}: {e}")),
        }
    }

    /// THE ROSTER AS THIS MACHINE'S LOGINS BIND IT ([`bound`]), `logins.json`
    /// read without the lock (it is replaced by a rename). A file that
    /// cannot be read as this build's refuses identity attribution.
    ///
    /// # Errors
    /// As [`Self::load_logins`].
    pub fn bound(&self, roster: &Roster, now: i64) -> Result<Roster, String> {
        self.load_logins(now)
            .map(|(logins, _)| bound(roster, &logins))
    }

    /// Write `logins.json` (a sibling temp file, 0600, then a rename).
    ///
    /// # Errors
    ///
    /// The directory cannot be made or the write or rename failed.
    pub fn save_logins(&self, logins: &Logins) -> io::Result<()> {
        self.ensure()?;
        let mut text = logins.to_json();
        text.push('\n');
        super::upgrade_models::write_atomic(&self.logins_file(), &text)
    }

    /// Replace the wake marker (a sibling temp file, then a rename): its
    /// file identity changes, which is what a watcher wakes on. Called after
    /// any write that makes an account usable sooner ([`Event::wakes`]).
    ///
    /// # Errors
    ///
    /// The directory cannot be made or the write or rename failed.
    pub fn touch_wake(&self) -> io::Result<()> {
        self.ensure()?;
        super::upgrade_models::write_atomic(&self.wake_path(), "")
    }

    /// The state as of `now` (the clock [`State::from_json`] reads stamps
    /// against), and a note when a file was there and held nothing.
    ///
    /// No file is the empty state. A file that is NOT JSON — cut short — is
    /// the empty state too, with a note saying what was wrong: it holds
    /// nothing to lose, and the next [`Store::save`] replaces it. A state
    /// with no machine id gets a fresh one ([`mint_machine`]), kept from the
    /// first save on. Never a panic.
    ///
    /// # Errors
    ///
    /// A FILE THAT IS NOT OURS TO REPLACE, by name: one that could not be
    /// read (a permission, an I/O error, a directory in its place), one
    /// larger than [`MAX_STATE_BYTES`], and one that is JSON of another
    /// schema or of none ([`Unread::NotOurs`]) — a newer build's state, read
    /// here by an older one. It may hold a pin, walls and signed-out words,
    /// so NOTHING IS SAVED OVER IT: a transaction that gets this answer
    /// stops, and a read-only caller shows an empty state beside the reason.
    pub fn load(&self, now: i64) -> Result<(State, Option<String>), String> {
        let (mut state, mut note) = match read_bounded(&self.state_file(), MAX_STATE_BYTES) {
            Ok(None) => (State::default(), None),
            Ok(Some(text)) => match State::from_json(&text, now) {
                Ok(state) => (state, None),
                Err(Unread::Torn(why)) => (
                    State::default(),
                    Some(format!("{STATE_FILE}: {why}; read as an empty state")),
                ),
                Err(Unread::NotOurs(why)) => return Err(format!("{STATE_FILE}: {why}")),
            },
            // Bytes that are not text are not JSON either: the same torn file.
            Err(e) if e.kind() == io::ErrorKind::InvalidData => (
                State::default(),
                Some(format!("{STATE_FILE}: {e}; read as an empty state")),
            ),
            Err(e) => return Err(format!("{STATE_FILE}: {e}")),
        };
        if state.machine.is_empty() {
            match mint_machine() {
                Ok(id) => state.machine = id,
                Err(e) => {
                    note.get_or_insert_with(|| {
                        format!("no machine id could be minted ({e}); observations carry none")
                    });
                }
            }
        }
        Ok((state, note))
    }

    /// Write the state (a sibling temp file, 0600, then a rename), so a
    /// reader sees the old file or the new one and never half of either.
    ///
    /// # Errors
    ///
    /// The directory cannot be made or the write or rename failed.
    pub fn save(&self, state: &State) -> io::Result<()> {
        self.ensure()?;
        let mut text = state.to_json();
        text.push('\n');
        super::upgrade_models::write_atomic(&self.state_file(), &text)
    }

    /// Append `events` to the ledger (0600) in ONE write, then cut the
    /// ledger back to its newest [`LEDGER_KEEP_ROWS`] rows once it is larger
    /// than [`LEDGER_MAX_BYTES`].
    ///
    /// # Errors
    ///
    /// The directory cannot be made or the append or the cut failed.
    pub fn append(&self, events: &[Event]) -> io::Result<()> {
        use std::io::Write as _;
        if events.is_empty() {
            return Ok(());
        }
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
            open.mode(0o600);
        }
        let mut file = open.open(&path)?;
        file.write_all(text.as_bytes())?;
        drop(file);
        super::cli::bound_ledger(&path, LEDGER_MAX_BYTES, LEDGER_KEEP_ROWS).map(|_| ())
    }

    /// The newest `max` ledger rows, oldest first; a line that is not a row
    /// of this ledger is skipped.
    ///
    /// # Errors
    ///
    /// The ledger exists and cannot be read.
    pub fn tail(&self, max: usize) -> io::Result<Vec<Event>> {
        Ok(super::cli::tail_lines(&self.ledger_path(), max)?
            .iter()
            .filter_map(|line| Event::of_json_line(line))
            .collect())
    }

    /// ONE TRANSACTION on the accounts state: take the lock, load, let `edit`
    /// change the state and answer its ledger rows, then save (only a state
    /// that changed), append the rows, and replace the wake marker when a
    /// row makes an account usable sooner ([`Event::wakes`]). The lock is
    /// held for the read-edit-write alone: `edit` must not wait on anything.
    ///
    /// `note` is set when a file was there and held nothing ([`Store::load`]:
    /// a torn state, read as empty and replaced by this write), whether or
    /// not the transaction then succeeds.
    ///
    /// A STATE FILE THAT IS NOT OURS IS NEVER SAVED OVER ([`Store::load`]):
    /// one of another schema (a newer build's), one that cannot be read, one
    /// too large. The transaction stops before `edit` runs.
    ///
    /// A state that is saved is saved without its stale session rows
    /// ([`drop_stale_sessions`]).
    ///
    /// THE LOCK IS WAITED FOR, however long its holder has it: the take of
    /// `aterm harness accounts …`, whose owner is at the keyboard. A loop or
    /// a host thread takes it bounded instead ([`Store::transact_within`]).
    ///
    /// # Errors
    ///
    /// That refusal, naming the file; `edit`'s own, with nothing written; or
    /// the file that could not be locked or written, by path.
    pub fn transact(
        &self,
        now: i64,
        note: &mut Option<String>,
        edit: impl FnOnce(&mut State) -> Result<Vec<Event>, String>,
    ) -> Result<Vec<Event>, String> {
        let held = self
            .acquire()
            .map_err(|e| format!("{}: {e}", self.dir.display()))?;
        self.transact_held(&held, now, note, edit)
    }

    /// [`Store::transact`] with a BOUNDED wait for the lock
    /// ([`Store::acquire_within`] over `ladder`; [`LOCK_LADDER`] is the
    /// product's): the same transaction, for a caller that must not block.
    ///
    /// # Errors
    ///
    /// As [`Store::transact`]; and EXACTLY [`LOCK_HELD`] when the lock
    /// stayed held past the ladder — nothing was read, `edit` did not run,
    /// and the caller asks again at its next point.
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
        self.transact_held(&held, now, note, edit)
    }

    /// ONE TRANSACTION ON THE STATE AND THE LOGINS TOGETHER, under the one
    /// lock ([`Store::transact`]'s, waited for: the CLI's): `edit` gets both,
    /// and each file is saved only where it changed. A `logins.json` that is
    /// not this build's is never saved over, as a `state.json` is not.
    ///
    /// # Errors
    ///
    /// As [`Store::transact`], for either file.
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

    /// [`Store::transact_logins`] with the BOUNDED wait of
    /// [`Store::transact_within`]: a host thread's.
    ///
    /// # Errors
    ///
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

    /// The logins' transaction itself, under the lock its caller took: the
    /// state's ([`Store::transact_held`]) with `logins.json` read and saved
    /// beside it — the logins FIRST, so a session the wake marker sends to
    /// decide again reads the binding the same transaction made.
    fn transact_logins_held(
        &self,
        held: &Lock,
        now: i64,
        note: &mut Option<String>,
        edit: impl FnOnce(&mut State, &mut Logins) -> Result<Vec<Event>, String>,
    ) -> Result<Vec<Event>, String> {
        let (mut logins, read) = self.load_logins(now).map_err(|why| {
            format!(
                "{why}: {} is left as it is and nothing was written",
                self.logins_file().display()
            )
        })?;
        let before = logins.clone();
        let mut saved = Ok(());
        let events = self.transact_held(held, now, note, |state| {
            let events = edit(state, &mut logins)?;
            if logins != before {
                saved = self.save_logins(&logins);
            }
            saved
                .as_ref()
                .map_err(|e| format!("{}: {e}", self.logins_file().display()))?;
            Ok(events)
        })?;
        if note.is_none() {
            *note = read;
        }
        Ok(events)
    }

    /// The transaction itself, under the lock its caller took.
    fn transact_held(
        &self,
        _held: &Lock,
        now: i64,
        note: &mut Option<String>,
        edit: impl FnOnce(&mut State) -> Result<Vec<Event>, String>,
    ) -> Result<Vec<Event>, String> {
        let (mut state, read) = self.load(now).map_err(|why| {
            format!(
                "{why}: {} is left as it is and nothing was written (move it aside to start \
                 from an empty state)",
                self.state_file().display()
            )
        })?;
        *note = read;
        let before = state.clone();
        let events = edit(&mut state)?;
        if state != before {
            drop_stale_sessions(&mut state, now);
            self.save(&state)
                .map_err(|e| format!("{}: {e}", self.state_file().display()))?;
        }
        self.append(&events)
            .map_err(|e| format!("{}: {e}", self.ledger_path().display()))?;
        if events.iter().any(Event::wakes) {
            self.touch_wake()
                .map_err(|e| format!("{}: {e}", self.wake_path().display()))?;
        }
        Ok(events)
    }
}

// ---------------------------------------------------------------------------
// 8. The recorder (design §5, "What counts as an observation")
// ---------------------------------------------------------------------------

/// What became of one call of the recorder ([`record_wall`]). A host logs it
/// by [`Recorded::as_str`] alone: the reason a [`Recorded::Refused`] carries
/// names files under the owner's state directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Recorded {
    /// The session is on record on `account`, and the wall is: written now,
    /// or already there from another session at the same wall.
    Wall {
        /// The account the session runs on ([`Me::label`]).
        account: String,
        /// The window it is recorded under: the row's limit type, or
        /// [`UNNAMED_WINDOW`].
        window: String,
        /// The reset it names.
        until: Option<i64>,
        /// The ledger rows this call appended: the `wall` row (none when
        /// the same wall was on record and still standing), a `move` row
        /// when the session was last read on another account, an `unpin`
        /// row when the wall is on the pinned account.
        events: usize,
    },
    /// No roster, or the process's login is no row's of it, nor one a
    /// renewal moved a row off ([`Me::label`]): there is no account to
    /// record for. Nothing was read or written.
    NotListed,
    /// The process's home is not the default one, or it does not
    /// authenticate by a stored login: its wall is not an account's of this
    /// roster. Nothing was read or written.
    NotStored,
    /// The row is older than the process: the PREVIOUS process's wall, still
    /// standing in the transcript after a relaunch — under rotation, the
    /// account that was left. It is recorded for nobody. Nothing was read
    /// or written.
    NotThisProcess,
    /// The row was written before the owner cleared this account's walls
    /// (`accounts use`, [`pin`]): it is the wall the owner cleared, still
    /// standing in the transcript of a session that has taken no turn
    /// since. No wall was recorded and no pin ended; the session is on
    /// record on the account all the same.
    Cleared,
    /// What a session's screen said of the account's usage — credits in use,
    /// a window filling — is on record now ([`record_usage`]), with `events`
    /// ledger rows.
    Usage {
        /// The account the session runs on ([`Me::label`]).
        account: String,
        events: usize,
    },
    /// Everything this call would write is on record. Nothing was written.
    Unchanged,
    /// The state is not this build's to write, or a file could not be
    /// locked or written — the reason, for a log that is the owner's own.
    /// EXACTLY [`LOCK_HELD`] when the lock stayed held past the bounded
    /// wait ([`Recorded::lock_held`]): nothing was read, and the caller
    /// asks again at the session's next point.
    Refused(String),
}

impl Recorded {
    /// Whether this is the refusal of a lock that stayed held
    /// ([`LOCK_HELD`]): the one refusal that asking again can change.
    #[must_use]
    pub fn lock_held(&self) -> bool {
        matches!(self, Recorded::Refused(why) if why == LOCK_HELD)
    }

    /// The one word a host logs.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Recorded::Wall { .. } => "wall",
            Recorded::Usage { .. } => "usage",
            Recorded::NotListed => "not-listed",
            Recorded::NotStored => "not-stored",
            Recorded::NotThisProcess => "not-this-process",
            Recorded::Cleared => "cleared",
            Recorded::Unchanged => "unchanged",
            Recorded::Refused(_) => "refused",
        }
    }
}

/// THE RECORDER: a session stands at a usage wall, and the wall is recorded
/// for the account its process runs on. The one call a host makes with the
/// facts it read — the session's id, its process's environment
/// ([`ProcessLogin`]), the standing limit row of its transcript
/// ([`super::upgrade::StandingWall`]) and when its process started — with
/// the store's lock taken INSIDE, for the read-edit-write alone, and WAITED
/// FOR NO LONGER THAN [`LOCK_LADDER`] ([`Store::transact_within`]): the
/// caller is a session's loop or a host thread, and a lock that stays held
/// is [`Recorded::Refused`] with [`LOCK_HELD`], to be asked again at the
/// session's next point. Nothing is typed and no session is touched; `now`
/// is the caller's clock.
///
/// WHAT COUNTS (design §5), judged in this order BEFORE any file is opened —
/// so with no roster nothing is read, made or locked:
///
/// 1. The process's login is a row of the roster ([`ProcessLogin::me`]).
/// 2. Its home is the default one and it authenticates by a stored login.
/// 3. THE ROW IS THIS PROCESS'S: its `at` is not before `process_started`.
///    Both are whole seconds, and A ROW IN THE PROCESS'S FIRST SECOND
///    COUNTS: a process cannot write a row before it starts, so an equal
///    second is its own unless the previous process wrote its wall and this
///    one started inside that same second — ASSUMED not to happen (the
///    supervisor's exit and relaunch are typed lines with waits between
///    them), not measured.
///
/// THEN, under the lock: the session is noted on the account
/// ([`note_session`]: a `move` row when it was last read on another; a
/// `held` word a refused move left on its row is kept), and the wall is
/// noted ([`note_wall`]) under the row's limit type, or [`UNNAMED_WINDOW`]
/// where it has none. The rows are appended WITH THE SESSION'S ID, and the
/// wake marker is replaced when one of them says so (a wall on the pinned
/// account ends the pin). A second session at the same wall writes its own
/// session row and no second `wall` row; the same session asking again
/// writes nothing ([`Recorded::Unchanged`]) — the call is cheap enough to
/// make at every look.
///
/// A row stamped AHEAD of `now` is recorded as written now: one machine's
/// clock wrote both, so a stamp ahead of it is a clock that was set back,
/// and a wall observed "later" than now would shadow every wall seen until
/// the clock caught up. A reset past what an `i64` holds is no reset, and
/// neither is one more than [`RESET_BELIEVED_S`] past its row
/// ([`believed_reset`]): the wall is recorded as naming none.
///
/// A ROW WRITTEN BEFORE THE OWNER CLEARED THE ACCOUNT'S WALLS (`accounts
/// use`, [`pin`]) is the wall the owner cleared ([`note_wall`]): the session
/// is noted and the wall is not ([`Recorded::Cleared`]), at this look and at
/// every later one, until the session meets a wall again. Without it the
/// first look after the owner's word would put the wall back and end the
/// pin, on a row the owner had already answered.
///
/// IT RECORDS, IT DOES NOT STRIKE. A wall on an account that has not served
/// a response yet is recorded with `active` left where it is, as
/// [`note_wall`] leaves it: putting `active` back is the move's own
/// transaction ([`strike`]), which nothing commits yet. And EVERY wall
/// limits the account ([`condition`]), a model's bucket included.
pub fn record_wall(
    store: &Store,
    roster: &Roster,
    sid: &str,
    who: &ProcessLogin,
    wall: &super::upgrade::StandingWall,
    process_started: i64,
    now: i64,
) -> Recorded {
    let me = who.me(roster);
    let Some(account) = me.label() else {
        return Recorded::NotListed;
    };
    if !me.default_home || !me.stored_login {
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
    let mut note = None;
    let done = store.transact_within(&LOCK_LADDER, now, &mut note, |state| {
        let before = state.clone();
        // The session's row is rewritten only when it says something new:
        // a look that finds everything on record must write nothing.
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
            .is_some_and(|record| record.cleared_since(at, now));
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

/// What the limit row `word`, written at `at`, says of `account`'s usage
/// credits, kept by [`Credits`]' rule: it replaces a word no newer than the
/// row — at the same second, a screen's — and an older row, or one that says
/// nothing, changes nothing. A row that says them on adds nothing to credits
/// seen in use.
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
            // A monthly cap's return can be weeks out: past what a wall's
            // reset is believed to be (`believed_reset`), and it decides only
            // whether a line says credits are off.
            until: until.and_then(|u| i64::try_from(u).ok()),
        }),
    });
}

/// Credits carry the session, by Claude Code's notice ([`UsageSaid`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CreditsRead {
    /// The reset of the window they carry past, where the notice they started
    /// on named it (placed by the caller).
    pub until: Option<i64>,
}

/// What Claude Code's warning that a window is filling says ([`UsageSaid`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WarningRead {
    /// The window: the week (`true`) or the five-hour session window.
    pub weekly: bool,
    /// Whole percent used.
    pub pct: u32,
    /// Its reset, placed by the caller.
    pub reset: Option<i64>,
}

/// WHAT ONE SESSION'S SCREEN SAYS OF ITS ACCOUNT'S USAGE, and when it was
/// true ([`record_usage`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UsageSaid {
    /// When it was true: the newest answer in the session's transcript
    /// written [`NOTICES_DRAWN_S`] or more before the screen was read (an
    /// API response's row, unix seconds). Claude Code draws its notices from
    /// the newest answer's headers and leaves them standing at rest, so a tab
    /// idle for days still shows what was true at its last answer — never
    /// the screen read's own time, at which a notice drawn before a limit
    /// reset and said again outranked the reset (the review of S5).
    pub at: i64,
    /// The local day of [`Self::at`] (days since the epoch on this machine's
    /// clock and zone), for the credits' once-a-day line.
    pub day: i64,
    /// Credits carry the session.
    pub credits: Option<CreditsRead>,
    /// The warning that a window is filling.
    pub warning: Option<WarningRead>,
}

/// THE RECORDER OF WHAT A SESSION'S SCREEN SAYS OF USAGE (accounts v2 §3.3,
/// §5.4): Claude Code's notice that usage credits carry the session, and its
/// warning that a window is filling, recorded for the account its process
/// runs on ([`Me::label`]: its login's row, or the row a renewal moved off
/// that login, through a [`bound`] roster) — the facts [`record_wall`] is
/// handed and judged by (the process's login a row of `roster`, its default
/// home, a stored login), under the store's lock taken inside for the
/// bounded wait. Nothing is typed, no session is touched, and no move is
/// advised by this: nothing that moves a session reads it.
///
/// ONE RULE KEEPS IT ([`Credits`], [`WindowSaid`]): what the screen says is
/// as old as `said.at`, the newest answer it was drawn after
/// ([`NOTICES_DRAWN_S`]); the newest word on credits, and on each window,
/// wins; an older one changes nothing. So the loop telling it again (every
/// [`CREDITS_RETELL_S`]) writes nothing unless an answer came since, and a
/// notice drawn before a limit reset refilled its window says nothing after
/// it ([`reset_outcome`]), on whatever tab and in whatever loop it is read.
///
/// * CREDITS: the answer is one served on them. Inside an episode that still
///   stood at it ([`CreditsUse::stands`]) it is that episode's newest;
///   otherwise, newer than the word on record, it begins an episode (a
///   `credits` ledger row, `in use`), which is the day's line of the digest
///   ([`credits_noted`]) when it is the first to begin on `said.day`. Credits
///   seen in use after a limit row said them off are on: that word is the
///   older ([`windows_full`]), and so is that row read again.
/// * THE WARNING: the named window's figure and reset, no older than the
///   word on record, replace it — of the same answer, the later read of its
///   screen, redrawn since — with a `warned` ledger row where they moved.
///   The other window keeps its own word: Claude Code shows one warning at a
///   time.
pub fn record_usage(
    store: &Store,
    roster: &Roster,
    sid: &str,
    who: &ProcessLogin,
    said: &UsageSaid,
    now: i64,
) -> Recorded {
    let me = who.me(roster);
    let Some(account) = me.label() else {
        return Recorded::NotListed;
    };
    if !me.default_home || !me.stored_login {
        return Recorded::NotStored;
    }
    let at = said.at.min(now);
    let sid = one_line(sid, WORD_MAX);
    let mut changed = false;
    let mut note = None;
    let done = store.transact_within(&LOCK_LADDER, now, &mut note, |state| {
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
        changed = *record != before;
        Ok(events
            .into_iter()
            .map(|event| event.with_sid(&sid))
            .collect())
    });
    match done {
        Err(why) => Recorded::Refused(why),
        Ok(_) if !changed => Recorded::Unchanged,
        Ok(events) => Recorded::Usage {
            account: account.to_string(),
            events: events.len(),
        },
    }
}

/// [`record_usage`]' word that credits carried the answer at `at`, on the
/// local day `day`: its ledger row where it began an episode.
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

/// [`record_usage`]' word that the warning's window stood at its figure at
/// `at`: its ledger row where the figure or the reset moved.
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

/// THE ACCOUNTS THE DIGEST'S CREDITS LINE NAMES (accounts v2 §5.4, "Credits in
/// use: Entry, once per account per day"), in roster order: usage credits
/// carry their sessions ([`CreditsUse::stands`]) in an episode that raised the
/// day's line ([`CreditsUse::told`]).
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

/// THE ACCOUNTS WHOSE WEEK IS NEARLY USED WITH NO CREDITS TO CARRY IT
/// (accounts v2 §3.3), in roster order, each with the vendor's word for
/// why its credits are off. A PURE FUNCTION OF THE ACCOUNT'S NEWEST WORDS at
/// `now` ([`record_usage`]'s rule): on its credits, a limit row's `off`, not
/// yet back ([`Credits::Off`]); on its week ([`Account::warned_week`]),
/// Claude Code's warning at [`WINDOW_FULL_PCT`] or more, its reset more than
/// [`WINDOW_FULL_AHEAD_S`] off, and no limit reset's refill since. Credits on,
/// in use, or that nothing said, make no line: a turn that crosses the limit
/// goes on, or nobody knows that it will not.
///
/// NOT [`Account::sample`]: the advice's figures are kept by rules of their
/// own — a `/usage` panel taken whole, the usage tap's largest reading within
/// a reset ([`note_reading`]), stamped when it is recorded — which a limit
/// reset's refill does not end and an idle tab's figures, recorded afresh,
/// outrank: ranked beside these words, a week refilled stood `nearly used`
/// again.
#[must_use]
pub fn windows_full(roster: &Roster, state: &State, now: i64) -> Vec<(String, String)> {
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
            (pct >= WINDOW_FULL_PCT && end.saturating_sub(now) > WINDOW_FULL_AHEAD_S)
                .then(|| (row.label.clone(), off.reason.clone()))
        })
        .collect()
}

/// THE SESSION `sid` ENDED (its agent did): what is kept of it is forgotten
/// ([`forget_session`]), under the store's lock taken inside. Whether
/// anything was. When it was the canary of an account that has not answered,
/// its hold ends with it and the wake marker is replaced: a session waiting
/// for that hold decides again. `now` is the caller's clock.
///
/// A machine with no state file has nothing to forget, and nothing is made
/// or locked for it: the packaged, rosterless state stays untouched.
///
/// The lock is waited for no longer than [`LOCK_LADDER`], as the recorder's
/// is: the caller is a host thread.
///
/// # Errors
///
/// As [`Store::transact_within`]: the state is not this build's to write, a
/// file could not be locked or written, or — EXACTLY [`LOCK_HELD`] — the
/// lock stayed held. Nothing was forgotten; a row left behind for that is
/// dropped by its age ([`drop_stale_sessions`]), and a canary's hold lapses
/// by its own clock.
pub fn record_gone(store: &Store, sid: &str, now: i64) -> Result<bool, String> {
    if !store.state_file().exists() {
        return Ok(false);
    }
    let sid = one_line(sid, WORD_MAX);
    let mut forgot = false;
    let mut held = false;
    let mut note = None;
    store.transact_within(&LOCK_LADDER, now, &mut note, |state| {
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

#[cfg(test)]
#[path = "accounts_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "accounts_logins_tests.rs"]
mod logins_tests;
