// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE TOOLCHAIN LANE'S WORDS — every sentence the ALab-toolchain status bar
//! used to compose (the retired `status_bars.rs`, the toolchain half), as PURE builders
//! that return an [`aterm_messages::Message`] for the
//! message center to hold (docs/DESIGN-unified-messages-2026-09-21.md §3.1,
//! §6 R25–R34). Nothing here keeps state, reads a clock or paints a cell:
//! the wake arms in `lib.rs` call a builder and `post`/`restate`; the center
//! ranks, holds, supersedes and logs; the band paints.
//!
//! # What may be claimed
//!
//! * A snapshot renders ONLY what [`crate::PkgProgressSnapshot`] supports (the
//!   classified read of `<prefix>/progress.json`): a not-running snapshot
//!   names its terminal outcome and never a live phase; an unknown schema
//!   `v` is the first-run title alone ("Installing ALab tools"), busy — the
//!   comet says it is ongoing; the title carries no ellipsis. Program names are
//!   untrusted until they round-trip [`atpkg::store::ToolName`]; error text is
//!   control-stripped and capped ([`atpkg::progress::sanitize_for_tty`]) —
//!   and the center's own `normalized` re-clips every field at ingress.
//! * Every sentence says what happened in plain words (2026-09-23; design
//!   ruling 67 of the ux/status-reporting merge): the record TITLES name the
//!   outcome in the lane's one noun — "ALab tools update postponed", "ALab
//!   tools installed", "ALab tools checked", "Couldn't update ALab tools", "ALab
//!   tools not installed", and a first run's "Couldn't install ALab tools" —
//!   where every entry used to read "ALab toolchain" or "Package update", and
//!   the first-run row is "Installing ALab tools" (one noun on every surface).
//!
//! # Silent by default: the lane has ONE row (upstream 2026-09-22)
//!
//! Every appearance of a row re-grids every window and sends a resize to
//! every running TUI, so the owner's direction of 2026-09-22 — package
//! updates "non-interrupting (ideally silent)", their record "reviewed in
//! settings where it doesn't interrupt the working user"
//! (docs/DESIGN-atpkg-vendor-direct-updates-2026-09-22.md, upstream
//! dbf97ecff + ac7942234) — leaves the toolchain lane its work in flight
//! and one failure on the glass, nothing else:
//!
//! * the FIRST-RUN live row, `Installing ALab tools` ([`announced`], the
//!   live [`snapshot_words`]), while the default set is laid onto a machine
//!   whose store held none of the ALab set when the pass began (multi-GB;
//!   silence there would read as a hang). It opens once and folds once, at
//!   the child's exit;
//! * a HEAVY routine pass (≥ [`HEAVY_PASS_BYTES`], ruling 144), `Updating ALab
//!   tools`: very heavy system use the person should see explained (design
//!   §10's rule; the host's `messages_host` decides which pass is heavy);
//! * a first run that ENDED SHORT, a failure row ([`first_run_short`],
//!   ruling 118, titled in main's words — ruling 144): the multi-gigabyte
//!   deliverable the first-run row promised did not arrive;
//! * a PERSON'S Settings ▸ Packages verb (Check, Install, Remove; design
//!   rulings 224, 225): they pressed it and wait for it, so its pass takes
//!   the row whatever it weighs (`message_reporters::packages_verb_row`,
//!   then this module's live words once the plan lands).
//!
//! Everything else — a light routine pass's announcement and meter, every
//! other pass outcome ([`installed`], [`ended`], [`failed`],
//! [`not_installed`]), [`managed_current`], [`machine_settings`], a
//! [`deferred`] pass, and every [`appnotice`] text (R34; ruling 147) — is a
//! [`Hold::LogOnly`] RECORD: `appstatus` lists it as a finished activity
//! with the words and outcome the bars' ledger gave it, Settings ▸ Messages
//! and `messages.log` keep it, and the glass never shows it. A routine
//! pass's failure is the Settings ▸ Packages badge (`packages_screen`). This
//! supersedes the design's §6 R25–R33 glass behaviour (the rulings on the
//! origin/main merge, 2026-09-23, and on the 2026-09-24 merge).
//!
//! # Keys
//!
//! Everything a pass says about itself — announced, the live meter, the
//! outcome records — carries [`KEY_PASS`], so a newer live report
//! SUPERSEDES the older row in its slot (the center keeps the slot, logs the
//! old row's final words); a record is never live, so its key only names
//! it in the log. The managed-current record and each machine-settings item
//! carry keys of their own.

use aterm_messages::text::shape_detail;
use aterm_messages::{
    Amount, DETAIL_LINE_CAP, Glyph, Hold, Intent, Load, Message, Meter, STALE_ANNOUNCE,
    STALE_TAILED, Severity, Tag, Unit, tags,
};
use atpkg::progress::{PROGRESS_VERSION, Phase, sanitize_for_tty};

/// The supersede key every report about ONE pass carries: announcement,
/// live meter, and the installed / ended / failed records.
pub(crate) const KEY_PASS: &str = "toolchain.pass";
/// The managed-current record's key (R32).
pub(crate) const KEY_MANAGED: &str = "toolchain.managed";
/// A ROUTINE pass planned at least this many bytes is VERY HEAVY system use
/// (design §10.6, ruling 84): its live row — `Updating ALab tools` (ruling
/// 144) — reaches the glass after the progress grace and folds at the
/// child's exit, like a first run's. A lighter routine pass stays silent
/// (ruling 34).
pub(crate) const HEAVY_PASS_BYTES: u64 = 250_000_000;
/// The prefix of a machine-settings item's key (R33): `toolchain.machine.`
/// plus the item's first word (`universal-control`, `spotlight-noindex`).
pub(crate) const KEY_MACHINE_PREFIX: &str = "toolchain.machine.";

/// Cap on a sanitized failure reason inside a row.
const ERROR_CAP: usize = 60;
/// Cap on a program name inside a row (the store's own names are short).
const NAME_CAP: usize = 24;

/// The first-run toolchain row's title — ONE NOUN on every surface (2026-09-23:
/// the bar said "ALab toolchain" while Settings said "ALab toolset").
pub(crate) const INSTALLING_ALAB: &str = "Installing ALab tools";
/// A heavy ROUTINE pass's live row (ruling 144): the same noun, and the verb
/// says it updates what is already there.
pub(crate) const UPDATING_ALAB: &str = "Updating ALab tools";
/// The toolchain RECORDS' titles, one per outcome (2026-09-23 audit: every entry
/// read "ALab toolchain", so `appstatus` and the record could not tell a
/// deferral, a failure and an install apart by title).
pub(crate) const PACKAGES_POSTPONED: &str = "ALab tools update postponed";
pub(crate) const PACKAGES_INSTALLED: &str = "ALab tools installed";
/// A pass that ended well with no roster (`seed-done:`): atpkg's sentence
/// states what is installed, not what the pass changed, so the title claims
/// only the check (review 2026-09-24; a first run's is [`PACKAGES_INSTALLED`]).
pub(crate) const PACKAGES_FINISHED: &str = "ALab tools checked";
pub(crate) const PACKAGES_FAILED: &str = "Couldn't update ALab tools";
/// A FIRST RUN that failed (its row and record): it was an install, not an
/// update, and the lane's noun stays ALab tools (audit 2026-09-24).
pub(crate) const ALAB_INSTALL_FAILED: &str = "Couldn't install ALab tools";
/// …and for a pass that ended with no ALab tools on the disk and no failure seen
/// (nothing is published for this Mac, the set was removed or excluded, a pass that
/// exited well over an empty store): "failed" would claim a failure nobody saw.
pub(crate) const PACKAGES_NOT_INSTALLED: &str = "Couldn't install any ALab tools";

/// `⇣` — a pass moving bytes.
const DOWN: char = '\u{21e3}';
/// `ℹ` — a RECORD of the lane at work: a pass put off, a machine setting a
/// pass changed (design ruling 302: a record wears its outcome's mark; the
/// `⏸` and `↻` these wore are a moving row's, and in the log they read as
/// work still under way).
const NOTED: char = '\u{2139}';

/// The pass name and start stamp that identify one `progress.json` writer,
/// so a live meter is clamped to its OWN pass's high-water mark and never
/// inherits a different pass's ([`snapshot_words`]).
pub(crate) type PassId = (String, u64);

/// The `Packages` capsule every toolchain RECORD carries: the page that holds
/// the durable record a press used to open. The LIVE rows carry none
/// ([`live_row`]).
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

