// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What the setup screen says (`docs/DESIGN-init-on-by-default-2026-10-25.md` §4.2, §4.3, §4.8),
//! decided from the rows ([`super::row_view`]) and the headline rule ([`super::headline`]), and
//! drawn by [`super::page`]: the hero's headline and sub-line, the five lines with the state
//! mark, status phrase and switch each, and the pinned card that holds everything a person does.
//! Pure: every rule here reads the page's state and nothing else.

use super::{
    Access, Headline, RowValue, RowView, Selection, Setting, SetupOperation, SetupStatus, Standing,
    Steps, Subject, SystemStateApp, ToolsWait,
};

/// The initial join observation interval; later retries remain automatic too.
pub(crate) use aterm_secrets::alab_join::JOIN_QUIET;

/// What the Secrets line says while the join asks by itself ([`JOIN_QUIET`]): calm, no card.
pub(crate) use aterm_secrets::alab_join::JOINING as JOINING_ALAB;

/// One line of the page's list (§4.2): presentation only, each governing its settings' bits.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Line {
    /// Caps Lock, scrolling, trackpad, Desktop, Widgets and Dock (the ALab profile).
    Mac,
    /// aterm's macOS grants.
    Permissions,
    /// The vault, and the Sync rows that ride it.
    Secrets,
    /// Rust in Spotlight and the GitHub repositories.
    Developer,
    /// The Messages and FaceTime sign-outs: last, and irreversible.
    Accounts,
}

impl Line {
    /// Every line, in page order.
    pub(crate) const ALL: [Self; 5] = [
        Self::Mac,
        Self::Permissions,
        Self::Secrets,
        Self::Developer,
        Self::Accounts,
    ];

    /// The line's name, which its switch is labelled with. The sign-outs' line names what it
    /// does, so turning it on never reads as turning Messages and FaceTime on.
    pub(crate) const fn title(self) -> &'static str {
        match self {
            Self::Mac => "Mac settings",
            Self::Permissions => "Permissions",
            Self::Secrets => "Secrets & identity",
            Self::Developer => "Developer",
            Self::Accounts => "Sign out of Messages & FaceTime",
        }
    }

    /// What the line covers, in a few calm words: all a line says until its rows are read
    /// (§4.3), so "Checking" is said once, by the headline.
    pub(crate) const fn scope(self) -> &'static str {
        match self {
            Self::Mac => "Caps Lock, scrolling, Desktop and Dock",
            Self::Permissions => "aterm's macOS permissions",
            Self::Secrets => "Your vault and what it syncs",
            Self::Developer => "Rust in Spotlight and GitHub repositories",
            Self::Accounts => "Last, and only on an ALab Mac",
        }
    }

    /// The word its keys carry (`system-state/line/<slug>/…`).
    pub(crate) const fn slug(self) -> &'static str {
        match self {
            Self::Mac => "mac",
            Self::Permissions => "permissions",
            Self::Secrets => "secrets",
            Self::Developer => "developer",
            Self::Accounts => "accounts",
        }
    }

    pub(crate) fn from_slug(slug: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|line| line.slug() == slug)
    }

    /// The plan bits its switch sets and clears.
    pub(crate) const fn settings(self) -> &'static [Setting] {
        match self {
            Self::Mac => &[
                Setting::CapsLock,
                Setting::NaturalScrolling,
                Setting::TrackpadScrolling,
                Setting::Desktop,
                Setting::Widgets,
                Setting::Dock,
            ],
            Self::Permissions => &[Setting::Permissions],
            Self::Secrets => &[Setting::Secrets],
            Self::Developer => &[Setting::Spotlight, Setting::Repositories],
            Self::Accounts => &[Setting::Messages, Setting::FaceTime],
        }
    }

    /// The line a row sits on: a grant on Permissions (Accessibility, which the sign-outs need,
    /// among them), a Sync row on Secrets & identity.
    pub(crate) const fn of(subject: Subject) -> Self {
        match subject {
            Subject::Access(_) => Self::Permissions,
            Subject::Sync(_) => Self::Secrets,
            Subject::Setting(setting) => match setting {
                Setting::CapsLock
                | Setting::NaturalScrolling
                | Setting::TrackpadScrolling
                | Setting::Desktop
                | Setting::Widgets
                | Setting::Dock => Self::Mac,
                Setting::Permissions => Self::Permissions,
                Setting::Secrets => Self::Secrets,
                Setting::Spotlight | Setting::Repositories => Self::Developer,
                Setting::Messages | Setting::FaceTime => Self::Accounts,
            },
        }
    }

    /// A line only an ALab Mac turns on by itself (§2.2): while the ALab answer is pending it
    /// reads "Checking this network…", never "Off".
    const fn profile(self) -> bool {
        matches!(self, Self::Mac | Self::Accounts)
    }
}

/// A state's mark (§4.4): a distinct shape for each, so colour is never the only signal.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Mark {
    /// A ring: not started, or not read yet.
    Pending,
    /// The spinner: its step runs now.
    Working,
    /// A ring with three dots: it waits for something that happens by itself.
    Waiting,
    /// A heavy ring with a dot: it needs the person (in the card).
    NeedsYou,
    /// A filled disc with a check.
    Done,
    /// A filled disc with `!` (in the card).
    Failed,
    /// An em dash: the person turned it off.
    Off,
}

impl Mark {
    pub(crate) const ALL: [Self; 7] = [
        Self::Pending,
        Self::Working,
        Self::Waiting,
        Self::NeedsYou,
        Self::Done,
        Self::Failed,
        Self::Off,
    ];

    /// The painter's value.
    pub(crate) const fn value(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Working => "working",
            Self::Waiting => "waiting",
            Self::NeedsYou => "needs-you",
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Off => "off",
        }
    }

    pub(crate) fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|mark| mark.value() == value)
    }

    /// The state in words, read in a line's label and drawn in every theme.
    /// A line still being read says [`CHECKING_WORD`] instead.
    pub(crate) const fn word(self) -> &'static str {
        match self {
            Self::Pending => "to do",
            Self::Working => "working",
            Self::Waiting => "waiting",
            Self::NeedsYou => "needs you",
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Off => "left as is",
        }
    }
}

