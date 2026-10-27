// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `aterm doctor`'s own rows as converge items (docs/DESIGN-init-system-2026-10-20.md §4.2, S5).

use std::sync::Mutex;

use aterm_converge::{Class, Explain, Item, ItemReport, Layer, Outcome, Probe, Readiness, Remedy};

use super::{DoctorRow, Mark};

/// The shell row: whether aterm can start the shell.
pub const SHELL: &str = "base.shell.starts";
/// The stale copies of aterm (macOS): the one `asked` item.
pub const COPIES: &str = "base.pkg.copies";

/// The Developer Tools row, whose fix-mode gather leaves the owner's hand-over to its pane
/// for after the report.
pub const DEVTOOLS: &str = "base.privacy.devtools";

/// Whether a run narrowed to `only` reads the Developer Tools row.
#[must_use]
pub fn hands_over(only: &[String]) -> bool {
    reads(only, "devtools:")
}

/// Whether a run narrowed to `only` reads the row labelled `label`: every row when no word
/// narrows it, the shell's always, and otherwise each row a word names.
#[must_use]
pub fn reads(only: &[String], label: &str) -> bool {
    only.is_empty()
        || label == "shell:"
        || named(label).is_some_and(|(id, family, _)| {
            only.iter()
                .any(|token| aterm_converge::selects(token, id, family, Some(Layer::Base)))
        })
}

/// Whether the `--only` word `token` names one of doctor's own rows on any platform.
#[must_use]
pub fn names(token: &str) -> bool {
    ROWS.iter()
        .any(|(_, id, family, _)| aterm_converge::selects(token, id, family, Some(Layer::Base)))
}

/// `aterm doctor`'s grammar after its name.
pub const GRAMMAR: &str = "[--read-only] [--json] [--verbose] [--only <id,...>] | (--check \
                           [--verbose] | --apply | --restore) [--json]";

/// One line per way to run `aterm doctor`, as `aterm doctor --help` and `aterm help doctor`
/// print them under the grammar.
pub const HELP_ROWS: &str = concat!(
    "  aterm doctor               check, repair what aterm lays, print only what needs you\n",
    "  aterm doctor --verbose     every row, the ok ones too\n",
    "  aterm doctor --read-only   the same checks, changing nothing (--json: the report as JSON)\n",
    "  aterm doctor --only <id,…> only those items, families or layers\n",
    "  aterm doctor --check       only the macOS settings profile (--apply, --restore change it)\n",
);

/// `aterm doctor --help`: the grammar and [`HELP_ROWS`].
#[must_use]
pub fn help() -> String {
    format!("usage: aterm doctor {GRAMMAR}\n{HELP_ROWS}")
}

const ROWS: [(&str, &str, &str, &str); 13] = [
    ("shell:", SHELL, "shell", "the shell aterm starts"),
    (
        "privacy:",
        "base.privacy.fda",
        "privacy",
        "Full Disk Access",
    ),
    (
        "debugger:",
        "base.privacy.debugger",
        "privacy",
        "debugger access",
    ),
    (
        "devtools:",
        DEVTOOLS,
        "privacy",
        "aterm on the Developer Tools list",
    ),
    ("copies:", COPIES, "pkg", "stale copies of aterm"),
    (
        "spotlight:",
        "base.machine.spotlight",
        "machine",
        "what Spotlight indexes of Rust build output and rustup's docs",
    ),
    (
        "syspolicyd:",
        "base.machine.syspolicyd",
        "machine",
        "whether macOS's program check answers",
    ),
    (
        "services:",
        "base.machine.services",
        "machine",
        "what macOS's own services hold in memory",
    ),
    (
        "conpty:",
        "base.shell.conpty",
        "shell",
        "the console host a new tab runs in",
    ),
    (
        "instances:",
        "base.session.instances",
        "session",
        "the aterm instances and other aterm programs running as you",
    ),
    (
        "fabric:",
        "base.session.fabric",
        "session",
        "the fabric's broker",
    ),
    (
        "recovery:",
        "base.session.recovery",
        "session",
        "the recovery ledger",
    ),
    (
        "exceptions:",
        "base.session.exceptions",
        "session",
        "open exceptions",
    ),
];

fn named(label: &str) -> Option<(&'static str, &'static str, &'static str)> {
    ROWS.iter()
        .find(|(l, ..)| *l == label)
        .map(|(_, id, family, title)| (*id, *family, *title))
}

/// What `aterm doctor` was asked.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Args {
    /// `--read-only`: probe and plan, change nothing.
    pub read_only: bool,
    /// `--json`: the rows in the one report schema, or a settings action's fleet document.
    pub json: bool,
    /// `--verbose`: every row, the ok and not-applicable ones too, and the readiness counts;
    /// without it the report is one verdict line and the rows that need the person. With
    /// `--check`, every settings row, the ones in place too.
    pub verbose: bool,
    /// `--only <id,...>`: the items, families or layers to run ([`aterm_converge::select`]).
    pub only: Vec<String>,
    /// `--check`, `--apply` or `--restore`: the fleet settings profile alone, none of the items.
    pub settings: Option<Settings>,
}

/// A settings-only action on the macOS fleet settings profile: no item runs and no window opens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Settings {
    /// `--check`: read the profile's settings, change nothing.
    Check,
    /// `--apply`: back up, apply and read back the profile.
    Apply,
    /// `--restore`: restore the backed-up values.
    Restore,
}

