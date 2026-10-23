// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The macOS access card: one question that stands in for macOS's many per-folder dialogs.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aterm_containment::consent::{DrClass, FdaState, Folder};

/// The marker beside `aterm.toml` ([`crate::config_marker`]).
pub const MARKER: &str = "privacy-access-card-answered";

/// A marker holds one token; anything bigger reads as "never answered", so the card shows.
pub const MAX_MARKER_BYTES: usize = 64;

/// The record beside `aterm.toml`: four `key=value` lines.
pub const RECORD: &str = "privacy-fda-record";

/// A record is four short lines.
pub const MAX_RECORD_BYTES: usize = 160;

/// How long, after the question is raised, the instance keeps re-checking the probe for the grant, and how long a due card waits on a pending probe before it is retired unraised.
pub const WATCH_FOR: Duration = Duration::from_secs(30 * 60);

const DECIDE_TIMEOUT: Duration = Duration::from_secs(20);

/// How many verdicts the one-shot will wait for before giving up for the life of the process.
pub const DECIDE_ATTEMPTS: u8 = 3;

/// How long after an ask the next may be made in the same epoch.
pub const RENEW_AFTER: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// How long an owner who pressed *Open Settings* is presumed to have acted.
pub const OWNER_ACTED_HOLD: Duration = Duration::from_secs(14 * 24 * 60 * 60);

/// How long a fresh process gives the window's own probe before a denied read beside a verified grant is believed.
pub const LAUNCH_SETTLE: Duration = Duration::from_secs(90);

/// Needs closer together than this are one need.
pub const NEED_DEBOUNCE: Duration = Duration::from_secs(60);

/// At most one `aterm.log` line per this interval says that macOS asked.
pub const NEED_LOG_EVERY: Duration = Duration::from_secs(6 * 60 * 60);

/// The pre-digest ROW's title (R15): the ASK — a question, so it claims nothing.
#[cfg_attr(not(test), allow(dead_code))]
pub const FDA_TITLE: &str = "Turn on Full Disk Access?";

/// The entry's `what` in the exceptions digest (glass-title form, cap 48).
pub const FDA_WHAT: &str = "macOS keeps asking for access";

/// The entry's `what` when no prompt was seen.
pub const FDA_WHAT_DENIED: &str = "macOS refused aterm a protected read";

/// The entry's `what` when a grant aterm had verified reads off again (`NeedWords::regressed`).
pub const FDA_WHAT_REGRESSED: &str = "Full Disk Access reads off again";

/// Why the owner is needed: an Allow is forgotten when aterm updates.
pub const FDA_WHY: &str = "macOS forgets an Allow when aterm updates";

/// The one action, as an imperative: the entry's `Then::press` sentence.
pub const FDA_THEN: &str = "turn on aterm in Full Disk Access";

/// What the switch grants, said where the ask is: a consequence Apple's own pane states ("data like Mail, Messages, Safari"), not a promise of a benefit.
pub const FDA_GRANTS: &str = "Full Disk Access lets programs you run in aterm, agents included, read protected data such as Mail and Messages";

/// What aterm does next, true whatever S1 answers.
pub const FDA_AFTER: &str = "aterm checks now and after each update, and says so in \
                                    aterm's Messages once Full Disk Access is on";

/// The hedge, always shown.
pub const FDA_DETAIL: &str = "Full Disk Access may already be enabled";

/// The ✓ record's title once the probe observes the grant.
pub const GRANTED_CAPTION: &str = "Full Disk Access granted";

/// The words after *Open Settings*.
pub fn opened_settings_caption(opened: bool, route: &str) -> String {
    if opened {
        format!(
            "Opened System Settings \u{2014} {route}; find aterm there (+ adds a missing app); \
             if already enabled, leave it on; aterm keeps checking access"
        )
    } else {
        format!(
            "System Settings did not open \u{2014} {route}; find aterm there (+ adds a missing \
             app); if already enabled, leave it on"
        )
    }
}

/// What made the card think the owner is needed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NeedCause {
    /// The sentinel saw `AUTHREQ_PROMPTING` for our own subject.
    PromptObserved {
        service: Option<Folder>,
        session: Option<u64>,
    },
    /// aterm's own read under a protected root took `EPERM`.
    ProtectedRead { session: Option<u64> },
}

impl NeedCause {
    /// The tab's session, when the cause names one.
    #[inline]
    pub const fn session(self) -> Option<u64> {
        match self {
            Self::PromptObserved { session, .. } => session,
            Self::ProtectedRead { session } => session,
        }
    }
}

/// The data the words are built from, so the reporter stays pure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NeedWords {
    pub cause: NeedCause,
    /// The 1-based tab number the prompting program belongs to, when it resolved.
    pub tab_number: Option<usize>,
    /// A verified grant later read as off: the epoch is a new one.
    pub regressed: bool,
}

impl NeedWords {
    /// What macOS did, in one sentence, from the cause.
    pub fn event_line(&self) -> String {
        if self.regressed {
            return "Full Disk Access was on for aterm and macOS now reports it off".to_owned();
        }
        let in_tab = self
            .tab_number
            .map_or_else(String::new, |n| format!(" in tab {n}"));
        match self.cause {
            NeedCause::PromptObserved { .. } => {
                let mut what = Folder::AppData.label().to_owned();
                if let Some(first) = what.get_mut(..1) {
                    first.make_ascii_lowercase();
                }
                format!("macOS asked to let a program{in_tab} read {what}")
            }
            NeedCause::ProtectedRead { .. } => {
                format!("macOS refused aterm a read of protected files{in_tab}")
            }
        }
    }
}

#[cfg(any(test, debug_assertions))]
impl PendingDecision {
    /// A launch-check half as the worker would deliver it: `marker` on disk, no record.
    pub fn launch(marker: Option<Marker>) -> Self {
        Self {
            marker,
            record: AccessRecord::default(),
            why: DecideWhy::Launch,
            verified_here: None,
        }
    }
}

#[cfg(any(test, debug_assertions))]
impl NeedWords {
    /// A sample need for tests of the reporter and the host.
    pub const fn sample() -> Self {
        Self {
            cause: NeedCause::PromptObserved {
                service: Some(Folder::AppData),
                session: Some(1),
            },
            tab_number: Some(1),
            regressed: false,
        }
    }
}

/// What the marker records.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Marker {
    /// *Not now* was pressed: the card is not offered again for the life of the config directory.
    NotNow,
    /// *Open Settings* was pressed and the outcome is not yet known.
    Opened,
}

impl Marker {
    /// The recorded token.
    #[inline]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NotNow => "not-now",
            Self::Opened => "opened",
        }
    }

    /// Parse a marker's text; anything else is not a record this code wrote.
    #[inline]
    pub fn parse(text: &str) -> Option<Self> {
        match text.trim() {
            "not-now" => Some(Self::NotNow),
            "opened" => Some(Self::Opened),
            _ => None,
        }
    }
}

/// Where the marker lives for this config, or `None` with no config path.
pub fn marker_path(config_path: Option<&Path>) -> Option<PathBuf> {
    config_path
        .and_then(Path::parent)
        .map(|dir| dir.join(MARKER))
}

/// The recorded marker, if any.
pub fn read_marker(config_path: Option<&Path>) -> Option<Marker> {
    marker_path(config_path)
        .and_then(|path| crate::config_marker::read_marker(&path, MAX_MARKER_BYTES))
        .and_then(|text| Marker::parse(&text))
}

static MARKER_WRITES: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Record `marker`.
pub fn record_marker(config_path: Option<&Path>, marker: Marker) -> std::io::Result<()> {
    let _serial = MARKER_WRITES.lock().unwrap_or_else(|p| p.into_inner());
    let Some(path) = marker_path(config_path) else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "no config directory to record the answer in",
        ));
    };
    if marker == Marker::Opened
        && crate::config_marker::read_marker(&path, MAX_MARKER_BYTES)
            .and_then(|text| Marker::parse(&text))
            == Some(Marker::NotNow)
    {
        return Ok(());
    }
    crate::config_marker::write_marker(
        &path,
        marker.as_str(),
        MAX_MARKER_BYTES,
        crate::durable::Flush::Ordered,
    )
}

/// Clear an `opened` marker once its grant has been acknowledged.
pub fn clear_opened(config_path: Option<&Path>) -> std::io::Result<()> {
    let _serial = MARKER_WRITES.lock().unwrap_or_else(|p| p.into_inner());
    let Some(path) = marker_path(config_path) else {
        return Ok(());
    };
    if crate::config_marker::read_marker(&path, MAX_MARKER_BYTES)
        .and_then(|text| Marker::parse(&text))
        != Some(Marker::Opened)
    {
        return Ok(());
    }
    crate::config_marker::remove_marker(&path)
}

/// What the record file holds: unix seconds, each field independent.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AccessRecord {
    /// When the question was last raised.
    pub asked_at: Option<u64>,
    /// When the owner last pressed *Open Settings* (through either surface).
    pub opened_at: Option<u64>,
    /// When the window's own probe last read the grant as held.
    pub verified_at: Option<u64>,
    /// The build number that was running then.
    pub verified_build: Option<u64>,
}

impl AccessRecord {
    /// The field-wise MAX of two records: no field ever moves backwards.
    #[inline]
    pub const fn merged(self, other: Self) -> Self {
        const fn max(a: Option<u64>, b: Option<u64>) -> Option<u64> {
            match (a, b) {
                (Some(a), Some(b)) => Some(if a >= b { a } else { b }),
                (Some(a), None) | (None, Some(a)) => Some(a),
                (None, None) => None,
            }
        }
        Self {
            asked_at: max(self.asked_at, other.asked_at),
            opened_at: max(self.opened_at, other.opened_at),
            verified_at: max(self.verified_at, other.verified_at),
            verified_build: max(self.verified_build, other.verified_build),
        }
    }

    /// The file's text: only the fields that are set.
    pub fn render(&self) -> String {
        let mut out = String::new();
        for (key, value) in [
            ("asked_at", self.asked_at),
            ("opened_at", self.opened_at),
            ("verified_at", self.verified_at),
            ("verified_build", self.verified_build),
        ] {
            if let Some(value) = value {
                out.push_str(&format!("{key}={value}\n"));
            }
        }
        out
    }