/// The state word of a line whose rows, or whose plan, are still being read: never "to do",
/// which would claim work nobody has found yet.
pub(crate) const CHECKING_WORD: &str = "checking";

/// One line as the page draws it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LineView {
    pub(crate) line: Line,
    /// The plan sets any of its bits.
    pub(crate) on: bool,
    /// Its switch's value: `None` while the plan in force, or whether the line applies to this
    /// Mac at all, is still being read (§2.2), so a pending answer never reads as off.
    pub(crate) switch: Option<bool>,
    /// The switch can be pressed. Not where the line is out of this Mac's scope (the sign-outs
    /// on a Mac outside ALab), nor while a Restore runs.
    pub(crate) offered: bool,
    pub(crate) mark: Mark,
    /// The state in words, said once in the label and drawn in every theme: the mark's
    /// word, or [`CHECKING_WORD`] while the line is read.
    pub(crate) word: &'static str,
    /// Its live status phrase: what is true now, and where that differs from what aterm sets
    /// up, `now → wanted`.
    pub(crate) phrase: String,
    /// The phrase reads in the tertiary ink ("Left as is", a line's scope while it is read).
    pub(crate) quiet: bool,
    /// The rows it opens to: every row of the line that is not in the card.
    pub(crate) rows: Vec<RowView>,
    /// The Secrets line opens to "Join a different vault…" even where the card holds its one
    /// row (an approval code it waits on), so another vault is never out of reach.
    pub(crate) other_vault: bool,
    /// What a screen reader hears of the line: its name, state and phrase.
    pub(crate) label: String,
}

impl LineView {
    /// Whether the line opens: to rows, or the Secrets line to another vault; an Off line opens
    /// to nothing (§4.3).
    pub(crate) fn expandable(&self) -> bool {
        !self.rows.is_empty() || self.other_vault
    }
}

/// `name: now → wanted` for a row that differs, else its name.
fn change_words(row: &RowView) -> String {
    let name = row.subject.name();
    match &row.value {
        RowValue::Change { now, wanted } => format!("{name}: {now} → {wanted}"),
        _ => name.to_owned(),
    }
}

/// The same words for a screen reader: an arrow is read as `to`.
fn spoken(words: &str) -> String {
    words.replace(" → ", " to ").replace(" · ", ", ")
}

/// Whether the ALab answer is still to come (§2.2): the plan in force is aterm's own, which the
/// ALab signal widens, and the vault has not been read, or a join it started is under way.
pub(crate) fn scope_pending(status: &SetupStatus) -> bool {
    if !status.provisional {
        return false;
    }
    let secrets = status
        .observed
        .iter()
        .find(|row| row.subject == Subject::Setting(Setting::Secrets));
    secrets.is_none_or(|row| {
        row.standing == Standing::Waiting
            || matches!(
                row.now.as_deref(),
                Some(
                    aterm_types::macos_setup::VAULT_ASKING
                        | aterm_types::macos_setup::VAULT_ALLOWED
                        | "Awaiting code"
                )
            )
    })
}

/// Whether this is an ALab Mac as far as the page can tell, for the lockup's words (§4.5): its
/// vault is the ALab vault with its local cache ready, or the plan in force sets up the ALab profile
/// that only an ALab Mac's plan holds by itself.
pub(crate) fn alab_lockup(status: &SetupStatus, selection: Selection) -> bool {
    let in_alab_vault = status.plan.as_ref().is_some_and(|plan| {
        aterm_types::macos_setup::DEFAULT_VAULT_URLS.contains(&plan.vault_url.as_str())
    }) && status.observed.iter().any(|row| {
        row.subject == Subject::Setting(Setting::Secrets)
            && row.standing == Standing::InPlace
            && row.now.as_deref() == Some(Setting::Secrets.wanted())
    });
    let profile = [Line::Mac, Line::Accounts]
        .iter()
        .flat_map(|line| line.settings())
        .any(|setting| selection.contains(*setting));
    in_alab_vault || (profile && !status.provisional) || (profile && status.widened)
}

/// A card button's stable view identity, action and words. Different rows may invoke the same
/// action without sharing a key in the compiled page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CardButton {
    pub(crate) key: String,
    pub(crate) action: String,
    pub(crate) label: String,
    pub(crate) face: String,
}

/// One thing the card asks of the person (§4.8).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CardItem {
    /// Its key's last word (`system-state/card/<id>`).
    pub(crate) id: String,
    pub(crate) mark: Mark,
    /// What to do, said once.
    pub(crate) title: String,
    /// Where it stands in a walk of several (`Step 1 of 3`).
    pub(crate) step: Option<String>,
    /// Why, or how: the row's own reason.
    pub(crate) reason: Option<String>,
    /// Its one button, with the label VoiceOver hears and its visible face.
    pub(crate) button: Option<CardButton>,
    /// The row it stands for, which its line then leaves out.
    pub(crate) subject: Option<Subject>,
    /// It holds the phrase field ([`super::PHRASE_FIELD`]).
    pub(crate) phrase: bool,
    /// It holds the vault's address and password fields.
    pub(crate) vault: bool,
    /// The reason is a command, set in the mono face.
    pub(crate) code: bool,
    /// What the walk reaches after this page (`Next: Developer Tools, Accessibility`), said
    /// under its button.
    pub(crate) next: Option<String>,
}

impl CardItem {
    fn new(id: impl Into<String>, mark: Mark, title: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            mark,
            title: title.into(),
            step: None,
            reason: None,
            button: None,
            subject: None,
            phrase: false,
            vault: false,
            code: false,
            next: None,
        }
    }

    fn reason(mut self, reason: Option<String>) -> Self {
        self.reason = reason.filter(|reason| !reason.trim().is_empty());
        self
    }

    fn button(self, action: &str, label: impl Into<String>, face: impl Into<String>) -> Self {
        self.button_at(action, action, label, face)
    }

    fn button_at(
        mut self,
        key: &str,
        action: &str,
        label: impl Into<String>,
        face: impl Into<String>,
    ) -> Self {
        self.button = Some(CardButton {
            key: key.to_owned(),
            action: action.to_owned(),
            label: label.into(),
            face: face.into(),
        });
        self
    }

    const fn for_row(mut self, subject: Subject) -> Self {
        self.subject = Some(subject);
        self
    }
}