/// `aterm doctor`'s arguments.
/// # Errors
/// The refusal's one line.
pub fn args(rest: &[String]) -> Result<Args, String> {
    let refuse =
        |problem: String| format!("aterm doctor: {problem} — usage: aterm doctor {GRAMMAR}");
    let mut parsed = Args::default();
    let mut only_given = false;
    let mut rest = rest.iter();
    while let Some(arg) = rest.next() {
        let action = match arg.as_str() {
            "--read-only" => Some(None),
            "--check" => Some(Some(Settings::Check)),
            "--apply" => Some(Some(Settings::Apply)),
            "--restore" => Some(Some(Settings::Restore)),
            _ => None,
        };
        if let Some(settings) = action {
            if parsed.read_only || parsed.settings.is_some() {
                let repeated =
                    parsed.settings == settings && parsed.read_only == settings.is_none();
                let problem = if repeated {
                    format!("unknown or repeated argument {arg:?}")
                } else {
                    "choose one action".to_owned()
                };
                return Err(refuse(problem));
            }
            parsed.read_only = settings.is_none();
            parsed.settings = settings;
            continue;
        }
        let only = match arg.as_str() {
            "--json" if !parsed.json => {
                parsed.json = true;
                continue;
            }
            "--verbose" if !parsed.verbose => {
                parsed.verbose = true;
                continue;
            }
            "--only" if !only_given => match rest.next() {
                Some(value) => value.as_str(),
                None => return Err(refuse("--only needs a list of items".to_owned())),
            },
            _ => match arg.strip_prefix("--only=") {
                Some(value) if !only_given => value,
                _ => return Err(refuse(format!("unknown or repeated argument {arg:?}"))),
            },
        };
        only_given = true;
        let tokens: Vec<String> = only
            .split(',')
            .map(str::trim)
            .filter(|token| !token.is_empty())
            .map(str::to_owned)
            .collect();
        if tokens.is_empty() {
            return Err(refuse("--only needs a list of items".to_owned()));
        }
        parsed.only = tokens;
    }
    if parsed.settings.is_some() && only_given {
        return Err(refuse(
            "--only narrows the items, not a settings action".to_owned(),
        ));
    }
    if parsed.verbose && matches!(parsed.settings, Some(Settings::Apply | Settings::Restore)) {
        return Err(refuse(
            "--verbose widens a health run's report or --check's, not --apply or --restore"
                .to_owned(),
        ));
    }
    Ok(parsed)
}

/// Why `action` may not run for whoever is at this process ([`aterm_machines::console`]):
/// `--apply` and `--restore` change the person's own Mac, so only a person at its console runs
/// them, never an SSH session, an agent or a run without a terminal; `--check` changes nothing.
/// `presence` is read only for an action that changes the Mac.
#[cfg(target_os = "macos")]
fn needs_person(
    action: Settings,
    presence: impl FnOnce() -> aterm_machines::console::Presence,
) -> Option<String> {
    (action != Settings::Check)
        .then(|| presence().needs_person())
        .flatten()
}

/// A settings action's report and exit ([`Settings`]); `verbose` lists every row of the text.
#[must_use]
pub fn settings(action: Settings, json: bool, verbose: bool) -> (String, u8) {
    #[cfg(target_os = "macos")]
    {
        use super::macos_doctor::{self, Action};
        if let Some(why) = needs_person(action, aterm_machines::console::here) {
            return settings_refused(json, &why);
        }
        let action = match action {
            Settings::Check => Action::Check,
            Settings::Apply => Action::Apply,
            Settings::Restore => Action::Restore,
        };
        match macos_doctor::run(action) {
            Ok(report) => {
                let code = u8::from(report.needs_attention());
                if !json {
                    let text = if verbose {
                        report.to_text_verbose()
                    } else {
                        report.to_text()
                    };
                    return (text, code);
                }
                match aterm_json::from_str::<aterm_json::Value>(&report.to_json()) {
                    Ok(settings) => {
                        let doc = aterm_json::json!({
                            "document": "aterm-doctor", "version": 1,
                            "diagnostics": "", "settings": settings,
                            "exit_code": code,
                        });
                        (format!("{doc}\n"), code)
                    }
                    Err(why) => {
                        settings_refused(json, &format!("could not encode settings report: {why}"))
                    }
                }
            }
            Err(why) => settings_refused(json, &why),
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (action, verbose);
        settings_refused(json, "the fleet settings profile requires macOS")
    }
}

fn settings_refused(json: bool, why: &str) -> (String, u8) {
    if json {
        let doc = aterm_json::json!({
            "document": "aterm-doctor", "version": 1,
            "error": why, "exit_code": 1,
        });
        (format!("{doc}\n"), 1)
    } else {
        (format!("aterm doctor: {why}\n"), 1)
    }
}

/// A row beside the items' own: the packages' summary, the repair's (`crates/aterm`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    /// The left column, such as `packages:`.
    pub label: &'static str,
    /// The verdict; never [`Mark::Fail`], which only the shell gives.
    pub mark: Mark,
    /// The fact; later lines sit under the fact column.
    pub fact: String,
}

/// What `aterm doctor` read of this machine: its rows, the stale-copy item, and the Developer
/// Tools hand-over due after the report.
pub struct Gathered {
    lines: Vec<DoctorRow>,
    copies: Option<Copies>,
    hand_over: Option<super::Missing>,
    /// Spotlight indexes `rust-docs` pages ([`Self::rust_docs_indexed`]).
    rust_docs: bool,
}

/// The hand-over a fix-mode gather left due ([`gather`]): Full Disk Access or Developer Tools
/// reads off and a person is at this terminal. [`answer`] makes it after the report, through
/// the aterm window's permission walk where one runs.
#[derive(Debug)]
pub struct HandOver(super::Missing);

impl Gathered {
    /// The facts already read, as the pure report's tests build them.
    #[cfg(test)]
    pub(crate) fn of_rows(lines: Vec<DoctorRow>) -> Self {
        Self {
            lines,
            copies: None,
            hand_over: None,
            rust_docs: false,
        }
    }

    /// Facts as a composing crate's tests give them (`crates/aterm`'s doctor).
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub fn of_facts(facts: &[(&str, Mark, &str)]) -> Self {
        Self {
            lines: facts
                .iter()
                .filter_map(|(label, mark, fact)| {
                    ROWS.iter()
                        .find(|(l, ..)| l == label)
                        .map(|(l, ..)| DoctorRow {
                            label: l,
                            mark: *mark,
                            fact: (*fact).to_owned(),
                            unmeasured: false,
                        })
                })
                .collect(),
            copies: None,
            hand_over: None,
            rust_docs: false,
        }
    }

    /// These facts with Spotlight found indexing `rust-docs` pages, as a composing crate's tests
    /// give them.
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub const fn with_rust_docs_indexed(mut self) -> Self {
        self.rust_docs = true;
        self
    }

    /// Whether the gather found Spotlight indexing `rust-docs` pages: bare `aterm doctor` then
    /// sets them aside. `false` when the `spotlight:` row was not read.
    #[must_use]
    pub const fn rust_docs_indexed(&self) -> bool {
        self.rust_docs
    }

    /// The hand-over due after the report, when the gather left one.
    #[must_use]
    pub fn hand_over(&self) -> Option<HandOver> {
        self.hand_over.clone().map(HandOver)
    }

