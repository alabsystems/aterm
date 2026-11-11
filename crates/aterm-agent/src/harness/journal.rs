// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The harness's side of the lab journal (`docs/DESIGN-alab-lab-model-2026-11-01.md` §4.17, §8,
//! step 8): what the accounts store already sees, as journal notes.
//!
//! `aterm-agent` does not link `aterm-db` (it would bring Commons and the broker into every
//! harness build), so the journal reaches it through a [`Sink`] the `aterm` executable installs
//! at start, as the account claims do (`super::claims`, `crates/aterm/src/alab_journal.rs`).
//! With no sink installed nothing is keyed and nothing is noted, unless a test supplies a
//! scoped keyer for its current-thread identity lookups.
//!
//! - [`learn`]: a roster-bound transaction keys each account it read by its lab key
//!   ([`super::accounts::Account::pk`]), the account key of the claims email (design §5).
//! - [`observe`]: after a transaction's edit, its ledger rows gain that key
//!   ([`super::accounts::Event::pk`]) and become notes in the journal's vocabulary, with the
//!   session starts and ends, quota readings and walls the state's diff shows.
//! - [`publish`]: once the transaction is durable, each note goes to the sink, whose `note` is
//!   one bounded append to the spool with no lock and no allocation (the typing hook).
//! - [`backfill`]: once per Mac, the local ledger's last 30 days and the state's standing facts;
//!   [`mark_backfilled`] once the caller has made them durable.
//! - [`probe_note`]: each provider usage read step 9's door reports
//!   ([`super::provider_read::ProbeEvent`]), as a `probe` of the account read with why it is in
//!   use; the executable fills that door with it. Claude Code's ledger `probe` claim row is not
//!   noted, since the door's report of the typed panel is the read itself.
//!
//! A note holds no ledger free text: every word in it is one of this module's constants, so no
//! address, URL, code or reason line can reach the journal.

#[cfg(unix)]
use super::accounts::Store;
use super::accounts::{Auth, Cli, Event, Kind, Roster, State, WindowFig};
use super::claims::LeaseKey;
use super::provider_read::{ProbeEvent, Read};
use crate::ai::connector::Region;
use std::collections::BTreeMap;
#[cfg(unix)]
use std::path::Path;
use std::sync::OnceLock;

/// An account's lab key: `ac-` (or `sa-`) and 16 lower-case hex digits, held inline.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Key([u8; 19]);

impl Key {
    /// `text` when it is an account or sub-account key.
    #[must_use]
    pub fn new(text: &str) -> Option<Key> {
        let raw: [u8; 19] = text.as_bytes().try_into().ok()?;
        let (prefix, hex) = raw.split_at(3);
        ((prefix == b"ac-" || prefix == b"sa-")
            && hex
                .iter()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(b)))
        .then_some(Key(raw))
    }

    /// The key.
    #[must_use]
    pub fn as_str(&self) -> &str {
        // Only ASCII is ever stored.
        std::str::from_utf8(&self.0).unwrap_or_default()
    }
}

impl std::fmt::Debug for Key {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A provider workspace's `AccountKey` (design §4.13): 12 lower-case hex digits, held inline. A
/// reading that names one folds into that sub-account's own `use` row, so two workspaces of one
/// login keep their own quota.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SubKey([u8; 12]);

impl SubKey {
    /// `text` when it is 12 lower-case hex digits.
    #[must_use]
    pub fn new(text: &str) -> Option<SubKey> {
        let raw: [u8; 12] = text.as_bytes().try_into().ok()?;
        raw.iter()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(b))
            .then_some(SubKey(raw))
    }

    /// The workspace an account's observed key names: a Codex key's (`x-<12 hex>`), the one
    /// provider whose login holds several workspaces, each its own `AccountKey` (`profile.rs`).
    /// Any other provider's key names its one account, so its readings stay on the account's
    /// row rather than split it in two.
    #[must_use]
    pub fn workspace(key: &super::profile::AccountKey) -> Option<SubKey> {
        let text = key.to_string();
        Self::new(text.strip_prefix("x-")?)
    }

    /// The key.
    #[must_use]
    pub fn as_str(&self) -> &str {
        // Only ASCII is ever stored.
        std::str::from_utf8(&self.0).unwrap_or_default()
    }
}

impl std::fmt::Debug for SubKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A tab, as a journal note names it: the first 8 hex digits of SHA-256 over this Mac's machine
/// id and the session's sid (`docs/DESIGN-account-discovery-2026-11-09.md`, until the registry
/// mints a tab id), held inline. The sid itself never leaves the Mac.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Tab([u8; 8]);

impl Tab {
    /// The tab `sid` is on the Mac whose machine id is `machine`.
    #[must_use]
    pub fn of(machine: &str, sid: &str) -> Tab {
        let mut hasher = aterm_digest::Sha256::new();
        hasher.update(machine.as_bytes());
        hasher.update([0]);
        hasher.update(sid.as_bytes());
        let digest = hasher.finalize();
        let mut out = [0u8; 8];
        for (i, byte) in digest.iter().take(4).enumerate() {
            const HEX: &[u8; 16] = b"0123456789abcdef";
            out[2 * i] = HEX[usize::from(byte >> 4)];
            out[2 * i + 1] = HEX[usize::from(byte & 0xf)];
        }
        Tab(out)
    }