    /// Parse a record's text, strictly.
    pub fn parse(text: &str) -> Self {
        let mut record = Self::default();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                return Self::default();
            };
            let Ok(value) = value.trim().parse::<u64>() else {
                return Self::default();
            };
            let slot = match key.trim() {
                "asked_at" => &mut record.asked_at,
                "opened_at" => &mut record.opened_at,
                "verified_at" => &mut record.verified_at,
                "verified_build" => &mut record.verified_build,
                _ => return Self::default(),
            };
            *slot = Some(value);
        }
        record
    }
}

/// Where the record lives for this config, or `None` with no config path.
pub fn record_path(config_path: Option<&Path>) -> Option<PathBuf> {
    config_path
        .and_then(Path::parent)
        .map(|dir| dir.join(RECORD))
}

/// The recorded facts; empty when there is no record.
pub fn read_record(config_path: Option<&Path>) -> AccessRecord {
    record_path(config_path)
        .and_then(|path| crate::config_marker::read_marker(&path, MAX_RECORD_BYTES))
        .map(|text| AccessRecord::parse(&text))
        .unwrap_or_default()
}

/// Merge `update` into the record on disk (field-wise max, under the shared write lock) and return what is there now.
pub fn write_record(
    config_path: Option<&Path>,
    update: AccessRecord,
) -> std::io::Result<AccessRecord> {
    let _serial = MARKER_WRITES.lock().unwrap_or_else(|p| p.into_inner());
    let Some(path) = record_path(config_path) else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "no config directory to keep the record in",
        ));
    };
    let current = crate::config_marker::read_marker(&path, MAX_RECORD_BYTES)
        .map(|text| AccessRecord::parse(&text))
        .unwrap_or_default();
    let merged = current.merged(update);
    if merged != current || !path.exists() {
        crate::config_marker::write_marker(
            &path,
            &merged.render(),
            MAX_RECORD_BYTES,
            crate::durable::Flush::Ordered,
        )?;
    }
    Ok(merged)
}

/// Where a grant was verified.
pub const VERIFIED_HOST: &str = "privacy-fda-verified-host";

const MAX_VERIFIED_HOSTS: usize = 16;

const MAX_VERIFIED_HOST_BYTES: usize = MAX_VERIFIED_HOSTS * 160;

fn verified_host_path(config_path: Option<&Path>) -> Option<PathBuf> {
    config_path
        .and_then(Path::parent)
        .map(|dir| dir.join(VERIFIED_HOST))
}

fn verified_key(here: &str, bundle_id: &str) -> String {
    format!("{here} {bundle_id}")
}

/// Whether the record's grant was verified on the Mac whose hardware UUID is `here`, for `bundle_id`.
pub fn verified_here(
    config_path: Option<&Path>,
    here: Option<&str>,
    bundle_id: Option<&str>,
) -> Option<bool> {
    let stamp = verified_host_path(config_path)
        .and_then(|path| crate::config_marker::read_marker(&path, MAX_VERIFIED_HOST_BYTES))?;
    let key = verified_key(here?, bundle_id?);
    Some(stamp.lines().any(|line| line.trim() == key))
}

fn with_verified_host(current: &str, key: &str) -> Option<String> {
    let mut hosts: Vec<&str> = current
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    if hosts.contains(&key) {
        return None;
    }
    hosts.push(key);
    let skip = hosts.len().saturating_sub(MAX_VERIFIED_HOSTS);
    Some(
        hosts[skip..]
            .iter()
            .map(|host| format!("{host}\n"))
            .collect(),
    )
}

/// Note that the grant was verified on the Mac whose hardware UUID is `here`, for `bundle_id`.
pub fn write_verified_host(
    config_path: Option<&Path>,
    here: &str,
    bundle_id: &str,
) -> std::io::Result<()> {
    let _serial = MARKER_WRITES.lock().unwrap_or_else(|p| p.into_inner());
    let Some(path) = verified_host_path(config_path) else {
        return Ok(());
    };
    let current =
        crate::config_marker::read_marker(&path, MAX_VERIFIED_HOST_BYTES).unwrap_or_default();
    match with_verified_host(&current, &verified_key(here, bundle_id)) {
        Some(text) => crate::config_marker::write_marker(
            &path,
            &text,
            MAX_VERIFIED_HOST_BYTES,
            crate::durable::Flush::Ordered,
        ),
        None => Ok(()),
    }
}

static PROCESS_STARTED_AT: std::sync::OnceLock<u64> = std::sync::OnceLock::new();

/// The current unix time in seconds (`0` if the clock is before the epoch).
pub fn now_unix_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Stamp this process's start.
pub fn note_process_start() {
    let _ = PROCESS_STARTED_AT.set(now_unix_secs());
}

/// When this process started, in unix seconds.
pub fn process_started_at() -> u64 {
    *PROCESS_STARTED_AT.get_or_init(now_unix_secs)
}

/// Why a worker was started: the launch check (never asks) or a need.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecideWhy {
    Launch,
    Need(NeedCause),
}

/// The worker's half of a decision, held by the host while a pending probe is waited on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PendingDecision {
    pub marker: Option<Marker>,
    pub record: AccessRecord,
    pub why: DecideWhy,
    /// Whether the record's grant was verified on this Mac ([`verified_here`]).
    pub verified_here: Option<bool>,
}

/// The host-side state of the card that is not the lifecycle machine.
#[derive(Clone, Copy, Debug, Default)]
pub struct CardHost {
    /// A verdict that arrived while the probe was still pending.
    pub pending: Option<PendingDecision>,
    /// The cause the in-flight need worker was started for.
    pub need: Option<NeedCause>,
    /// The latest need that arrived while the card could not decide it.
    pub kept_need: Option<NeedCause>,
    /// The words of the question that is due.
    pub words: Option<NeedWords>,
    /// The record on disk, as last read, merged with this process's writes.
    pub record: AccessRecord,
    /// The marker on disk, as last read or written.
    pub marker: Option<Marker>,
    /// Need events counted this process.
    pub needs: u32,
    /// The last need that was not coalesced ([`NEED_DEBOUNCE`]).
    pub need_last: Option<Instant>,
    /// The last time a need was written to `aterm.log` ([`NEED_LOG_EVERY`]).
    pub need_logged: Option<Instant>,
    /// A kept need must not be decided again before this ([`NotDue::Settling`]).
    pub settle_retry_at: Option<Instant>,
}

/// The `state=` token of the `access_card` line of `aterm ctl privacy`.
pub fn report_state(
    marker: Option<Marker>,
    record: &AccessRecord,
    fda: FdaState,
    now_secs: u64,
) -> &'static str {
    if marker == Some(Marker::NotNow) {
        return "declined";
    }
    if fda == FdaState::Granted {
        return "granted";
    }
    if record.opened_at.is_some_and(|at| {
        now_secs < at.saturating_add(OWNER_ACTED_HOLD.as_secs())
            && record.verified_at.is_none_or(|verified| verified <= at)
    }) {
        return "opened";
    }
    if record.asked_at.is_some_and(|at| {
        now_secs < at.saturating_add(RENEW_AFTER.as_secs())
            && record.verified_at.is_none_or(|verified| verified <= at)
    }) {
        return "asked";
    }
    "quiet"
}

/// Everything the launch decision reads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CardFacts {
    /// `[privacy] enabled && check && notice`.
    pub offered: bool,
    /// The instance has no window (inert probe arms).
    pub headless: bool,
    /// The cached Full Disk Access verdict.
    pub fda: FdaState,
    /// The designated requirement class a grant would be keyed to.
    pub dr: DrClass,
}

/// Everything the need decision reads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NeedFacts {
    pub card: CardFacts,
    pub now_secs: u64,
    /// When this process started ([`process_started_at`]).
    pub process_started_at: u64,
    pub cause: NeedCause,
}

/// Why the card is not due: one named reason, for the log.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NotDue {
    /// `[privacy] enabled`, `check` or `notice` is false.
    Off,
    /// A headless instance cannot observe a denial.
    Headless,
    /// *Not now* is already on record.
    Answered,
    /// The probe reports the grant is held.
    Granted,
    /// The probe did not observe a denial (`unknown`: refused, or the lane is off) — not a denial.
    NotObservedDenied,
    /// A grant keyed to this requirement class would not survive a rebuild.
    GrantUnstable(DrClass),
    /// The prompt was for a service this card is not about.
    NotThisClass,
    /// The owner pressed *Open Settings* and this window started before the press.
    OwnerActed,
    /// The question was already asked this epoch.
    RecentlyAsked,
    /// The launch check never asks: the card waits for a real need.
    NoNeedYet,
    /// The launch check found the grant held, as it was before.
    Held,
    /// A grant was verified before and the probe reads off now.
    ReadsOffAfterVerified,
    /// A verified grant reads off inside [`LAUNCH_SETTLE`] of this process starting: not concluded.
    Settling,
}

impl std::fmt::Display for NotDue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Off => f.write_str("[privacy] notice is off"),
            Self::Headless => f.write_str("headless instance"),
            Self::Answered => f.write_str("already answered (not now)"),
            Self::Granted => f.write_str("full disk access is granted"),
            Self::NotObservedDenied => f.write_str("the probe did not observe a denial"),
            Self::GrantUnstable(dr) => {
                write!(f, "dr={}: a grant would not survive a rebuild", dr.as_str())
            }
            Self::NotThisClass => f.write_str("a prompt for a service whose Allow is durable"),
            Self::OwnerActed => f.write_str(
                "the owner opened Settings and this window started before that; it may not see the switch",
            ),
            Self::RecentlyAsked => f.write_str("already asked this epoch"),
            Self::NoNeedYet => {
                f.write_str("no need yet: the card asks only after macOS shows the dialog")
            }
            Self::Held => f.write_str("full disk access is held, as verified before"),
            Self::ReadsOffAfterVerified => {
                f.write_str("full disk access was verified before and reads off now")
            }
            Self::Settling => f.write_str(
                "full disk access was verified before and reads off in this process's first \
                 seconds; read again once it has settled",
            ),
        }
    }
}

