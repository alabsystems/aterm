// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The toolchain lane's words.

use std::time::Duration;

use aterm_messages::text::shape_detail;
use aterm_messages::{
    Amount, DETAIL_LINE_CAP, Glyph, Intent, Load, Message, Meter, STALE_ANNOUNCE, STALE_TAILED,
    Severity, TITLE_CAP, Tag, Unit, tags,
};
use atpkg::progress::{PROGRESS_VERSION, Phase, sanitize_for_tty};

/// The supersede key every report about one pass carries: announcement, live meter, and the installed / ended / failed records.
pub(crate) const KEY_PASS: &str = "toolchain.pass";
/// The managed-current record's key.
pub(crate) const KEY_MANAGED: &str = "toolchain.managed";
/// A routine pass planned at least this many bytes is very heavy system use.
pub(crate) const HEAVY_PASS_BYTES: u64 = 250_000_000;
/// The prefix of a machine-settings item's key: `toolchain.machine.` plus the item's first word (`universal-control`).
pub(crate) const KEY_MACHINE_PREFIX: &str = "toolchain.machine.";

/// Cap on a sanitized failure reason inside a row.
const ERROR_CAP: usize = 60;
/// Cap on a program name inside a row (the store's own names are short).
const NAME_CAP: usize = 24;

/// The first-run toolchain row's title, the one noun on every surface.
pub(crate) const INSTALLING_ALAB: &str = "Installing ALab tools";
/// A heavy routine pass's live row: the same noun, and the verb says it updates what is already there.
pub(crate) const UPDATING_ALAB: &str = "Updating ALab tools";
/// The toolchain records' titles, one per outcome, so `appstatus` and the log tell a deferral, a failure and an install apart by title.
pub(crate) const PACKAGES_POSTPONED: &str = "ALab tools update postponed";
pub(crate) const PACKAGES_INSTALLED: &str = "ALab tools installed";
/// A pass that ended well with no roster (`seed-done:`).
pub(crate) const PACKAGES_FINISHED: &str = "ALab tools checked";
pub(crate) const PACKAGES_FAILED: &str = "Couldn't update ALab tools";
/// A first run that failed (its row and record): it was an install, not an update.
pub(crate) const ALAB_INSTALL_FAILED: &str = "Couldn't install ALab tools";
/// A pass that ended with no ALab tools on disk and no failure seen (the set excluded, a clean exit over an empty store).
pub(crate) const PACKAGES_NOT_INSTALLED: &str = "Couldn't install any ALab tools";
/// Nothing to install on this machine, as a fact (nothing published for its architecture, or every program removed on purpose).
pub(crate) const PACKAGES_UNAVAILABLE: &str = "No ALab tools to install";

/// `⇣` — a pass moving bytes.
const DOWN: char = '\u{21e3}';
/// `ℹ`, a record of the lane at work (a pass put off, a machine setting a pass
/// changed): a record wears its outcome's mark, not a moving row's.
const NOTED: char = '\u{2139}';

/// The pass name and start stamp that identify one `progress.json` writer, so a live meter is clamped to its own pass's high-water mark ([`snapshot_words`]).
pub(crate) type PassId = (String, u64);

/// The machine-settings records' capsule: Settings ▸ Security, where the
/// `[machine]` settings and the This Mac status are ([`machine_settings`]).
fn security() -> Intent {
    Intent::OpenSettings {
        route: crate::native_settings::SettingsRoute::Security
            .path()
            .to_string(),
    }
}

/// The `Packages` capsule every other toolchain record carries, pointing at the
/// page that holds the durable record.
fn packages() -> Intent {
    Intent::OpenSettings {
        route: crate::native_settings::SettingsRoute::Packages
            .path()
            .to_string(),
    }
}

/// A glyph from the band's closed set. A test checks every glyph this module
/// names is in it, so the fallback is unreachable here.
fn glyph(ch: char) -> Glyph {
    Glyph::or_fallback(ch)
}

/// A toolchain record's row: the `Packages` capsule, classified a record
/// ([`Message::record`]: `Hold::LogOnly`, never on glass).
fn row(severity: Severity, title: impl AsRef<str>) -> Message {
    Message::new(tags::TOOLCHAIN, severity, title)
        .action(packages())
        .record()
}

/// A record's words rebuilt unclassified at the default hold, for a test that needs them on the band.
#[cfg(test)]
pub(crate) fn held_row(record: &Message) -> Message {
    let mut row = Message::new(record.tag.clone(), record.severity, record.title.clone());
    row.glyph = record.glyph;
    row.detail = record.detail.clone();
    row.actions = record.actions.clone();
    row.key = record.key.clone();
    row.excerpt = record.excerpt;
    row.finished = record.finished.clone();
    row
}

/// A live toolchain row (the first run's announcement and meter, a heavy
/// routine pass) with no capsule: work in flight is not a decision, so
/// `Details ›` leads, and the cells a `Packages` chip would take keep the title
/// whole at 60 columns and `Details ›` whole at 80.
fn live_row(title: impl AsRef<str>, stale_after: Duration) -> Message {
    Message::new(tags::TOOLCHAIN, Severity::Info, title)
        .loads(PASS_LOADS)
        .primary_load(Load::Network)
        .working(stale_after)
}

/// The loads a pass row may show over its life ([`pass_load`] and the announcement's network).
pub(crate) const PASS_LOADS: [Load; 3] = [Load::Network, Load::Disk, Load::Cpu];

/// A toolchain record (module doc): logged and listed by `appstatus` as a
/// finished activity (`outcome=warn` ⇔ Warn), never on glass.
fn record(severity: Severity, title: impl AsRef<str>, detail: String) -> Message {
    row(severity, title).line(detail)
}

/// A record whose detail is atpkg's sentence, kept whole across lines: no
/// reporter pre-cuts a sentence it received, and a record is never painted.
fn whole_record(severity: Severity, title: &str, detail: &str) -> Message {
    row(severity, title).sentence(detail)
}

/// Which `~/.aterm/shell.d/00-atpkg.*` a frozen tab sources to pick up the managed programs in place.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum HookDialect {
    #[default]
    Zsh,
    Bash,
    Fish,
    PowerShell,
    None,
}

impl From<aterm_core::shell_integration::ShellType> for HookDialect {
    fn from(shell: aterm_core::shell_integration::ShellType) -> Self {
        use aterm_core::shell_integration::ShellType as S;
        match shell {
            S::Zsh => Self::Zsh,
            S::Bash => Self::Bash,
            S::Fish => Self::Fish,
            S::PowerShell => Self::PowerShell,
            S::Wsl => Self::None,
            S::Cmd | S::Unknown => Self::None,
            _ => Self::None,
        }
    }
}

impl HookDialect {
    /// The backtick-quoted line a person types in the frozen tab, or `None` for a shell without a hook.
    pub(crate) fn remedy(self) -> Option<String> {
        let base = atpkg::hooks::HOOK_BASENAME;
        let (verb, ext) = match self {
            Self::Zsh => (".", "zsh"),
            Self::Bash => (".", "bash"),
            Self::Fish => ("source", "fish"),
            Self::PowerShell => (".", "ps1"),
            Self::None => return None,
        };
        Some(format!("`{verb} ~/.aterm/shell.d/{base}.{ext}`"))
    }
}

/// Human byte figure in decimal units: `812 KB`, `512 MB`, `1.2 GB`.
pub(crate) fn fmt_bytes(n: u64) -> String {
    const KB: f64 = 1_000.0;
    const MB: f64 = 1_000_000.0;
    const GB: f64 = 1_000_000_000.0;
    let f = n as f64;
    if f >= GB - MB / 2.0 {
        format!("{:.1} GB", f / GB)
    } else if f >= MB - KB / 2.0 {
        format!("{:.0} MB", f / MB)
    } else if f >= KB {
        format!("{:.0} KB", f / KB)
    } else {
        format!("{n} B")
    }
}

/// `done / total` bytes as a meter's stats, `done` right-aligned to the total's width, so the stats keep one width for the whole transfer.
pub(crate) fn byte_stats(done: u64, total: u64) -> String {
    let (done, total) = (fmt_bytes(done.min(total)), fmt_bytes(total));
    let width = total.chars().count();
    format!("{done:>width$} / {total}")
}

/// `done` of `total` bytes as a meter fill in permille; `None` when the total is unknown (0), which is honestly no meter.
pub(crate) fn fill_permille(done: u64, total: u64) -> Option<u16> {
    (total > 0).then(|| {
        let f = done.min(total) as f64 / total as f64;
        (f * 1000.0).round() as u16
    })
}

/// `net-starting:` from a first-run child.
pub(crate) fn announced(detail: &str) -> Message {
    let size = detail
        .rsplit_once(" (")
        .and_then(|(_, t)| t.strip_suffix(')'))
        .map(|s| sanitize_for_tty(s, 40))
        .filter(|s| !s.is_empty());
    let stats = announced_stats(detail, size.as_deref());
    live_row(INSTALLING_ALAB, STALE_ANNOUNCE)
        .glyph(glyph(DOWN))
        .line(size.unwrap_or_default())
        .no_excerpt()
        .meter(Meter {
            stats,
            load: Some(Load::Network),
            ..Meter::busy("")
        })
        .key(KEY_PASS)
}