    /// The tab id.
    #[must_use]
    pub fn as_str(&self) -> &str {
        // Only ASCII hex is ever stored.
        std::str::from_utf8(&self.0).unwrap_or_default()
    }
}

impl std::fmt::Debug for Tab {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Every kind word a note can carry; each is a word of the journal's `ev` vocabulary.
pub const KINDS: &[&str] = &[
    "session-start",
    "session-end",
    "signed-in",
    "signed-out",
    "sign-in-failed",
    "login-ended",
    "renewed",
    "wrong-account",
    "displaced",
    "auth",
    "quota",
    "wall",
    "wall-cleared",
    "reset-used",
    "credits",
    "error",
    "switch",
    "probe",
    "token-minted",
    "token-served",
    "token-verified",
    "token-rejected",
    "token-fallback",
    "token-offered",
    "token-removed",
    "seen",
];
/// Every window word a note can carry.
pub const WINDOWS: &[&str] = &["5h", "7d", "7d-model"];
/// Every source word a note can carry.
pub const SOURCES: &[&str] = &[
    "statusline",
    "rollout",
    "session-usage",
    "cli-output",
    "signin-flow",
    "error",
    "probe",
    // Where an observed login was read (`seen`, [`ObservedSrc`]).
    "profile",
    "auth-file",
    "cli-log",
    "vault-token",
];
/// Every reset-resolution word a note can carry.
pub const RESES: &[&str] = &["exact", "hour"];
/// Every reason word a note can carry.
pub const WHYS: &[&str] = &[
    "ok",
    "unseen",
    "epoch",
    "reset-failed",
    "no-tap",
    "seen",
    // Why a provider read's account is in use (`provider_read::InUse::word`).
    "launching",
    "live-session",
    "at-wall",
    "asked",
    // Where a minted token's end came from (`TokenEvent::Minted`).
    "printed",
    "assumed",
    // Why aterm's wrapper started a session on its stored login (`TokenEvent::Fallback`,
    // `token_route::Fallback::word`).
    "claimed",
    "held",
    "busy",
    "unverified",
    "ended",
    "rejected",
    "mismatched",
    "unreadable",
    // Why a tab's sighting is noted ([`SeenWhy`]); `seen` above is the hourly refresh.
    "first",
    "changed",
    // How a sign-in that left its row without a login ended
    // ([`super::accounts::AttemptOutcome::as_str`]).
    "no-route",
    "failed",
    "blocked",
];

/// One journal note: an event about one account, in the journal's closed vocabulary. `Copy`,
/// with no heap value, so noting one allocates nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Note {
    /// When, Unix seconds.
    pub t: u64,
    /// One of [`KINDS`].
    pub kind: &'static str,
    /// The account; only a `probe` of an unpooled CLI whose login the read did not see names
    /// none, attributed by the realm its CLI word fixes ([`probe_note`]).
    pub account: Option<Key>,
    /// The CLI's word ([`Cli::id`], or [`Read::cli`] for a provider read).
    pub cli: &'static str,
    /// The provider workspace a reading or wall is of ([`SubKey::workspace`]).
    pub sub: Option<SubKey>,
    /// One of [`WINDOWS`].
    pub window: Option<&'static str>,
    /// Tenths of a percent used, at most 1000.
    pub pct_x10: Option<u16>,
    /// The window's reset, Unix seconds.
    pub reset: Option<u64>,
    /// One of [`RESES`].
    pub res: Option<&'static str>,
    /// Until when a wall stands, Unix seconds.
    pub until: Option<u64>,
    /// One of [`SOURCES`].
    pub src: &'static str,
    /// One of [`WHYS`].
    pub why: Option<&'static str>,
    /// The other account of a switch or a displacement; the believed account of a
    /// `wrong-account` a tab's observation contradicted.
    pub to: Option<Key>,
    /// The tab a sighting, a wrong account or a session is about.
    pub tab: Option<Tab>,
    /// The harness word of that tab's session (`[a-z0-9-]`, 16 bytes at most).
    pub harness: Option<&'static str>,
}

impl Note {
    fn new(t: u64, kind: &'static str, account: Key, cli: Cli, src: &'static str) -> Note {
        Note::worded(t, kind, account, cli.id(), src)
    }

    /// [`Note::new`] by the CLI's word ([`ObservedCli::id`]).
    fn worded(
        t: u64,
        kind: &'static str,
        account: Key,
        cli: &'static str,
        src: &'static str,
    ) -> Note {
        Note {
            t,
            kind,
            account: Some(account),
            cli,
            sub: None,
            window: None,
            pct_x10: None,
            reset: None,
            res: None,
            until: None,
            src,
            why: None,
            to: None,
            tab: None,
            harness: None,
        }
    }
}