    /// The rows as items, in report order: each fact, and the stale copies after the privacy rows.
    #[must_use]
    pub fn items(&self) -> Vec<Box<dyn Item + '_>> {
        let leading = self
            .lines
            .iter()
            .take_while(|line| {
                ["shell:", "privacy:", "debugger:", "devtools:"].contains(&line.label)
            })
            .count();
        let mut items: Vec<Box<dyn Item + '_>> = Vec::with_capacity(self.lines.len() + 1);
        for (at, line) in self.lines.iter().enumerate() {
            if at == leading
                && let Some(copies) = &self.copies
            {
                items.push(Box::new(CopiesItem(copies)));
            }
            if let Some((id, family, title)) = named(line.label) {
                items.push(Box::new(Fact {
                    line,
                    id,
                    family,
                    title,
                }));
            }
        }
        if self.lines.len() <= leading
            && let Some(copies) = &self.copies
        {
            items.push(Box::new(CopiesItem(copies)));
        }
        items
    }

    /// The items through the engine, planned and never applied: the pure report's tests.
    #[cfg(test)]
    pub(crate) fn plan(&self) -> Vec<ItemReport> {
        aterm_converge::run_scoped(
            &self.items(),
            aterm_converge::Mode::Plan,
            aterm_converge::Scope::REPAIR,
            &mut aterm_converge::ReadOnly,
            &mut |_| {},
        )
        .unwrap_or_default()
    }
}

/// Read this machine's facts for `aterm doctor`, only the rows `only` names (the shell's
/// always: [`reads`]), each probe on a thread of its own, the stale copies' look among them;
/// it waits on no person. With `fix`, the Full Disk Access and Developer Tools hand-over is left
/// due for after the report ([`Gathered::hand_over`]).
#[must_use]
pub fn gather(fix: bool, only: &[String]) -> Gathered {
    let reads = |label: &str| reads(only, label);
    std::thread::scope(|scope| {
        let copies = (cfg!(target_os = "macos") && reads("copies:"))
            .then(|| scope.spawn(|| atpkg::copies::prune(true)));
        let read = super::doctor_facts_rows(fix, &reads);
        Gathered {
            lines: read.rows,
            rust_docs: read.rust_docs,
            copies: copies.map(|look| {
                let look = look
                    .join()
                    .unwrap_or_else(|panic| std::panic::resume_unwind(panic));
                Copies::new(atpkg::copies::prune, Some(look))
                    .pruning_after(atpkg::copies::prune_after)
            }),
            hand_over: read.hand_over,
        }
    })
}

/// The rows and the `health:` line, with its exit code; only tests call it, and
/// `aterm doctor` prints [`render_readiness`].
#[must_use]
pub fn render(gathered: &Gathered, rows: &[ItemReport], sections: &[Section]) -> (String, i32) {
    super::render_doctor_lines(&report_lines(gathered, rows, sections))
}

/// The rows (a failing shell row with its fix), then the `readiness:` and `scope:` lines (all
/// doctor checks, or the `--only` words plus shell usability) and any pending-activation and
/// unresolved ids.
#[must_use]
pub fn render_readiness(
    gathered: &Gathered,
    rows: &[ItemReport],
    sections: &[Section],
    readiness: &Readiness,
) -> String {
    let mut lines = report_lines(gathered, rows, sections);
    if let Some(shell) = lines
        .iter_mut()
        .find(|line| line.label == "shell:" && line.mark == Mark::Fail)
    {
        shell.fact.push_str("; ");
        shell.fact.push_str(super::SHELL_REMEDY);
    }
    let mut text = super::render_doctor_rows(&lines);
    if readiness.all_green {
        text.push_str("readiness: ALL GREEN\n");
    } else {
        text.push_str(&format!(
            "readiness: NOT READY — {} satisfied, {} not applicable, {} pending activation, {} \
             unresolved\n",
            readiness.satisfied,
            readiness.not_applicable,
            readiness.pending_activation.len(),
            readiness.unresolved.len(),
        ));
    }
    if !readiness.scope.is_empty() {
        text.push_str(&format!(
            "scope: {} (plus shell usability)\n",
            readiness.scope.join(", ")
        ));
    }
    if !readiness.pending_activation.is_empty() {
        text.push_str(&format!(
            "pending activation (login/restart): {}\n",
            readiness.pending_activation.join(", ")
        ));
    }
    if !readiness.unresolved.is_empty() {
        text.push_str(&format!(
            "unresolved: {}\n",
            readiness.unresolved.join(", ")
        ));
    }
    text
}

/// How long one row of bare `aterm doctor` may be, its label included.
pub const ROW_CHARS: usize = 160;

/// The label column: the longest label, `exceptions:`, and a space.
const LABEL_COLUMN: usize = "exceptions: ".len();

/// What a row bare `aterm doctor` shows is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Only the person changes it, or the check failed.
    NeedsYou,
    /// The shell aterm starts does not run: aterm cannot open a tab.
    Fails,
    /// The check could not read its state.
    Unread,
    /// This run changed it.
    Changed,
    /// A fact to see that asks nothing now (the open exceptions), or a repair that waits for
    /// another pass to finish (counted as waiting when it is unresolved).
    Note,
}

impl Kind {
    const fn word(self) -> &'static str {
        match self {
            Self::NeedsYou => "warn",
            Self::Fails => "FAIL",
            Self::Unread => "unread",
            Self::Changed => "done",
            Self::Note => "note",
        }
    }
}

/// One row bare `aterm doctor` shows: its check's id, label and kind, and one line of at most
/// [`ROW_CHARS`] that names one fix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shown {
    /// The row's id (`base.pkg.path`); a change has its item's.
    pub id: String,
    /// The left column, such as `devtools:`.
    pub label: &'static str,
    /// What the row is.
    pub kind: Kind,
    /// The whole printed line.
    pub line: String,
}

impl Shown {
    /// `text`'s first line under `label`, shortened to one row ([`shorten`]).
    #[must_use]
    pub fn new(id: impl Into<String>, label: &'static str, kind: Kind, text: &str) -> Self {
        let lead = format!("{label:<LABEL_COLUMN$}{:<6} ", kind.word());
        let room = ROW_CHARS.saturating_sub(lead.chars().count());
        Self {
            id: id.into(),
            label,
            kind,
            line: format!("{lead}{}", shorten(text, room)),
        }
    }
}

/// Where the one fix in a row's sentence starts: its `fix:` or `run:`, or the `(aterm help …)`
/// that names where it is told.
fn fix_at(text: &str) -> Option<usize> {
    ["fix: ", "run: ", "(aterm help "]
        .into_iter()
        .filter_map(|marker| text.find(marker))
        .min()
}

