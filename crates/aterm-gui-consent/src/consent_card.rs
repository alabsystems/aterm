// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The macOS access card's decisions. It posts nothing: the need surfaces as System State's Full
//! Disk Access row.

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

/// How long the instance re-checks the probe for the grant after the question is raised, and how
/// long a due card waits on a pending probe before it is retired unraised.
pub const WATCH_FOR: Duration = Duration::from_secs(30 * 60);

const DECIDE_TIMEOUT: Duration = Duration::from_secs(20);

/// How many verdicts the one-shot will wait for before giving up for the life of the process.
pub const DECIDE_ATTEMPTS: u8 = 3;

/// How long after an ask the next may be made in the same epoch.
pub const RENEW_AFTER: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// How long an owner who pressed *Open Settings* is presumed to have acted.
pub const OWNER_ACTED_HOLD: Duration = Duration::from_secs(14 * 24 * 60 * 60);

/// How long a fresh process gives the window's own probe before a denied read beside a verified
/// grant is believed.
pub const LAUNCH_SETTLE: Duration = Duration::from_secs(90);

/// Needs closer together than this are one need.
pub const NEED_DEBOUNCE: Duration = Duration::from_secs(60);

/// At most one `aterm.log` line per this interval says that macOS asked.
pub const NEED_LOG_EVERY: Duration = Duration::from_secs(6 * 60 * 60);

/// The retired row's title: the ask, a question, so it claims nothing.
#[cfg_attr(not(test), allow(dead_code))]
pub const FDA_TITLE: &str = "Turn on Full Disk Access?";

/// Why the owner is needed: an Allow is forgotten when aterm updates.
pub const FDA_WHY: &str = "macOS forgets an Allow when aterm updates";

/// The one action, as an imperative: the entry's `Then::press` sentence.
pub const FDA_THEN: &str = "turn on aterm in Full Disk Access";

/// What the switch grants, said where the ask is: a consequence Apple's own pane states ("data
/// like Mail, Messages, Safari"), not a promise of a benefit.
pub const FDA_GRANTS: &str = "Full Disk Access lets programs you run in aterm, agents included, read protected data such as Mail and Messages";

/// What aterm does next.
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

/// Where the prompting program runs, as the card names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TabPlace {
    /// A tab the strip draws, 1-based.
    Number(usize),
    /// A worker's tab: under the Workers tab, with no chip for a number to name.
    Workers,
}

impl TabPlace {
    /// The place as a sentence ends on it: `in tab 2`, `under Workers`.
    #[must_use]
    pub fn words(self) -> String {
        match self {
            Self::Number(n) => format!("in tab {n}"),
            Self::Workers => "under Workers".to_owned(),
        }
    }
}

/// The data the words are built from, so the reporter stays pure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NeedWords {
    pub cause: NeedCause,
    /// Where the prompting program runs, when it resolved.
    pub tab: Option<TabPlace>,
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
            .tab
            .map_or_else(String::new, |tab| format!(" {}", tab.words()));
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
            tab: Some(TabPlace::Number(1)),
            regressed: false,
        }
    }
}

/// What the marker records.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Marker {
    /// *Not now* was pressed by an older aterm: System State spends it as the Not now of the
    /// first approved setup revision it meets, then retires it ([`retire_not_now`]).
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

/// Retire a `not-now` marker once System State has spent it as an approved
/// setup revision's Not now: removed only while it still reads `not-now`, so an
/// `opened` (or anything else) is left as found. Worker thread only.
pub fn retire_not_now(config_path: Option<&Path>) -> std::io::Result<()> {
    let _serial = MARKER_WRITES.lock().unwrap_or_else(|p| p.into_inner());
    let Some(path) = marker_path(config_path) else {
        return Ok(());
    };
    if crate::config_marker::read_marker(&path, MAX_MARKER_BYTES)
        .and_then(|text| Marker::parse(&text))
        != Some(Marker::NotNow)
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
    /// The field-wise max of two records: no field ever moves backwards.
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
            let value = value.trim();
            if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
                return Self::default();
            }
            let Ok(value) = value.parse::<u64>() else {
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

/// Merge `update` into the record on disk (field-wise max, under the shared write lock) and
/// return what is there now.
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

/// Whether the record's grant was verified on the Mac whose hardware UUID is `here`, for
/// `bundle_id`; `None` without a stamp, a `here` or a `bundle_id`.
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

/// Note that the grant was verified on the Mac whose hardware UUID is `here`, for `bundle_id`;
/// the stamp keeps the most recent `MAX_VERIFIED_HOSTS` entries.
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
    /// The launch check's looks again at a verified grant it read off while settling.
    pub settle_look: SettleLook,
    /// A `[privacy]` switch settled the card while its launch check was still owed; the host
    /// runs the check again once every switch is on ([`CardState::rearm_launch`]).
    pub launch_held: bool,
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
    /// The probe did not observe a denial (`unknown`: refused, or the lane is off).
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
    /// A verified grant reads off within [`LAUNCH_SETTLE`] of this process starting: not
    /// concluded.
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

/// Whether a need verdict is the anomaly the log keeps a trace of: macOS showed a dialog for a
/// program of this aterm while this process reads Full Disk Access as granted.
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

/// The launch decision: runs once per process at the first park and never asks. A verified grant
/// read off in a process younger than [`LAUNCH_SETTLE`] (`age`) is not concluded: an update's
/// successor reads it off in its first second, before macOS attributes the process to aterm
/// ([`SettleLook`] reads it again).
pub fn decide_launch(
    facts: &CardFacts,
    age: Duration,
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
        FdaState::Denied if record.verified_at.is_some() && age < LAUNCH_SETTLE => {
            LaunchVerdict::Quiet(NotDue::Settling)
        }
        FdaState::Denied if record.verified_at.is_some() => {
            LaunchVerdict::Quiet(NotDue::ReadsOffAfterVerified)
        }
        FdaState::Denied | FdaState::Unknown => LaunchVerdict::Quiet(NotDue::NoNeedYet),
    }
}