/// A Claude Code setup-token's life on this Mac, as a journal note's kind
/// (`docs/DESIGN-claude-setup-token-2026-11-05.md` §4.6, §5.6, §5.9). A token note names the
/// account by its key alone: it carries no value, entry text, email or reason line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenEvent {
    /// The verb made a token and the vault took it, or holds it waiting (`token-minted`).
    Minted,
    /// The first status-line reading under the token (`token-served`).
    Served,
    /// The minting Mac verified the token's account (`token-verified`).
    Verified,
    /// Claude Code refused the token on this Mac (`token-rejected`).
    Rejected,
    /// aterm's wrapper started a session on its stored login instead (`token-fallback`).
    Fallback,
    /// A surface offered a token for the account (`token-offered`).
    Offered,
    /// The verb removed the account's token from the vault (`token-removed`).
    Removed,
}

impl TokenEvent {
    /// The kind word, one of [`KINDS`].
    #[must_use]
    pub const fn kind(self) -> &'static str {
        match self {
            Self::Minted => "token-minted",
            Self::Served => "token-served",
            Self::Verified => "token-verified",
            Self::Rejected => "token-rejected",
            Self::Fallback => "token-fallback",
            Self::Offered => "token-offered",
            Self::Removed => "token-removed",
        }
    }
}

/// The note of `event` about Claude Code's `account` at `t`: the verb's own steps (minted,
/// removed, offered) are a sign-in flow's, the rest Claude Code's output. `why` is one of
/// [`WHYS`] (a minted token's `printed` or `assumed` end), or none.
#[must_use]
pub fn token_note(event: TokenEvent, account: Key, t: u64, why: Option<&'static str>) -> Note {
    let src = match event {
        TokenEvent::Minted | TokenEvent::Removed | TokenEvent::Offered => "signin-flow",
        TokenEvent::Served | TokenEvent::Verified | TokenEvent::Rejected | TokenEvent::Fallback => {
            "cli-output"
        }
    };
    let mut note = Note::new(t, event.kind(), account, Cli::Claude, src);
    note.why = why.filter(|why| WHYS.contains(why));
    note
}

/// The note of a token the capture made for another account than the one asked for
/// (`wrong-account`, design §4.5): the account asked for, by its key.
#[must_use]
pub fn wrong_token_account(account: Key, t: u64) -> Note {
    Note::new(t, "wrong-account", account, Cli::Claude, "signin-flow")
}

/// The CLI an observed login is of: a pooled CLI, or Muse Code, which keeps no pool and no
/// roster, so its login is known by observation alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ObservedCli {
    /// A pooled CLI.
    Pool(Cli),
    /// Muse Code.
    Muse,
}

impl ObservedCli {
    /// The CLI's word, as a note and an `account-obs` row carry it.
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::Pool(cli) => cli.id(),
            Self::Muse => "muse",
        }
    }

    /// The pooled CLI, where it is one: the one whose roster holds a belief to reconcile.
    #[must_use]
    pub const fn pool(self) -> Option<Cli> {
        match self {
            Self::Pool(cli) => Some(cli),
            Self::Muse => None,
        }
    }
}

impl From<Cli> for ObservedCli {
    fn from(cli: Cli) -> Self {
        Self::Pool(cli)
    }
}

/// Where a tab's observed login was read, as a `seen` note's source word.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ObservedSrc {
    /// Claude Code's profile (`.claude.json` and its variants).
    Profile,
    /// A CLI's auth file (Codex, Muse, Grok, Kimi).
    AuthFile,
    /// A CLI's own log (agy).
    CliLog,
    /// The vault token entry a token route runs on.
    VaultToken,
}

impl ObservedSrc {
    /// The source word, one of [`SOURCES`].
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Profile => "profile",
            Self::AuthFile => "auth-file",
            Self::CliLog => "cli-log",
            Self::VaultToken => "vault-token",
        }
    }
}

/// Why a tab's sighting is noted now: its (tab, account) pair is new, the tab's account changed,
/// or the hourly refresh of a pair still running.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SeenWhy {
    First,
    Changed,
    Refresh,
}

impl SeenWhy {
    /// The reason word, one of [`WHYS`]: a first sighting and a change are sharp, the refresh is
    /// not (`aterm_db::alab::journal::Event::sharp`).
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::First => "first",
            Self::Changed => "changed",
            Self::Refresh => "seen",
        }
    }
}

/// The note of a tab's sighting of `cli`'s account `account` at `t`
/// (`docs/DESIGN-account-discovery-2026-11-09.md`): read from `src`, in `tab`, run by `harness`.
#[must_use]
pub fn seen_note(
    cli: impl Into<ObservedCli>,
    account: Key,
    tab: Tab,
    harness: &'static str,
    src: ObservedSrc,
    why: SeenWhy,
    t: u64,
) -> Note {
    let mut note = Note::worded(t, "seen", account, cli.into().id(), src.word());
    note.tab = Some(tab);
    note.harness = Some(harness);
    note.why = Some(why.word());
    note
}