/// The Secrets item's Copy Command: the command that approves this Mac, for an admin Mac.
pub(crate) const COPY_APPROVE_COMMAND: &str = "system-state/card/secrets/copy-command";

/// What heads the worker's note of a join an admin Mac approves (`On an admin Mac: aterm secrets
/// approve <code>`).
const ON_AN_ADMIN_MAC: &str = "On an admin Mac: ";

/// The command an admin Mac runs to approve this Mac, from the vault row's note: `None` where
/// the note names none.
pub(crate) fn approve_command(note: &str) -> Option<&str> {
    let command = note.strip_prefix(ON_AN_ADMIN_MAC)?;
    // A join sent with a password adds how to retype it; the command ends before that.
    let command = command.split(", or ").next().unwrap_or(command).trim();
    (!command.is_empty()).then_some(command)
}

/// Open System Settings for a grant, as macOS names the place the person goes.
const OPEN_SETTINGS: &str = "Open System Settings";

/// What VoiceOver hears for the card's Go to tab: the tab it goes to, after the words it shows.
const GO_TO_TAB_LABEL: &str = "Go to tab: Command Line Tools";

/// What VoiceOver hears for the card's Go to installer: the window it goes to, after the words it
/// shows.
pub(super) const GO_TO_INSTALLER_LABEL: &str = "Go to installer: Apple's Command Line Tools";

/// THE CARD RULE (§4.8, pure): everything that needs the person, once each, in the order they
/// meet it — the permission walk's page, the Command Line Tools' password, the vault's Allow and phrase, a code to approve, a failed step with what aterm does
/// next.
pub(crate) fn card_items(app: &SystemStateApp, rows: &[RowView]) -> Vec<CardItem> {
    let status = &app.status;
    let mut items = Vec::new();
    if !app.plan_known || status.newer() {
        return items;
    }
    let needs = |row: &RowView| row.managed && row.standing == Some(Standing::NeedsYou);
    for row in rows.iter().filter(|row| needs(row)) {
        let subject = row.subject;
        let slug = super::row_slug(subject);
        if items.iter().any(|item| item.subject == Some(subject)) {
            continue;
        }
        let item = match subject {
            // The walk is one card item, at its current page (§4.8): the grants it reaches next
            // wait on their line, named under the page's button.
            Subject::Access(access)
                if status
                    .walk
                    .is_some_and(|walk| walk.access != access && walks_to(access)) =>
            {
                continue;
            }
            Subject::Access(access) => {
                let mut item =
                    CardItem::new(slug, Mark::NeedsYou, format!("Turn on {}", access.name()))
                        .reason(row.note.clone());
                if let Some(walk) = status.walk.filter(|walk| walk.access == access) {
                    item.step = Some(format!("Step {} of {}", walk.at, walk.of));
                    let next: Vec<&str> = rows
                        .iter()
                        .filter(|other| needs(other) && other.subject != subject)
                        .filter_map(|other| match other.subject {
                            Subject::Access(next) if walks_to(next) => Some(next.name()),
                            _ => None,
                        })
                        .collect();
                    item.next = (!next.is_empty()).then(|| format!("Next: {}", next.join(", ")));
                }
                item = item.for_row(subject);
                if row.open.is_some() {
                    let key = format!("system-state/row/{slug}/open");
                    item = item.button(
                        &key,
                        format!("{OPEN_SETTINGS} for {}", access.name()),
                        OPEN_SETTINGS,
                    );
                }
                item
            }
            Subject::Setting(Setting::Repositories) => match status.repositories_tools_wait() {
                Some(ToolsWait::Password) => CardItem::new(
                    slug,
                    Mark::NeedsYou,
                    super::TOOLS_PASSWORD_HEADLINE,
                )
                .button(super::GO_TO_TOOLS_TAB, GO_TO_TAB_LABEL, "Go to tab"),
                Some(ToolsWait::Recovered) => {
                    CardItem::new(slug, Mark::NeedsYou, super::TOOLS_RECOVERED_HEADLINE)
                        .reason(Some(super::TOOLS_RECOVERED_NOTE.to_owned()))
                        .button(super::GO_TO_TOOLS_TAB, GO_TO_TAB_LABEL, "Go to tab")
                }
                // Apple's window may sit behind others: its button finds it, and starts nothing.
                Some(ToolsWait::AppleWindow) => {
                    CardItem::new(slug, Mark::NeedsYou, super::APPLE_WINDOW_NOTE).button(
                        super::GO_TO_TOOLS_INSTALLER,
                        GO_TO_INSTALLER_LABEL,
                        "Go to installer",
                    )
                }
                // Only an administrator of this Mac installs them: their one step, said here.
                Some(ToolsWait::NotAdmin) => CardItem::new(
                    slug,
                    Mark::NeedsYou,
                    "Ask an administrator of this Mac to install Apple's Command Line Tools",
                ),
                Some(wait) if wait.calm() => continue,
                _ if row.install_tools => {
                    CardItem::new(slug, Mark::NeedsYou, "Install Apple's Command Line Tools")
                        .reason(row.note.clone())
                        .button(
                            super::INSTALL_TOOLS,
                            super::INSTALL_TOOLS_LABEL,
                            super::INSTALL_TOOLS_LABEL,
                        )
                }
                _ if row.copy_command => CardItem::new(
                    slug,
                    Mark::NeedsYou,
                    aterm_types::macos_setup::GITHUB_TOKEN_REFUSED,
                )
                .reason(Some(
                    aterm_types::macos_setup::GITHUB_TOKEN_COMMAND.to_owned(),
                ))
                .button(super::COPY_TOKEN_COMMAND, "Copy Command", "Copy Command"),
                _ => CardItem::new(
                    slug,
                    Mark::NeedsYou,
                    format!("Needs you: {}", subject.name()),
                )
                .reason(row.note.clone()),
            }
            .for_row(subject),
            Subject::Setting(Setting::Secrets) => {
                if app.phrase_asked() {
                    continue;
                } else if super::vault_waits_for_input(status) {
                    let mut item = CardItem::new(slug, Mark::NeedsYou, "Join your vault")
                        .reason(Some(
                            "Type its password, or leave it blank to join without one.".to_owned(),
                        ))
                        .for_row(subject);
                    item.vault = true;
                    item
                } else if let Some(command) = row.note.as_deref().and_then(approve_command) {
                    // An admin Mac approves this one: the code alone, in the mono face, and the
                    // command it runs there one press away.
                    let code = command
                        .strip_prefix("aterm secrets approve ")
                        .unwrap_or(command);
                    let mut item =
                        CardItem::new(slug, Mark::NeedsYou, "Approve this Mac from an admin Mac")
                            .reason(Some(code.to_owned()))
                            .button(COPY_APPROVE_COMMAND, "Copy Command", "Copy Command")
                            .for_row(subject);
                    item.code = true;
                    item
                } else {
                    CardItem::new(
                        slug,
                        Mark::NeedsYou,
                        format!("Needs you: {}", subject.name()),
                    )
                    .reason(row.note.clone())
                    .for_row(subject)
                }
            }
            _ => {
                let mut item = CardItem::new(
                    slug,
                    Mark::NeedsYou,
                    format!("Needs you: {}", subject.name()),
                )
                .reason(row.note.clone())
                .for_row(subject);
                if row.turn_on_sync {
                    item = item.button(super::TURN_ON_SYNC, "Turn On Sync", "Turn On Sync");
                }
                item
            }
        };
        items.push(item);
    }
    // A failed step: what failed, why, and what aterm does next (§4.8). A step running the
    // plan again can change, aterm runs again by itself; nothing here waits for a press.
    if let Some(row) = rows.iter().find(|row| {
        row.managed
            && row.subject == Subject::Sync(super::SyncKind::Accounts)
            && row.standing == Some(Standing::Waiting)
    }) {
        items.push(
            CardItem::new(
                "sync-accounts",
                Mark::Waiting,
                "AI account provisioning pending",
            )
            .reason(row.note.clone())
            .for_row(row.subject),
        );
    }
    let plan = app.reviewed_plan();
    let steps = Steps::of(status, rows, Some(&plan));
    for subject in &steps.failed {
        if items.iter().any(|item| item.subject == Some(*subject)) {
            continue;
        }
        let row = rows.iter().find(|row| row.subject == *subject);
        let why = row.and_then(|row| row.note.clone());
        let reason = if steps.rerun.contains(subject) {
            Some(match why.as_deref().map(str::trim) {
                Some(why) if !why.is_empty() => {
                    let stop = if why.ends_with('.') { "" } else { "." };
                    format!("{why}{stop} {TRIES_BY_ITSELF}")
                }
                _ => TRIES_BY_ITSELF.to_owned(),
            })
        } else {
            why
        };
        items.push(
            CardItem::new(
                super::row_slug(*subject),
                Mark::Failed,
                format!("{} failed", subject.name()),
            )
            .reason(reason)
            .for_row(*subject),
        );
    }
    items
}

