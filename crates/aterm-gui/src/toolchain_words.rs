// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The toolchain lane's words: pure builders that return an
//! [`aterm_messages::Message`] for the message center to hold
//! (docs/DESIGN-unified-messages-2026-09-21.md). Nothing here keeps state,
//! reads a clock or paints a cell: the wake arms in `lib.rs` call a builder and
//! `post`/`restate`; the center ranks, holds, supersedes and logs; the band
//! paints.
//!
//! # What may be claimed
//!
//! * A snapshot renders only what [`crate::PkgProgressSnapshot`] supports (the
//!   classified read of `<prefix>/progress.json`): a not-running snapshot names
//!   its terminal outcome, never a live phase; an unknown schema `v` is the
//!   first-run title alone ("Installing ALab tools"), busy. Program names are
//!   untrusted until they round-trip [`atpkg::store::ToolName`]; error text is
//!   control-stripped and capped ([`atpkg::progress::sanitize_for_tty`]), and
//!   the center's `normalized` re-clips every field at ingress.
//! * Record titles name the outcome with the lane's one noun: "ALab tools
//!   update postponed", "ALab tools installed", "ALab tools checked",
//!   "Couldn't update ALab tools", "Couldn't install any ALab tools", "No ALab
//!   tools to install", and a first
//!   run's "Couldn't install ALab tools"; the first-run row is "Installing
//!   ALab tools".
//!
//! # Silent by default
//!
//! Every row that appears re-grids every window and resizes every running TUI,
//! so package updates stay off the glass except:
//!
//! * the first-run live row, `Installing ALab tools` ([`announced`], the live
//!   [`snapshot_words`]), while the default set is laid onto a machine whose
//!   store held none of it (multi-GB; silence would read as a hang). It opens
//!   once and folds at the child's exit;
//! * a heavy routine pass (≥ [`HEAVY_PASS_BYTES`]), `Updating ALab tools`
//!   (the host's `messages_host` decides which pass is heavy);
//! * a first run that ended short, a failure row ([`first_run_short`]);
//! * a Settings ▸ Packages verb a person pressed (Check, Install, Remove): its
//!   pass takes the row whatever it weighs
//!   (`message_reporters::packages_verb_row`, then this module's live words).
//!
//! Everything else — a light routine pass's announcement and meter, every
//! other pass outcome ([`installed`], [`ended`], [`failed`],
//! [`not_installed`]), [`managed_current`], [`machine_settings`], a
//! [`deferred`] pass, and every [`appnotice`] text (R34; ruling 147) — is a
//! `Hold::LogOnly` RECORD: `appstatus` lists it as a finished activity
//! with the words and outcome the bars' ledger gave it, Settings ▸ Messages
//! and `messages.log` keep it, and the glass never shows it. A routine
//! pass's failure is the Settings ▸ Packages badge (`packages_screen`). This
//! supersedes the design's §6 R25–R33 glass behaviour (the rulings on the
//! origin/main merge, 2026-09-23, and on the 2026-09-24 merge).
//!
//! # Keys
//!
//! Everything a pass says about itself (announced, the live meter, outcome
//! records) carries [`KEY_PASS`], so a newer live report supersedes the older
//! row in its slot; a record is never live, so its key only names it in the
//! log. The managed-current record and each machine-settings item carry keys
//! of their own.

use std::time::Duration;

use aterm_messages::text::shape_detail;
use aterm_messages::{
    Amount, DETAIL_LINE_CAP, Glyph, Intent, Load, Message, Meter, STALE_ANNOUNCE, STALE_TAILED,
    Severity, Tag, Unit, tags,
};
use atpkg::progress::{PROGRESS_VERSION, Phase, sanitize_for_tty};

/// The supersede key every report about one pass carries: announcement, live
/// meter, and the installed / ended / failed records.
pub(crate) const KEY_PASS: &str = "toolchain.pass";
/// The managed-current record's key.
pub(crate) const KEY_MANAGED: &str = "toolchain.managed";
/// A routine pass planned at least this many bytes is very heavy system use:
/// its live row, `Updating ALab tools`, reaches the glass after the progress
/// grace and folds at the child's exit, like a first run's. A lighter routine
/// pass stays silent.
pub(crate) const HEAVY_PASS_BYTES: u64 = 250_000_000;
/// The prefix of a machine-settings item's key: `toolchain.machine.`
/// plus the item's first word (`universal-control`, `spotlight-noindex`).
pub(crate) const KEY_MACHINE_PREFIX: &str = "toolchain.machine.";

/// Cap on a sanitized failure reason inside a row.
const ERROR_CAP: usize = 60;
/// Cap on a program name inside a row (the store's own names are short).
const NAME_CAP: usize = 24;

/// The first-run toolchain row's title, the one noun on every surface.
pub(crate) const INSTALLING_ALAB: &str = "Installing ALab tools";
/// A heavy routine pass's live row: the same noun, and the verb
/// says it updates what is already there.
pub(crate) const UPDATING_ALAB: &str = "Updating ALab tools";
/// The toolchain records' titles, one per outcome, so `appstatus` and the log
/// tell a deferral, a failure and an install apart by title.
pub(crate) const PACKAGES_POSTPONED: &str = "ALab tools update postponed";
pub(crate) const PACKAGES_INSTALLED: &str = "ALab tools installed";
/// A pass that ended well with no roster (`seed-done:`): atpkg's sentence
/// states what is installed, not what the pass changed, so the title claims
/// only the check (a first run's is [`PACKAGES_INSTALLED`]).
pub(crate) const PACKAGES_FINISHED: &str = "ALab tools checked";
pub(crate) const PACKAGES_FAILED: &str = "Couldn't update ALab tools";
/// A first run that failed (its row and record): it was an install, not an
/// update.
pub(crate) const ALAB_INSTALL_FAILED: &str = "Couldn't install ALab tools";
/// A pass that ended with no ALab tools on disk and no failure seen (the set
/// excluded, a clean exit over an empty store): "failed" would claim a failure
/// nobody saw.
pub(crate) const PACKAGES_NOT_INSTALLED: &str = "Couldn't install any ALab tools";
/// Nothing to install on this machine, as a fact (nothing published for its
/// architecture, or every program removed on purpose): no attempt failed.
pub(crate) const PACKAGES_UNAVAILABLE: &str = "No ALab tools to install";

/// `⇣` — a pass moving bytes.
const DOWN: char = '\u{21e3}';
/// `ℹ`, a record of the lane at work (a pass put off, a machine setting a pass
/// changed): a record wears its outcome's mark, not a moving row's.
const NOTED: char = '\u{2139}';

/// The pass name and start stamp that identify one `progress.json` writer,
/// so a live meter is clamped to its OWN pass's high-water mark and never
/// inherits a different pass's ([`snapshot_words`]).
pub(crate) type PassId = (String, u64);

/// The machine-settings records' capsule: Settings ▸ Security, where the
/// `[machine]` switches and the "This Mac" card are ([`machine_settings`]).
fn security() -> Intent {
    Intent::OpenSettings {
        route: crate::native_settings::SettingsRoute::Security
            .path()
            .to_string(),
    }
}

/// The `Packages` capsule every other toolchain record carries, pointing at the
/// page that holds the durable record. Live rows carry none ([`live_row`]).
fn packages() -> Intent {
    Intent::OpenSettings {
        route: crate::native_settings::SettingsRoute::Packages
            .path()
            .to_string(),
    }
}

/// A glyph from the band's closed set. Every glyph this module names is in
/// it (a test walks them); the fallback is unreachable here and exists only
/// because the set is closed by construction.
fn glyph(ch: char) -> Glyph {
    Glyph::or_fallback(ch)
}

