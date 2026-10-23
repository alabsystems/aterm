// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Settings ▸ Packages read projection, the toolchain-package analogue of [`crate::update_screen`].

use crate::native_ui::FACT_SEPARATOR;

/// Which co-located `atpkg` verb is running.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PackagesBusy {
    /// `atpkg update` — check/update every installed managed program.
    Check,
    /// `atpkg install --default-set` — bootstrap-install the signed default set.
    Install,
    /// `atpkg uninstall --all` — remove the whole managed toolset, reclaiming its disk.
    Uninstall,
    /// `atpkg machine apply`: the `[machine]` host setting (Universal Control off), applied from the Security page's "Apply Now".
    MachineApply,
}

impl PackagesBusy {
    /// Whether the verb's atpkg may apply the `[machine]` settings.
    pub(crate) const fn applies_machine_settings(self) -> bool {
        matches!(self, Self::Check | Self::Install | Self::MachineApply)
    }

    const fn completed_headline(self) -> &'static str {
        match self {
            Self::Check => "Package check completed",
            Self::Install => "ALab tools install completed",
            Self::Uninstall => "ALab tools removed",
            Self::MachineApply => "Machine settings applied",
        }
    }

    const fn failed_headline(self) -> &'static str {
        match self {
            Self::Check => "Package check failed",
            Self::Install => "ALab tools install failed",
            Self::Uninstall => "ALab tools removal failed",
            Self::MachineApply => "Machine settings not applied",
        }
    }
}

/// The final result of the co-located `atpkg` process, distinct from the UI-thread admission result, so an old successful `status.toml` cannot pass for the outcome of a process that failed to launch or exited non-zero.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PackagesCommandOutcome {
    Succeeded {
        operation: PackagesBusy,
    },
    Failed {
        operation: PackagesBusy,
        message: String,
    },
}

impl PackagesCommandOutcome {
    const fn operation(&self) -> PackagesBusy {
        match self {
            Self::Succeeded { operation } | Self::Failed { operation, .. } => *operation,
        }
    }

    fn headline(&self) -> &'static str {
        match self {
            Self::Succeeded { operation } => operation.completed_headline(),
            Self::Failed { operation, .. } => operation.failed_headline(),
        }
    }

    fn feedback(&self) -> String {
        match self {
            Self::Succeeded { operation } => operation.completed_headline().to_string(),
            Self::Failed { operation, message } => {
                format!("{}: {message}", operation.failed_headline())
            }
        }
    }
}

/// One off-thread worker completion.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PackagesWorkerCompletion {
    pub(crate) report: PackagesStatusReport,
    pub(crate) command: Option<PackagesCommandOutcome>,
    /// The `atpkg machine: …` verdict a [`PackagesBusy::MachineApply`] worker read off the child's stdout (`applied — …` / `nothing changed — …`), quoted by the status and the feedback line.
    pub(crate) machine_verdict: Option<String>,
    /// The `machine-state:` record a [`PackagesBusy::MachineApply`] worker read off the child's stdout.
    pub(crate) machine_state: Option<atpkg::machine::MachineState>,
}

impl PackagesWorkerCompletion {
    pub(crate) const fn refresh(report: PackagesStatusReport) -> Self {
        Self {
            report,
            command: None,
            machine_verdict: None,
            machine_state: None,
        }
    }

    /// Attach the machine-apply verdict sentence (see `machine_verdict`).
    pub(crate) fn with_machine_verdict(mut self, verdict: Option<String>) -> Self {
        self.machine_verdict = verdict;
        self
    }

    /// Attach the machine-apply record (see `machine_state`).
    pub(crate) const fn with_machine_state(
        mut self,
        state: Option<atpkg::machine::MachineState>,
    ) -> Self {
        self.machine_state = state;
        self
    }

    pub(crate) const fn command(
        report: PackagesStatusReport,
        command: PackagesCommandOutcome,
    ) -> Self {
        Self {
            machine_verdict: None,
            machine_state: None,
            report,
            command: Some(command),
        }
    }
}

/// One managed program's last-known state, projected read-only for the page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PackagesProgramRow {
    pub(crate) name: String,
    /// The active build number, if one is installed.
    pub(crate) installed_build: Option<u64>,
    /// atpkg's state line, verbatim.
    pub(crate) state: String,
    /// The same line, parsed through `atpkg::state`'s own readers — never a second spelling.
    pub(crate) kind: ProgramStateKind,
    /// Vendor / license / size for an agent program, from atpkg's authored roster line (`atpkg::stub::describe`).
    pub(crate) facts: Option<VendorFacts>,
    /// Source annotation: `Some("dev-link → /path")` for a dev-linked checkout.
    pub(crate) annotation: Option<String>,
    /// The upstream an ALab build is based on, in `which`'s words (`based on upstream rust-v0.163.0+g0123456789ab`, [`atpkg::alab::Upstream::words`]).
    pub(crate) upstream: Option<String>,
    /// Version, source, last updated and latest known.
    pub(crate) update: ProgramUpdate,
    /// The row's words as the page paints them ([`ProgramUpdate::words`]).
    pub(crate) words: Option<ProgramWords>,
}

/// A program's four facts on Settings ▸ Packages.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ProgramUpdate {
    /// `2.1.280` / `build 1971`; `None` with no installed build.
    pub(crate) version: Option<String>,
    /// Where its builds come from.
    pub(crate) source: String,
    /// A shadowed row's fix-line.
    pub(crate) fix: Option<String>,
    /// When the active build went live (Unix seconds): a vendor build's verification, an ALab build's `current` flip.
    pub(crate) updated_at: Option<i64>,
    /// The newest build known: a vendor's head, an ALab index pin.
    pub(crate) latest_known: Option<String>,
    /// When a vendor's head was last checked (Unix seconds).
    pub(crate) checked_at: Option<i64>,
}

/// A program row's words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ProgramWords {
    pub(crate) summary: String,
    pub(crate) detail: Option<String>,
}

impl ProgramUpdate {
    /// The latest known build when it is not the installed one.
    fn newer_known(&self) -> Option<&str> {
        let version = self.version.as_deref()?;
        self.latest_known
            .as_deref()
            .filter(|latest| *latest != version)
    }

    /// Whether a row whose state says it is current (at its index's pin, at its vendor's head) knows a newer build than the one installed.
    pub(crate) fn behind(&self, kind: &ProgramStateKind) -> bool {
        matches!(
            kind,
            ProgramStateKind::Managed { .. } | ProgramStateKind::VendorLatest { .. }
        ) && self.newer_known().is_some()
    }

    /// The words for a row whose state is `kind`/`state`, times relative to `now` on the local `clock`.
    pub(crate) fn words(
        &self,
        kind: &ProgramStateKind,
        state: &str,
        now: i64,
        clock: LocalClock,
    ) -> Option<ProgramWords> {
        let version = self.version.as_deref()?;
        let newer = self.newer_known();
        let said = plain_state(kind, state)
            .filter(|_| !self.behind(kind))
            .or_else(|| source_plain(&self.source));
        let mut summary = String::from(version);
        let mut newer = newer;
        match said {
            Some(said) if said.starts_with(&format!("kept at {version} ")) => {
                summary = said;
                newer = None;
            }
            Some(said) => {
                summary.push_str(FACT_SEPARATOR);
                summary.push_str(&said);
            }
            None => {}
        }
        let mut detail: Vec<String> = Vec::new();
        if let Some(fix) = &self.fix {
            detail.push(fix.clone());
        }
        if let Some(at) = self.updated_at {
            detail.push(format!("Updated {}", when_words(at, now, clock)));
        }
        match newer {
            Some(latest) => {
                let mut known = format!("Latest known {latest}");
                if let Some(at) = self.checked_at {
                    known.push_str(", checked ");
                    known.push_str(&when_words(at, now, clock));
                }
                detail.push(known);
            }
            None => {
                if let Some(at) = self.checked_at {
                    detail.push(format!("Checked {}", when_words(at, now, clock)));
                }
            }
        }
        Some(ProgramWords {
            summary,
            detail: (!detail.is_empty()).then(|| detail.join(FACT_SEPARATOR)),
        })
    }
}

/// A row's state in a person's words.
pub(crate) fn plain_state(kind: &ProgramStateKind, state: &str) -> Option<String> {
    Some(match kind {
        ProgramStateKind::Managed { .. } => "up to date".to_string(),
        ProgramStateKind::VendorLatest { vendor, .. } => format!("latest from {vendor}"),
        ProgramStateKind::VendorKept { why, .. } if why.starts_with("held by local pin") => {
            "you pinned it".to_string()
        }
        ProgramStateKind::VendorKept { why, .. } => why.clone(),
        ProgramStateKind::Shadowed { path, .. } => format!("another copy runs first: {path}"),
        ProgramStateKind::System { path, .. } => format!("uses the copy at {path}"),
        ProgramStateKind::AgentInstalling => "installing\u{2026}".to_string(),
        ProgramStateKind::Unavailable => "not available for this Mac".to_string(),
        ProgramStateKind::Other => return held_words(state),
    })
}

/// `held: [<owner>'s ]pinned build <N> is not published for <target>; staying on build
/// <C>` ([`atpkg::state::held_unpublished`]) → `kept at build C — N isn't available for
/// this Mac` (`— <owner>'s build N …` on a sibling's row).
fn held_words(state: &str) -> Option<String> {
    let rest = state.strip_prefix(atpkg::state::HELD_PREFIX)?;
    let (owner, rest) = match rest.split_once("'s pinned build ") {
        Some((owner, rest)) => (Some(owner), rest),
        None => (None, rest.strip_prefix("pinned build ")?),
    };
    let (pinned, rest) = rest.split_once(" is not published for ")?;
    let (_, current) = rest.split_once("; staying on build ")?;
    let whose = owner.map_or_else(String::new, |owner| format!("{owner}'s build "));
    Some(format!(
        "kept at build {current} \u{2014} {whose}{pinned} isn\u{2019}t available for this Mac"
    ))
}

/// Whether a pass's recorded outcome is the plain healthy sentence — `up to date (index
/// build 45)`, `up to date (ay build 8256)` — with nothing after it.
fn outcome_is_plainly_current(outcome: &str) -> bool {
    let Some(rest) = outcome.strip_prefix("up to date") else {
        return false;
    };
    let rest = rest.trim();
    rest.is_empty() || (rest.starts_with('(') && rest.find(')') == Some(rest.len() - 1))
}

/// Where a build comes from, as a row says it when its state has no plain words:
/// `Anthropic latest` → `from Anthropic`, `ALab index 44` → `from ALab`.
fn source_plain(source: &str) -> Option<String> {
    let vendor = source
        .strip_suffix(" latest")
        .or_else(|| source.split_once(" index").map(|(vendor, _)| vendor))
        .unwrap_or(source)
        .trim();
    (!vendor.is_empty()).then(|| format!("from {vendor}"))
}

/// A time as a person reads it, in local time, never raw UTC: `just now`, `4 min ago`, `3 h ago` inside a day.
pub(crate) fn when_words(at: i64, now: i64, clock: LocalClock) -> String {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    const WEEKDAYS: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];
    let age = now.saturating_sub(at);
    if (0..60).contains(&age) {
        return "just now".to_string();
    }
    if (60..3600).contains(&age) {
        return format!("{} min ago", age / 60);
    }
    if (3600..86_400).contains(&age) {
        return format!("{} h ago", age / 3600);
    }
    let local = at.saturating_add(clock.offset_at(at));
    let day = local.div_euclid(86_400);
    let today = now.saturating_add(clock.offset_at(now)).div_euclid(86_400);
    let tod = local.rem_euclid(86_400);
    let time = format!("{:02}:{:02}", tod / 3600, (tod % 3600) / 60);
    let (year, month, date) = aterm_types::rfc3339::civil_from_days(day);
    let (this_year, _, _) = aterm_types::rfc3339::civil_from_days(today);
    let month = MONTHS[usize::try_from(month - 1).unwrap_or(0).min(11)];
    let mut words = if day == today {
        format!("today {time}")
    } else if day > today {
        format!("{month} {date} {time}")
    } else if day == today - 1 {
        format!("yesterday {time}")
    } else if day > today - 7 && day < today {
        let weekday = WEEKDAYS[usize::try_from(day.rem_euclid(7)).unwrap_or(0)];
        format!("{weekday} {time}")
    } else if year == this_year {
        format!("{month} {date}")
    } else {
        format!("{month} {date} {year}")
    };
    if !clock.known() {
        words.push_str(" UTC");
    }
    words
}

/// How far back the calendar shows a clock time (`yesterday 14:05`, `Mon 09:30`): a week.
const CLOCK_WINDOW_S: i64 = 7 * 86_400;

/// The local clock the Packages page reads its times on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LocalClock {
    offset_s: Option<i64>,
    /// `(changed_at, offset_before)` — an instant before `changed_at` was on the offset
    /// before.
    changed: Option<(i64, i64)>,
}

impl Default for LocalClock {
    fn default() -> Self {
        Self::UNKNOWN
    }
}

impl LocalClock {
    /// No offset could be read: times are UTC's, and say so.
    pub(crate) const UNKNOWN: Self = Self {
        offset_s: None,
        changed: None,
    };

    /// A clock `offset_s` east of UTC with no switch in the window.
    pub(crate) const fn fixed(offset_s: i64) -> Self {
        Self {
            offset_s: Some(offset_s),
            changed: None,
        }
    }

    /// The clock as this process reads it (`presence::local_offset_at`).
    pub(crate) fn read(now: i64) -> Self {
        Self::probe(now, crate::presence::local_offset_at)
    }

    /// [`Self::read`] over an injected offset reader. Pure for the test.
    fn probe(now: i64, offset_at: impl Fn(i64) -> Option<i64>) -> Self {
        let Some(offset) = offset_at(now) else {
            return Self::UNKNOWN;
        };
        let (mut before_at, mut after_at) = (now - CLOCK_WINDOW_S, now);
        let before = match offset_at(before_at) {
            Some(before) if before != offset => before,
            _ => return Self::fixed(offset),
        };
        while after_at - before_at > 60 {
            let mid = before_at + (after_at - before_at) / 2;
            match offset_at(mid) {
                Some(o) if o == before => before_at = mid,
                Some(_) => after_at = mid,
                None => return Self::fixed(offset),
            }
        }
        Self {
            offset_s: Some(offset),
            changed: Some((after_at, before)),
        }
    }

    /// The offset `at` was on (UTC's, `0`, when none is known).
    pub(crate) fn offset_at(self, at: i64) -> i64 {
        match self.changed {
            Some((since, before)) if at < since => before,
            _ => self.offset_s.unwrap_or(0),
        }
    }

    pub(crate) const fn known(self) -> bool {
        self.offset_s.is_some()
    }
}

impl PackagesProgramRow {
    fn from_status(name: &str, program: &atpkg::ProgramStatus, last_index_build: u64) -> Self {
        let kind = ProgramStateKind::parse(&program.state);
        let version = match &kind {
            ProgramStateKind::VendorLatest { version, .. }
            | ProgramStateKind::VendorKept { version, .. } => Some(version.clone()),
            _ => program
                .installed_build
                .map(atpkg::vendor_direct::build_words),
        };
        let latest_known = match &kind {
            ProgramStateKind::Managed { build, .. } => {
                Some(atpkg::vendor_direct::build_words(*build))
            }
            ProgramStateKind::VendorLatest { version, .. } => Some(version.clone()),
            _ => None,
        };
        Self {
            name: name.to_string(),
            installed_build: program.installed_build,
            state: program.state.clone(),
            facts: atpkg::stub::is_agent_program(name)
                .then(|| atpkg::stub::describe(name).map(VendorFacts::parse))
                .flatten(),
            update: ProgramUpdate {
                version,
                source: atpkg::state::source_words(name, &program.state, last_index_build),
                updated_at: None,
                latest_known,
                checked_at: None,
                fix: None,
            },
            words: None,
            kind,
            annotation: None,
            upstream: None,
        }
    }
}

/// A `status.toml` state line read through `atpkg::state`'s parsers, the same readers `doctor` and `which` use.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ProgramStateKind {
    Managed {
        build: u64,
        index: u64,
    },
    /// A vendor-direct program at its vendor's head: `managed 2.1.280 — Anthropic latest`.
    VendorLatest {
        version: String,
        vendor: String,
    },
    /// A vendor-direct program kept on `version` while the head was not taken, and why (a hold, a yank, a rollback).
    VendorKept {
        version: String,
        why: String,
    },
    Shadowed {
        build: u64,
        path: String,
    },
    System {
        path: String,
        retired: Option<String>,
    },
    /// An agent program the pass has not installed yet (`agent program — installing`, `atpkg::state::AGENT_INSTALLING`).
    AgentInstalling,
    Unavailable,
    Other,
}

impl ProgramStateKind {
    pub(crate) fn parse(state: &str) -> Self {
        if let Some((build, index)) = atpkg::state::managed_pin(state) {
            return Self::Managed { build, index };
        }
        if let Some((version, vendor)) = atpkg::state::vendor_latest(state) {
            return Self::VendorLatest {
                version: version.to_string(),
                vendor: vendor.to_string(),
            };
        }
        if let Some((version, why)) = atpkg::state::vendor_row(state) {
            return Self::VendorKept {
                version: version.to_string(),
                why: why.to_string(),
            };
        }
        if let Some((build, path)) = atpkg::state::shadowed_by(state) {
            return Self::Shadowed {
                build,
                path: path.to_string(),
            };
        }
        if let Some(path) = atpkg::state::system_path(state) {
            return Self::System {
                path: path.to_string(),
                retired: atpkg::state::system_retired(state).map(str::to_string),
            };
        }
        if state.starts_with(atpkg::state::AGENT_INSTALLING) {
            return Self::AgentInstalling;
        }
        if state.starts_with(atpkg::state::UNAVAILABLE_PREFIX) {
            return Self::Unavailable;
        }
        Self::Other
    }
}

/// What an agent program's row says beside its state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct VendorFacts {
    pub(crate) vendor: String,
    pub(crate) license: Option<String>,
    pub(crate) size: Option<String>,
}

impl VendorFacts {
    pub(crate) fn parse(line: &str) -> Self {
        let line = line.trim();
        let Some((vendor, rest)) = line.split_once(" \u{2014} ") else {
            return Self {
                vendor: line.to_string(),
                license: None,
                size: None,
            };
        };
        let mut parts = rest.split(',').map(str::trim).filter(|p| !p.is_empty());
        let license = parts.next().map(str::to_string);
        let size = parts
            .next()
            .filter(|p| p.contains("MB") || p.contains("GB") || p.contains("KB"))
            .map(str::to_string);
        Self {
            vendor: vendor.trim().to_string(),
            license,
            size,
        }
    }

    /// `"<vendor> · <license> · <size>"`, whichever facts the line carried.
    pub(crate) fn line(&self) -> String {
        let mut out = self.vendor.clone();
        for extra in [self.license.as_deref(), self.size.as_deref()]
            .into_iter()
            .flatten()
        {
            out.push_str(FACT_SEPARATOR);
            out.push_str(extra);
        }
        out
    }
}

/// How many lines one program's reason may take on the Packages page.
pub(crate) const MAX_REASON_LINES: usize = 4;

/// The narrowest row [`reason_lines`] wraps to: the bare `…(NNN more)` marker (11
/// characters) must fit on a line of its own.
const MIN_REASON_WIDTH: usize = 12;

/// atpkg's remedy marker: every remedy its text names is spelled `fix: <what to do>`.
const FIX_MARKER: &str = "fix:";

/// A long recorded refusal with its fix last (~700 characters): the shape the wrap must handle.
#[cfg(test)]
pub(crate) const INCIDENT_2026_09_14_REASON: &str = "error: stage: this process is \
    provenance-tracked (a probe file it wrote came back carrying com.apple.provenance) and \
    the untracked launchd lane could not run the helper (launchd job exited 78) — refusing \
    rather than write files that would all carry the tag, because the refuse policy is in \
    force (ATPKG_REFUSE_TRACKED_INSTALL set in this environment, or `[packages] \
    tracked_install = \"refuse\"` in aterm.toml): the tag follows the executable and the \
    parent process: every file a tagged trustc/targo (or a cutter descended from a tagged \
    process) writes inherits it, `xattr -d` exits 0 and removes nothing, and \
    tools/proof_snapshot.py refuses a tagged proof snapshot AFTER the ledger claim — a \
    burned build number (v0.83.0, 2026-09-12). fix: unset ATPKG_REFUSE_TRACKED_INSTALL and \
    set tracked_install = \"record\" (or drop the key) and the files are written in-process \
    and recorded beside the build as <build>.tracked-install (`aterm pkg doctor` names it \
    and every tagged shim), or clear what stopped launchd from running the helper — named \
    above — and retry";