/// What a failed step's card item says aterm does next, where running the plan again can change
/// it: the page starts that run by itself, a little later each time it fails again.
pub(crate) const TRIES_BY_ITSELF: &str = "aterm tries it again by itself.";

/// Whether the permission walk shows a page for `access`: a grant the person turns on in System
/// Settings, which the walk visits in turn. Debugger access, which setup's one early elevation
/// turns on, is never a walk page.
const fn walks_to(access: Access) -> bool {
    !matches!(access, Access::Debugger)
}

impl RowView {
    /// The row's value as it reads now, where it reads one.
    fn value_now(&self) -> Option<&str> {
        match &self.value {
            RowValue::Change { now, .. } => Some(now),
            RowValue::One(value) | RowValue::Muted(value) => Some(value),
            RowValue::LastKnown(_) => None,
        }
    }
}

/// THE LINE RULE (§4.2, §4.3, pure): one line's mark, phrase and rows from its rows' readings,
/// the plan and the card, which holds the rows that need the person or failed.
pub(crate) fn line_view(
    app: &SystemStateApp,
    line: Line,
    rows: &[RowView],
    card: &[CardItem],
) -> LineView {
    let status = &app.status;
    let selection = app.plan.selection;
    let bits = line.settings();
    let set = bits.iter().filter(|bit| selection.contains(**bit)).count();
    let on = app.plan_known && set > 0;
    let members: Vec<&RowView> = rows
        .iter()
        .filter(|row| Line::of(row.subject) == line)
        .collect();
    let in_card = |row: &RowView| card.iter().any(|item| item.subject == Some(row.subject));
    // A row the card asks about stays in its list with its mark and value, so a list never reads
    // as all done while one of its rows needs the person. The card holds its words and buttons.
    let shown: Vec<RowView> = members
        .iter()
        .map(|row| {
            let mut shown = (*row).clone();
            if in_card(row) {
                shown.note = None;
                shown.turn_on_sync = false;
                shown.install_tools = false;
                shown.copy_command = false;
            }
            shown
        })
        .collect();
    let plan = app.reviewed_plan();
    let steps = Steps::of(status, rows, Some(&plan));
    let state = line_state(app, line, &members, card, on, set < bits.len(), &steps);
    // The state is said once: where the phrase says it already ("Waiting for the vault…"), the
    // label is the line's name and its phrase.
    let said = spoken(&state.phrase);
    let label = if said.to_lowercase().contains(state.word) {
        format!("{}: {said}", line.title())
    } else {
        format!("{}, {}: {said}", line.title(), state.word)
    };
    // No on or off while the plan, or whether the line applies here, is still being read
    // (§2.2): a pending answer never reads as the person's choice.
    let undecided = !app.plan_known || (!on && line.profile() && scope_pending(status));
    LineView {
        line,
        on,
        switch: (!undecided).then_some(on),
        offered: app.plan_known && !app.restoring() && !out_of_scope(app, line, on),
        mark: state.mark,
        word: state.word,
        phrase: state.phrase,
        quiet: state.quiet,
        // An Off line shows no rows (§4.3); a newer aterm's rows read as they stand.
        rows: if on || status.newer() {
            shown
        } else {
            Vec::new()
        },
        other_vault: line == Line::Secrets
            && on
            && !status.newer()
            && !card.iter().any(|item| item.vault),
        label,
    }
}