/// What the need decision resolved to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NeedVerdict {
    /// Raise the question once the cached probe confirms the denial.
    Ask { regressed: bool },
    /// Nothing to show, and why.
    Quiet(NotDue),
}

/// What the launch decision resolved to; never an ask.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LaunchVerdict {
    /// The grant the owner started (or was asked for) is now held.
    Confirm,
    /// Nothing to show, and why.
    Quiet(NotDue),
}

/// What the card's lifecycle machine records of a decision.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Post the question once the cached probe confirms the denial.
    Offer,
    /// Post the ✓ record and clear the `opened` marker.
    Confirm,
    /// Nothing to show, and why.
    Quiet(NotDue),
}

impl From<&NeedVerdict> for Verdict {
    fn from(verdict: &NeedVerdict) -> Self {
        match verdict {
            NeedVerdict::Ask { .. } => Self::Offer,
            NeedVerdict::Quiet(why) => Self::Quiet(why.clone()),
        }
    }
}

impl From<&LaunchVerdict> for Verdict {
    fn from(verdict: &LaunchVerdict) -> Self {
        match verdict {
            LaunchVerdict::Confirm => Self::Confirm,
            LaunchVerdict::Quiet(why) => Self::Quiet(why.clone()),
        }
    }
}

/// Whether a need verdict is the anomaly the log keeps a trace of: macOS showed a dialog for a program of this aterm while this process reads Full Disk Access as granted.
pub const fn is_prompt_while_granted(cause: NeedCause, verdict: &NeedVerdict) -> bool {
    matches!(cause, NeedCause::PromptObserved { .. })
        && matches!(verdict, NeedVerdict::Quiet(NotDue::Granted))
}

/// The need decision, pure over the facts, the marker and the record.
pub fn decide_need(
    facts: &NeedFacts,
    marker: Option<Marker>,
    record: &AccessRecord,
) -> NeedVerdict {
    let card = &facts.card;
    if !card.offered {
        return NeedVerdict::Quiet(NotDue::Off);
    }
    if card.headless {
        return NeedVerdict::Quiet(NotDue::Headless);
    }
    if let NeedCause::PromptObserved { service, .. } = facts.cause
        && service != Some(Folder::AppData)
    {
        return NeedVerdict::Quiet(NotDue::NotThisClass);
    }
    if marker == Some(Marker::NotNow) {
        return NeedVerdict::Quiet(NotDue::Answered);
    }
    if !card.dr.grant_stable() {
        return NeedVerdict::Quiet(NotDue::GrantUnstable(card.dr));
    }
    if card.fda == FdaState::Granted {
        return NeedVerdict::Quiet(NotDue::Granted);
    }
    if card.fda == FdaState::Denied
        && record.verified_at.is_some()
        && facts.now_secs
            < facts
                .process_started_at
                .saturating_add(LAUNCH_SETTLE.as_secs())
    {
        return NeedVerdict::Quiet(NotDue::Settling);
    }
    if let Some(opened_at) = record.opened_at
        && facts.now_secs < opened_at.saturating_add(OWNER_ACTED_HOLD.as_secs())
        && record
            .verified_at
            .is_none_or(|verified| verified <= opened_at)
        && facts.process_started_at < opened_at
    {
        return NeedVerdict::Quiet(NotDue::OwnerActed);
    }
    if card.fda != FdaState::Denied {
        return NeedVerdict::Quiet(NotDue::NotObservedDenied);
    }
    if let Some(asked_at) = record.asked_at
        && facts.now_secs < asked_at.saturating_add(RENEW_AFTER.as_secs())
        && record
            .verified_at
            .is_none_or(|verified| verified <= asked_at)
    {
        return NeedVerdict::Quiet(NotDue::RecentlyAsked);
    }
    NeedVerdict::Ask {
        regressed: record.verified_at.is_some(),
    }
}

/// The launch decision: runs once per process at the first park and never asks.
pub fn decide_launch(
    facts: &CardFacts,
    marker: Option<Marker>,
    record: &AccessRecord,
) -> LaunchVerdict {
    if !facts.offered {
        return LaunchVerdict::Quiet(NotDue::Off);
    }
    if facts.headless {
        return LaunchVerdict::Quiet(NotDue::Headless);
    }
    if marker == Some(Marker::NotNow) {
        return LaunchVerdict::Quiet(NotDue::Answered);
    }
    match facts.fda {
        FdaState::Granted => {
            if marker == Some(Marker::Opened)
                || (record.asked_at.is_some() && record.verified_at.is_none())
            {
                LaunchVerdict::Confirm
            } else if record.verified_at.is_some() {
                LaunchVerdict::Quiet(NotDue::Held)
            } else {
                LaunchVerdict::Quiet(NotDue::Granted)
            }
        }
        FdaState::Denied if record.verified_at.is_some() => {
            LaunchVerdict::Quiet(NotDue::ReadsOffAfterVerified)
        }
        FdaState::Denied | FdaState::Unknown => LaunchVerdict::Quiet(NotDue::NoNeedYet),
    }
}

/// Whether a new process should re-assert a standing question at its launch check.
pub fn should_reassert(record: &AccessRecord, marker: Option<Marker>, fda: FdaState) -> bool {
    marker.is_none()
        && fda != FdaState::Granted
        && record.asked_at.is_some()
        && record
            .verified_at
            .is_none_or(|verified| verified <= record.asked_at.unwrap_or(0))
}

/// The per-process lifecycle of the card, driven from the event loop's park point.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CardPhase {
    /// Nothing asked yet — the launch-time one-shot has not run.
    Idle,
    /// The worker is warming the identity and reading the marker and the record, since `since`.
    Deciding { since: Instant },
    /// Due; the question is raised at the next park whose cached probe still reads `denied`.
    Due,
    /// The question is raised since `since` (or the owner opened Settings then).
    Watching { since: Instant },
    /// Not currently active.
    Settled,
}

/// See [`CardPhase`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CardState {
    phase: CardPhase,
    /// When the offer became due — the anchor of a due card's wait on a
    /// pending probe.
    due_since: Option<Instant>,
    /// When the question was first raised in this episode.
    first_raised: Option<Instant>,
    /// The owner pressed something on the row (Open Settings, or Details).
    owner_acted: bool,
    /// How many times the one-shot has asked its worker for a verdict
    /// ([`DECIDE_ATTEMPTS`]).
    decide_attempts: u8,
    /// A need arrived while the card was busy (launch check under way, or a
    /// question already raised): it is re-evaluated the moment the phase
    /// settles ([`CardState::take_pending_need`]).
    pending_need: bool,
}

impl CardState {
    pub const fn new() -> Self {
        Self {
            phase: CardPhase::Idle,
            due_since: None,
            decide_attempts: 0,
            first_raised: None,
            owner_acted: false,
            pending_need: false,
        }
    }

    pub const fn phase(self) -> CardPhase {
        self.phase
    }

    /// Every active phase owns an absolute expiry, even with no probe result.
    pub fn lifecycle_deadline(self) -> Option<Instant> {
        match self.phase {
            CardPhase::Idle | CardPhase::Settled => None,
            CardPhase::Deciding { since } => Some(since + DECIDE_TIMEOUT),
            CardPhase::Due => self.due_since.map(|since| since + WATCH_FOR),
            CardPhase::Watching { since } => Some(since + WATCH_FOR),
        }
    }

    /// Retire an expired probe wait or grant watch before asking for another observation.
    pub fn expire_wait(&mut self, now: Instant) -> bool {
        if matches!(self.phase, CardPhase::Due | CardPhase::Watching { .. })
            && self
                .lifecycle_deadline()
                .is_some_and(|deadline| now >= deadline)
        {
            self.settle();
            true
        } else {
            false
        }
    }

    /// Admit one lifecycle step against the current settings, including a pending worker verdict or a raised question.
    pub const fn admit_current_policy(&mut self, enabled: bool, notice: bool) -> bool {
        if enabled && notice {
            true
        } else {
            self.settle();
            self.pending_need = false;
            false
        }
    }

    /// The launch one-shot fired: a worker is deciding.
    pub fn begin_deciding(&mut self, now: Instant) -> bool {
        if self.phase != CardPhase::Idle {
            return false;
        }
        self.phase = CardPhase::Deciding { since: now };
        self.decide_attempts = self.decide_attempts.saturating_add(1);
        true
    }

    /// A need arrived (macOS showed the dialog, or aterm's own read was refused).
    pub fn on_need(&mut self, now: Instant) -> bool {
        if self.phase != CardPhase::Settled {
            self.pending_need = true;
            return false;
        }
        *self = Self {
            phase: CardPhase::Deciding { since: now },
            decide_attempts: 1,
            ..Self::new()
        };
        true
    }

    /// Keep a need without acting on it.
    pub const fn defer_need(&mut self) {
        self.pending_need = true;
    }

    /// The grant answers a need that was kept before it: drop the kept need without deciding it.
    pub const fn discard_pending_need(&mut self) {
        self.pending_need = false;
    }

    /// Whether a need was kept while busy; consumed by the call.
    pub fn take_pending_need(&mut self) -> bool {
        if self.phase == CardPhase::Settled && self.pending_need {
            self.pending_need = false;
            true
        } else {
            false
        }
    }

    /// The worker's verdict has not arrived within [`DECIDE_TIMEOUT`].
    pub fn deciding_lapsed(&self, now: Instant) -> bool {
        matches!(self.phase, CardPhase::Deciding { since }
            if now.duration_since(since) >= DECIDE_TIMEOUT)
    }

    /// A lapsed decision: back to `Idle` to ask again (`true`), or settled once [`DECIDE_ATTEMPTS`] are spent.
    pub const fn reopen_deciding(&mut self) -> bool {
        if self.decide_attempts >= DECIDE_ATTEMPTS {
            self.settle();
            return false;
        }
        self.phase = CardPhase::Idle;
        true
    }

