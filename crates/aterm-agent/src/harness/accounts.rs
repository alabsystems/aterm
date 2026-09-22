// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE ACCOUNT ROSTER (design §5.6): `accounts.toml`, its rows, and the
//! selection rule that says which account a `switch-account` may rotate into.
//!
//! **No credential is read, stored, forwarded or renewed anywhere in this
//! module.** What it reads is a roster the owner wrote (labels, kinds, config
//! directories, priorities) and two NON-SECRET facts the vendor itself left on
//! disk in each config directory's `.claude.json`: the account's organisation
//! and seat tier under `oauthAccount`, and the utilisation snapshot under
//! `cachedUsageUtilization`. The reader copies a CLOSED set of keys out of
//! that file and nothing else — no token, no keychain item, no environment
//! secret — and it never writes to it.
//!
//! This is the half that was missing, and it is why rotation refused. Design
//! §5.6 owns `accounts.toml` and §5.8's table owns the `switch-account` step;
//! [`super::watch::WatchConfig`]'s `accounts_enabled` and `account_dir` are
//! the two fields the actuator reads, and before this module existed NOTHING
//! wrote them — so `plan_limits` answered `refused:unresolved` for ever while
//! the table row and its tests read as live.
//!
//! **The selection rule is §5.6 verbatim**, with §5.8.10's exclusion:
//! *lowest seven-day utilisation among accounts whose five-hour window is not
//! exhausted, ties by priority*, and an account whose last observed state is
//! `login-expired` is NOT a candidate — an expired login is renewed by a
//! human at the vendor's own door, never routed around.
//!
//! **What this module does not know, stated.** It has no clock and no
//! network: every utilisation figure is a cache the vendor wrote, carrying
//! its own `fetchedAtMs`, and the caller decides what age is too old. It does
//! not know which account a session is really running under beyond the
//! `CLAUDE_CONFIG_DIR` it was handed. And it holds no history: the auth state
//! of an account is supplied by the caller ([`AuthState`]), because the only
//! account whose live state this process can observe is the active one.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use aterm_json::Value;

use super::align::{Block, Val, read_toml};
use super::limits::EXHAUSTED_PCT;
use super::truncate_bytes;

/// The file, under the harness state root (`<prefix>/harness/<name>/`).
pub const ACCOUNTS_FILE: &str = "accounts.toml";

/// The most rows one roster may carry. A rotation set is a handful of the
/// owner's own subscriptions, not a directory.
pub const MAX_ACCOUNTS: usize = 32;

/// The longest label, config directory and identity string kept.
pub const FIELD_CAP: usize = 256;

/// The most bytes of a `.claude.json` this module will read. The measured
/// file on this machine is far smaller; the cap is the fence against a file
/// that is not what it claims to be.
pub const MAX_CLAUDE_JSON_BYTES: u64 = 8 * 1024 * 1024;

/// The most directories `discover` will look at.
pub const MAX_DISCOVER_ENTRIES: usize = 1024;

// A second `EXHAUSTED_PCT` used to live here at 100.0, beside the 95.0 in
// [`super::limits`]. Two numbers for one word is two answers to "is this
// window exhausted?", and the design arbitrates: §5.8.2 gives exactly one
// figure — "any window with `source ≠ none` at ≥ 95 %" — while §5.6's
// selection rule says only "whose five-hour window is not exhausted" and
// names no number of its own. So the design's number is the one number, and
// the local invention is deleted rather than converted between.
//
// The change is also the SAFE direction: at 97 % the roster used to call an
// account a rotation candidate that the classifier would have called
// exhausted, and rotating into it buys a session that hits the wall almost
// at once.

// ---------------------------------------------------------------------------
// The rows
// ---------------------------------------------------------------------------

/// How an account's credentials reach the vendor. The design names four;
/// TWO are admitted here, and [`Kind::REFUSED`] says why the other two are
/// not and exactly what would admit them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// `CLAUDE_CONFIG_DIR` — the sanctioned multi-account mechanism, and the
    /// only kind an automatic rotation can use, because a relaunch carries a
    /// config DIRECTORY and nothing else.
    ConfigDir,
    /// A `secrets/<label>.env` the PRELUDE sources at exec. Listed, never
    /// selected here: selecting it would mean this module knowing where a
    /// credential lives, and it does not read one.
    ApiKey,
}