/// The announcement's stats, terse.
pub(crate) fn announced_stats(detail: &str, size: Option<&str>) -> String {
    let count = detail
        .strip_prefix("installing ")
        .and_then(|t| t.split_whitespace().next())
        .filter(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        .map(|n| format!("{n} program{}", if n == "1" { "" } else { "s" }));
    let size = size.map(|s| {
        let s = s
            .strip_suffix(" on disk when finished")
            .or_else(|| s.strip_suffix(" on disk"))
            .unwrap_or(s);
        s.strip_prefix("about ")
            .map_or_else(|| s.to_string(), |rest| format!("~{rest}"))
    });
    match (count, size) {
        (Some(c), Some(s)) => format!("{c} \u{00b7} {s}"),
        (c, s) => c.or(s).unwrap_or_default(),
    }
}

/// The store-lock wait ran out (atpkg exit 75).
pub(crate) fn deferred(detail: &str) -> Message {
    whole_record(Severity::Info, PACKAGES_POSTPONED, detail).glyph(glyph(NOTED))
}

/// Where the installed programs are used, when nothing narrows it: the one
/// clause the installed record and the managed-current record share.
const READY_IN_EVERY_TAB: &str = "ready in every aterm tab";

/// Why the installed programs reach no tab of this window, from the recorded shell-integration outcome (`spawn::shell_integration_outcome`), or `None` when the integration runs (or no shell was prepared).
pub(crate) fn unhooked_clause(
    shell_integration: Option<&crate::spawn::ShellIntegrationOutcome>,
) -> Option<String> {
    use crate::spawn::ShellIntegrationOutcome as Si;
    match shell_integration? {
        Si::Prepared => None,
        Si::UnknownShell(name) => Some(format!(
            "not wired into this window's tabs: aterm has no shell integration for {}",
            sanitize_for_tty(name, NAME_CAP)
        )),
        Si::WriteFailed(err) => Some(format!(
            "not wired into this window's tabs: aterm couldn't write its shell loader ({})",
            sanitize_for_tty(err, 80)
        )),
        Si::ArgsBypass(why) => Some(format!(
            "not wired into this window's tabs: shell integration is off ({})",
            sanitize_for_tty(why, 80)
        )),
    }
}

/// The installed pill's sentence.
pub(crate) fn seed_pill_text(
    installed: &[String],
    shell_integration: Option<&crate::spawn::ShellIntegrationOutcome>,
    frozen_tabs: usize,
    hook: HookDialect,
) -> String {
    let mut names = installed
        .iter()
        .take(5)
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(", ");
    if installed.len() > 5 {
        names.push('…');
    }
    if let Some(clause) = unhooked_clause(shell_integration) {
        return format!("✓ ALab toolchain installed: {names} — {clause}");
    }
    if frozen_tabs == 0 {
        return format!("✓ ALab toolchain installed: {names} — {READY_IN_EVERY_TAB}");
    }
    let agents_only = !installed.is_empty()
        && installed
            .iter()
            .all(|name| atpkg::stub::is_agent_program(name));
    if agents_only {
        return format!(
            "✓ ALab toolchain installed: {names} — ready in every tab opened since this update"
        );
    }
    let mut pill = format!(
        "✓ ALab toolchain installed: {names} — ready in every tab opened since this \
         update · {frozen_tabs} {} from before it",
        if frozen_tabs == 1 { "tab" } else { "tabs" }
    );
    match hook.remedy() {
        Some(command) => {
            pill.push_str(if frozen_tabs == 1 { " picks" } else { " pick" });
            pill.push_str(" them up with ");
            pill.push_str(&command);
        }
        None => pill.push_str(": a new tab picks them up"),
    }
    pill
}

/// `net-installed:`: the toolchain is here.
pub(crate) fn installed(text: &str) -> Message {
    let detail = text
        .strip_prefix("\u{2713} ALab toolchain installed: ")
        .unwrap_or(text);
    whole_record(Severity::Success, PACKAGES_INSTALLED, detail).key(KEY_PASS)
}

/// `seed-done:`: an announced pass that ended well with no install roster to name.
pub(crate) fn ended(detail: &str, first_run: bool) -> Message {
    let title = if first_run {
        PACKAGES_INSTALLED
    } else {
        PACKAGES_FINISHED
    };
    whole_record(Severity::Success, title, detail).key(KEY_PASS)
}

/// A bad terminal outcome for the toolchain lane (`seed-failed:` / `net-failed:`, a pass that could not launch, a coherence group's abort, a child that died after announcing).
pub(crate) fn failed(what: &str) -> Message {
    bad_outcome(PACKAGES_FAILED, what)
}

/// [`failed`] for a first run: the record the first-run failure row stands beside says what that row says.
pub(crate) fn install_failed(what: &str) -> Message {
    bad_outcome(ALAB_INSTALL_FAILED, what)
}

/// A pass that left no ALab tools on disk and saw no failure (`seed-nothing:`, or a `seed-unusable:` whose `[packages].exclude` left nothing).
pub(crate) fn not_installed(what: &str) -> Message {
    bad_outcome(PACKAGES_NOT_INSTALLED, what)
}

/// `seed-unusable:`: [`unavailable`] when `atpkg::cli::seed_unusable_is_informational` holds, else [`not_installed`].
pub(crate) fn unusable(what: &str) -> Message {
    if atpkg::cli::seed_unusable_is_informational(what) {
        unavailable(what)
    } else {
        not_installed(what)
    }
}

/// Nothing to install on this machine, and no failure: an Info record, "No ALab
/// tools to install", atpkg's sentence kept whole as its detail.
fn unavailable(what: &str) -> Message {
    whole_record(Severity::Info, PACKAGES_UNAVAILABLE, what)
        .glyph(glyph(NOTED))
        .key(KEY_PASS)
}

/// The Warn record [`failed`] and [`not_installed`] share: keyed to the pass,
/// the `Packages` capsule, the reason as whole lines ([`whole_reason`]).
fn bad_outcome(title: &str, what: &str) -> Message {
    row(Severity::Warn, title)
        .lines(whole_reason(what))
        .key(KEY_PASS)
}

/// A failure's reason, kept whole: a child that died after announcing leaves
/// only its stderr.
fn whole_reason(what: &str) -> Vec<String> {
    what.lines()
        .flat_map(|l| aterm_messages::text::split_sentence(l, DETAIL_LINE_CAP))
        .collect()
}

/// How a first run ended short (the marker its child printed): what the failure row after it says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FirstRunShort {
    /// `seed-failed:` / `net-failed:` (or a pass that could not launch).
    Failed,
    /// `seed-nothing:` or a non-informational `seed-unusable:`: the pass ran and nothing is installed.
    Nothing,
}

/// A cause the person can act on themselves, so its own words are painted: a
/// full disk, or atpkg's up-front refusal for one (`not enough disk space:
/// needs … free, … free here (… short)`, matched by its head,
/// [`atpkg::flow::DISK_REFUSAL_HEAD`]: the window and atpkg are one binary).
fn actionable_cause(cause: &str) -> bool {
    let lower = cause.to_ascii_lowercase();
    [
        atpkg::flow::DISK_REFUSAL_HEAD,
        "no space left",
        "disk full",
        "not enough space",
        "enospc",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

/// A first run that ended short.
pub(crate) fn first_run_short(how: FirstRunShort, cause: &str) -> Message {
    let (severity, title) = match how {
        FirstRunShort::Failed => (Severity::Warn, ALAB_INSTALL_FAILED),
        FirstRunShort::Nothing => (Severity::Warn, PACKAGES_NOT_INSTALLED),
    };
    let msg = Message::new(tags::PACKAGES, severity, title)
        .action(packages())
        .key(KEY_PASS);
    let msg = msg.lines(whole_reason(cause));
    if actionable_cause(cause) {
        msg
    } else {
        msg.no_excerpt()
    }
}

/// A plain text posted from outside the process (`aterm ctl appnotice <lane> <text>`), e.g. an `aterm pkg install claude` run in a terminal.
pub(crate) fn appnotice(tag: Tag, text: &str) -> Message {
    let text = sanitize_for_tty(text, 120);
    match installed_words(&text) {
        Some(title) => Message::new(tag, Severity::Info, title).line(text).record(),
        None => Message::new(tag, Severity::Info, text).record(),
    }
}

/// `Claude Code 2.1.267 installed` from `aterm pkg install claude: 2.1.267
/// installed`; `None` for text in any other shape.
fn installed_words(text: &str) -> Option<String> {
    let rest = text.strip_prefix("aterm pkg install ")?;
    let (program, outcome) = rest.split_once(": ")?;
    let version = outcome.strip_suffix(" installed")?.trim();
    let version = version
        .strip_prefix(program.trim())
        .map_or(version, str::trim_start);
    let name = known_as(program.trim());
    (!name.is_empty() && !version.is_empty() && !version.contains(' '))
        .then(|| format!("{name} {version} installed"))
}

/// The program as a person knows it: a vendor program's product from atpkg's
/// table (`claude` is Claude Code), any other name verbatim (`gemini`).
fn known_as(program: &str) -> &str {
    atpkg::vendor_direct::spec(program).map_or(program, |spec| spec.product)
}

/// [`managed_current_for`] with only frozen shells counted, for tests; `this_tab_hooked` false stands for an unknown shell.
#[cfg(test)]
pub(crate) fn managed_current(
    text: &str,
    frozen_tabs: usize,
    this_tab_hooked: bool,
    hook: HookDialect,
) -> Option<Message> {
    managed_current_for(
        text,
        TabsBehind {
            frozen_shells: frozen_tabs,
            ..TabsBehind::default()
        },
        test_unhooked(this_tab_hooked).as_deref(),
        hook,
    )
}

/// The window-side cause for a test's `this_tab_hooked`: an unknown shell.
#[cfg(test)]
fn test_unhooked(this_tab_hooked: bool) -> Option<String> {
    let unknown = crate::spawn::ShellIntegrationOutcome::UnknownShell("nu".to_string());
    (!this_tab_hooked)
        .then(|| unhooked_clause(Some(&unknown)))
        .flatten()
}

/// `managed-current:`: each agent program (`atpkg::stub::AGENT_PROGRAMS`) as a keyed record of [`managed_current_words_for`].
pub(crate) fn managed_current_for(
    text: &str,
    tabs: TabsBehind,
    unhooked: Option<&str>,
    hook: HookDialect,
) -> Option<Message> {
    let (title, detail) = managed_current_words_for(text, tabs, unhooked, hook)?;
    let severity = if managed_items(text)
        .iter()
        .all(|item| item.standing.is_none())
    {
        Severity::Success
    } else {
        Severity::Info
    };
    Some(record(severity, title, detail).key(KEY_MANAGED))
}

/// One program of the managed-current wire text: `<name> <version> (<standing>)`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ManagedItem {
    /// atpkg's name (`claude`).
    pub(crate) name: String,
    /// Its version, empty when the wire names none.
    pub(crate) version: String,
    /// `None` while it is its vendor's head (`(<Vendor> latest)`, an older atpkg's `(build <N>)`, or no parenthesis).
    pub(crate) standing: Option<String>,
}

/// The managed-current wire text, item by item ([`ManagedItem`]); an item with no name is dropped.
pub(crate) fn managed_items(text: &str) -> Vec<ManagedItem> {
    text.split(';')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .filter_map(|item| {
            let (head, paren) = match item.split_once('(') {
                Some((head, rest)) => (head.trim(), rest.trim_end_matches(')').trim()),
                None => (item, ""),
            };
            let mut words = head.split_whitespace();
            let name = words.next()?.to_string();
            let version = words.next().unwrap_or_default().to_string();
            let standing =
                if paren.is_empty() || paren.ends_with(" latest") || paren.starts_with("build ") {
                    None
                } else if paren == "failed" || paren == "refused" {
                    Some("failed to update".to_string())
                } else {
                    Some(sanitize_for_tty(paren, NAME_CAP))
                };
            Some(ManagedItem {
                name,
                version,
                standing,
            })
        })
        .collect()
}

/// How the live tabs stand against the programs the managed-current record names, so the record neither calls lagging sessions up to date nor hands a hook line to a tab whose foreground is an agent.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct TabsBehind {
    /// Tabs whose shell runs a frozen PATH (measured, or carried and not yet measured) with no agent in front.
    pub(crate) frozen_shells: usize,
    /// Frozen tabs whose foreground is an agent that the live upgrade is not moving.
    pub(crate) frozen_agents: usize,
    /// Running agent sessions on a build older than the version this record names for their program that the live upgrade moves on.
    pub(crate) agents_behind: PerAgent,
    /// Running agent sessions running a build older than that version that will not move on their own.
    pub(crate) agents_held: PerAgent,
    /// Live tabs with no adoption mark whose shell's PATH was measured, two jobs in a row, to put a foreign copy of an agent program first (`SessionTimeline::path_settled_frozen`).
    pub(crate) shadowed: usize,
}

/// A count of running sessions per agent the live upgrade moves ([`aterm_agent::harness::upgrade::Agent`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct PerAgent([usize; aterm_agent::harness::upgrade::Agent::ALL.len()]);

impl PerAgent {
    /// One more of `agent`'s sessions.
    pub(crate) fn count(&mut self, agent: aterm_agent::harness::upgrade::Agent) {
        self.0[agent as usize] += 1;
    }

    /// `n` of `agent`'s sessions and none of another's.
    #[cfg(test)]
    pub(crate) fn of(counts: &[(aterm_agent::harness::upgrade::Agent, usize)]) -> Self {
        let mut each = Self::default();
        for &(agent, n) in counts {
            each.0[agent as usize] += n;
        }
        each
    }

    /// Every agent's sessions together.
    fn total(self) -> usize {
        self.0.iter().sum()
    }

    /// Each agent's product name and count, Claude Code first and then in
    /// [`Agent::ALL`](aterm_agent::harness::upgrade::Agent::ALL)'s order: the
    /// order the record says them in.
    fn each(self) -> impl Iterator<Item = (&'static str, usize)> {
        aterm_agent::harness::upgrade::Agent::ALL
            .into_iter()
            .map(move |agent| (agent.product(), self.0[agent as usize]))
    }
}

/// The version the managed-current wire text gives `name` (`claude 2.1.282 (Anthropic latest); codex …`), or `None` when it names none.
pub(crate) fn managed_version(text: &str, name: &str) -> Option<String> {
    managed_items(text)
        .into_iter()
        .find(|item| item.name == name && !item.version.is_empty())
        .map(|item| item.version)
}

/// [`managed_current_words_for`] with only frozen shells counted, for tests.
#[cfg(test)]
pub(crate) fn managed_current_words(
    text: &str,
    frozen_tabs: usize,
    this_tab_hooked: bool,
    hook: HookDialect,
) -> Option<(String, String)> {
    managed_current_words_for(
        text,
        TabsBehind {
            frozen_shells: frozen_tabs,
            ..TabsBehind::default()
        },
        test_unhooked(this_tab_hooked).as_deref(),
        hook,
    )
}