/// The note of the machine sweep's sighting of `cli`'s account `account` at `t`, read from `src`
/// in a login place on this Mac: a `seen` that names no tab and no harness, since no tab's run
/// is what it saw (`docs/DESIGN-account-discovery-2026-11-09.md`, step 4).
#[must_use]
pub fn idle_seen_note(
    cli: impl Into<ObservedCli>,
    account: Key,
    src: ObservedSrc,
    why: SeenWhy,
    t: u64,
) -> Note {
    let mut note = Note::worded(t, "seen", account, cli.into().id(), src.word());
    note.why = Some(why.word());
    note
}

/// The note of a tab whose login file names `observed` while the roster believed `believed`
/// (`wrong-account` with `to` the believed account and `x` the tab).
#[must_use]
pub fn wrong_account_note(
    cli: impl Into<ObservedCli>,
    observed: Key,
    believed: Option<Key>,
    tab: Tab,
    src: ObservedSrc,
    t: u64,
) -> Note {
    let mut note = Note::worded(t, "wrong-account", observed, cli.into().id(), src.word());
    note.to = believed;
    note.tab = Some(tab);
    note
}

/// The `quota` notes of a tap reading `read` of `account`, one per window it carried: what a
/// tap reads of an account no roster row here names (`docs/DESIGN-account-discovery-2026-11-09.md`,
/// decision 7), which the fleet and the projection take from the journal alone, never from a
/// row's record.
#[must_use]
pub fn quota_notes(cli: Cli, account: Key, read: &super::accounts::TapRead) -> Vec<Note> {
    [
        (super::accounts::SESSION_LIMIT, read.five_hour),
        (super::accounts::WEEK_LIMIT, read.seven_day),
    ]
    .into_iter()
    .filter_map(|(name, window)| {
        let window = window?;
        let fig = WindowFig {
            pct_x10: window.pct_x10,
            reset: window.reset,
            at: read.at,
            by: String::new(),
            source: super::accounts::SOURCE_TAP.to_owned(),
            epoch_at: None,
        };
        of_reading(cli, account, name, &fig)
    })
    .collect()
}

/// The state of a login this Mac observed ([`ObservedLogin`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoginState {
    /// A running tab's CLI reads it.
    Running,
    /// It is present on this Mac with no tab on it.
    Idle,
    /// The file is there and names no account.
    SignedOut,
}

impl LoginState {
    /// The registry's `account-obs.state` word.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Idle => "idle",
            Self::SignedOut => "signed-out",
        }
    }
}

/// One login this Mac observed, for the registry's `account-obs` rows (the sink's
/// [`Sink::observed`]): the account by its claims realm and login email, never a credential.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObservedLogin {
    pub cli: ObservedCli,
    /// The claims realm ([`observed_claims`]).
    pub realm: String,
    /// The login email the file names.
    pub email: String,
    pub state: LoginState,
    /// The tab that runs it, where one does.
    pub tab: Option<Tab>,
    /// When, Unix seconds.
    pub at: u64,
    pub src: ObservedSrc,
}

/// Where notes go, and how an account is keyed: the `aterm` executable's journal glue.
pub trait Sink: Send + Sync {
    /// The account key of the canonical claims `realm` and claims email (design §5), or none.
    fn account_pk(&self, realm: &str, email: &str) -> Option<String>;
    /// Note one event: a bounded append, no lock, no allocation; a failure is dropped. Returns
    /// whether the journal took the note: false where it is off, has no spool, its spool is
    /// full or the note is not one of its events.
    fn note(&self, note: &Note) -> bool;
    /// Take up a login this Mac observed, for the registry: the executable hands it to a thread
    /// of its own and never makes the caller wait. Nothing by default.
    fn observed(&self, _login: &ObservedLogin) {}
}

static SINK: OnceLock<&'static dyn Sink> = OnceLock::new();

#[cfg(any(test, debug_assertions))]
type Keyer = fn(&str, &str) -> Option<String>;

#[cfg(any(test, debug_assertions))]
thread_local! {
    static KEYER: std::cell::Cell<Option<Keyer>> = const {
        std::cell::Cell::new(None)
    };
}

/// Run `body` with this thread's keyer, restoring the previous one even on unwind.
/// This changes key lookup only; notes still go exclusively to the installed sink.
#[cfg(any(test, debug_assertions))]
pub fn with_keyer<T>(keyer: Keyer, body: impl FnOnce() -> T) -> T {
    struct Restore(Option<Keyer>);
    impl Drop for Restore {
        fn drop(&mut self) {
            KEYER.with(|held| held.set(self.0));
        }
    }
    let _restore = Restore(KEYER.with(|held| held.replace(Some(keyer))));
    body()
}