/// A toolchain row: tag, severity, title, the `Packages` capsule.
fn row(severity: Severity, title: impl AsRef<str>) -> Message {
    Message::new(tags::TOOLCHAIN, severity, title).action(packages())
}

/// A LIVE toolchain row — the first run's announcement and its meter, a heavy
/// routine pass: tag, severity, title, and NO capsule. A live row is work in
/// flight, not a decision: `Details ›` leads, and the eleven cells a
/// `Packages` chip took keep the title whole at 60 columns beside its load
/// words and `Details ›` whole at 80 (review round 2, 2026-09-23 — at 80 the
/// row read `Packages   ›`, at 60 `Installing AL…`).
fn live_row(title: impl AsRef<str>) -> Message {
    // The pass IS a download first: its `network busy` says nothing the
    // title does not, and shows only while other work is measurably slowed;
    // the extraction's disk and the verification's CPU show as before
    // (ruling 246).
    Message::new(tags::TOOLCHAIN, Severity::Info, title)
        .loads(PASS_LOADS)
        .primary_load(Load::Network)
}

/// The loads a pass row may show over its life ([`pass_load`] and the
/// announcement's network): its load slot is reserved at the widest of
/// their words, so a phase change never re-lays it (design ruling 221).
pub(crate) const PASS_LOADS: [Load; 3] = [Load::Network, Load::Disk, Load::Cpu];

/// A toolchain RECORD (the silent lane, module doc): a row's words with
/// [`Hold::LogOnly`] — recorded in the log and on `appstatus` (a finished
/// activity, `outcome=warn` ⇔ Warn), never on glass. `detail` is the bars'
/// ledger detail, byte for byte; an empty one is no line.
fn record(severity: Severity, title: impl AsRef<str>, detail: String) -> Message {
    row(severity, title).line(detail).hold(Hold::LogOnly)
}

/// A record whose detail is atpkg's sentence, kept WHOLE across lines
/// (design ruling 64: no reporter pre-cuts a sentence it received). A record
/// is never painted, so the bar's old 160-character cut has no reason left
/// (audit 2026-09-24).
fn whole_record(severity: Severity, title: &str, detail: &str) -> Message {
    row(severity, title).sentence(detail).hold(Hold::LogOnly)
}

/// WHICH `~/.aterm/shell.d/00-atpkg.*` A FROZEN TAB SOURCES (2026-09-16). The
/// first cut of the frozen-tab note said `exec $SHELL`, and review showed that
/// to be the wrong remedy twice over: aterm delivers its zsh integration through
/// a `ZDOTDIR` wrapper that restores or unsets `ZDOTDIR` before the user's rc
/// runs (`aterm-shell-integration`'s `ZSH_WRAPPER`; bash rides `--rcfile`), so
/// the re-exec'd plain shell loads no integration — the tab silently loses its
/// OSC 133/633 marks, cwd/title tracking and the live PATH re-assert — and it
/// heals PATH at all only through the marker block `atpkg` writes into an rc
/// file that already EXISTS (`hooks::ensure_rc_sources_hooks` never creates one,
/// skips protected and symlinked rcs, and honours the block's own opt-out), so
/// on a fresh Mac with no `~/.zshrc` the promise was simply false. What atpkg's
/// own rc block does is `if [ -f "<hook>" ]; then . "<hook>"; fi`; sourcing that
/// hook in the live shell is the whole remedy — it moves `<prefix>/agents/` to
/// the front of PATH, exports `ATPKG_AGENTS` and appends `bin/`, idempotently —
/// and the tab keeps the integration it has. The file names are pinned from
/// `crates/atpkg/src/hooks.rs` (`HOOK_BASENAME`, one file per dialect; POSIX
/// `.` for zsh and bash, fish's `source`, PowerShell's dot-source), written by
/// every pass before the marker this row is built from is printed. A shell
/// atpkg has no hook for ([`HookDialect::None`]: nushell, xonsh, cmd) gets no
/// command; for it the honest note is that a new tab picks the programs up.
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
            // A WSL tab runs a Linux bash (`WSL_LAUNCH_SH`, `--rcfile` the bash
            // integration), and that bash sources `$HOME/.aterm/shell.d` in the
            // DISTRO's home — where atpkg, running on the Windows host, writes
            // nothing (its hooks land in the Windows profile, with Windows paths
            // in them). `. ~/.aterm/shell.d/00-atpkg.bash` in such a tab is
            // "No such file", so it is not the remedy (review, 2026-09-16: the
            // mapping to `Bash` was proposed and refuted on this ground).
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
    /// `~/.aterm/shell.d/<HOOK_BASENAME>.<ext>`. THE ONE SPELLING: the seed
    /// pill (`seed_pill_text` in `lib.rs`) reads it from here too, since
    /// 2026-09-16's review closed the restated copy it used to carry.
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

/// Human byte figure, decimal units like every download dialog: `812 KB`,
/// `512 MB`, `1.2 GB`. A figure that would ROUND to 1000 of a unit is the
/// next unit up (`999.6 MB` is `1.0 GB`, never `1000 MB`), so no figure is
/// wider than six cells below a terabyte.
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

/// `done / total` bytes as a meter's stats, `done` right-aligned to the
/// total's width: a stats string that keeps ONE width for the life of the
/// transfer, so the width law lays the row out once — `24 MB / 200 MB`
/// becoming `114 MB / 200 MB` must not flip the row between keeping its
/// stats and growing its meter mid-download — and the digits tick in place.
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

/// R25 — `atpkg` announced a FIRST-RUN pass (`net-starting:` from a child the
/// lane tagged first run): the row opens NOW, before `progress.json` exists,
/// because the extraction that follows is minutes long and gigabytes wide and
/// an app doing that silently is indistinguishable from one that is
/// misbehaving. Work in flight and very heavy use (ruling 144): BUSY (no
/// fraction yet), the SIZE atpkg computed as its stats — how big the wait is
/// — and the network declared as its load; the title alone on the glass and
/// NO capsule (ruling 101: work in flight is not a decision). atpkg's size is
/// the only detail line ("about 3 GB on disk when finished" — no
/// "starting…", and no line at all without a size, 2026-09-23). A routine
/// pass's announcement is a log line and never reaches here (module doc).
/// Live under the announcement cap ([`STALE_ANNOUNCE`]):
/// the child's exit normally folds it; if none is ever reported it folds on
/// its own.
pub(crate) fn announced(detail: &str) -> Message {
    // The size is atpkg's closing ` (…)` clause — spaced, so the `(s)` of
    // `program(s)` is never read as one.
    let size = detail
        .rsplit_once(" (")
        .and_then(|(_, t)| t.strip_suffix(')'))
        .map(|s| sanitize_for_tty(s, 40))
        .filter(|s| !s.is_empty());
    let stats = announced_stats(detail, size.as_deref());
    live_row(INSTALLING_ALAB)
        .glyph(glyph(DOWN))
        .line(size.unwrap_or_default())
        .no_excerpt()
        .meter(Meter {
            stats,
            load: Some(Load::Network),
            ..Meter::busy("")
        })
        .hold(Hold::Live {
            stale_after: STALE_ANNOUNCE,
        })
        .key(KEY_PASS)
}

/// The announcement's stats, terse: `10 programs · ~3 GB` from atpkg's
/// `installing 10 ALab program(s) … (about 3 GB on disk when finished)` —
/// how big the wait is, without the clause (review 2026-09-23); the whole
/// sentence stays behind Details.
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

/// R30 — the wait ran out (atpkg exit 75): the pass is DEFERRED — the loop
/// retries on its short backoff, or parks an hour when the holder looks wedged
/// (never the interval) — and `detail` says which. Routine: an Info RECORD
/// ("ALab tools update postponed", atpkg's sentence), the `ℹ` of a record, never the
/// word "failed" and never a row on glass (2026-09-22). A stand-down that will
/// not retry is the Packages badge as well (the host's `PkgLockTimedOut {
/// stands_down }` arm). No key.
pub(crate) fn deferred(detail: &str) -> Message {
    whole_record(Severity::Info, PACKAGES_POSTPONED, detail).glyph(glyph(NOTED))
}