impl Kind {
    /// The schema-1 spelling, as `accounts.toml` writes it.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::ConfigDir => "config-dir",
            Kind::ApiKey => "api-key",
        }
    }

    /// Read one `kind =` value. [`Kind::REFUSED`]'s two names are refused BY
    /// NAME with their reason, so a roster that carries one says why it did
    /// not load; anything else is simply not a kind.
    #[must_use]
    pub fn parse(token: &str) -> Option<Kind> {
        match token {
            "config-dir" => Some(Kind::ConfigDir),
            "api-key" => Some(Kind::ApiKey),
            _ => None,
        }
    }

    /// The design's OTHER two kinds, each with the reason this build refuses
    /// it and the measurement that would lift the refusal.
    ///
    /// They are refused rather than implemented, and the reason is the same
    /// for both: **this module's whole contract is that it reads no
    /// credential**, and the two cloud kinds have no credential-free identity
    /// to read. A `config-dir` row is selectable because the vendor itself
    /// leaves two NON-SECRET facts in that directory — `oauthAccount`'s
    /// organisation and seat tier, and the `cachedUsageUtilization` snapshot
    /// — so §5.6's selection rule (lowest seven-day utilisation among
    /// accounts whose five-hour window is not exhausted) has inputs. An
    /// `api-key` row is LISTED and never selected for the same reason from
    /// the other side: the harness would have to know where the key lives to
    /// select it, so the prelude sources it at exec and this module only
    /// names the row.
    ///
    /// A `bedrock` or `vertex` row is the api-key case WITHOUT the prelude's
    /// answer. Every fact a rotation needs about one — is it authenticated,
    /// what is its remaining window, does a relaunch bind to it — lives
    /// behind a cloud provider's own credential chain (`AWS_PROFILE`, a role
    /// assumption, an ADC file, a workload-identity token), and:
    ///
    /// * NONE of them writes a non-secret utilisation snapshot the vendor's
    ///   config directory carries, so §5.6's selection rule has no input and
    ///   would have to fall back to "first row wins", which is not a rotation;
    /// * reading any of them means reading a credential chain, which this
    ///   module does not do and is the one rule it cannot bend;
    /// * the relaunch seam is `CLAUDE_CONFIG_DIR`, one directory and nothing
    ///   else ([`Act::Relaunch`](super::watch::Act::Relaunch)), and neither
    ///   cloud kind is selected by a directory.
    ///
    /// WHAT WOULD ADMIT THEM, named so this is a decision and not a gap:
    /// a MEASURED, credential-free source of (a) whether that endpoint is
    /// authenticated and (b) its remaining capacity — the natural shape is
    /// the vendor writing per-endpoint rows into the same
    /// `cachedUsageUtilization` block it already writes — plus a relaunch
    /// seam that carries the endpoint selection the way `CLAUDE_CONFIG_DIR`
    /// carries the account one. Until both exist, an admitted row would be a
    /// table entry that reads as live and rotates nowhere, which is exactly
    /// the defect `accounts_enabled` was added to close.
    pub const REFUSED: [(&'static str, &'static str); 2] = [
        (
            "bedrock",
            "an Amazon Bedrock endpoint is selected by an AWS credential chain, not by a config \
             directory, and it publishes no credential-free utilisation snapshot for §5.6 to \
             rank — so an admitted row would read as live and rotate nowhere",
        ),
        (
            "vertex",
            "a Google Vertex endpoint is selected by application-default credentials, not by a \
             config directory, and it publishes no credential-free utilisation snapshot for \
             §5.6 to rank — so an admitted row would read as live and rotate nowhere",
        ),
    ];

    /// The refusal reason for `token`, where it is one of the two the design
    /// names and this build does not carry.
    #[must_use]
    pub fn refusal(token: &str) -> Option<&'static str> {
        Kind::REFUSED
            .iter()
            .find(|(name, _)| *name == token)
            .map(|(_, why)| *why)
    }
}