/// The managed-current title over `items`, `None` when there are none. Every
/// program is named: those at their vendor's head first, "current" (or
/// "installed" while an agent session runs an older build, `lagging`), then the
/// others grouped by what is said of them (`Claude Code 2.1.287 and Muse Code
/// 1.4.9-R6297.1 not checked`). Versions are shown as `current_versions` and
/// `other_versions` say; the parenthesis never is.
fn managed_title(
    items: &[ManagedItem],
    lagging: bool,
    current_versions: bool,
    other_versions: bool,
) -> Option<String> {
    let mut groups: Vec<(Option<&str>, Vec<String>)> = Vec::new();
    for item in items {
        let standing = item.standing.as_deref();
        let display = known_as(&item.name);
        let versioned = if standing.is_none() {
            current_versions
        } else {
            other_versions
        };
        let named = if versioned && !item.version.is_empty() {
            sanitize_for_tty(&format!("{display} {}", item.version), NAME_CAP + 24)
        } else {
            sanitize_for_tty(display, NAME_CAP)
        };
        match groups.iter_mut().find(|(said, _)| *said == standing) {
            Some((_, names)) => names.push(named),
            None => groups.push((standing, vec![named])),
        }
    }
    let said = |(standing, names): &(Option<&str>, Vec<String>)| {
        let word = match (*standing, names.len(), lagging) {
            (Some(word), ..) => word,
            (None, _, true) => "installed",
            (None, 1, false) => "is current",
            (None, _, false) => "are current",
        };
        format!("{} {word}", and_list(names))
    };
    let (current, others): (Vec<_>, Vec<_>) = groups.iter().partition(|(s, _)| s.is_none());
    let others: Vec<String> = others.into_iter().map(said).collect();
    let parts: Vec<String> = current
        .into_iter()
        .map(said)
        .chain((!others.is_empty()).then(|| and_list(&others)))
        .collect();
    (!parts.is_empty()).then(|| parts.join("; "))
}

/// `words` as a person reads a list: `a`, `a and b`, `a, b and c`.
fn and_list(words: &[String]) -> String {
    match words {
        [] => String::new(),
        [one] => one.clone(),
        [head @ .., last] => format!("{} and {last}", head.join(", ")),
    }
}

/// The managed-current record's `(title, detail)`, or `None` for an empty body.
pub(crate) fn managed_current_words_for(
    text: &str,
    tabs: TabsBehind,
    unhooked: Option<&str>,
    hook: HookDialect,
) -> Option<(String, String)> {
    let frozen_tabs = tabs.frozen_shells;
    let items = managed_items(text);
    let standings: Vec<&str> = items.iter().filter_map(|i| i.standing.as_deref()).collect();
    let unchecked = standings
        .iter()
        .filter(|s| **s == atpkg::cli::NOT_CHECKED)
        .count();
    let otherwise = standings.len() - unchecked;
    let lagging = tabs.agents_behind.total() + tabs.agents_held.total() > 0;
    let fits = |title: &String| title.chars().count() <= TITLE_CAP;
    let title = [(true, true), (true, false), (false, false)]
        .into_iter()
        .filter_map(|(current, others)| managed_title(&items, lagging, current, others))
        .reduce(|said, shorter| if fits(&said) { said } else { shorter })?;
    let title = sanitize_for_tty(&title, TITLE_CAP);
    let mut pieces: Vec<String> = Vec::new();
    if let Some(clause) = unhooked {
        pieces.push(clause.to_string());
    } else if frozen_tabs == 0 && tabs.frozen_agents == 0 && tabs.shadowed == 0 {
        pieces.push(READY_IN_EVERY_TAB.to_string());
    }
    if unchecked > 0 {
        pieces.push(
            if unchecked == 1 {
                "not checked: its vendor was not reached lately, so a newer build may be out"
            } else {
                "not checked: their vendors were not reached lately, so newer builds may be out"
            }
            .to_string(),
        );
    }
    if otherwise > 0 {
        pieces.push(
            if otherwise == 1 {
                "`aterm pkg doctor` says why it is not current"
            } else {
                "`aterm pkg doctor` says why they are not current"
            }
            .to_string(),
        );
    }
    for (product, n) in tabs.agents_behind.each() {
        match n {
            0 => {}
            1 => pieces.push(format!(
                "1 running {product} session moves onto it on its own"
            )),
            n => pieces.push(format!(
                "{n} running {product} sessions move onto it on their own"
            )),
        }
    }
    for (product, n) in tabs.agents_held.each() {
        match n {
            0 => {}
            1 => pieces.push(format!(
                "1 running {product} session is still on the old build (`aterm harness upgrade \
                 --status` says why)"
            )),
            n => pieces.push(format!(
                "{n} running {product} sessions are still on the old build (`aterm harness \
                 upgrade --status` says why)"
            )),
        }
    }
    if tabs.frozen_agents > 0 {
        let n = tabs.frozen_agents;
        let mut note = format!(
            "{n} {} from before this update running an agent",
            if n == 1 { "tab" } else { "tabs" }
        );
        match hook.remedy() {
            Some(command) => {
                note.push_str(if n == 1 { " picks" } else { " pick" });
                note.push_str(" them up with ");
                note.push_str(&command);
                note.push_str(" once the agent exits");
            }
            None => note.push_str(": a new tab picks them up"),
        }
        pieces.push(note);
    }
    if tabs.shadowed > 0 {
        let n = tabs.shadowed;
        pieces.push(if n == 1 {
            "1 tab's PATH puts another copy of an agent first".to_string()
        } else {
            format!("{n} tabs' PATHs put another copy of an agent first")
        });
    }
    if frozen_tabs > 0 {
        let mut note = format!(
            "{frozen_tabs} {} from before this update",
            if frozen_tabs == 1 { "tab" } else { "tabs" }
        );
        match hook.remedy() {
            Some(command) => {
                note.push_str(if frozen_tabs == 1 { " picks" } else { " pick" });
                note.push_str(" them up with ");
                note.push_str(&command);
            }
            None => note.push_str(": a new tab picks them up"),
        }
        pieces.push(note);
    }
    Some((
        title,
        shape_detail(&pieces.join(aterm_messages::PIECE_SEP), DETAIL_LINE_CAP),
    ))
}

/// `machine-settings:`: the machine-level settings a pass changed per doctor, as atpkg lists them: `universal-control disabled`.
pub(crate) fn machine_settings(text: &str) -> Vec<Message> {
    text.split(';')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|item| {
            let word = item
                .split_whitespace()
                .next()
                .unwrap_or_default()
                .to_ascii_lowercase();
            let (title, detail) = machine_setting_words(item);
            Message::new(tags::TOOLCHAIN, Severity::Info, title)
                .action(security())
                .record()
                .line(detail)
                .glyph(glyph(NOTED))
                .key(&format!("{KEY_MACHINE_PREFIX}{word}"))
        })
        .collect()
}

/// One machine-settings wire item as its row's words `(title, detail)`.
pub(crate) fn machine_setting_words(item: &str) -> (String, String) {
    let item = sanitize_for_tty(item, 80);
    let (title, detail) = if item == atpkg::machine::UNIVERSAL_CONTROL_ENTRY {
        (
            "ALab tools turned off Universal Control".to_string(),
            format!(
                "to turn it back on, set Universal Control on this Mac to leave and run: {}",
                atpkg::machine::UNIVERSAL_CONTROL_REVERT
            ),
        )
    } else if item == atpkg::machine::ROSETTA_ENTRY {
        (
            "ALab tools installed Rosetta 2".to_string(),
            "Intel apps and tools now run on this Mac; macOS keeps Rosetta 2, and [machine] \
             rosetta = false stops aterm installing it again"
                .to_string(),
        )
    } else {
        (item.clone(), String::new())
    };
    (
        sanitize_for_tty(&title, 120),
        shape_detail(&detail, DETAIL_LINE_CAP),
    )
}

/// One thing a pass's look found that aterm leaves alone, as atpkg named it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Foreign {
    /// The seam (`trust`) or the managed program a copy stands before.
    pub(crate) name: String,
    /// The entry or copy that is not aterm's.
    pub(crate) path: String,
    /// atpkg's fix sentence.
    pub(crate) fix: String,
}

/// A `foreign:` line's body (`seam none`, `path trust /u/bin/trustdoc — type …`) as its look's scope ([`atpkg::cli::FOREIGN_SEAM`], [`atpkg::cli::FOREIGN_PATH`]) and what it found.
pub(crate) fn foreign_report(body: &str) -> Option<(&'static str, Vec<Foreign>)> {
    let (scope, rest) = body.trim().split_once(' ')?;
    let scope = [atpkg::cli::FOREIGN_SEAM, atpkg::cli::FOREIGN_PATH]
        .into_iter()
        .find(|s| *s == scope)?;
    let rest = rest.trim();
    if rest == atpkg::cli::FOREIGN_NONE {
        return Some((scope, Vec::new()));
    }
    let items = rest
        .split("; ")
        .filter_map(|item| {
            let (head, fix) = item.split_once(" \u{2014} ")?;
            let fix = sanitize_for_tty(fix.trim(), DETAIL_LINE_CAP);
            let (name, path) = head.trim().split_once(' ')?;
            (!fix.is_empty()).then(|| Foreign {
                name: sanitize_for_tty(name, NAME_CAP),
                path: sanitize_for_tty(path.trim(), DETAIL_LINE_CAP),
                fix,
            })
        })
        .collect();
    Some((scope, items))
}

/// The digest entry for what one look of a pass found that aterm leaves alone ([`foreign_report`]): a fact for the owner, not now.
pub(crate) fn foreign_tools_entry(
    scope: &str,
    items: &[Foreign],
) -> aterm_messages::exceptions::Entry {
    use aterm_messages::exceptions::{Entry, Family, MORE_LINES};
    use aterm_messages::need::{Reason, Resurface, Then, Why};
    let seam = scope == atpkg::cli::FOREIGN_SEAM;
    let line = |f: &Foreign| {
        let fact = if seam {
            format!(
                "rustup's {} toolchain is {}, not aterm's link",
                f.name, f.path
            )
        } else {
            format!("{} runs before the managed {}", f.path, f.name)
        };
        format!("{fact}: {}", f.fix)
    };
    let mut entry = Entry::new(
        Family::Toolchain,
        scope,
        "foreign",
        if seam {
            "Rustup's toolchain link is not aterm's"
        } else {
            "Foreign tools run before ALab tools"
        },
        Why::OnlyYou(Reason::new(
            "aterm never changes a link or a copy it did not lay",
        )),
        Then::step("Fix each one as its line under Details says"),
    )
    .resurface(Resurface::CHOICE);
    let shown = if items.len() > MORE_LINES {
        MORE_LINES - 1
    } else {
        items.len()
    };
    for item in &items[..shown] {
        entry = entry.more(line(item));
    }
    if shown < items.len() {
        entry = entry.more(format!(
            "{} more: `aterm pkg doctor` names each",
            items.len() - shown
        ));
    }
    entry.log_line(format!(
        "toolchain-foreign scope={scope} items={}",
        items.len()
    ))
}

/// Why a pass files the C toolchain's entry ([`c_toolchain_entry`]): the [`atpkg::cli::C_TOOLCHAIN_MARKER`] body it said.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CToolchain {
    /// `missing`: no toolchain is installed (on a Mac, no Command Line Tools).
    Missing,
    /// `broken`: a compiler is installed and cannot build, which installing the toolchain fixes (off macOS: most often the libc headers).
    Broken,
}