/// `text`'s first line in at most `room` characters: whole when it fits, else its first clause
/// and its one fix ([`fix_at`]) with what lies between left out, each cut with `…` when even
/// that is too long.
#[must_use]
pub fn shorten(text: &str, room: usize) -> String {
    let cut = |text: &str, room: usize| -> String {
        if text.chars().count() <= room {
            return text.to_owned();
        }
        let mut short: String = text.chars().take(room.saturating_sub(1)).collect();
        short.push('\u{2026}');
        short
    };
    let line = text.lines().next().unwrap_or_default().trim();
    if line.chars().count() <= room {
        return line.to_owned();
    }
    let Some(at) = fix_at(line) else {
        return cut(line, room);
    };
    let fix = line[at..].trim_end_matches([')', '.', ';']).trim();
    let fix = if fix.starts_with('(') {
        fix.trim_start_matches('(')
    } else {
        fix
    };
    let fix = fix
        .split(" \u{2014} ")
        .next()
        .unwrap_or(fix)
        .split("; ")
        .next()
        .unwrap_or(fix);
    let head = line[..at].trim_end_matches([' ', '(', '\u{2014}', ';', ',', ':']);
    let clause = head
        .split(" \u{2014} ")
        .next()
        .unwrap_or(head)
        .split("; ")
        .next()
        .unwrap_or(head)
        .trim();
    let joint = " \u{2014} ";
    let fix = cut(fix, room.saturating_sub(joint.len() + 20).max(20));
    let left = room.saturating_sub(fix.chars().count() + joint.chars().count());
    if clause.is_empty() || left < 10 {
        return cut(&fix, room);
    }
    format!("{}{joint}{fix}", cut(clause, left))
}

/// The verdict line bare `aterm doctor` opens with: `healthy` alone, or how many rows need the
/// person, how many could not be read, how many wait for another pass and which changes wait
/// for a login or restart, then how many checks are fine; a single change rides on the line,
/// more stand as rows of their own.
#[must_use]
pub fn verdict(readiness: &Readiness, shown: &[Shown], change: Option<&str>) -> String {
    let ok = readiness.satisfied + readiness.not_applicable;
    let count = |kind: Kind| shown.iter().filter(|row| row.kind == kind).count();
    let needs = count(Kind::NeedsYou) + count(Kind::Fails);
    let unread = count(Kind::Unread);
    let waiting = shown
        .iter()
        .filter(|row| row.kind == Kind::Note && readiness.unresolved.contains(&row.id))
        .count();
    let pending = readiness.pending_activation.len();
    let checks = format!("{ok} check{} ok", if ok == 1 { "" } else { "s" });
    let mut line = if readiness.all_green {
        "aterm doctor: healthy".to_owned()
    } else {
        let mut parts = Vec::new();
        if needs > 0 {
            parts.push(format!(
                "{needs} need{} you",
                if needs == 1 { "s" } else { "" }
            ));
        }
        if unread > 0 {
            parts.push(format!("{unread} not measured"));
        }
        if waiting > 0 {
            parts.push(format!("{waiting} waiting for another pass"));
        }
        if pending > 0 {
            parts.push(format!(
                "{pending} change{} take{} effect at your next login or restart ({})",
                if pending == 1 { "" } else { "s" },
                if pending == 1 { "s" } else { "" },
                readiness.pending_activation.join(", ")
            ));
        }
        if parts.is_empty() {
            parts.push(format!("{} unresolved", readiness.unresolved.len()));
        }
        format!("aterm doctor: {} \u{2014} {checks}", parts.join(", "))
    };
    if let Some(change) = change {
        let room = ROW_CHARS.saturating_sub(line.chars().count() + 2);
        line.push_str("; ");
        line.push_str(&shorten(change, room.max(20)));
    }
    line
}

/// Bare `aterm doctor`'s report: the [`verdict`], the scope when `--only` narrowed it, then one
/// row per check that needs the person or could not be read, the open exceptions, and each
/// change when there is more than one.
/// `more` holds the rows of the repairs, the develop layer and the packages; `changes` what the
/// repairs changed. The facts' rows are this function's own.
#[must_use]
pub fn render_brief(
    gathered: &Gathered,
    rows: &[ItemReport],
    more: &[Shown],
    readiness: &Readiness,
    changes: &[String],
) -> String {
    let mut shown: Vec<Shown> = Vec::new();
    let mut changed: Vec<String> = Vec::new();
    for line in report_lines(gathered, rows, &[]) {
        let id = if line.label == "copies:" {
            COPIES
        } else {
            named(line.label).map_or("", |(id, ..)| id)
        };
        let outcome = rows.iter().find(|row| row.id == id).map(|row| row.outcome);
        if outcome == Some(Outcome::Changed) {
            changed.push(line.fact.lines().next().unwrap_or_default().to_owned());
            continue;
        }
        let kind = if !readiness.unresolved.iter().any(|u| u == id) {
            if line.label == "exceptions:" && line.mark == Mark::Note && !line.unmeasured {
                Kind::Note
            } else {
                continue;
            }
        } else if line.unmeasured || outcome == Some(Outcome::Unknown) {
            Kind::Unread
        } else if line.mark == Mark::Fail {
            Kind::Fails
        } else {
            Kind::NeedsYou
        };
        let text = if kind == Kind::Fails {
            format!(
                "{} \u{2014} fix: {}",
                line.fact.lines().next().unwrap_or_default(),
                super::SHELL_REMEDY
            )
        } else {
            line.fact.clone()
        };
        shown.push(Shown::new(id, line.label, kind, &text));
    }
    shown.extend(more.iter().cloned());
    changed.extend(changes.iter().cloned());
    let inline = (changed.len() == 1).then(|| changed[0].as_str());
    let mut text = verdict(readiness, &shown, inline);
    text.push('\n');
    if !readiness.scope.is_empty() {
        text.push_str(&format!(
            "scope: {} (plus shell usability)\n",
            readiness.scope.join(", ")
        ));
    }
    for row in &shown {
        text.push_str(&row.line);
        text.push('\n');
    }
    if changed.len() > 1 {
        for change in &changed {
            text.push_str(&Shown::new("", "changed:", Kind::Changed, change).line);
            text.push('\n');
        }
    }
    text
}