/// The installed pill's sentence. `frozen_tabs` is `App::frozen_path_tabs`: the
/// live tabs adopted from a build whose sessions lack the managed `agents/`;
/// `hook` is the dialect of the atpkg hook such a tab sources to catch up
/// (`HookDialect::remedy` — the managed row's own spelling, ONE source since
/// review 2026-09-16 closed the restated copy this file carried; the window's
/// spawn shell). The frozen clause is joined with the row's ` · ` so the
/// width law that keeps the row's remedy ([`shape_detail`], which
/// knows ". " and " · " as sentence joints) keeps the pill's too — the first
/// cut joined it with `;`/`:`, and at 90–130 cols the shaper kept the command
/// and dropped the qualifier, leaving "ready in every tab opened… `. ~/…`" —
/// the opposite meaning (review, 2026-09-16).
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
    // NOTHING ON THIS ROSTER SENDS ANYONE TO A NEW TAB (2026-09-16). Owner, that
    // day, of a surface that did: "aterm atpkg DID install the latest but it didn't
    // make them available for me. instead, it is telling me to open a new tab. NO!
    // all the latest and best MUST WORK IN THE SAME TAB with live update! fix this
    // and this message and audit that this is the actual behavior."
    //
    // The agent programs (`claude`/`codex`) are laid into `<prefix>/agents/`, which
    // the spawn seam puts in FRONT of every session's PATH and ensures at launch
    // (`spawn::managed_agents_dir`), and atpkg re-lays the twins in place — the
    // next invocation in any tab runs the build just installed. The REST of the
    // roster (`ay`, `trust`, `clean`, …) lives in `<prefix>/bin/`, which reaches a
    // tab through the atpkg hook `hooks::refresh` writes to `~/.aterm/shell.d`
    // AFTER this install — and until 2026-09-16 the shell integration sourced that
    // directory ONCE, at shell startup, so the tab showing this pill had no `bin/`
    // on PATH and `ay` there was "command not found": the pill said "open a new tab
    // to use them", and that clause was the truth of its day. It is not any more.
    // The integration's `__aterm_managed_path_live` (all four scripts) runs at
    // EVERY precmd and preexec: it sources the hook the moment the file exists
    // (`$ATPKG_AGENTS` unset; zsh also on the hook's zstat stamp changing) and
    // re-asserts `reroute/` and `agents/` in front, and the hook appends `bin/`
    // unconditionally — so the tab on glass has every program on the roster at
    // its next prompt, and the pill says so for the whole roster (verified on the
    // owner's machine, 2026-09-16: in the tab that had been told to open a new
    // one, `. ~/.aterm/shell.d/00-atpkg.zsh` put `<prefix>/agents` at the head of
    // PATH and `which claude` moved from `~/.local/bin` to the managed twin, with
    // the integration intact — which is exactly what the live path does unasked).
    //
    // The one honest narrowing is the tab that may still run the PRE-2026-09-16
    // script: adopted across the update from a build whose sessions lack the
    // live re-assert (`App::frozen_path_tabs`, the count the managed row also
    // carries). With such tabs alive the claim is the tabs opened since, and the
    // frozen ones are named with their in-place remedy — sourcing the hook,
    // spelled for the window's spawn shell (`HookDialect::remedy`; never `exec $SHELL`,
    // which drops the tab's integration: `toolchain_words::HookDialect`). A roster
    // of agent programs alone keeps naming them (the managed row that follows
    // carries the frozen count and the remedy for those).
    //
    // …AND ONLY WHEN THE INTEGRATION RUNS IN THIS TAB (review, 2026-09-16). The
    // seam's front position is not final: `/etc/zprofile`'s `path_helper` rebuilds
    // PATH and an rc line `export PATH="$HOME/.local/bin:$PATH"` — the native
    // installer's own line — goes in front of it (hooks.rs, measured on m27:
    // `agents/` at position 14). What restores the order is the shell integration
    // re-asserting it after the rc ran and sourcing the hook as it lands; a tab
    // whose integration FAILED (unknown shell, unwritable loader cache — the
    // recorded runtime outcome, not the advertised capability) has neither: no
    // `bin/` ever, and `claude` is whichever copy the rc put first. For that
    // outcome nothing is promised — say what is true, and point at the fix surface.
    use crate::spawn::ShellIntegrationOutcome as Si;
    if matches!(
        shell_integration,
        Some(Si::UnknownShell(_) | Si::WriteFailed(_))
    ) {
        return format!(
            "✓ ALab toolchain installed: {names} — but this shell isn't hooked up to \
             them; see Settings ▸ Packages"
        );
    }
    let agents = installed
        .iter()
        .map(String::as_str)
        .filter(|name| atpkg::stub::is_agent_program(name))
        .collect::<Vec<_>>();
    if !agents.is_empty() && agents.len() == installed.len() {
        let clause = match agents.as_slice() {
            [one] => format!("{one} already runs"),
            [head @ .., last] => format!("{} and {last} already run", head.join(", ")),
            [] => unreachable!("checked non-empty"),
        };
        let clause = if frozen_tabs == 0 {
            format!("{clause} in every aterm tab, this one too")
        } else {
            let clause = clause.replace(" already run", " run");
            format!("{clause} in every tab opened since this update")
        };
        return format!("✓ ALab toolchain installed: {names} — {clause}");
    }
    if frozen_tabs == 0 {
        return format!(
            "✓ ALab toolchain installed: {names} — ready in every aterm tab, this one too"
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

/// R27 — `net-installed:`: the toolchain is here. `text`
/// is the same sentence the pill used to carry (roster + the readiness clause
/// — "ready in every aterm tab, this one too" since 2026-09-16, never "open a
/// new tab" — or the shell-integration caveat), authored by the caller. The
/// sentence was authored for a pill with no title of its own; the record has
/// one, so its opening is not repeated as the detail. NO SUCCESS ROW
/// (2026-09-22): a Success RECORD — the first-run row folds at its child's
/// exit and the roster is the record's. No meter (ruling 19). Titled "ALab
/// tools installed".
pub(crate) fn installed(text: &str) -> Message {
    let detail = text
        .strip_prefix("\u{2713} ALab toolchain installed: ")
        .unwrap_or(text);
    whole_record(Severity::Success, PACKAGES_INSTALLED, detail).key(KEY_PASS)
}

/// R28 — `seed-done:`: THE POSITIVE TERMINAL — an announced pass that ended
/// well and has no install roster to name. Distinct from [`installed`], which
/// claims a roster; this one claims nothing beyond the sentence atpkg itself
/// printed — a Success RECORD ("ALab tools checked"), never a row
/// (2026-09-22). `first_run`: the latched first run's pass, which was an
/// install — "ALab tools installed", as [`install_failed`] is its failure.
pub(crate) fn ended(detail: &str, first_run: bool) -> Message {
    let title = if first_run {
        PACKAGES_INSTALLED
    } else {
        PACKAGES_FINISHED
    };
    whole_record(Severity::Success, title, detail).key(KEY_PASS)
}

/// R29 — a bad terminal outcome for the toolchain lane (`seed-failed:` /
/// `net-failed:` / `seed-unusable:` / a coherence group's
/// abort / the synthetic "child died after announcing"). `what` is the whole
/// sentence the bars' ledger recorded — atpkg's clipped cause or the fixed
/// verdict (the host's `failure_cause`); the `Packages` capsule is the pointer
/// to the page, so the sentence does not name it again (audit 2026-09-24).
/// Never for a store-lock wait's timeout — that is [`deferred`] (2026-09-10).
///
/// NO ROW (2026-09-22): a Warn RECORD, and the failure's surface is the
/// Settings ▸ Packages badge (`PackagesService::note_pass_trouble`). A
/// failure never carries a meter (the 2026-09-11 screenshot of "failed"
/// beside a full bar inherited from another pass's reading): with no row,
/// no meter can be fused with a verdict at all, in either order. Titled
/// "Couldn't update ALab tools"; the reason is kept WHOLE ([`whole_reason`]).
pub(crate) fn failed(what: &str) -> Message {
    bad_outcome(PACKAGES_FAILED, what)
}

/// [`failed`] for a FIRST RUN: the record the first-run failure row stands
/// beside says what that row says — an install, in the lane's noun.
pub(crate) fn install_failed(what: &str) -> Message {
    bad_outcome(ALAB_INSTALL_FAILED, what)
}

/// A pass that left NO ALab tools on the disk and saw no failure
/// (`seed-nothing:`, `seed-unusable:` — nothing published for this Mac, the set
/// removed or excluded): "Couldn't install any ALab tools", a Warn RECORD shaped like
/// [`failed`] — never the word "failed", which would claim a failure nobody
/// saw. The reason is kept whole.
pub(crate) fn not_installed(what: &str) -> Message {
    bad_outcome(PACKAGES_NOT_INSTALLED, what)
}

/// The Warn record [`failed`] and [`not_installed`] share: keyed to the pass,
/// the `Packages` capsule, the reason as whole lines ([`whole_reason`]).
fn bad_outcome(title: &str, what: &str) -> Message {
    row(Severity::Warn, title)
        .lines(whole_reason(what))
        .hold(Hold::LogOnly)
        .key(KEY_PASS)
}

/// A failure's reason is worth the line, KEPT WHOLE (2026-09-23; design
/// rulings 44 and 63): a child that died after announcing leaves only its
/// stderr, and the 160-character cut lost it. One detail line per line of
/// the reason, and a line past the engine's line cap split at its joints
/// with every word kept (`aterm_messages::text::split_sentence`, design
/// ruling 64: no reporter pre-cuts a sentence it received — a cap-length cut
/// lost the tail, and with it the `see Settings ▸ Packages` the host ends
/// the sentence with); the center keeps at most `DETAIL_LINES_CAP` of them.
/// packages.log has the routine outcomes, so only these keep the whole text.
fn whole_reason(what: &str) -> Vec<String> {
    what.lines()
        .flat_map(|l| aterm_messages::text::split_sentence(l, DETAIL_LINE_CAP))
        .collect()
}

/// How a FIRST RUN ended short (the marker its child printed): what the
/// failure row after it says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FirstRunShort {
    /// `seed-failed:` / `net-failed:` (or a pass that could not launch).
    Failed,
    /// `seed-nothing:` or a non-informational `seed-unusable:`: the pass ran
    /// and nothing is installed.
    Nothing,
}

/// A cause the person can act on themselves, so it is painted: a full disk.
fn actionable_cause(cause: &str) -> Option<&'static str> {
    let lower = cause.to_ascii_lowercase();
    ["no space left", "disk full", "not enough space", "enospc"]
        .iter()
        .any(|needle| lower.contains(needle))
        .then_some("no space left on device")
}