/// The lines a program's `state` line paints on the Packages page, each within `width` average glyphs. atpkg spells every remedy `fix: …`, and this reader relies on it.
pub(crate) fn reason_lines(state: &str, width: usize) -> Vec<String> {
    let width = width.max(MIN_REASON_WIDTH);
    if fits(state, width) {
        return vec![state.to_string()];
    }
    let (diagnosis, fix) = match fix_marker(state) {
        Some(at) => {
            let fix = state[at + FIX_MARKER.len()..].trim();
            let diagnosis = state[..at]
                .trim_end()
                .trim_end_matches(['.', ';', ',', ':', '—', '–', '-'])
                .trim_end();
            if fix.is_empty() {
                (state, None)
            } else {
                (diagnosis, Some(fix))
            }
        }
        None => (state, None),
    };
    let mut lines = match fix {
        Some(fix) => wrap_words(&format!("{FIX_MARKER} {fix}"), width),
        None => Vec::new(),
    };
    let rest = wrap_words(diagnosis, width);
    let room = MAX_REASON_LINES.saturating_sub(lines.len());
    if rest.len() <= room {
        lines.extend(rest);
    } else if room == 0 {
        lines.push(more_marker(rest.len()));
    } else {
        lines.extend(bound_with_marker(rest, room, width));
    }
    lines
}

fn more_marker(hidden: usize) -> String {
    format!("\u{2026}({hidden} more)")
}

/// The first `room` of `lines`, the last of them giving up trailing words to the
/// `…(N more)` marker until it fits `width`.
fn bound_with_marker(lines: Vec<String>, room: usize, width: usize) -> Vec<String> {
    debug_assert!(room >= 1 && lines.len() > room);
    let hidden = lines.len() - room;
    let mut kept: Vec<String> = lines.into_iter().take(room).collect();
    let Some(last) = kept.pop() else {
        return kept;
    };
    let mut words: Vec<&str> = last.split_whitespace().collect();
    loop {
        let candidate = if words.is_empty() {
            more_marker(hidden + 1)
        } else {
            format!("{} {}", words.join(" "), more_marker(hidden))
        };
        if fits(&candidate, width) || words.is_empty() {
            kept.push(candidate);
            return kept;
        }
        words.pop();
    }
}

/// The byte offset of the `fix:` marker in `state`, when it carries one: the word
/// `fix:` at a word boundary and followed by whitespace or the end — `prefix:` and
/// `suffix:` are not remedies, and `fix:` glued to a path segment is not one either.
fn fix_marker(state: &str) -> Option<usize> {
    state.match_indices(FIX_MARKER).find_map(|(at, _)| {
        let boundary_before = state[..at]
            .chars()
            .next_back()
            .is_none_or(|c| !(c.is_alphanumeric() || c == '_'));
        let boundary_after = state[at + FIX_MARKER.len()..]
            .chars()
            .next()
            .is_none_or(char::is_whitespace);
        (boundary_before && boundary_after).then_some(at)
    })
}

/// Greedy word wrap to `width` average glyphs ([`glyph_advance`]'s estimate, since this
/// pure projection has no font), at the places the shared rule lets a line break
/// ([`aterm_grapheme::pieces`]): white space of every kind but a no-break space (a recorded
/// error may carry a newline), and between CJK characters, kinsoku kept, with nothing put
/// between them.
fn wrap_words(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut current = String::new();
    let mut used = 0usize;
    let budget = width * ADVANCE_UNIT;
    for piece in aterm_grapheme::pieces(text, aterm_grapheme::BreakKind::is_word_boundary) {
        let word = piece.text;
        let len = advance(word);
        let space = if piece.spaced { ADVANCE_UNIT } else { 0 };
        if used > 0 && used + space + len <= budget {
            if piece.spaced {
                current.push(' ');
            }
            current.push_str(word);
            used += space + len;
            continue;
        }
        if used > 0 {
            lines.push(std::mem::take(&mut current));
        }
        if len <= budget {
            current.push_str(word);
            used = len;
            continue;
        }
        let mut chunk = String::new();
        let mut chunk_used = 0usize;
        for c in word.chars() {
            let a = glyph_advance(c);
            if chunk_used + a > budget && !chunk.is_empty() {
                lines.push(std::mem::take(&mut chunk));
                chunk_used = 0;
            }
            chunk.push(c);
            chunk_used += a;
        }
        current = chunk;
        used = chunk_used;
    }
    if used > 0 {
        lines.push(current);
    }
    lines
}

/// The wrap's unit: one average lowercase glyph, in the sub-unit [`glyph_advance`]
/// counts in.
const ADVANCE_UNIT: usize = 20;

/// An approximate advance for `c` in a proportional UI face, in twentieths of an average
/// lowercase glyph: capitals, `_`, `—` and `@#%&` 1.2, `m`/`w` 1.5, the narrow glyphs and
/// most punctuation 0.65, a wide char 2, the rest 1.
const fn glyph_advance(c: char) -> usize {
    match c {
        'A'..='Z' | '_' | '\u{2014}' | '\u{2013}' | '@' | '#' | '%' | '&' => 24,
        'm' | 'w' => 30,
        'i' | 'j' | 'l' | 't' | 'f' | 'r' | '\'' | '"' | '`' | '.' | ',' | ':' | ';' | '!'
        | '|' | '(' | ')' | '[' | ']' | '{' | '}' | '/' | '\\' | '-' | ' ' => 13,
        _ if aterm_grapheme::char_width(c) > 1 => 2 * ADVANCE_UNIT,
        _ => ADVANCE_UNIT,
    }
}

/// The advance of `word`, summed over its glyphs ([`glyph_advance`]).
fn advance(word: &str) -> usize {
    word.chars().map(glyph_advance).sum()
}

/// Whether `text` fits a row of `width` units — the one predicate the short-state
/// pass-through, the wrap and the marker's tail share.
fn fits(text: &str, width: usize) -> bool {
    advance(text) <= width * ADVANCE_UNIT
}

/// Whether `text` fits one row line at `width`'s budget, measured as [`reason_lines`] measures, so a program row's detail rides inline exactly when it fits.
pub(crate) fn fits_one_line(text: &str, width: usize) -> bool {
    fits(text, width.max(MIN_REASON_WIDTH))
}

/// How many lines one Activity event may take on the Packages page.
pub(crate) const MAX_ACTIVITY_LINES: usize = 3;

/// The lines one Activity event paints, each within `width` average glyphs.
pub(crate) fn activity_text_lines(text: &str, width: usize) -> Vec<String> {
    let width = width.max(MIN_REASON_WIDTH);
    if fits(text, width) {
        return vec![text.to_string()];
    }
    let lines = wrap_words(text, width);
    if lines.len() <= MAX_ACTIVITY_LINES {
        lines
    } else {
        bound_with_marker(lines, MAX_ACTIVITY_LINES, width)
    }
}

/// atpkg's status ledger names its whole-toolset pseudo-program `*toolset*`, emphasis a native page would paint as literal asterisks.
pub(crate) fn package_program_display_name(name: &str) -> String {
    if name == "*toolset*" {
        return "ALab tools".to_string();
    }
    name.strip_prefix('*')
        .and_then(|inner| inner.strip_suffix('*'))
        .filter(|inner| !inner.is_empty())
        .unwrap_or(name)
        .to_string()
}

/// The words of atpkg's row for a vendor build whose head check has not reached
/// its vendor for a day: `managed <version> — <Vendor>'s release channel not
/// reached since <date>` (`record_vendor_status` in `crates/atpkg/src/cli.rs`).
const VENDOR_UNREACHED: &str = "'s release channel not reached since ";

/// One recorded row the Packages badge counts, as the headline names it, or `None`.
pub(crate) fn attention_item(name: &str, state: &str) -> Option<String> {
    if atpkg::state::is_unserved_toolset(state) {
        return None;
    }
    let display = package_program_display_name(name);
    if atpkg::doctor::is_recorded_problem(name, state) {
        let what = match state.split_once(':').map_or("", |(head, _)| head) {
            "aborted" => "aborted",
            "tombstoned" => "disabled",
            "unavailable" | "blocked" => "unavailable",
            _ if atpkg::state::not_current(state).is_some_and(|(r, _)| r == "refused") => "refused",
            _ => "failed",
        };
        return Some(format!("{display} {what}"));
    }
    let (_, why) = state
        .strip_prefix(atpkg::state::MANAGED_PREFIX)?
        .split_once(" \u{2014} ")?;
    let (vendor, since) = why.split_once(VENDOR_UNREACHED)?;
    Some(format!("{vendor} not reached since {since}"))
}

/// Everything the Packages badge counts.
pub(crate) fn attention_items(
    programs: &[PackagesProgramRow],
    declined: bool,
    pass_trouble: Option<&PassTrouble>,
    cc_prereq: Option<&atpkg::prereq::CcVerdict>,
) -> Vec<String> {
    programs
        .iter()
        .filter(|_| !declined)
        .filter_map(|row| attention_item(&row.name, &row.state))
        .chain(pass_trouble.map(PassTrouble::item))
        .chain(cc_prereq.map(cc_prereq_item))
        .collect()
}

/// The attention item for a C toolchain that blocks builds, short enough for the headline ([`PackagesStatusReport::cc_prereq`]).
pub(crate) fn cc_prereq_item(verdict: &atpkg::prereq::CcVerdict) -> String {
    if matches!(verdict, atpkg::prereq::CcVerdict::Broken { .. }) {
        "the C compiler cannot build a program".to_string()
    } else if cfg!(target_os = "macos") {
        format!("no C toolchain \u{2014} run {}", atpkg::prereq::act())
    } else {
        "no working C compiler".to_string()
    }
}

/// The detail under "Package tool missing": what is wrong with this copy, and how to get one that manages ALab tools.
pub(crate) const PACKAGE_TOOL_MISSING_DETAIL: &str =
    "This copy of aterm has no package tool. Install aterm from alab.systems to manage ALab tools.";

/// The longest cause a headline item quotes, in characters.
const HEADLINE_CAUSE_CHARS: usize = 48;

/// The prefix every attention headline opens with ([`attention_headline`]).
pub(crate) const ATTENTION_PREFIX: &str = "Needs attention: ";

/// The headline that names what failed (`Needs attention: codex failed · …`), at most [`ATTENTION_NAMED`] items and a count for the rest.
pub(crate) fn attention_headline(items: &[String]) -> Option<String> {
    if items.is_empty() {
        return None;
    }
    let mut named = items
        .iter()
        .take(ATTENTION_NAMED)
        .cloned()
        .collect::<Vec<_>>()
        .join(crate::native_ui::FACT_SEPARATOR);
    if items.len() > ATTENTION_NAMED {
        named.push_str(&format!(
            "{}and {} more",
            crate::native_ui::FACT_SEPARATOR,
            items.len() - ATTENTION_NAMED
        ));
    }
    Some(format!("{ATTENTION_PREFIX}{named}"))
}

/// The headline for a machine the signed index serves nothing for ([`atpkg::state::is_unserved_toolset`]).
pub(crate) const UNSERVED_HEADLINE: &str = "ALab tools aren\u{2019}t available for this Mac yet";

/// The headline while a `[packages] prefix` this build ignores stops unattended installs (`atpkg::config::PackagesConfig::installs_unattended`).
pub(crate) const IGNORED_PREFIX_HEADLINE: &str =
    "Automatic installs are paused — [packages] prefix is set";

/// The card's detail while a verb runs: where its progress is. Work that ends inside the
/// band's progress grace never shows a row, so the card promises none.
const BUSY_DETAIL: &str = "If it takes a while, its progress shows at the top of the window.";

/// Which command result a Packages headline presents, as the `NativePackagesWorker` model counts it (`presented_result`: 0 none, 1 success, 2 failure).
#[cfg(test)]
pub(crate) fn presented_result_of(headline: &str) -> u8 {
    if headline.starts_with(ATTENTION_PREFIX) {
        0
    } else if headline.ends_with("completed") {
        1
    } else if headline.ends_with("failed") {
        2
    } else {
        0
    }
}

/// What kind of trouble a background pass left the Packages badge ([`PackagesService::note_pass_trouble`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PassTroubleKind {
    /// A failure marker, a refused whole pass, a child that died mid-pass.
    Failed,
    /// The window stood down for this launch behind a pass that never finished, and will not retry; the cause names the command to run.
    StoodDown,
}

impl PassTroubleKind {
    /// The badge's item for it ([`attention_items`]).
    const fn item(self) -> &'static str {
        match self {
            Self::Failed => "the last package check failed",
            Self::StoodDown => "the package check didn\u{2019}t run",
        }
    }
}

/// A background pass's trouble the Packages badge remembers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PassTrouble {
    pub(crate) kind: PassTroubleKind,
    pub(crate) cause: String,
    pub(crate) noted_unix: i64,
}

impl PassTrouble {
    /// The badge's item, naming the cause: `the last package check failed: prefix is not
    /// writable`, atpkg's own words without their `atpkg: ` prefix, the first clause only
    /// and at most [`HEADLINE_CAUSE_CHARS`]; the whole cause leads the detail.
    fn item(&self) -> String {
        let cause = self.cause.trim();
        let cause = cause.strip_prefix("atpkg: ").unwrap_or(cause);
        let clause = cause
            .split([';', '\u{2014}'])
            .next()
            .unwrap_or(cause)
            .trim()
            .trim_end_matches(['.', ',', ':']);
        let mut short: String = clause.chars().take(HEADLINE_CAUSE_CHARS).collect();
        if clause.chars().count() > HEADLINE_CAUSE_CHARS {
            short = short
                .rsplit_once(' ')
                .map_or(short.clone(), |(head, _)| head.to_string());
            short.push('\u{2026}');
        }
        if short.is_empty() {
            self.kind.item().to_string()
        } else {
            format!("{}: {short}", self.kind.item())
        }
    }
}

/// How many failures the headline names before it counts the rest.
const ATTENTION_NAMED: usize = 3;

/// Facts about the co-located package manager, collected off the event loop by one worker pass.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PackagesStatusReport {
    /// The co-located `atpkg` binary exists beside our executable.
    pub(crate) available: bool,
    /// The co-located CLI's own posture: the compiled root anchor is present.
    pub(crate) manager_enabled: bool,
    /// The pinned root key's operator-facing fingerprint (the doctor line).
    pub(crate) root_fingerprint: String,
    /// `status.toml` existed and parsed (atpkg has run at least once).
    pub(crate) recorded: bool,
    /// `status.toml` fields (empty when `recorded` is false).
    pub(crate) updated_at: String,
    pub(crate) outcome: String,
    pub(crate) index_source: String,
    pub(crate) programs: Vec<PackagesProgramRow>,
    /// Bounded, operator-readable diagnostic when some private package metadata could not be admitted or parsed.
    pub(crate) collection_error: Option<String>,
    /// The store carries atpkg's declined marker (`aterm pkg uninstall --all`).
    pub(crate) declined: bool,
    /// `status.toml`'s `last_success_at` (RFC 3339, empty when never).
    pub(crate) last_success_at: String,
    /// The package log's newest events, newest first (at most [`atpkg::packages_log::ACTIVITY_EVENTS`]) — Settings ▸ Packages' Activity.
    pub(crate) activity: Vec<atpkg::packages_log::Entry>,
    /// Where the package log is (`<logs dir>/packages.log`), when a log directory resolves: what "Open Log" opens.
    pub(crate) log_path: Option<std::path::PathBuf>,
    /// The local clock, read on the worker at each collection ([`LocalClock::read`]).
    pub(crate) clock: LocalClock,
    /// The sentence for a `[packages] prefix` this build does not use (`atpkg::config::PackagesConfig::ignored_prefix`, in `atpkg::config::ignored_prefix_note`'s words).
    pub(crate) ignored_prefix: Option<String>,
    /// The C toolchain's verdict when it blocks builds (no compiler, or one that cannot build a program), probed only over an installed, undeclined toolset.
    pub(crate) cc_prereq: Option<atpkg::prereq::CcVerdict>,
}

impl PackagesStatusReport {
    /// The zero state before any worker observation: nothing is claimed available or
    /// enabled until a collection pass has looked.
    const fn unobserved() -> Self {
        Self {
            activity: Vec::new(),
            log_path: None,
            clock: LocalClock::UNKNOWN,
            available: false,
            manager_enabled: false,
            root_fingerprint: String::new(),
            recorded: false,
            updated_at: String::new(),
            outcome: String::new(),
            index_source: String::new(),
            programs: Vec::new(),
            collection_error: None,
            declined: false,
            last_success_at: String::new(),
            ignored_prefix: None,
            cc_prereq: None,
        }
    }

    /// Pure assembly from atpkg's own record types — the testable core of [`collect_packages_status`].
    pub(crate) fn from_parts(
        available: bool,
        manager_enabled: bool,
        root_fingerprint: String,
        status: Option<&atpkg::Status>,
        links: &[(String, Option<std::path::PathBuf>)],
    ) -> Self {
        let mut programs: Vec<PackagesProgramRow> = status
            .map(|status| {
                status
                    .programs
                    .iter()
                    .map(|(name, program)| {
                        PackagesProgramRow::from_status(name, program, status.last_index_build)
                    })
                    .collect()
            })
            .unwrap_or_default();
        for (name, target) in links {
            let annotation = Some(match target {
                Some(path) => {
                    let mut label = String::from("dev-link → ");
                    label.push_str(&path.display().to_string());
                    label
                }
                None => "dev-link".to_string(),
            });
            if let Some(row) = programs.iter_mut().find(|row| row.name == *name) {
                row.annotation = annotation;
            } else {
                programs.push(PackagesProgramRow {
                    name: name.clone(),
                    installed_build: None,
                    state: "linked".to_string(),
                    kind: ProgramStateKind::Other,
                    facts: None,
                    annotation,
                    upstream: None,
                    update: ProgramUpdate::default(),
                    words: None,
                });
            }
        }
        if let Some(status) = status {
            for key in &status.seams {
                let Some(rest) = key.strip_prefix("refused:rustup:") else {
                    continue;
                };
                let (name, why) = rest.split_once(": ").unwrap_or((rest, ""));
                programs.push(PackagesProgramRow {
                    name: format!("rustup:{name}"),
                    installed_build: None,
                    state: if why.is_empty() {
                        "rustup seam refused".to_string()
                    } else {
                        format!("rustup seam refused: {why}")
                    },
                    kind: ProgramStateKind::Other,
                    facts: None,
                    annotation: Some("cargo +trust does not run the managed toolchain".to_string()),
                    upstream: None,
                    update: ProgramUpdate::default(),
                    words: None,
                });
            }
        }
        programs.sort_by(|a, b| a.name.cmp(&b.name));
        Self {
            available,
            manager_enabled,
            root_fingerprint,
            recorded: status.is_some(),
            updated_at: status.map(|s| s.updated_at.clone()).unwrap_or_default(),
            outcome: status.map(|s| s.outcome.clone()).unwrap_or_default(),
            index_source: status.map(|s| s.index_source.clone()).unwrap_or_default(),
            programs,
            collection_error: None,
            declined: false,
            last_success_at: status
                .map(|s| s.last_success_at.clone())
                .unwrap_or_default(),
            activity: Vec::new(),
            log_path: None,
            clock: LocalClock::UNKNOWN,
            ignored_prefix: None,
            cc_prereq: None,
        }
    }

    /// What the store says about each installed row, read on the worker.
    pub(crate) fn attach_store_facts(
        &mut self,
        layout: &atpkg::Layout,
        pins: Option<&std::collections::BTreeMap<String, u64>>,
    ) {
        for row in &mut self.programs {
            let Some(build) = row.installed_build else {
                continue;
            };
            if atpkg::vendor_direct::is_vendor(&row.name) {
                if let Some(record) =
                    atpkg::vendor_direct::complete_record(&layout.build_dir(&row.name, build))
                {
                    row.update.version = Some(record.version.to_string());
                    row.update.updated_at = Some(record.verified_at);
                }
                if let Some(stamp) = atpkg::vendor_direct::ProgramStamp::read(layout, &row.name) {
                    if let Some(head) = stamp.last_head {
                        row.update.latest_known = Some(head.to_string());
                    }
                    row.update.checked_at =
                        (stamp.last_checked_at > 0).then_some(stamp.last_checked_at);
                }
            } else if let Some(pin) = pins.and_then(|p| p.get(&row.name)) {
                row.update.latest_known = Some(atpkg::vendor_direct::build_words(*pin));
            }
            if row.update.updated_at.is_none() {
                row.update.updated_at =
                    std::fs::symlink_metadata(layout.program_current(&row.name))
                        .and_then(|m| m.modified())
                        .ok()
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .and_then(|d| i64::try_from(d.as_secs()).ok());
            }
        }
    }
}