/// Whether `line` is out of this Mac's scope: the sign-outs, which only an ALab Mac makes
/// (§2.2, decision 3), on a Mac outside ALab whose plan leaves them off. Its switch stays off
/// there, and its phrase says aterm leaves Messages and FaceTime alone.
pub(crate) fn out_of_scope(app: &SystemStateApp, line: Line, on: bool) -> bool {
    line == Line::Accounts
        && !on
        && app.plan_known
        && !app.status.alab
        && !app.status.newer()
        && !scope_pending(&app.status)
}

/// What the sign-outs' line says on a Mac outside ALab ([`out_of_scope`]).
pub(crate) const LEAVES_ACCOUNTS: &str = "aterm leaves Messages and FaceTime alone on this Mac";

/// What a line says while a newer aterm's setup is in force ([`SetupStatus::newer`]).
const AS_THEY_STAND: &str = "As this Mac reads now";

/// What an Off line says: the person, or aterm's plan, leaves it as it is.
pub(crate) const LEFT_AS_IS: &str = "Left as is";

/// What the Secrets line says on a Mac in no vault (no vault answered on its network): no secret
/// and no identity syncs, which is never "done".
pub(crate) const NO_VAULT_HERE: &str = "Not joined · no vault on this network";

/// A line's state, for [`line_view`].
struct LineState {
    mark: Mark,
    word: &'static str,
    phrase: String,
    quiet: bool,
}

impl LineState {
    fn new(mark: Mark, phrase: impl Into<String>) -> Self {
        Self {
            mark,
            word: mark.word(),
            phrase: phrase.into(),
            quiet: false,
        }
    }

    /// A line still being read: its ring and its scope, calm, and "checking" for its word.
    fn checking(line: Line) -> Self {
        Self {
            mark: Mark::Pending,
            word: CHECKING_WORD,
            phrase: line.scope().to_owned(),
            quiet: true,
        }
    }

    fn quiet(mut self) -> Self {
        self.quiet = true;
        self
    }

    fn word(mut self, word: &'static str) -> Self {
        self.word = word;
        self
    }
}

/// The mark, the words and whether they are quiet, for [`line_view`].
fn line_state(
    app: &SystemStateApp,
    line: Line,
    members: &[&RowView],
    card: &[CardItem],
    on: bool,
    partly: bool,
    steps: &Steps,
) -> LineState {
    let status = &app.status;
    if status.newer() {
        // A newer aterm's setup is in force: this aterm knows no plan, and its rows read as they
        // stand, which the headline says.
        return LineState::new(Mark::Off, AS_THEY_STAND).quiet();
    }
    if !app.plan_known {
        return LineState::checking(line);
    }
    if !on {
        if line.profile() && scope_pending(status) {
            return LineState::checking(line);
        }
        if out_of_scope(app, line, on) {
            return LineState::new(Mark::Off, LEAVES_ACCOUNTS).quiet();
        }
        return LineState::new(Mark::Off, LEFT_AS_IS).quiet();
    }
    let managed: Vec<&RowView> = members.iter().copied().filter(|row| row.managed).collect();
    let lead = if partly { "Partly on · " } else { "" };
    let with_lead = |state: LineState| LineState {
        phrase: format!("{lead}{}", state.phrase),
        ..state
    };
    // The card holds it: the line names it, once, and the card says what to do.
    let carded: Vec<&RowView> = managed
        .iter()
        .copied()
        .filter(|row| card.iter().any(|item| item.subject == Some(row.subject)))
        .collect();
    if let Some(first) = carded.first() {
        if card
            .iter()
            .any(|item| item.subject == Some(first.subject) && item.mark == Mark::Waiting)
        {
            return with_lead(LineState::new(Mark::Waiting, waiting_for(first)));
        }
        let failed = card
            .iter()
            .any(|item| item.subject == Some(first.subject) && item.mark == Mark::Failed);
        if failed {
            return with_lead(LineState::new(Mark::Failed, "Failed"));
        }
        let more = match carded.len() - 1 {
            0 => String::new(),
            n => format!(" and {n} more"),
        };
        return with_lead(LineState::new(
            Mark::NeedsYou,
            format!("Needs you: {}{more}", first.subject.name()),
        ));
    }
    let has_history = managed
        .iter()
        .any(|row| matches!(row.value, RowValue::LastKnown(_)));
    // Nothing of it read yet: its scope, calm, and the headline says Checking once.
    // Past readings instead keep their own explicitly historical group presentation below.
    if !has_history && !managed.is_empty() && managed.iter().all(|row| unread(row)) {
        return LineState::checking(line);
    }
    // The Secrets line's own words (§4.3): looking, joining, joined, or no vault here.
    if line == Line::Secrets
        && let Some(vault) = managed
            .iter()
            .find(|row| row.subject == Subject::Setting(Setting::Secrets))
    {
        if app.phrase_asked() {
            return with_lead(LineState::new(Mark::Working, JOINING_ALAB));
        }
        if unread(vault) && !has_history {
            let running = status.running.contains(&Setting::Secrets);
            let state = if running {
                LineState::new(Mark::Working, "Looking for your other Macs…")
            } else {
                LineState::new(Mark::Pending, "Looking for your other Macs…").word(CHECKING_WORD)
            };
            return with_lead(state);
        }
        // A Mac in no vault (none answered on its network) syncs nothing: calm, never done.
        if vault.settled() && vault.value_now() == Some("Not joined") {
            return with_lead(
                LineState::new(Mark::Off, NO_VAULT_HERE)
                    .word("not joined")
                    .quiet(),
            );
        }
    }
    let running: Vec<&RowView> = managed
        .iter()
        .copied()
        .filter(|row| status.running.contains(&row.subject.setting()) && !row.settled())
        .collect();
    if status.running()
        && let Some(first) = running.first()
    {
        return with_lead(LineState::new(Mark::Working, change_words(first)));
    }
    // A step aterm tries again by itself, or one a run or a Check could not confirm, waits: never
    // settled, never a failure (§3.2, §3.5).
    let waits_by_step = |row: &RowView| {
        steps.retried.contains(&row.subject) || steps.unconfirmed.contains(&row.subject)
    };
    let waiting: Vec<&RowView> = managed
        .iter()
        .copied()
        .filter(|row| {
            row.standing == Some(Standing::Waiting)
                || row.chip == Some(super::Chip::Waiting)
                || waits_by_step(row)
                || (row.standing == Some(Standing::Failed) && !row.settled())
        })
        .collect();
    let to_change: Vec<&RowView> = managed
        .iter()
        .copied()
        .filter(|row| row.to_change() && !waiting.iter().any(|w| w.subject == row.subject))
        .collect();
    if let Some(first) = to_change.first() {
        // The sign-outs go last: what they are now, and when they change.
        let what = if line == Line::Accounts {
            format!(
                "{} → signs out last",
                first.value_now().unwrap_or("Signed in")
            )
        } else {
            match to_change.len() - 1 {
                0 => change_words(first),
                n => format!("{} and {n} more", change_words(first)),
            }
        };
        return with_lead(LineState::new(Mark::Pending, what));
    }
    if let Some(first) = waiting.first() {
        let why = if waits_by_step(first) && steps.unconfirmed.contains(&first.subject) {
            "Waiting for the next check".to_owned()
        } else if waits_by_step(first) || first.standing == Some(Standing::Failed) {
            "Waiting for aterm's next try".to_owned()
        } else {
            waiting_for(first)
        };
        return with_lead(LineState::new(Mark::Waiting, why));
    }
    if has_history
        && !managed
            .iter()
            .any(|row| matches!(row.value, RowValue::Muted(_)))
    {
        let observing = managed
            .iter()
            .filter_map(|row| {
                status.observation_progress(row.subject, Some(app.plan.vault_url.as_str()))
            })
            .reduce(|a, b| a && b);
        if managed
            .iter()
            .all(|row| row.settled() || row.chip == Some(super::Chip::LastKnown))
        {
            let phase = match observing {
                Some(true) => " · waiting to read",
                Some(false) => " · checking",
                None => "",
            };
            // The check is past presentation only; progress still counts current readings.
            return with_lead(
                LineState::new(Mark::Done, format!("In place as last read{phase}"))
                    .word("as last read"),
            );
        }
        if observing.is_none() {
            return with_lead(
                LineState::new(
                    Mark::Pending,
                    "Some results as last read · current reading unavailable",
                )
                .word("as last read")
                .quiet(),
            );
        }
    }
    if managed.iter().any(|row| unread(row)) {
        return with_lead(LineState::checking(line));
    }
    // A reading that ended without an answer carries a note, so it is no longer `unread`,
    // but it has established no completed state either. Only settled rows earn the check.
    if managed
        .iter()
        .any(|row| !row.settled() && !row.changed_by_you)
    {
        return with_lead(LineState::checking(line));
    }
    let pending_grants = managed
        .iter()
        .filter(|row| matches!(&row.value, RowValue::Muted(word) if word == super::OFFERED_WORD))
        .count();
    if pending_grants > 0 {
        return with_lead(LineState::new(
            Mark::Waiting,
            format!("{pending_grants} macOS grants pending"),
        ));
    }
    if managed.is_empty() {
        // Nothing of it applies on this Mac (every row not needed).
        return with_lead(LineState::new(Mark::Done, "Nothing to set up here"));
    }
    with_lead(LineState::new(Mark::Done, now_words(line, &managed)))
}