fn account_pk(realm: &str, email: &str) -> Option<String> {
    #[cfg(any(test, debug_assertions))]
    if let Some(keyer) = KEYER.with(std::cell::Cell::get) {
        return keyer(realm, email);
    }
    SINK.get()?.account_pk(realm, email)
}

/// Install the process's journal sink; the first install wins.
pub fn install(sink: &'static dyn Sink) {
    let _ = SINK.set(sink);
}

/// Note `note` through the installed sink; nothing without one.
pub fn note(note: &Note) {
    let _ = note_taken(note);
}

/// [`note`], saying whether the journal took it ([`Sink::note`]): false without a sink. For a
/// caller that keeps what it noted, so a note the journal did not take is made again.
#[must_use]
pub fn note_taken(note: &Note) -> bool {
    SINK.get().is_some_and(|sink| sink.note(note))
}

/// Hand `login` to the installed sink's registry path; nothing without one.
pub fn observed(login: &ObservedLogin) {
    if let Some(sink) = SINK.get() {
        sink.observed(login);
    }
}

/// The claims realm and claims email of `cli`'s observed login `email` ([`LeaseKey::for_cli`]),
/// as the registry and the keyer take it.
#[must_use]
pub fn claims_of(cli: Cli, email: &str) -> Option<(String, String)> {
    let lease = LeaseKey::for_cli(cli, None, email).ok()?;
    Some((lease.pool, lease.email))
}

/// The claims realm and claims email of an observed login `email` of `cli`, as the registry and
/// the keyer take it: a pooled CLI's own claims ([`claims_of`]), but the provider's realm for
/// agy (`google`) and Muse Code (`meta`), whose accounts the registry keys by provider
/// ([`realm_key`]) since neither is in the claims ring's key. `region` is the account system
/// the login was observed in, which for Kimi Code is the realm (`kimi:global` or
/// `kimi:mainland-cn`).
#[must_use]
pub fn observed_claims(
    cli: ObservedCli,
    region: Option<Region>,
    email: &str,
) -> Option<(String, String)> {
    let realm = match cli {
        ObservedCli::Pool(Cli::Agy) => "google",
        ObservedCli::Muse => "meta",
        ObservedCli::Pool(cli @ (Cli::Claude | Cli::Codex | Cli::Grok | Cli::Kimi)) => {
            let lease = LeaseKey::for_cli(cli, region, email).ok()?;
            return Some((lease.pool, lease.email));
        }
    };
    // The claims spelling of the address (validated, trimmed, lower case), under the
    // provider's realm.
    let (_, email) = claims_of(Cli::Agy, email)?;
    Some((realm.to_owned(), email))
}

/// The account key of an unpooled CLI's login, by its realm (`google` for agy, `meta` for Muse)
/// and the login its own files name, through the installed sink: what a provider read of agy or
/// Muse is attributed to ([`ProbeEvent::login`]). None without a sink or scoped test keyer.
#[must_use]
pub fn realm_key(realm: &str, login: &str) -> Option<Key> {
    Key::new(&account_pk(realm, login)?)
}

/// The lab key of the Claude Code account whose claims email is `email` (design §5): what a
/// row-less session's account is keyed by, through the installed sink or a scoped test keyer.
#[must_use]
pub fn claude_email_key(email: &str) -> Option<Key> {
    let lease = LeaseKey::for_cli(Cli::Claude, None, email).ok()?;
    Key::new(&account_pk(&lease.pool, &lease.email)?)
}

/// The current row's canonical claims key, independently of a previously learned label key.
#[must_use]
pub fn account_key(cli: Cli, row: &super::accounts::Row) -> Option<Key> {
    account_key_with(cli, row, &account_pk)
}

/// [`account_key`] through an explicit canonical realm-and-email keyer.
#[must_use]
pub fn account_key_with(
    cli: Cli,
    row: &super::accounts::Row,
    keyer: &dyn Fn(&str, &str) -> Option<String>,
) -> Option<Key> {
    let lease = LeaseKey::for_cli(cli, row.vendor.region, row.expected_identity_email()?).ok()?;
    Key::new(&keyer(&lease.pool, &lease.email)?)
}

/// Send each note to the installed sink, once its transaction is durable.
pub fn publish(notes: &[Note]) {
    for each in notes {
        note(each);
    }
}

/// The account key of `cli`'s roster row with `label`, through `keyer` (realm, claims email).
fn row_key(
    cli: Cli,
    roster: &Roster,
    label: &str,
    keyer: &dyn Fn(&str, &str) -> Option<String>,
) -> Option<String> {
    let row = roster.row(label)?;
    account_key_with(cli, row, keyer).map(|key| key.as_str().to_owned())
}

/// Key every account of `state` that a row of `roster` names by the installed sink's account
/// key; nothing without a sink. A row's key follows the email its row names now, as the claims
/// key does, so a relabelled login is keyed as the account it holds; what was observed under the
/// key it had stays under that key, in [`State::forgotten`](super::accounts::State::forgotten).
pub fn learn(cli: Cli, roster: &Roster, state: &mut State) {
    learn_with(cli, roster, state, &account_pk);
}