/// R29f — a FIRST RUN that ended short (review 2026-09-24, amends ruling 34
/// for the first run only). The first-run row told the person to wait
/// minutes for a multi-gigabyte deliverable; when it does not arrive, a 0.6 s
/// `failed` echo and a record were all they got, and a screen reader got
/// nothing. So a FAILURE ROW the person acts on — retry from Settings ▸
/// Packages, free the disk — keyed by the pass, `Packages` its capsule, the
/// default hold. The cause rides behind Details unless it is one the person
/// fixes themselves ([`actionable_cause`]), and it is kept WHOLE
/// ([`whole_reason`], rulings 63 and 144) — the row and its record say the
/// same cause, every word of it. The record ([`failed`]) and the
/// Packages badge stay as they are; a heavy ROUTINE pass stays a record,
/// since the versions already installed keep working. Tagged `packages`,
/// like the admin install's failure: `appstatus` lists the TOOLCHAIN lane's
/// unretired rows as `phase=live` work, and this row is an outcome, not work
/// in flight (the record carries the lane's `phase=done` line).
pub(crate) fn first_run_short(how: FirstRunShort, cause: &str) -> Message {
    let (severity, title) = match how {
        // One severity for the family (ruling 309): a first run that failed
        // left the Mac as it was and the page retries it, as a first run
        // that installed nothing — a warning both, where the pair read Error
        // above Warn for the same `Couldn't install … ALab tools`.
        FirstRunShort::Failed => (Severity::Warn, ALAB_INSTALL_FAILED),
        FirstRunShort::Nothing => (Severity::Warn, PACKAGES_NOT_INSTALLED),
    };
    let msg = Message::new(tags::PACKAGES, severity, title)
        .action(packages())
        .key(KEY_PASS);
    match actionable_cause(cause) {
        Some(fix) => msg.line(fix).lines(whole_reason(cause)),
        None => msg.lines(whole_reason(cause)).no_excerpt(),
    }
}

/// R34 — a plain text posted from OUTSIDE the process (`aterm ctl
/// appnotice <lane> <text>`): the voice of an `aterm pkg install claude` run
/// in a terminal, which has no GUI child to stream markers through. It has
/// no severity, no action and no progress, so it is FYI by construction: a
/// RECORD (design §10.3 T10, H7) — Settings ▸ Messages and `appstatus` keep
/// it, the glass never shows it. `tag` names the lane (`toolchain` or
/// `update`).
pub(crate) fn appnotice(tag: Tag, text: &str) -> Message {
    let text = sanitize_for_tty(text, 120);
    match installed_words(&text) {
        // `aterm pkg install claude: 2.1.267 installed` is a command's
        // transcript line, not a title (ruling 261): the program as a person
        // knows it and what happened — `Claude Code 2.1.267 installed` — the
        // line itself behind it.
        Some(title) => Message::new(tag, Severity::Info, title)
            .line(text)
            .hold(Hold::LogOnly),
        None => Message::new(tag, Severity::Info, text).hold(Hold::LogOnly),
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
// The managed-current and machine-settings rows (R32, R33).
// ---------------------------------------------------------------------------

/// R32 — `managed-current:`: every AGENT program (claude, codex) that is
/// installed AND at its vendor's latest, as atpkg lists it: `claude 2.1.280
/// (Anthropic latest); codex 0.156.0 (OpenAI latest)` (an older atpkg wrote
/// `(build <N>)` in the parentheses — upstream cb26ff4d1; it still parses). The
/// record's TITLE names the programs the way a person knows them and says what
/// the marker means — "Claude Code 2.1.280 and Codex 0.156.1 are current";
/// the detail says where they are used ([`managed_current_words`]). A Success
/// RECORD, keyed.
///
/// `frozen_tabs` (2026-09-16) is how many live tabs were adopted across the
/// update from a build whose sessions have no managed `agents/` on PATH
/// (`App::frozen_path_tabs`); for those the detail is the in-place remedy —
/// sourcing the atpkg hook, spelled for `hook`'s dialect — instead of a claim
/// about every tab. `this_tab_hooked` (review, 2026-09-16) is whether the
/// shell integration RUNS in this window's tabs (`App::this_tab_hooked`, the
/// recorded runtime outcome): only its per-prompt re-assert keeps `agents/`
/// in front once the user's rc has run, so a window whose integration failed
/// is not told "every tab" — the same honesty the seed pill keeps, on the
/// same evidence.
///
/// NEVER A ROW ON GLASS (2026-09-22): atpkg prints the marker at the end of
/// every pass, and the row it used to raise still moved the grid under
/// running TUIs for news that changed nothing they were doing. The host
/// records it only when its words change (`App::post_managed_current`,
/// design ruling 58). `None` for an empty body. The window posts through
/// [`managed_current_for`], which also knows the tabs running an agent and the
/// sessions the live upgrade is moving; this is its frozen-shells-only form.
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
        this_tab_hooked,
        hook,
    )
}

/// [`managed_current`] over the whole of [`TabsBehind`].
pub(crate) fn managed_current_for(
    text: &str,
    tabs: TabsBehind,
    this_tab_hooked: bool,
    hook: HookDialect,
) -> Option<Message> {
    let (title, detail) = managed_current_words_for(text, tabs, this_tab_hooked, hook)?;
    Some(record(Severity::Success, title, detail).key(KEY_MANAGED))
}