/// A toolchain RECORD's row: tag, severity, title, the `Packages` capsule,
/// classified a record ([`Message::record`]: `Hold::LogOnly`, never on
/// glass — every caller is one of the silent lane's records).
fn row(severity: Severity, title: impl AsRef<str>) -> Message {
    Message::new(tags::TOOLCHAIN, severity, title)
        .action(packages())
        .record()
}

/// A RECORD's words as a held row, the way a build before the silent lane
/// carried them: unclassified, at the default hold. A record's own class keeps
/// it off the glass whatever hold it is given, so a test that needs these
/// words on the band rebuilds them unclassified.
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

/// A LIVE toolchain row — the first run's announcement and its meter, a heavy
/// routine pass: tag, severity, title, and NO capsule. A live row is work in
/// flight, not a decision: `Details ›` leads, and the eleven cells a
/// `Packages` chip took keep the title whole at 60 columns beside its load
/// words and `Details ›` whole at 80 (review round 2, 2026-09-23 — at 80 the
/// row read `Packages   ›`, at 60 `Installing AL…`).
/// WORK IN FLIGHT ([`Message::working`]): live under `stale_after`, and every
/// caller gives it its indicator (a fill, `busy`, or [`Message::in_flight`]).
fn live_row(title: impl AsRef<str>, stale_after: Duration) -> Message {
    // The pass IS a download first: its `network busy` says nothing the
    // title does not, and shows only while other work is measurably slowed;
    // the extraction's disk and the verification's CPU show as before
    // (ruling 246).
    Message::new(tags::TOOLCHAIN, Severity::Info, title)
        .loads(PASS_LOADS)
        .primary_load(Load::Network)
        .working(stale_after)
}

/// The loads a pass row may show over its life ([`pass_load`] and the
/// announcement's network): the load slot is reserved at the widest of their
/// words, so a phase change never re-lays it.
pub(crate) const PASS_LOADS: [Load; 3] = [Load::Network, Load::Disk, Load::Cpu];

/// A toolchain RECORD (the silent lane, module doc): a row's words with
/// `Hold::LogOnly` — recorded in the log and on `appstatus` (a finished
/// activity, `outcome=warn` ⇔ Warn), never on glass. `detail` is the bars'
/// ledger detail, byte for byte; an empty one is no line.
fn record(severity: Severity, title: impl AsRef<str>, detail: String) -> Message {
    row(severity, title).line(detail)
}

/// A record whose detail is atpkg's sentence, kept whole across lines: no
/// reporter pre-cuts a sentence it received, and a record is never painted.
fn whole_record(severity: Severity, title: &str, detail: &str) -> Message {
    row(severity, title).sentence(detail)
}

/// Which `~/.aterm/shell.d/00-atpkg.*` a frozen tab sources to pick up the
/// managed programs in place. Not `exec $SHELL`: aterm's zsh integration rides
/// a `ZDOTDIR` wrapper (bash rides `--rcfile`), so a re-exec'd plain shell
/// loses its OSC 133/633 marks, cwd/title tracking and live PATH re-assert,
/// and only picks up PATH through the rc block `atpkg` writes into an existing
/// rc file. Sourcing the hook (what atpkg's own rc block does) moves
/// `<prefix>/agents/` to the front of PATH, exports `ATPKG_AGENTS` and appends
/// `bin/`, idempotently, keeping the tab's integration. The file names follow
/// `crates/atpkg/src/hooks.rs` (`HOOK_BASENAME`, one file per dialect: POSIX
/// `.` for zsh and bash, fish's `source`, PowerShell's dot-source). A shell
/// atpkg has no hook for ([`HookDialect::None`]: nushell, xonsh, cmd) gets no
/// command; a new tab picks the programs up.
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
            // A WSL tab's bash sources `$HOME/.aterm/shell.d` in the distro's
            // home, where atpkg (running on the Windows host) writes nothing, so
            // `. ~/.aterm/shell.d/00-atpkg.bash` would be "No such file".
            S::Wsl => Self::None,
            S::Cmd | S::Unknown => Self::None,
            // `ShellType` is non-exhaustive: a shell added later has no hook
            // until `hooks::hook_files` writes one for it.
            _ => Self::None,
        }
    }
}

impl HookDialect {
    /// The backtick-quoted line a person types in the frozen tab, or `None` for
    /// a shell without a hook. The path is the one `hooks::refresh` writes,
    /// `~/.aterm/shell.d/<HOOK_BASENAME>.<ext>`. The one spelling: the seed pill
    /// (`seed_pill_text` in `lib.rs`) reads it from here too.
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

/// Human byte figure in decimal units: `812 KB`, `512 MB`, `1.2 GB`. A figure
/// that would round to 1000 of a unit is the next unit up (`999.6 MB` is
/// `1.0 GB`), so no figure is wider than six cells below a terabyte.
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

/// `done / total` bytes as a meter's stats, `done` right-aligned to the total's
/// width, so the stats keep one width for the whole transfer: the width law
/// lays the row out once and the digits tick in place.
pub(crate) fn byte_stats(done: u64, total: u64) -> String {
    let (done, total) = (fmt_bytes(done.min(total)), fmt_bytes(total));
    let width = total.chars().count();
    format!("{done:>width$} / {total}")
}

/// `done` of `total` bytes as a meter fill in permille; `None` when the
/// total is unknown (0), which is honestly no meter.
pub(crate) fn fill_permille(done: u64, total: u64) -> Option<u16> {
    (total > 0).then(|| {
        let f = done.min(total) as f64 / total as f64;
        (f * 1000.0).round() as u16
    })
}

// ---------------------------------------------------------------------------
// The markers.
// ---------------------------------------------------------------------------

/// `atpkg` announced a first-run pass (`net-starting:` from a child the lane
/// tagged first run): the row opens now, before `progress.json` exists, since
/// the extraction that follows is minutes long and gigabytes wide. Busy (no
/// fraction yet), with atpkg's computed size as its stats, the network as its
/// load, and no capsule (work in flight is not a decision). atpkg's size is
/// the only detail line ("about 3 GB on disk when finished"; none without a
/// size). A routine pass's announcement is a log line and never reaches here.
/// Live under the announcement cap ([`STALE_ANNOUNCE`]): the child's exit
/// normally folds it; if none is reported it folds on its own.
pub(crate) fn announced(detail: &str) -> Message {
    // The size is atpkg's closing ` (…)` clause — spaced, so the `(s)` of
    // `program(s)` is never read as one.
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

/// The announcement's stats, terse: `10 programs · ~3 GB` from atpkg's
/// `installing 10 ALab program(s) … (about 3 GB on disk when finished)`; the
/// whole sentence stays behind Details.
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

/// The wait ran out (atpkg exit 75): the pass is deferred (the loop retries on
/// its short backoff, or parks an hour when the holder looks wedged) and
/// `detail` says which. An Info record ("ALab tools update postponed",
/// atpkg's sentence), never "failed" and never a row on glass. A stand-down
/// that will not retry is also the Packages badge (the host's
/// `PkgLockTimedOut { stands_down }` arm). No key.
pub(crate) fn deferred(detail: &str) -> Message {
    whole_record(Severity::Info, PACKAGES_POSTPONED, detail).glyph(glyph(NOTED))
}

/// Where the installed programs are used, when nothing narrows it: the one
/// clause the installed record and the managed-current record share.
const READY_IN_EVERY_TAB: &str = "ready in every aterm tab";

/// Why the installed programs reach no tab of this window, from the recorded
/// shell-integration outcome (`spawn::shell_integration_outcome`), or `None`
/// when the integration runs (or no shell was prepared). The installed record
/// and the managed-current record both say it.
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
    }
}