/// Digest entry when a pass found no working C toolchain where an install would fix it ([`atpkg::cli::C_TOOLCHAIN_MARKER`]).
pub(crate) fn c_toolchain_entry(cause: CToolchain) -> aterm_messages::exceptions::Entry {
    use aterm_messages::exceptions::{Entry, Family};
    use aterm_messages::need::{Reason, Resurface, Then, Why};
    let mac = cfg!(target_os = "macos");
    let (word, what) = match cause {
        CToolchain::Missing if mac => ("missing", "Command Line Tools are missing"),
        CToolchain::Missing => ("missing", "No C compiler is installed"),
        CToolchain::Broken => ("broken", "The C compiler cannot build"),
    };
    Entry::new(
        Family::Toolchain,
        "clt",
        word,
        what,
        Why::OnlyYou(Reason::new(if mac {
            "installing them asks for your password in a terminal"
        } else {
            "installing one needs your distribution's package manager as root"
        })),
        if mac {
            Then::step(format!("Run `{}` in a terminal", atpkg::prereq::act()))
                .or_short("Run the command under Details")
        } else {
            Then::step("Install your distribution's C compiler")
        },
    )
    .resurface(Resurface::CHOICE)
    .more(if mac {
        "Builds that compile C (most Rust crates), git and make need cc and the macOS SDK"
    } else {
        atpkg::prereq::act()
    })
    .log_line(format!("toolchain-c {word}"))
}

/// What one classified `progress.json` read says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SnapshotWords {
    /// The file vanished at child exit: a live row has nothing honest left to say.
    Vanished,
    /// The pass is over (ended cleanly, with failures, or its writer gone) and the read claims no outcome.
    Over,
    /// Nothing planned while the pass runs.
    Quiet,
    /// A pass in flight: the words, the meter and the tailed cap, plus the pass they belong to (for the next read's high-water clamp).
    Live {
        /// The row's words: `Hold::Live { STALE_TAILED }`, keyed [`KEY_PASS`].
        message: Box<Message>,
        /// The writer's identity — pass name and start stamp.
        pass_id: PassId,
    },
}

/// The words for one classified read, pure.
pub(crate) fn snapshot_words(
    snap: Option<&crate::PkgProgressSnapshot>,
    peak: Option<&(PassId, u16)>,
    routine: bool,
) -> SnapshotWords {
    let Some(snap) = snap else {
        return SnapshotWords::Vanished;
    };
    if !snap.running {
        return SnapshotWords::Over;
    }
    let f = &snap.file;
    if f.v != PROGRESS_VERSION {
        return SnapshotWords::Live {
            message: Box::new(
                live_row(INSTALLING_ALAB, STALE_TAILED)
                    .glyph(glyph(DOWN))
                    .no_excerpt()
                    .in_flight()
                    .key(KEY_PASS),
            ),
            pass_id: (f.pass.clone(), f.started_unix),
        };
    }
    let total = f.overall.programs_total;
    let done = f.overall.programs_done;
    let title = match f.pass.as_str() {
        _ if routine => UPDATING_ALAB,
        _ => INSTALLING_ALAB,
    };
    let planned = total > 0 || !f.programs.is_empty() || !f.queue.is_empty();
    if !planned {
        return SnapshotWords::Quiet;
    }
    let pass_id: PassId = (f.pass.clone(), f.started_unix);
    let (detail, row_fill) = current_program_line(f);
    let same_pass = peak.filter(|(id, _)| *id == pass_id).map(|(_, fill)| *fill);
    let fill = overall_fill(f)
        .or(row_fill)
        .map(|x| same_pass.map_or(x, |p| x.max(p)));
    let stats = if total > 0 {
        format!("{done} of {total} programs")
    } else {
        String::new()
    };
    let downloaded = (f.overall.bytes_total > 0).then(|| {
        format!(
            "{} downloaded",
            byte_stats(f.overall.bytes_done, f.overall.bytes_total)
        )
    });
    let held = f
        .programs
        .values()
        .any(|r| matches!(r.phase, Phase::Verify | Phase::Link));
    let amount = fill.map(|done| Amount {
        series: Amount::series_of(&format!("toolchain {} {}", pass_id.0, pass_id.1)),
        done: u64::from(done),
        total: 1000,
        unit: if held { Unit::HeldSteps } else { Unit::Steps },
    });
    let message = live_row(title, STALE_TAILED)
        .glyph(glyph(DOWN))
        .line(detail)
        .lines(downloaded)
        .no_excerpt()
        .meter(Meter {
            fill_permille: fill,
            stats,
            amount,
            load: pass_load(f),
            busy: fill.is_none(),
        })
        .key(KEY_PASS);
    SnapshotWords::Live {
        message: Box::new(message),
        pass_id,
    }
}

/// The resource the pass loads hardest right now: any program extracting or linking loads the disk.
pub(crate) fn pass_load(f: &atpkg::progress::ProgressFile) -> Option<Load> {
    let active = |phases: &[Phase]| f.programs.values().any(|r| phases.contains(&r.phase));
    if active(&[Phase::Extract, Phase::Link]) {
        Some(Load::Disk)
    } else if active(&[Phase::Verify]) {
        Some(Load::Cpu)
    } else if active(&[Phase::Download]) {
        Some(Load::Network)
    } else {
        None
    }
}

/// The overall meter: programs, each weighted by phase. A finished program
/// (done, failed or skipped) counts whole; one in flight counts its download
/// as the first half (by bytes), a verify as half, its extraction as the
/// second half (by bytes, to 99 %), a link as 99 %.
fn overall_fill(f: &atpkg::progress::ProgressFile) -> Option<u16> {
    let total = u64::from(f.overall.programs_total);
    if total == 0 {
        return None;
    }
    let share = |r: &atpkg::progress::ProgramProgress, weight: u64| {
        (weight * r.bytes_done.min(r.bytes_total))
            .checked_div(r.bytes_total)
            .unwrap_or(0)
    };
    let in_flight: u64 = f
        .programs
        .values()
        .map(|r| match r.phase {
            Phase::Download => share(r, 500),
            Phase::Verify => 500,
            Phase::Extract => 500 + share(r, 490),
            Phase::Link => 990,
            Phase::Queued | Phase::Done | Phase::Failed | Phase::Skipped => 0,
        })
        .sum();
    let done = u64::from(f.overall.programs_done.min(f.overall.programs_total)) * 1000;
    u16::try_from(((done + in_flight) / total).min(1000)).ok()
}

/// The program the pass is working on now, as one phrase, plus that program's
/// own byte meter when its phase has one: the fallback fill for a pass whose
/// overall rollup is silent (no signed sizes to sum).
fn current_program_line(f: &atpkg::progress::ProgressFile) -> (String, Option<u16>) {
    let active = f
        .programs
        .iter()
        .filter(|(_, r)| {
            matches!(
                r.phase,
                Phase::Download | Phase::Verify | Phase::Extract | Phase::Link
            )
        })
        .min_by_key(|(_, r)| (r.downloaded(), r.phase as u8));
    let Some((raw, row)) =
        active.or_else(|| f.queue.first().and_then(|n| f.programs.get_key_value(n)))
    else {
        return (String::new(), None);
    };
    let Some(name) = admitted_name(raw) else {
        return (String::new(), None);
    };
    let frac = fill_permille(row.bytes_done, row.bytes_total);
    let phase = match row.phase {
        Phase::Queued => "queued".to_string(),
        Phase::Download => "downloading".to_string(),
        Phase::Verify => "verifying".to_string(),
        Phase::Extract => "extracting".to_string(),
        Phase::Link => "linking".to_string(),
        Phase::Done => "installed".to_string(),
        Phase::Failed => match row.error.as_deref() {
            Some(e) => format!("failed: {}", sanitize_for_tty(e, ERROR_CAP)),
            None => "failed".to_string(),
        },
        Phase::Skipped => "up to date".to_string(),
    };
    let line = format!("{name} \u{00b7} {phase}");
    (
        line,
        if matches!(row.phase, Phase::Download | Phase::Extract) {
            frac
        } else {
            None
        },
    )
}

/// A program name is untrusted until it round-trips the store's name gate; one
/// that fails simply has no words on the row.
fn admitted_name(raw: &str) -> Option<String> {
    let name = atpkg::store::ToolName::new(raw)?;
    Some(sanitize_for_tty(name.as_str(), NAME_CAP))
}

#[cfg(test)]
mod tests {
    use super::*;
    use aterm_agent::harness::upgrade::Agent;
    use aterm_messages::{Hold, Instant, MessageCenter, MessageLog, TITLE_CAP, WallStamp};
    use std::collections::BTreeMap;

    fn file(running_pid: Option<u32>, pass: &str) -> atpkg::progress::ProgressFile {
        atpkg::progress::ProgressFile {
            v: PROGRESS_VERSION,
            pid: running_pid,
            pass: pass.to_string(),
            started_unix: 1_700_000_000,
            generation: None,
            heartbeat_unix: 1_700_000_000,
            overall: atpkg::progress::Overall {
                programs_done: 3,
                programs_total: 10,
                bytes_done: 512_000_000,
                bytes_total: 1_200_000_000,
            },
            queue: vec!["ty".into(), "ay".into()],
            programs: BTreeMap::from([
                (
                    "trust".to_string(),
                    atpkg::progress::ProgramProgress {
                        phase: Phase::Extract,
                        bytes_done: 120_000_000,
                        bytes_total: 900_000_000,
                        build: Some(5520),
                        bumped: false,
                        error: None,
                    },
                ),
                (
                    "ty".to_string(),
                    atpkg::progress::ProgramProgress {
                        phase: Phase::Queued,
                        bytes_done: 0,
                        bytes_total: 0,
                        build: None,
                        bumped: false,
                        error: None,
                    },
                ),
            ]),
            ended_unix: None,
        }
    }

    fn snap(running: bool) -> crate::PkgProgressSnapshot {
        crate::PkgProgressSnapshot {
            file: file(Some(7), "net"),
            running,
        }
    }

    fn live_of(words: SnapshotWords) -> (Message, PassId) {
        match words {
            SnapshotWords::Live { message, pass_id } => (*message, pass_id),
            other => panic!("expected a live reading, got {other:?}"),
        }
    }

    fn fill_of(msg: &Message) -> Option<u16> {
        msg.meter.as_ref().and_then(|m| m.fill_permille)
    }

    fn stats_of(msg: &Message) -> &str {
        msg.meter.as_ref().map_or("", |m| m.stats.as_str())
    }

    /// Every live toolchain row completes in its finished form (`Installed ALab
    /// tools`, `Updated ALab tools`) with nothing else on the row moving at 60,
    /// 80, 120 and 160 columns.
    #[test]
    fn every_live_toolchain_row_completes_in_its_finished_form() {
        use crate::message_band::assert_completes_in_place as completes;
        completes(
            &announced("installing 2 program(s) (about 1 GB)"),
            "Installed ALab tools",
        );
        completes(
            &live_of(snapshot_words(Some(&snap(true)), None, false)).0,
            "Installed ALab tools",
        );
        completes(
            &live_of(snapshot_words(Some(&snap(true)), None, true)).0,
            "Updated ALab tools",
        );
        let mut other = snap(true);
        other.file.pass = "seed".into();
        completes(
            &live_of(snapshot_words(Some(&other), None, false)).0,
            "Installed ALab tools",
        );
        let mut newer = snap(true);
        newer.file.v = PROGRESS_VERSION + 1;
        completes(
            &live_of(snapshot_words(Some(&newer), None, false)).0,
            "Installed ALab tools",
        );
    }