/// [`learn`] with an explicit keyer.
pub fn learn_with(
    cli: Cli,
    roster: &Roster,
    state: &mut State,
    keyer: &dyn Fn(&str, &str) -> Option<String>,
) {
    let mut handed_off = Vec::new();
    for (label, account) in &mut state.accounts {
        if let Some(pk) = row_key(cli, roster, label, keyer)
            && account.pk.as_deref() != Some(pk.as_str())
        {
            // The row now names another account: what this Mac observed under the label is the
            // account it was keyed as, so it is filed under that key, never carried to the new one.
            if let Some(was) = account.pk.replace(pk) {
                handed_off.push((was, super::accounts::take_observations(account)));
            }
        }
    }
    for (was, observed) in handed_off {
        super::accounts::remember_forgotten(state, was, observed);
    }
    // A row of an account this Mac forgot takes back what it observed of it, whatever the row is
    // called now: observations are the account's, keyed by its lab key beside the label
    // (`accounts::forget_account`), so a removed and re-added or renamed row starts from them.
    if state.forgotten.is_empty() {
        return;
    }
    for row in &roster.rows {
        let Some(pk) = row_key(cli, roster, &row.label, keyer) else {
            continue;
        };
        let Some(held) = state.forgotten.remove(&pk) else {
            continue;
        };
        let account = state.accounts.entry(row.label.clone()).or_default();
        account.pk = Some(pk);
        super::accounts::fill_observations(account, held);
    }
}

fn key_of(state: &State, label: &str) -> Option<Key> {
    state.accounts.get(label)?.pk.as_deref().and_then(Key::new)
}

fn secs(t: i64) -> Option<u64> {
    u64::try_from(t).ok().filter(|t| *t > 0)
}

/// The journal's window word for the harness's window name.
fn window_word(name: &str) -> Option<&'static str> {
    match name {
        super::accounts::SESSION_LIMIT => Some("5h"),
        super::accounts::WEEK_LIMIT => Some("7d"),
        other if other.starts_with("seven_day_") => Some("7d-model"),
        _ => None,
    }
}

/// The note a ledger row becomes, keyed by `pk` (and `to` for a switch or a displacement), or
/// none for a kind the journal does not take (a sample, a proof, a pin, a warning, a move, a
/// wall, which the state's diff notes with its reset, and a probe claim, which
/// [`probe_note`] notes from the read the claim led to), for a sign-in that signed its row in,
/// whose `renewed` or `signed-in` row notes it, or for a step of one
/// ([`super::accounts::sign_in_step`]). A failed sign-in carries how it ended as one word, never
/// its reason line.
fn of_ledger(cli: Cli, event: &Event, pk: Key, to: Option<Key>) -> Option<Note> {
    let t = secs(event.t)?;
    let (kind, src, why) = match event.kind {
        Kind::Rotate => ("switch", "cli-output", None),
        Kind::ResetUsed | Kind::Reset => ("reset-used", "cli-output", None),
        Kind::ResetFailed => ("error", "error", Some("reset-failed")),
        Kind::Auth => match Auth::parse(&event.why) {
            Auth::SignedOut => ("signed-out", "cli-output", None),
            Auth::Ok => ("auth", "cli-output", Some("ok")),
            Auth::Unseen => ("auth", "cli-output", Some("unseen")),
        },
        Kind::NoTap => ("error", "error", Some("no-tap")),
        Kind::Credits => ("credits", "cli-output", None),
        Kind::Renewed => ("renewed", "signin-flow", None),
        Kind::SignedIn => ("signed-in", "signin-flow", None),
        Kind::LoginEnded => ("login-ended", "cli-output", None),
        Kind::WrongAccount => ("wrong-account", "signin-flow", None),
        Kind::Epoch => ("auth", "cli-output", Some("epoch")),
        Kind::SignIn if super::accounts::sign_in_step(&event.why) => return None,
        Kind::SignIn => match super::accounts::AttemptOutcome::of_sign_in(&event.why) {
            Some(super::accounts::AttemptOutcome::SignedIn) => return None,
            outcome => (
                "sign-in-failed",
                "signin-flow",
                outcome.map(super::accounts::AttemptOutcome::as_str),
            ),
        },
        Kind::Displaced => ("displaced", "signin-flow", None),
        Kind::Wall
        | Kind::Probe
        | Kind::Sample
        | Kind::Proven
        | Kind::Struck
        | Kind::Move
        | Kind::Use
        | Kind::Unpin
        | Kind::ResetKept
        | Kind::Warned => return None,
    };
    let mut note = Note::new(t, kind, pk, cli, src);
    note.why = why;
    if matches!(event.kind, Kind::Rotate | Kind::Displaced) {
        note.to = to;
    }
    Some(note)
}

