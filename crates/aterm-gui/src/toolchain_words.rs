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

const ERROR_CAP: usize = 60;

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

const DOWN: char = '\u{21e3}';

const NOTED: char = '\u{2139}';

/// The pass name and start stamp that identify one `progress.json` writer, so a live meter is clamped to its own pass's high-water mark ([`snapshot_words`]).
pub(crate) type PassId = (String, u64);

fn security() -> Intent {
    Intent::OpenSettings {
        route: crate::native_settings::SettingsRoute::Security
            .path()
            .to_string(),
    }
}

fn packages() -> Intent {
    Intent::OpenSettings {
        route: crate::native_settings::SettingsRoute::Packages
            .path()
            .to_string(),
    }
}

fn glyph(ch: char) -> Glyph {
    Glyph::or_fallback(ch)
}

fn row(severity: Severity, title: impl AsRef<str>) -> Message {
    Message::new(tags::TOOLCHAIN, severity, title)
        .action(packages())
        .record()
}

/// A record's words rebuilt unclassified at the default hold, for a test that needs them live.
#[cfg(test)]
pub(crate) fn held_row(record: &Message) -> Message {
    let mut row = Message::new(record.tag.clone(), record.severity, record.title.clone());
    row.glyph = record.glyph;
    row.detail = record.detail.clone();
    row.actions = record.actions.clone();
    row.key = record.key.clone();
    row.finished = record.finished.clone();
    row
}

fn live_row(title: impl AsRef<str>, stale_after: Duration) -> Message {
    Message::new(tags::TOOLCHAIN, Severity::Info, title)
        .primary_load(Load::Network)
        .working(stale_after)
}

fn record(severity: Severity, title: impl AsRef<str>, detail: String) -> Message {
    row(severity, title).line(detail)
}

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
    let min_done_width = if total >= 99_500 {
        6
    } else if total >= 1_000 {
        5
    } else {
        0
    };
    let (done, total) = (fmt_bytes(done.min(total)), fmt_bytes(total));
    let width = total.chars().count().max(min_done_width);
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

fn unavailable(what: &str) -> Message {
    whole_record(Severity::Info, PACKAGES_UNAVAILABLE, what)
        .glyph(glyph(NOTED))
        .key(KEY_PASS)
}

fn bad_outcome(title: &str, what: &str) -> Message {
    row(Severity::Warn, title)
        .lines(whole_reason(what))
        .key(KEY_PASS)
}

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

/// A first run that ended short.
pub(crate) fn first_run_short(how: FirstRunShort, cause: &str) -> Message {
    let (severity, title) = match how {
        FirstRunShort::Failed => (Severity::Warn, ALAB_INSTALL_FAILED),
        FirstRunShort::Nothing => (Severity::Warn, PACKAGES_NOT_INSTALLED),
    };
    Message::new(tags::PACKAGES, severity, title)
        .action(packages())
        .key(KEY_PASS)
        .lines(whole_reason(cause))
}

/// A plain text posted from outside the process (`aterm ctl appnotice <lane> <text>`), e.g. an `aterm pkg install claude` run in a terminal.
pub(crate) fn appnotice(tag: Tag, text: &str) -> Message {
    let text = sanitize_for_tty(text, 120);
    match installed_words(&text) {
        Some(title) => Message::new(tag, Severity::Info, title).line(text).record(),
        None => Message::new(tag, Severity::Info, text).record(),
    }
}

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
    pub(crate) const fn count(&mut self, agent: aterm_agent::harness::upgrade::Agent) {
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

    fn total(self) -> usize {
        self.0.iter().sum()
    }

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

/// `a`, `a and b`, `a, b and c`.
pub(crate) fn and_list(words: &[String]) -> String {
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

fn admitted_name(raw: &str) -> Option<String> {
    let name = atpkg::store::ToolName::new(raw)?;
    Some(sanitize_for_tty(name.as_str(), NAME_CAP))
}

#[cfg(test)]
#[path = "toolchain_words_tests.rs"]
mod tests;