const MAX_PACKAGE_COLLECTION_ERRORS: usize = 4;

fn collection_error_summary(errors: Vec<String>, total: usize) -> Option<String> {
    if errors.is_empty() {
        return None;
    }
    let mut summary = errors.join(" · ");
    if total > errors.len() {
        summary.push_str(" · … and ");
        summary.push_str(&(total - errors.len()).to_string());
        summary.push_str(" more metadata errors");
    }
    Some(summary)
}

/// Collect the packages report from the real machine.
pub(crate) fn collect_packages_status(available: bool) -> PackagesStatusReport {
    let config = atpkg::config::load();
    let layout = atpkg::store::resolve_from(&config);
    let login_path = std::ffi::OsString::from(crate::spawn::atpkg_child_path_once());
    let mut report =
        collect_packages_status_from_layout(available, layout.as_ref(), Some(&login_path));
    report.ignored_prefix = config
        .ignored_prefix
        .as_deref()
        .map(atpkg::config::ignored_prefix_note);
    report.cc_prereq = cc_prereq_note(&report, || atpkg::prereq::probe(Some(&login_path)));
    report.log_path = atpkg::packages_log::log_path();
    report.activity =
        atpkg::packages_log::tail_of(atpkg::packages_log::ACTIVITY_EVENTS, shown_in_activity);
    let now = aterm_time::unix_secs().map_or(0, |s| i64::try_from(s).unwrap_or(i64::MAX));
    report.clock = LocalClock::read(now);
    report
}

/// The C toolchain's verdict for the page ([`PackagesStatusReport::cc_prereq`]): `probe`
/// runs only over a toolset it would stop building (an installed program, nothing removed
/// on purpose: doctor's own gate), and the verdict is said only when it is an answer that
/// blocks builds.
fn cc_prereq_note(
    report: &PackagesStatusReport,
    probe: impl FnOnce() -> atpkg::prereq::CcVerdict,
) -> Option<atpkg::prereq::CcVerdict> {
    let toolset_installed = !report.declined
        && report
            .programs
            .iter()
            .any(|row| row.installed_build.is_some());
    if !toolset_installed {
        return None;
    }
    let verdict = probe();
    verdict.blocks_builds().then_some(verdict)
}

/// The page's report over `layout`, its shadowed rows computed for `path_var` (the login
/// shell's PATH, what the user's terminals run with).
fn collect_packages_status_from_layout(
    available: bool,
    layout: Option<&atpkg::Layout>,
    path_var: Option<&std::ffi::OsStr>,
) -> PackagesStatusReport {
    let mut errors = Vec::new();
    let mut error_count = 0usize;
    let mut record_error = |message: String| {
        error_count = error_count.saturating_add(1);
        if errors.len() < MAX_PACKAGE_COLLECTION_ERRORS {
            errors.push(message);
        }
    };
    let status = layout.and_then(|layout| match atpkg::status::read_checked(layout) {
        Ok(status) => status.map(|s| atpkg::status::with_shadows(layout, s, path_var)),
        Err(error) => {
            record_error(format!("Could not read status.toml: {error}"));
            None
        }
    });
    let shadow_fix = |name: &str, kind: &ProgramStateKind| match (layout, kind) {
        (Some(layout), ProgramStateKind::Shadowed { path, .. }) => {
            atpkg::cli::shadowed_fix(layout, name, std::path::Path::new(path), path_var)
        }
        _ => None,
    };
    let links: Vec<(String, Option<std::path::PathBuf>)> = layout
        .map(|layout| match atpkg::linked_programs_checked(layout) {
            Ok(names) => names
                .into_iter()
                .filter_map(|name| match atpkg::linked_checkout_checked(layout, &name) {
                    Ok(Some(target)) => Some((name, Some(target))),
                    Ok(None) => {
                        record_error(format!("Link marker {name:?} disappeared while reading"));
                        None
                    }
                    Err(error) => {
                        record_error(format!("Could not read link marker {name:?}: {error}"));
                        None
                    }
                })
                .collect(),
            Err(error) => {
                record_error(format!("Could not enumerate package links: {error}"));
                Vec::new()
            }
        })
        .unwrap_or_default();
    let manager_enabled = atpkg::manager_enabled();
    let mut report = PackagesStatusReport::from_parts(
        available,
        manager_enabled,
        atpkg::root_key_fingerprint(),
        status.as_ref(),
        &links,
    );
    for row in &mut report.programs {
        row.update.fix = shadow_fix(&row.name, &row.kind);
    }
    if let Some(layout) = layout {
        for row in &mut report.programs {
            row.upstream =
                atpkg::alab::program_upstream(layout, &row.name).map(|upstream| upstream.words());
        }
        report.declined = layout.declined().is_file();
        let pins = atpkg::cli::latest_known_pins(layout, &atpkg::config::load());
        report.attach_store_facts(layout, pins.as_ref().map(|(_, pins)| pins));
    }
    report.collection_error = collection_error_summary(errors, error_count);
    report
}

/// Host-owned reducer for the packages surface.
pub(crate) struct PackagesService {
    revision: u64,
    /// Worker generation: a completion must present the exact sequence its
    /// `begin` minted, so a superseded worker cannot import stale facts.
    sequence: u64,
    inflight: bool,
    busy: Option<PackagesBusy>,
    report: Option<PackagesStatusReport>,
    last_command: Option<PackagesCommandOutcome>,
    /// What bare `atpkg machine` last measured, plus this launch's memory of what
    /// changed.
    machine: MachinePosture,
    /// The last background toolchain pass's trouble (a failure marker, a whole pass
    /// refused before it could write a record, a stand-down that will not retry), held
    /// until a pass is known to have completed since ([`Self::note_pass_trouble`], cleared
    /// by [`Self::note_pass_clean`] and [`Self::finish`]).
    pass_trouble: Option<PassTrouble>,
    /// How many programs the last check or install moved to a new build, so the card says
    /// `2 programs updated` rather than a bare "completed".
    updated: usize,
    /// Whether this copy can manage packages at all — known before any status read
    /// ([`Self::note_tool`]): the co-located `atpkg` stat and the compiled root key.
    tool: Option<PackageTool>,
}

/// What this copy's package tool IS, as opposed to what its status says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PackageTool {
    pub(crate) available: bool,
    pub(crate) manager_enabled: bool,
}

/// The `[machine]` host settings as the window confirmed them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct MachinePosture {
    /// A read has completed at least once (a parsed state or a read error).
    pub(crate) observed: bool,
    /// The parsed record, when the last read produced one.
    pub(crate) state: Option<atpkg::machine::MachineState>,
    /// Why the last read produced no record (the child could not be spawned, or printed no `machine-state:` line — which is never "nothing to apply").
    pub(crate) read_error: Option<String>,
    /// The last `machine-settings:` body this process saw and when it arrived.
    pub(crate) last_change: Option<(String, std::time::SystemTime)>,
    /// The last `atpkg machine apply` verdict sentence this process saw.
    pub(crate) last_verdict: Option<String>,
    /// The machine read worker is running.
    pub(crate) refreshing: bool,
    /// A read was asked for while one was running.
    pub(crate) rerun: bool,
    /// A pass printed a `machine-settings:` change and its own `machine-state:` record is expected on the same stream.
    pub(crate) awaiting_record: bool,
    /// A worker read is running and a newer record (a pass's own, measured after its apply) landed meanwhile.
    pub(crate) superseded: bool,
    /// Reads started so far — bumped when one is spawned (a request admitted, or a queued rerun handed back to be spawned).
    pub(crate) reads_started: u64,
    /// `reads_started` when the open expectation was opened.
    pub(crate) expectation_read_gen: u64,
}

/// Scalar projection used only by Tier-1 conformance.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PackagesModelState {
    pub(crate) sequence: u64,
    pub(crate) inflight: bool,
    pub(crate) operation: u8,
    pub(crate) observed: bool,
    pub(crate) last_operation: u8,
    pub(crate) last_result: u8,
    pub(crate) presented_result: u8,
}

impl PackagesService {
    pub(crate) fn new() -> Self {
        Self {
            revision: 1,
            sequence: 0,
            inflight: false,
            busy: None,
            report: None,
            last_command: None,
            machine: MachinePosture::default(),
            pass_trouble: None,
            updated: 0,
            tool: None,
        }
    }

    /// Record what this copy's package tool is.
    pub(crate) fn note_tool(&mut self, available: bool) -> bool {
        let tool = PackageTool {
            available,
            manager_enabled: atpkg::manager_enabled(),
        };
        if self.tool == Some(tool) {
            return false;
        }
        self.tool = Some(tool);
        self.revision = self.revision.saturating_add(1);
        true
    }

    pub(crate) const fn revision(&self) -> u64 {
        self.revision
    }

    pub(crate) const fn busy(&self) -> Option<PackagesBusy> {
        self.busy
    }

    /// The machine posture as last reduced.
    #[cfg(test)]
    pub(crate) fn machine(&self) -> &MachinePosture {
        &self.machine
    }

    /// Mark a read as running without asking for one.
    #[cfg(test)]
    pub(crate) fn set_machine_refreshing(&mut self, refreshing: bool) {
        if self.machine.refreshing == refreshing {
            return;
        }
        self.machine.refreshing = refreshing;
        self.revision = self.revision.saturating_add(1);
    }

    /// Ask for a machine read.
    pub(crate) const fn request_machine_read(&mut self) -> bool {
        if self.machine.refreshing {
            self.machine.rerun = true;
            return false;
        }
        self.machine.refreshing = true;
        self.machine.reads_started = self.machine.reads_started.saturating_add(1);
        self.revision = self.revision.saturating_add(1);
        true
    }

    /// Reduce one machine read: a parsed `machine-state:` record, or the reason there was none.
    #[must_use = "a queued rerun must be spawned by the caller"]
    pub(crate) fn replace_machine_state(
        &mut self,
        result: Result<atpkg::machine::MachineState, String>,
    ) -> bool {
        self.machine.observed = true;
        let rerun = std::mem::take(&mut self.machine.rerun);
        self.machine.refreshing = rerun;
        let completed_gen = self.machine.reads_started;
        if self.machine.awaiting_record && completed_gen > self.machine.expectation_read_gen {
            self.machine.awaiting_record = false;
        }
        if rerun {
            // The caller spawns the queued read now: count it as started.
            self.machine.reads_started = self.machine.reads_started.saturating_add(1);
        }
        if std::mem::take(&mut self.machine.superseded) {
            self.revision = self.revision.saturating_add(1);
            return rerun;
        }
        match result {
            Ok(state) => {
                self.machine.state = Some(state);
                self.machine.read_error = None;
            }
            Err(error) => {
                // Keep the last good record beside the error: a transient read
                // failure must not erase what was measured a moment ago.
                self.machine.read_error = Some(error);
            }
        }
        self.revision = self.revision.saturating_add(1);
        rerun
    }

    /// Remember a `machine-settings:` change (the pull-down row's body) and when this process saw it.
    pub(crate) fn note_machine_change(&mut self, body: String, at: std::time::SystemTime) {
        self.machine.last_change = Some((body, at));
        self.revision = self.revision.saturating_add(1);
    }

    /// A pass printed a change; its own `machine-state:` record follows on the same stream.
    pub(crate) const fn expect_machine_record(&mut self) {
        if self.machine.awaiting_record {
            return;
        }
        self.machine.awaiting_record = true;
        self.machine.expectation_read_gen = self.machine.reads_started;
        self.revision = self.revision.saturating_add(1);
    }

    /// Reduce a record a pass printed after its apply, the newest measurement there is.
    pub(crate) fn note_machine_record(&mut self, state: atpkg::machine::MachineState) {
        self.machine.observed = true;
        self.machine.state = Some(state);
        self.machine.read_error = None;
        self.machine.awaiting_record = false;
        self.machine.rerun = false;
        if self.machine.refreshing {
            self.machine.superseded = true;
        }
        self.revision = self.revision.saturating_add(1);
    }

    /// The pass's stream ended (or its record did not parse) while a record was still expected.
    #[must_use = "an open expectation must be answered by a spawned read"]
    pub(crate) const fn take_record_expectation(&mut self) -> bool {
        if !self.machine.awaiting_record {
            return false;
        }
        self.machine.awaiting_record = false;
        self.revision = self.revision.saturating_add(1);
        true
    }

    /// A background toolchain pass left trouble of `kind` with atpkg's `cause`, learned at `noted`.
    pub(crate) fn note_pass_trouble(
        &mut self,
        kind: PassTroubleKind,
        cause: String,
        noted: std::time::SystemTime,
    ) -> bool {
        if self
            .pass_trouble
            .as_ref()
            .is_some_and(|t| t.kind == kind && t.cause == cause)
        {
            return false;
        }
        let noted_unix = noted
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX));
        self.pass_trouble = Some(PassTrouble {
            kind,
            cause,
            noted_unix,
        });
        self.revision = self.revision.saturating_add(1);
        true
    }

    /// A later whole pass ran clean: the condition the remembered trouble named is gone, and so is that half of the badge.
    pub(crate) fn note_pass_clean(&mut self) -> bool {
        if self.pass_trouble.take().is_none() {
            return false;
        }
        self.revision = self.revision.saturating_add(1);
        true
    }

    /// Whether `completion` proves a pass completed after the remembered trouble: a
    /// whole-pass verb from Settings (Check, Install) that succeeded, or a record whose
    /// `last_success_at` (stamped by whoever ran the pass, a terminal's `aterm pkg update`
    /// included) is later than the moment the trouble was noted.
    fn completion_clears_trouble(&self, completion: &PackagesWorkerCompletion) -> bool {
        let Some(trouble) = self.pass_trouble.as_ref() else {
            return false;
        };
        if matches!(
            completion.command,
            Some(PackagesCommandOutcome::Succeeded {
                operation: PackagesBusy::Check | PackagesBusy::Install,
            })
        ) {
            return true;
        }
        aterm_update_core::pkg_check::rfc3339_to_unix(&completion.report.last_success_at)
            .is_some_and(|success| success > trouble.noted_unix)
    }

    /// Record that a pass refused or failed to apply machine settings so the status does not show a stale success.
    pub(crate) fn note_machine_verdict(&mut self, verdict: String) {
        self.machine.last_verdict = Some(verdict);
        self.revision = self.revision.saturating_add(1);
    }

    #[cfg(test)]
    pub(crate) fn model_state(&self) -> PackagesModelState {
        let operation = match (self.inflight, self.busy) {
            (false, None) => 0,
            (true, None) => 1,
            (_, Some(PackagesBusy::Check)) => 2,
            (_, Some(PackagesBusy::Install)) => 3,
            (_, Some(PackagesBusy::Uninstall)) => 4,
            (_, Some(PackagesBusy::MachineApply)) => 5,
        };
        let (last_operation, last_result) = match self.last_command.as_ref() {
            None => (0, 0),
            Some(PackagesCommandOutcome::Succeeded { operation }) => (
                match operation {
                    PackagesBusy::Check => 2,
                    PackagesBusy::Install => 3,
                    PackagesBusy::Uninstall => 4,
                    PackagesBusy::MachineApply => 5,
                },
                1,
            ),
            Some(PackagesCommandOutcome::Failed { operation, .. }) => (
                match operation {
                    PackagesBusy::Check => 2,
                    PackagesBusy::Install => 3,
                    PackagesBusy::Uninstall => 4,
                    PackagesBusy::MachineApply => 5,
                },
                2,
            ),
        };
        let headline = self.state(true, true, false).projection().headline;
        let presented_result = presented_result_of(&headline);
        PackagesModelState {
            sequence: self.sequence,
            inflight: self.inflight,
            operation,
            observed: self.report.is_some(),
            last_operation,
            last_result,
            presented_result,
        }
    }

    /// Begin one worker pass (`busy = None` ⇒ a silent status refresh).
    pub(crate) fn begin(&mut self, busy: Option<PackagesBusy>) -> Option<u64> {
        if self.inflight {
            return None;
        }
        self.sequence = self.sequence.saturating_add(1);
        self.inflight = true;
        self.busy = busy;
        if busy.is_some() {
            self.last_command = None;
            self.updated = 0;
        }
        self.revision = self.revision.saturating_add(1);
        Some(self.sequence)
    }

    /// Reduce one worker completion.
    pub(crate) fn finish(&mut self, sequence: u64, completion: PackagesWorkerCompletion) -> bool {
        if !self.inflight || sequence != self.sequence {
            return false;
        }
        let command_operation = completion
            .command
            .as_ref()
            .map(PackagesCommandOutcome::operation);
        if self.busy != command_operation {
            return false;
        }
        self.inflight = false;
        self.busy = None;
        if self.completion_clears_trouble(&completion) {
            self.pass_trouble = None;
        }
        if let Some(PackagesCommandOutcome::Succeeded {
            operation: PackagesBusy::Check | PackagesBusy::Install,
        }) = completion.command
        {
            self.updated = updated_programs(self.report.as_ref(), &completion.report);
        }
        self.tool = Some(PackageTool {
            available: completion.report.available,
            manager_enabled: completion.report.manager_enabled,
        });
        self.report = Some(completion.report);
        if completion.command.is_some() {
            self.last_command = completion.command;
        }
        if let Some(state) = completion.machine_state {
            // The apply's own record is the newest measurement, like a streamed pass's: it
            // closes the expectation its change line opened.
            self.note_machine_record(state);
        }
        if let Some(verdict) = completion.machine_verdict {
            self.machine.last_verdict = Some(match self.last_command.as_ref() {
                Some(PackagesCommandOutcome::Failed { operation, message })
                    if operation.applies_machine_settings() =>
                {
                    format!("failed — {message}; earlier in the pass: {verdict}")
                }
                _ => verdict,
            });
        }
        self.revision = self.revision.saturating_add(1);
        true
    }

    /// Release a reservation whose worker never started (thread-spawn failure).
    pub(crate) const fn abort(&mut self, sequence: u64) -> bool {
        if !self.inflight || sequence != self.sequence {
            return false;
        }
        self.inflight = false;
        self.busy = None;
        self.revision = self.revision.saturating_add(1);
        true
    }

    /// The state a Settings controller renders, resolved against the current config consent flags (the config is host-owned; the service holds only worker facts).
    pub(crate) fn state(
        &self,
        master_enabled: bool,
        auto_install: bool,
        loop_running: bool,
    ) -> PackagesState {
        let report = self.report.clone().unwrap_or_else(|| {
            let mut report = PackagesStatusReport::unobserved();
            if let Some(tool) = self.tool {
                report.available = tool.available;
                report.manager_enabled = tool.manager_enabled;
            }
            report
        });
        PackagesState {
            observed: self.report.is_some(),
            tool_observed: self.tool.is_some(),
            report,
            busy: self.busy,
            inflight: self.inflight,
            last_command: self.last_command.clone(),
            master_enabled,
            auto_install,
            loop_running,
            machine: self.machine.clone(),
            saved_universal_control: atpkg::config::UniversalControlPolicy::Off,
            saved_rosetta: true,
            pass_trouble: self.pass_trouble.clone(),
            retired_switch_note: None,
            updated: self.updated,
        }
    }
}

/// How many programs `after` holds at a different installed build than
/// `before` did (a program new to the report with a build counts): what a
/// check or an install changed.
fn updated_programs(before: Option<&PackagesStatusReport>, after: &PackagesStatusReport) -> usize {
    after
        .programs
        .iter()
        .filter(|row| row.installed_build.is_some())
        .filter(|row| {
            before
                .and_then(|b| b.programs.iter().find(|old| old.name == row.name))
                .is_none_or(|old| old.installed_build != row.installed_build)
        })
        .count()
}