/// A managed row nothing has read yet.
fn unread(row: &RowView) -> bool {
    row.standing.is_none() || (row.standing == Some(Standing::Unknown) && row.note.is_none())
}

/// What a waiting row waits for, from its note: `Waiting for the vault on this network`, never
/// a past failure under the waiting mark (§4.3).
pub(crate) fn waiting_for(row: &RowView) -> String {
    use aterm_types::macos_setup::VAULT_ALLOWED_NOTE;
    let Some(note) = row.note.as_deref() else {
        return format!("Waiting · {}", row.subject.name());
    };
    if let Some(rest) = note.strip_prefix("Waits for ") {
        return format!("Waiting for {rest}");
    }
    if note.starts_with("Waiting for ") {
        return note.to_owned();
    }
    if let Some(grant) = note
        .strip_prefix("Readable once ")
        .and_then(|rest| rest.strip_suffix(" is on"))
    {
        return format!("Waiting for {grant}");
    }
    use aterm_types::macos_setup::StageRow;
    if note.ends_with(StageRow::PARKED_TAIL) {
        return "Waiting for aterm's next check".to_owned();
    }
    if note.ends_with(StageRow::HELD_TAIL) {
        return "Waiting for Check again".to_owned();
    }
    if note.ends_with(StageRow::STILL_RUNNING_TAIL) {
        return "Still running after more than an hour".to_owned();
    }
    match note {
        "The ALab vault did not answer on this network" => {
            "Waiting for the vault on this network".to_owned()
        }
        "No vault on this network" => "Waiting for a vault on this network".to_owned(),
        VAULT_ALLOWED_NOTE => "Waiting for the vault to record this Mac".to_owned(),
        super::TOOLS_QUEUED_NOTE => "Waiting for the macOS permission step".to_owned(),
        super::INSTALLING_TOOLS_NOTE => "Waiting for Apple's Command Line Tools".to_owned(),
        _ => format!("Waiting · {}", row.subject.name()),
    }
}