fn report_lines(gathered: &Gathered, rows: &[ItemReport], sections: &[Section]) -> Vec<DoctorRow> {
    let mut lines: Vec<DoctorRow> = Vec::new();
    for row in rows {
        if row.id == COPIES
            && let Some(copies) = &gathered.copies
        {
            let said = atpkg::copies::row(&copies.shown());
            lines.push(DoctorRow {
                label: "copies:",
                mark: super::mark_of_level(said.level),
                fact: said.text,
                unmeasured: row.outcome == Outcome::Unknown,
            });
            continue;
        }
        let Some(line) = gathered
            .lines
            .iter()
            .find(|line| named(line.label).is_some_and(|(id, ..)| id == row.id))
        else {
            continue;
        };
        lines.push(DoctorRow {
            label: line.label,
            mark: if line.unmeasured {
                line.mark
            } else {
                mark_of(row)
            },
            fact: line.fact.clone(),
            unmeasured: line.unmeasured,
        });
    }
    lines.extend(sections.iter().map(|section| DoctorRow {
        label: section.label,
        mark: section.mark,
        fact: section.fact.clone(),
        unmeasured: false,
    }));
    lines
}

/// Print `aterm doctor`'s answer, spelled for the shell that reads it, then make the
/// hand-over the gather left due ([`HandOver`]). A report (`stdout`: the text,
/// the JSON, a settings action's) goes to stdout whatever its code, which carries the verdict
/// (`aterm doctor && aterm`), so `aterm doctor | less` and a captured stdout read it; only a
/// refusal goes to stderr. The hand-over follows the report and never holds it back; its
/// lines go to stderr.
#[must_use]
pub fn answer(
    text: &str,
    code: u8,
    stdout: bool,
    hand_over: Option<HandOver>,
) -> std::process::ExitCode {
    let status = answer_over(
        text,
        code,
        stdout,
        hand_over.as_ref().map(|HandOver(missing)| missing),
        &Ends {
            stdout: &|text| aterm_uds::stdout::answer(&super::cli_text::for_stdout(text)),
            stderr: &|text| eprint!("{}", super::cli_text::for_stderr(text)),
            hand_over: &super::permissions_hand_over_live,
        },
    );
    std::process::ExitCode::from(status)
}

/// Where [`answer`] writes and what it runs after: injected, so a test reads both streams and
/// scripts the hand-over.
pub(crate) struct Ends<'a> {
    /// Write the report whole to stdout ([`aterm_uds::stdout::answer`]).
    pub(crate) stdout: &'a dyn Fn(&str) -> std::io::Result<()>,
    /// Write to stderr.
    pub(crate) stderr: &'a dyn Fn(&str),
    /// The hand-over ([`super::permissions_hand_over_live`]).
    pub(crate) hand_over: &'a dyn Fn(&super::Missing),
}

/// [`answer`] over `ends`: the text, then the hand-over; the exit status, which is `code`
/// unless the report could not be written ([`aterm_uds::stdout::answer_status`]: 1, and its
/// line on stderr).
pub(crate) fn answer_over(
    text: &str,
    code: u8,
    stdout: bool,
    hand_over: Option<&super::Missing>,
    ends: &Ends<'_>,
) -> u8 {
    let status = if stdout {
        aterm_uds::stdout::answer_status("aterm doctor", (ends.stdout)(text), code).unwrap_or_else(
            |line| {
                (ends.stderr)(&format!("{line}\n"));
                1
            },
        )
    } else {
        (ends.stderr)(text);
        code
    };
    if let Some(missing) = hand_over {
        (ends.hand_over)(missing);
    }
    status
}

const fn mark_of(row: &ItemReport) -> Mark {
    match row.outcome {
        Outcome::Ok | Outcome::Changed => Mark::Ok,
        Outcome::NotApplicable | Outcome::Deferred => Mark::Note,
        _ if row.essential && row.fails_surface() => Mark::Fail,
        _ => Mark::Warn,
    }
}

struct Fact<'a> {
    line: &'a DoctorRow,
    id: &'static str,
    family: &'static str,
    title: &'static str,
}

impl Item for Fact<'_> {
    fn id(&self) -> String {
        self.id.to_owned()
    }

    fn family(&self) -> &'static str {
        self.family
    }

    fn layer(&self) -> Layer {
        Layer::Base
    }

    fn class(&self) -> Class {
        Class::Person
    }

    fn title(&self) -> String {
        self.title.to_owned()
    }

    fn probe(&self) -> Probe {
        let fact = self.line.fact.clone();
        if self.line.unmeasured {
            return Probe::Unusable { why: fact };
        }
        match self.line.mark {
            Mark::Ok => Probe::Ok { now: fact },
            Mark::Note => Probe::NotApplicable { why: fact },
            Mark::Warn | Mark::Fail => Probe::Person {
                step: self.id.to_owned(),
                how: fact,
            },
        }
    }

    fn apply(&self) -> Result<(), String> {
        Err(format!(
            "`aterm doctor` reports {}; it changes nothing here",
            self.title
        ))
    }

    fn explain(&self, _: &Probe) -> Explain {
        Explain {
            why: self.line.fact.clone(),
            remedy: if self.id == SHELL {
                Remedy::Person {
                    pane: "your shell".to_owned(),
                    how: super::SHELL_REMEDY.to_owned(),
                }
            } else {
                Remedy::None
            },
        }
    }

    fn essential(&self) -> bool {
        self.id == SHELL
    }
}

struct Copies {
    prune: fn(bool) -> atpkg::copies::Pruned,
    /// The prune that follows the probe's look, moving what it found with only the census the
    /// moves are checked against ([`atpkg::copies::prune_after`]); without one the apply prunes
    /// from a census of its own.
    after: Option<fn(&atpkg::copies::Pruned) -> atpkg::copies::Pruned>,
    /// The look [`gather`] took beside the other facts, which the first probe takes instead of
    /// looking again.
    early: Mutex<Option<atpkg::copies::Pruned>>,
    looked: Mutex<Option<atpkg::copies::Pruned>>,
    did: Mutex<Option<atpkg::copies::Pruned>>,
}

/// The stale copies of aterm as an item outside a gather, which a launch applies by itself:
/// looked at with `prune(true)` and moved by `after` that look, else by `prune(false)`
/// ([`atpkg::copies::prune`] and [`atpkg::copies::prune_after`], or a test's).
pub struct StaleCopies(Copies);

impl StaleCopies {
    #[must_use]
    pub fn new(
        prune: fn(bool) -> atpkg::copies::Pruned,
        after: Option<fn(&atpkg::copies::Pruned) -> atpkg::copies::Pruned>,
    ) -> Self {
        let mut copies = Copies::new(prune, None);
        copies.after = after;
        Self(copies)
    }