/// The snapshot a [`crate::native_settings::SettingsApp`] holds (the [`crate::update_screen::UpdateState`] analogue).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PackagesState {
    /// A worker pass has completed at least once (before that the page says
    /// "Reading package status…" instead of claiming anything).
    observed: bool,
    /// What the package tool is — present, and built with its root key — is
    /// known ([`PackagesService::note_tool`]), whether or not a status read has
    /// landed: admission reads THIS, the status lines read [`Self::observed`].
    tool_observed: bool,
    report: PackagesStatusReport,
    busy: Option<PackagesBusy>,
    /// Any worker is inflight, the silent status refresh (`busy` `None`) included. Actions
    /// gate on this, not `busy`: the host's one-worker rule would drop a click admitted
    /// during a silent refresh, so the buttons disable for that brief window.
    inflight: bool,
    /// Final result of the most recent UI-initiated verb. Silent refreshes keep
    /// it; starting a new verb clears it.
    last_command: Option<PackagesCommandOutcome>,
    /// Resolved `[packages]` flags (`enabled`, the retired `auto_update` folded in, and
    /// `auto_install`), display only; the switches edit the config keys.
    master_enabled: bool,
    auto_install: bool,
    /// The background updater thread started for this process. What it runs follows the
    /// saved switches live, so only its absence needs saying.
    loop_running: bool,
    /// The `[machine]` host settings as confirmed by the machine read worker.
    machine: MachinePosture,
    /// The saved `[machine]` switch, from the host's config: display only, so the status can
    /// say when a saved switch has not been read yet.
    saved_universal_control: atpkg::config::UniversalControlPolicy,
    /// The saved `[machine] rosetta` switch, from the host's config (default on), which the
    /// apply child reads fresh, as it reads the Universal Control one.
    saved_rosetta: bool,
    /// The last background pass's trouble, until a pass completes since
    /// ([`PackagesService::note_pass_trouble`]).
    pass_trouble: Option<PassTrouble>,
    /// atpkg's sentence for a retired `[packages] auto_update = false` that holds Keep ALab
    /// tools up to date off (`atpkg::config::auto_update_note`): why
    /// [`Self::master_enabled`] is off, never a second switch.
    retired_switch_note: Option<String>,
    /// Programs the last check or install moved ([`PackagesService`]'s count).
    updated: usize,
}

impl PackagesState {
    /// Attach the host's saved `[machine]` switch (the config is host-owned; the service holds only worker facts).
    pub(crate) const fn with_machine_config(
        mut self,
        universal_control: atpkg::config::UniversalControlPolicy,
    ) -> Self {
        self.saved_universal_control = universal_control;
        self
    }

    /// Attach the host's saved `[machine] rosetta` switch (`Config::machine_rosetta`).
    pub(crate) const fn with_machine_rosetta(mut self, rosetta: bool) -> Self {
        self.saved_rosetta = rosetta;
        self
    }

    /// Attach the host's note on a retired `[packages] auto_update = false` holding Keep ALab tools up to date off (`Config::packages_retired_switch_note`).
    pub(crate) fn with_retired_switch_note(mut self, note: Option<String>) -> Self {
        self.retired_switch_note = note;
        self
    }
    /// The pre-observation default a freshly-created Settings controller holds until the host publishes a real snapshot.
    pub(crate) fn unobserved() -> Self {
        Self {
            observed: false,
            tool_observed: false,
            report: PackagesStatusReport::unobserved(),
            busy: None,
            inflight: false,
            last_command: None,
            master_enabled: true,
            auto_install: true,
            loop_running: false,
            machine: MachinePosture::default(),
            saved_universal_control: atpkg::config::UniversalControlPolicy::Off,
            saved_rosetta: true,
            pass_trouble: None,
            retired_switch_note: None,
            updated: 0,
        }
    }

    /// Snapshot the exact state the Packages page paints.
    pub(crate) fn projection(&self) -> PackagesProjection {
        self.projection_at(std::time::SystemTime::now())
    }

    /// [`Self::projection`] with the clock injected: every time on the page is said relative to it, on the local clock.
    pub(crate) fn projection_at(&self, now: std::time::SystemTime) -> PackagesProjection {
        let report = &self.report;
        let actions_enabled = self.observed
            && report.available
            && report.manager_enabled
            && report.collection_error.is_none()
            && !self.inflight;
        let attention = attention_items(
            &report.programs,
            report.declined,
            self.pass_trouble.as_ref(),
            report.cc_prereq.as_ref(),
        );
        let attention_line = attention_headline(&attention);
        let unserved = !report.declined
            && report
                .programs
                .iter()
                .any(|row| atpkg::state::is_unserved_toolset(&row.state));
        let headline = if !self.observed {
            "Reading package status…".to_string()
        } else if let Some(busy) = self.busy {
            match busy {
                PackagesBusy::Check => "Checking for package updates…".to_string(),
                PackagesBusy::Install => "Installing ALab tools…".to_string(),
                PackagesBusy::Uninstall => "Removing ALab tools…".to_string(),
                PackagesBusy::MachineApply => "Applying the machine settings…".to_string(),
            }
        } else if let Some(command) = self.last_command.as_ref() {
            command.headline().to_string()
        } else if !report.available {
            "Package tool missing".to_string()
        } else if !report.manager_enabled {
            "Package updates are off in this build".to_string()
        } else if let Some(line) = attention_line.clone() {
            line
        } else if report.ignored_prefix.is_some() {
            IGNORED_PREFIX_HEADLINE.to_string()
        } else if unserved {
            UNSERVED_HEADLINE.to_string()
        } else if report.collection_error.is_some() {
            "Some package details couldn\u{2019}t be read".to_string()
        } else if report.declined {
            // Removed on purpose (`aterm pkg uninstall --all`, Remove ALab Tools): what is
            // left in the record is stale, and "up to date" would be about nothing.
            "ALab tools aren\u{2019}t installed".to_string()
        } else if report.recorded
            && report
                .programs
                .iter()
                .any(|row| row.update.behind(&row.kind))
        {
            // Never "up to date" over a row that names a newer build (the row itself
            // stops saying so too, [`ProgramUpdate::words`]).
            "Updates are available".to_string()
        } else if report.recorded {
            "ALab tools are up to date".to_string()
        } else {
            "Not checked yet".to_string()
        };
        let now_unix = now
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX));
        let offset = report.clock;
        let recorded_detail = |say_current: bool| {
            let mut parts: Vec<String> = Vec::new();
            let outcome = report.outcome.trim();
            if outcome_is_plainly_current(outcome) {
                if say_current {
                    parts.push("up to date".to_string());
                }
            } else if !outcome.is_empty() {
                parts.push(outcome.to_string());
            }
            let stamp = if report.last_success_at.is_empty() {
                &report.updated_at
            } else {
                &report.last_success_at
            };
            if !stamp.is_empty() {
                parts.push(match aterm_update_core::pkg_check::rfc3339_to_unix(stamp) {
                    Some(at) => format!("checked {}", when_words(at, now_unix, offset)),
                    None => format!("checked {stamp}"),
                });
            }
            (!parts.is_empty()).then(|| parts.join(FACT_SEPARATOR))
        };
        let command_feedback = match (
            self.last_command.as_ref(),
            self.machine.last_verdict.as_deref(),
        ) {
            (
                Some(PackagesCommandOutcome::Succeeded {
                    operation: PackagesBusy::MachineApply,
                }),
                Some(verdict),
            ) => Some(format!("Machine settings: {verdict}")),
            (Some(command), _) => Some(command.feedback()),
            (None, _) => None,
        };
        let mut detail = if !self.observed {
            None
        } else if let Some(busy) = self.busy {
            // While the verb runs the card says where its progress is, never "Click Check &
            // Update Now" under a disabled button. The machine apply (a local edit well
            // under a second) has no band row: its headline says it all.
            (busy != PackagesBusy::MachineApply).then(|| BUSY_DETAIL.to_string())
        } else if let Some(PackagesCommandOutcome::Succeeded { operation }) =
            self.last_command.as_ref().filter(|c| {
                !matches!(
                    c,
                    PackagesCommandOutcome::Succeeded {
                        operation: PackagesBusy::MachineApply
                    }
                )
            })
        {
            let mut parts: Vec<String> = Vec::new();
            if matches!(operation, PackagesBusy::Check | PackagesBusy::Install) && self.updated > 0
            {
                parts.push(match self.updated {
                    1 => "1 program updated".to_string(),
                    n => format!("{n} programs updated"),
                });
            }
            parts.extend(attention_line.clone());
            parts.extend(recorded_detail(false));
            (!parts.is_empty()).then(|| parts.join(FACT_SEPARATOR))
        } else if let Some(command) = self.last_command.as_ref() {
            let mut detail = command_feedback
                .clone()
                .unwrap_or_else(|| command.feedback());
            if let Some(line) = attention_line.as_deref() {
                detail.push_str(FACT_SEPARATOR);
                detail.push_str(line);
            }
            match command {
                PackagesCommandOutcome::Succeeded { .. } => {
                    if let Some(recorded) = recorded_detail(false) {
                        detail.push_str(FACT_SEPARATOR);
                        detail.push_str(&recorded);
                    }
                }
                PackagesCommandOutcome::Failed { .. } => {
                    if let Some(recorded) = recorded_detail(true) {
                        detail.push_str(FACT_SEPARATOR);
                        detail.push_str("Before this attempt: ");
                        detail.push_str(&recorded);
                    }
                }
            }
            Some(detail)
        } else if !report.available {
            Some(PACKAGE_TOOL_MISSING_DETAIL.to_string())
        } else if !report.manager_enabled {
            // An inert manager's one cause is an unpinned build; `aterm pkg doctor` names
            // the key.
            Some("This build can\u{2019}t install packages.".to_string())
        } else if report.recorded {
            recorded_detail(false).map(|recorded| crate::update_words::capitalize_first(&recorded))
        } else {
            Some("Click Check & Update Now to check.".to_string())
        };
        if self.observed
            && self.last_command.is_none()
            && let Some(trouble) = self.pass_trouble.as_ref()
        {
            let cause = trouble.cause.trim();
            let last = format!(
                "Last check: {}",
                cause.strip_prefix("atpkg: ").unwrap_or(cause)
            );
            detail = Some(match detail {
                Some(rest) => format!("{last}{FACT_SEPARATOR}{rest}"),
                None => last,
            });
        }
        if self.observed
            && let Some(note) = report.ignored_prefix.as_deref()
        {
            match detail.as_mut() {
                Some(detail) => {
                    detail.push_str(FACT_SEPARATOR);
                    detail.push_str(note);
                }
                None => detail = Some(note.to_string()),
            }
        }
        if self.observed
            && let Some(line) = report
                .cc_prereq
                .as_ref()
                .map(atpkg::prereq::CcVerdict::line)
        {
            match detail.as_mut() {
                Some(detail) => {
                    detail.push_str(FACT_SEPARATOR);
                    detail.push_str(&line);
                }
                None => detail = Some(line),
            }
        }
        if self.observed
            && let Some(error) = report.collection_error.as_deref()
        {
            let warning = format!("Package metadata warning: {error}");
            match detail.as_mut() {
                Some(detail) => {
                    detail.push_str(FACT_SEPARATOR);
                    detail.push_str(&warning);
                }
                None => detail = Some(warning),
            }
        }
        let still_works = if report.available && report.manager_enabled {
            "; Check & Update Now still works"
        } else {
            ""
        };
        let loop_status = if self.tool_observed && !report.available {
            "Unavailable in this copy of aterm".to_string()
        } else if !self.loop_running {
            format!("Not running in this window{still_works}")
        } else if !self.master_enabled && self.retired_switch_note.is_some() {
            format!(
                "Off — [packages] auto_update = false holds it off (turning on Keep ALab tools \
                 up to date removes it){still_works}"
            )
        } else if !self.master_enabled {
            format!("Off — nothing updates by itself{still_works}")
        } else {
            "On — updates arrive in the background, without interrupting".to_string()
        };
        let programs = report
            .programs
            .iter()
            .map(|row| {
                let mut row = row.clone();
                row.words = row.update.words(&row.kind, &row.state, now_unix, offset);
                row
            })
            .collect();
        let activity = activity_lines(&report.activity, now_unix, offset);
        let activity_capped = report.activity.len() >= atpkg::packages_log::ACTIVITY_EVENTS;
        let machine_idle = self.tool_observed && report.available && !self.inflight;
        let machine =
            self.machine_projection(machine_idle, self.tool_observed && !report.available, now);
        PackagesProjection {
            machine,
            observed: self.observed,
            tool_observed: self.tool_observed,
            available: report.available,
            manager_enabled: report.manager_enabled,
            root_fingerprint: report.root_fingerprint.clone(),
            recorded: report.recorded,
            updated_at: report.updated_at.clone(),
            outcome: report.outcome.clone(),
            index_source: report.index_source.clone(),
            programs,
            activity,
            activity_capped,
            log_path: report.log_path.clone(),
            collection_error: report.collection_error.clone(),
            busy: self.busy,
            refreshing: self.inflight && self.busy.is_none(),
            master_enabled: self.master_enabled,
            auto_install: self.auto_install,
            loop_running: self.loop_running,
            loop_status,
            retired_switch_note: self.retired_switch_note.clone(),
            actions_enabled,
            headline,
            detail,
            attention,
        }
    }

    /// The "This Mac" status's words, derived here so pixels, accessibility and `aterm ctl`
    /// read one truth.
    fn machine_projection(
        &self,
        idle: bool,
        tool_missing: bool,
        now: std::time::SystemTime,
    ) -> MachineProjection {
        use atpkg::config::UniversalControlPolicy;
        use atpkg::machine::{HomePosture, MachineState, RosettaPosture, UcPosture};
        let posture = &self.machine;
        let supported = cfg!(target_os = "macos");
        let state = posture.state.as_ref();
        let prior = if posture.read_error.is_some() && state.is_some() {
            " (as last read)"
        } else {
            ""
        };
        let rosetta = state.and_then(|s| match (s.rosetta, self.saved_rosetta) {
            (RosettaPosture::Missing, true) => Some(format!(
                "Rosetta 2 isn\u{2019}t installed, so Intel apps and tools can\u{2019}t run on \
                 this Mac{prior}."
            )),
            (RosettaPosture::Missing, false) => Some(format!(
                "Rosetta 2 isn\u{2019}t installed, and aterm leaves it so{prior}."
            )),
            (RosettaPosture::Unknown, _) => Some(format!(
                "aterm couldn\u{2019}t check Rosetta 2 on this Mac{prior}."
            )),
            (RosettaPosture::Present | RosettaPosture::NotApplicable, _) => None,
        });
        let universal_control = match state {
            None if posture.read_error.is_some() => {
                "aterm couldn\u{2019}t check Universal Control on this Mac.".to_string()
            }
            None => "Universal Control hasn\u{2019}t been checked yet.".to_string(),
            Some(s) => {
                let words = match s.universal_control {
                    UcPosture::Disabled => "Universal Control is off on this Mac",
                    UcPosture::Default => {
                        "Universal Control is on: the pointer can move to other Macs and iPads"
                    }
                    UcPosture::Partial => "Universal Control is partly off on this Mac",
                    UcPosture::Unknown => {
                        "aterm couldn\u{2019}t read Universal Control on this Mac"
                    }
                };
                format!("{words}{prior}.")
            }
        };
        let last_change = posture.last_change.as_ref().map(|(items, at)| {
            let age_ms = now
                .duration_since(*at)
                .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
                .unwrap_or(0);
            let said = items
                .split(';')
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .map(|item| crate::toolchain_words::machine_setting_words(item).0)
                .collect::<Vec<_>>()
                .join("; ");
            format!(
                "Last change: {said} · {}",
                crate::session_chrome::relative_age(age_ms)
            )
        });
        let as_saved = |s: &MachineState| MachineState {
            policy: self.saved_universal_control,
            rosetta_wanted: self.saved_rosetta,
            ..s.clone()
        };
        let reason = if let Some(error) = posture.read_error.as_deref() {
            Some(format!(
                "Couldn\u{2019}t read this Mac\u{2019}s settings: {error}"
            ))
        } else if state.is_some_and(|s| s.config_unreadable) {
            Some(
                "Not applied: aterm.toml has an error, so this setting can\u{2019}t be read. \
                 Fix the file and aterm applies it at the next check."
                    .to_string(),
            )
        } else {
            let wants_change = state.is_some_and(|s| {
                MachineState {
                    home: HomePosture::Account,
                    ..as_saved(s)
                }
                .next()
            });
            match state.map(|s| s.home).filter(|_| wants_change) {
                Some(HomePosture::Mismatch) => Some(
                    "Not applied: this window uses a different home folder than your \
                     account, so aterm won\u{2019}t change this Mac\u{2019}s settings from it."
                        .to_string(),
                ),
                Some(HomePosture::Unresolved) => Some(
                    "Not applied: aterm couldn\u{2019}t find your account\u{2019}s home folder."
                        .to_string(),
                ),
                Some(HomePosture::Account) | None => None,
            }
        };
        let next = state.is_some_and(|s| as_saved(s).next());
        let uc_next = state.is_some_and(|s| as_saved(s).universal_control_next());
        let rosetta_next = state.is_some_and(|s| as_saved(s).rosetta_next());
        let off_when = crate::prefs::universal_control_timing(
            UniversalControlPolicy::Off,
            self.master_enabled,
        )
        .map_or("", crate::prefs::ApplicationTiming::clause);
        let saved = state
            .filter(|s| s.policy != self.saved_universal_control)
            .and_then(|s| match self.saved_universal_control {
                UniversalControlPolicy::Off if uc_next && reason.is_none() => Some(format!(
                    "Saved: aterm turns Universal Control off {off_when}. Apply Now does it now."
                )),
                UniversalControlPolicy::Off if s.universal_control == UcPosture::Disabled => None,
                UniversalControlPolicy::Off => Some(format!(
                    "Saved: aterm turns Universal Control off {off_when}."
                )),
                UniversalControlPolicy::Leave => {
                    Some("Saved: aterm leaves Universal Control as macOS has it.".to_string())
                }
            });
        let apply_enabled = idle
            && supported
            && posture.observed
            && !posture.refreshing
            && !posture.awaiting_record
            && reason.is_none()
            && next;
        let nothing_to_apply = posture.observed && reason.is_none() && !next;
        let next = if next && reason.is_none() {
            machine_apply_would(uc_next, rosetta_next).to_string()
        } else if nothing_to_apply {
            MACHINE_NOTHING_TO_APPLY.to_string()
        } else {
            String::new()
        };
        MachineProjection {
            supported,
            tool_missing,
            observed: posture.observed,
            refreshing: posture.refreshing || posture.awaiting_record,
            universal_control,
            rosetta,
            last_change,
            last_verdict: posture.last_verdict.clone(),
            reason,
            saved,
            next,
            apply_enabled,
            nothing_to_apply,
        }
    }
}

/// What Apply Now would do, said under the measured state beside the button.
pub(crate) const MACHINE_APPLY_WOULD: &str = "Apply Now turns Universal Control off for this Mac.";
/// What Apply Now would do when only Rosetta 2 is left.
pub(crate) const MACHINE_APPLY_WOULD_ROSETTA: &str = "Apply Now installs Rosetta 2 on this Mac.";
/// What Apply Now would do when both are left.
pub(crate) const MACHINE_APPLY_WOULD_BOTH: &str =
    "Apply Now turns Universal Control off and installs Rosetta 2 on this Mac.";

/// The Apply Now sentence for what is left: Universal Control, Rosetta 2, or both.
const fn machine_apply_would(universal_control: bool, rosetta: bool) -> &'static str {
    match (universal_control, rosetta) {
        (true, true) => MACHINE_APPLY_WOULD_BOTH,
        (false, true) => MACHINE_APPLY_WOULD_ROSETTA,
        _ => MACHINE_APPLY_WOULD,
    }
}
/// The verdict when the Mac already matches the saved choice.
pub(crate) const MACHINE_NOTHING_TO_APPLY: &str =
    "Nothing to apply: Universal Control is already as you chose.";

/// Settings ▸ Security's "This Mac" status, as words (the [`PackagesProjection`] slice for the `[machine]` host settings).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MachineProjection {
    /// These are macOS host settings; elsewhere the status is absent.
    pub(crate) supported: bool,
    /// This copy has no package tool, so the Mac can't be read or changed from it: the status says so rather than show a failed read.
    pub(crate) tool_missing: bool,
    /// A machine read has completed at least once.
    pub(crate) observed: bool,
    /// The machine read worker is running.
    pub(crate) refreshing: bool,
    /// The measured posture, as a sentence.
    pub(crate) universal_control: String,
    /// Rosetta 2, as a sentence, when there is something to say: missing, or unread.
    pub(crate) rosetta: Option<String>,
    /// `Last change: … · <age>`, when this launch changed something.
    pub(crate) last_change: Option<String>,
    /// The last `atpkg machine apply` verdict this launch, when there is one.
    pub(crate) last_verdict: Option<String>,
    /// Why an apply would do nothing here (home mismatch/unresolved) or why the state could not be read.
    pub(crate) reason: Option<String>,
    /// A `[machine]` switch saved after the last read, and what will use it.
    pub(crate) saved: Option<String>,
    /// What Apply Now would do ([`MACHINE_APPLY_WOULD`]), or the "nothing to apply" sentence.
    pub(crate) next: String,
    /// The Apply Now button: page idle, macOS, observed, not refreshing, and the verdict says something is left to do.
    pub(crate) apply_enabled: bool,
    /// Observed, readable, and the verdict is "nothing to apply": the status offers no button, and a press from the palette says so.
    pub(crate) nothing_to_apply: bool,
}