/// What is true now of a line whose rows are all as wanted (§4.3): `Caps Lock is Control ·
/// Dock 3 apps`, `Joined · local cache ready`.
fn now_words(line: Line, managed: &[&RowView]) -> String {
    let value = |setting: Setting| {
        managed
            .iter()
            .find(|row| row.subject == Subject::Setting(setting))
            .and_then(|row| row.value_now())
            .map(|value| aterm_types::macos_setup::live_until_login(value).unwrap_or(value))
    };
    let parts: Vec<String> = match line {
        Line::Mac => [
            value(Setting::CapsLock).map(|v| format!("Caps Lock is {v}")),
            value(Setting::Dock).map(|v| format!("Dock {v}")),
        ]
        .into_iter()
        .flatten()
        .collect(),
        // A grant on offer is nobody's yet: the line says so rather than Granted.
        Line::Permissions => vec![
            if managed.iter().any(
                |row| matches!(&row.value, RowValue::Muted(word) if word == super::OFFERED_WORD),
            ) {
                super::OFFERED_WORD
            } else {
                "Granted"
            }
            .to_owned(),
        ],
        Line::Secrets => vec![match value(Setting::Secrets) {
            Some(other) => format!("Joined · {}", other.to_lowercase()),
            None => "No local reading".to_owned(),
        }],
        Line::Developer => [
            value(Setting::Spotlight).map(|_| "Rust docs out of Spotlight".to_owned()),
            value(Setting::Repositories).map(|v| {
                if v == "All" {
                    "All repositories".to_owned()
                } else {
                    format!("{v} repositories")
                }
            }),
        ]
        .into_iter()
        .flatten()
        .collect(),
        Line::Accounts => vec!["Signed out of Messages and FaceTime".to_owned()],
    };
    if parts.is_empty() {
        "As wanted".to_owned()
    } else {
        parts.join(" · ")
    }
}

/// The hero (§4.3): the headline, which changes only with the phase, and the sub-line under it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Hero {
    pub(crate) headline: String,
    /// What the live region says (§4.7): what the card asks, or the phase in words of its own,
    /// never the heading again and never a count that moves.
    pub(crate) phase: String,
    pub(crate) sub: String,
    /// Stop shows beside the sub-line (a run goes).
    pub(crate) stop: bool,
    /// Set up now shows in the footer: a quiet recovery while a hold stands (a Stop, an older
    /// aterm's Cancel), the only time changes to make wait for a press (revision 4).
    pub(crate) start: bool,
    /// The lockup's progress: the managed rows done, of those read. Both 0 until something was
    /// read, and then no progress is claimed.
    pub(crate) done: usize,
    pub(crate) total: usize,
}

/// `11 of 16 done`.
pub(crate) fn progress_words(done: usize, total: usize) -> String {
    format!("{done} of {total} done")
}

/// `1 min 12 s`, `48 s`.
pub(crate) fn duration_words(took: std::time::Duration) -> String {
    let secs = took.as_secs();
    match (secs / 60, secs % 60) {
        (0, s) => format!("{s} s"),
        (m, 0) => format!("{m} min"),
        (m, s) => format!("{m} min {s} s"),
    }
}

/// `1 thing`, `2 things` (numerals, as every count on the page).
fn things(n: usize) -> String {
    if n == 1 {
        "1 thing needs you".to_owned()
    } else {
        format!("{n} things need you")
    }
}

/// THE PROGRESS RULE (§4.5, pure): how many managed rows are done, of those aterm manages, once
/// anything was read. Done means done: a row in the card, a step that failed, waits for aterm's
/// next try or could not be confirmed is not, so full progress is every line done.
pub(crate) fn progress(
    app: &SystemStateApp,
    rows: &[RowView],
    card: &[CardItem],
) -> (usize, usize) {
    let status = &app.status;
    if !app.plan_known || status.observed.is_empty() || status.newer() {
        return (0, 0);
    }
    let plan = app.reviewed_plan();
    let steps = Steps::of(status, rows, Some(&plan));
    let total = rows.iter().filter(|row| row.managed).count();
    let done = rows
        .iter()
        .filter(|row| {
            row.managed
                && (row.settled() || row.changed_by_you)
                && row.chip != Some(super::Chip::Waiting)
                && !matches!(&row.value, RowValue::Muted(word) if word == super::OFFERED_WORD)
                && !card.iter().any(|item| item.subject == Some(row.subject))
                && !steps.failed.contains(&row.subject)
                && !steps.retried.contains(&row.subject)
                && !steps.unconfirmed.contains(&row.subject)
        })
        .count();
    (done, total)
}

/// THE HERO RULE (§4.3, pure): the phase headline, from the headline rule's own reading
/// ([`super::headline`]), the sub-line, where the counts live, and the live region's words.
pub(crate) fn hero(app: &SystemStateApp, rows: &[RowView], card: &[CardItem]) -> Hero {
    let mut hero = hero_words(app, rows, card);
    hero.phase = phase_words(app, &hero, card);
    if rows
        .iter()
        .any(|row| matches!(row.value, RowValue::LastKnown(_)))
    {
        hero.sub = "Showing last useful results".into();
    }
    hero
}

/// The live region's words (§4.7): what the one card item asks, or the phase in its own short
/// words, which never repeat the heading beside it and carry no count.
fn phase_words(app: &SystemStateApp, hero: &Hero, card: &[CardItem]) -> String {
    let status = &app.status;
    let needs: Vec<&CardItem> = card
        .iter()
        .filter(|item| item.mark == Mark::NeedsYou)
        .collect();
    match needs.as_slice() {
        [item] => return item.title.clone(),
        [_, _, ..] => return "Setup needs you".to_owned(),
        [] => {}
    }
    let failed: Vec<String> = card
        .iter()
        .filter(|item| item.mark == Mark::Failed)
        .filter_map(|item| item.subject.map(|subject| subject.name().to_owned()))
        .collect();
    if !failed.is_empty() {
        return format!("{} failed", crate::toolchain_words::and_list(&failed));
    }
    let headline = hero.headline.as_str();
    let words = match headline {
        super::CHECKING => "Checking",
        SETTING_UP => "Setup running",
        READY => "Setup finished",
        "Selected setup complete" => "Selected setup finished",
        "Setup incomplete" => "Setup incomplete",
        TRIES_AGAIN => "Waiting to try again",
        STOPPED => "Setup stopped",
        super::EVERYTHING_LEFT_AS_IS => "Nothing to set up",
        super::NEWER_HEADLINE => "Set up by a newer aterm",
        super::STARTING_HEADLINE => "Starting",
        _ if status.busy && status.operation == Some(SetupOperation::Restore) => {
            if status.queued {
                "Waiting for another setup"
            } else {
                "Restore running"
            }
        }
        _ if status.busy && status.queued => "Waiting for another setup",
        _ if headline.starts_with(super::RESTORE_FINISHED) => "Restore failed",
        _ => "Setup waiting",
    };
    words.to_owned()
}