    /// Every record is a toolchain row with the `Packages` capsule (a
    /// machine-settings record's is Settings ▸ Security, where its switches are)
    /// and every live row carries none; each has a title inside the cap and a
    /// glyph from the band's closed set, and each route is the Settings page's
    /// own path.
    #[test]
    fn every_toolchain_record_carries_the_packages_capsule_and_an_admitted_glyph() {
        let rows = vec![
            announced("installing 2 program(s) (about 1 GB)"),
            deferred("an earlier toolchain pass is still running"),
            not_installed("[packages].exclude narrows the toolset to nothing"),
            unusable(
                "ALab publishes no build for this machine's architecture (x86_64-apple-darwin)",
            ),
            installed("✓ ALab toolchain installed: ay, trust — ready in every aterm tab"),
            ended("the pass finished; 12 ALab program(s) are installed", false),
            failed("partly installed: ay"),
            install_failed("no reason was given"),
            managed_current(
                "claude 2.1.280 (Anthropic latest)",
                0,
                true,
                HookDialect::Zsh,
            )
            .unwrap(),
            live_of(snapshot_words(Some(&snap(true)), None, false)).0,
        ];
        let machine = machine_settings("universal-control disabled; something-new tuned");
        for m in &machine {
            assert_eq!(m.actions, vec![security()], "{}", m.title);
        }
        let rows: Vec<Message> = rows.into_iter().chain(machine).collect();
        for m in &rows {
            assert_eq!(m.tag, tags::TOOLCHAIN, "{}", m.title);
            if matches!(m.hold, Hold::Live { .. }) {
                assert!(m.actions.is_empty(), "{}: a live row", m.title);
            } else if !m
                .key
                .as_deref()
                .is_some_and(|k| k.starts_with(KEY_MACHINE_PREFIX))
            {
                assert_eq!(m.actions, vec![packages()], "{}", m.title);
            }
            let class = crate::message_reporters::attention(m)
                .unwrap_or_else(|why| panic!("{why}: {}", m.title));
            if matches!(m.hold, Hold::Live { .. }) {
                assert!(m.meter.is_some(), "{}: in flight", m.title);
                assert_eq!(class, crate::message_reporters::Attention::Progress);
            }
            assert!(m.title.chars().count() <= TITLE_CAP, "{}", m.title);
            assert!(
                Glyph::ALLOWED.contains(&m.glyph.ch()) && m.glyph != Glyph::FALLBACK,
                "{}: {:?}",
                m.title,
                m.glyph
            );
        }
        assert_eq!(
            packages(),
            Intent::OpenSettings {
                route: "/packages".into()
            }
        );
        assert_eq!(
            security(),
            Intent::OpenSettings {
                route: crate::native_settings::SettingsRoute::Security
                    .path()
                    .into()
            }
        );
        for ch in [DOWN, NOTED] {
            assert!(Glyph::new(ch).is_some(), "{ch:?} is in the closed set");
        }
    }

    /// A first run that ended short is a failure row titled by how it ended;
    /// the cause rides behind Details unless the person can fix it (a full
    /// disk), and the capsule is the page that retries.
    #[test]
    fn a_first_run_that_ended_short_is_a_failure_row() {
        use crate::message_reporters::{Attention, attention};
        let failed = first_run_short(FirstRunShort::Failed, "could not reach the index");
        assert_eq!(
            failed.title, ALAB_INSTALL_FAILED,
            "an install, in the lane's noun"
        );
        assert_eq!(failed.severity, Severity::Warn, "ruling 309");
        assert_eq!(
            failed.severity,
            first_run_short(FirstRunShort::Nothing, "x").severity,
            "one family, one severity"
        );
        assert_eq!(failed.hold, Hold::Default);
        assert_eq!(failed.key.as_deref(), Some(KEY_PASS));
        assert_eq!(
            failed.tag,
            tags::PACKAGES,
            "an outcome, not the lane's live work"
        );
        assert_eq!(failed.actions, vec![packages()]);
        assert_eq!(failed.detail, ["could not reach the index"]);
        assert!(
            !failed.excerpt,
            "a cause the person cannot fix rides behind Details"
        );
        assert_eq!(attention(&failed), Ok(Attention::Failure));
        let full = first_run_short(FirstRunShort::Failed, "trust: No space left on device");
        assert!(full.excerpt, "a full disk is the person's to fix: painted");
        assert_eq!(
            full.detail,
            ["trust: No space left on device"],
            "the cause once, in its own words"
        );
        let refusal = atpkg::flow::FlowError::InsufficientDisk {
            required: 4 << 30,
            available: 3 << 30,
        }
        .to_string();
        let refused = first_run_short(FirstRunShort::Failed, &refusal);
        assert!(
            refused.excerpt,
            "the disk check's refusal is painted: {refusal}"
        );
        assert_eq!(refused.detail, [refusal.as_str()]);
        let stderr = format!("atpkg: bootstrap install trust failed: {refusal} (continuing)");
        assert!(first_run_short(FirstRunShort::Failed, &stderr).excerpt);
        let long = "net pass could not finish: the index at \
                    https://packages.example.invalid/alab/index.json answered 503 Service \
                    Unavailable three times in a row, and the mirror list had no other \
                    entry to try before the deadline ran out";
        assert!(long.chars().count() > 160, "PRECONDITION: past the old cut");
        let whole = first_run_short(FirstRunShort::Failed, long);
        let words = |lines: &[String]| {
            lines
                .iter()
                .flat_map(|l| l.split_whitespace())
                .map(str::to_string)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            words(&whole.detail),
            long.split_whitespace()
                .map(str::to_string)
                .collect::<Vec<_>>(),
            "every word survives: {:?}",
            whole.detail
        );
        assert!(
            !whole.detail.iter().any(|l| l.contains('\u{2026}')),
            "no cut"
        );
        assert_eq!(
            whole.detail,
            super::failed(long).detail,
            "the row and its record say the same cause"
        );
        let nothing = first_run_short(FirstRunShort::Nothing, "");
        assert_eq!(nothing.title, PACKAGES_NOT_INSTALLED);
        assert_eq!(nothing.severity, Severity::Warn);
        assert!(nothing.detail.is_empty());
    }

    /// The announcement opens the row before any snapshot, keeps atpkg's size
    /// as its only detail, is busy (no fraction yet), and is live under the
    /// announcement cap.
    #[test]
    fn an_announcement_opens_the_row_before_any_snapshot() {
        let m = announced("installing 2 program(s) (about 1 GB)");
        assert_eq!(m.title, INSTALLING_ALAB);
        assert_eq!(m.detail, vec!["about 1 GB"]);
        assert!(!m.excerpt, "the title alone on the glass");
        assert_eq!(m.glyph.ch(), DOWN);
        assert_eq!(m.severity, Severity::Info);
        assert_eq!(
            m.hold,
            Hold::Live {
                stale_after: STALE_ANNOUNCE
            }
        );
        assert_eq!(m.key.as_deref(), Some(KEY_PASS));
        let meter = m.meter.as_ref().expect("busy: no bytes yet, no fraction");
        assert_eq!(meter.fill_permille, None, "no bytes yet: no fill");
        assert!(meter.busy, "work in flight moves");
        assert!(m.actions.is_empty(), "work in flight carries no capsule");
        assert_eq!(
            meter.stats, "2 programs \u{00b7} ~1 GB",
            "how big the wait is"
        );
        assert_eq!(
            announced(
                "installing 10 ALab program(s) over the network (about 3 GB on disk when finished)"
            )
            .meter
            .unwrap()
            .stats,
            "10 programs \u{00b7} ~3 GB",
            "terse: no clause on the glass"
        );
        assert_eq!(meter.load, Some(Load::Network));
        assert_eq!(meter.amount, None, "no ETA for a phase of a larger job");
        let unsized_pass = announced("installing 3 ALab program(s)");
        assert_eq!(
            unsized_pass.meter.as_ref().unwrap().stats,
            "3 programs",
            "the (s) of program(s) is not a size"
        );
        assert!(unsized_pass.detail.is_empty(), "no size: no line");
        let bare = announced("installing two programs");
        assert!(bare.detail.is_empty(), "no size in the sentence: no line");
        assert_eq!(bare.meter.map(|m| m.stats), Some(String::new()));
    }

    /// The first run is one row, every outcome a record: the announcement and
    /// meter share the pass key, so the meter supersedes the announcement; the
    /// outcome, managed-current and machine-settings words are `Hold::LogOnly`
    /// records, and the one live row stays the meter.
    #[test]
    fn the_first_run_is_one_row_and_every_outcome_is_a_record() {
        let now = Instant::now();
        let stamp = WallStamp { unix_ms: 1 };
        let mut center = MessageCenter::new(MessageLog::empty(), now);
        let a = center.post(announced("installing (about 1 GB)"), stamp, now);
        let (live, _) = live_of(snapshot_words(Some(&snap(true)), None, false));
        let b = center.post(live, stamp, now);
        assert_eq!(
            b.outcome,
            aterm_messages::PostOutcome::Superseded(a.id),
            "the meter takes the announcement's slot"
        );
        let records = [
            installed("\u{2713} ALab toolchain installed: ay, trust"),
            ended("the pass finished", false),
            failed("install failed \u{2014} see Settings \u{25b8} Packages"),
            deferred("an earlier toolchain pass is still running"),
            not_installed("ay: the signature did not verify"),
            managed_current(
                "claude 2.1.280 (Anthropic latest)",
                0,
                true,
                HookDialect::Zsh,
            )
            .unwrap(),
        ]
        .into_iter()
        .chain(machine_settings("universal-control disabled"));
        for m in records {
            assert_eq!(m.hold, Hold::LogOnly, "{}: a record, never a row", m.title);
            let posted = center.post(m, stamp, now);
            assert!(center.live(posted.id).is_none(), "never live");
        }
        assert_eq!(center.live_rows().count(), 1, "the meter alone is live");
        assert_eq!(center.live_rows().next().map(|l| l.id), Some(b.id));
        assert_eq!(center.log().len(), 9, "every report is on record");
    }

    /// A wait that ran out is an unkeyed deferred record (Info, "ALab tools
    /// update postponed", atpkg's sentence), never "failed", never a row.
    #[test]
    fn a_lock_wait_timeout_is_a_deferred_record_not_a_failure() {
        let m = deferred(
            "waiting for an earlier package update to finish \u{2014} trying again when it finishes or in 30 s",
        );
        assert_eq!(m.severity, Severity::Info);
        assert_eq!(m.glyph.ch(), NOTED, "a record's mark (ruling 302)");
        assert_eq!(m.hold, Hold::LogOnly);
        assert_eq!(m.key, None, "unkeyed");
        assert!(!m.detail[0].contains("failed"), "{}", m.detail[0]);
        assert!(m.detail[0].contains("trying again when it finishes or in 30 s"));
    }

    /// A pass that left no ALab tools and saw no failure (`seed-nothing:`, an
    /// exclusion's `seed-unusable:`) is "Couldn't install any ALab tools": Warn,
    /// a record keyed to the pass with the Packages capsule, never titled
    /// "failed".
    #[test]
    fn a_pass_that_left_no_tools_is_never_titled_failed() {
        let m = not_installed("ay: the signature did not verify");
        assert_eq!(m.title, PACKAGES_NOT_INSTALLED);
        assert_eq!(m.severity, Severity::Warn);
        assert_eq!(m.hold, Hold::LogOnly);
        assert_eq!(m.key.as_deref(), Some(KEY_PASS));
        assert_eq!(m.actions, vec![packages()]);
        assert!(!m.title.contains("failed"), "{}", m.title);
        assert_eq!(m.detail, vec!["ay: the signature did not verify"]);
        assert!(not_installed("").detail.is_empty(), "no echo of the title");
        let excluded = unusable(
            "[packages].exclude narrows the toolset to nothing installable on this machine \
             (aarch64-apple-darwin; excluded: ay, clean) \u{2014} shorten it to receive the toolset",
        );
        assert_eq!(excluded.title, PACKAGES_NOT_INSTALLED, "a line to shorten");
        assert_eq!(excluded.severity, Severity::Warn);
    }

    /// A `seed-unusable:` stating a fact about the machine (nothing published
    /// for its architecture, every program removed on purpose) claims no failed
    /// attempt: an Info record, "No ALab tools to install", atpkg's sentence
    /// whole.
    #[test]
    fn an_informational_unusable_is_an_info_record_not_a_failure() {
        for body in [
            "ALab publishes no build for this machine's architecture (x86_64-apple-darwin) \
             \u{2014} it builds for aarch64-apple-darwin; nothing installs here until one is published",
            "every ALab program available here was removed on this machine (ay, trust) \u{2014} \
             nothing to install. `aterm pkg install <program>` brings one back",
        ] {
            let m = unusable(body);
            assert_eq!(m.severity, Severity::Info, "{body}");
            assert_eq!(m.glyph.ch(), NOTED, "a record's mark");
            assert_eq!(m.hold, Hold::LogOnly);
            assert_eq!(m.key.as_deref(), Some(KEY_PASS));
            assert_eq!(m.detail.join(" "), body, "atpkg's sentence, whole");
            assert!(!m.title.contains("Couldn't"), "{}", m.title);
        }
    }