/// The launch check's looks again at a verified grant it read off while the process settled
/// ([`NotDue::Settling`]): once settled, then, if it still reads off, at the first focus gain
/// after that. Each look reads the window's probe afresh.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SettleLook {
    /// Nothing to look at again.
    #[default]
    Idle,
    /// The settled look, at `at`.
    Due { at: Instant },
    /// A look's fresh read is out; `focus_after`: the settled look, so a grant still off is looked
    /// at again at the next focus gain.
    Reading { focus_after: bool },
    /// Still off at the settled look: the next focus gain looks again.
    AtFocus,
}

/// What a [`SettleLook`] read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettleRead {
    /// The grant reads held now: the launch read was the young process's.
    Held,
    /// The grant still reads off; `settled`: at the settled look, the first to say so.
    StillOff { settled: bool },
    /// The probe could not say.
    Unread,
}

impl SettleLook {
    /// A launch check that concluded `Settling` at process age `age`, at `now`: the settled look
    /// waits out the rest of [`LAUNCH_SETTLE`] (at least a second).
    pub fn after_launch(age: Duration, now: Instant) -> Self {
        let wait = LAUNCH_SETTLE
            .saturating_sub(age)
            .max(Duration::from_secs(1));
        Self::Due {
            at: now.checked_add(wait).unwrap_or(now),
        }
    }

    /// When the settled look is due, while it waits.
    pub const fn deadline(self) -> Option<Instant> {
        match self {
            Self::Due { at } => Some(at),
            _ => None,
        }
    }

    /// Whether a fresh read is owed at `now` (the settled look's time came): the host reads the
    /// probe afresh and the look waits for that read.
    pub fn start(&mut self, now: Instant) -> bool {
        match *self {
            Self::Due { at } if now >= at => {
                *self = Self::Reading { focus_after: true };
                true
            }
            _ => false,
        }
    }

    /// A focus gain, whose probe read is fresh: the look after a settled one waits for it.
    pub const fn focus(&mut self) {
        if matches!(self, Self::AtFocus) {
            *self = Self::Reading { focus_after: false };
        }
    }

    /// Whether a look waits for a read.
    pub const fn reading(self) -> bool {
        matches!(self, Self::Reading { .. })
    }

    /// The read a look waited for answered `fda`.
    pub const fn read(&mut self, fda: FdaState) -> Option<SettleRead> {
        let Self::Reading { focus_after } = *self else {
            return None;
        };
        let (next, read) = match fda {
            FdaState::Granted => (Self::Idle, SettleRead::Held),
            FdaState::Denied if focus_after => {
                (Self::AtFocus, SettleRead::StillOff { settled: true })
            }
            FdaState::Denied => (Self::Idle, SettleRead::StillOff { settled: false }),
            FdaState::Unknown => (Self::Idle, SettleRead::Unread),
        };
        *self = next;
        Some(read)
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
    /// When the offer became due: the anchor of a due card's wait on a pending probe.
    due_since: Option<Instant>,
    /// When the question was first raised in this episode.
    first_raised: Option<Instant>,
    /// The owner pressed something on the row (Open Settings, or Details).
    owner_acted: bool,
    /// How many times the one-shot has asked its worker for a verdict ([`DECIDE_ATTEMPTS`]).
    decide_attempts: u8,
    /// A need arrived while the card was busy (launch check under way, or a question raised);
    /// it is re-evaluated once the phase settles ([`CardState::take_pending_need`]).
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
            CardPhase::Deciding { since } => since.checked_add(DECIDE_TIMEOUT),
            CardPhase::Due => self
                .due_since
                .and_then(|since| since.checked_add(WATCH_FOR)),
            CardPhase::Watching { since } => since.checked_add(WATCH_FOR),
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

    /// Admit one lifecycle step against the current settings, including a pending worker verdict
    /// or a raised question. With a switch off the card settles and drops a kept need; a launch
    /// check it settled runs again once every switch is on ([`Self::rearm_launch`]).
    pub const fn admit_current_policy(&mut self, enabled: bool, notice: bool) -> bool {
        if enabled && notice {
            true
        } else {
            self.settle();
            self.pending_need = false;
            false
        }
    }

    /// Back to [`CardPhase::Idle`] with a fresh attempt budget and a clean episode, once every
    /// `[privacy]` switch is on again after one settled the card before its launch check gave a
    /// verdict: a switch off is a pause, never an answer. Only from [`CardPhase::Settled`]; `true`
    /// when re-armed. The host, not the machine, knows whether the launch check gave its verdict.
    pub fn rearm_launch(&mut self) -> bool {
        if self.phase != CardPhase::Settled {
            return false;
        }
        *self = Self::new();
        true
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

    /// The grant answers a need kept before it: drop the kept need undecided.
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
            if now.saturating_duration_since(since) >= DECIDE_TIMEOUT)
    }

    /// A lapsed decision: back to `Idle` to ask again (`true`), or settled once
    /// [`DECIDE_ATTEMPTS`] are spent.
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
#[path = "consent_card_tests.rs"]
mod tests;