/// Owned, structured read projection shared with the native Settings `/packages` route (the [`crate::update_screen::UpdateProjection`] analogue).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PackagesProjection {
    /// The `[machine]` host settings slice (Settings ▸ Security's "This Mac" status).
    pub(crate) machine: MachineProjection,
    /// A status read has landed (the headline, Activity and program rows wait on it).
    pub(crate) observed: bool,
    /// What the package tool is, is known: package admission reads this, never [`Self::observed`] ([`PackagesService::note_tool`]).
    pub(crate) tool_observed: bool,
    pub(crate) available: bool,
    pub(crate) manager_enabled: bool,
    pub(crate) root_fingerprint: String,
    pub(crate) recorded: bool,
    pub(crate) updated_at: String,
    pub(crate) outcome: String,
    pub(crate) index_source: String,
    pub(crate) programs: Vec<PackagesProgramRow>,
    pub(crate) collection_error: Option<String>,
    pub(crate) busy: Option<PackagesBusy>,
    /// A silent status-refresh worker is inflight (no `busy` label).
    pub(crate) refreshing: bool,
    /// `[packages] enabled` (Keep ALab tools up to date), resolved with the retired `auto_update` folded in.
    pub(crate) master_enabled: bool,
    /// `[packages] auto_install` — the one install consent, resolved.
    pub(crate) auto_install: bool,
    pub(crate) loop_running: bool,
    /// What is actually running now, plus any saved-vs-live mismatch.
    pub(crate) loop_status: String,
    /// Wrapped sentence for a retired `[packages] auto_update = false` (`atpkg::config::auto_update_note`).
    pub(crate) retired_switch_note: Option<String>,
    /// Both action buttons: observed, available, enabled, read without error, and idle.
    pub(crate) actions_enabled: bool,
    pub(crate) headline: String,
    pub(crate) detail: Option<String>,
    /// What failed, named ([`attention_items`]).
    pub(crate) attention: Vec<String>,
    /// The package log's newest events as the Activity card reads them, newest first ([`activity_lines`]).
    pub(crate) activity: Vec<ActivityLine>,
    /// Whether the tail is full ([`atpkg::packages_log::ACTIVITY_EVENTS`] events).
    pub(crate) activity_capped: bool,
    /// The package log, for "Open Log" (`None`: no log directory resolves).
    pub(crate) log_path: Option<std::path::PathBuf>,
}

/// One line of Settings ▸ Packages' Activity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ActivityLine {
    /// When, relative and local ([`when_words`]).
    pub(crate) when: String,
    /// What happened.
    pub(crate) text: String,
    /// A failure or a refusal — the kind of line a person scans for.
    pub(crate) trouble: bool,
}

/// The Activity list: the package log's events ([`atpkg::packages_log`]), newest first, as sentences.
pub(crate) fn activity_lines(
    entries: &[atpkg::packages_log::Entry],
    now: i64,
    clock: LocalClock,
) -> Vec<ActivityLine> {
    activity_sentences(entries)
        .into_iter()
        .map(|(entry, text, trouble)| ActivityLine {
            when: entry.at.map_or_else(
                || "at an unknown time".to_string(),
                |at| when_words(at, now, clock),
            ),
            text,
            trouble,
        })
        .collect()
}

/// Whether the Activity says anything of `entry`'s kind ([`activity_sentences`]).
pub(crate) fn shown_in_activity(entry: &atpkg::packages_log::Entry) -> bool {
    use atpkg::packages_log::kind;
    [kind::PROGRAM, kind::PASS_END, kind::PASS_START, kind::SEAM].contains(&entry.kind.as_str())
}

/// [`activity_lines`]' sentences with the event each says, before any time is put to them.
pub(crate) fn activity_sentences(
    entries: &[atpkg::packages_log::Entry],
) -> Vec<(&atpkg::packages_log::Entry, String, bool)> {
    use atpkg::packages_log::kind;
    let mut ended: Vec<u32> = Vec::new();
    let mut out = Vec::new();
    for entry in entries {
        let get = |key: &str| entry.get(key).unwrap_or("");
        let (text, trouble) = match entry.kind.as_str() {
            kind::PROGRAM => program_activity(
                get("program"),
                entry.get("from"),
                entry.get("to"),
                get("source"),
                get("result"),
                get("reason"),
            ),
            kind::PASS_END => {
                ended.push(entry.pid);
                let exit: u8 = get("exit").parse().unwrap_or(1);
                const OFFLINE: u8 = aterm_update_core::pkg_check::PASS_OFFLINE_EXIT;
                const CONTENDED: u8 = atpkg::lock::CONTENDED_EXIT;
                let mut text = format!(
                    "{} \u{b7} {}",
                    pass_words(get("verb")),
                    lane_words(get("lane"))
                );
                let said = match exit {
                    0 => "finished",
                    OFFLINE => "offline — tried again soon",
                    CONTENDED => "did not run",
                    1 => "failed",
                    _ => "ended",
                };
                text.push_str(crate::native_ui::FACT_SEPARATOR);
                text.push_str(said);
                let outcome = get("outcome");
                if !outcome.is_empty() && !(exit == CONTENDED && outcome.starts_with("did not run"))
                {
                    text.push_str(": ");
                    text.push_str(outcome);
                } else if !matches!(exit, 0 | 1 | OFFLINE | CONTENDED) {
                    text.push_str(&format!(" (exit {exit})"));
                }
                if let Ok(secs) = get("secs").parse::<u64>()
                    && secs > 0
                {
                    text.push_str(&format!("{}{secs} s", crate::native_ui::FACT_SEPARATOR));
                }
                (text, !matches!(exit, 0 | OFFLINE | CONTENDED))
            }
            kind::PASS_START if !ended.contains(&entry.pid) => (
                format!(
                    "{} \u{b7} {} \u{b7} started, no end recorded yet",
                    pass_words(get("verb")),
                    lane_words(get("lane"))
                ),
                false,
            ),
            kind::SEAM => (
                seam_activity(get("seam"), get("from"), get("why"), get("undo")),
                false,
            ),
            _ => continue,
        };
        out.push((entry, text, trouble));
    }
    out
}

/// A program transition as the Activity says it.
fn program_activity(
    program: &str,
    from: Option<&str>,
    to: Option<&str>,
    source: &str,
    result: &str,
    reason: &str,
) -> (String, bool) {
    let name = package_program_display_name(program);
    let with_source = |mut text: String| {
        if !source.is_empty() {
            text.push_str(crate::native_ui::FACT_SEPARATOR);
            text.push_str(source);
        }
        text
    };
    match (result, from, to) {
        ("installed", _, Some(to)) => (with_source(format!("{name} {to} installed")), false),
        ("updated" | "rolled back", Some(from), Some(to)) => (
            with_source(format!("{name} {from} \u{2192} {to} \u{b7} {result}")),
            false,
        ),
        ("removed", Some(from), _) => (format!("{name} {from} removed"), false),
        ("current", _, _) => (with_source(format!("{name} is current again")), false),
        _ => {
            let why = [
                "error: ",
                "aborted: ",
                "tombstoned: ",
                "rejected: ",
                "deferred: ",
                "held: ",
                "unavailable: ",
                "blocked: ",
            ]
            .iter()
            .find_map(|p| reason.strip_prefix(p))
            .unwrap_or(reason)
            .trim();
            let text = if why.is_empty() {
                format!("{name} {result}")
            } else if why
                .strip_prefix(program)
                .is_some_and(|rest| rest.starts_with(' '))
            {
                why.to_string()
            } else if why.starts_with(result) {
                format!("{name} {why}")
            } else {
                format!("{name} {result}: {why}")
            };
            (text, matches!(result, "failed" | "refused" | "disabled"))
        }
    }
}

/// A rustup seam that replaced a stale link, as the Activity says it: the one change a pass
/// makes to `~/.rustup` that nobody laid there for it (atpkg re-points rustup's `trust` away
/// from an older live build tree), so the sentence names what it named, why, and the
/// command that puts it back.
fn seam_activity(seam: &str, from: &str, why: &str, undo: &str) -> String {
    let name = seam.strip_prefix("rustup:").unwrap_or(seam);
    let mut text = format!("rustup `{name}` re-pointed at the managed toolchain");
    if !from.is_empty() {
        text.push_str(" \u{b7} was ");
        text.push_str(from);
        if !why.is_empty() {
            text.push_str(", ");
            text.push_str(why);
        }
    }
    if !undo.is_empty() {
        text.push_str(" \u{b7} ");
        text.push_str(undo);
    }
    text
}

/// A pass's verb as the Activity names it.
fn pass_words(verb: &str) -> String {
    let mut words = verb.split(' ');
    match (words.next().unwrap_or(""), words.next()) {
        ("update", None) => "Update check".to_string(),
        ("update", Some(program)) => format!("{} update", package_program_display_name(program)),
        ("seed", _) => "Launch check".to_string(),
        ("install", Some("--default-set")) => "ALab tools install".to_string(),
        ("install", Some(program)) => format!("Install {program}"),
        ("uninstall", Some("--all")) => "ALab tools removal".to_string(),
        (other, rest) => {
            let mut text = crate::update_words::capitalize_first(other);
            if let Some(rest) = rest {
                text.push(' ');
                text.push_str(rest);
            }
            text
        }
    }
}

