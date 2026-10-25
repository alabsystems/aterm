// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The host-facing anatomy of a digest entry (an exception the owner looks at later).

use crate::model::{Intent, Message, Severity, Tag, tags};
use crate::need::{Fingerprint, LaterAsk, Mover, Resurface, Then, Why};
use crate::text::glass_title_fault;

/// Chars of an entry's `what`: a glass title (six words, no clause).
pub const WHAT_CAP: usize = 48;
/// Lines of volatile detail an entry may carry behind `Details ›`.
pub const MORE_LINES: usize = 3;

/// Which reporter speaks.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Family {
    /// Full Disk Access and the OS's questions about aterm.
    Privacy,
    /// The updater (`update.review.<kind>`, a dev build's writes).
    Update,
    /// The live agent upgrade's rows (`harness.upgrade.<kind>[.<tab>]`).
    Upgrade,
    /// The upgrade's host (`harness.upgrade-host.<kind>`): a fact the host observes by event.
    UpgradeHost,
    /// The resume loop's exceptions (`harness.resume.<kind>`).
    Resume,
    /// The supervisor that keeps failing (`harness.supervisor.<kind>`).
    Supervisor,
    /// The Claude Code accounts as a whole (`harness.accounts.<kind>`).
    Accounts,
    /// The AI CLIs aterm manages.
    Ai,
    /// The toolchain lane.
    Toolchain,
    /// The disk reclaim pass.
    Disk,
    /// The render stream's repair that gave up (`render.<tab>.unrepaired`).
    Render,
    /// What a program in a tab asked the terminal for and the owner's own configuration refused.
    Terminal,
    /// Settings a removed feature left behind.
    Removed,
    /// The tabs whose agent cannot reach its API (`harness.api-outage.<cause>`).
    ApiOutage,
    /// The tabs the person took over and has held for half an hour.
    LongHold,
    /// The fleet this Mac cannot read (`fleet.unreadable.<cause>`).
    FleetUnreadable,
    /// The folders whose direct entry count makes everything that lists them slow.
    HugeDir,
    /// The fabric bridge's own state dir (`fabric.state.<cause>`).
    FabricState,
    /// Harness occupancy (`harness.occupancy.<subject>.<cause>`).
    Occupancy,
    /// System State's setup of this Mac (`setup.<cause>`): a fact its page says calmly, such
    /// as changes it made and could not log.
    Setup,
}

impl Family {
    /// The key stem: `Fingerprint::new(stem, subject, cause)` is an entry's key.
    #[inline]
    #[must_use]
    pub const fn stem(self) -> &'static str {
        match self {
            Self::Privacy => "privacy",
            Self::Update => "update.review",
            Self::Upgrade => "harness.upgrade",
            Self::UpgradeHost => "harness.upgrade-host",
            Self::Resume => "harness.resume",
            Self::Supervisor => "harness.supervisor",
            Self::Accounts => "harness.accounts",
            Self::Ai => "ai",
            Self::Toolchain => "toolchain",
            Self::Disk => "disk",
            Self::Render => "render",
            Self::Terminal => "terminal",
            Self::Removed => "removed",
            Self::ApiOutage => "harness.api-outage",
            Self::LongHold => "harness.long-hold",
            Self::FleetUnreadable => "fleet.unreadable",
            Self::HugeDir => "vitals.huge-dir",
            Self::FabricState => "fabric.state",
            Self::Occupancy => "harness.occupancy",
            Self::Setup => "setup",
        }
    }

    /// The prefix of every key this family owns.
    #[must_use]
    pub fn prefix(self) -> String {
        format!("{}.", self.stem())
    }

    /// How the family's one producer speaks.
    #[inline]
    #[must_use]
    pub const fn mode(self) -> Mode {
        match self {
            Self::Upgrade
            | Self::Resume
            | Self::Ai
            | Self::ApiOutage
            | Self::LongHold
            | Self::FleetUnreadable
            | Self::HugeDir => Mode::Level,
            Self::Privacy
            | Self::Update
            | Self::UpgradeHost
            | Self::Supervisor
            | Self::Accounts
            | Self::Toolchain
            | Self::Disk
            | Self::Render
            | Self::Terminal
            | Self::Removed
            | Self::FabricState
            | Self::Occupancy
            | Self::Setup => Mode::Event,
        }
    }

    /// Every family, in digest order.
    pub const ALL: [Self; 20] = [
        Self::Privacy,
        Self::Update,
        Self::Upgrade,
        Self::UpgradeHost,
        Self::Resume,
        Self::Supervisor,
        Self::Accounts,
        Self::Ai,
        Self::Toolchain,
        Self::Disk,
        Self::Render,
        Self::Terminal,
        Self::Removed,
        Self::ApiOutage,
        Self::LongHold,
        Self::FleetUnreadable,
        Self::HugeDir,
        Self::FabricState,
        Self::Occupancy,
        Self::Setup,
    ];

    /// The message tag the family's entries carry.
    #[inline]
    #[must_use]
    pub const fn tag(self) -> Tag {
        match self {
            Self::Privacy => tags::PRIVACY,
            Self::Update => tags::UPDATE,
            Self::Upgrade
            | Self::UpgradeHost
            | Self::Resume
            | Self::Supervisor
            | Self::Accounts
            | Self::Ai
            | Self::ApiOutage
            | Self::LongHold
            | Self::Occupancy => tags::HARNESS,
            Self::Toolchain => tags::TOOLCHAIN,
            Self::Disk | Self::FleetUnreadable | Self::HugeDir | Self::Setup => tags::SYSTEM,
            Self::Render => tags::RENDER,
            Self::Terminal | Self::Removed => tags::CONFIG,
            Self::FabricState => tags::FABRIC,
        }
    }
}