    /// The main thread decided.
    pub fn on_decided(&mut self, verdict: &Verdict, now: Instant) -> bool {
        if !matches!(self.phase, CardPhase::Deciding { .. }) {
            return false;
        }
        self.phase = match verdict {
            Verdict::Offer => {
                self.due_since.get_or_insert(now);
                CardPhase::Due
            }
            Verdict::Confirm | Verdict::Quiet(_) => CardPhase::Settled,
        };
        true
    }

    /// Whether the question may be raised at `now`.
    pub fn raise_allowed(&mut self, now: Instant) -> bool {
        if self.phase != CardPhase::Due {
            return false;
        }
        !self.expire_wait(now)
    }

    /// The question was raised at `now`.
    pub fn on_raised(&mut self, now: Instant) {
        if self.phase == CardPhase::Due {
            self.phase = CardPhase::Watching { since: now };
            self.first_raised.get_or_insert(now);
        }
    }

    /// The owner opened the row's Details: read; the watch continues.
    pub const fn on_owner_acted(&mut self) {
        if matches!(self.phase, CardPhase::Watching { .. }) {
            self.owner_acted = true;
        }
    }

    /// The row folded with nothing pressed: the unanswered path, settled (`true`).
    pub const fn on_row_retired(&mut self) -> bool {
        if !matches!(self.phase, CardPhase::Watching { .. }) || self.owner_acted {
            return false;
        }
        self.settle();
        true
    }

    /// The owner pressed *Open Settings* through either UI.
    pub const fn on_opened_settings(&mut self, now: Instant) {
        self.phase = CardPhase::Watching { since: now };
        self.owner_acted = true;
    }

    /// Whether the probe should be re-checked at `now`.
    pub fn wants_probe(&mut self, now: Instant) -> bool {
        matches!(self.phase, CardPhase::Watching { .. }) && !self.expire_wait(now)
    }

    /// Nothing more in this episode: *Not now*, the grant observed, or the decision said no.
    pub const fn settle(&mut self) {
        self.phase = CardPhase::Settled;
    }
}