/// The last state anything OBSERVED for an account. Supplied by the caller;
/// this module never probes an account and never calls the network.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AuthState {
    /// Nothing has observed it. The honest default for every account but the
    /// one this session is running under.
    #[default]
    Unknown,
    /// Its config directory carries an `oauthAccount`, so the vendor has
    /// signed in there at some point. Not a proof that the token is live.
    SignedIn,
    /// Its config directory carries NO `oauthAccount`: the design's
    /// "unauthenticated — visible but not selectable".
    Unauthenticated,
    /// The classifier last read `auth` for it (design §5.8.10). NOT a
    /// rotation candidate: an expired login is renewed, not routed around.
    LoginExpired,
}

impl AuthState {
    /// The `auth=` column's word (design §5.8.10, the Settings page).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            AuthState::Unknown => "unknown",
            AuthState::SignedIn => "signed-in",
            AuthState::Unauthenticated => "unauthenticated",
            AuthState::LoginExpired => "login-expired",
        }
    }

    /// Read one word back. Anything outside the closed list is
    /// [`AuthState::Unknown`] — never an error, and never a stronger answer
    /// than the word deserved.
    #[must_use]
    pub fn parse(token: &str) -> AuthState {
        match token {
            "signed-in" => AuthState::SignedIn,
            "unauthenticated" => AuthState::Unauthenticated,
            "login-expired" => AuthState::LoginExpired,
            _ => AuthState::Unknown,
        }
    }
}

/// One roster row, exactly as `accounts.toml` carries it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// The owner's label. Unique within a roster.
    pub label: String,
    /// How its credentials reach the vendor.
    pub kind: Kind,
    /// Its `CLAUDE_CONFIG_DIR`, absolute. For [`Kind::ApiKey`] this is the
    /// directory the prelude would still run in, and may be empty.
    pub dir: String,
    /// Tie-break order, LOWEST first (design §5.6: "ties by priority").
    /// Absent reads as [`DEFAULT_PRIORITY`].
    pub priority: i64,
}

/// The priority a row with no `priority =` gets: the middle of the range, so
/// a roster can express both "before the unstated ones" and "after".
pub const DEFAULT_PRIORITY: i64 = 100;

/// The two NON-SECRET identity fields the vendor leaves in a config
/// directory's `.claude.json` under `oauthAccount`.
///
/// `emailAddress` is deliberately NOT read. The design redacts it by config
/// and the roster has no use for it: a label already names the account to the
/// owner, and an address in a ledger row is a personal datum this harness has
/// no reason to hold.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Identity {
    /// `oauthAccount.organizationName`.
    pub org: Option<String>,
    /// `oauthAccount.seatTier`.
    pub tier: Option<String>,
}

impl Identity {
    /// Whether anything named this account at all.
    #[must_use]
    pub fn known(&self) -> bool {
        self.org.is_some() || self.tier.is_some()
    }
}

/// One window out of a config directory's utilisation cache.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CachedWindow {
    /// Percent used, as the cache reports it. Not clamped.
    pub used_pct: f64,
    /// Epoch seconds the window resets, where the cache carried one.
    pub resets_at: Option<i64>,
}

/// Everything read out of ONE config directory's `.claude.json` — the closed
/// set, and nothing else in that file is copied anywhere.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Snapshot {
    /// Whether the file carried an `oauthAccount` object at all.
    pub signed_in: bool,
    /// The two non-secret identity fields.
    pub identity: Identity,
    /// `cachedUsageUtilization.utilization.<name>`, by the vendor's name.
    pub windows: BTreeMap<String, CachedWindow>,
    /// `cachedUsageUtilization.fetchedAtMs`, where present.
    pub fetched_at_ms: Option<i64>,
}

impl Snapshot {
    /// The cache's age in seconds at `now` (epoch seconds), where the cache
    /// said when it was fetched. A cache from the future reads `0` rather
    /// than a negative age.
    #[must_use]
    pub fn age_s(&self, now: i64) -> Option<u64> {
        let fetched = self.fetched_at_ms? / 1000;
        Some(u64::try_from(now.saturating_sub(fetched)).unwrap_or(0))
    }

    /// One window by the vendor's name (`five_hour`, `seven_day`, …).
    #[must_use]
    pub fn window(&self, name: &str) -> Option<CachedWindow> {
        self.windows.get(name).copied()
    }
}

/// The vendor's window names this module asks about by name.
pub const FIVE_HOUR: &str = "five_hour";
/// The seven-day window the selection rule orders by.
pub const SEVEN_DAY: &str = "seven_day";