/// How a family's one producer speaks ([`Family::mode`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Its whole answer, every look; what it stops naming ends.
    Level,
    /// Raised and cleared by event; never swept.
    Event,
}

/// Why an entry cannot be built as written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryFault {
    /// The `what` is not a glass title.
    What(&'static str),
    /// The reason or the action fails the vocabulary ([`LaterAsk::fault`]).
    Ask(&'static str),
    /// The navigation beside the action is a decline.
    Decline,
}

/// One thing that needs the owner's look, but not now.
#[derive(Clone, Debug)]
pub struct Entry {
    family: Family,
    subject: String,
    cause: String,
    what: String,
    why: Why,
    then: Then,
    nav: Option<Intent>,
    severity: Severity,
    resurface: Resurface,
    more: Vec<String>,
    log_line: Option<String>,
    at_once: bool,
}

impl Entry {
    /// An entry of `family`.
    #[must_use]
    pub fn new(
        family: Family,
        subject: &str,
        cause: &str,
        what: &str,
        why: Why,
        then: Then,
    ) -> Self {
        Self {
            family,
            subject: subject.to_string(),
            cause: cause.to_string(),
            what: what.to_string(),
            why,
            then,
            nav: None,
            severity: Severity::Info,
            resurface: Resurface::WAIT,
            more: Vec::new(),
            log_line: None,
            at_once: false,
        }
    }

    /// Warn ranks first in the digest and marks the batch a Problem.
    #[must_use]
    pub fn severity(mut self, severity: Severity) -> Self {
        self.severity = severity.min(Severity::Warn);
        self
    }

    /// How long a dismissal holds.
    #[must_use]
    pub const fn resurface(mut self, resurface: Resurface) -> Self {
        self.resurface = resurface;
        self
    }

    /// A navigation drawn beside the sentence (`Show tab 1`).
    #[must_use]
    pub fn show(mut self, nav: Intent) -> Self {
        self.nav = Some(nav);
        self
    }

    /// One line of volatile detail (holders, `--status`).
    #[must_use]
    pub fn more(mut self, line: impl AsRef<str>) -> Self {
        if self.more.len() < MORE_LINES {
            self.more.push(line.as_ref().to_string());
        }
        self
    }

    /// The one machine line `aterm.log` keeps when the entry is first raised.
    #[must_use]
    pub fn log_line(mut self, line: impl AsRef<str>) -> Self {
        self.log_line = Some(line.as_ref().to_string());
        self
    }

    /// Its words move only when its fact does, so each restatement is shown at once.
    #[must_use]
    pub const fn restated_at_once(mut self) -> Self {
        self.at_once = true;
        self
    }

    /// The `aterm.log` line, if the reporter gave one.
    #[inline]
    #[must_use]
    pub fn log_line_text(&self) -> Option<&str> {
        self.log_line.as_deref()
    }

    /// The family.
    #[inline]
    #[must_use]
    pub const fn family(&self) -> Family {
        self.family
    }

    /// The fact's identity: the key the digest files it under.
    #[must_use]
    pub fn key(&self) -> String {
        Fingerprint::new(self.family.stem(), &self.subject, &self.cause)
            .as_str()
            .to_string()
    }

    /// Build the checked [`crate::need::Need::Later`] message with glass-title `what` and capsules.
    /// # Errors
    /// [`EntryFault`] when the `what` is not a glass title, the reason or the action fails the
    /// vocabulary, or the navigation is a decline.
    pub fn message(&self) -> Result<Message, EntryFault> {
        if self.what.chars().count() > WHAT_CAP {
            return Err(EntryFault::What("a title over 48 characters"));
        }
        if let Some(fault) = glass_title_fault(&self.what) {
            return Err(EntryFault::What(fault));
        }
        if self.nav.as_ref().is_some_and(Intent::is_snooze) {
            return Err(EntryFault::Decline);
        }
        let mut ask = LaterAsk::person(
            self.why.clone(),
            self.then.clone(),
            Fingerprint::new(self.family.stem(), &self.subject, &self.cause),
            self.resurface,
        );
        if self.at_once {
            ask = ask.restated_at_once();
        }
        if let Some(fault) = ask.fault() {
            return Err(EntryFault::Ask(fault));
        }
        let mut msg = Message::new(self.family.tag(), self.severity, &self.what).later(ask);
        if let Some(nav) = &self.nav {
            msg = msg.action(nav.clone());
        }
        for line in &self.more {
            msg = msg.line(line);
        }
        Ok(msg)
    }

    /// The button the action carries, if one press does it.
    #[must_use]
    pub const fn mover(&self) -> Option<&Mover> {
        self.then.mover()
    }
}