impl Default for CardState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AccessRecord, CardFacts, CardPhase, CardState, DECIDE_ATTEMPTS, DECIDE_TIMEOUT,
        LAUNCH_SETTLE, LaunchVerdict, MARKER, MAX_MARKER_BYTES, MAX_RECORD_BYTES, Marker,
        NeedCause, NeedFacts, NeedVerdict, NeedWords, NotDue, OWNER_ACTED_HOLD, RECORD,
        RENEW_AFTER, Verdict, WATCH_FOR, clear_opened, decide_launch, decide_need,
        is_prompt_while_granted, marker_path, read_marker, read_record, record_marker, record_path,
        report_state, should_reassert, write_record, write_verified_host,
    };
    use aterm_containment::consent::{DrClass, FdaState, Folder};

    #[test]
    fn the_verified_stamp_keeps_every_mac_that_verified() {
        use super::{MAX_VERIFIED_HOSTS, verified_here, with_verified_host, write_verified_host};
        let a = "AAAAAAAA-0000-0000-0000-000000000000";
        let b = "BBBBBBBB-0000-0000-0000-000000000000";
        let once = with_verified_host("", a).expect("a new Mac is added");
        assert_eq!(once, format!("{a}\n"));
        assert_eq!(with_verified_host(&once, a), None, "never twice");
        let both = with_verified_host(&once, b).expect("a second Mac is added");
        assert_eq!(both, format!("{a}\n{b}\n"));
        let mut many = String::new();
        for n in 0..=MAX_VERIFIED_HOSTS {
            many = with_verified_host(&many, &format!("{n:08}-0000-0000-0000-000000000000"))
                .expect("added");
        }
        assert_eq!(many.lines().count(), MAX_VERIFIED_HOSTS);
        assert!(!many.contains("00000000-0000"), "the oldest left first");

        let dir = aterm_tempfile::tempdir().expect("tempdir");
        let config = dir.path().join("aterm.toml");
        let release = Some("com.aterm.aterm");
        let dev = Some("com.aterm.aterm.dev");
        assert_eq!(
            verified_here(Some(&config), Some(a), release),
            None,
            "no stamp"
        );
        write_verified_host(Some(&config), a, "com.aterm.aterm").expect("stamp");
        write_verified_host(Some(&config), b, "com.aterm.aterm").expect("stamp");
        assert_eq!(verified_here(Some(&config), Some(a), release), Some(true));
        assert_eq!(verified_here(Some(&config), Some(b), release), Some(true));
        assert_eq!(
            verified_here(Some(&config), Some(a), dev),
            Some(false),
            "another bundle's grant on this Mac is not this one's"
        );
        assert_eq!(
            verified_here(
                Some(&config),
                Some("CCCCCCCC-0000-0000-0000-000000000000"),
                release
            ),
            Some(false)
        );
        assert_eq!(verified_here(Some(&config), None, release), None, "no UUID");
        assert_eq!(
            verified_here(Some(&config), Some(a), None),
            None,
            "no bundle id"
        );
    }
    use std::time::{Duration, Instant};

    fn denied_release() -> CardFacts {
        CardFacts {
            offered: true,
            headless: false,
            fda: FdaState::Denied,
            dr: DrClass::Identity,
        }
    }

    fn bounded_wait_model() -> aterm_spec::derive::Model {
        aterm_spec::ty_model! {
            MacosAccessCardBoundedWait {
                const Buggy = 0;
                var phase = 2;
                var expired = 0;
                action Raise when (phase == 2 && expired == 0) {
                    phase = 3;
                }
                action Expire when (phase > 1 && phase <= 3 && expired == 0) {
                    expired = 1;
                    phase = if Buggy == 1 && phase == 2 { phase } else { 4 };
                }
                invariant Bounds: phase > 1 && phase <= 4 && expired <= 1;
                invariant NoWorkPastCap: expired == 0 || phase == 4;
            }
        }
    }

    #[test]
    fn bounded_wait_model_proves_and_catches_a_never_shown_card() {
        let model = bounded_wait_model();
        aterm_spec::verify::prove_and_catch_scalar(&model, model.name);
    }

    #[test]
    fn bounded_wait_conforms_at_each_real_phase_deadline() {
        let now = Instant::now();
        let model = bounded_wait_model();
        let scenarios: &[&[&str]] = &[&[], &["Raise"]];
        let mut negative_controls = 0;
        for actions in scenarios {
            let mut actual = CardState::new();
            assert_eq!(actual.lifecycle_deadline(), None);
            assert!(actual.begin_deciding(now));
            assert_eq!(actual.lifecycle_deadline(), Some(now + DECIDE_TIMEOUT));
            assert!(actual.on_decided(&Verdict::Offer, now));
            assert_eq!(
                actual.lifecycle_deadline(),
                Some(now + WATCH_FOR),
                "a due card waits on a pending probe with the watch's patience"
            );
            let mut expected = model.init_state();
            for (step, action) in actions.iter().enumerate() {
                let at = now + Duration::from_secs(step as u64 + 1);
                match *action {
                    "Raise" => actual.on_raised(at),
                    _ => unreachable!(),
                }
                assert!(model.fire(action, &mut expected));
                let phase = match actual.phase() {
                    CardPhase::Due => 2,
                    CardPhase::Watching { .. } => 3,
                    _ => panic!("unexpected waiting phase"),
                };
                assert_eq!(phase, expected["phase"]);
            }
            let deadline = actual.lifecycle_deadline().expect("every wait has a cap");
            assert!(!actual.expire_wait(deadline - Duration::from_nanos(1)));
            assert_eq!(actual.lifecycle_deadline(), Some(deadline));
            let before = expected.clone();
            assert!(actual.expire_wait(deadline));
            assert!(model.fire("Expire", &mut expected));
            assert_eq!(actual.phase(), CardPhase::Settled);
            assert_eq!(expected["phase"], 4);
            assert!(model.check_invariant("NoWorkPastCap", &expected));
            assert_eq!(
                actual.lifecycle_deadline(),
                None,
                "a settled card owns no deadline: there is no daily return any more"
            );
            assert!(!actual.raise_allowed(deadline));
            assert!(!actual.wants_probe(deadline));
            if before["phase"] == 2 {
                let mut old = expected.clone();
                old.insert("phase", 2);
                assert!(!model.check_invariant("NoWorkPastCap", &old));
                negative_controls += 1;
            }
        }
        assert_eq!(negative_controls, 1, "the first-offer omission");
    }

    fn current_policy_model() -> aterm_spec::derive::Model {
        aterm_spec::ty_model! {
            MacosAccessCardCurrentPolicy {
                const Buggy = 0;
                var enabled = 1;
                var notice = 1;
                var phase = 0;
                action Begin when (phase == 0 && enabled == 1 && notice == 1) {
                    phase = 1;
                }
                action Decide when (phase == 1 && enabled == 1 && notice == 1) {
                    phase = 2;
                }
                action Raise when (phase == 2 && enabled == 1 && notice == 1) {
                    phase = 3;
                }
                action Finish when (phase == 3) {
                    phase = 4;
                }
                action DisableMaster when (enabled == 1) {
                    enabled = 0;
                    phase = if Buggy == 1 && phase > 0 { phase } else { 4 };
                }
                action DisableNotice when (notice == 1) {
                    notice = 0;
                    phase = if Buggy == 1 && phase > 0 { phase } else { 4 };
                }
                action EnableBoth when (enabled == 0 || notice == 0) {
                    enabled = 1;
                    notice = 1;
                }
                action Rearm when (phase == 4 && (Buggy == 1 || (enabled == 1 && notice == 1))) {
                    phase = 1;
                }
                invariant Bounds: enabled <= 1 && notice <= 1 && phase <= 4;
                invariant DisabledSettles: enabled == 1 && notice == 1 || phase == 4;
            }
        }
    }

    #[test]
    fn current_policy_model_proves_and_catches_a_stale_card() {
        let model = current_policy_model();
        aterm_spec::verify::prove_and_catch_scalar(&model, model.name);
    }

    #[test]
    fn current_policy_conforms_in_every_real_phase() {
        fn phase_value(state: CardState) -> i64 {
            match state.phase() {
                CardPhase::Idle => 0,
                CardPhase::Deciding { .. } => 1,
                CardPhase::Due => 2,
                CardPhase::Watching { .. } => 3,
                CardPhase::Settled => 4,
            }
        }

        let now = Instant::now();
        let model = current_policy_model();
        let mut actual = CardState::new();
        let mut expected = model.init_state();
        let mut phases = vec![(actual, expected.clone())];
        for action in ["Begin", "Decide", "Raise", "Finish"] {
            match action {
                "Begin" => assert!(actual.begin_deciding(now)),
                "Decide" => assert!(actual.on_decided(&Verdict::Offer, now)),
                "Raise" => actual.on_raised(now),
                "Finish" => actual.settle(),
                _ => unreachable!(),
            }
            assert!(model.fire(action, &mut expected));
            assert_eq!(phase_value(actual), expected["phase"]);
            phases.push((actual, expected.clone()));
        }
        let mut caught_old_guards = 0;
        for (before, projection) in phases {
            for (enabled, notice) in [(true, true), (false, true), (true, false), (false, false)] {
                let mut actual = before;
                let mut expected = projection.clone();
                let admitted = actual.admit_current_policy(enabled, notice);
                assert_eq!(admitted, enabled && notice);
                if !enabled {
                    assert!(model.fire("DisableMaster", &mut expected));
                }
                if !notice {
                    assert!(model.fire("DisableNotice", &mut expected));
                }
                assert_eq!(phase_value(actual), expected["phase"]);
                assert!(model.check_invariant("DisabledSettles", &expected));
                if admitted {
                    assert_eq!(actual, before, "an enabled policy preserves every phase");
                } else {
                    let mut old = expected.clone();
                    old.insert("phase", phase_value(before));
                    if !matches!(before.phase(), CardPhase::Idle | CardPhase::Settled) {
                        assert!(!model.check_invariant("DisabledSettles", &old));
                        caught_old_guards += 1;
                    }
                    assert!(!actual.on_decided(&Verdict::Offer, now));
                    assert!(!actual.on_decided(&Verdict::Confirm, now));
                    assert!(actual.admit_current_policy(true, true));
                    assert!(model.fire("EnableBoth", &mut expected));
                    assert_eq!(phase_value(actual), expected["phase"]);
                    assert_eq!(actual.phase(), CardPhase::Settled);
                }
            }
        }
        assert_eq!(
            caught_old_guards, 9,
            "three active phases, three disabled policies"
        );

        for (enabled, notice) in [(true, true), (false, true), (true, false), (false, false)] {
            let mut actual = CardState::new();
            actual.settle();
            let mut expected = model.init_state();
            for action in ["Begin", "Decide", "Raise", "Finish"] {
                assert!(model.fire(action, &mut expected));
            }
            if !enabled {
                assert!(model.fire("DisableMaster", &mut expected));
            }
            if !notice {
                assert!(model.fire("DisableNotice", &mut expected));
            }
            assert_eq!(expected["phase"], 4);
            let admitted = actual.admit_current_policy(enabled, notice);
            assert_eq!(admitted, model.action_enabled("Rearm", &expected));
            if admitted {
                assert!(actual.on_need(now));
                assert!(model.fire("Rearm", &mut expected));
                assert_eq!(phase_value(actual), expected["phase"]);
            } else {
                assert_eq!(actual.phase(), CardPhase::Settled);
                let mut buggy = expected.clone();
                buggy.insert("phase", 1);
                assert!(!model.check_invariant("DisabledSettles", &buggy));
            }
        }
    }

    fn need_facts(cause: NeedCause) -> NeedFacts {
        NeedFacts {
            card: denied_release(),
            now_secs: 10_000_000,
            process_started_at: 9_999_000,
            cause,
        }
    }

    fn app_data_prompt() -> NeedCause {
        NeedCause::PromptObserved {
            service: Some(Folder::AppData),
            session: Some(1),
        }
    }

    #[test]
    fn a_verified_grant_reading_off_in_a_fresh_process_is_settling_not_regressed() {
        let verified = AccessRecord {
            verified_at: Some(9_000_000),
            ..AccessRecord::default()
        };
        let fresh = NeedFacts {
            process_started_at: 10_000_000 - 5,
            ..need_facts(app_data_prompt())
        };
        assert_eq!(
            decide_need(&fresh, None, &verified),
            NeedVerdict::Quiet(NotDue::Settling)
        );
        let settled = NeedFacts {
            process_started_at: 10_000_000 - LAUNCH_SETTLE.as_secs(),
            ..fresh.clone()
        };
        assert_eq!(
            decide_need(&settled, None, &verified),
            NeedVerdict::Ask { regressed: true },
            "the second denied read marks the grant regressed"
        );
        assert_eq!(
            decide_need(&fresh, None, &AccessRecord::default()),
            NeedVerdict::Ask { regressed: false }
        );
    }

    #[test]
    fn a_prompt_while_the_probe_reads_granted_logs_an_anomaly_and_asks_nothing() {
        let granted = NeedFacts {
            card: CardFacts {
                fda: FdaState::Granted,
                ..denied_release()
            },
            ..need_facts(app_data_prompt())
        };
        let verdict = decide_need(&granted, None, &AccessRecord::default());
        assert_eq!(verdict, NeedVerdict::Quiet(NotDue::Granted));
        assert!(is_prompt_while_granted(app_data_prompt(), &verdict));
        assert!(!is_prompt_while_granted(
            NeedCause::ProtectedRead { session: None },
            &verdict
        ));
        assert!(!is_prompt_while_granted(
            app_data_prompt(),
            &NeedVerdict::Ask { regressed: false }
        ));
    }

    #[test]
    fn the_opened_state_lapses_with_the_owner_acted_hold() {
        let record = AccessRecord {
            opened_at: Some(1_000_000),
            ..AccessRecord::default()
        };
        let hold = OWNER_ACTED_HOLD.as_secs();
        assert_eq!(
            report_state(None, &record, FdaState::Denied, 1_000_000 + hold - 1),
            "opened"
        );
        assert_eq!(
            report_state(None, &record, FdaState::Denied, 1_000_000 + hold),
            "quiet"
        );
    }

    #[test]
    fn decide_need_is_a_table_of_named_refusals_then_an_ask() {
        let none = AccessRecord::default();
        let facts = need_facts(app_data_prompt());
        assert_eq!(
            decide_need(&facts, None, &none),
            NeedVerdict::Ask { regressed: false }
        );
        assert_eq!(
            decide_need(&facts, Some(Marker::Opened), &none),
            NeedVerdict::Ask { regressed: false },
            "an opened marker is not an answer"
        );
        let off = NeedFacts {
            card: CardFacts {
                offered: false,
                ..denied_release()
            },
            ..facts.clone()
        };
        assert_eq!(
            decide_need(&off, None, &none),
            NeedVerdict::Quiet(NotDue::Off)
        );
        let headless = NeedFacts {
            card: CardFacts {
                headless: true,
                ..denied_release()
            },
            ..facts.clone()
        };
        assert_eq!(
            decide_need(&headless, None, &none),
            NeedVerdict::Quiet(NotDue::Headless)
        );
        for service in [Some(Folder::Documents), Some(Folder::Desktop), None] {
            let other = need_facts(NeedCause::PromptObserved {
                service,
                session: None,
            });
            assert_eq!(
                decide_need(&other, None, &none),
                NeedVerdict::Quiet(NotDue::NotThisClass),
                "{service:?}"
            );
        }
        let eperm = need_facts(NeedCause::ProtectedRead { session: Some(2) });
        assert_eq!(
            decide_need(&eperm, None, &none),
            NeedVerdict::Ask { regressed: false }
        );
        assert_eq!(
            decide_need(&facts, Some(Marker::NotNow), &none),
            NeedVerdict::Quiet(NotDue::Answered)
        );
        for dr in [DrClass::Cdhash, DrClass::Unsigned, DrClass::Unknown] {
            let unstable = NeedFacts {
                card: CardFacts {
                    dr,
                    ..denied_release()
                },
                ..facts.clone()
            };
            assert_eq!(
                decide_need(&unstable, None, &none),
                NeedVerdict::Quiet(NotDue::GrantUnstable(dr)),
                "{dr:?} is not grant-stable"
            );
        }
        let granted = NeedFacts {
            card: CardFacts {
                fda: FdaState::Granted,
                ..denied_release()
            },
            ..facts.clone()
        };
        assert_eq!(
            decide_need(&granted, None, &none),
            NeedVerdict::Quiet(NotDue::Granted)
        );
        let not_observed = NeedFacts {
            card: CardFacts {
                fda: FdaState::Unknown,
                ..denied_release()
            },
            ..facts.clone()
        };
        assert_eq!(
            decide_need(&not_observed, None, &none),
            NeedVerdict::Quiet(NotDue::NotObservedDenied),
            "unknown is not denied"
        );
        let asked = AccessRecord {
            asked_at: Some(facts.now_secs - 60),
            ..none
        };
        assert_eq!(
            decide_need(&facts, None, &asked),
            NeedVerdict::Quiet(NotDue::RecentlyAsked)
        );
        let week = RENEW_AFTER.as_secs();
        let inside = NeedFacts {
            now_secs: facts.now_secs - 61 + week,
            ..facts.clone()
        };
        assert_eq!(
            decide_need(&inside, None, &asked),
            NeedVerdict::Quiet(NotDue::RecentlyAsked),
            "one second short of asked_at + the week is still inside it"
        );
        let renewed = NeedFacts {
            now_secs: facts.now_secs - 60 + week,
            ..facts.clone()
        };
        assert_eq!(
            decide_need(&renewed, None, &asked),
            NeedVerdict::Ask { regressed: false },
            "the renewal boundary is exact"
        );
        for reason in [
            NotDue::Off,
            NotDue::Headless,
            NotDue::Answered,
            NotDue::Granted,
            NotDue::NotObservedDenied,
            NotDue::GrantUnstable(DrClass::Cdhash),
            NotDue::NotThisClass,
            NotDue::OwnerActed,
            NotDue::RecentlyAsked,
            NotDue::NoNeedYet,
            NotDue::Held,
            NotDue::ReadsOffAfterVerified,
        ] {
            let line = reason.to_string();
            assert!(!line.is_empty() && !line.contains('\n'), "{line:?}");
        }
    }

    #[test]
    fn an_ask_after_a_verified_grant_reads_regressed_and_opens_a_new_epoch() {
        let facts = need_facts(app_data_prompt());
        let t = facts.now_secs;
        let record = AccessRecord {
            asked_at: Some(t - 3600),
            verified_at: Some(t - 1800),
            verified_build: Some(7),
            ..AccessRecord::default()
        };
        assert_eq!(
            decide_need(&facts, None, &record),
            NeedVerdict::Ask { regressed: true },
            "an ask made before the verification does not bar the new epoch"
        );
        let asked_again = AccessRecord {
            asked_at: Some(t - 10),
            ..record
        };
        assert_eq!(
            decide_need(&facts, None, &asked_again),
            NeedVerdict::Quiet(NotDue::RecentlyAsked)
        );
    }

    #[test]
    fn decide_launch_never_asks_and_confirms_only_an_opened_grant() {
        let none = AccessRecord::default();
        let denied = denied_release();
        assert_eq!(
            decide_launch(&denied, None, &none),
            LaunchVerdict::Quiet(NotDue::NoNeedYet),
            "a denied probe alone raises nothing"
        );
        assert_eq!(
            decide_launch(&denied, Some(Marker::Opened), &none),
            LaunchVerdict::Quiet(NotDue::NoNeedYet),
            "opened-but-still-denied is not an answer, and not a question either"
        );
        let off = CardFacts {
            offered: false,
            ..denied_release()
        };
        assert_eq!(
            decide_launch(&off, None, &none),
            LaunchVerdict::Quiet(NotDue::Off)
        );
        let headless = CardFacts {
            headless: true,
            ..denied_release()
        };
        assert_eq!(
            decide_launch(&headless, None, &none),
            LaunchVerdict::Quiet(NotDue::Headless)
        );
        assert_eq!(
            decide_launch(&denied, Some(Marker::NotNow), &none),
            LaunchVerdict::Quiet(NotDue::Answered)
        );
        let granted = CardFacts {
            fda: FdaState::Granted,
            ..denied_release()
        };
        assert_eq!(
            decide_launch(&granted, None, &none),
            LaunchVerdict::Quiet(NotDue::Granted)
        );
        assert_eq!(
            decide_launch(&granted, Some(Marker::Opened), &none),
            LaunchVerdict::Confirm,
            "the flip the owner started in an earlier process is acknowledged once"
        );
        let asked = AccessRecord {
            asked_at: Some(5),
            ..none
        };
        assert_eq!(
            decide_launch(&granted, None, &asked),
            LaunchVerdict::Confirm,
            "a grant after an ask is acknowledged whichever gesture made it"
        );
        let verified = AccessRecord {
            asked_at: Some(5),
            verified_at: Some(9),
            ..none
        };
        assert_eq!(
            decide_launch(&granted, None, &verified),
            LaunchVerdict::Quiet(NotDue::Held),
            "already verified: nothing to say again"
        );
        assert_eq!(
            decide_launch(&granted, Some(Marker::NotNow), &none),
            LaunchVerdict::Quiet(NotDue::Answered)
        );
        assert_eq!(
            decide_launch(&denied, None, &verified),
            LaunchVerdict::Quiet(NotDue::ReadsOffAfterVerified),
            "quiet and LOGGED; the next ask's words say it regressed"
        );
        let unknown = CardFacts {
            fda: FdaState::Unknown,
            ..denied_release()
        };
        assert_eq!(
            decide_launch(&unknown, None, &none),
            LaunchVerdict::Quiet(NotDue::NoNeedYet)
        );
        assert!(should_reassert(&asked, None, FdaState::Denied));
        assert!(!should_reassert(
            &asked,
            Some(Marker::Opened),
            FdaState::Denied
        ));
        assert!(!should_reassert(
            &asked,
            Some(Marker::NotNow),
            FdaState::Denied
        ));
        assert!(!should_reassert(&asked, None, FdaState::Granted));
        assert!(!should_reassert(&none, None, FdaState::Denied));
        assert!(!should_reassert(&verified, None, FdaState::Denied));
    }

    #[test]
    fn an_owner_who_opened_settings_is_not_asked_again_by_an_older_window() {
        let t = 10_000_000_u64;
        let record = AccessRecord {
            asked_at: Some(t - 100),
            opened_at: Some(t - 50),
            ..AccessRecord::default()
        };
        let older_window = NeedFacts {
            now_secs: t + RENEW_AFTER.as_secs() + 1,
            process_started_at: t - 5_000,
            ..need_facts(app_data_prompt())
        };
        assert_eq!(
            decide_need(&older_window, None, &record),
            NeedVerdict::Quiet(NotDue::OwnerActed),
            "the week elapsed and the probe reads denied, but this window predates the press"
        );
        let lapsed = NeedFacts {
            now_secs: t - 50 + OWNER_ACTED_HOLD.as_secs(),
            ..older_window.clone()
        };
        assert_eq!(
            decide_need(&lapsed, None, &record),
            NeedVerdict::Ask { regressed: false },
            "the hold lapses after fourteen days and the week has long elapsed"
        );
        let just_inside = NeedFacts {
            now_secs: t - 51 + OWNER_ACTED_HOLD.as_secs(),
            ..older_window.clone()
        };
        assert_eq!(
            decide_need(&just_inside, None, &record),
            NeedVerdict::Quiet(NotDue::OwnerActed)
        );
        let newer_window = NeedFacts {
            process_started_at: t,
            ..older_window.clone()
        };
        assert_eq!(
            decide_need(&newer_window, None, &record),
            NeedVerdict::Ask { regressed: false }
        );
        let newer_soon = NeedFacts {
            now_secs: t + 60,
            process_started_at: t,
            ..older_window.clone()
        };
        assert_eq!(
            decide_need(&newer_soon, None, &record),
            NeedVerdict::Quiet(NotDue::RecentlyAsked)
        );
        let verified = AccessRecord {
            verified_at: Some(t),
            ..record
        };
        assert_eq!(
            decide_need(&older_window, None, &verified),
            NeedVerdict::Ask { regressed: true }
        );
    }

    #[test]
    fn the_marker_round_trips_beside_the_config() {
        let dir = std::env::temp_dir().join(format!(
            "aterm-consent-card-{}-{}",
            std::process::id(),
            aterm_time::since_epoch().unwrap().as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let config = dir.join("aterm.toml");
        assert_eq!(marker_path(Some(&config)), Some(dir.join(MARKER)));
        assert_eq!(marker_path(None), None);
        assert_eq!(read_marker(Some(&config)), None);
        assert!(record_marker(None, Marker::NotNow).is_err());
        assert!(
            clear_opened(None).is_ok(),
            "nothing to clear is not an error"
        );

        record_marker(Some(&config), Marker::Opened).unwrap();
        assert_eq!(read_marker(Some(&config)), Some(Marker::Opened));
        clear_opened(Some(&config)).unwrap();
        assert_eq!(read_marker(Some(&config)), None, "opened is consumed");
        assert!(!dir.join(MARKER).exists());

        record_marker(Some(&config), Marker::NotNow).unwrap();
        assert_eq!(read_marker(Some(&config)), Some(Marker::NotNow));
        record_marker(Some(&config), Marker::Opened).unwrap();
        assert_eq!(
            read_marker(Some(&config)),
            Some(Marker::NotNow),
            "opening Settings is not an answer: it never replaces the owner's not-now"
        );
        clear_opened(Some(&config)).unwrap();
        assert_eq!(
            read_marker(Some(&config)),
            Some(Marker::NotNow),
            "an answer is the owner's and is never cleared"
        );
        for marker in [Marker::NotNow, Marker::Opened] {
            assert!(marker.as_str().len() <= MAX_MARKER_BYTES);
            assert_eq!(Marker::parse(marker.as_str()), Some(marker));
        }
        std::fs::write(dir.join(MARKER), "not-now\n").unwrap();
        assert_eq!(read_marker(Some(&config)), Some(Marker::NotNow));
        std::fs::write(dir.join(MARKER), "x".repeat(MAX_MARKER_BYTES + 1)).unwrap();
        assert_eq!(read_marker(Some(&config)), None);
        std::fs::write(dir.join(MARKER), "granted").unwrap();
        assert_eq!(
            read_marker(Some(&config)),
            None,
            "an unknown token is no record"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_cards_records_are_only_ordered() {
        use aterm_durable::{Flush, seam::Recording};
        let dir = std::env::temp_dir().join(format!(
            "aterm-consent-card-flush-{}-{}",
            std::process::id(),
            aterm_time::since_epoch().unwrap().as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let config = dir.join("aterm.toml");
        let recording = Recording::start();
        record_marker(Some(&config), Marker::NotNow).unwrap();
        assert_eq!(recording.take(), [Flush::Ordered], "the answer");
        let update = AccessRecord {
            verified_at: Some(7),
            ..AccessRecord::default()
        };
        write_record(Some(&config), update).unwrap();
        assert_eq!(recording.take(), [Flush::Ordered], "the access record");
        write_verified_host(Some(&config), "HOST-UUID", "com.aterm.aterm").unwrap();
        assert_eq!(recording.take(), [Flush::Ordered], "the verified host");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_entry_line_names_the_cause_and_the_tab_it_knows() {
        let tab = NeedWords {
            cause: app_data_prompt(),
            tab_number: Some(1),
            regressed: false,
        };
        let no_tab = NeedWords {
            tab_number: None,
            ..tab
        };
        assert!(tab.event_line().contains("tab 1"));
        assert!(!no_tab.event_line().contains("tab"), "never guessed");
        let regressed = NeedWords {
            regressed: true,
            ..tab
        };
        let eperm = NeedWords {
            cause: NeedCause::ProtectedRead { session: Some(2) },
            tab_number: Some(2),
            regressed: false,
        };
        assert_ne!(regressed.event_line(), tab.event_line());
        assert_ne!(eperm.event_line(), tab.event_line());
        assert!(eperm.event_line().contains("tab 2"));
    }

    fn asked_once_model() -> aterm_spec::derive::Model {
        aterm_spec::ty_model! {
            AccessCardAskedOnce {
                const Buggy = 0;
                const Cap = 3;
                var gen = 0;
                var asked = 0;
                var raised = 0;
                var granted = 0;
                var need = 0;
                var opened = 0;
                var fresh = 1;
                var acted_ask = 0;
                action Need when (need == 0) { need = 1; }
                action Ask when (need == 1 && granted == 0 && asked == 0 && (opened == 0 || fresh == 1 || Buggy == 1)) {
                    asked = 1; raised = raised + 1; need = 0;
                    acted_ask = if opened == 1 && fresh == 0 { 1 } else { acted_ask };
                }
                action Suppress when (need == 1 && (asked == 1 || granted == 1 || (opened == 1 && fresh == 0 && Buggy == 0))) { need = 0; }
                action Open when (asked == 1 && opened == 0 && granted == 0) { opened = 1; fresh = 0; }
                action HoldLapses when (opened == 1 && fresh == 0) { opened = 0; fresh = 1; asked = 0; raised = 0; }
                action Grant when (granted == 0) { granted = 1; }
                action Regress when (granted == 1) { granted = 0; asked = 0; raised = 0; opened = 0; fresh = 1; }
                action Renew when (asked == 1 && granted == 0) { asked = 0; raised = 0; }
                action Update when (gen <= Cap - 1) {
                    gen = gen + 1; need = 0; fresh = 1;
                    asked = if Buggy == 1 { 0 } else { asked };
                }
                invariant AtMostOncePerEpoch: raised <= 1;
                invariant NoAskToAnOwnerWhoActedOnAnOlderWindow: acted_ask == 0;
            }
        }
    }

    #[test]
    fn asked_once_model_proves_and_catches_the_historical_shapes() {
        let model = asked_once_model();
        aterm_spec::verify::prove_and_catch_scalar(&model, model.name);
    }

    struct Epochs {
        dir: std::path::PathBuf,
        config: std::path::PathBuf,
        fda: FdaState,
        now: u64,
        started: u64,
        generation: i64,
        raised: i64,
        need: bool,
        acted_ask: bool,
        /// Negative control: a process that keeps no record.
        forgets_record: bool,
        /// Negative control: an ask that ignores the window's age.
        ignores_window_age: bool,
        /// A negative control: the run departs from the model, so its guards are not asserted
        /// (its invariants are the check).
        lenient: bool,
    }

    impl Epochs {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "aterm-asked-once-{tag}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            let config = dir.join("aterm.toml");
            Self {
                dir,
                config,
                fda: FdaState::Denied,
                now: 10_000_000,
                started: 10_000_000,
                generation: 0,
                raised: 0,
                need: false,
                acted_ask: false,
                forgets_record: false,
                ignores_window_age: false,
                lenient: false,
            }
        }

        fn record(&self) -> AccessRecord {
            read_record(Some(&self.config))
        }

        fn facts(&self) -> NeedFacts {
            NeedFacts {
                card: CardFacts {
                    fda: self.fda,
                    ..denied_release()
                },
                now_secs: self.now,
                process_started_at: if self.ignores_window_age {
                    u64::MAX
                } else {
                    self.started
                },
                cause: app_data_prompt(),
            }
        }

        fn verdict(&self) -> NeedVerdict {
            decide_need(&self.facts(), None, &self.record())
        }

        fn write(&self, update: AccessRecord) {
            write_record(Some(&self.config), update).unwrap();
        }

        fn project(&self) -> std::collections::BTreeMap<&'static str, i64> {
            let record = self.record();
            let granted = self.fda == FdaState::Granted;
            let live = |at: Option<u64>, span: Duration| {
                at.is_some_and(|at| {
                    self.now < at + span.as_secs()
                        && (granted || record.verified_at.is_none_or(|v| v <= at))
                })
            };
            let asked = live(record.asked_at, RENEW_AFTER);
            let opened = live(record.opened_at, OWNER_ACTED_HOLD);
            let fresh = !opened || self.started >= record.opened_at.unwrap_or(0);
            std::collections::BTreeMap::from([
                ("gen", self.generation),
                ("asked", i64::from(asked)),
                ("raised", self.raised),
                ("granted", i64::from(granted)),
                ("need", i64::from(self.need)),
                ("opened", i64::from(opened)),
                ("fresh", i64::from(fresh)),
                ("acted_ask", i64::from(self.acted_ask)),
            ])
        }

        fn apply(
            &mut self,
            model: &aterm_spec::derive::Model,
            action: &str,
            before: &std::collections::BTreeMap<&'static str, i64>,
        ) -> Result<(), String> {
            self.now += 1;
            match action {
                "Need" => self.need = true,
                "Ask" | "Suppress" => {
                    self.now = self.now.max(self.started + LAUNCH_SETTLE.as_secs());
                    let verdict = self.verdict();
                    let asks = matches!(verdict, NeedVerdict::Ask { .. });
                    if asks != (action == "Ask") {
                        return Err(format!("the real decision {verdict:?} is not {action}"));
                    }
                    let other = if action == "Ask" { "Suppress" } else { "Ask" };
                    if !self.lenient && model.action_enabled(other, before) {
                        return Err(format!("the model allows {other} too"));
                    }
                    self.need = false;
                    if asks {
                        self.acted_ask =
                            self.acted_ask || (before["opened"] == 1 && before["fresh"] == 0);
                        self.raised += 1;
                        self.write(AccessRecord {
                            asked_at: Some(self.now),
                            ..AccessRecord::default()
                        });
                    }
                }
                "Open" => self.write(AccessRecord {
                    opened_at: Some(self.now),
                    ..AccessRecord::default()
                }),
                "HoldLapses" => {
                    self.now += OWNER_ACTED_HOLD.as_secs() + 1;
                    self.raised = 0;
                }
                "Grant" => {
                    self.fda = FdaState::Granted;
                    self.write(AccessRecord {
                        verified_at: Some(self.now),
                        verified_build: Some(7),
                        ..AccessRecord::default()
                    });
                }
                "Regress" => {
                    self.fda = FdaState::Denied;
                    self.raised = 0;
                }
                "Renew" => {
                    self.now += RENEW_AFTER.as_secs() + 1;
                    self.raised = 0;
                }
                "Update" => {
                    self.generation += 1;
                    self.need = false;
                    self.started = self.now;
                    if self.forgets_record {
                        let _ = std::fs::remove_file(record_path(Some(&self.config)).unwrap());
                    }
                }
                other => return Err(format!("unknown action {other}")),
            }
            Ok(())
        }
    }

    impl Drop for Epochs {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn run_epochs(
        epochs: &mut Epochs,
        actions: &[&str],
    ) -> std::collections::BTreeMap<&'static str, i64> {
        let model = asked_once_model();
        let mut expected = model.init_state();
        assert_eq!(epochs.project(), expected, "both start in the same state");
        for action in actions {
            let before = expected.clone();
            assert!(
                model.action_enabled(action, &before),
                "{action}: the model forbids it in {before:?}"
            );
            epochs
                .apply(&model, action, &before)
                .unwrap_or_else(|why| panic!("{action}: {why}"));
            assert!(model.fire(action, &mut expected));
            assert_eq!(
                epochs.project(),
                expected,
                "{action}: the real state departs from the model"
            );
        }
        expected
    }

    #[test]
    fn asked_once_conforms_across_process_generations() {
        let model = asked_once_model();
        for (tag, actions) in [
            (
                "eight-updates",
                &[
                    "Need", "Ask", "Update", "Need", "Suppress", "Update", "Need", "Suppress",
                ][..],
            ),
            (
                "opened-then-updated",
                &["Need", "Ask", "Open", "Update", "Need", "Suppress"][..],
            ),
            (
                "owner-acted",
                &["Need", "Ask", "Open", "Renew", "Need", "Suppress"][..],
            ),
            (
                "hold-lapses",
                &["Need", "Ask", "Open", "HoldLapses", "Need", "Ask"][..],
            ),
            (
                "grant-and-regress",
                &[
                    "Need", "Ask", "Grant", "Need", "Suppress", "Regress", "Need", "Ask",
                ][..],
            ),
            (
                "renews-weekly",
                &["Need", "Ask", "Renew", "Need", "Ask"][..],
            ),
            (
                "granted-first",
                &["Grant", "Update", "Regress", "Need", "Ask"][..],
            ),
        ] {
            let mut epochs = Epochs::new(tag);
            let end = run_epochs(&mut epochs, actions);
            assert!(model.check_invariant("AtMostOncePerEpoch", &end), "{tag}");
            assert!(
                model.check_invariant("NoAskToAnOwnerWhoActedOnAnOlderWindow", &end),
                "{tag}"
            );
        }
    }

    #[test]
    fn asked_once_catches_the_process_that_forgets_and_the_ask_that_ignores_the_window() {
        let model = asked_once_model();
        let mut forgetful = Epochs::new("forgets");
        forgetful.forgets_record = true;
        forgetful.lenient = true;
        let model_free = |epochs: &mut Epochs, action: &str| {
            let before = epochs.project();
            epochs.apply(&model, action, &before).unwrap();
        };
        for action in ["Need", "Ask", "Update", "Need", "Ask"] {
            model_free(&mut forgetful, action);
        }
        assert_eq!(forgetful.raised, 2, "the per-process card asks twice");
        assert!(
            !model.check_invariant("AtMostOncePerEpoch", &forgetful.project()),
            "the invariant reports it"
        );
        let mut blind = Epochs::new("blind");
        blind.ignores_window_age = true;
        blind.lenient = true;
        for action in ["Need", "Ask", "Open", "Renew", "Need", "Ask"] {
            model_free(&mut blind, action);
        }
        assert!(
            blind.acted_ask,
            "asked an owner who acted on an older window"
        );
        assert!(
            !model.check_invariant("NoAskToAnOwnerWhoActedOnAnOlderWindow", &blind.project()),
            "the invariant reports it"
        );
    }

    #[test]
    fn the_access_record_round_trips_and_a_foreign_file_is_no_record() {
        let epochs = Epochs::new("record");
        let config = Some(epochs.config.as_path());
        assert_eq!(read_record(config), AccessRecord::default());
        assert!(write_record(None, AccessRecord::default()).is_err());
        let first = AccessRecord {
            asked_at: Some(100),
            opened_at: Some(200),
            verified_at: None,
            verified_build: None,
        };
        assert_eq!(write_record(config, first).unwrap(), first);
        assert_eq!(read_record(config), first);
        let merged = write_record(
            config,
            AccessRecord {
                asked_at: Some(50),
                opened_at: Some(300),
                verified_at: Some(400),
                verified_build: Some(9),
            },
        )
        .unwrap();
        assert_eq!(
            merged,
            AccessRecord {
                asked_at: Some(100),
                opened_at: Some(300),
                verified_at: Some(400),
                verified_build: Some(9),
            },
            "opened_at is written as a max"
        );
        assert_eq!(AccessRecord::parse(&merged.render()), merged);
        assert!(merged.render().len() <= MAX_RECORD_BYTES);
        let maximal = AccessRecord {
            asked_at: Some(u64::MAX),
            opened_at: Some(u64::MAX),
            verified_at: Some(u64::MAX),
            verified_build: Some(u64::MAX),
        };
        assert!(
            maximal.render().len() <= MAX_RECORD_BYTES,
            "four maximal fields still fit the bound"
        );
        let path = epochs.dir.join(RECORD);
        for foreign in [
            "hello\n",
            "asked_at=abc\n",
            "asked_at=1\nsecret=2\n",
            "asked_at=1\nnot a line\n",
        ] {
            std::fs::write(&path, foreign).unwrap();
            assert_eq!(read_record(config), AccessRecord::default(), "{foreign:?}");
        }
        std::fs::write(&path, "asked_at=1\n".repeat(MAX_RECORD_BYTES)).unwrap();
        assert_eq!(read_record(config), AccessRecord::default(), "oversized");
        std::fs::write(&path, "\nasked_at = 12\n\nverified_at=13\n").unwrap();
        assert_eq!(
            read_record(config),
            AccessRecord {
                asked_at: Some(12),
                verified_at: Some(13),
                ..AccessRecord::default()
            }
        );
    }

    #[test]
    fn on_need_re_arms_a_settled_card_and_reoffer_no_longer_exists() {
        let t0 = Instant::now();
        let mut s = CardState::new();
        assert!(!s.on_need(t0));
        assert_eq!(s.phase(), CardPhase::Idle);
        assert!(!s.take_pending_need(), "only a settled card gives it up");
        assert!(s.begin_deciding(t0));
        assert!(!s.on_need(t0), "a verdict is coming: the need is kept");
        assert!(s.on_decided(&Verdict::Quiet(NotDue::NoNeedYet), t0));
        assert_eq!(s.phase(), CardPhase::Settled);
        assert!(s.take_pending_need(), "re-evaluated the moment it settles");
        assert!(!s.take_pending_need(), "and only once");
        assert!(s.on_need(t0 + Duration::from_secs(1)));
        assert!(matches!(s.phase(), CardPhase::Deciding { .. }));
        assert_eq!(
            s.lifecycle_deadline(),
            Some(t0 + Duration::from_secs(1) + DECIDE_TIMEOUT)
        );
        assert!(s.on_decided(&Verdict::Offer, t0));
        assert_eq!(s.phase(), CardPhase::Due);
        assert!(!s.on_need(t0));
        assert!(s.raise_allowed(t0));
        s.on_raised(t0);
        assert!(!s.on_need(t0));
        assert!(!s.take_pending_need(), "still watching");
        assert!(s.on_row_retired());
        assert_eq!(s.phase(), CardPhase::Settled);
        assert_eq!(s.lifecycle_deadline(), None);
        assert!(
            s.take_pending_need(),
            "the need kept while watching returns"
        );
        let mut lapses = CardState::new();
        lapses.settle();
        assert!(lapses.on_need(t0));
        for attempt in 1..=DECIDE_ATTEMPTS {
            assert!(lapses.deciding_lapsed(t0 + DECIDE_TIMEOUT));
            assert_eq!(lapses.reopen_deciding(), attempt < DECIDE_ATTEMPTS);
            if attempt < DECIDE_ATTEMPTS {
                assert!(lapses.begin_deciding(t0));
            }
        }
        assert_eq!(lapses.phase(), CardPhase::Settled);
    }

    #[test]
    fn a_swallowed_verdict_is_asked_for_again_and_then_given_up_on() {
        let t0 = Instant::now();
        let mut s = CardState::new();
        for attempt in 1..=DECIDE_ATTEMPTS {
            assert!(s.begin_deciding(t0), "attempt {attempt} starts");
            assert!(!s.deciding_lapsed(t0), "not lapsed on the same instant");
            assert!(!s.deciding_lapsed(t0 + DECIDE_TIMEOUT - Duration::from_millis(1)));
            assert!(s.deciding_lapsed(t0 + DECIDE_TIMEOUT));
            let again = s.reopen_deciding();
            assert_eq!(
                again,
                attempt < DECIDE_ATTEMPTS,
                "attempt {attempt} of {DECIDE_ATTEMPTS}"
            );
        }
        assert_eq!(s.phase(), CardPhase::Settled, "the attempts are spent");
        assert_eq!(s.lifecycle_deadline(), None);
        assert!(!s.begin_deciding(t0), "and it does not start again");
        assert!(
            !s.on_decided(&Verdict::Offer, t0),
            "a late verdict is stale"
        );
        let mut ok = CardState::new();
        assert!(ok.begin_deciding(t0));
        assert!(ok.on_decided(&Verdict::Offer, t0));
        assert_eq!(ok.phase(), CardPhase::Due);
    }

    #[test]
    fn the_lifecycle_decides_once_watches_for_a_while_and_settles() {
        let t0 = Instant::now();
        let mut s = CardState::new();
        assert_eq!(s.phase(), CardPhase::Idle);
        assert!(!s.wants_probe(t0), "idle: nothing to watch");
        assert!(!s.on_row_retired(), "idle: no row to lose");
        assert!(s.begin_deciding(Instant::now()));
        assert!(!s.begin_deciding(Instant::now()), "the one-shot fires once");
        assert!(matches!(s.phase(), CardPhase::Deciding { .. }));
        s.on_raised(t0);
        assert!(
            matches!(s.phase(), CardPhase::Deciding { .. }),
            "nothing to raise yet"
        );

        assert!(s.on_decided(&Verdict::Offer, t0));
        assert_eq!(s.phase(), CardPhase::Due);
        assert!(
            !s.on_decided(&Verdict::Offer, t0),
            "a second verdict is stale"
        );
        assert!(!s.wants_probe(t0), "due but not posted: no watch yet");
        assert!(!s.on_row_retired(), "not posted: no row to lose");
        assert!(s.raise_allowed(t0), "the first raise is always allowed");
        s.on_raised(t0);
        assert_eq!(s.phase(), CardPhase::Watching { since: t0 });
        assert!(s.wants_probe(t0 + Duration::from_secs(60)));
        assert!(!s.raise_allowed(t0), "posted once; never twice");

        let mut d = CardState::new();
        assert!(d.begin_deciding(Instant::now()));
        assert!(d.on_decided(&Verdict::Offer, t0));
        assert!(d.raise_allowed(t0 + WATCH_FOR - Duration::from_secs(1)));
        assert!(!d.raise_allowed(t0 + WATCH_FOR));
        assert_eq!(d.phase(), CardPhase::Settled);
        assert_eq!(d.lifecycle_deadline(), None);

        let mut c = CardState::new();
        assert!(c.begin_deciding(Instant::now()));
        assert!(c.on_decided(&Verdict::Confirm, t0));
        assert_eq!(c.phase(), CardPhase::Settled);
        assert_eq!(c.lifecycle_deadline(), None);

        let t2 = t0 + Duration::from_secs(20);
        let mut o = CardState::new();
        assert!(o.begin_deciding(Instant::now()));
        assert!(o.on_decided(&Verdict::Offer, t0));
        o.on_raised(t0);
        o.on_opened_settings(t2);
        assert_eq!(o.phase(), CardPhase::Watching { since: t2 });
        assert!(!o.on_row_retired());
        assert_eq!(o.phase(), CardPhase::Watching { since: t2 });
        assert!(o.wants_probe(t2 + WATCH_FOR - Duration::from_secs(1)));
        assert!(!o.wants_probe(t2 + WATCH_FOR), "the watch is bounded");
        assert_eq!(o.phase(), CardPhase::Settled);
        o.on_opened_settings(t2);
        assert_eq!(
            o.phase(),
            CardPhase::Watching { since: t2 },
            "a new Settings gesture starts a fresh bounded watch"
        );
        assert!(!o.wants_probe(t2 + WATCH_FOR));
        assert_eq!(
            o.lifecycle_deadline(),
            None,
            "an explicit gesture never reoffers the card"
        );

        let mut b = CardState::new();
        assert!(b.begin_deciding(Instant::now()));
        assert!(b.on_decided(&Verdict::Offer, t0));
        b.on_raised(t0);
        b.on_owner_acted();
        assert!(b.wants_probe(t0 + Duration::from_secs(1)));
        assert!(!b.on_row_retired());
        assert_eq!(b.phase(), CardPhase::Watching { since: t0 });
        assert!(!b.wants_probe(t0 + WATCH_FOR));
        assert_eq!(b.lifecycle_deadline(), None, "read rows do not return");

        let mut n = CardState::new();
        assert!(n.begin_deciding(Instant::now()));
        assert!(n.on_decided(&Verdict::Quiet(NotDue::Granted), t0));
        assert_eq!(n.phase(), CardPhase::Settled);

        let mut w = CardState::new();
        assert!(w.begin_deciding(Instant::now()));
        assert!(w.on_decided(&Verdict::Offer, t0));
        w.on_raised(t0);
        w.settle();
        assert_eq!(w.phase(), CardPhase::Settled);
        assert!(!w.wants_probe(t0));
        assert!(!w.on_row_retired());
    }
}