// ---------------------------------------------------------------------------
// The roster
// ---------------------------------------------------------------------------

/// An admitted `accounts.toml`.
///
/// `enabled` is the per-capability consent of design §5.6 ("only when
/// `[accounts] enabled = true`"). DEVIATION, stated rather than hidden: the
/// design puts that key in the harness's own `config.toml`, and no
/// `config.toml` reader ships yet. It is read here from the same `[accounts]`
/// block spelling, in the file this module already owns, so moving it later
/// is a path change and not a grammar change. Default OFF.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Roster {
    /// `[accounts] enabled` — rotation consent. OFF by default.
    pub enabled: bool,
    /// The rows, in file order.
    pub rows: Vec<Row>,
}

impl Roster {
    /// Read one `accounts.toml`, or name the first thing that breaks it.
    ///
    /// Admission is WHOLE-FILE and fail-closed, exactly as the contract's is
    /// ([`super::align`]): one bad row refuses the file rather than loading
    /// the rest, because a roster loaded minus a row is a roster that rotates
    /// somewhere the owner did not write.
    ///
    /// # Errors
    ///
    /// The file leaves the reader's TOML subset, a block is not `[accounts]`
    /// or `[[account]]`, a row is missing `label` or `dir`, a `kind` is
    /// outside the closed list, two rows share a label, or there are more
    /// rows than [`MAX_ACCOUNTS`].
    pub fn parse(text: &str) -> Result<Roster, String> {
        let blocks = read_toml(text, ACCOUNTS_FILE)?;
        let mut roster = Roster::default();
        for block in &blocks {
            match (block.name.as_str(), block.array) {
                ("", false) => {
                    if !block.keys.is_empty() {
                        return Err(format!(
                            "{ACCOUNTS_FILE}: keys before the first block; every key belongs \
                             under [accounts] or [[account]]"
                        ));
                    }
                }
                ("accounts", false) => {
                    roster.enabled = matches!(block.get("enabled"), Some(Val::Bool(true)));
                }
                ("account", true) => {
                    if roster.rows.len() >= MAX_ACCOUNTS {
                        return Err(format!(
                            "{ACCOUNTS_FILE}: more than {MAX_ACCOUNTS} [[account]] rows"
                        ));
                    }
                    roster.rows.push(row_of(block)?);
                }
                (other, _) => {
                    return Err(format!(
                        "{ACCOUNTS_FILE}: unknown block [{other}]; this grammar has [accounts] \
                         and [[account]]"
                    ));
                }
            }
        }
        if let Some(dup) = first_duplicate(&roster.rows) {
            return Err(format!("{ACCOUNTS_FILE}: label {dup:?} is declared twice"));
        }
        Ok(roster)
    }

    /// Read the roster at `path`. A file that is not there is the EMPTY
    /// roster with rotation off — the normal state of a machine that has
    /// never configured one, and never an error, because nothing about a
    /// missing roster may make the harness un-launchable (design §1.4).
    ///
    /// # Errors
    ///
    /// The file exists and does not admit.
    pub fn load(path: &Path) -> Result<Roster, String> {
        match std::fs::read_to_string(path) {
            Ok(text) => Roster::parse(&text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Roster::default()),
            Err(e) => Err(format!("{}: {e}", path.display())),
        }
    }

    /// The row with this label.
    #[must_use]
    pub fn row(&self, label: &str) -> Option<&Row> {
        self.rows.iter().find(|r| r.label == label)
    }

    /// The row whose `dir` is `dir` — how the ACTIVE account is named, since
    /// what a session carries is `CLAUDE_CONFIG_DIR` and not a label.
    #[must_use]
    pub fn row_for_dir(&self, dir: &Path) -> Option<&Row> {
        self.rows
            .iter()
            .find(|r| !r.dir.is_empty() && Path::new(&r.dir) == dir)
    }
}

fn first_duplicate(rows: &[Row]) -> Option<&str> {
    rows.iter()
        .enumerate()
        .find(|(i, r)| rows[..*i].iter().any(|p| p.label == r.label))
        .map(|(_, r)| r.label.as_str())
}