    /// The positive terminals are records with no meter and drop the pill's
    /// own opening from the detail.
    #[test]
    fn the_positive_terminals_carry_no_meter_and_keep_the_sentence() {
        let m = installed(
            "\u{2713} ALab toolchain installed: claude, codex \u{2014} ready in every aterm tab",
        );
        assert_eq!(
            m.detail,
            vec!["claude, codex \u{2014} ready in every aterm tab"],
            "the pill's own opening is dropped"
        );
        assert_eq!(m.severity, Severity::Success);
        assert_eq!(m.meter, None, "a terminal Success record carries no meter");
        assert_eq!(m.hold, Hold::LogOnly, "no success row (2026-09-22)");
        let e = ended("the pass finished; 12 ALab program(s) are installed", false);
        assert_ne!(e.title, m.title, "no claim the pass changed anything");
        assert_eq!(
            ended("the pass finished; 12 ALab program(s) are installed", true).title,
            m.title,
            "a first run was an install"
        );
        assert_eq!(
            e.detail,
            vec!["the pass finished; 12 ALab program(s) are installed"]
        );
        assert_eq!(e.meter, None, "a terminal Success record carries no meter");
        assert_eq!(e.hold, Hold::LogOnly);
        assert!(
            ended("", false).detail.is_empty(),
            "no words from atpkg: the title alone"
        );
        let long = format!("the pass finished; {} are installed", "program ".repeat(30));
        for m in [installed(&long), ended(&long, false), deferred(&long)] {
            let rejoined = m.detail.join(" ");
            let words = |t: &str| {
                t.split_whitespace()
                    .map(|w| w.trim_end_matches(';').to_string())
                    .collect::<Vec<_>>()
            };
            assert_eq!(words(&rejoined), words(&long), "{}", m.title);
            assert!(!rejoined.contains('\u{2026}'), "{rejoined}");
        }
    }

    /// A failure is a Warn record with no meter: "Couldn't update ALab tools",
    /// then the host's whole sentence (`failure_cause`), control-stripped and
    /// kept whole; its surface is the Settings ▸ Packages badge, never a row.
    #[test]
    fn a_failure_is_a_warn_record_with_no_meter() {
        let m = failed("partly installed: ay, trust");
        assert_eq!(m.detail, vec!["partly installed: ay, trust"]);
        assert_eq!(m.meter, None);
        assert_eq!(m.severity, Severity::Warn);
        assert_eq!(m.hold, Hold::LogOnly);
        assert_eq!(m.key.as_deref(), Some(KEY_PASS));
        let hostile = failed("bad\u{1b}[31m thing\u{7}");
        let d = &hostile.detail[0];
        assert!(!d.contains('\u{1b}') && !d.contains('\u{7}'), "{d:?}");
        let long = format!("the child died after announcing: {}", "x".repeat(170));
        let m = failed(&long);
        assert!(m.detail[0].chars().count() > 160, "{}", m.detail[0]);
        assert_eq!(m.detail, vec![long]);
        let m = failed("error: tar exited 2\nno space left on device");
        assert_eq!(
            m.detail,
            vec!["error: tar exited 2", "no space left on device"]
        );
        let cause = format!(
            "net-failed: claude: download failed: {}\u{2014} HTTP 403 from the mirror",
            "the release mirror refused the request ".repeat(8)
        );
        assert!(cause.chars().count() > DETAIL_LINE_CAP);
        let m = failed(&cause);
        assert!(m.detail.len() > 1, "{:?}", m.detail);
        assert!(
            m.detail
                .iter()
                .all(|l| l.chars().count() <= DETAIL_LINE_CAP)
        );
        assert!(
            m.detail
                .last()
                .is_some_and(|l| l.ends_with("HTTP 403 from the mirror")),
            "{:?}",
            m.detail
        );
        assert_eq!(m.detail.join(" "), cause, "every word, in order");
    }

    #[test]
    fn bytes_read_like_a_download_dialog() {
        assert_eq!(fmt_bytes(0), "0 B");
        assert_eq!(fmt_bytes(812_000), "812 KB");
        assert_eq!(fmt_bytes(512_000_000), "512 MB");
        assert_eq!(fmt_bytes(1_200_000_000), "1.2 GB");
        assert_eq!(fmt_bytes(999_600_000), "1.0 GB", "never `1000 MB`");
        assert_eq!(fmt_bytes(999_700), "1 MB", "never `1000 KB`");
        assert_eq!(fmt_bytes(999_400_000), "999 MB");
        assert_eq!(fill_permille(0, 0), None);
        assert_eq!(fill_permille(5, 10), Some(500));
        assert_eq!(fill_permille(20, 10), Some(1000), "clamped to the total");
    }

    #[test]
    fn a_running_snapshot_reports_the_current_program_and_the_rollup() {
        let (m, pass_id) = live_of(snapshot_words(Some(&snap(true)), None, false));
        assert_eq!(m.title, INSTALLING_ALAB);
        assert_eq!(
            m.detail,
            vec!["trust \u{00b7} extracting", "512 MB / 1.2 GB downloaded"],
            "program and phase behind Details, and the download rollup with them"
        );
        assert!(!m.excerpt);
        assert!(m.actions.is_empty(), "work in flight carries no capsule");
        assert!(
            !m.meter.as_ref().is_some_and(|x| x.busy),
            "a fill is not busy"
        );
        assert_eq!(
            stats_of(&m),
            "3 of 10 programs",
            "the one count that agrees with the bar"
        );
        assert_eq!(
            m.meter.as_ref().and_then(|x| x.load),
            Some(Load::Disk),
            "an extraction loads the disk"
        );
        assert_eq!(
            m.meter.as_ref().and_then(|x| x.amount),
            Some(Amount {
                series: Amount::series_of("toolchain net 1700000000"),
                done: 356,
                total: 1000,
                unit: Unit::Steps,
            }),
            "the fill is the amount: the ETA is the whole pass's own rate"
        );
        for phase in [Phase::Verify, Phase::Link] {
            let mut held = snap(true);
            held.file.programs.get_mut("trust").unwrap().phase = phase;
            let (h, _) = live_of(snapshot_words(Some(&held), None, false));
            let amount = h.meter.as_ref().and_then(|x| x.amount).unwrap();
            assert_eq!(amount.unit, Unit::HeldSteps, "{phase:?}");
            assert_eq!(
                amount.series,
                Amount::series_of("toolchain net 1700000000"),
                "the same series: the estimator keeps its readings"
            );
        }
        let (routine, _) = live_of(snapshot_words(Some(&snap(true)), None, true));
        assert_eq!(routine.title, UPDATING_ALAB);
        assert_eq!(
            fill_of(&m),
            Some(356),
            "three of ten done, trust half-way through its phases (download, then 120 of 900 MB \
             extracted): (3000 + 500 + 65) / 10 — not the download rollup's 427"
        );
        assert_eq!(
            m.hold,
            Hold::Live {
                stale_after: STALE_TAILED
            }
        );
        assert_eq!(m.key.as_deref(), Some(KEY_PASS));
        assert_eq!(pass_id, ("net".to_string(), 1_700_000_000));
    }

    /// A pass whose byte rollup is silent (no byte total) still fills by
    /// programs weighted by phase; every first-run pass, whatever its name, is
    /// titled [`INSTALLING_ALAB`].
    #[test]
    fn a_silent_rollup_changes_nothing_the_programs_measure() {
        let mut f = file(Some(7), "net");
        f.overall.bytes_total = 0;
        f.overall.bytes_done = 0;
        let (m, _) = live_of(snapshot_words(
            Some(&crate::PkgProgressSnapshot {
                file: f.clone(),
                running: true,
            }),
            None,
            false,
        ));
        assert_eq!(m.title, INSTALLING_ALAB);
        assert_eq!(
            fill_of(&m),
            Some(356),
            "programs by phase: a silent byte rollup changes nothing"
        );
        assert_eq!(stats_of(&m), "3 of 10 programs");
        f.pass = "seed".to_string();
        let (m, _) = live_of(snapshot_words(
            Some(&crate::PkgProgressSnapshot {
                file: f,
                running: true,
            }),
            None,
            false,
        ));
        assert_eq!(
            m.title, INSTALLING_ALAB,
            "one noun for every first-run pass"
        );
    }

    /// A live meter never runs backwards within one pass: the clamp is to the
    /// same pass's high-water mark, and a different pass starts fresh.
    #[test]
    fn a_live_meter_never_runs_backwards_within_a_pass() {
        let (first, pass_id) = live_of(snapshot_words(Some(&snap(true)), None, false));
        let peak = (pass_id, fill_of(&first).unwrap());
        let mut dip = file(Some(7), "net");
        dip.overall.bytes_done = 100_000_000;
        dip.programs.get_mut("trust").unwrap().bytes_done = 0;
        let (m, _) = live_of(snapshot_words(
            Some(&crate::PkgProgressSnapshot {
                file: dip.clone(),
                running: true,
            }),
            Some(&peak),
            false,
        ));
        assert_eq!(fill_of(&m), Some(356), "clamped to the pass's own peak");
        assert_eq!(stats_of(&m), "3 of 10 programs");
        assert_eq!(
            m.detail.get(1).map(String::as_str),
            Some("100 MB / 1.2 GB downloaded"),
            "the figures are live"
        );
        dip.started_unix += 1;
        let (m, _) = live_of(snapshot_words(
            Some(&crate::PkgProgressSnapshot {
                file: dip,
                running: true,
            }),
            Some(&peak),
            false,
        ));
        assert_eq!(fill_of(&m), Some(350), "a new pass starts fresh");
    }

    /// A pass that is over claims no outcome: a read of a pass that ended
    /// (cleanly, with a failure, or with its writer gone) is `Over` whatever
    /// the file says, and the outcome is the markers' record.
    #[test]
    fn a_pass_that_is_over_claims_no_outcome() {
        let mut f = file(None, "net");
        f.ended_unix = Some(1_700_000_100);
        let over = |f: &atpkg::progress::ProgressFile| {
            snapshot_words(
                Some(&crate::PkgProgressSnapshot {
                    file: f.clone(),
                    running: false,
                }),
                Some(&(("net".to_string(), 1_700_000_000), 427)),
                false,
            )
        };
        assert_eq!(over(&f), SnapshotWords::Over, "ended cleanly");
        f.programs.get_mut("trust").unwrap().phase = Phase::Failed;
        assert_eq!(over(&f), SnapshotWords::Over, "ended with a failure");
        f.ended_unix = None;
        assert_eq!(over(&f), SnapshotWords::Over, "the writer is gone");
        let mut newer = f.clone();
        newer.v = PROGRESS_VERSION + 1;
        assert_eq!(over(&newer), SnapshotWords::Over, "any schema");
        assert_eq!(snapshot_words(None, None, false), SnapshotWords::Vanished);
        let mut nothing = file(Some(7), "net");
        nothing.overall.programs_total = 0;
        nothing.programs.clear();
        nothing.queue.clear();
        assert_eq!(
            snapshot_words(
                Some(&crate::PkgProgressSnapshot {
                    file: nothing.clone(),
                    running: true
                }),
                None,
                false
            ),
            SnapshotWords::Quiet
        );
        assert_eq!(
            snapshot_words(
                Some(&crate::PkgProgressSnapshot {
                    file: nothing,
                    running: false
                }),
                None,
                false
            ),
            SnapshotWords::Over
        );
        let mut newer = file(Some(7), "net");
        newer.v = PROGRESS_VERSION + 1;
        let (m, _) = live_of(snapshot_words(
            Some(&crate::PkgProgressSnapshot {
                file: newer,
                running: true,
            }),
            None,
            false,
        ));
        assert_eq!(m.title, INSTALLING_ALAB, "the title alone");
        assert!(m.detail.is_empty(), "{:?}", m.detail);
        assert!(m.meter.as_ref().is_some_and(|m| m.busy));
    }