/// HOW THE LIVE TABS STAND against the programs the managed-current record
/// names (gap audit 2026-09-24). Until then the record knew one count — tabs
/// carrying the adoption mark — and told the owner to type the atpkg hook
/// into every one of them, while calling everything "up to date". Measured
/// that day: the only marked tab had been healed by the live upgrade's
/// relaunch line a day earlier, its foreground was Claude (a typed hook would
/// have been a prompt), and two Claude sessions ran 2.1.280 and 2.1.281 under
/// a record that said "Claude Code 2.1.282 … up to date".
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct TabsBehind {
    /// Tabs whose shell runs a frozen PATH — measured, or carried and not yet
    /// measured — with no agent in front: the hook remedy is typed there.
    pub(crate) frozen_shells: usize,
    /// Frozen tabs whose foreground IS an agent (`claude`, `codex`) that the
    /// live upgrade is not moving: never told to type into (it is a prompt
    /// there), told to source the hook once the agent exits.
    pub(crate) frozen_agents: usize,
    /// Running Claude Code sessions the live upgrade moves onto THE VERSION
    /// THIS RECORD NAMES at their next turn end, on its own (its relaunch
    /// line sources the hook too, so a frozen tab among them heals with the
    /// move). Never a stalled one (review of 2026-09-25: an overdue session
    /// was said to move "at its next turn end" here while the band showed it
    /// stalled), and never one moving to another version.
    pub(crate) agents_behind: usize,
    /// Running Claude Code sessions behind that same version that will NOT move
    /// on their own: stalled (the band has a row for each), or held by the
    /// owner's `--defer`/`--skip`. They keep the title at "installed" — "up
    /// to date" is not true of them — and are pointed at `--status`.
    pub(crate) agents_held: usize,
    /// Live tabs carrying NO adoption mark whose shell's PATH was MEASURED,
    /// two jobs in a row, to put a foreign `claude`/`codex` first
    /// (`SessionTimeline::path_settled_frozen`). Not "from before this
    /// update" — the shell or its rc reorders PATH — so no hook remedy is
    /// named, but "used in every tab" is not said while any is.
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
/// marker means: "Claude Code 2.1.280 and Codex 0.156.1 are current" — one name
/// "… is current", the names the wire's own, so `gemini` reads as gemini. The
/// detail says where they are used: "used in every tab". Neither the vendor source
/// nor an older atpkg's `(build <N>)` store id is repeated (2026-09-23 audit: "builds
/// 2026092201 / 2026092301" meant nothing beside the version already in the title;
/// design ruling 58 supersedes ruling 35's source clause, not its parser).
///
/// UNTIL 2026-09-16 the detail read "what `claude` and `codex` run in new tabs".
/// Owner, that day, with the row on glass in a tab where `claude` was still the
/// native installer's copy: "aterm atpkg DID install the latest but it didn't
/// make them available for me. instead, it is telling me to open a new tab. NO!
/// all the latest and best MUST WORK IN THE SAME TAB with live update! fix this
/// and this message". The managed `agents/` is in front of every session's PATH
/// from launch now (`spawn::managed_agents_dir`), and atpkg re-lays the twins in
/// place, so the next invocation in ANY tab runs the build the entry names. The two
/// exceptions are named rather than papered over, and each replaces the claim: a
/// window whose shell integration failed (`this_tab_hooked`) is told the seed pill's
/// sentence, and `frozen_tabs` live tabs adopted from a build before the
/// self-healing sessions keep the PATH they were born with — for them the detail is
/// the remedy, "1 tab from before this update picks them up with
/// `. ~/.aterm/shell.d/00-atpkg.zsh`", the command atpkg's own rc block runs,
/// spelled for `hook` ([`HookDialect`]: why not `exec $SHELL`), said LAST so a cut
/// keeps it ([`shape_detail`]). The frozen-shells-only form of
/// [`managed_current_words_for`], which the window uses.
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
        this_tab_hooked,
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
    this_tab_hooked: bool,
    hook: HookDialect,
) -> Option<(String, String)> {
    let frozen_tabs = tabs.frozen_shells;
    let mut names: Vec<String> = Vec::new();
    for item in text.split(';').map(str::trim).filter(|s| !s.is_empty()) {
        // `<name> <version> (<Vendor> latest)`, or an older atpkg's `(build <N>)` —
        // the version and the parenthesis are each optional on the wire, so a bare
        // name still renders. The parenthesis is not shown.
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
    // A record's eight words (ruling 261): the programs as a person knows
    // them, with their versions, and that they are current — or, while an
    // agent session still runs an older build of one (gap audit 2026-09-24),
    // that they are installed: "current" would claim the sessions are too.
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
    // "EVERY TAB" IS SAID ON THE SAME EVIDENCE THE PILL USES (review, 2026-09-16):
    // the seam's front position is undone by the user's rc, and only the
    // integration's per-prompt re-assert restores it — so a window whose
    // integration FAILED (unknown shell, unwritable loader cache) is told what the
    // pill tells it, in the pill's words, instead of a claim about a tab that runs
    // whichever `claude` its rc put first.
    if !this_tab_hooked {
        pieces
            .push("this shell isn't hooked up to them; see Settings \u{25b8} Packages".to_string());
    } else if frozen_tabs == 0 && tabs.frozen_agents == 0 && tabs.shadowed == 0 {
        pieces.push("used in every tab".to_string());
    }
    if tabs.agents_behind > 0 {
        let n = tabs.agents_behind;
        pieces.push(if n == 1 {
            "1 running Claude Code session moves onto it at its next turn end".to_string()
        } else {
            format!("{n} running Claude Code sessions move onto it at their next turn end")
        });
    }
    if tabs.agents_held > 0 {
        let n = tabs.agents_held;
        pieces.push(if n == 1 {
            "1 running Claude Code session stays behind it (`aterm harness upgrade --status` \
             says why)"
                .to_string()
        } else {
            format!(
                "{n} running Claude Code sessions stay behind it (`aterm harness upgrade \
                 --status` says why)"
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

/// R33 — `machine-settings:`: the machine-level settings a pass CHANGED per
/// doctor, as atpkg lists them: `spotlight-noindex 73 dir(s) migrated;
/// universal-control disabled`. ONE RECORD PER ITEM
/// ([`machine_setting_words`]), Info, in WIRE order, and none on glass
/// (2026-09-22): the change and its undo are Settings ▸ Security's "This Mac"
/// card ("Last change: …", `PackagesService::note_machine_change`), the log
/// and `appstatus`. (The undo-first order the rows kept so the revert was the
/// next thing read after the pass row went with the rows.) The glyph is a
/// record's `ℹ` (ruling 302). Each item is keyed on its own
/// first word.
pub(crate) fn machine_settings(text: &str) -> Vec<Message> {
    text.split(';')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|item| {
            // The key is the WIRE item's first word, not the display title's:
            // atpkg's vocabulary is the stable one.
            let word = item
                .split_whitespace()
                .next()
                .unwrap_or_default()
                .to_ascii_lowercase();
            let (title, detail) = machine_setting_words(item);
            record(Severity::Info, title, detail)
                .glyph(glyph(NOTED))
                .key(&format!("{KEY_MACHINE_PREFIX}{word}"))
        })
        .collect()
}

/// One machine-settings wire item as its own row's words `(title, detail)`.
/// The two items atpkg emits today read as a person would say them:
/// `universal-control disabled` → **"ALab tools turned off Universal Control"** with the
/// undo in the detail — the pointer first (`aterm pkg machine` prints the
/// revert — the default `aterm pkg doctor` no longer does, 2026-09-23), then
/// the revert command itself, byte-identical to
/// [`atpkg::machine::UNIVERSAL_CONTROL_REVERT`], as its own ` · ` piece: a row
/// too narrow for both sheds the command WHOLE and keeps the pointer
/// ([`shape_detail`]) — it never shows the front half of a `defaults` line and
/// an ellipsis, which is not a revert; `spotlight-noindex N dir(s) migrated` →
/// **"Spotlight: N build dirs moved to .noindex"**, no detail. An item this
/// build does not know is its own title, verbatim, with no detail.
pub(crate) fn machine_setting_words(item: &str) -> (String, String) {
    let item = sanitize_for_tty(item, 80);
    let (title, detail) = if item == atpkg::machine::UNIVERSAL_CONTROL_ENTRY {
        (
            "ALab tools turned off Universal Control".to_string(),
            format!(
                "undo: `aterm pkg machine` prints the revert \u{00b7} {}",
                atpkg::machine::UNIVERSAL_CONTROL_REVERT
            ),
        )
    } else if let Some(count) = spotlight_noindex_count(&item) {
        (
            format!(
                "Spotlight now skips {count} build {}",
                if count == 1 { "folder" } else { "folders" }
            ),
            format!(
                "{count} build {} moved to .noindex, so Spotlight does not index them",
                if count == 1 { "folder" } else { "folders" }
            ),
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
// The tailed snapshot (R26 and the terminal readings of the file).
// ---------------------------------------------------------------------------

/// What one classified `progress.json` read says — the pure half of the old
/// `toolchain_snapshot`. The host applies it against the row that carries
/// [`KEY_PASS`], and ONLY a first run's reads paint (the silent lane, module
/// doc; upstream dbf97ecff + ac7942234):
///
/// * `Live` restates the first run's live row in place (a `restate` costs no
///   log line, so the 10 Hz tailer is free) or posts it when none is up; a
///   routine pass's `Live` paints nothing;
/// * `Over` and `Vanished` claim NO outcome — "all N installed", "N failed"
///   and "stopped" were held rows until 2026-09-22; the markers now say the
///   outcome as records, and a failure is the Settings ▸ Packages badge. A
///   routine read folds a live row at once; a first run's ending read (one of
///   its sub-passes over, or its final read) folds nothing — the child's exit
///   does (`App::toolchain_pass_ended`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SnapshotWords {
    /// The file vanished at child exit: a live row has nothing honest left to
    /// say.
    Vanished,
    /// The pass is OVER — ended cleanly, with failures, or its writer gone —
    /// and the read claims no outcome.
    Over,
    /// Nothing planned while the pass runs: atpkg begins its "net" pass
    /// BEFORE the signed index resolves, and the common outcome is a plan of
    /// zero programs. A row that appears for that, re-grids every window, says
    /// "nothing to do" and re-grids again is the exact churn the owner's brief
    /// rules out: leave the glass alone (an announced row keeps its
    /// announcement until the plan lands).
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

/// The words for one classified read, pure. `peak` is the live row's meter
/// as the host holds it — its pass and its fill — so A LIVE METER NEVER RUNS
/// BACKWARDS within one pass: atpkg's `.part` poller reports 0 for the instant
/// between curl promoting the file and the watch stopping, and a program's
/// credit can dip for the length of its verify phase (found by the 2026-08-26
/// progress-model survey) — the fill is clamped to this pass's own high-water
/// mark. A different pass (identity: pass name + start stamp) starts fresh.
///
/// The row paints its title and its indicator alone (design §10.3 T2): the
/// current program and phase (`trust · extracting`) ride behind Details, and
/// so does atpkg's byte rollup (`512 MB / 1.2 GB downloaded`) — it counts
/// download bytes only, and beside the bar it read 43 % while the bar said
/// 35 % and `3 of 10` said 30 %, three measures of one wait (review round 2,
/// 2026-09-23). The stats are the one count that agrees with the bar, `3 of
/// 10 programs`. The HEAVIEST active phase is the declared load
/// ([`pass_load`]). The fill counts programs weighted by phase
/// ([`overall_fill`]) — the whole deliverable, reaching 100 % only when every
/// program has finished — and it is the row's [`Amount`] too (in
/// [`Unit::Steps`], a series per pass): the band's ETA is the fill's own
/// rate, so the longest wait in the product says how long it has left.
/// `routine` titles a heavy routine pass (`Updating ALab tools`, design
/// §10.5 H8).
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
                live_row(INSTALLING_ALAB)
                    .glyph(glyph(DOWN))
                    .no_excerpt()
                    .in_flight()
                    .hold(Hold::Live {
                        stale_after: STALE_TAILED,
                    })
                    .key(KEY_PASS),
            ),
            pass_id: (f.pass.clone(), f.started_unix),
        };
    }
    let total = f.overall.programs_total;
    let done = f.overall.programs_done;
    // ONE TITLE PER INSTALL (audit 2026-09-14): the announcement's row says
    // [`INSTALLING_ALAB`], and so does the pass it announced (`net` is the
    // one pass atpkg writes since Phase 5 deleted the sealed `seed` pass). A
    // heavy ROUTINE pass updates what is already there (ruling 144).
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
    // A program verifying or linking HOLDS its share (500‰, 990‰) with no
    // meter of its own: a minute of verify is the plan, not a stall (design
    // ruling 266, until the owner rules on ruling 265's question).
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
    let message = live_row(title)
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
        .hold(Hold::Live {
            stale_after: STALE_TAILED,
        })
        .key(KEY_PASS);
    SnapshotWords::Live {
        message: Box::new(message),
        pass_id,
    }
}

/// The resource the pass loads HARDEST right now (design §10.6): any program
/// extracting or linking loads the disk; else a verification the CPU; else a
/// download the network. It names the heaviest active phase, not the one the
/// detail line picks — an extraction running beside a download is what makes
/// the machine sluggish. `None` with nothing in flight.
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

/// The overall meter: PROGRAMS, each weighted by its phase — a finished
/// program (done, failed or skipped) counts whole; one in flight counts its
/// download as the first half (by its bytes), a verify as half, its
/// extraction as the second half (by its bytes, to 99 %), a link as 99 %.
/// The download rollup alone read 42 % while `trust` extracted with three of
/// ten programs done, and would have sat full through the extraction tail
/// — the disk-busy phase (review 2026-09-23); this reaches 100 % only when
/// every program has finished. `None` when the pass planned no programs (a
/// seed pass before its plan lands, or nothing to do).
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

/// The program the pass is working on right now, as one honest phrase, plus
/// that program's own byte meter when its phase has one — the fallback fill for
/// a pass whose overall rollup is silent (no signed sizes to sum, so the rollup
/// jumps per program; the program's own meter is the live truth).
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
    // Why a program was pulled forward — asked for, or needed by one that was —
    // is scheduling, not progress: progress.json and `aterm pkg doctor` keep
    // it, the row does not (2026-09-23).
    (
        line,
        if matches!(row.phase, Phase::Download | Phase::Extract) {
            frac
        } else {
            None
        },
    )
}

/// A program name is UNTRUSTED until it round-trips the store's name gate; one
/// that fails simply has no words on the row.
fn admitted_name(raw: &str) -> Option<String> {
    let name = atpkg::store::ToolName::new(raw)?;
    Some(sanitize_for_tty(name.as_str(), NAME_CAP))
}

#[cfg(test)]
mod tests {
    use super::*;
    use aterm_messages::{Instant, MessageCenter, MessageLog, TITLE_CAP, WallStamp};
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

    /// EVERY LIVE TOOLCHAIN ROW COMPLETES IN ITS FINISHED FORM (design ruling
    /// 154): `Installed ALab tools`, `Updated ALab tools`, `Installed
    /// packages` — the table's words read right for every title this lane
    /// uses — with nothing else on the row moving at 60, 80, 120 and 160.
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

    /// Every RECORD is a toolchain row with the `Packages` capsule and every
    /// LIVE row carries none (work in flight is not a decision: review round
    /// 2, 2026-09-23); each has a title inside the cap and a glyph from the
    /// band's closed set — and the `Packages` route is the Settings page's
    /// own path, never a spelling of this module's.
    #[test]
    fn every_toolchain_record_carries_the_packages_capsule_and_an_admitted_glyph() {
        let rows = vec![
            announced("installing 2 program(s) (about 1 GB)"),
            deferred("an earlier toolchain pass is still running"),
            not_installed("the update finished without installing anything"),
            installed("✓ ALab toolchain installed: ay, trust — ready in every aterm tab"),
            ended("the pass finished; 12 ALab program(s) are installed", false),
            failed("partly installed \u{2014} see Settings \u{25b8} Packages"),
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
        let rows: Vec<Message> = rows
            .into_iter()
            .chain(machine_settings("universal-control disabled"))
            .collect();
        for m in &rows {
            assert_eq!(m.tag, tags::TOOLCHAIN, "{}", m.title);
            if matches!(m.hold, Hold::Live { .. }) {
                assert!(m.actions.is_empty(), "{}: a live row", m.title);
            } else {
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
        for ch in [DOWN, NOTED] {
            assert!(Glyph::new(ch).is_some(), "{ch:?} is in the closed set");
        }
    }

    /// R29f — A FIRST RUN THAT ENDED SHORT IS A FAILURE ROW the person acts
    /// on, titled by how it ended; the cause rides behind Details unless the
    /// person can fix it themselves (a full disk), and the capsule is the page
    /// that retries.
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
        // (`seed-partial:` went with the sealed seed, Phase 5.)
        let full = first_run_short(FirstRunShort::Failed, "trust: No space left on device");
        assert!(full.excerpt, "a full disk is the person's to fix: painted");
        assert_eq!(
            full.detail,
            ["no space left on device", "trust: No space left on device"]
        );
        // The reason is kept WHOLE (ruling 63, M9): a long atpkg stderr
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

    /// R25: the announcement opens the row before any snapshot, keeps the
    /// size atpkg computed as its only detail, is BUSY (no fraction yet: the
    /// host animates it), and is live under the announcement cap.
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

    /// THE FIRST RUN IS ONE ROW, EVERY OUTCOME A RECORD (the silent lane,
    /// upstream 2026-09-22): the announcement and the meter share the pass key,
    /// so the meter supersedes the announcement in its slot; the outcome, the
    /// managed-current and machine-settings words are `Hold::LogOnly` records —
    /// on the log, never live — and the one live row stays the meter.
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
            not_installed(
                "the update finished without installing anything \u{2014} see Settings \u{25b8} Packages",
            ),
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

    /// R30: a wait that ran out is a DEFERRED record — Info, ⏸, "Package
    /// update postponed" and atpkg's sentence, never the word "failed", never a
    /// row — and unkeyed.
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

    /// A pass that LEFT NO ALAB TOOLS and saw no failure (`seed-nothing:`,
    /// `seed-unusable:`) is "Couldn't install any ALab tools" — Warn, a record keyed to
    /// the pass with the Packages capsule — and never titled "failed", which
    /// would claim a failure nobody saw.
    #[test]
    fn a_pass_that_left_no_tools_is_never_titled_failed() {
        let m = not_installed(
            "the update finished without installing anything \u{2014} see Settings \u{25b8} Packages",
        );
        assert_eq!(m.title, PACKAGES_NOT_INSTALLED);
        assert_eq!(m.severity, Severity::Warn);
        assert_eq!(m.hold, Hold::LogOnly);
        assert_eq!(m.key.as_deref(), Some(KEY_PASS));
        assert_eq!(m.actions, vec![packages()]);
        assert!(!m.title.contains("failed"), "{}", m.title);
        assert_eq!(
            m.detail,
            vec![
                "the update finished without installing anything \u{2014} see Settings \u{25b8} Packages"
            ]
        );
    }

    /// R27/R28: the positive terminals are records with NO meter (ruling 19;
    /// no success row since the silent lane) and drop the pill's own opening
    /// from the detail.
    #[test]
    fn the_positive_terminals_carry_no_meter_and_keep_the_sentence() {
        let m = installed(
            "\u{2713} ALab toolchain installed: claude, codex \u{2014} claude and codex already run in every aterm tab, this one too",
        );
        assert_eq!(m.title, "ALab tools installed");
        assert_eq!(
            m.detail,
            vec![
                "claude, codex \u{2014} claude and codex already run in every aterm tab, this one too"
            ]
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
        // Records are never painted: atpkg's sentence is kept WHOLE, never cut
        // mid-word at 160 characters (audit 2026-09-24, design ruling 64).
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

    /// R29: a failure is a Warn RECORD with NO meter (the 2026-09-11
    /// screenshot) — "Couldn't update ALab tools", then the whole sentence the host
    /// built (`failure_cause`), control-stripped and KEPT WHOLE (design
    /// ruling 63: no 160-character cut); its surface is the Settings ▸
    /// Packages badge, never a row.
    #[test]
    fn a_failure_is_a_warn_record_with_no_meter() {
        let m = failed("partly installed \u{2014} see Settings \u{25b8} Packages");
        assert_eq!(m.title, "Couldn't update ALab tools");
        assert_eq!(
            m.detail,
            vec!["partly installed \u{2014} see Settings \u{25b8} Packages"]
        );
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
        // A line past the line cap is split at its joints, never cut: the
        // tail — the pointer the host ends the sentence with — survives
        // (design ruling 64).
        let cause = format!(
            "net-failed: claude: download failed: {}(HTTP 403) \u{2014} see Settings \u{25b8} Packages",
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
                .is_some_and(|l| l.ends_with("see Settings \u{25b8} Packages")),
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
        // A verify or a link HOLDS the program's share with no meter: the
        // same count, declared held, so it never reads stalled (ruling 266).
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
    /// programs weighted by phase; the title is the announcement's for the `net`
    /// pass and the generic one for any other name (the sealed `seed` pass and
    /// its "Preparing" title went with Phase 5).
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

    /// A LIVE METER NEVER RUNS BACKWARDS within one pass: the clamp is to the
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

    /// A PASS THAT IS OVER CLAIMS NO OUTCOME (the silent lane, upstream
    /// 2026-09-22): a read of a pass that ended — cleanly, with a failure, or
    /// with its writer gone — used to be a held "all 10 installed" / "1
    /// failed" / "stopped" row; now it is `Over` whatever the file says, and
    /// the outcome is the markers' record. A vanished file, a plan of nothing,
    /// and an unknown schema keep their own readings.
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
    /// program was pulled forward — scheduling, not progress — stays off the
    /// row (2026-09-23): progress.json and `aterm pkg doctor` keep it.
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
        // atpkg's text: control-stripped before they become words (upstream's
        // `hostile_strings_are_sanitized_before_they_become_cells`, 2026-09-22).
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
            vec!["used in every tab"],
            "one name reads singular; an older atpkg's wire still parses, its build \
             unrepeated"
        );
        assert_eq!(m.hold, Hold::LogOnly, "a record on every pass, never a row");
        assert_eq!(m.meter, None, "a record carries no meter");
        assert_eq!(m.severity, Severity::Success);
        assert_eq!(m.key.as_deref(), Some(KEY_MANAGED));

        // Today's wire (upstream cb26ff4d1): vendor programs by version and
        // source, never a store id.
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
            vec!["used in every tab"],
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
        assert_eq!(m.detail, vec!["used in every tab"]);
        assert!(
            managed_current("  ;  ", 0, true, HookDialect::Zsh).is_none(),
            "an empty body records nothing"
        );

        let rows = machine_settings("spotlight-noindex 12 dir(s) migrated");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].title, "Spotlight now skips 12 build folders");
        assert_eq!(
            rows[0].detail,
            ["12 build folders moved to .noindex, so Spotlight does not index them"],
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
        assert_eq!(detail, "used in every tab");
    }

    /// THE RECORD SAYS WHAT IS TRUE OF THE RUNNING SESSIONS (gap audit
    /// 2026-09-24: "Claude Code 2.1.282 … up to date" over two sessions on
    /// 2.1.280 and 2.1.281, with the hook named for a tab whose foreground was
    /// Claude). Sessions the live upgrade is moving turn "up to date" into
    /// "installed" and are said with WHEN; a frozen tab with an agent in front
    /// is told to source the hook once the agent exits, never handed a line to
    /// type into it; the shell remedy stays last. NEGATIVE CONTROL: with no
    /// lagging session and no agent tab the words are the old ones.
    #[test]
    fn the_managed_record_says_installed_while_sessions_lag_and_spares_agent_tabs() {
        let wire = "claude 2.1.282 (Anthropic latest); codex 0.157.0 (OpenAI latest)";
        let words = |tabs: TabsBehind| {
            managed_current_words_for(wire, tabs, true, HookDialect::Zsh).unwrap()
        };
        let (title, detail) = words(TabsBehind {
            agents_behind: 2,
            ..TabsBehind::default()
        });
        assert_eq!(title, "Claude Code 2.1.282 and Codex 0.157.0 installed");
        assert_eq!(
            detail,
            "used in every tab · 2 running Claude Code sessions move onto it at their next turn end"
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
        assert_eq!(detail, "used in every tab");
    }

    /// A SESSION THAT WILL NOT MOVE ON ITS OWN IS NOT SAID TO MOVE (review of
    /// 2026-09-25: an overdue session was counted "moves onto it at its next
    /// turn end" while the band showed it stalled). It keeps the title at
    /// "installed" — "up to date" is false of it — and is pointed at
    /// `--status`. A tab MEASURED to put a foreign copy first, with no
    /// adoption mark, is not "from before this update" and gets no hook line,
    /// but "used in every tab" is not said while it lasts.
    #[test]
    fn a_held_session_is_not_said_to_move_and_a_shadowed_tab_is_not_from_before() {
        let wire = "claude 2.1.282 (Anthropic latest)";
        let words = |tabs: TabsBehind| {
            managed_current_words_for(wire, tabs, true, HookDialect::Zsh).unwrap()
        };
        let (title, detail) = words(TabsBehind {
            agents_held: 1,
            ..TabsBehind::default()
        });
        assert_eq!(title, "Claude Code 2.1.282 installed");
        assert_eq!(
            detail,
            "used in every tab · 1 running Claude Code session stays behind it \
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

    /// THE REMEDY IS SPELLED FOR THE SHELL THIS WINDOW SPAWNS (2026-09-16): the
    /// hook atpkg writes has one file per dialect (`crates/atpkg/src/hooks.rs`
    /// `hook_files`: `.zsh`, `.bash`, `.fish`, `.ps1`), sourced the way that
    /// shell sources — POSIX `.`, fish's `source`, PowerShell's dot-source — and
    /// a shell atpkg has no hook for is told the truth: a new tab.
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
        // A WSL tab is a Linux bash whose `$HOME/.aterm/shell.d` is the DISTRO's
        // home, where the Windows-side atpkg writes nothing: `. ~/.aterm/shell.d/
        // 00-atpkg.bash` there is "No such file", so the honest note for it is
        // the new tab, not bash's remedy (review, 2026-09-16, refuted mapping).
        assert_eq!(HookDialect::from(ShellType::Wsl), HookDialect::None);
        // The file name is atpkg's, never a spelling of our own.
        assert_eq!(atpkg::hooks::HOOK_BASENAME, "00-atpkg");
    }

    /// "EVERY TAB" ON THE PILL'S EVIDENCE (review, 2026-09-16): a window
    /// whose shell integration failed is told the pill's sentence instead.
    #[test]
    fn the_managed_row_stops_claiming_this_tab_when_integration_failed() {
        let wire = "claude 2.1.273 (build 2026091601); codex 0.154.0 (build 2026091001)";
        let (_, detail) = managed_current_words(wire, 0, false, HookDialect::Zsh).unwrap();
        assert_eq!(
            detail,
            "this shell isn't hooked up to them; see Settings \u{25b8} Packages"
        );
        assert!(!detail.contains("every tab"), "{detail}");
        // With frozen tabs alive as well, the note still comes last.
        let (_, detail) = managed_current_words(wire, 1, false, HookDialect::Zsh).unwrap();
        assert!(
            detail.contains("isn't hooked up to them")
                && detail.ends_with(
                    "\u{00b7} 1 tab from before this update picks them up with \
                     `. ~/.aterm/shell.d/00-atpkg.zsh`"
                ),
            "{detail}"
        );
        let (_, detail) = managed_current_words(wire, 0, true, HookDialect::Zsh).unwrap();
        assert_eq!(detail, "used in every tab");
    }

    /// THE SEED PILL, over every roster shape × integration outcome × frozen-tab
    /// count × hook dialect (moved here from `lib.rs`'s five example-string pins,
    /// 2026-09 test audit). Owner, 2026-09-16: "NO! all the latest and best MUST
    /// WORK IN THE SAME TAB with live update!" — so:
    ///
    /// - the marker glyph leads (the renderer tints the FIRST char; losing it
    ///   renders "ALab" as "A Lab") and the roster is capped at five names;
    /// - never `exec` (it drops the tab's integration) and never "restart";
    /// - a FAILED integration promises nothing and points at the fix surface;
    /// - otherwise no pill sends a hooked tab to a new one: with no frozen tabs
    ///   the whole roster is ready in this tab too; with frozen tabs the claim
    ///   narrows to the tabs opened since, and for a roster with `bin/` tools
    ///   the frozen ones are named with the hook remedy — the managed record's
    ///   own spelling (`HookDialect::remedy`, one source) — and only a shell
    ///   atpkg has no hook for is told a new tab, about itself;
    /// - an agents-only roster names the agents ("claude and codex already run
    ///   in every aterm tab"); any roster with a `bin/` tool, a mixed one
    ///   included, is "ready" whole.
    ///
    /// Behind the property, the exact sentences the five pinned, byte for byte.
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
                            assert!(
                                pill.ends_with(
                                    "but this shell isn't hooked up to them; see Settings \u{25b8} Packages"
                                ),
                                "{case}"
                            );
                            assert!(
                                !pill.contains("ready in") && !pill.contains("this one too"),
                                "{case}"
                            );
                            continue;
                        }
                        if frozen == 0 {
                            assert!(pill.ends_with("in every aterm tab, this one too"), "{case}");
                        } else {
                            assert!(
                                pill.contains("in every tab opened since this update"),
                                "{case}"
                            );
                            assert!(!pill.contains("this one too"), "{case}");
                        }
                        if agents_only {
                            // The claim's subject is the agents themselves.
                            let subject = match roster.as_slice() {
                                [one] => one.clone(),
                                [head @ .., last] => format!("{} and {last}", head.join(", ")),
                                [] => unreachable!("every roster names a program"),
                            };
                            let verb = match (frozen == 0, roster.len() == 1) {
                                (true, true) => "already runs",
                                (true, false) => "already run",
                                (false, true) => "runs",
                                (false, false) => "run",
                            };
                            assert!(
                                pill.contains(&format!(" \u{2014} {subject} {verb} in every ")),
                                "{case}"
                            );
                            assert!(!pill.contains(" \u{b7} "), "no remedy clause: {case}");
                            assert!(!lower.contains("new tab"), "{case}");
                            continue;
                        }
                        // A roster with a `bin/` tool — a mixed one included — is
                        // ready whole: no split agents clause.
                        assert!(pill.contains(" \u{2014} ready in every "), "{case}");
                        if frozen == 0 {
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
        // The exact sentences, byte for byte (the five example-string tests'
        // goldens, every row kept): the cap, the frozen-tab remedy in each
        // dialect, the agents clause for one and two, a failed integration, and a
        // mixed roster that reads as ready whole.
        let six = names(&["ay", "clean", "ny", "trust", "ty", "nn"]);
        let tools = names(&["ay", "trust"]);
        let agents = names(&["claude", "codex"]);
        let mixed = names(&["ay", "claude", "trust"]);
        let unhooked = "but this shell isn't hooked up to them; see Settings ▸ Packages";
        /// `(roster, integration outcome, frozen tabs, hook dialect, the pill
        /// after its opening)`.
        type Golden<'a> = (&'a [String], Option<Si>, usize, HookDialect, String);
        let goldens: [Golden<'_>; 18] = [
            (
                &six[..3],
                Some(Si::Prepared),
                0,
                HookDialect::Zsh,
                "ay, clean, ny — ready in every aterm tab, this one too".into(),
            ),
            (
                &six[..5],
                Some(Si::Prepared),
                0,
                HookDialect::Zsh,
                "ay, clean, ny, trust, ty — ready in every aterm tab, this one too".into(),
            ),
            (
                &six,
                Some(Si::Prepared),
                0,
                HookDialect::Zsh,
                "ay, clean, ny, trust, ty… — ready in every aterm tab, this one too".into(),
            ),
            (
                &tools,
                Some(Si::Prepared),
                0,
                HookDialect::Zsh,
                "ay, trust — ready in every aterm tab, this one too".into(),
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
                "claude, codex — claude and codex already run in every aterm tab, this one too"
                    .into(),
            ),
            (
                &agents[..1],
                None,
                0,
                HookDialect::Zsh,
                "claude — claude already runs in every aterm tab, this one too".into(),
            ),
            (
                &agents,
                Some(Si::WriteFailed("ro".into())),
                0,
                HookDialect::Zsh,
                format!("claude, codex — {unhooked}"),
            ),
            (
                &agents,
                Some(Si::UnknownShell("nu".into())),
                0,
                HookDialect::Zsh,
                format!("claude, codex — {unhooked}"),
            ),
            (
                &agents,
                Some(Si::Prepared),
                2,
                HookDialect::Zsh,
                "claude, codex — claude and codex run in every tab opened since this update".into(),
            ),
            (
                &agents[..1],
                None,
                1,
                HookDialect::Zsh,
                "claude — claude runs in every tab opened since this update".into(),
            ),
            (
                &mixed,
                Some(Si::Prepared),
                0,
                HookDialect::Zsh,
                "ay, claude, trust — ready in every aterm tab, this one too".into(),
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
                format!("ay, claude, trust — {unhooked}"),
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
        // included (no row since 2026-09-22: a `Hold::LogOnly` record).
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

    /// MACHINE SETTINGS: ONE RECORD PER CHANGED ITEM, IN WIRE ORDER (the
    /// silent lane: the undo-first order the rows kept, so the revert was read
    /// right after the pass row, went with the rows — upstream records them in
    /// the order atpkg printed them). An item this build does not know is its
    /// own title, verbatim; each item has a key of its own.
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
        // The undo record's detail: the pointer first, the revert command last,
        // byte-identical to what `aterm pkg machine` prints.
        assert_eq!(
            rows[2].detail,
            vec![
                "undo: `aterm pkg machine` prints the revert \u{00b7} \
                 defaults -currentHost delete com.apple.universalcontrol Disable; \
                 defaults -currentHost delete com.apple.universalcontrol DisableMagicEdges"
            ]
        );
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

    /// §10.6: the declared load is the HEAVIEST active phase — an extraction
    /// or a link loads the disk, else a verification the CPU, else a download
    /// the network — not the phase the detail line happens to name.
    /// A transfer's stats keep ONE width from its first megabyte to its
    /// last, so the width law never flips the row between its stats and a
    /// wider meter mid-download, and the digits tick in place.
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