fn row_of(block: &Block) -> Result<Row, String> {
    let text = |key: &str| match block.get(key) {
        Some(Val::Str(s)) => Some(truncate_bytes(s, FIELD_CAP).to_string()),
        _ => None,
    };
    let label = text("label").filter(|l| !l.is_empty()).ok_or_else(|| {
        format!("{ACCOUNTS_FILE}: an [[account]] row with no label (a non-empty string)")
    })?;
    if !admissible_label(&label) {
        return Err(format!(
            "{ACCOUNTS_FILE}: label {label:?} is outside [A-Za-z0-9._-]; a label names a \
             directory on a relaunch command line and is quoted into ledger rows"
        ));
    }
    let kind_token = text("kind").unwrap_or_else(|| Kind::ConfigDir.as_str().to_string());
    let kind = Kind::parse(&kind_token).ok_or_else(|| {
        // A design-named kind refuses WITH ITS REASON; anything else refuses
        // as an unknown word. A bare refusal with no reason is what this
        // message used to be.
        match Kind::refusal(&kind_token) {
            Some(why) => format!(
                "{ACCOUNTS_FILE}: account {label:?} has kind {kind_token:?}, which this build \
                 refuses: {why}. See `Kind::REFUSED` for what would admit it."
            ),
            None => format!(
                "{ACCOUNTS_FILE}: account {label:?} has kind {kind_token:?}; this build admits \
                 {} and {}",
                Kind::ConfigDir.as_str(),
                Kind::ApiKey.as_str()
            ),
        }
    })?;
    let dir = text("dir").unwrap_or_default();
    if kind == Kind::ConfigDir && !Path::new(&dir).is_absolute() {
        return Err(format!(
            "{ACCOUNTS_FILE}: account {label:?} is a {} row whose dir {dir:?} is not absolute; a \
             relaunch exports CLAUDE_CONFIG_DIR and a relative path would resolve against \
             whatever directory the relaunch happened to start in",
            Kind::ConfigDir.as_str()
        ));
    }
    let priority = match block.get("priority") {
        Some(Val::Int(n)) => *n,
        None => DEFAULT_PRIORITY,
        Some(_) => {
            return Err(format!(
                "{ACCOUNTS_FILE}: account {label:?} has a priority that is not an integer"
            ));
        }
    };
    Ok(Row {
        label,
        kind,
        dir,
        priority,
    })
}

// ---------------------------------------------------------------------------
// The per-directory snapshot — a CLOSED set of non-secret keys
// ---------------------------------------------------------------------------

/// Read the closed set of non-secret facts out of `<dir>/.claude.json`.
///
/// Nothing else in that file is read, copied, logged or returned. The file
/// also carries per-project history and settings, and it is NOT parsed for
/// any of it: the two keys below are looked up by name on the parsed document
/// and the document is dropped.
///
/// A file that is missing, too large, or not JSON is [`None`] — that is an
/// account nothing is known about, which is a weaker candidate, never an
/// error and never a reason to refuse the roster.
#[must_use]
pub fn snapshot(dir: &Path) -> Option<Snapshot> {
    let path = dir.join(".claude.json");
    let meta = std::fs::metadata(&path).ok()?;
    if !meta.is_file() || meta.len() > MAX_CLAUDE_JSON_BYTES {
        return None;
    }
    let text = std::fs::read_to_string(&path).ok()?;
    snapshot_of(&text)
}