    /// The phase words say "up to date" for a skipped program, and why a
    /// program was pulled forward (scheduling, not progress) stays off the row.
    #[test]
    fn phase_words_say_up_to_date_and_keep_scheduling_off_the_row() {
        let mut f = file(Some(7), "net");
        for row in f.programs.values_mut() {
            row.bumped = true;
        }
        let (m, _) = live_of(snapshot_words(
            Some(&crate::PkgProgressSnapshot {
                file: f.clone(),
                running: true,
            }),
            None,
            false,
        ));
        assert!(
            !m.detail
                .iter()
                .any(|d| d.contains("bumped") || d.contains("you asked")),
            "{:?}",
            m.detail
        );
        let mut skipped = file(Some(7), "net");
        let name = skipped.queue[0].clone();
        for (key, row) in skipped.programs.iter_mut() {
            row.phase = if *key == name {
                Phase::Skipped
            } else {
                Phase::Done
            };
        }
        let (line, _) = current_program_line(&skipped);
        assert_eq!(line, format!("{name} \u{00b7} up to date"));
    }

    /// An archive atpkg fetched ahead waits at its whole size for the pass: the
    /// row names the program the pass is extracting, not the finished download.
    #[test]
    fn a_download_waiting_for_the_pass_yields_to_the_program_in_hand() {
        let program = |phase, bytes_done, bytes_total| atpkg::progress::ProgramProgress {
            phase,
            bytes_done,
            bytes_total,
            build: None,
            bumped: false,
            error: None,
        };
        let mut f = file(Some(7), "net");
        f.programs = BTreeMap::from([
            ("ay".to_string(), program(Phase::Download, 9, 9)),
            ("trust".to_string(), program(Phase::Extract, 1, 4)),
        ]);
        assert_eq!(current_program_line(&f).0, "trust \u{00b7} extracting");
        f.programs
            .insert("ty".to_string(), program(Phase::Download, 3, 9));
        assert_eq!(current_program_line(&f).0, "ty \u{00b7} downloading");
    }

    #[test]
    fn hostile_strings_are_sanitized_before_they_become_words() {
        let mut f = file(Some(7), "net");
        f.programs.clear();
        f.queue = vec!["../evil".into()];
        f.programs.insert(
            "../evil".into(),
            atpkg::progress::ProgramProgress {
                phase: Phase::Download,
                bytes_done: 1,
                bytes_total: 2,
                build: None,
                bumped: false,
                error: None,
            },
        );
        let (m, _) = live_of(snapshot_words(
            Some(&crate::PkgProgressSnapshot {
                file: f,
                running: true,
            }),
            None,
            false,
        ));
        assert_eq!(
            m.detail,
            ["512 MB / 1.2 GB downloaded"],
            "no program line: only the byte rollup, which carries no name"
        );
        for m in [
            announced("installing (bad\u{1b}[31m size\u{7})"),
            failed("bad\u{1b}[31m thing\u{7}"),
        ] {
            let d = &m.detail[0];
            assert!(!d.contains('\u{1b}') && !d.contains('\u{7}'), "{d:?}");
        }
        let n = appnotice(tags::TOOLCHAIN, "hello\u{1b}[2J from the terminal");
        assert_eq!(n.title, "hello[2J from the terminal", "sanitized");
        assert_eq!(n.severity, Severity::Info);
        assert_eq!(n.hold, Hold::LogOnly, "free text is a record");
        assert_eq!(n.key, None);
        assert!(n.actions.is_empty(), "a wire notice authors no capsule");
        assert_eq!(appnotice(tags::UPDATE, "x").tag, tags::UPDATE);
    }

    /// A program the pass could not check is named, never left out: the title
    /// says it was not checked and the mark is a plain record's.
    #[test]
    fn a_program_the_pass_could_not_check_is_named_and_no_success() {
        let words = |wire: &str| managed_current(wire, 0, true, HookDialect::Zsh).unwrap();
        let m = words("claude 2.1.287 (not checked); codex 0.160.0 (OpenAI latest)");
        assert!(
            m.title.contains("Claude Code 2.1.287 not checked") && m.title.contains("Codex CLI"),
            "both programs are named: {}",
            m.title
        );
        assert_eq!(m.severity, Severity::Info, "no success");
        assert!(!m.detail[0].contains("doctor"), "{:?}", m.detail);
        let m = words("claude 2.1.287 (failed); codex 0.160.0 (held)");
        assert!(
            m.title.contains("failed") && m.title.contains("held"),
            "{}",
            m.title
        );
        assert!(m.detail[0].contains("`aterm pkg doctor`"), "{:?}", m.detail);
        let m = words("claude 2.1.287 (not checked); codex 0.160.0 (held)");
        assert!(
            m.detail[0].contains("`aterm pkg doctor`"),
            "doctor is named for the held one: {:?}",
            m.detail
        );
        assert_eq!(
            managed_version("claude 2.1.287 (not checked)", "claude").as_deref(),
            Some("2.1.287"),
            "its build is still the one the rows are counted against"
        );
        let m = words("claude 2.1.287 (Anthropic latest); codex 0.160.0 (OpenAI latest)");
        assert_eq!(m.severity, Severity::Success);
    }

    /// Four agents fit the title whole: programs that share a word are grouped,
    /// and a title still over the cap drops the versions of the programs not
    /// current before anything is cut.
    #[test]
    fn four_agents_fit_the_title_whole() {
        let title = |[a, b, c, d]: [&str; 4]| {
            let wire = format!(
                "claude 2.1.287 ({a}); codex 0.160.0 ({b}); agy 1.3.6 ({c}); muse 1.4.9-R6297.1 ({d})"
            );
            let title = managed_current(&wire, 0, true, HookDialect::Zsh)
                .unwrap()
                .title;
            assert!(
                title.chars().count() <= TITLE_CAP && !title.contains('\u{2026}'),
                "{title}"
            );
            title
        };
        title(["not checked"; 4]);
        title(["failed"; 4]);
        title(["Anthropic latest", "not checked", "held", "failed"]);
        title(["Vendor latest"; 4]);
    }

