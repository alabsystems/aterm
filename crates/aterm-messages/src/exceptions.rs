// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE HOST-FACING ANATOMY OF A DIGEST ENTRY (owner rule, 2026-09-28: *"a
//! single batched 'exceptions' [surface] that I can dismiss and they go to some
//! log for me to look at later"*).
//!
//! A reporter never spells the class ceremony ([`crate::need`]) at every call
//! site. It states one [`Entry`]: which family speaks, the fact's identity, the
//! glass-title `what`, WHY the owner is needed ([`Why`]), the ONE action
//! ([`Then`], optionally with the button that does it), and how long a
//! dismissal holds. [`Entry::message`] turns it into the checked
//! [`Message`] (a [`crate::need::Need::Later`]) that
//! [`crate::MessageCenter::sync_digest`] keeps equal to the reporter's whole
//! answer for the family. Everything a person reads — the title, the reason,
//! the action, the capsules — is bounded here once, so a builder that runs past
//! a cap or leans on "waiting" is a red roster test, not a long line in the
//! menu bar.
//!
//! An entry is NEVER a band row: the digest is a store beside the live set
//! ([`crate::digest`]), so nothing that lays out the band can seat one.

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
    /// The updater (`update.review.<kind>`, a dev build's writes). Its stem is
    /// NOT the health record's (`update.health.<kind>`): an exception and a
    /// record never share a key, so a record posted under the fact's own key
    /// can neither stand in for the entry nor be mistaken for its dismissal.
    Update,
    /// The live agent upgrade's ROWS (`harness.upgrade.<kind>[.<tab>]`): a
    /// level-triggered pass over its records, every look.
    Upgrade,
    /// The upgrade's HOST (`harness.upgrade-host.<kind>`): a fact the host
    /// observes by event (the state folder it cannot use). It has a prefix of
    /// its own because the rows' pass sweeps everything under
    /// `harness.upgrade.` and would end it two minutes after it was raised.
    UpgradeHost,
    /// The resume loop's exceptions (`harness.resume.<kind>`).
    Resume,
    /// The supervisor that keeps failing (`harness.supervisor.<kind>`).
    Supervisor,
    /// The Claude Code accounts as a whole (`harness.accounts.<kind>`): a fact
    /// of the machine's roster, not of a tab (an account signed out, a roster
    /// that is refused). Raised and cleared by the host's look, by event.
    Accounts,
    /// The toolchain lane.
    Toolchain,
    /// The disk reclaim pass.
    Disk,
    /// The developer root (`devroot.<kind>`, `atpkg::devroot`): a measurement that
    /// Spotlight indexes `~/Developer`. Raised and cleared by the host's look, by
    /// event: an unmeasured look changes nothing.
    DevRoot,
    /// The render stream's repair that gave up (`render.<tab>.unrepaired`): a
    /// tab the repaint could not put right, so its screen may be drawn a row
    /// off. Raised at the give-up and cleared when the epoch heals, by event.
    Render,
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
            Self::Toolchain => "toolchain",
            Self::Disk => "disk",
            Self::DevRoot => "devroot",
            Self::Render => "render",
        }
    }

    /// The prefix of every key this family owns.
    #[must_use]
    pub fn prefix(self) -> String {
        format!("{}.", self.stem())
    }

    /// HOW the family's producer speaks — the family has exactly one, and one
    /// mode. A [`Mode::Level`] family states its whole answer every look
    /// (`sync_digest`: what it stops naming ends), so nothing else may raise an
    /// entry under its prefix; a [`Mode::Event`] family raises and clears by
    /// event (`post`, `clear_digest`) and is never swept. The host refuses the
    /// wrong call (`App::except`, `App::except_replace`).
    #[must_use]
    pub const fn mode(self) -> Mode {
        match self {
            Self::Upgrade | Self::Resume => Mode::Level,
            Self::Privacy
            | Self::Update
            | Self::UpgradeHost
            | Self::Supervisor
            | Self::Accounts
            | Self::Toolchain
            | Self::Disk
            | Self::DevRoot
            | Self::Render => Mode::Event,
        }
    }

    /// Every family, in digest order.
    pub const ALL: [Self; 11] = [
        Self::Privacy,
        Self::Update,
        Self::Upgrade,
        Self::UpgradeHost,
        Self::Resume,
        Self::Supervisor,
        Self::Accounts,
        Self::Toolchain,
        Self::Disk,
        Self::DevRoot,
        Self::Render,
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
            | Self::Accounts => tags::HARNESS,
            Self::Toolchain => tags::TOOLCHAIN,
            Self::Disk | Self::DevRoot => tags::SYSTEM,
            Self::Render => tags::RENDER,
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
}

impl Entry {
    /// An entry of `family`. The fact's identity is `(family, subject, cause)`:
    /// `subject` names the place when the action is per place (a tab), or is
    /// empty when one action serves every place (one entry naming them all);
    /// `cause` is a short lowercase word (`own-work`, `needed`) and is the ONLY
    /// thing that makes a fact NEW after a dismissal — never a version, a pid,
    /// an age or a tab set. `what` is the glass-title subject; `why` says why
    /// the owner and not the machine; `then` is the one action.
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

    /// The ONE machine line `aterm.log` keeps when the entry is first raised
    /// (`upgrade need=own-work tab=s-3e79 move=2.1.283->2.1.284 ...`): what a
    /// developer greps, never shown to the owner.
    #[must_use]
    pub fn log_line(mut self, line: impl AsRef<str>) -> Self {
        self.log_line = Some(line.as_ref().to_string());
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
        let ask = LaterAsk::person(
            self.why.clone(),
            self.then.clone(),
            Fingerprint::new(self.family.stem(), &self.subject, &self.cause),
            self.resurface,
        );
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