/// [`snapshot`] over text that is already in hand. Separated so the tests
/// drive the same reader the filesystem path does.
#[must_use]
pub fn snapshot_of(text: &str) -> Option<Snapshot> {
    let doc: Value = aterm_json::from_str(text).ok()?;
    let mut out = Snapshot::default();
    let string = |v: Option<&Value>| {
        v.and_then(Value::as_str)
            .map(|s| truncate_bytes(s, FIELD_CAP).to_string())
            .filter(|s| !s.is_empty())
    };
    if let Some(account) = doc.get("oauthAccount").and_then(Value::as_object) {
        out.signed_in = true;
        out.identity = Identity {
            org: string(account.get("organizationName")),
            tier: string(account.get("seatTier")),
        };
    }
    if let Some(cache) = doc.get("cachedUsageUtilization").and_then(Value::as_object) {
        out.fetched_at_ms = cache.get("fetchedAtMs").and_then(number).map(|n| n as i64);
        if let Some(util) = cache.get("utilization").and_then(Value::as_object) {
            for (name, value) in util.iter().take(super::usage::MAX_WINDOWS) {
                // Two shapes are accepted because the cache carries both: a
                // bare number, and an object with `utilization`/`resetsAt`.
                // A `null` window (the measured `seven_day_opus` on this
                // box) is neither, and is simply absent.
                if let Some(pct) = number(value) {
                    out.windows.insert(
                        truncate_bytes(name, FIELD_CAP).to_string(),
                        CachedWindow {
                            used_pct: pct,
                            resets_at: None,
                        },
                    );
                } else if let Some(obj) = value.as_object()
                    && let Some(pct) = obj
                        .get("utilization")
                        .or_else(|| obj.get("used_percentage"))
                        .and_then(number)
                {
                    out.windows.insert(
                        truncate_bytes(name, FIELD_CAP).to_string(),
                        CachedWindow {
                            used_pct: pct,
                            resets_at: obj
                                .get("resetsAt")
                                .or_else(|| obj.get("resets_at"))
                                .and_then(number)
                                .map(|n| n as i64),
                        },
                    );
                }
            }
        }
    }
    Some(out)
}

fn number(v: &Value) -> Option<f64> {
    v.as_f64()
        .or_else(|| v.as_i64().map(|n| n as f64))
        .or_else(|| v.as_u64().map(|n| n as f64))
}

// ---------------------------------------------------------------------------
// The view one row presents
// ---------------------------------------------------------------------------

/// One row plus everything read about it: what `harness accounts` prints and
/// what [`select`] decides over.
#[derive(Debug, Clone, PartialEq)]
pub struct AccountState {
    /// The roster row.
    pub row: Row,
    /// Whether this is the account the live session runs under.
    pub active: bool,
    /// The last observed auth state.
    pub auth: AuthState,
    /// The config directory's snapshot, where one was readable.
    pub snapshot: Option<Snapshot>,
}

impl AccountState {
    /// A state with nothing read yet.
    #[must_use]
    pub fn new(row: Row) -> AccountState {
        AccountState {
            row,
            active: false,
            auth: AuthState::Unknown,
            snapshot: None,
        }
    }

    /// Read the config directory and fill in what it says: the snapshot, and
    /// the auth state where it is still [`AuthState::Unknown`].
    ///
    /// An auth state the CALLER already set is never overwritten — the live
    /// classifier's `login-expired` is a stronger fact than the presence of
    /// an `oauthAccount` object on disk, and §5.8.10 turns on it.
    #[must_use]
    pub fn read_dir(mut self) -> AccountState {
        if self.row.kind == Kind::ConfigDir && !self.row.dir.is_empty() {
            self.snapshot = snapshot(Path::new(&self.row.dir));
        }
        if self.auth == AuthState::Unknown {
            self.auth = match &self.snapshot {
                Some(s) if s.signed_in => AuthState::SignedIn,
                Some(_) => AuthState::Unauthenticated,
                None => AuthState::Unknown,
            };
        }
        self
    }

    /// The seven-day utilisation the selection rule orders by, where the
    /// cache carried one.
    #[must_use]
    pub fn seven_day_pct(&self) -> Option<f64> {
        self.snapshot
            .as_ref()?
            .window(SEVEN_DAY)
            .map(|w| w.used_pct)
    }

    /// Whether the five-hour window is KNOWN exhausted. An account whose
    /// cache says nothing is not known exhausted, and the tie-break below
    /// ranks it after every account with measured headroom rather than
    /// refusing it — a roster with no caches must still be able to rotate.
    #[must_use]
    pub fn five_hour_exhausted(&self) -> bool {
        self.snapshot
            .as_ref()
            .and_then(|s| s.window(FIVE_HOUR))
            .is_some_and(|w| w.used_pct >= EXHAUSTED_PCT)
    }
}

// ---------------------------------------------------------------------------
// The selection — design §5.6, with §5.8.10's exclusion
// ---------------------------------------------------------------------------

