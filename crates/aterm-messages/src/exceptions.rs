// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The host-facing anatomy of a digest entry (an exception the owner looks at later).
//!
//! A reporter states one [`Entry`] instead of spelling the class ceremony
//! ([`crate::need`]): which family speaks, the fact's identity, the glass-title `what`,
//! why the owner is needed ([`Why`]), the one action ([`Then`], optionally with the
//! button that does it), and how long a dismissal holds. [`Entry::message`] turns it
//! into the checked [`Message`] (a [`crate::need::Need::Later`]) that
//! [`crate::MessageCenter::sync_digest`] keeps equal to the family's whole answer.
//! Everything a person reads is bounded here once, so a builder that runs past a cap or
//! leans on "waiting" fails a roster test, not a long line in the menu bar. An entry is
//! never a band row: the digest is a store beside the live set ([`crate::digest`]).

use crate::model::{Intent, Message, Severity, Tag, tags};
use crate::need::{Fingerprint, LaterAsk, Mover, Resurface, Then, Why};
use crate::text::glass_title_fault;

/// Chars of an entry's `what`: a glass title (six words, no clause).
pub const WHAT_CAP: usize = 48;
/// Lines of volatile detail an entry may carry behind `Details ›` (holders,
/// ages, the `--status` line): never part of the fact's identity or its
/// one-line form.
pub const MORE_LINES: usize = 3;

/// Which reporter speaks. A family owns a key prefix, so one family's pass
/// ([`crate::MessageCenter::sync_digest`]) can end only its own entries.
/// Declared in digest order after severity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Family {
    /// Full Disk Access and the OS's questions about aterm.
    Privacy,
    /// The updater (`update.review.<kind>`, a dev build's writes). Its stem is not the
    /// health record's (`update.health.<kind>`): an exception and a record never share
    /// a key, so a record posted under the fact's own key can neither stand in for the
    /// entry nor be mistaken for its dismissal.
    Update,
    /// The live agent upgrade's rows (`harness.upgrade.<kind>[.<tab>]`): a
    /// level-triggered pass over its records, every look.
    Upgrade,
    /// The upgrade's host (`harness.upgrade-host.<kind>`): a fact the host observes by
    /// event (the state folder it cannot use). Its prefix is its own because the rows'
    /// pass sweeps everything under `harness.upgrade.` and would end it
    /// [`crate::need::RESOLVE_AFTER`] after it was raised.
    UpgradeHost,
    /// The resume loop's exceptions (`harness.resume.<kind>`).
    Resume,
    /// The supervisor that keeps failing (`harness.supervisor.<kind>`).
    Supervisor,
    /// The Claude Code accounts as a whole (`harness.accounts.<kind>`): a fact
    /// of the machine's roster, not of a tab (an account signed out, a roster
    /// that is refused). Raised and cleared by the host's look, by event.
    Accounts,
    /// The AI CLIs aterm manages (`ai.<cli>.<state>`,
    /// docs/DESIGN-ai-connections-2026-10-16.md §7.3): a wanted CLI that needs
    /// a person (signed out, not installed, a login due). A level-triggered
    /// pass: the window's look states its whole answer every time.
    Ai,
    /// The toolchain lane.
    Toolchain,
    /// The disk reclaim pass.
    Disk,
    /// The render stream's repair that gave up (`render.<tab>.unrepaired`): a
    /// tab the repaint could not put right, so its screen may be drawn a row
    /// off. Raised at the give-up and cleared when the epoch heals, by event.
    Render,
    /// What a program in a tab asked the terminal for and the owner's own
    /// configuration refused (`terminal.<cause>`): a palette set under an
    /// explicit `allow_palette_reconfigure = false`. One line for every tab, since the
    /// setting is one; raised by event at a session's first refusal.
    Terminal,
    /// Settings a removed feature left behind, where another feature now does
    /// its work (`removed.<feature>.<cause>`): raised by a config load that
    /// finds them and cleared by one that does not, by event. Its own prefix,
    /// apart from the config band's (`config.`), which a clean reload resolves.
    Removed,
    /// The tabs whose agent cannot reach its API (`harness.api-outage.<cause>`):
    /// one fact for every tab the network keeps from it, never one per tab
    /// (`docs/DESIGN-owner-surfaces-2026-10-16.md` §2.2). A level family: the
    /// need publication states its whole answer whenever a tab's verdict
    /// enters or leaves the wait, so the outage ending sweeps it.
    ApiOutage,
    /// The tabs the person took over and has held for half an hour
    /// (`harness.long-hold.<tab>.taken-over`): one entry per tab, since the
    /// one act — Hand Back — is per tab. A level family: the window states its
    /// whole answer whenever a person hold moves and at every look, so a Hand
    /// Back sweeps its entry (`docs/DESIGN-owner-surfaces-2026-10-16.md` §4).
    LongHold,
    /// The fleet this Mac cannot read (`fleet.unreadable.<cause>`): every
    /// peer refused at once with no route, which on a Mac is local-network
    /// privacy until the person allows aterm
    /// (`docs/DESIGN-owner-surfaces-2026-10-16.md` §3, S10). A level family:
    /// the fleet glance states its whole answer at every read, so a read that
    /// reaches a peer again sweeps it.
    FleetUnreadable,
}