    /// The [`COPIES`] item.
    #[must_use]
    pub fn item(&self) -> Box<dyn Item + '_> {
        Box::new(CopiesItem(&self.0))
    }

    /// What the last prune or look says ([`atpkg::copies::row`]).
    #[must_use]
    pub fn said(&self) -> String {
        atpkg::copies::row(&self.0.shown()).text
    }
}

impl Copies {
    fn new(prune: fn(bool) -> atpkg::copies::Pruned, early: Option<atpkg::copies::Pruned>) -> Self {
        Self {
            prune,
            after: None,
            early: Mutex::new(early),
            looked: Mutex::new(None),
            did: Mutex::new(None),
        }
    }

    /// Prune with `after` once a look was taken ([`Copies::after`]).
    fn pruning_after(mut self, after: fn(&atpkg::copies::Pruned) -> atpkg::copies::Pruned) -> Self {
        self.after = Some(after);
        self
    }

    fn shown(&self) -> atpkg::copies::Pruned {
        let did = self
            .did
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        did.or_else(|| {
            self.looked
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone()
        })
        .unwrap_or_else(|| atpkg::copies::Pruned::NotLooked("not looked at".to_owned()))
    }
}

struct CopiesItem<'a>(&'a Copies);

impl Item for CopiesItem<'_> {
    fn id(&self) -> String {
        COPIES.to_owned()
    }

    fn family(&self) -> &'static str {
        "pkg"
    }

    fn layer(&self) -> Layer {
        Layer::Base
    }

    fn class(&self) -> Class {
        Class::Asked
    }

    fn title(&self) -> String {
        "stale copies of aterm".to_owned()
    }

    fn probe(&self) -> Probe {
        use atpkg::copies::Pruned;
        let early = self
            .0
            .early
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        let look = early.unwrap_or_else(|| (self.0.prune)(true));
        let said = atpkg::copies::row(&look);
        let probe = match &look {
            Pruned::NotLooked(_) | Pruned::Incomplete { .. } => Probe::Unusable { why: said.text },
            Pruned::Found { offered, .. } if !offered.is_empty() => Probe::Change {
                from: format!(
                    "{} stale cop{} of aterm",
                    offered.len(),
                    if offered.len() == 1 { "y" } else { "ies" }
                ),
                to: "in the Trash".to_owned(),
            },
            Pruned::Found {
                complete: false, ..
            } => Probe::Unusable { why: said.text },
            Pruned::Found { .. } | Pruned::Done { .. } => Probe::Ok { now: said.text },
        };
        *self
            .0
            .looked
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(look);
        probe
    }

    fn apply(&self) -> Result<(), String> {
        let looked = self
            .0
            .looked
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let pruned = match (self.0.after, looked) {
            (Some(after), Some(look)) => after(&look),
            _ => (self.0.prune)(false),
        };
        let said = atpkg::copies::row(&pruned);
        *self
            .0
            .did
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(pruned);
        if said.level == atpkg::doctor::Level::Warn {
            Err(said.text)
        } else {
            Ok(())
        }
    }

    fn explain(&self, _: &Probe) -> Explain {
        Explain {
            why: atpkg::copies::row(&self.0.shown()).text,
            remedy: Remedy::None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(args: &[&str]) -> Vec<String> {
        args.iter().map(ToString::to_string).collect()
    }

    /// `--apply` and `--restore` run only for a person at this Mac's console, refused in the
    /// presence's one line otherwise; `--check` runs for anyone and reads nobody's presence.
    #[cfg(target_os = "macos")]
    #[test]
    fn only_a_person_at_the_console_applies_or_restores_the_settings() {
        use aterm_machines::console::Presence;
        for action in [Settings::Apply, Settings::Restore] {
            assert_eq!(needs_person(action, || Presence::AtConsole), None);
            for away in [
                Presence::Remote,
                Presence::NoTerminal,
                Presence::AgentTab,
                Presence::OtherLogin,
                Presence::NoConsole,
            ] {
                let why = needs_person(action, || away).expect("refused");
                assert_eq!(Some(why), away.needs_person());
            }
        }
        assert_eq!(
            needs_person(Settings::Check, || unreachable!(
                "a check reads no presence"
            )),
            None
        );
    }

    #[test]
    fn the_grammar_takes_one_action_json_and_only_each_once() {
        assert_eq!(args(&[]), Ok(Args::default()));
        assert_eq!(
            args(&strings(&[
                "--read-only",
                "--json",
                "--only",
                "pkg,base.shell.rc"
            ])),
            Ok(Args {
                read_only: true,
                json: true,
                verbose: false,
                only: strings(&["pkg", "base.shell.rc"]),
                settings: None,
            })
        );
        assert_eq!(
            args(&strings(&["--verbose", "--read-only"])).map(|a| (a.verbose, a.read_only)),
            Ok((true, true))
        );
        assert_eq!(
            args(&strings(&["--check", "--verbose"])).map(|a| (a.settings, a.verbose)),
            Ok((Some(Settings::Check), true))
        );
        for (flag, action) in [
            ("--check", Settings::Check),
            ("--apply", Settings::Apply),
            ("--restore", Settings::Restore),
        ] {
            for argv in [&[flag][..], &[flag, "--json"], &["--json", flag]] {
                assert_eq!(
                    args(&strings(argv)).map(|a| (a.settings, a.read_only)),
                    Ok((Some(action), false)),
                    "{argv:?}"
                );
            }
        }
        assert_eq!(
            args(&strings(&["--only=shell"])).map(|a| a.only),
            Ok(strings(&["shell"]))
        );
        for refused in [
            &["--json", "--json"][..],
            &["--only"],
            &["--only", ","],
            &["--only", "pkg", "--only", "shell"],
            &["--fix"],
            &["pkg"],
            &["--check", "--apply"],
            &["--read-only", "--restore"],
            &["--apply", "--apply"],
            &["--read-only", "--read-only"],
            &["--check", "--only", "pkg"],
            &["--only=pkg", "--restore"],
            &["--verbose", "--verbose"],
            &["--apply", "--verbose"],
            &["--restore", "--verbose"],
            &["--check", "--verbose", "--verbose"],
        ] {
            let refusal = args(&strings(refused)).unwrap_err();
            assert!(
                refusal.ends_with(&format!("usage: aterm doctor {GRAMMAR}")),
                "{refused:?}: {refusal}"
            );
        }
    }

    #[test]
    fn a_refused_settings_action_answers_in_the_fleet_document() {
        let (text, code) = settings_refused(true, "the fleet settings profile requires macOS");
        assert_eq!(code, 1);
        let doc: aterm_json::Value = aterm_json::from_str(&text).unwrap();
        assert_eq!(
            doc,
            aterm_json::json!({
                "document": "aterm-doctor", "version": 1,
                "error": "the fleet settings profile requires macOS", "exit_code": 1,
            })
        );
        assert_eq!(
            settings_refused(false, "no"),
            ("aterm doctor: no\n".to_owned(), 1)
        );
    }

    #[test]
    fn the_stale_copies_are_looked_at_by_the_probe_and_moved_only_by_the_apply() {
        use atpkg::copies::Pruned;
        use std::path::PathBuf;
        static MOVED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        fn prune(dry_run: bool) -> Pruned {
            let copy = PathBuf::from("/Applications/aterm 2.app");
            if dry_run {
                Pruned::Found {
                    offered: if MOVED.load(std::sync::atomic::Ordering::SeqCst) {
                        Vec::new()
                    } else {
                        vec![copy]
                    },
                    kept: Vec::new(),
                    complete: true,
                }
            } else {
                MOVED.store(true, std::sync::atomic::Ordering::SeqCst);
                Pruned::Done {
                    moved: vec![(copy, aterm_containment::consent::Retired::Moved)],
                    kept: Vec::new(),
                }
            }
        }
        let gathered = Gathered {
            lines: Vec::new(),
            copies: Some(Copies::new(prune, None)),
            hand_over: None,
            rust_docs: false,
        };
        let items = gathered.items();
        let planned = aterm_converge::run_scoped(
            &items,
            aterm_converge::Mode::Plan,
            aterm_converge::Scope::REPAIR,
            &mut aterm_converge::ReadOnly,
            &mut |_| {},
        )
        .unwrap();
        assert_eq!(planned[0].outcome, Outcome::Planned);
        assert!(
            !MOVED.load(std::sync::atomic::Ordering::SeqCst),
            "a plan moves nothing"
        );
        let applied = aterm_converge::run_scoped(
            &items,
            aterm_converge::Mode::Apply,
            aterm_converge::Scope::REPAIR,
            &mut aterm_converge::NoJournal,
            &mut |_| {},
        )
        .unwrap();
        assert_eq!(applied[0].outcome, Outcome::Changed, "{applied:?}");
        let (report, code) = render(&gathered, &applied, &[]);
        assert_eq!(code, 0);
        let line = report
            .lines()
            .find(|l| l.starts_with("copies:"))
            .unwrap_or_else(|| panic!("{report}"));
        assert!(line.contains("aterm 2.app"), "{report}");
    }

    /// A HEALTHY RUN IS ONE LINE: the verdict, with no counts; a run that changed one thing says
    /// it on that line. NEGATIVE CONTROL: one warning adds its one row, at most [`ROW_CHARS`]
    /// long, with its fix, and the verdict counts it and the checks that passed.
    #[test]
    fn a_healthy_report_is_one_line_and_a_warning_adds_its_row() {
        let healthy = Gathered::of_facts(&[
            ("shell:", Mark::Ok, "/bin/zsh (executable)"),
            ("privacy:", Mark::Ok, "Full Disk Access granted"),
            ("fabric:", Mark::Note, "off: no [fabric] command"),
            ("exceptions:", Mark::Ok, "none open"),
        ]);
        let rows = healthy.plan();
        let readiness = Readiness::of(&rows, &[]);
        let text = render_brief(&healthy, &rows, &[], &readiness, &[]);
        assert_eq!(text, "aterm doctor: healthy\n");
        let one_change = render_brief(
            &healthy,
            &rows,
            &[],
            &readiness,
            &["moved 1 stale copy of aterm to the Trash".to_owned()],
        );
        assert_eq!(
            one_change,
            "aterm doctor: healthy; moved 1 stale copy of aterm to the Trash\n"
        );
        assert!(one_change.lines().count() <= 2);

        let devtools = format!(
            "aterm is switched off on Privacy & Security \u{25b8} Developer Tools \u{2014} macOS \
             holds every new program it starts for a first-run check (syspolicyd), one at a \
             time, so builds and tests wait and timing tests flake{} \u{2014} fix: switch on \
             aterm's row under Developer Tools (`aterm doctor` opens the pane for you, or `open \
             x-apple.systempreferences:...`; macOS asks for your password)",
            "; and more".repeat(20)
        );
        let warned = Gathered::of_facts(&[
            ("shell:", Mark::Ok, "/bin/zsh (executable)"),
            ("devtools:", Mark::Warn, &devtools),
            ("exceptions:", Mark::Ok, "none open"),
        ]);
        let rows = warned.plan();
        let readiness = Readiness::of(&rows, &[]);
        let text = render_brief(&warned, &rows, &[], &readiness, &[]);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2, "{text}");
        assert_eq!(lines[0], "aterm doctor: 1 needs you \u{2014} 2 checks ok");
        assert!(
            lines[1].starts_with("devtools:   warn   aterm is switched off"),
            "{text}"
        );
        assert!(lines[1].contains("fix: switch on aterm's row"), "{text}");
        assert!(
            lines[1].chars().count() <= ROW_CHARS,
            "{}",
            lines[1].chars().count()
        );
        assert!(devtools.chars().count() > ROW_CHARS);
    }

    /// The open exceptions show in the brief report by title, and an unread check is named as
    /// such and counted apart.
    #[test]
    fn open_exceptions_and_unread_checks_show_in_the_brief_report() {
        let gathered = Gathered::of_facts(&[
            ("shell:", Mark::Ok, "/bin/zsh (executable)"),
            (
                "exceptions:",
                Mark::Note,
                "2 open: disk low; update held (aterm exceptions)",
            ),
        ]);
        let rows = gathered.plan();
        let readiness = Readiness::of(&rows, &[]);
        let text = render_brief(&gathered, &rows, &[], &readiness, &[]);
        assert!(text.starts_with("aterm doctor: healthy"), "{text}");
        assert!(
            text.lines().any(|l| l
                == "exceptions: note   2 open: disk low; update held (aterm exceptions)"),
            "{text}"
        );
        let unread = Shown::new(
            "base.pkg.disk",
            "packages:",
            Kind::Unread,
            "could not query free space",
        );
        let readiness = Readiness {
            all_green: false,
            unresolved: vec!["base.pkg.disk".to_owned()],
            ..readiness
        };
        let text = render_brief(&gathered, &rows, &[unread], &readiness, &[]);
        assert!(
            text.starts_with("aterm doctor: 1 not measured \u{2014} "),
            "{text}"
        );
        assert!(
            text.contains("packages:   unread could not query free space"),
            "{text}"
        );
    }

    /// A row too long for one line keeps its first clause and its one fix. NEGATIVE CONTROL: a
    /// line that fits is left whole, and one with no fix is cut.
    #[test]
    fn a_long_row_keeps_its_first_clause_and_its_fix() {
        let long = format!(
            "managed bin/ is not on PATH here{} \u{2014} fix: open a new aterm tab",
            ", because of a reason".repeat(10)
        );
        let short = shorten(&long, 80);
        assert_eq!(
            short,
            "managed bin/ is not on PATH here, because of a reas\u{2026} \u{2014} fix: open a new \
             aterm tab"
        );
        assert!(short.chars().count() <= 80, "{short}");
        assert_eq!(shorten("fits", 80), "fits");
        let plain = "x".repeat(100);
        assert_eq!(shorten(&plain, 10).chars().count(), 10);
        let help = format!(
            "no full disk access for the responsible app (Terminal){} (aterm help permissions)",
            "; and so on".repeat(12)
        );
        let short = shorten(&help, 100);
        assert!(short.ends_with("aterm help permissions"), "{short}");
        assert!(
            short.starts_with("no full disk access for the responsible app"),
            "{short}"
        );
        assert_eq!(shorten("one\ntwo", 80), "one", "the first line alone");
    }

    /// THE APPLY MOVES WHAT THE PROBE'S LOOK FOUND: with a prune-after, the apply hands it the
    /// look and never prunes from a census of its own. NEGATIVE CONTROL: without one, the apply
    /// prunes afresh.
    #[test]
    fn the_apply_prunes_from_the_look_the_probe_took() {
        use atpkg::copies::Pruned;
        use std::path::PathBuf;
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst};
        static OWN: AtomicUsize = AtomicUsize::new(0);
        static AFTER: AtomicUsize = AtomicUsize::new(0);
        static MOVED: AtomicBool = AtomicBool::new(false);
        fn copy() -> PathBuf {
            PathBuf::from("/Applications/aterm 3.app")
        }
        fn prune(dry_run: bool) -> Pruned {
            if dry_run {
                return Pruned::Found {
                    offered: if MOVED.load(SeqCst) {
                        Vec::new()
                    } else {
                        vec![copy()]
                    },
                    kept: Vec::new(),
                    complete: true,
                };
            }
            OWN.fetch_add(1, SeqCst);
            MOVED.store(true, SeqCst);
            Pruned::Done {
                moved: vec![(copy(), aterm_containment::consent::Retired::Moved)],
                kept: Vec::new(),
            }
        }
        fn after(look: &Pruned) -> Pruned {
            AFTER.fetch_add(1, SeqCst);
            MOVED.store(true, SeqCst);
            let Pruned::Found { offered, kept, .. } = look else {
                panic!("the probe's look: {look:?}");
            };
            assert_eq!(offered, &[copy()]);
            Pruned::Done {
                moved: offered
                    .iter()
                    .map(|path| (path.clone(), aterm_containment::consent::Retired::Moved))
                    .collect(),
                kept: kept.clone(),
            }
        }
        let apply = |copies: Copies| {
            MOVED.store(false, SeqCst);
            let gathered = Gathered {
                lines: Vec::new(),
                copies: Some(copies),
                hand_over: None,
                rust_docs: false,
            };
            let rows = aterm_converge::run_scoped(
                &gathered.items(),
                aterm_converge::Mode::Apply,
                aterm_converge::Scope::REPAIR,
                &mut aterm_converge::NoJournal,
                &mut |_| {},
            )
            .unwrap();
            assert_eq!(rows[0].outcome, Outcome::Changed, "{rows:?}");
        };
        apply(Copies::new(prune, None).pruning_after(after));
        assert_eq!(
            (AFTER.load(SeqCst), OWN.load(SeqCst)),
            (1, 0),
            "pruned from the look"
        );
        apply(Copies::new(prune, None));
        assert_eq!(
            (AFTER.load(SeqCst), OWN.load(SeqCst)),
            (1, 1),
            "no prune-after: afresh"
        );
    }

    #[test]
    fn an_incomplete_or_refused_copy_census_is_unknown_even_when_it_found_no_copies() {
        use atpkg::copies::Pruned;
        fn incomplete(_: bool) -> Pruned {
            Pruned::Found {
                offered: Vec::new(),
                kept: Vec::new(),
                complete: false,
            }
        }
        fn refused(_: bool) -> Pruned {
            Pruned::NotLooked("no readable bundle identifier".to_owned())
        }
        fn complete(_: bool) -> Pruned {
            Pruned::Found {
                offered: Vec::new(),
                kept: Vec::new(),
                complete: true,
            }
        }
        for (probe, expected) in [
            (incomplete as fn(bool) -> Pruned, Outcome::Unknown),
            (refused, Outcome::Unknown),
            (complete, Outcome::Ok),
        ] {
            let gathered = Gathered {
                lines: Vec::new(),
                copies: Some(Copies::new(probe, None)),
                hand_over: None,
                rust_docs: false,
            };
            let rows = gathered.plan();
            assert_eq!(rows[0].outcome, expected);
            assert_eq!(Readiness::of(&rows, &[]).all_green, expected == Outcome::Ok);
        }
    }

    /// THE COPIES' LOOK IS TAKEN ONCE, BESIDE THE OTHER FACTS: the probe takes the look the
    /// gather made on its own thread and looks no second time; a later probe looks afresh.
    /// NEGATIVE CONTROL: with no early look the probe looks itself.
    #[test]
    fn the_copies_probe_takes_the_look_the_gather_made() {
        use atpkg::copies::Pruned;
        static LOOKS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        fn look(_: bool) -> Pruned {
            LOOKS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Pruned::NotLooked("looked again".to_owned())
        }
        let early = Pruned::Found {
            offered: Vec::new(),
            kept: Vec::new(),
            complete: true,
        };
        let gathered = Gathered {
            lines: Vec::new(),
            copies: Some(Copies::new(look, Some(early))),
            hand_over: None,
            rust_docs: false,
        };
        assert_eq!(gathered.plan()[0].outcome, Outcome::Ok, "the early look");
        assert_eq!(LOOKS.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert_eq!(gathered.plan()[0].outcome, Outcome::Unknown, "a fresh look");
        assert_eq!(LOOKS.load(std::sync::atomic::Ordering::SeqCst), 1);
    }
}