/// Why an account is not a rotation candidate. Printed, never inferred.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotCandidate {
    /// It is the account the session is already running under.
    Active,
    /// Its kind cannot be rotated into by a relaunch: only a config
    /// directory can, because that is all a relaunch carries.
    Kind,
    /// Its row names no config directory.
    NoDir,
    /// Its five-hour window is at or past [`EXHAUSTED_PCT`].
    FiveHourExhausted,
    /// Its last observed state is `login-expired` (§5.8.10): an expired
    /// login is renewed at the vendor's own door, never routed around.
    LoginExpired,
    /// Its config directory carries no `oauthAccount` — the design's
    /// "visible but not selectable".
    Unauthenticated,
}

impl NotCandidate {
    /// The schema-1 word.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            NotCandidate::Active => "active",
            NotCandidate::Kind => "kind-not-rotatable",
            NotCandidate::NoDir => "no-config-dir",
            NotCandidate::FiveHourExhausted => "five-hour-exhausted",
            NotCandidate::LoginExpired => "login-expired",
            NotCandidate::Unauthenticated => "unauthenticated",
        }
    }
}

/// Whether `state` may be rotated into, and WHY NOT when it may not.
pub fn candidacy(state: &AccountState) -> Result<(), NotCandidate> {
    if state.active {
        return Err(NotCandidate::Active);
    }
    if state.row.kind != Kind::ConfigDir {
        return Err(NotCandidate::Kind);
    }
    if state.row.dir.is_empty() {
        return Err(NotCandidate::NoDir);
    }
    match state.auth {
        AuthState::LoginExpired => return Err(NotCandidate::LoginExpired),
        AuthState::Unauthenticated => return Err(NotCandidate::Unauthenticated),
        AuthState::Unknown | AuthState::SignedIn => {}
    }
    if state.five_hour_exhausted() {
        return Err(NotCandidate::FiveHourExhausted);
    }
    Ok(())
}

/// The account a `switch-account` should rotate into, or `None`.
///
/// Design §5.6 verbatim: **lowest seven-day utilisation among accounts whose
/// five-hour window is not exhausted, ties by priority.** Plus §5.8.10: an
/// account whose last observed state is `login-expired` is not a candidate.
///
/// Two orderings are stated here because the design's one sentence does not
/// settle them, and a tie has to break somewhere:
///
/// * an account whose cache carries NO seven-day figure is ranked after every
///   account that has one. It is still a candidate — a roster with no caches
///   must be able to rotate at all — but measured headroom wins over silence;
/// * after utilisation and priority, the LABEL breaks the remaining tie, so
///   the answer does not depend on the order rows happen to sit in the file.
#[must_use]
pub fn select(states: &[AccountState]) -> Option<&AccountState> {
    states
        .iter()
        .filter(|s| candidacy(s).is_ok())
        .min_by(|a, b| {
            let key = |s: &AccountState| {
                (
                    s.seven_day_pct().is_none(),
                    s.seven_day_pct().unwrap_or(f64::MAX),
                    s.row.priority,
                )
            };
            let (ak, bk) = (key(a), key(b));
            ak.0.cmp(&bk.0)
                .then(ak.1.total_cmp(&bk.1))
                .then(ak.2.cmp(&bk.2))
                .then(a.row.label.cmp(&b.row.label))
        })
}