fn hero_words(app: &SystemStateApp, rows: &[RowView], card: &[CardItem]) -> Hero {
    let status = &app.status;
    let detail: Headline = app.headline();
    let (done, total) = progress(app, rows, card);
    let counts = if total > 0 {
        progress_words(done, total)
    } else {
        String::new()
    };
    let with_counts = |words: &str| {
        if counts.is_empty() {
            words.to_owned()
        } else {
            format!("{words} · {counts}")
        }
    };
    let held = detail.start && !status.busy && app.held();
    let mut hero = Hero {
        phase: String::new(),
        headline: detail.text.clone(),
        sub: counts.clone(),
        stop: status.running(),
        start: held,
        done,
        total,
    };
    if !app.plan_known {
        hero.sub = String::new();
        if status.busy && status.operation.is_none() && !status.queued {
            hero.headline = SETTING_UP.to_owned();
            hero.sub = STARTING.to_owned();
        }
        return hero;
    }
    let needs = card
        .iter()
        .filter(|item| item.mark == Mark::NeedsYou)
        .count();
    let failed = card.iter().filter(|item| item.mark == Mark::Failed).count();
    if needs > 0 {
        hero.headline = things(needs);
        if status.running() {
            hero.sub = with_counts("Everything else carries on");
        }
        return hero;
    }
    match status.operation {
        Some(SetupOperation::Check) if status.busy && !status.queued => {
            hero.headline = super::CHECKING.to_owned();
            hero.sub = String::new();
            return hero;
        }
        Some(SetupOperation::Restore) if status.busy => return hero,
        _ => {}
    }
    if status.busy && status.queued {
        return hero;
    }
    if status.running() || (status.busy && status.operation.is_none()) {
        hero.headline = SETTING_UP.to_owned();
        let current = rows.iter().find(|row| {
            row.managed && status.running.contains(&row.subject.setting()) && !row.settled()
        });
        hero.sub = match current {
            _ if done == 0 && status.running.is_empty() => STARTING.to_owned(),
            Some(row) => with_counts(row.subject.name()),
            None => counts,
        };
        return hero;
    }
    if status.joins_quietly() {
        // A calm wait, as one aterm retries by itself (§4.3): the Secrets line says it. "Joining
        // ALab…" names the step this Mac is on (it asks the members, and its subscriber asks
        // again), never what lets it in: that depends on where it is, and the Secrets row's own
        // note says it (`aterm_secrets::alab_join::asking_note`: automatic on the ALab LAN only),
        // so the hero promises nothing off the LAN.
        hero.headline = SETTING_UP.to_owned();
        hero.sub = with_counts(JOINING_ALAB);
        return hero;
    }
    if failed > 0 {
        // The phase and its count; the card says what failed and what aterm does next (§4.3).
        hero.headline = if failed == 1 {
            "1 step failed".to_owned()
        } else {
            format!("{failed} steps failed")
        };
        return hero;
    }
    // Changes to make that nothing holds start by themselves at the page's next tick (revision
    // 4): the page says it sets up, never the count of a press it waits for.
    if app.starts_by_itself() {
        hero.headline = SETTING_UP.to_owned();
        hero.sub = STARTING.to_owned();
        return hero;
    }
    if detail.set_up && done == total && total > 0 {
        let disabled = Setting::ALL
            .into_iter()
            .filter(|setting| !app.reviewed_plan().selection.contains(*setting))
            .count();
        hero.headline = if disabled == 0 {
            READY.to_owned()
        } else {
            "Selected setup complete".to_owned()
        };
        // After a Check that failed as a whole, the rows are what aterm read before.
        let stale = detail.text.ends_with(super::AS_LAST_READ);
        hero.sub = match status.ready_in {
            Some(took) if !stale && total > 0 => {
                format!("{done} of {total} · ready in {}", duration_words(took))
            }
            _ => counts.clone(),
        };
        if disabled > 0 {
            hero.sub = format!("{} · {disabled} disabled", hero.sub);
        }
        if stale {
            hero.sub = format!("{} · {}", hero.sub, super::AS_LAST_READ);
        }
        return hero;
    }
    if detail.set_up {
        hero.headline = "Setup incomplete".to_owned();
        hero.sub = counts.clone();
        return hero;
    }
    if held {
        // A Stop, or an older aterm's Cancel, holds the changes to make: the footer's Set up now
        // goes on with them.
        hero.headline = STOPPED.to_owned();
        hero.sub = with_counts(&detail.text);
        return hero;
    }
    // A step aterm tries again by itself is no failure (§3.5): setup will try again, and the
    // sub-line names it. A step that waits on something else reads as setup going on.
    let plan = app.reviewed_plan();
    let steps = Steps::of(status, rows, Some(&plan));
    if !steps.retried.is_empty() || status.setup_retried(Some(&plan)) || app.retry_waits() {
        hero.headline = TRIES_AGAIN.to_owned();
        let names: Vec<&str> = steps.retried.iter().map(|subject| subject.name()).collect();
        hero.sub = if names.is_empty() {
            counts
        } else {
            with_counts(&names.join(", "))
        };
        return hero;
    }
    if !steps.waiting(rows).is_empty() && !detail.start {
        hero.headline = SETTING_UP.to_owned();
        hero.sub = with_counts(&detail.text);
        return hero;
    }
    hero
}

/// The headline while setup runs.
pub(crate) const SETTING_UP: &str = "Setting up your Mac";

/// The headline once every line is done.
pub(crate) const READY: &str = "Your Mac is ready";

/// The headline while a step, or a run, waits for aterm to try it again by itself.
pub(crate) const TRIES_AGAIN: &str = "Setup will try again";

/// The headline while a Stop (or an older aterm's Cancel) holds changes to make.
pub(crate) const STOPPED: &str = "Setup is stopped";

/// The sub-line before the first step reports.
pub(crate) const STARTING: &str = "Starting…";