impl Family {
    /// The key stem: `Fingerprint::new(stem, subject, cause)` is an entry's
    /// key, so `stem` + `.` is the prefix a family pass sweeps.
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
        }
    }

    /// The prefix of every key this family owns.
    #[must_use]
    pub fn prefix(self) -> String {
        format!("{}.", self.stem())
    }

    /// How the family's one producer speaks. A [`Mode::Level`] family states its whole
    /// answer every look (`sync_digest`: what it stops naming ends), so nothing else may
    /// raise an entry under its prefix; a [`Mode::Event`] family raises and clears by
    /// event (`post`, `clear_digest`) and is never swept. The host refuses the wrong
    /// call (`App::except`, `App::except_replace`).
    #[must_use]
    pub const fn mode(self) -> Mode {
        match self {
            Self::Upgrade
            | Self::Resume
            | Self::Ai
            | Self::ApiOutage
            | Self::LongHold
            | Self::FleetUnreadable => Mode::Level,
            Self::Privacy
            | Self::Update
            | Self::UpgradeHost
            | Self::Supervisor
            | Self::Accounts
            | Self::Toolchain
            | Self::Disk
            | Self::Render
            | Self::Terminal
            | Self::Removed => Mode::Event,
        }
    }

    /// Every family, in digest order.
    pub const ALL: [Self; 16] = [
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
    ];

    /// The message tag the family's entries carry.
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
            | Self::LongHold => tags::HARNESS,
            Self::Toolchain => tags::TOOLCHAIN,
            Self::Disk | Self::FleetUnreadable => tags::SYSTEM,
            Self::Render => tags::RENDER,
            Self::Terminal | Self::Removed => tags::CONFIG,
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

/// Why an entry cannot be built as written. The host logs it once and the
/// fact is recorded instead: an owner is never shown words that fail the rule.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryFault {
    /// The `what` is not a glass title.
    What(&'static str),
    /// The reason or the action fails the vocabulary ([`LaterAsk::fault`]).
    Ask(&'static str),
    /// The navigation beside the action is a decline (`Not today`, `Skip
    /// version`, `Not now`, `Details`): a decline is never an action.
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
    /// An entry of `family`. The fact's identity is `(family, subject, cause)`:
    /// `subject` names the place when the action is per place (a tab), or is empty when
    /// one action serves every place; `cause` is a short lowercase word (`own-work`,
    /// `needed`) and is the only thing that makes a fact new after a dismissal, never a
    /// version, a pid, an age or a tab set. `what` is the glass-title subject; `why` says
    /// why the owner and not the machine; `then` is the one action.
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

    /// How long a dismissal holds ([`Resurface::WAIT`] for a wait somebody
    /// could have ended, [`Resurface::CHOICE`] for the owner's own choice).
    #[must_use]
    pub fn resurface(mut self, resurface: Resurface) -> Self {
        self.resurface = resurface;
        self
    }

    /// A navigation drawn beside the sentence (`Show tab 1`): it takes the
    /// owner to where the action is done. It is never the action itself, and a
    /// decline is refused ([`EntryFault::Decline`]).
    #[must_use]
    pub fn show(mut self, nav: Intent) -> Self {
        self.nav = Some(nav);
        self
    }

    /// One line of volatile detail (holders, `--status`): behind `Details ›`,
    /// at most [`MORE_LINES`], never in the entry's identity or its one-line
    /// form.
    #[must_use]
    pub fn more(mut self, line: impl AsRef<str>) -> Self {
        if self.more.len() < MORE_LINES {
            self.more.push(line.as_ref().to_string());
        }
        self
    }

    /// The one machine line `aterm.log` keeps when the entry is first raised
    /// (`upgrade need=own-work tab=s-3e79 move=2.1.283->2.1.284 ...`): what a developer
    /// greps, never shown to the owner.
    #[must_use]
    pub fn log_line(mut self, line: impl AsRef<str>) -> Self {
        self.log_line = Some(line.as_ref().to_string());
        self
    }

    /// Its words move only when its fact does (a count of the tabs it holds,
    /// stated as they join and leave), so each restatement is shown at once
    /// ([`LaterAsk::restated_at_once`]).
    #[must_use]
    pub fn restated_at_once(mut self) -> Self {
        self.at_once = true;
        self
    }

    /// The `aterm.log` line, if the reporter gave one.
    #[must_use]
    pub fn log_line_text(&self) -> Option<&str> {
        self.log_line.as_deref()
    }

    /// The family.
    #[must_use]
    pub fn family(&self) -> Family {
        self.family
    }

    /// The fact's identity: the key the digest files it under.
    #[must_use]
    pub fn key(&self) -> String {
        Fingerprint::new(self.family.stem(), &self.subject, &self.cause)
            .as_str()
            .to_string()
    }

    /// The checked message: a [`crate::need::Need::Later`], glass-title
    /// `what`, the reason and the action leading its detail, the action's
    /// button (if any) first among the capsules and the navigation second.
    ///
    /// # Errors
    ///
    /// [`EntryFault`] when the `what` is not a glass title, the reason or the
    /// action fails the vocabulary, or the navigation is a decline.
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
    pub fn mover(&self) -> Option<&Mover> {
        self.then.mover()
    }
}