/// Build the roster's states, mark the active one, and read each config
/// directory. `active_dir` is what the session carries in
/// `CLAUDE_CONFIG_DIR`; `observed` is the caller's own auth evidence by label
/// (the live classifier's `auth` class for the account it is running under),
/// which outranks anything read off disk.
#[must_use]
pub fn roster_states(
    roster: &Roster,
    active_dir: Option<&Path>,
    observed: &BTreeMap<String, AuthState>,
) -> Vec<AccountState> {
    roster
        .rows
        .iter()
        .map(|row| {
            let active =
                active_dir.is_some_and(|d| !row.dir.is_empty() && Path::new(&row.dir) == d);
            let mut state = AccountState::new(row.clone());
            state.active = active;
            state.auth = observed.get(&row.label).copied().unwrap_or_default();
            state.read_dir()
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Discovery — design §5.6, Read / OwnerOnly, writes nothing
// ---------------------------------------------------------------------------

/// One config directory `discover` found, and what its `.claude.json` says.
#[derive(Debug, Clone, PartialEq)]
pub struct Proposal {
    /// The label a row would get: the directory's basename.
    pub label: String,
    /// The directory.
    pub dir: PathBuf,
    /// What was read out of it — `None` when the file is missing or
    /// unreadable.
    pub snapshot: Option<Snapshot>,
    /// Whether the roster already carries a row for this directory.
    pub already: bool,
}

impl Proposal {
    /// Whether the directory carries an `oauthAccount`. A directory without
    /// one is listed as unauthenticated: visible, never selectable.
    #[must_use]
    pub fn signed_in(&self) -> bool {
        self.snapshot.as_ref().is_some_and(|s| s.signed_in)
    }

    /// The `[[account]]` row this proposal would become, ready to append.
    #[must_use]
    pub fn to_toml(&self) -> String {
        row_toml(&self.label, &self.dir).unwrap_or_default()
    }
}

/// ONE `[[account]]` row, ready to append, or `None` when the label is
/// outside the character set a row's own reader admits.
///
/// It is shared by `discover --write` and by `accounts add` so there is ONE
/// spelling of the row: the hand-written path used to have none at all, and a
/// second `format!` would be a second grammar for the same file. The label
/// check here is the reader's check ([`row_of`]), asked BEFORE the row is
/// written rather than after — a roster refuses whole, so a bad label
/// appended by hand would take the whole file down with it.
#[must_use]
pub fn row_toml(label: &str, dir: &Path) -> Option<String> {
    if label.is_empty() || !admissible_label(label) {
        return None;
    }
    Some(format!(
        "\n[[account]]\nlabel = {:?}\nkind = {:?}\ndir = {:?}\npriority = {DEFAULT_PRIORITY}\n",
        label,
        Kind::ConfigDir.as_str(),
        dir.display().to_string()
    ))
}

/// The label character set a `[[account]]` row admits: `[A-Za-z0-9._-]`.
#[must_use]
pub fn admissible_label(label: &str) -> bool {
    label
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.')
}

/// Scan for config directories: `~/.claude`, every `~/.claude-*`, and
/// `$CLAUDE_CONFIG_DIR` where the caller passes one.
///
/// Reads each hit's `.claude.json` through [`snapshot`] — the closed,
/// non-secret key set — and **never** a keychain, a token, or any other file.
/// Writes nothing: the caller decides whether a proposal becomes a row.
///
/// The scan is one directory level under `home`, and the bound is on
/// MATCHES: at most [`MAX_DISCOVER_ENTRIES`] `.claude-*` directories are
/// taken, however many entries the home directory holds. The `take` used to
/// run BEFORE the filter, in unspecified `read_dir` order, so a home with
/// more than the bound dropped whichever `.claude-*` happened to sort late in
/// readdir order — a different answer on different runs. A directory that is
/// a symbolic link is
/// followed only as far as `metadata` already does — a link into somewhere
/// else still only yields its `.claude.json`, which is read under the same
/// size cap as every other.
#[must_use]
pub fn discover(home: &Path, config_dir: Option<&Path>, roster: &Roster) -> Vec<Proposal> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    let push = |dir: PathBuf, dirs: &mut Vec<PathBuf>| {
        if !dirs.contains(&dir) && dir.is_dir() {
            dirs.push(dir);
        }
    };
    push(home.join(".claude"), &mut dirs);
    if let Ok(entries) = std::fs::read_dir(home) {
        let mut found: Vec<PathBuf> = entries
            .flatten()
            .filter(|e| {
                e.file_name()
                    .to_str()
                    .is_some_and(|n| n.starts_with(".claude-"))
            })
            .take(MAX_DISCOVER_ENTRIES)
            .map(|e| e.path())
            .collect();
        found.sort();
        for dir in found {
            push(dir, &mut dirs);
        }
    }
    if let Some(dir) = config_dir {
        push(dir.to_path_buf(), &mut dirs);
    }
    dirs.into_iter()
        .map(|dir| Proposal {
            label: dir
                .file_name()
                .and_then(|n| n.to_str())
                .map(|n| truncate_bytes(n.trim_start_matches('.'), FIELD_CAP).to_string())
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| "claude".to_string()),
            snapshot: snapshot(&dir),
            already: roster.row_for_dir(&dir).is_some(),
            dir,
        })
        .collect()
}

#[cfg(test)]
#[path = "accounts_tests.rs"]
mod tests;