    #[test]
    fn the_managed_and_machine_records_carry_their_words() {
        let m = managed_current(
            "claude 2.1.267 (build 2026091001)",
            0,
            true,
            HookDialect::Zsh,
        )
        .unwrap();
        assert!(m.title.contains("2.1.267"), "{}", m.title);
        assert!(
            !m.detail[0].contains("2026091001"),
            "an older atpkg's wire still parses, its build unrepeated: {:?}",
            m.detail
        );
        assert_eq!(m.hold, Hold::LogOnly, "a record on every pass, never a row");
        assert_eq!(m.meter, None, "a record carries no meter");
        assert_eq!(m.severity, Severity::Success);
        assert_eq!(m.key.as_deref(), Some(KEY_MANAGED));

        let three = managed_current(
            "claude 2.1.280 (Anthropic latest); codex 0.156.1 (OpenAI latest); gemini 1.0",
            0,
            true,
            HookDialect::Zsh,
        )
        .unwrap();
        for name in ["2.1.280", "0.156.1", "gemini 1.0"] {
            assert!(three.title.contains(name), "{name}: {}", three.title);
        }
        assert!(!three.title.contains("latest") && !three.detail[0].contains("latest"));
        assert!(
            managed_current("  ;  ", 0, true, HookDialect::Zsh).is_none(),
            "an empty body records nothing"
        );

        let rows = machine_settings("universal-control disabled");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].severity, Severity::Info);
        assert_eq!(
            rows[0].glyph.ch(),
            NOTED,
            "from the band's closed set — ⚙ rendered blank headless"
        );
        assert_eq!(rows[0].hold, Hold::LogOnly);
        assert_eq!(
            rows[0].key.as_deref(),
            Some("toolchain.machine.universal-control")
        );
    }

    /// The record says what is true of the running sessions: sessions the live
    /// upgrade is moving turn "up to date" into "installed" and say when; a
    /// frozen tab with an agent in front is told to source the hook once the
    /// agent exits, never handed a line to type; the shell remedy stays last.
    #[test]
    fn the_managed_record_says_installed_while_sessions_lag_and_spares_agent_tabs() {
        let wire = "claude 2.1.282 (Anthropic latest); codex 0.157.0 (OpenAI latest)";
        let words = |tabs: TabsBehind| {
            managed_current_words_for(wire, tabs, None, HookDialect::Zsh).unwrap()
        };
        let remedy = HookDialect::Zsh.remedy().expect("zsh has a hook");
        let (current, plain) = words(TabsBehind::default());
        let (moving, detail) = words(TabsBehind {
            agents_behind: PerAgent::of(&[(Agent::Claude, 2)]),
            ..TabsBehind::default()
        });
        assert_ne!(
            moving, current,
            "lagging sessions turn current into installed"
        );
        assert!(detail.contains('2'), "{detail}");
        let (_, detail) = words(TabsBehind {
            frozen_agents: 1,
            ..TabsBehind::default()
        });
        assert!(detail.contains(&remedy), "{detail}");
        assert!(
            !detail.ends_with(&remedy),
            "an agent tab is told to wait: {detail}"
        );
        assert!(!detail.contains("every tab"), "{detail}");
        let (title, detail) = words(TabsBehind {
            frozen_shells: 1,
            frozen_agents: 1,
            agents_behind: PerAgent::of(&[(Agent::Claude, 1)]),
            ..TabsBehind::default()
        });
        assert_eq!(title, moving);
        assert!(
            detail.ends_with(&remedy),
            "the shell remedy is said last: {detail}"
        );
        assert_eq!(plain, "ready in every aterm tab");
    }

    /// ONE COUNT PER AGENT, BY [`Agent::ALL`]: each agent counted lands on its
    /// own product and no other's, said in `ALL`'s order (Claude Code first),
    /// and the total is every count.
    #[test]
    fn each_agent_is_counted_on_its_own_product_in_alls_order() {
        let mut each = PerAgent::default();
        for (n, agent) in Agent::ALL.into_iter().enumerate() {
            for _ in 0..=n {
                each.count(agent);
            }
        }
        let said: Vec<(&str, usize)> = each.each().collect();
        let expected: Vec<(&str, usize)> = Agent::ALL
            .into_iter()
            .enumerate()
            .map(|(n, agent)| (agent.product(), n + 1))
            .collect();
        assert_eq!(said, expected);
        assert_eq!(said[0].0, "Claude Code");
        assert_eq!(each.total(), expected.iter().map(|(_, n)| n).sum::<usize>());
        assert_eq!(PerAgent::default().total(), 0, "the negative control");
    }

    /// EACH PRODUCT IS SAID WITH ITS OWN COUNT: the words named every lagging
    /// session "Claude Code", so a Codex session behind the managed codex would
    /// have read as a Claude Code one.
    #[test]
    fn each_lagging_product_is_named_with_its_own_count() {
        let wire = "claude 2.1.282 (Anthropic latest); codex 0.157.0 (OpenAI latest)";
        let words = |tabs: TabsBehind| {
            managed_current_words_for(wire, tabs, None, HookDialect::Zsh).unwrap()
        };
        let (title, detail) = words(TabsBehind {
            agents_behind: PerAgent::of(&[(Agent::Claude, 1), (Agent::Codex, 2)]),
            agents_held: PerAgent::of(&[(Agent::Codex, 1)]),
            ..TabsBehind::default()
        });
        assert_eq!(title, "Claude Code 2.1.282 and Codex CLI 0.157.0 installed");
        assert_eq!(
            detail,
            "ready in every aterm tab · 1 running Claude Code session moves onto it on its own · \
             2 running Codex sessions move onto it on their own · 1 running Codex session is \
             still on the old build (`aterm harness upgrade --status` says why)"
        );
        let (title, detail) = words(TabsBehind {
            agents_behind: PerAgent::of(&[(Agent::Codex, 1)]),
            ..TabsBehind::default()
        });
        assert_eq!(title, "Claude Code 2.1.282 and Codex CLI 0.157.0 installed");
        assert_eq!(
            detail,
            "ready in every aterm tab · 1 running Codex session moves onto it on its own"
        );
        let (_, detail) = words(TabsBehind {
            agents_behind: PerAgent::of(&[(Agent::Claude, 1)]),
            ..TabsBehind::default()
        });
        assert!(!detail.contains("Codex"), "{detail}");
    }

    /// A session that will not move on its own is not said to move: it keeps
    /// the title at "installed" and is pointed at `--status`.
    #[test]
    fn a_held_session_is_not_said_to_move_and_a_shadowed_tab_is_not_from_before() {
        let wire = "claude 2.1.282 (Anthropic latest)";
        let words = |tabs: TabsBehind| {
            managed_current_words_for(wire, tabs, None, HookDialect::Zsh).unwrap()
        };
        let (current, _) = words(TabsBehind::default());
        let (title, detail) = words(TabsBehind {
            agents_held: PerAgent::of(&[(Agent::Claude, 1)]),
            ..TabsBehind::default()
        });
        assert_ne!(
            title, current,
            "a held session keeps the title at installed"
        );
        assert!(
            detail.contains("`aterm harness upgrade --status`"),
            "{detail}"
        );
        let (title, detail) = words(TabsBehind {
            shadowed: 2,
            ..TabsBehind::default()
        });
        assert_eq!(title, current);
        assert!(!detail.contains("every aterm tab"), "{detail}");
        assert!(!detail.contains("00-atpkg"), "no hook to type: {detail}");
        assert_eq!(
            managed_version("claude 2.1.282 (Anthropic latest); codex 0.157.0", "claude")
                .as_deref(),
            Some("2.1.282")
        );
        assert_eq!(
            managed_version("codex 0.157.0 (OpenAI latest)", "claude"),
            None
        );
    }

    /// The remedy is spelled for the shell this window spawns: atpkg writes one
    /// hook per dialect (`atpkg::hooks::hook_files`: `.zsh`, `.bash`, `.fish`,
    /// `.ps1`), sourced the way that shell sources, and a shell atpkg has no hook
    /// for is told a new tab.
    #[test]
    fn the_frozen_tab_remedy_is_the_hook_in_the_spawn_shells_dialect() {
        use aterm_core::shell_integration::ShellType;
        let wire = "claude 2.1.273 (build 2026091601)";
        let note = |shell: HookDialect| {
            let (_, detail) = managed_current_words(wire, 1, true, shell).unwrap();
            detail.split(" \u{00b7} ").last().unwrap().to_string()
        };
        assert_eq!(
            note(HookDialect::Zsh),
            "1 tab from before this update picks them up with `. ~/.aterm/shell.d/00-atpkg.zsh`"
        );
        assert_eq!(
            note(HookDialect::Bash),
            "1 tab from before this update picks them up with `. ~/.aterm/shell.d/00-atpkg.bash`"
        );
        assert_eq!(
            note(HookDialect::Fish),
            "1 tab from before this update picks them up with `source ~/.aterm/shell.d/00-atpkg.fish`"
        );
        assert_eq!(
            note(HookDialect::PowerShell),
            "1 tab from before this update picks them up with `. ~/.aterm/shell.d/00-atpkg.ps1`"
        );
        assert_eq!(
            note(HookDialect::None),
            "1 tab from before this update: a new tab picks them up"
        );
        assert_eq!(HookDialect::default(), HookDialect::Zsh);
        assert_eq!(HookDialect::from(ShellType::Bash), HookDialect::Bash);
        assert_eq!(HookDialect::from(ShellType::Fish), HookDialect::Fish);
        assert_eq!(
            HookDialect::from(ShellType::PowerShell),
            HookDialect::PowerShell
        );
        assert_eq!(HookDialect::from(ShellType::Unknown), HookDialect::None);
        assert_eq!(HookDialect::from(ShellType::Cmd), HookDialect::None);
        assert_eq!(HookDialect::from(ShellType::Wsl), HookDialect::None);
    }

    /// "Every tab" rests on the installed record's evidence: a window whose
    /// shell integration failed is told that record's cause instead.
    #[test]
    fn the_managed_row_stops_claiming_this_tab_when_integration_failed() {
        let wire = "claude 2.1.273 (build 2026091601); codex 0.154.0 (build 2026091001)";
        let (_, detail) = managed_current_words(wire, 0, false, HookDialect::Zsh).unwrap();
        assert!(!detail.contains("every"), "{detail}");
        let (_, detail) = managed_current_words(wire, 1, false, HookDialect::Zsh).unwrap();
        assert!(!detail.contains("every"), "{detail}");
        assert!(
            detail.ends_with(&HookDialect::Zsh.remedy().expect("zsh has a hook")),
            "{detail}"
        );
        let (_, detail) = managed_current_words(wire, 0, true, HookDialect::Zsh).unwrap();
        assert_eq!(detail, "ready in every aterm tab");
    }

    /// The seed pill over every roster shape × integration outcome × frozen-tab
    /// count × hook dialect:
    #[test]
    fn the_seed_pill_keeps_every_tab_promise_it_makes() {
        use crate::spawn::ShellIntegrationOutcome as Si;
        const PREFIX: &str = "\u{2713} ALab toolchain installed: ";
        let names = |list: &[&str]| list.iter().map(|n| (*n).to_string()).collect::<Vec<_>>();
        let rosters = [
            names(&["ay"]),
            names(&["ay", "trust"]),
            names(&["ay", "clean", "ny", "trust", "ty"]),
            names(&["ay", "clean", "ny", "trust", "ty", "nn"]),
            names(&["claude"]),
            names(&["claude", "codex"]),
            names(&["ay", "claude", "trust"]),
        ];
        let outcomes = [
            None,
            Some(Si::Prepared),
            Some(Si::WriteFailed("read-only cache".into())),
            Some(Si::UnknownShell("nu".into())),
        ];
        let dialects = [
            HookDialect::Zsh,
            HookDialect::Bash,
            HookDialect::Fish,
            HookDialect::PowerShell,
            HookDialect::None,
        ];
        let mut checked = 0;
        for roster in &rosters {
            let shown = if roster.len() > 5 {
                format!("{}\u{2026}", roster[..5].join(", "))
            } else {
                roster.join(", ")
            };
            let agents_only = roster.iter().all(|n| atpkg::stub::is_agent_program(n));
            for outcome in &outcomes {
                let failed = matches!(outcome, Some(Si::WriteFailed(_) | Si::UnknownShell(_)));
                for frozen in [0usize, 1, 2] {
                    for hook in dialects {
                        let pill = seed_pill_text(roster, outcome.as_ref(), frozen, hook);
                        let case =
                            format!("{roster:?} {outcome:?} frozen={frozen} {hook:?}: {pill}");
                        checked += 1;
                        assert!(
                            pill.starts_with(&format!("{PREFIX}{shown} \u{2014} ")),
                            "{case}"
                        );
                        let lower = pill.to_lowercase();
                        assert!(
                            !lower.contains("exec") && !lower.contains("restart"),
                            "{case}"
                        );
                        if failed {
                            let clause = unhooked_clause(outcome.as_ref()).expect("a cause");
                            assert!(pill.ends_with(&format!(" \u{2014} {clause}")), "{case}");
                            assert!(!pill.contains("ready in"), "{case}");
                            continue;
                        }
                        if frozen == 0 {
                            assert!(
                                pill.ends_with(" \u{2014} ready in every aterm tab"),
                                "{case}"
                            );
                            continue;
                        }
                        assert!(
                            pill.contains(" \u{2014} ready in every tab opened since this update"),
                            "{case}"
                        );
                        if agents_only {
                            assert!(!pill.contains(" \u{b7} "), "no remedy clause: {case}");
                            assert!(!lower.contains("new tab"), "{case}");
                            continue;
                        }
                        let tabs = if frozen == 1 { "1 tab" } else { "2 tabs" };
                        assert!(
                            pill.contains(&format!(" \u{b7} {tabs} from before it")),
                            "{case}"
                        );
                        let record = managed_current(
                            "claude 2.1.273 (build 2026091601)",
                            frozen,
                            true,
                            hook,
                        )
                        .expect("the managed record posts");
                        let record_note = record.detail[0]
                            .rsplit(" \u{00b7} ")
                            .next()
                            .expect("the note is the last clause")
                            .to_string();
                        match hook.remedy() {
                            Some(command) => {
                                let verb = if frozen == 1 { "picks" } else { "pick" };
                                let tail = format!(" {verb} them up with {command}");
                                assert!(pill.ends_with(&tail), "{case}");
                                assert!(record_note.ends_with(&tail), "{hook:?}: {record_note}");
                                assert!(!lower.contains("new tab"), "{case}");
                            }
                            None => {
                                let tail = ": a new tab picks them up";
                                assert!(pill.ends_with(tail), "{case}");
                                assert!(record_note.ends_with(tail), "{hook:?}: {record_note}");
                            }
                        }
                    }
                }
            }
        }
        assert_eq!(checked, rosters.len() * outcomes.len() * 3 * dialects.len());
        let tools = names(&["ay", "trust"]);
        let pill = seed_pill_text(&tools, None, 1, HookDialect::Zsh);
        let record = installed(&pill);
        assert_eq!(record.hold, Hold::LogOnly, "no success row");
        assert_eq!(
            Some(record.detail[0].as_str()),
            pill.strip_prefix(PREFIX),
            "the record keeps the pill whole after its opening"
        );
        assert!(
            HookDialect::Zsh
                .remedy()
                .unwrap()
                .contains(atpkg::hooks::HOOK_BASENAME)
        );
    }

    /// Machine settings: one record per changed item, in wire order.
    #[test]
    fn machine_settings_record_one_entry_per_item_in_wire_order() {
        let rows =
            machine_settings("older-thing done; something-new tuned; universal-control disabled");
        assert_eq!(
            rows[0].title, "older-thing done",
            "an unknown item, verbatim"
        );
        let keys: Vec<&str> = rows.iter().map(|m| m.key.as_deref().unwrap()).collect();
        assert_eq!(
            keys,
            [
                "toolchain.machine.older-thing",
                "toolchain.machine.something-new",
                "toolchain.machine.universal-control",
            ],
            "each item keyed on atpkg's own word for it"
        );
        for m in &rows {
            assert_eq!(m.hold, Hold::LogOnly);
            assert_eq!(m.severity, Severity::Info);
            assert_eq!(m.glyph.ch(), NOTED, "a record's mark (ruling 302)");
        }
        assert!(rows[1].detail.is_empty());
        assert!(
            rows[2].detail[0].ends_with(atpkg::machine::UNIVERSAL_CONTROL_REVERT),
            "the command whole, last"
        );
        assert_eq!(rows[2].actions, vec![security()], "where the switch is");
        assert!(
            machine_settings(" ; ").is_empty(),
            "an empty body raises nothing"
        );
    }

    /// A transfer's stats keep one width from its first megabyte to its last.
    #[test]
    fn byte_stats_keep_one_width_for_the_life_of_a_transfer() {
        for total in [200_000_000u64, 1_200_000_000, 74_000_000] {
            let width = byte_stats(total, total).chars().count();
            for done in (1_000_000..=total).step_by(997_000) {
                assert_eq!(
                    byte_stats(done, total).chars().count(),
                    width,
                    "{done} of {total}"
                );
            }
        }
        assert_eq!(
            byte_stats(900_000_000, 74_000_000),
            byte_stats(74_000_000, 74_000_000),
            "clamped"
        );
    }

    #[test]
    fn pass_load_names_the_heaviest_active_phase() {
        let program = |phase| atpkg::progress::ProgramProgress {
            phase,
            bytes_done: 0,
            bytes_total: 0,
            build: None,
            bumped: false,
            error: None,
        };
        let with = |phases: &[Phase]| {
            let mut f = file(Some(7), "net");
            f.programs = phases
                .iter()
                .enumerate()
                .map(|(i, p)| (format!("p{i}"), program(*p)))
                .collect();
            pass_load(&f)
        };
        assert_eq!(with(&[Phase::Download, Phase::Extract]), Some(Load::Disk));
        assert_eq!(with(&[Phase::Download, Phase::Link]), Some(Load::Disk));
        assert_eq!(with(&[Phase::Download, Phase::Verify]), Some(Load::Cpu));
        assert_eq!(with(&[Phase::Download, Phase::Queued]), Some(Load::Network));
        assert_eq!(
            with(&[Phase::Queued, Phase::Done]),
            None,
            "nothing in flight"
        );
    }
}
