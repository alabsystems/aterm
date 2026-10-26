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
pub const GRAMMAR: &str =
    "[--read-only] [--json] [--only <id,...>] | (--check | --apply | --restore) [--json]";

const ROWS: [(&str, &str, &str, &str); 12] = [
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

/// A settings action's report and exit ([`Settings`]).
#[must_use]
pub fn settings(action: Settings, json: bool) -> (String, u8) {
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
                    return (report.to_text(), code);
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
        let _ = action;
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
    hand_over: Option<atpkg::devtools::Facts>,
}

/// The Developer Tools hand-over a fix-mode gather left due ([`gather`]): aterm is not granted
/// and a person is at this terminal. [`answer`] makes it after the report.
#[derive(Debug)]
pub struct HandOver(atpkg::devtools::Facts);

impl Gathered {
    /// The facts already read, as the pure report's tests build them.
    #[cfg(test)]
    pub(crate) fn of_rows(lines: Vec<DoctorRow>) -> Self {
        Self {
            lines,
            copies: None,
            hand_over: None,
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
        }
    }

    /// The Developer Tools hand-over due after the report, when the gather left one.
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
/// it waits on no person. With `fix`, the Developer Tools hand-over is left due for after the
/// report ([`Gathered::hand_over`]).
#[must_use]
pub fn gather(fix: bool, only: &[String]) -> Gathered {
    let reads = |label: &str| reads(only, label);
    std::thread::scope(|scope| {
        let copies = (cfg!(target_os = "macos") && reads("copies:"))
            .then(|| scope.spawn(|| atpkg::copies::prune(true)));
        let (lines, hand_over) = super::doctor_facts_rows(fix, &reads);
        Gathered {
            lines,
            copies: copies.map(|look| {
                let look = look
                    .join()
                    .unwrap_or_else(|panic| std::panic::resume_unwind(panic));
                Copies::new(atpkg::copies::prune, Some(look))
            }),
            hand_over,
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
    let scope = if readiness.scope.is_empty() {
        "all doctor checks".to_owned()
    } else {
        format!("{} (plus shell usability)", readiness.scope.join(", "))
    };
    text.push_str(&format!(
        "readiness: {} — {} satisfied, {} not applicable, {} pending activation, {} unresolved\nscope: {scope}\n",
        if readiness.all_green { "ALL GREEN" } else { "NOT READY" },
        readiness.satisfied,
        readiness.not_applicable,
        readiness.pending_activation.len(),
        readiness.unresolved.len(),
    ));
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
/// Developer Tools hand-over the gather left due ([`HandOver`]). A report (`stdout`: the text,
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
        hand_over.as_ref().map(|HandOver(facts)| facts),
        &Ends {
            stdout: &|text| aterm_uds::stdout::answer(&super::cli_text::for_stdout(text)),
            stderr: &|text| eprint!("{}", super::cli_text::for_stderr(text)),
            hand_over: &super::devtools_hand_over_live,
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
    /// The hand-over ([`super::devtools_hand_over_live`]).
    pub(crate) hand_over: &'a dyn Fn(&atpkg::devtools::Facts),
}

/// [`answer`] over `ends`: the text, then the hand-over; the exit status, which is `code`
/// unless the report could not be written ([`aterm_uds::stdout::answer_status`]: 1, and its
/// line on stderr).
pub(crate) fn answer_over(
    text: &str,
    code: u8,
    stdout: bool,
    hand_over: Option<&atpkg::devtools::Facts>,
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
    if let Some(facts) = hand_over {
        (ends.hand_over)(facts);
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
    /// The look [`gather`] took beside the other facts, which the first probe takes instead of
    /// looking again.
    early: Mutex<Option<atpkg::copies::Pruned>>,
    looked: Mutex<Option<atpkg::copies::Pruned>>,
    did: Mutex<Option<atpkg::copies::Pruned>>,
}

impl Copies {
    fn new(prune: fn(bool) -> atpkg::copies::Pruned, early: Option<atpkg::copies::Pruned>) -> Self {
        Self {
            prune,
            early: Mutex::new(early),
            looked: Mutex::new(None),
            did: Mutex::new(None),
        }
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
        let pruned = (self.0.prune)(false);
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
                only: strings(&["pkg", "base.shell.rc"]),
                settings: None,
            })
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
        };
        assert_eq!(gathered.plan()[0].outcome, Outcome::Ok, "the early look");
        assert_eq!(LOOKS.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert_eq!(gathered.plan()[0].outcome, Outcome::Unknown, "a fresh look");
        assert_eq!(LOOKS.load(std::sync::atomic::Ordering::SeqCst), 1);
    }
}