/// The note a provider usage read is (step 9's report door, [`ProbeEvent`]): a `probe` with
/// why the account is in use as its reason. A pooled read is of the account the roster row
/// `roster(cli)` names it by (through `keyer`, as [`learn`] keys); none where no roster row here
/// keys its label. An unpooled read (agy, Muse) names no row and is attributed by realm: its CLI
/// word fixes the realm, and its account is the login the read saw ([`ProbeEvent::login`]) where
/// it saw one, so every such read is on record. None for a time before the epoch. It reads one
/// roster and no state: the door reports before a network request or a CLI run, and a state load
/// could write (a machine id).
#[must_use]
pub fn probe_note(
    roster: &dyn Fn(Cli) -> Option<Roster>,
    keyer: &dyn Fn(&str, &str) -> Option<String>,
    event: &ProbeEvent,
) -> Option<Note> {
    let t = secs(event.at)?;
    let cli = match event.read {
        Read::ClaudePanel => Cli::Claude,
        Read::KimiPanel => Cli::Kimi,
        Read::CodexEndpoint => Cli::Codex,
        Read::AgyUsage | Read::MuseUsage => {
            return Some(Note {
                t,
                kind: "probe",
                account: event.login,
                cli: event.read.cli(),
                sub: None,
                window: None,
                pct_x10: None,
                reset: None,
                res: None,
                until: None,
                src: "probe",
                why: Some(event.why.word()),
                to: None,
                tab: None,
                harness: None,
            });
        }
    };
    let label = event.account.as_deref()?;
    let pk = Key::new(&row_key(cli, &roster(cli)?, label, keyer)?)?;
    let mut note = Note::new(t, "probe", pk, cli, "probe");
    note.why = Some(event.why.word());
    Some(note)
}

/// A quota note for a window reading.
fn of_reading(cli: Cli, pk: Key, name: &str, fig: &WindowFig) -> Option<Note> {
    let window = window_word(name)?;
    let (src, res) = match (fig.source.as_str(), cli) {
        (super::accounts::SOURCE_TAP, Cli::Codex) => ("rollout", "exact"),
        (super::accounts::SOURCE_TAP, _) => ("statusline", "exact"),
        // A `/usage` page names its reset to the hour ("Resets 1pm").
        (super::accounts::SOURCE_SAMPLE, _) => ("session-usage", "hour"),
        _ => return None,
    };
    let mut note = Note::new(secs(fig.at)?, "quota", pk, cli, src);
    note.window = Some(window);
    note.pct_x10 = Some(u16::try_from(fig.pct_x10.min(1000)).unwrap_or(1000));
    note.reset = secs(fig.reset);
    note.res = Some(res);
    Some(note)
}

/// A reading changed enough to note: a point or more, or another reset (design §8).
fn moved(before: Option<&WindowFig>, after: &WindowFig) -> bool {
    before.is_none_or(|b| b.reset != after.reset || b.pct_x10.abs_diff(after.pct_x10) >= 10)
}

/// What one transaction did, as notes: `events` (its ledger rows) gain their account's key,
/// and each becomes a note where the journal takes its kind; then the sessions `after` gained
/// or lost, each window reading that moved a point or its reset, and each wall seen or cleared.
/// Only accounts with a learned key are noted; `now` is the transaction's time.
pub fn observe(
    cli: Cli,
    before: &State,
    after: &State,
    events: &mut [Event],
    now: i64,
) -> Vec<Note> {
    let key = |label: &str| key_of(after, label).or_else(|| key_of(before, label));
    let mut notes = Vec::new();
    for event in events.iter_mut() {
        let Some(pk) = key(&event.account) else {
            continue;
        };
        event.pk = Some(pk.as_str().to_owned());
        let to = event.to.as_deref().and_then(key);
        notes.extend(of_ledger(cli, event, pk, to));
    }
    let at = secs(now);
    let session = |kind, label: &str| Some(Note::new(at?, kind, key(label)?, cli, "cli-output"));
    for (sid, row) in &after.sessions {
        match before.sessions.get(sid) {
            Some(old) if old.account == row.account => {}
            Some(old) => {
                notes.extend(session("session-end", &old.account));
                notes.extend(session("session-start", &row.account));
            }
            None => notes.extend(session("session-start", &row.account)),
        }
    }
    for (sid, old) in &before.sessions {
        if !after.sessions.contains_key(sid) {
            notes.extend(session("session-end", &old.account));
        }
    }
    let empty = BTreeMap::new();
    // A reading or a wall another Mac made is that Mac's to publish, never this one's again.
    let mine = |by: &str| by.is_empty() || after.machine.is_empty() || by == after.machine;
    for (label, account) in &after.accounts {
        let Some(pk) = account.pk.as_deref().and_then(Key::new) else {
            continue;
        };
        let sub = account.key.as_ref().and_then(SubKey::workspace);
        let old = before.accounts.get(label);
        let old_windows = old.map_or(&empty, |a| &a.windows);
        for (name, fig) in &account.windows {
            if mine(&fig.by) && moved(old_windows.get(name), fig) {
                notes.extend(of_reading(cli, pk, name, fig).map(|note| Note { sub, ..note }));
            }
        }
        for (name, wall) in &account.walls {
            if !mine(&wall.by) || old.and_then(|a| a.walls.get(name)) == Some(wall) {
                continue;
            }
            let Some(t) = secs(wall.at) else { continue };
            let mut note = if wall.cleared {
                Note::new(t, "wall-cleared", pk, cli, "cli-output")
            } else {
                let mut note = Note::new(t, "wall", pk, cli, "cli-output");
                note.until = wall.until.and_then(secs);
                note
            };
            note.window = window_word(name);
            note.sub = sub;
            notes.push(note);
        }
    }
    notes
}