/// The installed pill's sentence. `frozen_tabs` is `App::frozen_path_tabs`:
/// live tabs adopted from a build whose sessions lack the managed `agents/`;
/// `hook` is the dialect of the atpkg hook such a tab sources to catch up
/// (`HookDialect::remedy`, the window's spawn shell). The frozen clause is
/// joined with ` · ` so the width law ([`shape_detail`], which knows ". " and
/// " · " as sentence joints) keeps the qualifier together with the command.
pub(crate) fn seed_pill_text(
    installed: &[String],
    shell_integration: Option<&crate::spawn::ShellIntegrationOutcome>,
    frozen_tabs: usize,
    hook: HookDialect,
) -> String {
    // Cap the roster so the pill stays a pill: ≤5 names, then an ellipsis
    // standing in for the rest.
    let mut names = installed
        .iter()
        .take(5)
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(", ");
    if installed.len() > 5 {
        names.push('…');
    }
    // No pill sends a hooked tab to a new one. `claude`/`codex` live in
    // `<prefix>/agents/`, which the spawn seam puts in front of every session's
    // PATH (`spawn::managed_agents_dir`), and atpkg re-lays them in place. The
    // rest of the roster lives in `<prefix>/bin/`, reached through the atpkg
    // hook in `~/.aterm/shell.d`; the integration's `__aterm_managed_path_live`
    // runs at every precmd and preexec, sources the hook once it exists and
    // re-asserts `reroute/` and `agents/` in front, so the tab on glass has the
    // whole roster at its next prompt.
    //
    // Tabs adopted from a build without that live re-assert
    // (`App::frozen_path_tabs`) narrow the claim to tabs opened since, and are
    // named with their in-place remedy (`HookDialect::remedy`, never
    // `exec $SHELL`). An agents-only roster names no remedy: its frozen tabs
    // are the managed-current record's to count.
    //
    // A tab whose integration failed (unknown shell, unwritable loader cache:
    // the recorded runtime outcome) has neither the re-assert nor `bin/`, since
    // the user's rc can reorder PATH; nothing is promised there, and the
    // sentence names the recorded cause ([`unhooked_clause`]).
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

/// `net-installed:`: the toolchain is here. `text` is the pill's sentence
/// (roster plus the readiness clause or the shell-integration caveat), authored
/// by the caller; the record has its own title, so the pill's opening is not
/// repeated as the detail. A Success record with no meter, titled "ALab tools
/// installed"; the first-run row folds at its child's exit.
pub(crate) fn installed(text: &str) -> Message {
    let detail = text
        .strip_prefix("\u{2713} ALab toolchain installed: ")
        .unwrap_or(text);
    whole_record(Severity::Success, PACKAGES_INSTALLED, detail).key(KEY_PASS)
}

/// `seed-done:`: an announced pass that ended well with no install roster to
/// name. Unlike [`installed`], it claims nothing beyond atpkg's sentence: a
/// Success record ("ALab tools checked"). `first_run`: the first run's pass,
/// which was an install, so "ALab tools installed" (as [`install_failed`] is
/// its failure).
pub(crate) fn ended(detail: &str, first_run: bool) -> Message {
    let title = if first_run {
        PACKAGES_INSTALLED
    } else {
        PACKAGES_FINISHED
    };
    whole_record(Severity::Success, title, detail).key(KEY_PASS)
}

/// A bad terminal outcome for the toolchain lane (`seed-failed:` /
/// `net-failed:` / `seed-unusable:` / a coherence group's abort / a child that
/// died after announcing). `what` is the whole sentence the ledger recorded
/// (atpkg's clipped cause or the host's `failure_cause` verdict); the
/// `Packages` capsule points at the page, so the sentence does not name it.
/// Never for a store-lock wait's timeout, which is [`deferred`].
///
/// A Warn record, never a row: the failure's surface is the Settings ▸
/// Packages badge (`PackagesService::note_pass_trouble`), and no meter can be
/// fused with the verdict. Titled "Couldn't update ALab tools"; the reason is
/// kept whole ([`whole_reason`]).
pub(crate) fn failed(what: &str) -> Message {
    bad_outcome(PACKAGES_FAILED, what)
}

/// [`failed`] for a first run: the record the first-run failure row stands
/// beside says what that row says — an install, in the lane's noun.
pub(crate) fn install_failed(what: &str) -> Message {
    bad_outcome(ALAB_INSTALL_FAILED, what)
}

/// A pass that left no ALab tools on disk and saw no failure (`seed-nothing:`,
/// or a `seed-unusable:` whose `[packages].exclude` left nothing):
/// "Couldn't install any ALab tools", a Warn record shaped like [`failed`],
/// never titled "failed". The reason is kept whole.
pub(crate) fn not_installed(what: &str) -> Message {
    bad_outcome(PACKAGES_NOT_INSTALLED, what)
}

/// `seed-unusable:`: [`unavailable`] when atpkg's sentence states a fact about
/// the machine (`atpkg::cli::seed_unusable_is_informational`: nothing published
/// for this architecture, or every program removed on purpose), else
/// [`not_installed`] (an exclusion a person can shorten).
pub(crate) fn unusable(what: &str) -> Message {
    if atpkg::cli::seed_unusable_is_informational(what) {
        unavailable(what)
    } else {
        not_installed(what)
    }
}

/// Nothing to install on this machine, and no failure: an Info record, "No ALab
/// tools to install", atpkg's sentence kept whole as its detail. No attempt
/// failed, so it is never titled "Couldn't …".
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
/// only its stderr. One detail line per line of the reason, and a line past
/// the engine's line cap split at its joints with every word kept
/// (`aterm_messages::text::split_sentence`), so the reason's last words
/// survive; the center keeps at most `DETAIL_LINES_CAP` of them.
fn whole_reason(what: &str) -> Vec<String> {
    what.lines()
        .flat_map(|l| aterm_messages::text::split_sentence(l, DETAIL_LINE_CAP))
        .collect()
}

/// How a first run ended short (the marker its child printed): what the
/// failure row after it says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FirstRunShort {
    /// `seed-failed:` / `net-failed:` (or a pass that could not launch).
    Failed,
    /// `seed-nothing:` or a non-informational `seed-unusable:`: the pass ran
    /// and nothing is installed.
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

/// A first run that ended short. The first-run row told the person to wait for
/// a multi-gigabyte deliverable, so when it does not arrive this is a failure
/// row they act on (retry from Settings ▸ Packages, free the disk), keyed by
/// the pass, `Packages` its capsule, the default hold. The cause rides behind
/// Details unless the person can fix it themselves ([`actionable_cause`]), and
/// is kept whole ([`whole_reason`]). The record ([`failed`]) and the Packages
/// badge stay as they are; a heavy routine pass stays a record, since the
/// installed versions keep working. Tagged `packages`: `appstatus` lists the
/// toolchain lane's unretired rows as `phase=live` work, and this is an
/// outcome.
pub(crate) fn first_run_short(how: FirstRunShort, cause: &str) -> Message {
    let (severity, title) = match how {
        // One severity for the family: a failed first run left the Mac as it
        // was and the page retries it, like a first run that installed nothing.
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

/// A plain text posted from outside the process (`aterm ctl appnotice <lane>
/// <text>`), e.g. an `aterm pkg install claude` run in a terminal. No severity,
/// action or progress, so it is a record: Settings ▸ Messages and `appstatus`
/// keep it, the glass never shows it. `tag` names the lane (`toolchain` or
/// `update`).
pub(crate) fn appnotice(tag: Tag, text: &str) -> Message {
    let text = sanitize_for_tty(text, 120);
    match installed_words(&text) {
        // `aterm pkg install claude: 2.1.267 installed` is a command's
        // transcript line, not a title (ruling 261): the program as a person
        // knows it and what happened — `Claude Code 2.1.267 installed` — the
        // line itself behind it.
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
    // atpkg may name the program again: `claude: claude 2.1.281 installed`.
    let version = version
        .strip_prefix(program.trim())
        .map_or(version, str::trim_start);
    let name = match program.trim() {
        "claude" => "Claude Code",
        "codex" => "Codex",
        other => other,
    };
    (!name.is_empty() && !version.is_empty() && !version.contains(' '))
        .then(|| format!("{name} {version} installed"))
}

// ---------------------------------------------------------------------------
// The managed-current and machine-settings records.
// ---------------------------------------------------------------------------

/// `managed-current:`: every agent program (claude, codex) installed and at its
/// vendor's latest, as atpkg lists it: `claude 2.1.280 (Anthropic latest);
/// codex 0.156.0 (OpenAI latest)` (an older atpkg's `(build <N>)` still
/// parses). The title names the programs as a person knows them ("Claude Code
/// 2.1.280 and Codex 0.156.1 are current"); the detail says where they are used
/// ([`managed_current_words`]). A keyed Success record, never a row: the host
/// records it only when its words change (`App::post_managed_current`).
///
/// `frozen_tabs` counts live tabs adopted from a build whose sessions have no
/// managed `agents/` on PATH (`App::frozen_path_tabs`); for those the detail is
/// the in-place remedy (sourcing the atpkg hook for `hook`'s dialect).
/// `this_tab_hooked` is whether the shell integration runs in this window's
/// tabs (the window passes [`unhooked_clause`]'s cause instead): only its
/// per-prompt re-assert keeps `agents/` in front after the user's rc, so a
/// window whose integration failed is not told "every tab".
///
/// `None` for an empty body. The window posts through [`managed_current_for`],
/// which also knows the tabs running an agent and the sessions the live
/// upgrade is moving; this is its frozen-shells-only form.
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

/// [`managed_current`] over the whole of [`TabsBehind`]. `unhooked` is
/// [`unhooked_clause`] for this window: why its tabs do not get the programs.
pub(crate) fn managed_current_for(
    text: &str,
    tabs: TabsBehind,
    unhooked: Option<&str>,
    hook: HookDialect,
) -> Option<Message> {
    let (title, detail) = managed_current_words_for(text, tabs, unhooked, hook)?;
    Some(record(Severity::Success, title, detail).key(KEY_MANAGED))
}

/// How the live tabs stand against the programs the managed-current record
/// names, so the record neither calls lagging sessions up to date nor hands a
/// hook line to a tab whose foreground is an agent.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct TabsBehind {
    /// Tabs whose shell runs a frozen PATH (measured, or carried and not yet
    /// measured) with no agent in front: the hook remedy is typed there.
    pub(crate) frozen_shells: usize,
    /// Frozen tabs whose foreground is an agent (`claude`, `codex`) that the
    /// live upgrade is not moving: never told to type into (it is a prompt
    /// there), told to source the hook once the agent exits.
    pub(crate) frozen_agents: usize,
    /// Running Claude Code sessions the live upgrade moves onto the version
    /// this record names, on their own (its relaunch line sources the hook too,
    /// so a frozen tab among them heals with the move). Never a stalled one, and
    /// never one moving to another version.
    pub(crate) agents_behind: usize,
    /// Running Claude Code sessions behind that version that will not move on
    /// their own: stalled (the band has a row for each), or held by the owner's
    /// `--defer`/`--skip`. They keep the title at "installed" and are pointed at
    /// `--status`.
    pub(crate) agents_held: usize,
    /// Live tabs with no adoption mark whose shell's PATH was measured, two jobs
    /// in a row, to put a foreign `claude`/`codex` first
    /// (`SessionTimeline::path_settled_frozen`). The shell or its rc reorders
    /// PATH, so no hook remedy is named, but "ready in every aterm tab" is not said.
    pub(crate) shadowed: usize,
}

/// The version the managed-current wire text gives `name`
/// (`claude 2.1.282 (Anthropic latest); codex …`), or `None` when it names
/// none. What the live upgrade's rows are matched against, so the record
/// counts only the sessions moving onto the build it announces.
pub(crate) fn managed_version(text: &str, name: &str) -> Option<String> {
    text.split(';').map(str::trim).find_map(|item| {
        let head = item.split_once('(').map_or(item, |(head, _)| head.trim());
        let mut words = head.split_whitespace();
        (words.next()? == name)
            .then(|| words.next().map(str::to_string))
            .flatten()
    })
}

/// The managed-current entry's words from the wire text, or `None` for an empty
/// body. The title names the programs as a person knows them and says what the
/// marker means: "Claude Code 2.1.280 and Codex 0.156.1 are current" (one name:
/// "… is current"; unknown names verbatim, so `gemini` reads as gemini). The
/// detail says where they are used: "ready in every aterm tab". Neither the vendor
/// source nor an older atpkg's `(build <N>)` store id is shown.
///
/// The managed `agents/` is in front of every session's PATH from launch
/// (`spawn::managed_agents_dir`) and atpkg re-lays the twins in place, so the
/// next invocation in any tab runs the named build. Two exceptions replace the
/// claim: a window whose shell integration failed (`this_tab_hooked`) gets the
/// installed record's cause ([`unhooked_clause`]), and `frozen_tabs` tabs adopted from an older build get
/// the remedy ("1 tab from before this update picks them up with
/// `. ~/.aterm/shell.d/00-atpkg.zsh`", spelled for `hook`, see [`HookDialect`]),
/// said last so a cut keeps it ([`shape_detail`]). The frozen-shells-only form
/// of [`managed_current_words_for`], which the window uses.
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

/// [`managed_current_words`] over the whole of [`TabsBehind`]: the title says
/// "installed", not "current", while running sessions lag
/// (`agents_behind`, named in the detail with when they move); an agent tab
/// is never handed the hook to type (`frozen_agents` are told "once it
/// exits"); and the hook remedy, for the shells alone, is said last.
pub(crate) fn managed_current_words_for(
    text: &str,
    tabs: TabsBehind,
    unhooked: Option<&str>,
    hook: HookDialect,
) -> Option<(String, String)> {
    let frozen_tabs = tabs.frozen_shells;
    let mut names: Vec<String> = Vec::new();
    for item in text.split(';').map(str::trim).filter(|s| !s.is_empty()) {
        // `<name> <version> (<Vendor> latest)`, or an older atpkg's `(build
        // <N>)`: version and parenthesis are each optional on the wire, so a
        // bare name still renders. The parenthesis is not shown.
        let head = item.split_once('(').map_or(item, |(head, _)| head.trim());
        let mut words = head.split_whitespace();
        let name = words.next().unwrap_or_default();
        let version = words.next().unwrap_or_default();
        let display = match name {
            "claude" => "Claude Code",
            "codex" => "Codex",
            other => other,
        };
        names.push(if version.is_empty() {
            sanitize_for_tty(display, NAME_CAP)
        } else {
            sanitize_for_tty(&format!("{display} {version}"), NAME_CAP + 24)
        });
    }
    // The programs as a person knows them, with versions, and that they are
    // current; or, while an agent session still runs an older build of one,
    // that they are installed ("current" would claim the sessions are too).
    let lagging = tabs.agents_behind + tabs.agents_held > 0;
    let title = match (names.as_slice(), lagging) {
        ([], _) => return None,
        ([one], false) => format!("{one} is current"),
        ([head @ .., last], false) => format!("{} and {last} are current", head.join(", ")),
        ([one], true) => format!("{one} installed"),
        ([head @ .., last], true) => format!("{} and {last} installed", head.join(", ")),
    };
    let title = sanitize_for_tty(&title, 120);
    let mut pieces: Vec<String> = Vec::new();
    // "Every tab" rests on the same evidence as the pill: the user's rc undoes
    // the seam's front position and only the integration's per-prompt re-assert
    // restores it, so a window whose integration failed gets the installed
    // record's cause.
    if let Some(clause) = unhooked {
        pieces.push(clause.to_string());
    } else if frozen_tabs == 0 && tabs.frozen_agents == 0 && tabs.shadowed == 0 {
        pieces.push(READY_IN_EVERY_TAB.to_string());
    }
    // "On its own", not "at its next turn end": the live upgrade starts at a
    // quiet moment, and a goal-mode session's turns never end (owner, 2026-09-28).
    if tabs.agents_behind > 0 {
        let n = tabs.agents_behind;
        pieces.push(if n == 1 {
            "1 running Claude Code session moves onto it on its own".to_string()
        } else {
            format!("{n} running Claude Code sessions move onto it on their own")
        });
    }
    if tabs.agents_held > 0 {
        let n = tabs.agents_held;
        pieces.push(if n == 1 {
            "1 running Claude Code session is still on the old build (`aterm harness upgrade \
             --status` says why)"
                .to_string()
        } else {
            format!(
                "{n} running Claude Code sessions are still on the old build (`aterm harness \
                 upgrade --status` says why)"
            )
        });
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
            "1 tab's PATH puts another claude or codex first".to_string()
        } else {
            format!("{n} tabs' PATHs put another claude or codex first")
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

/// `machine-settings:`: the machine-level settings a pass changed per doctor,
/// as atpkg lists them: `spotlight-noindex 73 dir(s) migrated;
/// universal-control disabled`. One Info record per item
/// ([`machine_setting_words`]), in wire order, none on glass: the change and its
/// undo are on Settings ▸ Security's "This Mac" card
/// (`PackagesService::note_machine_change`), the log and `appstatus`. Each item
/// wears a record's `ℹ`, is keyed on its own first word, and its capsule opens
/// Settings ▸ Security, where the `[machine]` switches are.
pub(crate) fn machine_settings(text: &str) -> Vec<Message> {
    text.split(';')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|item| {
            // The key is the wire item's first word, not the display title's:
            // atpkg's vocabulary is the stable one.
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

/// One machine-settings wire item as its row's words `(title, detail)`:
/// `universal-control disabled` → "ALab tools turned off Universal Control",
/// whose detail is the whole undo: the `[machine]` switch set to leave (the
/// default policy turns it off again at the next window or the day's first
/// terminal session) and the revert command, byte-identical to
/// [`atpkg::machine::UNIVERSAL_CONTROL_REVERT`] (atpkg's `doctor_line` says the
/// same); `spotlight-noindex N dir(s) migrated` → "Spotlight now skips N build
/// folders", whose detail is what a person acts on: the rename, and that cargo
/// still builds there. An unknown item is its own title, verbatim, with no
/// detail.
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
    } else if let Some(count) = spotlight_noindex_count(&item) {
        (
            format!(
                "Spotlight now skips {count} build {}",
                if count == 1 { "folder" } else { "folders" }
            ),
            "renamed with a .noindex suffix; cargo still builds there".to_string(),
        )
    } else {
        (item.clone(), String::new())
    };
    (
        sanitize_for_tty(&title, 120),
        shape_detail(&detail, DETAIL_LINE_CAP),
    )
}

/// The N of `spotlight-noindex N dir(s) migrated`, when the item is that.
fn spotlight_noindex_count(item: &str) -> Option<u64> {
    item.strip_prefix("spotlight-noindex ")?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

// ---------------------------------------------------------------------------
// The tailed snapshot (the terminal readings of the file).
// ---------------------------------------------------------------------------

/// What one classified `progress.json` read says. The host applies it against
/// the row that carries [`KEY_PASS`], and only a first run's reads paint (see
/// the module docs):
///
/// * `Live` restates the first run's live row in place (a `restate` costs no
///   log line, so the 10 Hz tailer is free) or posts it when none is up; a
///   routine pass's `Live` paints nothing;
/// * `Over` and `Vanished` claim no outcome: the markers record the outcome,
///   and a failure is the Settings ▸ Packages badge. A routine read folds a
///   live row at once; a first run's ending read folds nothing; the child's
///   exit does (`App::toolchain_pass_ended`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SnapshotWords {
    /// The file vanished at child exit: a live row has nothing honest left to
    /// say.
    Vanished,
    /// The pass is over (ended cleanly, with failures, or its writer gone)
    /// and the read claims no outcome.
    Over,
    /// Nothing planned while the pass runs: atpkg begins its "net" pass before
    /// the signed index resolves, and the common outcome is a plan of zero
    /// programs. Leave the glass alone rather than re-grid twice for "nothing
    /// to do" (an announced row keeps its announcement until the plan lands).
    Quiet,
    /// A pass in flight: the words, the meter and the tailed cap, plus the
    /// pass they belong to (for the next read's high-water clamp).
    Live {
        /// The row's words: `Hold::Live { STALE_TAILED }`, keyed [`KEY_PASS`].
        /// Boxed: a `Message` outweighs every other variant many times over,
        /// and the 10 Hz tailer moves one of these per read.
        message: Box<Message>,
        /// The writer's identity — pass name and start stamp.
        pass_id: PassId,
    },
}

/// The words for one classified read, pure. `peak` is the live row's meter as
/// the host holds it (its pass and its fill), so a live meter never runs
/// backwards within one pass: atpkg's `.part` poller reports 0 between curl
/// promoting the file and the watch stopping, and a program's credit can dip
/// during its verify phase. A different pass (pass name + start stamp) starts
/// fresh.
///
/// The row paints its title and indicator alone: the current program and
/// phase (`trust · extracting`) and atpkg's byte rollup (download bytes only,
/// which disagrees with the bar) ride behind Details. The stats are the count
/// that agrees with the bar, `3 of 10 programs`. The heaviest active phase is
/// the declared load ([`pass_load`]). The fill counts programs weighted by
/// phase ([`overall_fill`]), reaching 100 % only when every program has
/// finished, and is the row's [`Amount`] too ([`Unit::Steps`], a series per
/// pass), so the band's ETA is the fill's own rate. `routine` titles a heavy
/// routine pass (`Updating ALab tools`).
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
        // A progress format a newer aterm writes: the title alone, moving —
        // nothing else about it is known here.
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
    // One title per install: the announcement's row says [`INSTALLING_ALAB`],
    // and so does the pass it announced (`net`, the one pass atpkg writes). A
    // heavy routine pass updates what is already there.
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
    // A program verifying or linking holds its share (500‰, 990‰) with no
    // meter of its own: a minute of verify is the plan, not a stall.
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
            // No known fraction yet (a pass before its plan lands, a program
            // whose phase has no bytes): busy, and the engine moves it.
            busy: fill.is_none(),
            level: false,
        })
        .key(KEY_PASS);
    SnapshotWords::Live {
        message: Box::new(message),
        pass_id,
    }
}

/// The resource the pass loads hardest right now: any program extracting or
/// linking loads the disk; else a verification the CPU; else a download the
/// network. It names the heaviest active phase, not the one the detail line
/// picks. `None` with nothing in flight.
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
/// second half (by bytes, to 99 %), a link as 99 %. Reaches 100 % only when
/// every program has finished. `None` when the pass planned no programs.
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
    // Mid-flight phases first, in pass order; a queued front-of-queue program
    // only when nothing is mid-flight.
    let active = f
        .programs
        .iter()
        .filter(|(_, r)| {
            matches!(
                r.phase,
                Phase::Download | Phase::Verify | Phase::Extract | Phase::Link
            )
        })
        .min_by_key(|(_, r)| r.phase as u8);
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
        // Label-only phases: not byte streams atpkg can meter, and the row does
        // not pretend otherwise.
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
    // Why a program was pulled forward is scheduling, not progress:
    // progress.json and `aterm pkg doctor` keep it, the row does not.
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
    use aterm_messages::{Hold, Instant, MessageCenter, MessageLog, TITLE_CAP, WallStamp};
    use std::collections::BTreeMap;

    fn file(running_pid: Option<u32>, pass: &str) -> atpkg::progress::ProgressFile {
        atpkg::progress::ProgressFile {
            v: PROGRESS_VERSION,
            pid: running_pid,
            pass: pass.to_string(),
            started_unix: 1_700_000_000,
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
    /// tools`, `Updated ALab tools`, `Installed packages`) with nothing else on
    /// the row moving at 60, 80, 120 and 160 columns.
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
        let machine = machine_settings("universal-control disabled; spotlight-noindex 2 dir(s)");
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
            // The owner's rule: progress with its indicator, or a record.
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
        assert_eq!(packages().label(), "Packages");
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
        assert_eq!(failed.title, "Couldn't install ALab tools");
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
        // atpkg's up-front refusal names the need and what this Mac has: painted.
        // Built from atpkg's own error, so a rewording there fails here.
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
        // As the child's stderr carries it, behind atpkg's own head.
        let stderr = format!("atpkg: bootstrap install trust failed: {refusal} (continuing)");
        assert!(first_run_short(FirstRunShort::Failed, &stderr).excerpt);
        // The reason is kept whole: a long atpkg stderr
        // keeps every word, split at its joints like the record's.
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
        assert_eq!(m.title, "ALab tools update postponed");
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
    /// "failed". With no words from atpkg it has no detail line: the title is
    /// the fact.
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
    /// whole. The bodies are atpkg's words (`seed_unusable_tests`).
    #[test]
    fn an_informational_unusable_is_an_info_record_not_a_failure() {
        for body in [
            "ALab publishes no build for this machine's architecture (x86_64-apple-darwin) \
             \u{2014} it builds for aarch64-apple-darwin; nothing installs here until one is published",
            "every ALab program available here was removed on this machine (ay, trust) \u{2014} \
             nothing to install. `aterm pkg install <program>` brings one back",
        ] {
            let m = unusable(body);
            assert_eq!(m.title, "No ALab tools to install", "{body}");
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
        assert_eq!(m.title, "ALab tools installed");
        assert_eq!(
            m.detail,
            vec!["claude, codex \u{2014} ready in every aterm tab"]
        );
        assert_eq!(m.severity, Severity::Success);
        assert_eq!(m.meter, None, "a terminal Success record carries no meter");
        assert_eq!(m.hold, Hold::LogOnly, "no success row (2026-09-22)");
        let e = ended("the pass finished; 12 ALab program(s) are installed", false);
        assert_eq!(
            e.title, "ALab tools checked",
            "no claim the pass changed anything"
        );
        assert_eq!(
            ended("the pass finished; 12 ALab program(s) are installed", true).title,
            "ALab tools installed",
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
        // Records are never painted: atpkg's sentence is kept whole, never cut
        // mid-word.
        let long = format!("the pass finished; {} are installed", "program ".repeat(30));
        for m in [installed(&long), ended(&long, false), deferred(&long)] {
            let rejoined = m.detail.join(" ");
            // Every word, in order; a line joint (`; `) is where a line breaks.
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
        assert_eq!(m.title, "Couldn't update ALab tools");
        assert_eq!(m.detail, vec!["partly installed: ay, trust"]);
        assert_eq!(m.meter, None);
        assert_eq!(m.severity, Severity::Warn);
        assert_eq!(m.hold, Hold::LogOnly);
        assert_eq!(m.key.as_deref(), Some(KEY_PASS));
        let hostile = failed("bad\u{1b}[31m thing\u{7}");
        let d = &hostile.detail[0];
        assert!(!d.contains('\u{1b}') && !d.contains('\u{7}'), "{d:?}");
        // The whole reason: a 200-char cause is one line, uncut; a stderr of
        // two lines is two detail lines.
        let long = format!("the child died after announcing: {}", "x".repeat(170));
        let m = failed(&long);
        assert!(m.detail[0].chars().count() > 160, "{}", m.detail[0]);
        assert_eq!(m.detail, vec![long]);
        let m = failed("error: tar exited 2\nno space left on device");
        assert_eq!(
            m.detail,
            vec!["error: tar exited 2", "no space left on device"]
        );
        // A line past the line cap is split at its joints, never cut: the tail
        // (the reason's last words) survives.
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
        // A verify or a link holds the program's share with no meter: the
        // same count, declared held, so it never reads stalled.
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
    /// programs weighted by phase; the title is the announcement's for the
    /// `net` pass and the generic one for any other name.
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
        // The extract meter reads 0 for the instant between two writes.
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
    /// the file says, and the outcome is the markers' record. A vanished file,
    /// a plan of nothing and an unknown schema keep their own readings.
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
        // The file vanished; nothing planned.
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
        // An unknown schema, running, is one generic line.
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

    #[test]
    fn hostile_strings_are_sanitized_before_they_become_words() {
        // A program name that fails the store gate has no row words at all.
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
        // The first-run announcement's size and a failure's sentence are
        // atpkg's text: control-stripped before they become words.
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

    #[test]
    fn the_managed_and_machine_records_carry_their_words() {
        let m = managed_current(
            "claude 2.1.267 (build 2026091001)",
            0,
            true,
            HookDialect::Zsh,
        )
        .unwrap();
        assert_eq!(m.title, "Claude Code 2.1.267 is current");
        assert_eq!(
            m.detail,
            vec!["ready in every aterm tab"],
            "one name reads singular; an older atpkg's wire still parses, its build \
             unrepeated"
        );
        assert_eq!(m.hold, Hold::LogOnly, "a record on every pass, never a row");
        assert_eq!(m.meter, None, "a record carries no meter");
        assert_eq!(m.severity, Severity::Success);
        assert_eq!(m.key.as_deref(), Some(KEY_MANAGED));

        // The current wire: vendor programs by version and source, never a
        // store id.
        let m = managed_current(
            "claude 2.1.280 (Anthropic latest); codex 0.156.0 (OpenAI latest)",
            0,
            true,
            HookDialect::Zsh,
        )
        .unwrap();
        assert_eq!(m.title, "Claude Code 2.1.280 and Codex 0.156.0 are current");
        assert_eq!(
            m.detail,
            vec!["ready in every aterm tab"],
            "no vendor source beside the version (design ruling 58)"
        );
        let three = managed_current(
            "claude 2.1.280 (Anthropic latest); codex 0.156.1 (OpenAI latest); gemini 1.0",
            0,
            true,
            HookDialect::Zsh,
        )
        .unwrap();
        assert_eq!(
            three.title,
            "Claude Code 2.1.280, Codex 0.156.1 and gemini 1.0 are current"
        );

        let m = managed_current("gemini; codex 0.154.0", 0, true, HookDialect::Zsh).unwrap();
        assert_eq!(
            m.title, "gemini and Codex 0.154.0 are current",
            "the names are the wire's, never a hardcoded pair"
        );
        assert_eq!(m.detail, vec!["ready in every aterm tab"]);
        assert!(
            managed_current("  ;  ", 0, true, HookDialect::Zsh).is_none(),
            "an empty body records nothing"
        );

        let rows = machine_settings("spotlight-noindex 12 dir(s) migrated");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].title, "Spotlight now skips 12 build folders");
        assert_eq!(
            rows[0].detail,
            ["renamed with a .noindex suffix; cargo still builds there"],
            "the mechanism behind the plain title (ruling 261)"
        );
        assert_eq!(rows[0].severity, Severity::Info);
        assert_eq!(
            rows[0].glyph.ch(),
            NOTED,
            "from the band's closed set — ⚙ rendered blank headless"
        );
        assert_eq!(rows[0].hold, Hold::LogOnly);
        assert_eq!(
            rows[0].key.as_deref(),
            Some("toolchain.machine.spotlight-noindex")
        );
    }

    #[test]
    fn the_managed_row_names_the_frozen_tabs() {
        let wire = "claude 2.1.273 (build 2026091601); codex 0.154.0 (build 2026091001)";
        let (title, detail) = managed_current_words(wire, 1, true, HookDialect::Zsh).unwrap();
        assert_eq!(title, "Claude Code 2.1.273 and Codex 0.154.0 are current");
        assert_eq!(
            detail,
            "1 tab from before this update picks them up with `. ~/.aterm/shell.d/00-atpkg.zsh`",
            "the remedy, never a claim on every tab"
        );
        assert!(
            !detail.contains("every tab"),
            "no claim on a tab that may be frozen"
        );
        assert!(
            !detail.contains("exec"),
            "never `exec $SHELL`: it drops the tab's integration and needs an rc block"
        );
        let (_, detail) = managed_current_words(wire, 2, true, HookDialect::Zsh).unwrap();
        assert_eq!(
            detail,
            "2 tabs from before this update pick them up with `. ~/.aterm/shell.d/00-atpkg.zsh`"
        );
        let (_, detail) = managed_current_words(wire, 0, true, HookDialect::Zsh).unwrap();
        assert_eq!(detail, "ready in every aterm tab");
    }

    /// The record says what is true of the running sessions: sessions the live
    /// upgrade is moving turn "up to date" into "installed" and say when; a
    /// frozen tab with an agent in front is told to source the hook once the
    /// agent exits, never handed a line to type; the shell remedy stays last.
    /// Negative control: with no lagging session and no agent tab the words
    /// are the plain ones.
    #[test]
    fn the_managed_record_says_installed_while_sessions_lag_and_spares_agent_tabs() {
        let wire = "claude 2.1.282 (Anthropic latest); codex 0.157.0 (OpenAI latest)";
        let words = |tabs: TabsBehind| {
            managed_current_words_for(wire, tabs, None, HookDialect::Zsh).unwrap()
        };
        let (title, detail) = words(TabsBehind {
            agents_behind: 2,
            ..TabsBehind::default()
        });
        assert_eq!(title, "Claude Code 2.1.282 and Codex 0.157.0 installed");
        assert_eq!(
            detail,
            "ready in every aterm tab · 2 running Claude Code sessions move onto it on their own"
        );
        let (_, detail) = words(TabsBehind {
            frozen_agents: 1,
            ..TabsBehind::default()
        });
        assert_eq!(
            detail,
            "1 tab from before this update running an agent picks them up with \
             `. ~/.aterm/shell.d/00-atpkg.zsh` once the agent exits"
        );
        assert!(!detail.contains("every tab"), "{detail}");
        let (title, detail) = words(TabsBehind {
            frozen_shells: 1,
            frozen_agents: 1,
            agents_behind: 1,
            ..TabsBehind::default()
        });
        assert_eq!(title, "Claude Code 2.1.282 and Codex 0.157.0 installed");
        assert!(
            detail.ends_with(
                "1 tab from before this update picks them up with `. ~/.aterm/shell.d/00-atpkg.zsh`"
            ),
            "the shell remedy is said last: {detail}"
        );
        let (title, detail) = words(TabsBehind::default());
        assert_eq!(title, "Claude Code 2.1.282 and Codex 0.157.0 are current");
        assert_eq!(detail, "ready in every aterm tab");
    }

    /// A session that will not move on its own is not said to move: it keeps
    /// the title at "installed" and is pointed at `--status`. A tab measured to
    /// put a foreign copy first, with no adoption mark, gets no hook line, but
    /// "ready in every aterm tab" is not said while it lasts.
    #[test]
    fn a_held_session_is_not_said_to_move_and_a_shadowed_tab_is_not_from_before() {
        let wire = "claude 2.1.282 (Anthropic latest)";
        let words = |tabs: TabsBehind| {
            managed_current_words_for(wire, tabs, None, HookDialect::Zsh).unwrap()
        };
        let (title, detail) = words(TabsBehind {
            agents_held: 1,
            ..TabsBehind::default()
        });
        assert_eq!(title, "Claude Code 2.1.282 installed");
        assert_eq!(
            detail,
            "ready in every aterm tab · 1 running Claude Code session is still on the old build \
             (`aterm harness upgrade --status` says why)"
        );
        assert!(!detail.contains("turn end"), "{detail}");
        let (title, detail) = words(TabsBehind {
            shadowed: 2,
            ..TabsBehind::default()
        });
        assert_eq!(title, "Claude Code 2.1.282 is current");
        assert_eq!(detail, "2 tabs' PATHs put another claude or codex first");
        assert!(!detail.contains("before this update"), "{detail}");
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
    /// hook per dialect (`crates/atpkg/src/hooks.rs` `hook_files`: `.zsh`,
    /// `.bash`, `.fish`, `.ps1`), sourced the way that shell sources, and a
    /// shell atpkg has no hook for is told a new tab.
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
        // A WSL tab's `$HOME/.aterm/shell.d` is the distro's home, where the
        // Windows-side atpkg writes nothing, so its note is the new tab.
        assert_eq!(HookDialect::from(ShellType::Wsl), HookDialect::None);
        // The file name is atpkg's, never a spelling of our own.
        assert_eq!(atpkg::hooks::HOOK_BASENAME, "00-atpkg");
    }

    /// "Every tab" rests on the installed record's evidence: a window whose
    /// shell integration failed is told that record's cause instead.
    #[test]
    fn the_managed_row_stops_claiming_this_tab_when_integration_failed() {
        let wire = "claude 2.1.273 (build 2026091601); codex 0.154.0 (build 2026091001)";
        let (_, detail) = managed_current_words(wire, 0, false, HookDialect::Zsh).unwrap();
        assert_eq!(
            detail,
            "not wired into this window's tabs: aterm has no shell integration for nu"
        );
        assert!(!detail.contains("every"), "{detail}");
        // With frozen tabs alive as well, the note still comes last.
        let (_, detail) = managed_current_words(wire, 1, false, HookDialect::Zsh).unwrap();
        assert!(
            detail.starts_with("not wired into this window's tabs")
                && detail.ends_with(
                    "\u{00b7} 1 tab from before this update picks them up with \
                     `. ~/.aterm/shell.d/00-atpkg.zsh`"
                ),
            "{detail}"
        );
        let (_, detail) = managed_current_words(wire, 0, true, HookDialect::Zsh).unwrap();
        assert_eq!(detail, "ready in every aterm tab");
    }

    /// The seed pill over every roster shape × integration outcome × frozen-tab
    /// count × hook dialect (owner, 2026-09-16):
    ///
    /// - the marker glyph leads (the renderer tints the first char) and the
    ///   roster is capped at five names;
    /// - never `exec` (it drops the tab's integration) and never "restart";
    /// - a failed integration promises nothing and names its recorded cause;
    /// - otherwise no pill sends a hooked tab to a new one: with no frozen tabs
    ///   the whole roster is ready in every aterm tab; with frozen tabs the claim
    ///   narrows to tabs opened since, and for a roster with `bin/` tools the
    ///   frozen ones are named with the hook remedy (`HookDialect::remedy`);
    ///   only a shell atpkg has no hook for is told a new tab;
    /// - the roster is named once: every roster is "ready", agents or not.
    ///
    /// Plus the exact sentences, byte for byte.
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
                        // One clause, whatever the roster: no second roster.
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
                        // The frozen-tab remedy is the managed record's, from one source.
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
        // The exact sentences, byte for byte: the cap, the frozen-tab remedy in
        // each dialect, the agents clause for one and two, a failed
        // integration, and a mixed roster that reads as ready whole.
        let six = names(&["ay", "clean", "ny", "trust", "ty", "nn"]);
        let tools = names(&["ay", "trust"]);
        let agents = names(&["claude", "codex"]);
        let mixed = names(&["ay", "claude", "trust"]);
        let no_shell = |name: &str| {
            format!("not wired into this window's tabs: aterm has no shell integration for {name}")
        };
        /// `(roster, integration outcome, frozen tabs, hook dialect, the pill
        /// after its opening)`.
        type Golden<'a> = (&'a [String], Option<Si>, usize, HookDialect, String);
        let goldens: [Golden<'_>; 18] = [
            (
                &six[..3],
                Some(Si::Prepared),
                0,
                HookDialect::Zsh,
                "ay, clean, ny — ready in every aterm tab".into(),
            ),
            (
                &six[..5],
                Some(Si::Prepared),
                0,
                HookDialect::Zsh,
                "ay, clean, ny, trust, ty — ready in every aterm tab".into(),
            ),
            (
                &six,
                Some(Si::Prepared),
                0,
                HookDialect::Zsh,
                "ay, clean, ny, trust, ty… — ready in every aterm tab".into(),
            ),
            (
                &tools,
                Some(Si::Prepared),
                0,
                HookDialect::Zsh,
                "ay, trust — ready in every aterm tab".into(),
            ),
            (
                &tools,
                Some(Si::Prepared),
                1,
                HookDialect::Zsh,
                "ay, trust — ready in every tab opened since this update · 1 tab from before it \
                 picks them up with `. ~/.aterm/shell.d/00-atpkg.zsh`"
                    .into(),
            ),
            (
                &tools,
                None,
                2,
                HookDialect::Bash,
                "ay, trust — ready in every tab opened since this update · 2 tabs from before it \
                 pick them up with `. ~/.aterm/shell.d/00-atpkg.bash`"
                    .into(),
            ),
            (
                &tools,
                None,
                1,
                HookDialect::Fish,
                "ay, trust — ready in every tab opened since this update · 1 tab from before it \
                 picks them up with `source ~/.aterm/shell.d/00-atpkg.fish`"
                    .into(),
            ),
            (
                &tools,
                None,
                1,
                HookDialect::PowerShell,
                "ay, trust — ready in every tab opened since this update · 1 tab from before it \
                 picks them up with `. ~/.aterm/shell.d/00-atpkg.ps1`"
                    .into(),
            ),
            (
                &tools,
                None,
                1,
                HookDialect::None,
                "ay, trust — ready in every tab opened since this update · 1 tab from before \
                 it: a new tab picks them up"
                    .into(),
            ),
            (
                &agents,
                Some(Si::Prepared),
                0,
                HookDialect::Zsh,
                "claude, codex — ready in every aterm tab".into(),
            ),
            (
                &agents[..1],
                None,
                0,
                HookDialect::Zsh,
                "claude — ready in every aterm tab".into(),
            ),
            (
                &agents,
                Some(Si::WriteFailed(
                    "Read-only file system (os error 30)".into(),
                )),
                0,
                HookDialect::Zsh,
                "claude, codex — not wired into this window's tabs: aterm couldn't write its \
                 shell loader (Read-only file system (os error 30))"
                    .into(),
            ),
            (
                &agents,
                Some(Si::UnknownShell("nu".into())),
                0,
                HookDialect::Zsh,
                format!("claude, codex — {}", no_shell("nu")),
            ),
            (
                &agents,
                Some(Si::Prepared),
                2,
                HookDialect::Zsh,
                "claude, codex — ready in every tab opened since this update".into(),
            ),
            (
                &agents[..1],
                None,
                1,
                HookDialect::Zsh,
                "claude — ready in every tab opened since this update".into(),
            ),
            (
                &mixed,
                Some(Si::Prepared),
                0,
                HookDialect::Zsh,
                "ay, claude, trust — ready in every aterm tab".into(),
            ),
            (
                &mixed,
                None,
                1,
                HookDialect::Zsh,
                "ay, claude, trust — ready in every tab opened since this update · 1 tab from \
                 before it picks them up with `. ~/.aterm/shell.d/00-atpkg.zsh`"
                    .into(),
            ),
            (
                &mixed,
                Some(Si::UnknownShell("fish".into())),
                1,
                HookDialect::Zsh,
                format!("ay, claude, trust — {}", no_shell("fish")),
            ),
        ];
        for (roster, outcome, frozen, hook, want) in goldens {
            assert_eq!(
                seed_pill_text(roster, outcome.as_ref(), frozen, hook),
                format!("{PREFIX}{want}"),
                "{roster:?} {outcome:?} frozen={frozen} {hook:?}"
            );
        }
        // The record strips the pill's opening and keeps the rest whole, remedy
        // included (a `Hold::LogOnly` record).
        let pill = seed_pill_text(&tools, None, 1, HookDialect::Zsh);
        let record = installed(&pill);
        assert_eq!(record.hold, Hold::LogOnly, "no success row");
        assert_eq!(
            record.detail[0],
            "ay, trust — ready in every tab opened since this update · 1 tab from before it \
             picks them up with `. ~/.aterm/shell.d/00-atpkg.zsh`"
        );
        // The hook's file name is atpkg's, never a spelling of our own.
        assert!(
            HookDialect::Zsh
                .remedy()
                .unwrap()
                .contains(atpkg::hooks::HOOK_BASENAME)
        );
    }

    /// Machine settings: one record per changed item, in wire order. An
    /// unknown item is its own title, verbatim; each item has its own key.
    #[test]
    fn machine_settings_record_one_entry_per_item_in_wire_order() {
        let rows = machine_settings(
            "spotlight-noindex 1 dir(s) migrated; something-new tuned; universal-control disabled",
        );
        let titles: Vec<&str> = rows.iter().map(|m| m.title.as_str()).collect();
        assert_eq!(
            titles,
            [
                "Spotlight now skips 1 build folder",
                "something-new tuned",
                "ALab tools turned off Universal Control",
            ],
            "wire order"
        );
        let keys: Vec<&str> = rows.iter().map(|m| m.key.as_deref().unwrap()).collect();
        assert_eq!(
            keys,
            [
                "toolchain.machine.spotlight-noindex",
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
        // The undo record's detail: the switch that makes the revert hold, then
        // the revert command, byte-identical to what `aterm pkg machine` prints.
        assert_eq!(
            rows[2].detail,
            vec![
                "to turn it back on, set Universal Control on this Mac to leave and run: \
                 defaults -currentHost delete com.apple.universalcontrol Disable; \
                 defaults -currentHost delete com.apple.universalcontrol DisableMagicEdges"
            ]
        );
        assert!(
            rows[2].detail[0].ends_with(atpkg::machine::UNIVERSAL_CONTROL_REVERT),
            "the command whole, last"
        );
        assert_eq!(rows[2].actions, vec![security()], "where the switch is");
        assert_eq!(
            atpkg::machine::UNIVERSAL_CONTROL_REVERT,
            "defaults -currentHost delete com.apple.universalcontrol Disable; \
             defaults -currentHost delete com.apple.universalcontrol DisableMagicEdges"
        );
        assert!(
            machine_settings(" ; ").is_empty(),
            "an empty body raises nothing"
        );
    }

    /// The declared load is the heaviest active phase (an extraction or link
    /// loads the disk, else a verification the CPU, else a download the
    /// network), not the phase the detail line names. A transfer's stats keep
    /// one width from its first megabyte to its last.
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
        assert_eq!(byte_stats(24_000_000, 200_000_000), " 24 MB / 200 MB");
        assert_eq!(byte_stats(512_000_000, 1_200_000_000), "512 MB / 1.2 GB");
        assert_eq!(
            byte_stats(900_000_000, 74_000_000),
            "74 MB / 74 MB",
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
        assert_eq!(HEAVY_PASS_BYTES, 250_000_000);
    }
}