/// Which lane ran a pass, as the Activity names it (atpkg's lane words: `window`,
/// `session`, `head watch`, `agent update`, `typed`).
fn lane_words(lane: &str) -> &str {
    match lane {
        "window" => "aterm window",
        "session" => "terminal session",
        "head watch" => "release watch",
        "agent update" => "you asked",
        "typed" => "command line",
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refresh(report: PackagesStatusReport) -> PackagesWorkerCompletion {
        PackagesWorkerCompletion::refresh(report)
    }

    fn succeeded(
        report: PackagesStatusReport,
        operation: PackagesBusy,
    ) -> PackagesWorkerCompletion {
        PackagesWorkerCompletion::command(report, PackagesCommandOutcome::Succeeded { operation })
    }

    fn status(outcome: &str) -> atpkg::Status {
        let mut programs = std::collections::BTreeMap::new();
        programs.insert(
            "ay".to_string(),
            atpkg::ProgramStatus {
                installed_build: Some(1971),
                state: "active".to_string(),
                tree_root: String::new(),
            },
        );
        atpkg::Status {
            schema: 1,
            updated_at: "2026-07-21T00:00:00Z".to_string(),
            enabled: true,
            index_source: "alabsystems/aterm".to_string(),
            outcome: outcome.to_string(),
            seams: Vec::new(),
            last_success_at: String::new(),
            last_index_build: 0,
            index_build_changed_at: String::new(),
            last_pass: String::new(),
            last_pass_at: String::new(),
            last_pass_attempted_index_build: 0,
            last_pass_attempted_at: String::new(),
            pass_seq: 0,
            stale_index_only_pass_seq: 0,
            programs,
            refused_shims: Default::default(),
            extra: Default::default(),
        }
    }

    /// One worker at a time; a stale sequence is reducer-inert, and so is a
    /// current one whose completion kind/operation does not match the
    /// reservation; a matching completion clears busy, stores the report, and
    /// bumps the revision.
    #[test]
    fn service_serializes_workers_and_rejects_stale_completions() {
        let mut service = PackagesService::new();
        let base = service.revision();
        let seq = service
            .begin(Some(PackagesBusy::Check))
            .expect("idle → begin");
        assert!(service.revision() > base, "busy flip fans out");
        assert_eq!(service.busy(), Some(PackagesBusy::Check));
        assert!(
            service.begin(Some(PackagesBusy::Install)).is_none(),
            "second verb joins, never overlaps"
        );

        let report =
            PackagesStatusReport::from_parts(true, true, "fp".into(), Some(&status("ok")), &[]);
        assert!(
            !service.finish(seq + 1, succeeded(report.clone(), PackagesBusy::Check)),
            "a superseded worker cannot import facts"
        );
        assert!(!service.finish(seq, refresh(report.clone())), "kind");
        assert!(
            !service.finish(seq, succeeded(report.clone(), PackagesBusy::Install)),
            "operation"
        );
        assert_eq!(
            service.busy(),
            Some(PackagesBusy::Check),
            "a stale or mismatched finish is inert"
        );
        let before = service.revision();
        assert!(service.finish(seq, succeeded(report, PackagesBusy::Check)));
        assert_eq!(service.busy(), None);
        assert!(service.revision() > before);
        let projection = service.state(true, true, true).projection();
        assert!(projection.recorded);
        assert!(projection.actions_enabled);
    }

    /// A spawn-failure abort releases the reservation without importing facts: an
    /// unobserved surface stays unobserved, an observed one keeps its report; a stale
    /// abort is inert.
    #[test]
    fn abort_releases_the_reservation_without_fabricating_a_report() {
        let mut service = PackagesService::new();
        let seq = service.begin(None).expect("idle → begin");
        let before = service.revision();
        assert!(service.abort(seq), "current abort releases");
        assert!(service.revision() > before, "the un-busy flip fans out");
        assert_eq!(service.busy(), None);
        assert!(
            !service.state(true, true, true).projection().observed,
            "never-observed stays honestly unobserved after an abort"
        );

        let seq = service.begin(Some(PackagesBusy::Check)).unwrap();
        let report =
            PackagesStatusReport::from_parts(true, true, "fp".into(), Some(&status("ok")), &[]);
        assert!(service.finish(seq, succeeded(report, PackagesBusy::Check)));
        let seq = service.begin(Some(PackagesBusy::Install)).unwrap();
        assert!(!service.abort(seq + 1), "stale abort is inert");
        assert_eq!(service.busy(), Some(PackagesBusy::Install));
        assert!(service.abort(seq));
        let projection = service.state(true, true, true).projection();
        assert!(projection.recorded, "the prior report's facts survive");
        assert_eq!(projection.programs.len(), 1);
        assert!(projection.actions_enabled);
    }

    /// The posture ladder: unobserved → unavailable → recorded → busy, each with its own
    /// action gating (the inert manager's gate is `machine_apply_ignores_the_manager_gate`).
    #[test]
    fn projection_headlines_track_the_honest_manager_posture() {
        let unobserved = PackagesState::unobserved().projection();
        assert!(
            !unobserved.observed && !unobserved.actions_enabled,
            "nothing is claimed before observation"
        );

        let mut service = PackagesService::new();
        let seq = service.begin(None).unwrap();
        assert!(service.finish(
            seq,
            refresh(PackagesStatusReport::from_parts(
                false,
                false,
                String::new(),
                None,
                &[],
            )),
        ));
        let unavailable = service.state(true, true, true).projection();
        assert_eq!(
            unavailable.detail.as_deref(),
            Some(super::PACKAGE_TOOL_MISSING_DETAIL)
        );
        assert!(!unavailable.actions_enabled);

        let seq = service.begin(None).unwrap();
        assert!(service.finish(
            seq,
            refresh(PackagesStatusReport::from_parts(
                true,
                true,
                "fp".into(),
                Some(&status("up to date")),
                &[]
            )),
        ));
        let live = service.state(true, true, true).projection();
        assert!(live.actions_enabled);

        let _ = service.begin(Some(PackagesBusy::Install)).unwrap();
        let busy = service.state(true, true, true).projection();
        assert_eq!(busy.busy, Some(PackagesBusy::Install));
        assert!(!busy.actions_enabled);
    }

    /// A report whose program rows are `rows` (name, state), as a pass wrote them.
    fn report_with(rows: &[(&str, String)]) -> PackagesStatusReport {
        let mut status = status("up to date (index build 44)");
        status.programs = rows
            .iter()
            .map(|(name, state)| {
                (
                    (*name).to_string(),
                    atpkg::ProgramStatus {
                        installed_build: Some(
                            atpkg::state::managed_pin(state).map_or(1, |(build, _)| build),
                        ),
                        state: state.clone(),
                        tree_root: String::new(),
                    },
                )
            })
            .collect();
        PackagesStatusReport::from_parts(true, true, "fp".into(), Some(&status), &[])
    }

    /// One silent refresh that observes `report`.
    fn observe(service: &mut PackagesService, report: PackagesStatusReport) {
        let sequence = service.begin(None).expect("idle");
        assert!(service.finish(sequence, refresh(report)));
    }

    fn projection(service: &PackagesService) -> PackagesProjection {
        service.state(true, true, true).projection()
    }

    /// What the badge counts, and nothing else: the faults `atpkg doctor` lists (its own
    /// predicate) and a vendor build whose release channel has not been reached for a day.
    #[test]
    fn attention_is_the_doctors_faults_and_an_unreached_vendor() {
        let item = |name: &str, state: &str| attention_item(name, state);
        let unreached = atpkg::state::vendor_kept(
            "2.1.280",
            "Anthropic's release channel not reached since 2026-09-20",
        );
        for (name, fault) in [
            ("codex", "error: codex 0.157.0 refused: x"),
            ("codex", "error: stage: disk full"),
            ("trust", "aborted: stage"),
            ("claude", "tombstoned: claude 2.1.279 was yanked"),
            (
                "*toolset*",
                "unavailable: every published build was refused",
            ),
            ("*index*", "error: index unreachable"),
            ("claude", unreached.as_str()),
        ] {
            assert!(item(name, fault).is_some(), "{name}: {fault}");
        }
        let p = std::path::Path::new;
        for benign in [
            atpkg::state::managed(6808, 41),
            atpkg::state::vendor_managed("2.1.280", "Anthropic"),
            atpkg::state::vendor_kept("2.1.280", "held by local pin"),
            atpkg::state::vendor_source("2.1.280", "Anthropic"),
            atpkg::state::shadowed(1971, p("/Users//dev/.local/bin/ay")),
            atpkg::state::system(p("/opt/homebrew/bin/gh"), None),
            atpkg::state::AGENT_INSTALLING.to_string(),
            "extra — not installed (opt in: aterm pkg install gemini)".to_string(),
            "needs admin — run: aterm pkg install clt".to_string(),
            "unavailable on x86_64-apple-darwin: no build is published for this target".to_string(),
            "blocked by clt: needs admin — run: aterm pkg install clt".to_string(),
            "held: pinned build 9 is not published for x86_64-apple-darwin; staying on build 8"
                .to_string(),
            "active".to_string(),
            "linked".to_string(),
        ] {
            assert_eq!(item("x", &benign), None, "{benign}");
        }
        assert_eq!(item("--help", "error: not a program"), None, "a stray row");
        for unserved in [
            atpkg::state::TOOLSET_UNSERVED,
            atpkg::state::TOOLSET_UNSERVED_BLOCKED,
        ] {
            assert_eq!(item("*toolset*", unserved), None, "{unserved}");
        }
    }

    /// The store removed on purpose wears no badge: a declined store's stale fault rows
    /// count for nothing, while a remembered pass trouble still counts.
    #[test]
    fn a_declined_store_wears_no_badge() {
        let mut declined = report_with(&[
            (
                "ay",
                "error: ay stage failed: no space left on device".to_string(),
            ),
            ("--help", "error: stray".to_string()),
        ]);
        declined.declined = true;
        let mut service = PackagesService::new();
        observe(&mut service, declined.clone());
        let p = projection(&service);
        assert!(
            p.attention.is_empty(),
            "declined: no badge: {:?}",
            p.attention
        );
        assert!(service.note_pass_trouble(
            PassTroubleKind::Failed,
            "atpkg: prefix is not writable".into(),
            at(100),
        ));
        assert_eq!(
            projection(&service).attention.len(),
            1,
            "the pass's own trouble is not a declined row"
        );
    }

    /// `secs` past the epoch, as the clock a trouble is noted at.
    fn at(secs: u64) -> std::time::SystemTime {
        std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs)
    }

    /// A report whose record says a pass completed at `stamp` (RFC 3339).
    fn completed_at(stamp: &str) -> PackagesStatusReport {
        let mut report = report_with(&[("ay", atpkg::state::managed(1971, 44))]);
        report.last_success_at = stamp.to_string();
        report
    }

    /// A C toolchain that blocks builds is named on the page
    /// (`DESIGN-clt-prerequisite-2026-08-31.md` §6): the badge counts it, the headline names
    /// it with its one act, the detail carries doctor's whole sentence, and it is no program
    /// row.
    #[test]
    fn a_missing_c_toolchain_is_the_badge_and_names_its_act() {
        let mut service = PackagesService::new();
        let healthy = report_with(&[("ay", atpkg::state::managed(1971, 44))]);
        observe(&mut service, healthy.clone());
        assert!(
            projection(&service).attention.is_empty(),
            "control: no badge"
        );
        let line = atpkg::prereq::CcVerdict::NoDriver.line();
        let mut blocked = healthy;
        blocked.cc_prereq = Some(atpkg::prereq::CcVerdict::NoDriver);
        observe(&mut service, blocked);
        let shown = projection(&service);
        let item = cc_prereq_item(&atpkg::prereq::CcVerdict::NoDriver);
        assert_eq!(shown.attention, vec![item.clone()], "one badge item");
        assert!(
            shown.detail.as_deref().is_some_and(|d| d.contains(&line)),
            "{:?}",
            shown.detail
        );
        assert_eq!(shown.programs.len(), 1, "no synthetic program row");

        let mut troubled = report_with(&[("ay", "error: verify failed".to_string())]);
        troubled.cc_prereq = Some(atpkg::prereq::CcVerdict::NoDriver);
        observe(&mut service, troubled);
        let shown = projection(&service);
        assert_eq!(shown.attention.len(), 2, "{:?}", shown.attention);
        assert_eq!(shown.attention[1], item, "{:?}", shown.attention);
    }

    /// The page probes where doctor does and says only an answer that blocks builds: a
    /// blocking verdict is doctor's own sentence ending in the act; an unknown and a ready
    /// compiler say nothing; no installed program, or a toolset removed on purpose, never
    /// probes.
    #[test]
    fn the_c_toolchain_is_probed_where_doctor_probes_and_said_only_when_it_blocks() {
        let installed = report_with(&[("ay", atpkg::state::managed(1971, 44))]);
        let note = cc_prereq_note(&installed, || atpkg::prereq::CcVerdict::NoDriver)
            .expect("an installed toolset with no C compiler is named");
        assert_eq!(note, atpkg::prereq::CcVerdict::NoDriver);
        let cc = std::path::PathBuf::from("/usr/bin/cc");
        let wedged = atpkg::prereq::CcVerdict::Unanswered { driver: cc.clone() };
        assert_eq!(cc_prereq_note(&installed, || wedged), None, "an unknown");
        let ready = atpkg::prereq::CcVerdict::Ready { driver: cc };
        assert_eq!(cc_prereq_note(&installed, || ready), None, "a compiler");
        let nothing = report_with(&[]);
        assert_eq!(
            cc_prereq_note(&nothing, || panic!("probed with nothing installed")),
            None
        );
        let mut declined = installed;
        declined.declined = true;
        assert_eq!(
            cc_prereq_note(&declined, || panic!("probed a declined toolset")),
            None
        );
    }

    /// A pass trouble with no row of its own (a refusal at atpkg's dispatch edge, a child
    /// that died after announcing) is the badge too: its cause leads the detail, the
    /// headline names it, and a later clean whole pass clears it.
    #[test]
    fn a_failed_pass_is_the_badge_until_a_clean_pass_clears_it() {
        let mut service = PackagesService::new();
        observe(
            &mut service,
            report_with(&[("ay", atpkg::state::managed(1971, 44))]),
        );
        assert!(projection(&service).attention.is_empty());
        let before = service.revision();
        assert!(service.note_pass_trouble(
            PassTroubleKind::Failed,
            "atpkg: prefix is not writable".into(),
            at(100),
        ));
        assert!(service.revision() > before, "the badge fans out");
        let settled = service.revision();
        assert!(!service.note_pass_trouble(
            PassTroubleKind::Failed,
            "atpkg: prefix is not writable".into(),
            at(200),
        ));
        assert_eq!(service.revision(), settled, "same words: nothing new");
        assert_eq!(projection(&service).attention.len(), 1, "the badge is up");
        assert!(service.note_pass_clean());
        assert!(!service.note_pass_clean(), "idempotent");
        let p = projection(&service);
        assert!(p.attention.is_empty(), "cleared: the badge is off");
        assert!(
            service.note_pass_trouble(
                PassTroubleKind::StoodDown,
                "an earlier toolchain pass was still running after 30 min — automatic updates \
             are off; run: aterm pkg seed"
                    .into(),
                at(100),
            )
        );
        assert_eq!(
            projection(&service).attention.len(),
            1,
            "a stand-down badges too"
        );
    }

    /// The trouble clears when a pass is known to have completed since, not only when the
    /// window's own lane runs one clean: a Settings Check or Install that succeeds clears it
    /// (a failed one, or a verb that is not a whole pass, does not), and so does a refresh
    /// whose record's `last_success_at` is later than when the trouble was noted, a
    /// terminal's `aterm pkg update` included; an earlier or unparseable stamp does not.
    #[test]
    fn a_pass_completed_since_the_trouble_clears_it() {
        let noted = at(1_790_000_000);
        let troubled = || {
            let mut service = PackagesService::new();
            observe(&mut service, completed_at(""));
            assert!(service.note_pass_trouble(
                PassTroubleKind::Failed,
                "ay stage failed: no space left on device".into(),
                noted,
            ));
            service
        };
        let badged = |service: &PackagesService| !projection(service).attention.is_empty();
        for operation in [PackagesBusy::Check, PackagesBusy::Install] {
            let mut service = troubled();
            let sequence = service.begin(Some(operation)).unwrap();
            assert!(service.finish(sequence, succeeded(completed_at(""), operation)));
            assert!(!badged(&service), "{operation:?} succeeded: cleared");
            assert_eq!(
                projection(&service).headline,
                operation.completed_headline(),
                "the verb's own words, and nothing over them"
            );
        }
        let mut service = troubled();
        let sequence = service.begin(Some(PackagesBusy::Check)).unwrap();
        assert!(service.finish(
            sequence,
            PackagesWorkerCompletion::command(
                completed_at(""),
                PackagesCommandOutcome::Failed {
                    operation: PackagesBusy::Check,
                    message: "exit 1".into(),
                },
            ),
        ));
        assert!(badged(&service), "a failed Check proves nothing");
        let mut service = troubled();
        let sequence = service.begin(Some(PackagesBusy::MachineApply)).unwrap();
        assert!(service.finish(
            sequence,
            succeeded(completed_at(""), PackagesBusy::MachineApply)
        ));
        assert!(badged(&service), "a machine apply is not a pass");
        for (stamp, clears) in [
            ("2026-09-21T14:13:21Z", true),
            ("2026-09-21T14:13:20Z", false),
            ("2026-09-20T00:00:00Z", false),
            ("not a time", false),
        ] {
            let mut service = troubled();
            observe(&mut service, completed_at(stamp));
            assert_eq!(!badged(&service), clears, "{stamp}");
        }
    }

    /// A status record carrying every canonical §17.2 state: the rows parse through
    /// `atpkg::state`'s own readers, the text is kept verbatim, and the agent programs carry
    /// their authored vendor / license / size.
    #[test]
    fn rows_parse_every_canonical_state() {
        let p = std::path::Path::new;
        let claude = atpkg::vendor_direct::Version::parse("2.1.280")
            .unwrap()
            .build_id();
        let fixture: Vec<(&str, String, Option<u64>)> = vec![
            ("trust", atpkg::state::managed(6808, 41), Some(6808)),
            (
                "ay",
                atpkg::state::shadowed(1971, p("/Users//dev/.local/bin/ay")),
                Some(1971),
            ),
            (
                "gh",
                atpkg::state::system(p("/opt/homebrew/bin/gh"), Some("2026-08-27")),
                None,
            ),
            ("codex", atpkg::state::agent_installing(), None),
            (
                "claude",
                atpkg::state::vendor_managed("2.1.280", "Anthropic"),
                Some(claude),
            ),
            (
                "vendorx",
                "extra — not installed (opt in: aterm pkg install vendorx)".to_string(),
                None,
            ),
            (
                "brew",
                "blocked by clt: needs admin — run: aterm pkg install clt".to_string(),
                None,
            ),
            (
                "emacs",
                atpkg::state::unavailable("aarch64-pc-windows-msvc", "no Windows-on-ARM build"),
                None,
            ),
            ("ny", "error: x".to_string(), None),
        ];
        let mut status = status("up to date");
        status.programs = fixture
            .iter()
            .map(|(name, state, build)| {
                (
                    (*name).to_string(),
                    atpkg::ProgramStatus {
                        installed_build: *build,
                        state: state.clone(),
                        tree_root: String::new(),
                    },
                )
            })
            .collect();
        let report = PackagesStatusReport::from_parts(true, true, "fp".into(), Some(&status), &[]);
        let row = |name: &str| {
            report
                .programs
                .iter()
                .find(|row| row.name == name)
                .unwrap_or_else(|| panic!("{name} row"))
        };
        for (name, state, _) in &fixture {
            assert_eq!(
                &row(name).state,
                state,
                "{name} keeps the canonical spelling"
            );
        }
        assert_eq!(
            row("trust").kind,
            ProgramStateKind::Managed {
                build: 6808,
                index: 41
            }
        );
        assert_eq!(
            row("ay").kind,
            ProgramStateKind::Shadowed {
                build: 1971,
                path: "/Users//dev/.local/bin/ay".into()
            }
        );
        assert_eq!(
            row("gh").kind,
            ProgramStateKind::System {
                path: "/opt/homebrew/bin/gh".into(),
                retired: Some("2026-08-27".into())
            }
        );
        assert_eq!(row("codex").kind, ProgramStateKind::AgentInstalling);
        assert_eq!(
            row("claude").kind,
            ProgramStateKind::VendorLatest {
                version: "2.1.280".into(),
                vendor: "Anthropic".into()
            }
        );
        assert_eq!(
            ProgramStateKind::parse(&atpkg::state::vendor_kept("2.1.279", "held by local pin")),
            ProgramStateKind::VendorKept {
                version: "2.1.279".into(),
                why: "held by local pin".into()
            }
        );
        assert_eq!(
            ProgramStateKind::parse(&atpkg::state::shadowed(
                claude,
                p("/Users//dev/.local/bin/claude")
            )),
            ProgramStateKind::Shadowed {
                build: claude,
                path: "/Users//dev/.local/bin/claude".into()
            }
        );
        assert_eq!(row("emacs").kind, ProgramStateKind::Unavailable);
        for retired in ["vendorx", "brew", "ny"] {
            assert_eq!(row(retired).kind, ProgramStateKind::Other, "{retired}");
        }
        for name in ["trust", "ay", "gh", "emacs", "ny", "vendorx"] {
            assert!(row(name).facts.is_none(), "{name} carries no vendor facts");
        }

        assert!(row("codex").facts.is_some(), "codex facts");
        assert!(row("claude").facts.is_some(), "claude facts");
        let odd = VendorFacts::parse("Some Tool without the grammar");
        assert_eq!(odd.vendor, "Some Tool without the grammar");
        assert_eq!(odd.license, None);
        assert_eq!(odd.size, None);
    }

    /// Dev-linked programs surface annotated — merged into an existing status
    /// row when atpkg recorded one, appended as `linked` when it never did —
    /// and the row list stays name-sorted for stable paint.
    #[test]
    fn report_merges_dev_links_into_the_program_rows() {
        let links = vec![
            (
                "ay".to_string(),
                Some(std::path::PathBuf::from("/Users//x/ay")),
            ),
            ("orc".to_string(), None),
        ];
        let report =
            PackagesStatusReport::from_parts(true, true, "fp".into(), Some(&status("ok")), &links);
        assert_eq!(report.programs.len(), 2);
        let ay = report.programs.iter().find(|row| row.name == "ay").unwrap();
        assert_eq!(ay.installed_build, Some(1971), "status row kept");
        assert!(
            ay.annotation
                .as_deref()
                .is_some_and(|a| a.contains("/Users//x/ay"))
        );
        let orc = report
            .programs
            .iter()
            .find(|row| row.name == "orc")
            .unwrap();
        assert_eq!(orc.state, "linked");
        assert_eq!(orc.installed_build, None);
        assert!(report.programs.windows(2).all(|w| w[0].name <= w[1].name));
    }

    /// Shadowed is read, not recorded: atpkg records the managed row, and the page reads a
    /// program as shadowed against the login shell's PATH it is handed.
    #[cfg(unix)]
    #[test]
    fn the_collector_reads_shadowing_against_the_path_it_is_handed() {
        use std::os::unix::fs::PermissionsExt as _;
        let root =
            std::env::temp_dir().join(format!("aterm-packages-shadow-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let layout = atpkg::Layout {
            prefix: root.join("pkg"),
        };
        let build = layout.build_dir("ay", 18);
        std::fs::create_dir_all(build.join("bin")).unwrap();
        std::fs::write(build.join("bin/ay"), b"#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(build.join("bin/ay"), std::fs::Permissions::from_mode(0o755))
            .unwrap();
        atpkg::activate::install_shims(
            &layout,
            &build,
            &["ay".to_string()],
            atpkg::activate::Aliases::Off,
        )
        .unwrap();
        atpkg::store::mark_build_ready(&build).unwrap();
        let mut programs = std::collections::BTreeMap::new();
        programs.insert(
            "ay".to_string(),
            atpkg::ProgramStatus {
                installed_build: Some(18),
                state: atpkg::state::managed(18, 41),
                tree_root: String::new(),
            },
        );
        atpkg::status::write(
            &layout,
            &atpkg::Status {
                schema: 1,
                programs,
                ..Default::default()
            },
        )
        .unwrap();
        let foreign = root.join("foreign");
        std::fs::create_dir_all(&foreign).unwrap();
        std::fs::write(foreign.join("ay"), b"#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(foreign.join("ay"), std::fs::Permissions::from_mode(0o755))
            .unwrap();
        let row = |path: &std::ffi::OsStr| {
            collect_packages_status_from_layout(true, Some(&layout), Some(path))
                .programs
                .into_iter()
                .find(|r| r.name == "ay")
                .unwrap()
        };
        let ahead = std::env::join_paths([foreign.clone(), layout.bin_dir()]).unwrap();
        assert!(
            matches!(
                row(&ahead).kind,
                ProgramStateKind::Shadowed { build: 18, .. }
            ),
            "{:?}",
            row(&ahead).state
        );
        assert_eq!(row(&ahead).update.fix, None);
        atpkg::activate::install_shims(
            &layout,
            &build,
            &["ay".to_string()],
            atpkg::activate::Aliases::Alab,
        )
        .unwrap();
        let shadowed = row(&ahead);
        assert_eq!(
            shadowed.update.fix.as_deref(),
            Some(atpkg::state::alias_hint("alab-ay").as_str())
        );
        std::fs::write(foreign.join("alab-ay"), b"#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(
            foreign.join("alab-ay"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        assert_eq!(row(&ahead).update.fix, None);
        std::fs::remove_file(foreign.join("alab-ay")).unwrap();
        let behind = std::env::join_paths([layout.bin_dir(), foreign]).unwrap();
        assert!(matches!(
            row(&behind).kind,
            ProgramStateKind::Managed {
                build: 18,
                index: 41
            }
        ));
        assert_eq!(
            atpkg::status::read(&layout).unwrap().programs["ay"].state,
            atpkg::state::managed(18, 41),
            "the record itself is never touched by a read"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn real_collector_turns_status_fifo_into_a_completed_error_report() {
        use std::os::unix::ffi::OsStrExt as _;

        let prefix =
            std::env::temp_dir().join(format!("aterm-packages-fifo-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&prefix);
        std::fs::create_dir_all(&prefix).unwrap();
        let layout = atpkg::Layout {
            prefix: prefix.clone(),
        };
        let status = layout.status();
        let status_c = std::ffi::CString::new(status.as_os_str().as_bytes()).unwrap();
        // SAFETY: `status_c` is a live NUL-terminated path in our private fixture.
        assert_eq!(unsafe { libc::mkfifo(status_c.as_ptr(), 0o600) }, 0);

        let report = collect_packages_status_from_layout(true, Some(&layout), None);
        assert!(
            report
                .collection_error
                .as_deref()
                .is_some_and(|error| error.contains("regular non-link")),
            "the production collector surfaces the checked read's FIFO refusal: {:?}",
            report.collection_error
        );
        let mut service = PackagesService::new();
        let sequence = service.begin(None).unwrap();
        assert!(service.finish(sequence, refresh(report)));
        let projection = service.state(true, true, true).projection();
        assert!(projection.observed && !projection.refreshing);
        let _ = std::fs::remove_dir_all(prefix);
    }

    fn machine_state(
        uc: atpkg::machine::UcPosture,
        home: atpkg::machine::HomePosture,
    ) -> atpkg::machine::MachineState {
        atpkg::machine::MachineState {
            universal_control: uc,
            policy: atpkg::config::UniversalControlPolicy::Off,
            home,
            config_unreadable: false,
            rosetta: atpkg::machine::RosettaPosture::Present,
            rosetta_wanted: true,
        }
    }

    /// A service that has observed a live, idle report: the gate every verb
    /// (the machine card's included) shares.
    fn live_service() -> PackagesService {
        let mut service = PackagesService::new();
        observe(
            &mut service,
            PackagesStatusReport::from_parts(true, true, "fp".into(), Some(&status("ok")), &[]),
        );
        service
    }

    /// The pass's own record answers the change: a `machine-settings:` line opens an
    /// expectation (the status reads as refreshing, Apply disabled) and no read is spawned;
    /// the `machine-state:` record the pass prints behind it closes it.
    #[test]
    fn a_passes_record_answers_its_change_without_a_read() {
        use atpkg::machine::{HomePosture, UcPosture};
        let now = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
        let mut service = PackagesService::new();
        service.note_machine_change("universal-control disabled".into(), now);
        service.expect_machine_record();
        let waiting = service.state(true, true, true).projection();
        assert!(waiting.machine.refreshing, "the status reads as refreshing");
        assert!(!waiting.machine.apply_enabled);
        assert!(service.machine().awaiting_record && !service.machine().refreshing);
        service.note_machine_record(machine_state(UcPosture::Disabled, HomePosture::Account));
        let m = service.machine();
        assert!(m.observed && !m.awaiting_record && !m.refreshing);
        assert_eq!(
            m.state.as_ref().map(|s| s.universal_control),
            Some(UcPosture::Disabled)
        );
        assert!(!service.take_record_expectation(), "nothing left to answer");

        assert!(service.request_machine_read(), "a read is spawned");
        assert!(
            !service.request_machine_read(),
            "…and a second queues a rerun"
        );
        service.note_machine_record(machine_state(UcPosture::Disabled, HomePosture::Account));
        assert!(service.machine().superseded);
        assert!(
            !service.machine().rerun,
            "the record satisfies the queued rerun"
        );
        let rerun = service
            .replace_machine_state(Ok(machine_state(UcPosture::Default, HomePosture::Account)));
        assert!(!rerun);
        let m = service.machine();
        assert!(!m.refreshing && !m.superseded);
        assert_eq!(
            m.state.as_ref().map(|s| s.universal_control),
            Some(UcPosture::Disabled),
            "the older read must not overwrite the pass's record"
        );

        service.expect_machine_record();
        assert!(service.take_record_expectation());
        assert!(!service.take_record_expectation());
        assert!(!service.machine().awaiting_record);

        assert!(service.request_machine_read(), "a read is running…");
        service.expect_machine_record();
        assert!(
            !service
                .replace_machine_state(Ok(machine_state(UcPosture::Default, HomePosture::Account)))
        );
        assert!(
            service.machine().awaiting_record,
            "a pre-change read measured the machine the change then changed"
        );
        assert!(
            service.request_machine_read(),
            "…and one started afterwards"
        );
        assert!(
            !service.replace_machine_state(Ok(machine_state(
                UcPosture::Disabled,
                HomePosture::Account
            )))
        );
        assert!(!service.machine().awaiting_record, "answers it");
        let seq = service.begin(Some(PackagesBusy::MachineApply)).unwrap();
        service.expect_machine_record();
        let done = succeeded(
            PackagesStatusReport::unobserved(),
            PackagesBusy::MachineApply,
        )
        .with_machine_state(Some(machine_state(
            UcPosture::Disabled,
            HomePosture::Account,
        )));
        assert!(service.finish(seq, done));
        let m = service.machine();
        assert!(!m.awaiting_record);
        assert_eq!(
            m.state.as_ref().map(|s| s.universal_control),
            Some(UcPosture::Disabled)
        );
    }

    /// The "This Mac" status reads the machine posture through the packages
    /// projection: unobserved offers no button; a read bumps the revision and ends
    /// the refresh (the verdict per posture is `machine_projection_covers_every_posture`);
    /// a failed read after a good one keeps the last measurement but never
    /// re-enables Apply on it.
    #[test]
    fn projection_carries_the_machine_posture() {
        use atpkg::machine::{HomePosture, UcPosture};
        let unobserved = PackagesService::new().state(true, true, true).projection();
        assert!(!unobserved.machine.observed);
        assert!(!unobserved.machine.apply_enabled);
        assert!(!unobserved.machine.nothing_to_apply);
        assert_eq!(unobserved.machine.last_change, None);
        assert_eq!(unobserved.machine.supported, cfg!(target_os = "macos"));

        let mut service = live_service();
        service.set_machine_refreshing(true);
        let before = service.revision();
        let _ = service
            .replace_machine_state(Ok(machine_state(UcPosture::Default, HomePosture::Account)));
        assert!(service.revision() > before, "a read fans out");
        assert!(!service.machine().refreshing, "a read ends the refresh");
        let live = service.state(true, true, true).projection();
        assert!(live.machine.observed);
        assert_eq!(live.machine.apply_enabled, cfg!(target_os = "macos"));

        let now = std::time::SystemTime::now();
        for (uc, home, saved) in [
            (
                UcPosture::Default,
                HomePosture::Account,
                Some(
                    "Saved: aterm turns Universal Control off at the next ALab tools check. \
                     Apply Now does it now.",
                ),
            ),
            (UcPosture::Disabled, HomePosture::Account, None),
            (
                UcPosture::Default,
                HomePosture::Mismatch,
                Some("Saved: aterm turns Universal Control off at the next ALab tools check."),
            ),
        ] {
            let _ = service.replace_machine_state(Ok(atpkg::machine::MachineState {
                policy: atpkg::config::UniversalControlPolicy::Leave,
                ..machine_state(uc, home)
            }));
            let off = service
                .state(true, true, true)
                .with_machine_config(atpkg::config::UniversalControlPolicy::Off)
                .projection_at(now);
            assert_eq!(off.machine.saved.as_deref(), saved, "{uc:?} {home:?}");
            assert_eq!(
                saved.is_some_and(|saved| saved.contains("Apply Now")),
                off.machine.next == MACHINE_APPLY_WOULD,
                "Apply Now named exactly where it is drawn ({uc:?} {home:?})"
            );
            let kept_off = service
                .state(false, true, true)
                .with_machine_config(atpkg::config::UniversalControlPolicy::Off)
                .projection_at(now);
            let when = crate::prefs::ApplicationTiming::DailyLaunchApply.clause();
            assert_eq!(
                kept_off.machine.saved.as_deref(),
                saved
                    .map(|saved| saved.replace("at the next ALab tools check", when))
                    .as_deref(),
                "{uc:?} {home:?} with Keep off"
            );
        }
        let _ = service
            .replace_machine_state(Ok(machine_state(UcPosture::Default, HomePosture::Account)));

        let _ = service.replace_machine_state(Err("atpkg machine printed no state".to_string()));
        let errored = service.state(true, true, true).projection();
        assert!(errored.machine.observed);
        assert!(!errored.machine.apply_enabled);
        assert!(!errored.machine.nothing_to_apply);
        assert!(errored.machine.next.is_empty(), "{}", errored.machine.next);
        assert!(
            service.machine().state.is_some(),
            "a failed read never erases the last measurement"
        );
    }

    /// `atpkg machine apply` is a local verb: Apply Now does not inherit the package page's
    /// manager gate.
    #[test]
    fn machine_apply_ignores_the_manager_gate() {
        use atpkg::machine::{HomePosture, UcPosture};
        let manager_off =
            PackagesStatusReport::from_parts(true, false, "fp".into(), Some(&status("ok")), &[]);
        let mut torn =
            PackagesStatusReport::from_parts(true, true, "fp".into(), Some(&status("ok")), &[]);
        torn.collection_error = Some("status.toml: torn record".to_string());
        for (report, why) in [(manager_off, "manager off"), (torn, "collection error")] {
            let mut service = PackagesService::new();
            let seq = service.begin(None).unwrap();
            assert!(service.finish(seq, refresh(report)));
            let _ = service
                .replace_machine_state(Ok(machine_state(UcPosture::Default, HomePosture::Account)));
            let p = service.state(true, true, true).projection();
            assert!(!p.actions_enabled, "{why}: the package verbs are gated");
            assert_eq!(
                p.machine.apply_enabled,
                cfg!(target_os = "macos"),
                "{why}: the machine verb is not"
            );
            assert!(!p.machine.nothing_to_apply, "{why}");
        }
    }

    /// The apply child reads the saved `[machine]` switch, so the verdict and the button
    /// follow measured posture × saved switch, never the record's stale `policy=`, in both
    /// directions.
    #[test]
    fn flipped_machine_switch_drives_the_apply_verdict_not_the_stale_record() {
        use atpkg::config::UniversalControlPolicy;
        use atpkg::machine::{HomePosture, UcPosture};
        let mut service = live_service();
        let _ = service
            .replace_machine_state(Ok(machine_state(UcPosture::Default, HomePosture::Account)));
        let b = service
            .state(true, true, true)
            .with_machine_config(UniversalControlPolicy::Leave)
            .projection();
        assert!(!b.machine.apply_enabled);
        assert!(b.machine.nothing_to_apply);
        assert!(
            b.machine.saved.is_some(),
            "the saved switch differs from the record"
        );
        let _ = service.replace_machine_state(Ok(atpkg::machine::MachineState {
            policy: UniversalControlPolicy::Leave,
            ..machine_state(UcPosture::Default, HomePosture::Account)
        }));
        let a = service
            .state(true, true, true)
            .with_machine_config(UniversalControlPolicy::Off)
            .projection();
        assert_eq!(a.machine.apply_enabled, cfg!(target_os = "macos"));
        assert!(!a.machine.nothing_to_apply);
        assert!(
            a.machine.saved.is_some(),
            "the saved switch differs from the record"
        );
    }

    /// A missing Rosetta 2 is something to apply on its own, with Universal Control already
    /// off: the status says it is missing and Apply Now names the install, not Universal
    /// Control.
    #[test]
    fn a_missing_rosetta_is_said_and_applied_on_its_own() {
        use atpkg::machine::{HomePosture, MachineState, RosettaPosture, UcPosture};
        let mut service = live_service();
        let missing = MachineState {
            rosetta: RosettaPosture::Missing,
            ..machine_state(UcPosture::Disabled, HomePosture::Account)
        };
        let _ = service.replace_machine_state(Ok(missing.clone()));
        let p = service.state(true, true, true).projection().machine;
        assert!(
            p.rosetta
                .as_deref()
                .is_some_and(|line| line.starts_with("Rosetta 2 isn\u{2019}t installed, so")),
            "{:?}",
            p.rosetta
        );
        assert_eq!(p.next, MACHINE_APPLY_WOULD_ROSETTA);
        assert_eq!(p.apply_enabled, cfg!(target_os = "macos"));
        assert!(!p.nothing_to_apply);
        assert!(
            p.saved.is_none(),
            "the Universal Control switch did not move"
        );
        let _ = service.replace_machine_state(Ok(MachineState {
            universal_control: UcPosture::Default,
            ..missing.clone()
        }));
        let both = service.state(true, true, true).projection().machine;
        assert_eq!(both.next, MACHINE_APPLY_WOULD_BOTH);
        let _ = service.replace_machine_state(Ok(missing));
        let off = service
            .state(true, true, true)
            .with_machine_rosetta(false)
            .projection()
            .machine;
        assert!(!off.apply_enabled);
        assert!(off.nothing_to_apply);
        assert!(
            off.rosetta
                .as_deref()
                .is_some_and(|line| line.contains("leaves it so")),
            "{:?}",
            off.rosetta
        );
        let _ = service
            .replace_machine_state(Ok(machine_state(UcPosture::Disabled, HomePosture::Account)));
        let done = service.state(true, true, true).projection().machine;
        assert!(done.rosetta.is_none());
        assert!(done.nothing_to_apply);
        assert_eq!(done.next, MACHINE_NOTHING_TO_APPLY);
    }

    /// A read asked for while one is running is queued behind it, never joined: the running
    /// read may have started before an apply and would land as a pre-apply record.
    #[test]
    fn a_read_in_flight_when_the_apply_finishes_is_rerun_not_joined() {
        use atpkg::machine::{HomePosture, UcPosture};
        let report =
            || PackagesStatusReport::from_parts(true, true, "fp".into(), Some(&status("ok")), &[]);
        let mut service = live_service();
        assert!(service.request_machine_read(), "idle ⇒ the caller spawns");
        assert!(service.machine().refreshing);
        assert!(!service.machine().rerun);

        let seq = service.begin(Some(PackagesBusy::MachineApply)).unwrap();
        assert!(
            service.finish(
                seq,
                succeeded(report(), PackagesBusy::MachineApply)
                    .with_machine_verdict(Some("applied — universal-control disabled".to_string())),
            )
        );
        assert!(
            !service.request_machine_read(),
            "a read in flight is queued, not joined"
        );
        assert!(service.machine().refreshing);
        assert!(service.machine().rerun);

        assert!(
            service
                .replace_machine_state(Ok(machine_state(UcPosture::Default, HomePosture::Account)))
        );
        assert!(!service.machine().rerun, "handed back exactly once");
        let stale = service.state(true, true, true).projection();
        assert!(stale.machine.observed);
        assert!(stale.machine.refreshing);
        assert!(!stale.machine.apply_enabled);

        assert!(
            !service.replace_machine_state(Ok(machine_state(
                UcPosture::Disabled,
                HomePosture::Account
            )))
        );
        let fresh = service.state(true, true, true).projection();
        assert!(!fresh.machine.refreshing);
        assert!(fresh.machine.nothing_to_apply);
        assert!(!fresh.machine.apply_enabled);
    }

    /// A change a pass reported reaches the status and asks for a fresh read: the wake arm
    /// notes the change (the status's "Last change") and starts a re-read.
    #[test]
    fn a_reported_change_lands_on_the_status_and_asks_for_a_read() {
        use atpkg::machine::{HomePosture, UcPosture};
        let mut service = live_service();
        let _ = service
            .replace_machine_state(Ok(machine_state(UcPosture::Default, HomePosture::Account)));
        let before = service.state(true, true, true).projection().machine;
        assert_eq!(before.last_change, None);

        let at = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000);
        service.note_machine_change("universal-control disabled".to_string(), at);
        let after = service
            .state(true, true, true)
            .projection_at(at + std::time::Duration::from_secs(24))
            .machine;
        assert_ne!(after.last_change, before.last_change);
        assert!(
            after.last_change.as_deref().is_some_and(|change| change
                .starts_with("Last change: ALab tools turned off Universal Control · ")),
            "{:?}",
            after.last_change
        );
        assert!(
            service.request_machine_read(),
            "the arm's other half: a re-read starts"
        );
    }

    /// A config that does not parse is a refusal, not a measurement: the `[machine]` default
    /// acts, so the status offers no Apply and says which file to fix when the record
    /// reports that the switches could not be read.
    #[test]
    fn an_unreadable_config_is_said_in_the_status_and_disables_apply() {
        use atpkg::config::UniversalControlPolicy::Off;
        use atpkg::machine::{HomePosture, MachineState, UcPosture};
        let mut service = live_service();
        let _ = service.replace_machine_state(Ok(MachineState {
            config_unreadable: true,
            ..machine_state(UcPosture::Default, HomePosture::Account)
        }));
        let projection = service
            .state(true, true, true)
            .with_machine_config(Off)
            .projection()
            .machine;
        let reason = projection.reason.as_deref().expect("a refusal is said");
        assert!(
            reason.contains("aterm.toml"),
            "names the file to fix: {reason}"
        );
        assert!(
            projection.next.is_empty(),
            "nothing is next when nothing may be applied: {projection:?}"
        );
        assert!(
            !projection.apply_enabled,
            "Apply must be off: {projection:?}"
        );
        assert!(
            !projection.nothing_to_apply,
            "a refusal is not the same as being already applied: {projection:?}"
        );
    }

    /// Every posture the card can be in, from the record, with the verdict each
    /// one yields: the half/unknown Universal Control states still apply, a posture
    /// already where `[machine]` wants it has nothing to apply, and an unresolved
    /// home refuses with a reason.
    #[test]
    fn machine_projection_covers_every_posture() {
        use atpkg::config::UniversalControlPolicy::{Leave, Off};
        use atpkg::machine::{HomePosture, MachineState, UcPosture};
        let rows = [
            (
                "default",
                machine_state(UcPosture::Default, HomePosture::Account),
                Off,
                false,
                false,
                true,
            ),
            (
                "disabled: nothing left",
                machine_state(UcPosture::Disabled, HomePosture::Account),
                Off,
                false,
                true,
                false,
            ),
            (
                "home mismatch",
                machine_state(UcPosture::Default, HomePosture::Mismatch),
                Off,
                true,
                false,
                false,
            ),
            (
                "partial",
                machine_state(UcPosture::Partial, HomePosture::Account),
                Off,
                false,
                false,
                true,
            ),
            (
                "unknown",
                machine_state(UcPosture::Unknown, HomePosture::Account),
                Off,
                false,
                false,
                true,
            ),
            (
                "default, left alone",
                MachineState {
                    policy: Leave,
                    ..machine_state(UcPosture::Default, HomePosture::Account)
                },
                Leave,
                false,
                true,
                false,
            ),
            (
                "home mismatch, already as chosen",
                machine_state(UcPosture::Disabled, HomePosture::Mismatch),
                Off,
                false,
                true,
                false,
            ),
            (
                "home unresolved",
                machine_state(UcPosture::Default, HomePosture::Unresolved),
                Off,
                true,
                false,
                false,
            ),
        ];
        for (name, state, saved, reason, nothing_to_apply, apply_on_macos) in rows {
            let mut service = live_service();
            let _ = service.replace_machine_state(Ok(state));
            let p = service
                .state(true, true, true)
                .with_machine_config(saved)
                .projection();
            assert_eq!(p.machine.reason.is_some(), reason, "{name}");
            assert_eq!(p.machine.nothing_to_apply, nothing_to_apply, "{name}");
            assert_eq!(
                p.machine.apply_enabled,
                cfg!(target_os = "macos") && apply_on_macos,
                "{name}"
            );
            assert!(p.machine.saved.is_none(), "{name}: saved == record");
        }
    }

    /// The long recorded refusal ([`INCIDENT_2026_09_14_REASON`]).
    fn incident_reason() -> String {
        INCIDENT_2026_09_14_REASON.to_string()
    }

    /// The canonical `atpkg::state` spellings the page paints every day fit one row at
    /// the narrowest budget and come back byte-for-byte: the layout of the common case
    /// is unchanged by the wrap.
    #[test]
    fn reason_lines_keeps_a_short_state_unchanged() {
        for state in [
            "managed 1971 — pinned by index 41",
            "",
            "error: stale link. fix: `aterm pkg repair`",
        ] {
            assert_eq!(
                reason_lines(state, 64),
                vec![state.to_string()],
                "{state:?}"
            );
        }
        let edge = "x".repeat(64);
        assert_eq!(reason_lines(&edge, 64), vec![edge.clone()]);
        assert_eq!(reason_lines(&format!("{edge} y"), 64).len(), 2);
    }

    /// The long refusal: the fix leads, whole, at every budget the page uses — nothing of
    /// it is cut, no line exceeds its budget, and the diagnosis is bounded after it.
    #[test]
    fn reason_lines_puts_the_fix_first_and_whole() {
        let incident = incident_reason();
        assert!(
            incident.chars().count() > 600,
            "{}",
            incident.chars().count()
        );
        let fix_at = incident
            .find(" fix: ")
            .expect("atpkg spells the remedy ` fix: `");
        let fix_clause = &incident[fix_at + " fix: ".len()..];
        for width in [64usize, 104, 144] {
            let lines = reason_lines(&incident, width);
            assert!(
                lines[0].starts_with("fix: unset ATPKG_REFUSE_TRACKED_INSTALL"),
                "{width}: {:?}",
                lines[0]
            );
            for line in &lines {
                assert!(fits(line, width), "{width}: {line:?}");
            }
            let fix_lines: Vec<&str> = lines
                .iter()
                .map(String::as_str)
                .take_while(|line| {
                    !line.starts_with("error: stage:") && !line.starts_with('\u{2026}')
                })
                .collect();
            assert_eq!(
                fix_lines.join(" "),
                format!("fix: {fix_clause}"),
                "{width}: nothing of the fix is cut"
            );
            let last = lines.last().unwrap();
            assert!(last.ends_with(" more)"), "{width}: {last:?}");
            let expected_len = if fix_lines.len() >= MAX_REASON_LINES {
                fix_lines.len() + 1
            } else {
                MAX_REASON_LINES
            };
            assert_eq!(lines.len(), expected_len, "{width}: {lines:#?}");
            if fix_lines.len() < MAX_REASON_LINES {
                assert!(
                    lines[fix_lines.len()]
                        .starts_with("error: stage: this process is provenance-tracked"),
                    "{width}: the diagnosis head follows the fix: {:?}",
                    lines[fix_lines.len()]
                );
            }
        }
    }

    /// No remedy to lead with: the reason wraps whole while it fits the bound, and past
    /// it the marker replaces the last visible line's tail, counting the hidden lines.
    #[test]
    fn reason_lines_wraps_and_bounds_a_reason_without_a_fix() {
        let within = "error: stage: the signed index names build 1971 for ay but the store holds \
                      no such tree and the download step was refused by the policy";
        let wrapped = reason_lines(within, 48);
        assert!(
            wrapped.len() > 1 && wrapped.len() <= MAX_REASON_LINES,
            "{wrapped:#?}"
        );
        assert_eq!(
            wrapped.join(" "),
            within.split_whitespace().collect::<Vec<_>>().join(" ")
        );
        for line in &wrapped {
            assert!(fits(line, 48), "{line:?}");
        }

        let long: String = (0..60)
            .map(|n| format!("word{n:02}"))
            .collect::<Vec<_>>()
            .join(" ");
        let all = wrap_words(&long, 40);
        assert!(all.len() > MAX_REASON_LINES);
        let bounded = reason_lines(&long, 40);
        assert_eq!(bounded.len(), MAX_REASON_LINES);
        assert_eq!(bounded[..MAX_REASON_LINES - 1], all[..MAX_REASON_LINES - 1]);
        let hidden = all.len() - MAX_REASON_LINES;
        let last = &bounded[MAX_REASON_LINES - 1];
        assert!(
            last.ends_with(&format!(" \u{2026}({hidden} more)")),
            "{last:?}"
        );
        assert!(fits(last, 40), "{last:?}");
        assert!(all[MAX_REASON_LINES - 1].starts_with(last.split(" \u{2026}").next().unwrap()));

        let path = format!(
            "/Users//example/Library/Application-Support/aterm/pkg/{}",
            "a".repeat(70)
        );
        let pieces = wrap_words(&path, 40);
        assert!(pieces.len() > 1 && pieces.iter().all(|line| fits(line, 40)));
        assert_eq!(pieces.concat(), path);
    }

    /// A CJK REASON WRAPS BETWEEN ITS CHARACTERS, each at two average glyphs: every line
    /// is inside its budget at that charge, no line starts with a closer or a small kana,
    /// nothing is put between the characters a break parted, and a Latin word beside them
    /// keeps its space.
    #[test]
    fn a_cjk_reason_wraps_between_its_characters() {
        let reason = format!(
            "{}。{}、atpkg を確認してください。",
            "パッケージの状態を読み込めませんでし", "フォルダーの権限がありません"
        );
        for width in 12..=48 {
            let lines = wrap_words(&reason, width);
            assert!(lines.len() > 1, "{width}: {lines:?}");
            for line in &lines {
                assert!(fits(line, width), "{width}: {line:?}");
                let wide = line
                    .chars()
                    .filter(|c| aterm_grapheme::char_width(*c) > 1)
                    .count();
                assert!(
                    2 * wide <= width,
                    "{width}: {line:?} holds {wide} wide chars"
                );
            }
            for line in &lines[1..] {
                let first = line.chars().next().expect("a line has a character");
                assert!(
                    !['。', '、', 'ッ', 'ー'].contains(&first),
                    "{width}: {lines:?}"
                );
            }
            assert_eq!(
                lines.concat().replace(' ', ""),
                reason.replace(' ', ""),
                "{width}: {lines:?}"
            );
        }
        assert_eq!(
            wrap_words("設定 atpkg", 40),
            ["設定 atpkg"],
            "a space between a CJK word and a Latin one stays"
        );
    }

    /// The marker is the word `fix:`: a `prefix:` in a path sentence is not a remedy, a
    /// bundle line's parenthesised `fix:` is, and one glued to a path segment is not.
    #[test]
    fn reason_lines_marker_is_the_word_fix() {
        let prefix = format!(
            "error: stage: the prefix: /Users//example/Library/Application Support/aterm/pkg is {}",
            "not writable and the untracked lane could not be reached for the install pass"
        );
        let lines = reason_lines(&prefix, 48);
        assert!(
            lines[0].starts_with("error: stage: the prefix:"),
            "{lines:#?}"
        );
        assert!(
            lines.iter().all(|line| !line.starts_with("fix:")),
            "{lines:#?}"
        );

        let bundle = format!(
            "error: the bundle at /Applications/aterm.app is missing {} (the bundle's fix: {})",
            "Contents/MacOS/aterm",
            "reinstall aterm from its disk image, then `aterm pkg repair` re-lays the shims"
        );
        let lines = reason_lines(&bundle, 64);
        assert!(
            lines[0].starts_with("fix: reinstall aterm from its disk image"),
            "{lines:#?}"
        );

        let glued = format!(
            "error: stage: /tmp/fix:abc is not a store path {}",
            "x".repeat(60)
        );
        let lines = reason_lines(&glued, 48);
        assert!(
            lines[0].starts_with("error: stage: /tmp/fix:abc"),
            "{lines:#?}"
        );
    }

    /// 2026-09-21T14:13:20Z, a Monday.
    const NOW: i64 = 1_790_000_000;
    /// UTC-7 (a Pacific summer clock).
    const PDT: i64 = -7 * 3600;

    /// When, as a person reads it: relative inside a day, then the local calendar (the
    /// offset moves the day and the clock, never a raw UTC stamp); a time ahead of now (a
    /// clock set back) is never "ago".
    #[test]
    fn when_words_are_relative_then_the_local_calendar() {
        let w = |ago: i64, offset: i64| when_words(NOW - ago, NOW, LocalClock::fixed(offset));
        assert_eq!(w(4 * 60, PDT), "4 min ago");
        assert_eq!(w(86_400, PDT), "yesterday 07:13");
        assert_eq!(w(400 * 86_400, PDT), "Aug 17 2025");
        assert_eq!(w(25 * 3600, 0), "yesterday 13:13");
        assert_eq!(w(25 * 3600, 12 * 3600), "yesterday 01:13");
        assert_eq!(w(25 * 3600, -14 * 3600), "Sat 23:13");
        let utc = LocalClock::fixed(0);
        assert_eq!(when_words(NOW + 7200, NOW, utc), "today 16:13");
        assert_eq!(when_words(NOW + 86_400, NOW, utc), "Sep 22 14:13");
        for words in [
            w(86_400, PDT),
            w(400 * 86_400, 0),
            when_words(NOW + 60, NOW, utc),
        ] {
            assert!(!words.contains('T') && !words.contains('Z'), "{words}");
        }
    }

    /// Each instant reads on its own offset: a window that lives across a daylight-saving
    /// switch reads it on its next collection, and a time from before it keeps the hour it
    /// was stamped under (14:05 PDT the Saturday before `SWITCH` reads `14:05`, never
    /// `13:05`).
    #[test]
    fn the_local_clock_reads_each_instant_on_its_own_offset() {
        const PST: i64 = -8 * 3600;
        const SWITCH: i64 = 1_793_523_600;
        let now = SWITCH + 36 * 3600;
        let zone = |at: i64| Some(if at < SWITCH { PDT } else { PST });
        let clock = LocalClock::probe(now, zone);
        let (since, before) = clock.changed.expect("the switch is inside the week");
        assert!((SWITCH..SWITCH + 60).contains(&since), "{since}");
        assert_eq!(before, PDT);
        let saturday = 1_793_480_700;
        assert_eq!(when_words(saturday, now, clock), "Sat 14:05");
        assert_eq!(
            when_words(saturday, now, LocalClock::fixed(PST)),
            "Sat 13:05",
            "one offset for every instant is the hour off this replaces"
        );
        assert_eq!(
            when_words(saturday + 20 * 3600, now, clock),
            "yesterday 09:05"
        );
        assert_eq!(
            LocalClock::probe(now + 30 * 86_400, zone),
            LocalClock::fixed(PST)
        );
        assert_eq!(LocalClock::probe(now, |_| None), LocalClock::UNKNOWN);
        assert_eq!(
            when_words(saturday, now, LocalClock::UNKNOWN),
            "Sat 21:05 UTC"
        );
        assert_eq!(
            when_words(now - 120, now, LocalClock::UNKNOWN),
            "2 min ago",
            "a relative time needs no zone"
        );
        if let (Some(then), Some(at_now)) = (
            crate::presence::local_offset_at(now - CLOCK_WINDOW_S),
            crate::presence::local_offset_at(now),
        ) {
            let live = LocalClock::read(now);
            assert_eq!(live.offset_s, Some(at_now));
            assert_eq!(live.changed.is_some(), then != at_now, "{live:?}");
        }
    }

    fn vendor_id(version: &str) -> u64 {
        atpkg::vendor_direct::Version::parse(version)
            .unwrap()
            .build_id()
    }

    /// A report over `rows` (name, installed build, state), index build 44.
    fn report_of(rows: &[(&str, Option<u64>, String)]) -> PackagesStatusReport {
        let mut status = status("up to date (index build 44)");
        status.last_index_build = 44;
        status.programs = rows
            .iter()
            .map(|(name, build, state)| {
                (
                    (*name).to_string(),
                    atpkg::ProgramStatus {
                        installed_build: *build,
                        state: state.clone(),
                        tree_root: String::new(),
                    },
                )
            })
            .collect();
        PackagesStatusReport::from_parts(true, true, "fp".into(), Some(&status), &[])
    }

    /// The headline says "up to date" only when every row that claims it is: a row at its
    /// index's pin that knows a newer build stops saying it, and so does the headline.
    #[test]
    fn the_headline_says_up_to_date_only_when_every_current_row_is() {
        let headline = |ay_latest: &str| {
            let mut report = report_of(&[
                ("ay", Some(1971), atpkg::state::managed(1971, 44)),
                (
                    "codex",
                    Some(vendor_id("0.156.0")),
                    atpkg::state::vendor_kept("0.156.0", "held by local pin"),
                ),
            ]);
            for r in &mut report.programs {
                r.update.latest_known = Some(if r.name == "ay" {
                    ay_latest.to_string()
                } else {
                    "0.157.0".to_string()
                });
            }
            let mut service = PackagesService::new();
            observe(&mut service, report);
            projection(&service).headline
        };
        assert!(headline("build 1980").contains("available"));
        assert!(
            headline("build 1971").contains("up to date"),
            "a pinned row knowing a newer build is not the page's news"
        );
    }

    /// A pass's own sentence is dropped only when the headline says all of it: `up to date
    /// (index build 45)` folds into "Checked 3 min ago"; one followed by any qualification
    /// stays.
    #[test]
    fn a_qualified_up_to_date_outcome_is_shown_not_folded() {
        assert!(outcome_is_plainly_current("up to date"));
        assert!(outcome_is_plainly_current("up to date (index build 45)"));
        assert!(outcome_is_plainly_current("up to date (ay build 8256)"));
        let qualified = "up to date (ay build 1971) \u{2014} a clause the headline does not carry";
        assert!(!outcome_is_plainly_current(qualified));
        assert!(!outcome_is_plainly_current("installed ay build 1972"));
        let mut service = PackagesService::new();
        observe(
            &mut service,
            PackagesStatusReport::from_parts(
                true,
                true,
                "fp".into(),
                Some(&status(qualified)),
                &[],
            ),
        );
        let detail = projection(&service).detail.unwrap_or_default();
        assert!(
            detail.contains("a clause the headline does not carry"),
            "{detail}"
        );
    }

    /// What the store says, read on the worker: a vendor build's `.vendor` record names its
    /// version and when it was verified (its "updated"), its stamp the head last seen and
    /// when; an ALab build's latest known is the verified index's pin, and its "updated"
    /// is when its `current` link was flipped.
    #[cfg(unix)]
    #[test]
    fn the_store_supplies_when_a_build_went_live_and_the_latest_known() {
        let root =
            std::env::temp_dir().join(format!("aterm-packages-facts-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let layout = atpkg::Layout {
            prefix: root.join("pkg"),
        };
        let version = atpkg::vendor_direct::Version::parse("2.1.280").unwrap();
        let build = layout.build_dir("claude", version.build_id());
        std::fs::create_dir_all(build.join("bin")).unwrap();
        let spec = atpkg::vendor_direct::spec("claude").unwrap();
        let record = atpkg::vendor_direct::VendorRecord {
            schema: 1,
            program: "claude".into(),
            version,
            vendor: spec.vendor.to_string(),
            source_url: format!("{}2.1.280/darwin-arm64/claude", spec.url_prefixes[0]),
            sha256: "a".repeat(64),
            sha512: String::new(),
            size: 1,
            tree_root: "0".repeat(64),
            apple_team: cfg!(target_os = "macos").then(|| spec.apple_team.to_string()),
            anchor: spec.anchor,
            build_date: None,
            verified_at: NOW - 7200,
        };
        std::fs::write(
            atpkg::vendor_direct::record_path(&build).unwrap(),
            aterm_toml::to_string(&record).unwrap(),
        )
        .unwrap();
        atpkg::store::mark_build_ready(&build).unwrap();
        atpkg::vendor_direct::ProgramStamp::record_check(
            &layout,
            "claude",
            NOW - 240,
            atpkg::vendor_direct::Version::parse("2.1.281"),
            None,
        )
        .unwrap();
        let ay = layout.build_dir("ay", 1971);
        std::fs::create_dir_all(&ay).unwrap();
        std::os::unix::fs::symlink(&ay, layout.program_current("ay")).unwrap();

        let mut report = report_of(&[
            (
                "claude",
                Some(version.build_id()),
                atpkg::state::vendor_managed("2.1.280", "Anthropic"),
            ),
            ("ay", Some(1971), atpkg::state::managed(1971, 44)),
        ]);
        let pins: std::collections::BTreeMap<String, u64> =
            [("ay".to_string(), 1980)].into_iter().collect();
        report.attach_store_facts(&layout, Some(&pins));
        let fact = |name: &str| {
            report
                .programs
                .iter()
                .find(|r| r.name == name)
                .unwrap()
                .update
                .clone()
        };
        let claude = fact("claude");
        assert_eq!(claude.version.as_deref(), Some("2.1.280"));
        assert_eq!(claude.source, "Anthropic latest");
        assert_eq!(claude.updated_at, Some(NOW - 7200), "its verification");
        assert_eq!(
            claude.latest_known.as_deref(),
            Some("2.1.281"),
            "the stamp's head"
        );
        assert_eq!(claude.checked_at, Some(NOW - 240));
        let ay = fact("ay");
        assert_eq!(ay.version.as_deref(), Some("build 1971"));
        assert_eq!(ay.source, "ALab index 44");
        assert_eq!(
            ay.latest_known.as_deref(),
            Some("build 1980"),
            "the index pin"
        );
        let flipped = ay.updated_at.expect("the current link's time");
        let wall = aterm_time::unix_secs().unwrap() as i64;
        assert!(
            (wall - 120..=wall + 5).contains(&flipped),
            "{flipped} vs {wall}"
        );
        let mut unpinned = report_of(&[("ay", Some(1971), atpkg::state::managed(1971, 44))]);
        unpinned.attach_store_facts(&layout, None);
        assert_eq!(
            unpinned.programs[0].update.latest_known.as_deref(),
            Some("build 1971")
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The worker reads the upstream an ALab codex is based on off the record of the store
    /// build its shim runs, in `which`'s words: a record from before the field, a malformed
    /// one and a program ALab does not build carry none.
    #[cfg(unix)]
    #[test]
    fn the_worker_reads_the_upstream_an_alab_build_records() {
        use std::os::unix::fs::PermissionsExt as _;
        let root = aterm_tempfile::tempdir().expect("scratch folder");
        let layout = atpkg::Layout {
            prefix: root.path().join("pkg"),
        };
        const BUILD: u64 = 2_026_101_601;
        let dir = layout.build_dir("codex", BUILD);
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        std::fs::write(dir.join("bin/codex"), b"#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(
            dir.join("bin/codex"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        std::fs::write(
            dir.join(atpkg::alab::PACKAGE_JSON),
            r#"{"layoutVersion":1,"version":"0.164.0-dev.12+gabc","entrypoint":"bin/codex"}"#,
        )
        .unwrap();
        std::fs::write(dir.join(atpkg::alab::ALAB_BUILD_JSON), br#"{"source":"0"}"#).unwrap();
        atpkg::store::mark_build_ready(&dir).unwrap();
        atpkg::activate::activate_build(&layout, &dir).unwrap();
        atpkg::activate::install_shims(
            &layout,
            &dir,
            &["codex".to_string()],
            atpkg::activate::Aliases::Off,
        )
        .unwrap();
        let mut programs = std::collections::BTreeMap::new();
        for (name, build) in [("codex", BUILD), ("ay", 1971)] {
            programs.insert(
                name.to_string(),
                atpkg::ProgramStatus {
                    installed_build: Some(build),
                    state: atpkg::state::managed(build, 75),
                    tree_root: String::new(),
                },
            );
        }
        atpkg::status::write(
            &layout,
            &atpkg::Status {
                schema: 1,
                programs,
                ..Default::default()
            },
        )
        .unwrap();
        let upstream = |name: &str| {
            collect_packages_status_from_layout(true, Some(&layout), None)
                .programs
                .into_iter()
                .find(|r| r.name == name)
                .unwrap()
                .upstream
        };
        assert_eq!(upstream("codex"), None, "a record from before the field");
        let sha = "0123456789abcdef0123456789abcdef01234567";
        let record = dir.join(atpkg::alab::ALAB_BUILD_JSON);
        std::fs::write(
            &record,
            format!(
                r#"{{"source":"0","upstream":{{"release":"rust-v0.163.0","commit":"{sha}","commit_date":"2026-10-15T18:02:11Z"}}}}"#
            ),
        )
        .unwrap();
        assert_eq!(
            upstream("codex").as_deref(),
            Some("based on upstream rust-v0.163.0+g0123456789ab")
        );
        assert_eq!(upstream("ay"), None, "ALab does not build it");
        for bad in [
            r#"{"source":"0","upstream":"rust-v0.163.0"}"#,
            r#"{"source":"0","upstream":{"release":"rust-v0.163.0","commit":"0123"}}"#,
            "{\"upstream\":{",
        ] {
            std::fs::write(&record, bad).unwrap();
            assert_eq!(upstream("codex"), None, "{bad}");
        }
    }

    fn entry(at: i64, kind: &str, pid: u32, fields: &[(&str, &str)]) -> atpkg::packages_log::Entry {
        atpkg::packages_log::Entry {
            at: Some(at),
            kind: kind.to_string(),
            pid,
            fields: fields
                .iter()
                .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                .collect(),
        }
    }

    /// THE ACTIVITY READS PASSES, not lines: a log of idle passes, each a start, the
    /// discovery's `index-resolved` line and an end, still shows the newest 25 passes, as it
    /// did before every discovery wrote its line — the read counts only the kinds the
    /// Activity says, where a read of the last 50 lines of any kind showed 16.
    #[test]
    fn the_activity_shows_as_many_passes_beside_each_discovery_line() {
        let dir = aterm_tempfile::tempdir().expect("scratch folder");
        let mut log = String::new();
        for pass in 0..40u32 {
            let at = |s: u32| {
                format!(
                    "2026-09-21T{:02}:{:02}:{:02}Z",
                    pass / 6,
                    (pass % 6) * 10,
                    s
                )
            };
            let pid = 1000 + pass;
            log.push_str(&format!(
                "{}\tpass-start\tpid={pid}\tlane=window\tverb=update\n",
                at(0)
            ));
            log.push_str(&format!(
                "{}\tindex-resolved\tpid={pid}\theads=3\trounds=1\tgets=0\tfound=44\n",
                at(1)
            ));
            log.push_str(&format!(
                "{}\tpass-end\tpid={pid}\tlane=window\tverb=update\texit=0\tsecs=1\n",
                at(2)
            ));
        }
        std::fs::write(dir.path().join(atpkg::packages_log::LOG_NAME), log).unwrap();
        let entries = atpkg::packages_log::read_tail_of(
            dir.path(),
            atpkg::packages_log::ACTIVITY_EVENTS,
            shown_in_activity,
        );
        let passes = activity_sentences(&entries)
            .iter()
            .filter(|(entry, ..)| entry.kind == atpkg::packages_log::kind::PASS_END)
            .count();
        assert_eq!(passes, 25, "{entries:?}");
    }

    /// The Activity: the package log's events, newest first, with relative local times: a
    /// refusal is trouble, a hold is not; a pass's end that was offline or "did not run" is
    /// no trouble, a failure is; a start with no end after it is listed, a start whose end
    /// is listed is left out.
    #[test]
    fn the_activity_reads_the_log_newest_first() {
        use atpkg::packages_log::kind;
        let entries = vec![
            entry(
                NOW - 30,
                kind::PASS_START,
                9,
                &[("lane", "session"), ("verb", "update")],
            ),
            entry(
                NOW - 60,
                kind::PASS_END,
                7,
                &[
                    ("lane", "window"),
                    ("verb", "update"),
                    ("exit", "0"),
                    ("secs", "7"),
                    ("outcome", "up to date (index build 44)"),
                ],
            ),
            entry(
                NOW - 70,
                kind::PROGRAM,
                7,
                &[
                    ("program", "claude"),
                    ("from", "2.1.278"),
                    ("to", "2.1.280"),
                    ("source", "Anthropic latest"),
                    ("result", "updated"),
                ],
            ),
            entry(
                NOW - 80,
                kind::PROGRAM,
                7,
                &[
                    ("program", "codex"),
                    ("from", "0.156.0"),
                    ("source", "OpenAI latest"),
                    ("result", "refused"),
                    ("reason", "error: codex 0.157.0 refused: digest mismatch"),
                ],
            ),
            entry(
                NOW - 90,
                kind::PROGRAM,
                7,
                &[
                    ("program", "ay"),
                    ("from", "build 1971"),
                    ("source", "ALab index 44"),
                    ("result", "held"),
                    ("reason", "held: build 1980 is not published for this Mac"),
                ],
            ),
            entry(
                NOW - 100,
                kind::PASS_START,
                7,
                &[("lane", "window"), ("verb", "update")],
            ),
            entry(
                NOW - 3 * 3600,
                kind::PASS_END,
                5,
                &[
                    ("lane", "head watch"),
                    ("verb", "update claude"),
                    ("exit", "69"),
                ],
            ),
            entry(
                NOW - 86_400,
                kind::PASS_END,
                4,
                &[
                    ("lane", "typed"),
                    ("verb", "seed"),
                    ("exit", "75"),
                    ("outcome", "did not run: another pass holds the store"),
                ],
            ),
            entry(
                NOW - 2 * 86_400,
                kind::PASS_END,
                3,
                &[
                    ("lane", "agent update"),
                    ("verb", "update codex"),
                    ("exit", "1"),
                    ("secs", "12"),
                    ("outcome", "codex: stage failed"),
                ],
            ),
        ];
        let lines = activity_lines(&entries, NOW, LocalClock::fixed(PDT));
        let got: Vec<(&str, bool)> = lines.iter().map(|l| (l.when.as_str(), l.trouble)).collect();
        assert_eq!(
            got,
            vec![
                ("just now", false),
                ("1 min ago", false),
                ("1 min ago", false),
                ("1 min ago", true),
                ("1 min ago", false),
                ("3 h ago", false),
                ("yesterday 07:13", false),
                ("Sat 07:13", true),
            ]
        );
        assert!(lines[2].text.contains("2.1.280"), "{}", lines[2].text);
        assert!(
            lines[3].text.contains("digest mismatch"),
            "{}",
            lines[3].text
        );
        assert!(activity_lines(&[], NOW, LocalClock::fixed(0)).is_empty());
        assert!(
            program_activity(
                "ty",
                Some("build 5"),
                None,
                "ALab index 44",
                "failed",
                "error: stage: disk full"
            )
            .1
        );
    }
}