/// The folder in the harness state that holds the backfill's marker.
#[cfg(unix)]
const BACKFILL_DIR: &str = "alab";
/// The marker a finished backfill leaves there.
#[cfg(unix)]
const BACKFILL_MARK: &str = "backfilled";
/// How far back the backfill reaches: the spool keeps 30 days.
#[cfg(unix)]
const BACKFILL_S: i64 = 30 * 86_400;

/// Whether this Mac's backfill is done.
#[cfg(unix)]
#[must_use]
pub fn backfilled(harness_state: &Path) -> bool {
    aterm_uds::private_file::read_private(&harness_state.join(BACKFILL_DIR), BACKFILL_MARK, 16)
        .is_ok()
}

/// Mark this Mac's backfill done: the caller's word that the notes [`backfill`] returned are
/// durable, so it comes after they are, and a process that ends between the two leaves the next
/// start to backfill again (a repeated note is harmless; a lost one is not).
///
/// # Errors
/// The marker's folder cannot be made private, or the marker cannot be written.
#[cfg(unix)]
pub fn mark_backfilled(harness_state: &Path) -> std::io::Result<()> {
    let dir = harness_state.join(BACKFILL_DIR);
    aterm_uds::private_file::ensure_private_dir(&dir)?;
    aterm_uds::private_file::write_private(&dir, BACKFILL_MARK, b"1\n")
}

/// Once per Mac: the notes each CLI's local ledger holds from the last 30 days, and the state's
/// standing facts (current window readings, standing walls, each login's word, the last time a
/// session was read on an account), each account keyed by its learned key or else the key of
/// the roster row `roster(cli)` gives. Returns the notes and writes nothing: the caller marks
/// the backfill done ([`mark_backfilled`]) once the notes are durable.
#[cfg(unix)]
#[must_use]
pub fn backfill(
    harness_state: &Path,
    user_home: &Path,
    roster: &dyn Fn(Cli) -> Option<Roster>,
    keyer: &dyn Fn(&str, &str) -> Option<String>,
    now: i64,
) -> Vec<Note> {
    let mut notes = Vec::new();
    if !harness_state.is_dir() {
        // No harness has run here yet; the next start looks again.
        return notes;
    }
    for cli in Cli::ALL {
        let store = Store::at_for(harness_state, cli, user_home);
        let Ok((mut state, _)) = store.load(now) else {
            continue;
        };
        if let Some(roster) = roster(cli) {
            learn_with(cli, &roster, &mut state, keyer);
        }
        let mut events = store
            .tail(super::accounts::LEDGER_KEEP_ROWS)
            .unwrap_or_default();
        let quiet = State {
            accounts: state
                .accounts
                .iter()
                .map(|(label, a)| {
                    let pk = a.pk.clone();
                    (
                        label.clone(),
                        super::accounts::Account {
                            pk,
                            ..Default::default()
                        },
                    )
                })
                .collect(),
            ..State::default()
        };
        let mut seeded = state.clone();
        seeded.sessions.clear();
        for account in seeded.accounts.values_mut() {
            account.windows.retain(|_, fig| fig.current(now));
            account
                .walls
                .retain(|_, wall| wall.stands_until().is_some_and(|end| now < end));
        }
        notes.extend(observe(cli, &quiet, &seeded, &mut events, now));
        for account in state.accounts.values() {
            let Some(pk) = account.pk.as_deref().and_then(Key::new) else {
                continue;
            };
            if let Some(t) = account.auth_at.and_then(secs) {
                match account.auth {
                    Auth::SignedOut => {
                        notes.push(Note::new(t, "signed-out", pk, cli, "cli-output"))
                    }
                    Auth::Ok => {
                        let mut note = Note::new(t, "auth", pk, cli, "cli-output");
                        note.why = Some("ok");
                        notes.push(note);
                    }
                    Auth::Unseen => {}
                }
            }
            if let Some(t) = account.ran_at.and_then(secs) {
                let mut note = Note::new(t, "session-end", pk, cli, "cli-output");
                note.why = Some("seen");
                notes.push(note);
            }
        }
    }
    notes.retain(|n| i64::try_from(n.t).is_ok_and(|t| t >= now.saturating_sub(BACKFILL_S)));
    notes
}

#[cfg(test)]
#[path = "journal_tests.rs"]
mod tests;
