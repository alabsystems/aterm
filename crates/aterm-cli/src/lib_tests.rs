// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

#[cfg(any(windows, target_os = "macos"))]
use super::doctor_report;
use super::driver;
#[cfg(windows)]
use super::show_config_report;
use super::{
    CliAction, DEBUGGER_ACCESS_REVERT, DIAG_COMMANDS, DebuggerFacts, DebuggerMember, DebuggerMode,
    DebuggerProbe, DebuggerProbeLabel, DevtoolsFacts, DrClass, ExceptionsFacts, FdaState,
    MachineFacts, Mark, PrivacyFacts, ProbeLabel, RecoveryFacts, SelfHardening, ShellCheck, Verb,
    agents_dir_mode, config_conpty_from_text, config_path_paragraph_with, decide_args, diag_report,
    doctor_checks_with_origin, exceptions_report_at, explain_config_report, front_door_alias,
    front_door_verb, help_text, is_tool_candidate, list_fonts_report, list_themes_alias,
    managed_agents_dir, prepend_path, read_exceptions_log, reroute_dir_from_values, session_limits,
    session_path_env, show_face_report, version_text,
};
#[cfg(unix)]
use super::{args_read_withheld, fabric_mark, unix_shell};

fn decide(args: &[&str]) -> CliAction {
    decide_args(args.iter().map(|s| s.to_string()))
}

fn unmeasured() -> PrivacyFacts {
    PrivacyFacts::not_measured(ProbeLabel::RefusedOutOfBundle)
}

fn no_debugger() -> DebuggerFacts {
    DebuggerFacts::not_measured(DebuggerProbeLabel::RefusedOutOfBundle)
}

fn no_devtools() -> DevtoolsFacts {
    DevtoolsFacts::absent()
}

fn debugger(mode: DebuggerMode, member: DebuggerMember, hardening: SelfHardening) -> DebuggerFacts {
    DebuggerFacts {
        probe: DebuggerProbe {
            mode,
            member,
            label: DebuggerProbeLabel::StatusOk,
        },
        hardening,
    }
}

fn no_recovery() -> RecoveryFacts {
    Ok((
        aterm_update::recovery_ledger::Checked::default(),
        std::path::PathBuf::from("/logs/recovery-ledger.log"),
    ))
}

fn no_exceptions() -> ExceptionsFacts {
    Ok((0, Vec::new()))
}

fn no_census() -> Result<aterm_ctl::census::Census, String> {
    Ok(aterm_ctl::census::Census {
        dir: "/run/aterm".to_string(),
        instances: 0,
        unreached: Vec::new(),
        programs: Vec::new(),
    })
}

/// Doctor's `network:` row is the place's summary as it came, a note away from the ALab LAN
/// and ok on it, and being away needs nobody: the plan's outcome for it is no failure and
/// the brief report stays healthy.
/// NEGATIVE CONTROL: leaving `network_rows` out of `doctor_rows` drops the row, and this test
/// fails at "the place's row".
#[test]
fn doctor_names_where_this_mac_is_on_the_network_and_away_needs_nobody() {
    let row_of = |away: bool| {
        let machine = MachineFacts {
            network: Some((away, "remote (phone hotspot) since 08:00".to_owned())),
            ..MachineFacts::absent()
        };
        super::doctor_rows(
            "/bin/sh",
            ShellCheck::Runs,
            None,
            &unmeasured(),
            &no_debugger(),
            &no_devtools(),
            &Err("not read".to_owned()),
            &no_recovery(),
            &no_exceptions(),
            &machine,
        )
        .into_iter()
        .find(|row| row.label == "network:")
    };
    let away = row_of(true).expect("the place's row");
    assert_eq!(
        (away.mark, away.fact.as_str()),
        (Mark::Note, "remote (phone hotspot) since 08:00")
    );
    assert_eq!(row_of(false).map(|row| row.mark), Some(Mark::Ok));
    assert!(super::doctor::names("base.machine.network"));
    let gathered = super::doctor::Gathered::of_facts(&[
        ("shell:", Mark::Ok, "/bin/zsh (executable)"),
        ("network:", Mark::Note, "remote (phone hotspot) since 08:00"),
    ]);
    let rows = gathered.plan();
    let readiness = aterm_converge::Readiness::of(&rows, &[]);
    let text = super::doctor::render_brief(&gathered, &rows, &[], &readiness, &[]);
    assert!(text.starts_with("aterm doctor: healthy"), "{text}");
}

#[test]
fn doctor_readiness_keeps_unmeasured_notes_unknown_and_advisories_nonfatal() {
    use aterm_converge::{Outcome, Readiness};
    let census = Err("process table unavailable".to_owned());
    let exceptions = Err("log unreadable".to_owned());
    let machine = MachineFacts {
        spotlight: Some(Err("index unavailable".to_owned())),
        program_check: Some(aterm_uds::program_check::Look::Unread),
        fabric: Some((Mark::Note, "withheld".to_owned(), true, None)),
        ..MachineFacts::absent()
    };
    let gathered = super::doctor::Gathered::of_rows(super::doctor_rows(
        "/bin/sh",
        ShellCheck::Runs,
        None,
        &unmeasured(),
        &no_debugger(),
        &no_devtools(),
        &census,
        &Err("ledger unreadable".to_owned()),
        &exceptions,
        &machine,
    ));
    let all = gathered.plan();
    for id in [
        "base.privacy.fda",
        "base.privacy.debugger",
        "base.session.instances",
        "base.session.exceptions",
        "base.machine.spotlight",
        "base.machine.syspolicyd",
        "base.session.fabric",
        "base.session.recovery",
    ] {
        if cfg!(windows) && id == "base.session.recovery" {
            continue;
        }
        let rows: Vec<_> = all
            .iter()
            .filter(|r| r.id == id || r.id == super::doctor::SHELL)
            .cloned()
            .collect();
        assert_eq!(
            rows.iter().find(|r| r.id == id).unwrap().outcome,
            Outcome::Unknown,
            "{id}"
        );
        let readiness = Readiness::of(&rows, &[id.to_owned()]);
        assert!(!readiness.all_green, "{id}");
        assert_eq!(readiness.unresolved, [id]);
        let text = super::doctor::render_readiness(&gathered, &rows, &[], &readiness);
        assert!(text.contains("readiness: NOT READY"), "{id}: {text}");
    }

    let gathered = super::doctor::Gathered::of_rows(super::doctor_rows(
        "/bin/sh",
        ShellCheck::Runs,
        None,
        &measured(FdaState::Denied, DrClass::Identity, "aterm"),
        &debugger(
            DebuggerMode::Disabled,
            DebuggerMember::Admin,
            SelfHardening::Hardened,
        ),
        &no_devtools(),
        &no_census(),
        &no_recovery(),
        &Ok((2, Vec::new())),
        &MachineFacts::absent(),
    ));
    let rows = gathered.plan();
    assert!(Readiness::of(&rows, &[]).all_green, "{rows:?}");
}

fn measured(fda: FdaState, dr: DrClass, who: &str) -> PrivacyFacts {
    PrivacyFacts {
        fda,
        fda_label: match fda {
            FdaState::Granted => ProbeLabel::OpenOk,
            FdaState::Denied => ProbeLabel::OpenEperm,
            FdaState::Unknown => ProbeLabel::OpenErrno(2),
        },
        dr,
        responsible: Some(who.to_string()),
    }
}

fn diag(cmd: &str, arg: Option<&str>) -> CliAction {
    CliAction::Diag {
        cmd: cmd.to_string(),
        arg: arg.map(str::to_string),
    }
}

#[test]
fn a_diag_subcommand_refuses_an_argument_it_would_have_swallowed() {
    for cmd in [
        "show-config",
        "explain-config",
        "list-fonts",
        "list-kitty-commands",
    ] {
        match decide(&[cmd, "--json"]) {
            CliAction::Usage(msg) => {
                assert!(
                    msg.contains(cmd) && msg.contains("--json") && msg.contains("takes none"),
                    "the refusal must name the verb, the argument, and the \
                     arity: {msg}"
                );
            }
            other => panic!("`aterm {cmd} --json` must be a usage error, got {other:?}"),
        }
        assert_eq!(decide(&[cmd]), diag(cmd, None), "bare `{cmd}` still runs");
    }
}

#[test]
fn a_diag_help_request_describes_and_never_runs() {
    for argv in [
        vec!["doctor", "--help"],
        vec!["show-config", "-h"],
        vec!["show-face", "--help"],
        vec!["show-face", "-h"],
    ] {
        match decide(&argv) {
            CliAction::DiagHelp { cmd } => assert_eq!(cmd, argv[0]),
            other => panic!("{argv:?} must be a help request, got {other:?}"),
        }
    }
}

#[test]
fn doctor_operand_grammar_is_enforced_at_the_edge() {
    assert_eq!(decide(&["doctor"]), diag("doctor", None));
    assert_eq!(
        decide(&["doctor", "--read-only"]),
        diag("doctor", Some("--read-only"))
    );
    assert_eq!(
        decide(&["doctor", "--json", "--only", "pkg"]),
        diag("doctor", Some("--json --only pkg"))
    );
    assert_eq!(
        decide(&["doctor", "--check", "--json"]),
        diag("doctor", Some("--check --json"))
    );
    assert_eq!(
        decide(&["doctor", "--verbose", "--read-only"]),
        diag("doctor", Some("--verbose --read-only"))
    );
    assert_eq!(
        decide(&["doctor", "--check", "--verbose"]),
        diag("doctor", Some("--check --verbose"))
    );
    for (argv, bad_token) in [
        (vec!["doctor", "--fix"], Some("\"--fix\"")),
        (
            vec!["doctor", "--verbose", "--verbose"],
            Some("\"--verbose\""),
        ),
        (vec!["doctor", "--apply", "--verbose"], None),
        (vec!["doctor", "--read-only", "x"], Some("\"x\"")),
        (vec!["doctor", "--json", "--json"], Some("\"--json\"")),
        (vec!["doctor", "--check", "--apply"], None),
        (vec!["doctor", "--apply", "--only", "pkg"], None),
    ] {
        match decide(&argv) {
            CliAction::Usage(msg) => {
                if let Some(tok) = bad_token {
                    assert!(msg.contains(tok), "{argv:?} should name {tok}: {msg}");
                }
            }
            other => panic!("{argv:?} must be a usage error, got {other:?}"),
        }
    }
}

#[test]
fn exceptions_operand_grammar_is_enforced_at_the_edge() {
    assert_eq!(decide(&["exceptions"]), diag("exceptions", None));
    assert_eq!(
        decide(&["exceptions", "--all"]),
        diag("exceptions", Some("--all"))
    );
    assert_eq!(
        decide(&["exceptions", "--help"]),
        CliAction::DiagHelp {
            cmd: "exceptions".to_string()
        }
    );
    for (argv, bad_token) in [
        (vec!["exceptions", "--json"], "\"--json\""),
        (vec!["exceptions", "--all", "extra"], "\"extra\""),
        (vec!["exceptions", "all"], "\"all\""),
    ] {
        match decide(&argv) {
            CliAction::Usage(msg) => {
                assert!(
                    msg.contains("aterm exceptions [--all]"),
                    "the refusal must state the grammar: {msg}"
                );
                assert!(
                    msg.contains(bad_token),
                    "{argv:?} should name {bad_token}: {msg}"
                );
            }
            other => panic!("{argv:?} must be a usage error, got {other:?}"),
        }
    }
}

struct Scratch(aterm_tempfile::TempDir);

impl Scratch {
    fn new() -> Self {
        Self(
            aterm_tempfile::Builder::new()
                .prefix("aterm-exceptions-reader-")
                .tempdir()
                .expect("scratch dir"),
        )
    }

    fn log(&self) -> std::path::PathBuf {
        self.0.path().join("messages.log")
    }

    fn rotated(&self) -> std::path::PathBuf {
        self.0.path().join("messages.log.1")
    }
}

fn exception_lines() -> Vec<String> {
    use aterm_messages::need::{Fingerprint, Reason, Resurface, Then, Why};
    use aterm_messages::{Instant, Message, MessageCenter, MessageLog, Severity, WallStamp, tags};
    const T0: u64 = 1_790_000_000_000;
    let now = Instant::now();
    let mut c = MessageCenter::new(MessageLog::empty(), now);
    let ask = |stem: &str, cause: &str, why: &str, then: &str| {
        aterm_messages::need::LaterAsk::person(
            Why::Question(Reason::new(why)),
            Then::step(then),
            Fingerprint::new(stem, "", cause),
            Resurface::CHOICE,
        )
    };
    c.post(
        Message::new(
            tags::PRIVACY,
            Severity::Info,
            "macOS keeps asking for access",
        )
        .later(ask(
            "privacy",
            "fda",
            "macOS forgets an Allow when aterm updates",
            "Turn on aterm in Full Disk Access",
        )),
        WallStamp { unix_ms: T0 },
        now,
    );
    c.post(
        Message::new(tags::UPDATE, Severity::Warn, "An older thing").later(ask(
            "update",
            "old",
            "the old thing needs you",
            "Do the old thing",
        )),
        WallStamp {
            unix_ms: T0 + 1_000,
        },
        now,
    );
    let mut lines: Vec<String> = c
        .drain_shelved_for_persist()
        .iter()
        .map(|(l, _)| l.encode())
        .collect();
    c.file_digest(
        WallStamp {
            unix_ms: T0 + 60_000,
        },
        now,
    );
    c.post(
        Message::new(tags::UPDATE, Severity::Warn, "Updates keep failing").later(ask(
            "update",
            "failing",
            "the updater has failed for a day",
            "Open Settings and check the update lane",
        )),
        WallStamp {
            unix_ms: T0 + 120_000,
        },
        now,
    );
    lines.extend(
        c.drain_shelved_for_persist()
            .iter()
            .map(|(l, _)| l.encode()),
    );
    lines
}

#[test]
fn the_exceptions_reader_replays_the_log_without_a_window() {
    let dir = Scratch::new();
    let none = exceptions_report_at(&dir.log(), false, 1, 0);
    assert!(
        none.starts_with("exceptions: none (no log yet at "),
        "{none}"
    );

    let lines = exception_lines();
    let posted: Vec<&String> = lines.iter().filter(|l| l.contains("kind=posted")).collect();
    let rest: Vec<&String> = lines
        .iter()
        .filter(|l| !l.contains("kind=posted"))
        .collect();
    std::fs::write(dir.rotated(), format!("{}\n{}\n", posted[0], posted[1])).unwrap();
    std::fs::write(
        dir.log(),
        format!(
            "m1\tkind=posted\tid=9\n{}\n{}\n",
            rest.iter()
                .map(|l| l.as_str())
                .collect::<Vec<_>>()
                .join("\n"),
            posted[2]
        ),
    )
    .unwrap();

    const T0: u64 = 1_790_000_000_000;
    let report = exceptions_report_at(&dir.log(), false, T0 + 3_600_000, 3_600);
    assert!(report.contains("To look at (1)"), "{report}");
    assert!(report.contains("Updates keep failing"), "{report}");
    assert!(
        report.contains("why    the updater has failed for a day"),
        "{report}"
    );
    assert!(
        report.contains("then   Open Settings and check the update lane"),
        "{report}"
    );
    assert!(report.contains("Filed (2)"), "{report}");
    assert!(report.contains("macOS keeps asking for access"), "{report}");
    assert!(report.contains("dismissed"), "{report}");
    assert!(
        report.contains("(1 line(s) of the log could not be read)"),
        "{report}"
    );
    assert!(
        report.find("To look at").unwrap() < report.find("Filed (").unwrap(),
        "{report}"
    );
    let all = exceptions_report_at(&dir.log(), true, T0 + 3_600_000, 3_600);
    assert!(all.contains("Filed (2)"), "{all}");
    assert!(!all.contains("--all lists every filed entry"), "{all}");

    let read = read_exceptions_log(&dir.log());
    assert_eq!(
        read.log
            .records()
            .filter(|r| r.later && r.is_live())
            .count(),
        1
    );

    // Across a daylight fall-back after T0 (`T0` at +2h, `now` at +1h), `exceptions`
    // prints the entry's own +2h wall clock rather than shifting it by `now`'s +1h.
    let change_s = i64::try_from(T0 / 1000).unwrap() + 1_800;
    let dst = super::exceptions_report_at_local(&dir.log(), false, T0 + 3_600_000, &|unix| {
        Some(if unix < change_s { 7_200 } else { 3_600 })
    });
    let summer = aterm_messages::words::clock_words(T0, 7_200);
    let winter = aterm_messages::words::clock_words(T0, 3_600);
    assert_ne!(summer, winter);
    assert!(dst.contains(&summer), "{dst}");
    assert!(
        report.contains(&winter) && !report.contains(&summer),
        "CONTROL: flat +1h shifts T0's clock by an hour: {report}"
    );
}

/// A log is never worth a symlink: a `messages.log` that points elsewhere is not
/// followed and is said as unreadable (never "does not exist"), and a directory in
/// its place likewise, even when the rotated copy reads.
#[cfg(unix)]
#[test]
fn the_exceptions_reader_never_follows_a_symlink() {
    let dir = Scratch::new();
    let real = dir.0.path().join("elsewhere.log");
    std::fs::write(&real, format!("{}\n", exception_lines().join("\n"))).unwrap();
    std::os::unix::fs::symlink(&real, dir.log()).unwrap();
    let out = exceptions_report_at(&dir.log(), false, 1, 0);
    assert!(out.contains("is a symlink or folder, not the log"), "{out}");
    let other = Scratch::new();
    std::fs::create_dir(other.log()).unwrap();
    let out = exceptions_report_at(&other.log(), false, 1, 0);
    assert!(out.contains("is a symlink or folder, not the log"), "{out}");
    assert!(read_exceptions_log(&other.log()).unreadable.is_some());
    let rotated = Scratch::new();
    let older = rotated.0.path().join("messages.log.1");
    std::fs::create_dir(&older).unwrap();
    let out = exceptions_report_at(&rotated.log(), false, 1, 0);
    assert!(
        out.contains(&format!("{} is a symlink", older.display())),
        "{out}"
    );
    let unreadable = read_exceptions_log(&rotated.log()).unreadable.unwrap();
    assert_eq!(unreadable.name(), "messages.log.1");
    let stale = Scratch::new();
    let posted: Vec<String> = exception_lines()
        .into_iter()
        .filter(|l| l.contains("kind=posted"))
        .collect();
    std::fs::write(stale.rotated(), format!("{}\n", posted.join("\n"))).unwrap();
    std::fs::create_dir(stale.log()).unwrap();
    let out = exceptions_report_at(&stale.log(), false, 1, 0);
    assert!(!out.starts_with("exceptions in "), "{out}");
    let unreadable = read_exceptions_log(&stale.log()).unreadable.unwrap();
    assert_eq!(unreadable.name(), "messages.log");
    assert!(unreadable.not_a_file);
}

#[test]
fn the_doctor_exceptions_row_counts_the_open_entries_and_never_fails() {
    let row = |facts: ExceptionsFacts| {
        let (r, code) = doctor_checks_with_origin(
            "/bin/sh",
            ShellCheck::Runs,
            None,
            &unmeasured(),
            &no_debugger(),
            &no_devtools(),
            &no_census(),
            &no_recovery(),
            &facts,
            &MachineFacts::absent(),
        );
        assert_eq!(code, 0, "{r}");
        r
    };
    let none = row(Ok((0, Vec::new())));
    assert!(
        none.lines().any(|l| l == "exceptions: ok    none open"),
        "{none}"
    );
    let two = row(Ok((2, Vec::new())));
    assert!(
        two.lines()
            .any(|l| l == "exceptions: note  2 open (aterm exceptions)"),
        "{two}"
    );
    // The newest open ones by title, inline.
    let titled = row(Ok((
        5,
        vec![
            "disk low".to_owned(),
            "update held".to_owned(),
            "vault quiet".to_owned(),
        ],
    )));
    assert!(
        titled.lines().any(|l| l
            == "exceptions: note  5 open: disk low; update held; vault quiet (aterm exceptions)"),
        "{titled}"
    );
    let homeless = row(Err("no log folder".to_string()));
    assert!(
        homeless
            .lines()
            .any(|l| l == "exceptions: note  not measured (no log folder)"),
        "{homeless}"
    );
}

#[test]
fn show_face_operand_grammar_is_enforced_at_the_edge() {
    assert_eq!(
        decide(&["show-face", "Menlo"]),
        diag("show-face", Some("Menlo"))
    );
    for argv in [
        vec!["show-face"],
        vec!["show-face", "--verbose"],
        vec!["show-face", "Menlo", "extra"],
    ] {
        match decide(&argv) {
            CliAction::Usage(msg) => assert!(
                msg.contains("show-face <family>"),
                "the refusal must state the grammar: {msg}"
            ),
            other => panic!("{argv:?} must be a usage error, got {other:?}"),
        }
    }
}

#[test]
fn the_windowing_verbs_are_exactly_the_ones_the_grammar_parses() {
    for verb in Verb::ALL {
        let parses =
            super::parse_window_request(verb.name(), &[], |raw| Ok(raw.to_string())).is_ok();
        assert_eq!(
            verb.is_windowing(),
            parses,
            "`{}`: is_windowing() must agree with parse_window_request",
            verb.name()
        );
    }
    assert_eq!(
        Verb::ALL.iter().filter(|v| v.is_windowing()).count(),
        3,
        "new-tab, new-window, split-pane"
    );
}

#[test]
fn diag_commands_advertised_and_dispatchable() {
    assert!(!DIAG_COMMANDS.is_empty(), "registry must not be empty");
    for (name, _desc) in DIAG_COMMANDS {
        assert!(
            help_text()
                .lines()
                .any(|l| l.trim_start().starts_with(name)),
            "subcommand {name:?} is not advertised as a --help line"
        );
        assert!(
            diag_report(name, None).is_some()
                || list_themes_alias(name, &[]).is_some()
                || *name == "doctor",
            "subcommand {name:?} is in the registry but has no dispatch"
        );
        if *name == "show-face" {
            assert_eq!(decide(&[name, "Menlo"]), diag(name, Some("Menlo")));
        } else {
            assert_eq!(decide(&[name]), diag(name, None));
        }
    }
    assert!(diag_report("definitely-not-a-command", None).is_none());
}

#[test]
fn list_kitty_commands_prints_one_row_per_line_and_every_trick() {
    let (report, code) =
        diag_report("list-kitty-commands", None).expect("the subcommand is dispatchable");
    assert_eq!(code, 0);
    assert!(report.ends_with('\n'), "the report ends its last row");
    for trick in aterm_lexicon::Trick::ALL {
        let row = format!("command trick={} lang=en gated=0 ", trick.code());
        assert!(
            report.lines().any(|l| l.starts_with(&row)),
            "no English row for trick {:?} in:\n{report}",
            trick.code()
        );
    }
    for line in report.lines() {
        let kind = line.split(' ').next().unwrap_or_default();
        assert!(
            matches!(kind, "command" | "vocative" | "filler"),
            "not a vocabulary row (a banner would break `| grep lang=`): {line:?}"
        );
        assert!(
            line.split(' ').skip(1).all(|field| field.contains('=')),
            "every field after the row kind is key=value: {line:?}"
        );
    }
    assert!(
        report
            .lines()
            .next()
            .is_some_and(|l| l.contains(" lang=en ")),
        "the first row is English"
    );
    assert_eq!(
        decide(&["list-kitty-commands", "--help"]),
        CliAction::DiagHelp {
            cmd: "list-kitty-commands".to_string()
        }
    );
}

#[test]
fn help_word_routes_to_the_manual_and_is_advertised() {
    assert_eq!(
        decide(&["help"]),
        CliAction::Manual {
            topic: None,
            section: None
        }
    );
    assert_eq!(
        decide(&["help", "trust"]),
        CliAction::Manual {
            topic: Some("trust".to_string()),
            section: None
        }
    );
    assert_eq!(
        decide(&["help", "sys", "wait"]),
        CliAction::Manual {
            topic: Some("sys".to_string()),
            section: Some("wait".to_string())
        }
    );
    assert!(matches!(
        decide(&["help", "sys", "wait", "now"]),
        CliAction::Usage(_)
    ));
    assert_eq!(decide(&["-h"]), CliAction::Help);
    assert!(
        help_text()
            .lines()
            .any(|l| l.trim_start().starts_with("aterm help")),
        "`aterm help` is not advertised in --help"
    );
    assert!(!is_tool_candidate(Some("help")));
}

#[test]
fn agents_word_routes_to_the_primer_installer_and_is_advertised() {
    assert_eq!(decide(&["agents"]), CliAction::Agents { rest: vec![] });
    assert_eq!(
        decide(&["agents", "install", "claude"]),
        CliAction::Agents {
            rest: vec!["install".to_string(), "claude".to_string()]
        }
    );
    assert!(
        help_text()
            .lines()
            .any(|l| l.trim_start().starts_with("aterm agents")),
        "`aterm agents` is not advertised in --help"
    );
    assert!(!is_tool_candidate(Some("agents")));
}

#[test]
fn no_synopsis_runs_into_its_blurb() {
    assert!(Verb::Init.usage().len() >= crate::VERB_BLURB_COLUMN - 4);
    for verb in Verb::ALL {
        let block = crate::verb_help_block(*verb);
        let first = block.lines().next().unwrap_or_default();
        let rest = first
            .trim_start()
            .strip_prefix(verb.usage())
            .unwrap_or_else(|| panic!("{first:?}"));
        assert!(rest.is_empty() || rest.starts_with(' '), "{first:?}");
        assert_eq!(
            block.lines().count(),
            verb.blurb().len() + usize::from(rest.is_empty()),
            "{block}"
        );
    }
}

#[test]
fn front_door_verbs_are_the_one_command_surface() {
    assert!(!Verb::ALL.is_empty(), "roster must not be empty");
    let help = help_text();
    for verb in Verb::ALL {
        assert!(
            help.lines().any(|l| l
                .trim_start()
                .starts_with(&format!("aterm {}", verb.name()))),
            "verb `aterm {}` is not advertised in --help",
            verb.name()
        );
        assert!(
            !is_tool_candidate(Some(verb.name())),
            "verb {:?} can be shadowed by a store tool of the same name",
            verb.name()
        );
        assert_eq!(Verb::from_operand(verb.name()), Some(*verb));
        assert!(
            !verb.blurb().is_empty(),
            "verb {:?} has no help",
            verb.name()
        );
    }
    for (i, verb) in Verb::ALL.iter().enumerate() {
        assert!(
            !Verb::ALL[..i].iter().any(|u| u.name() == verb.name()),
            "duplicate front-door verb {}",
            verb.name()
        );
    }
    assert_eq!(Verb::from_operand("definitely-not-a-verb"), None);
    assert_eq!(Verb::from_operand("ct"), None);
    assert_eq!(Verb::from_operand(""), None);
}

#[test]
fn path_injection_prepends_once_and_is_idempotent() {
    let sep = if cfg!(windows) { ';' } else { ':' };
    let bin = "/opt/aterm/bin";
    assert_eq!(
        prepend_path(bin, Some("/usr/bin")),
        Some(("PATH".to_string(), format!("{bin}{sep}/usr/bin")))
    );
    assert_eq!(
        prepend_path(bin, None),
        Some(("PATH".to_string(), bin.to_string()))
    );
    let already = format!("/usr/bin{sep}{bin}{sep}/bin");
    assert_eq!(prepend_path(bin, Some(&already)), None);
}

#[test]
fn session_path_puts_the_reroute_dir_first_by_move_to_front_and_is_idempotent() {
    let sep = if cfg!(windows) { ';' } else { ':' };
    let join = |parts: &[&str]| parts.join(&sep.to_string());
    let reroute = "/Users//u/Library/Application Support/aterm/pkg/reroute";
    let bin = "/opt/aterm/bin";
    assert_eq!(
        session_path_env(
            Some(reroute),
            None,
            Some(bin),
            Some(&join(&["/usr/bin", "/bin"]))
        ),
        Some((
            "PATH".to_string(),
            join(&[reroute, bin, "/usr/bin", "/bin"])
        ))
    );
    let inherited = join(&["/Users//u/.cargo/bin", reroute, bin, "/usr/bin", reroute]);
    let (_, value) =
        session_path_env(Some(reroute), None, Some(bin), Some(&inherited)).expect("injects");
    assert_eq!(
        value,
        join(&[reroute, "/Users//u/.cargo/bin", bin, "/usr/bin"])
    );
    assert_eq!(value.matches(reroute).count(), 1);
    let (_, again) =
        session_path_env(Some(reroute), None, Some(bin), Some(&value)).expect("injects");
    assert_eq!(again, value);
    assert_eq!(
        session_path_env(Some(reroute), None, None, None).map(|p| p.1),
        Some(reroute.to_string())
    );
    assert_eq!(
        session_path_env(Some(reroute), None, None, Some("")).map(|p| p.1),
        Some(reroute.to_string())
    );
    assert_eq!(
        session_path_env(None, None, Some(bin), Some("/usr/bin")),
        prepend_path(bin, Some("/usr/bin"))
    );
    assert_eq!(
        session_path_env(None, None, Some(bin), Some(&join(&["/usr/bin", bin]))),
        None
    );
    assert_eq!(session_path_env(None, None, None, Some("/usr/bin")), None);
}

#[test]
fn session_path_puts_the_agents_dir_second_by_the_same_move_to_front_rule() {
    let sep = if cfg!(windows) { ';' } else { ':' };
    let join = |parts: &[&str]| parts.join(&sep.to_string());
    let reroute = "/Users//u/Library/Application Support/aterm/pkg/reroute";
    let agents = "/Users//u/Library/Application Support/aterm/pkg/agents";
    let bin = "/opt/aterm/bin";
    let foreign = ["/Users//u/.local/bin", "/opt/homebrew/bin", "/usr/bin"];
    let (_, value) = session_path_env(
        Some(reroute),
        Some(agents),
        Some(bin),
        Some(&join(&foreign)),
    )
    .expect("injects");
    assert_eq!(
        value,
        join(&[
            reroute,
            agents,
            bin,
            "/Users//u/.local/bin",
            "/opt/homebrew/bin",
            "/usr/bin"
        ])
    );
    let inherited = join(&["/Users//u/.local/bin", agents, "/opt/homebrew/bin", agents]);
    let (_, value) =
        session_path_env(Some(reroute), Some(agents), None, Some(&inherited)).expect("injects");
    assert_eq!(
        value,
        join(&[reroute, agents, "/Users//u/.local/bin", "/opt/homebrew/bin"])
    );
    assert_eq!(value.matches(agents).count(), 1);
    let (_, again) =
        session_path_env(Some(reroute), Some(agents), None, Some(&value)).expect("injects");
    assert_eq!(again, value);
    assert_eq!(
        session_path_env(None, Some(agents), Some(bin), Some("/usr/bin")).map(|p| p.1),
        Some(join(&[agents, bin, "/usr/bin"]))
    );
    assert_eq!(
        session_path_env(None, Some(agents), None, None).map(|p| p.1),
        Some(agents.to_string())
    );
    assert_eq!(
        session_path_env(Some(agents), Some(agents), Some(bin), Some("/usr/bin")).map(|p| p.1),
        Some(join(&[agents, bin, "/usr/bin"])),
        "when reroute and agents name one directory, PATH lists it once"
    );
}

#[test]
fn the_agents_dir_is_the_reroute_dirs_sibling_in_atpkgs_layout() {
    let scratch = scratch_dir("agents-sibling");
    let layout = atpkg::store::Layout {
        prefix: scratch.join("pkg"),
    };
    let reroute = layout.reroute_dir();
    std::fs::create_dir_all(&reroute).expect("the front door's reroute dir");
    assert!(
        !layout.agents_dir().is_dir(),
        "a fresh prefix has no agents/"
    );
    let derived = managed_agents_dir(None, reroute.to_str(), None).expect("derived and created");
    assert_eq!(derived, layout.agents_dir().to_str().unwrap());
    assert!(layout.agents_dir().is_dir(), "ensured, like the window's");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(layout.agents_dir())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o700, "a $HOME prefix's directory is private");
    }
    assert_eq!(
        managed_agents_dir(None, reroute.to_str(), None).as_deref(),
        Some(derived.as_str())
    );
    assert_eq!(reroute, layout.prefix.join(atpkg::reroute::DIR_NAME));
    assert_eq!(layout.agents_dir(), layout.prefix.join("agents"));
}

#[test]
fn managed_agents_dir_prefers_the_front_doors_handoff_then_the_sibling_then_the_hook() {
    let scratch = scratch_dir("agents-precedence");
    let handed = scratch.join("handed-agents");
    std::fs::create_dir(&handed).unwrap();
    let handed_str = handed.to_str().unwrap();
    let prefix = scratch.join("pkg");
    let reroute = prefix.join("reroute");
    std::fs::create_dir_all(&reroute).unwrap();
    let reroute_str = reroute.to_str().unwrap();
    let hook = scratch.join("hook-agents");
    std::fs::create_dir(&hook).unwrap();
    let hook_str = hook.to_str().unwrap();
    assert_eq!(
        managed_agents_dir(Some(handed_str), None, None).as_deref(),
        Some(handed_str),
        "the --no-reroute shape: the handoff alone fronts the managed agents"
    );
    assert_eq!(
        managed_agents_dir(Some(handed_str), Some(reroute_str), Some(hook_str)).as_deref(),
        Some(handed_str)
    );
    assert!(
        !prefix.join("agents").exists(),
        "a taken handoff derives and creates nothing beside the reroute dir"
    );
    for stray in ["", "agents", "./handed-agents"] {
        assert_eq!(
            managed_agents_dir(Some(stray), None, Some(hook_str)).as_deref(),
            Some(hook_str),
            "{stray:?} is not an absolute handoff"
        );
    }
    assert_eq!(
        managed_agents_dir(scratch.join("absent").to_str(), None, Some(hook_str)).as_deref(),
        Some(hook_str),
        "must exist"
    );
    let filed = scratch.join("filed-agents");
    std::fs::write(&filed, b"not a dir").unwrap();
    assert_eq!(
        managed_agents_dir(filed.to_str(), None, Some(hook_str)).as_deref(),
        Some(hook_str),
        "must be a directory"
    );
    #[cfg(unix)]
    {
        let linked = scratch.join("linked-agents");
        std::os::unix::fs::symlink(&handed, &linked).unwrap();
        assert!(linked.is_dir(), "the link resolves");
        assert_eq!(
            managed_agents_dir(linked.to_str(), None, Some(hook_str)).as_deref(),
            Some(hook_str),
            "a symlink is never the front door's handoff"
        );
    }
    let derived = managed_agents_dir(None, Some(reroute_str), Some(hook_str)).expect("derived");
    assert_eq!(derived, prefix.join("agents").to_str().unwrap());
    assert!(prefix.join("agents").is_dir(), "ensured");
    assert_eq!(
        managed_agents_dir(Some(""), Some(reroute_str), Some(hook_str)).as_deref(),
        Some(derived.as_str()),
        "an EMPTY handoff means none — the sibling rule applies"
    );
    assert_eq!(
        managed_agents_dir(None, None, Some(hook_str)).as_deref(),
        Some(hook_str)
    );
    assert_eq!(managed_agents_dir(None, None, None), None);
    let _ = std::fs::remove_dir_all(&scratch);
}

struct TestScratch(aterm_tempfile::TempDir);

impl std::ops::Deref for TestScratch {
    type Target = std::path::Path;

    fn deref(&self) -> &Self::Target {
        self.0.path()
    }
}

impl AsRef<std::path::Path> for TestScratch {
    fn as_ref(&self) -> &std::path::Path {
        self.0.path()
    }
}

fn scratch_dir(tag: &str) -> TestScratch {
    TestScratch(
        aterm_tempfile::Builder::new()
            .prefix(&format!("aterm-cli-{tag}-"))
            .tempdir()
            .expect("scratch dir"),
    )
}

#[test]
fn reroute_dir_from_values_takes_only_an_absolute_existing_dir() {
    let scratch = scratch_dir("reroute-values");
    let reroute = scratch.join("reroute");
    std::fs::create_dir(&reroute).unwrap();
    let abs = reroute.to_str().unwrap();
    assert_eq!(
        reroute_dir_from_values(None, Some(abs)).as_deref(),
        Some(abs)
    );
    assert_eq!(
        reroute_dir_from_values(Some(""), Some(abs)).as_deref(),
        Some(abs)
    );
    assert_eq!(
        reroute_dir_from_values(Some("0"), Some(abs)).as_deref(),
        Some(abs)
    );
    assert_eq!(reroute_dir_from_values(Some("1"), Some(abs)), None);
    assert_eq!(reroute_dir_from_values(None, None), None);
    assert_eq!(reroute_dir_from_values(None, Some("")), None);
    assert_eq!(
        reroute_dir_from_values(None, Some("reroute")),
        None,
        "relative"
    );
    assert_eq!(
        reroute_dir_from_values(None, Some("./reroute")),
        None,
        "relative"
    );
    assert_eq!(
        reroute_dir_from_values(None, scratch.join("absent").to_str()),
        None,
        "must exist"
    );
    let _ = std::fs::remove_dir_all(&scratch);
}

#[test]
fn agents_dir_mode_restates_layout_ensure_dirs_rule() {
    assert_eq!(agents_dir_mode(501, 0o700, 501), Some(0o700));
    assert_eq!(agents_dir_mode(501, 0o755, 501), Some(0o700));
    assert_eq!(agents_dir_mode(0, 0o755, 0), Some(0o755));
    assert_eq!(agents_dir_mode(0, 0o755, 501), Some(0o755));
    assert_eq!(agents_dir_mode(0, 0o700, 0), Some(0o755));
    assert_eq!(agents_dir_mode(0, 0o775, 0), Some(0o700));
    assert_eq!(agents_dir_mode(0, 0o777, 501), None);
    assert_eq!(agents_dir_mode(501, 0o700, 0), None);
    assert_eq!(agents_dir_mode(501, 0o700, 502), None);
}

#[test]
fn managed_agents_dir_refuses_relative_handles_and_links_and_falls_back_to_the_enclosing_shell() {
    let scratch = scratch_dir("agents-refuse");
    let cwd_agents = std::path::Path::new("agents");
    assert!(
        !cwd_agents.exists(),
        "precondition: the test cwd has no `agents` entry"
    );
    assert_eq!(managed_agents_dir(None, Some("reroute"), None), None);
    assert_eq!(managed_agents_dir(None, Some("./pkg/reroute"), None), None);
    assert!(!cwd_agents.exists(), "no `agents` created in the cwd");
    let enclosing = scratch.join("enclosing-agents");
    std::fs::create_dir(&enclosing).unwrap();
    let enclosing_str = enclosing.to_str().unwrap();
    assert_eq!(
        managed_agents_dir(None, None, Some(enclosing_str)).as_deref(),
        Some(enclosing_str)
    );
    assert_eq!(
        managed_agents_dir(None, Some("reroute"), Some(enclosing_str)).as_deref(),
        Some(enclosing_str),
        "a refused relative handle still leaves the fallback"
    );
    assert_eq!(managed_agents_dir(None, None, Some("")), None);
    assert_eq!(
        managed_agents_dir(None, None, Some("agents")),
        None,
        "relative"
    );
    assert_eq!(
        managed_agents_dir(None, None, scratch.join("absent").to_str()),
        None,
        "must exist"
    );
    assert_eq!(managed_agents_dir(None, None, None), None);
    let prefix = scratch.join("pkg");
    let reroute = prefix.join("reroute");
    std::fs::create_dir_all(&reroute).unwrap();
    let derived = managed_agents_dir(None, reroute.to_str(), Some(enclosing_str)).expect("derived");
    assert_eq!(derived, prefix.join("agents").to_str().unwrap());
    let filed = scratch.join("filed");
    std::fs::create_dir_all(filed.join("reroute")).unwrap();
    std::fs::write(filed.join("agents"), b"not a dir").unwrap();
    assert_eq!(
        managed_agents_dir(None, filed.join("reroute").to_str(), None),
        None
    );
    assert_eq!(
        managed_agents_dir(None, filed.join("reroute").to_str(), Some(enclosing_str)).as_deref(),
        Some(enclosing_str)
    );
    assert!(filed.join("agents").is_file(), "left alone");
    #[cfg(unix)]
    {
        let linked = scratch.join("linked");
        std::fs::create_dir_all(linked.join("reroute")).unwrap();
        std::os::unix::fs::symlink(&enclosing, linked.join("agents")).unwrap();
        assert!(linked.join("agents").is_dir(), "the link resolves");
        assert_eq!(
            managed_agents_dir(None, linked.join("reroute").to_str(), None),
            None
        );
        assert_eq!(
            managed_agents_dir(None, linked.join("reroute").to_str(), Some(enclosing_str))
                .as_deref(),
            Some(enclosing_str)
        );
        assert!(
            std::fs::symlink_metadata(linked.join("agents"))
                .unwrap()
                .file_type()
                .is_symlink(),
            "left alone"
        );
    }
    let _ = std::fs::remove_dir_all(&scratch);
}

/// A prefix that refuses the `mkdir` yields `None` and creates nothing.
#[cfg(unix)]
#[test]
fn managed_agents_dir_says_no_when_the_prefix_refuses_the_mkdir() {
    use std::os::unix::fs::PermissionsExt as _;
    // SAFETY: getuid() takes no arguments and cannot fail.
    if unsafe { libc::getuid() } == 0 {
        aterm_spec::host::could_not_run("running as root, a read-only prefix refuses nothing");
    }
    let scratch = scratch_dir("agents-readonly");
    let prefix = scratch.join("pkg");
    let reroute = prefix.join("reroute");
    std::fs::create_dir_all(&reroute).unwrap();
    std::fs::set_permissions(&prefix, std::fs::Permissions::from_mode(0o500)).unwrap();
    assert_eq!(managed_agents_dir(None, reroute.to_str(), None), None);
    assert!(!prefix.join("agents").exists());
    std::fs::set_permissions(&prefix, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(
        managed_agents_dir(None, reroute.to_str(), None).as_deref(),
        prefix.join("agents").to_str()
    );
    let mode = std::fs::metadata(prefix.join("agents"))
        .unwrap()
        .permissions()
        .mode()
        & 0o7777;
    assert_eq!(mode, 0o700);
    let _ = std::fs::remove_dir_all(&scratch);
}

#[test]
fn show_config_report_has_stable_keys() {
    let (r, code) = diag_report("show-config", None).expect("show-config dispatches");
    assert_eq!(code, 0, "show-config always succeeds");
    for key in [
        "version=",
        "shell=",
        "term=",
        "rows=",
        "cols=",
        "containment_default=user",
    ] {
        assert!(
            r.contains(key),
            "show-config missing {key:?}\n--- report ---\n{r}"
        );
    }
    for line in r.lines().filter(|l| !l.is_empty()) {
        assert!(line.contains('='), "non key=value line: {line:?}");
    }
}

#[test]
fn show_face_requires_and_validates_family() {
    let (msg, code) = show_face_report(None);
    assert_eq!(code, 2);
    assert!(msg.contains("usage: aterm show-face <family>"), "{msg}");
    let (msg, code) = show_face_report(Some("definitely-not-a-real-font-xyzzy"));
    assert_eq!(code, 1);
    assert!(msg.contains("aterm list-fonts"), "{msg}");
}

fn mark_of(report: &str, label: &str) -> String {
    report
        .lines()
        .find(|l| l.starts_with(label))
        .and_then(|l| l.split_whitespace().nth(1))
        .unwrap_or_else(|| panic!("no {label} row in\n{report}"))
        .to_string()
}

#[test]
fn config_conpty_reads_the_top_level_key_the_window_reads() {
    assert_eq!(config_conpty_from_text(""), None);
    assert_eq!(config_conpty_from_text("# conpty = \"auto\"\n"), None);
    assert_eq!(config_conpty_from_text("[x]\nconpty = \"auto\"\n"), None);
    assert_eq!(config_conpty_from_text("conpty = 1\n"), None);
    assert_eq!(config_conpty_from_text("conpty = \"auto\" x\n"), None);
    assert_eq!(
        config_conpty_from_text("conpty = 'C:\\oc\\OpenConsole.exe'\n").as_deref(),
        Some("C:\\oc\\OpenConsole.exe")
    );
}

#[test]
fn the_conpty_row_is_ok_for_the_default_and_a_note_for_a_fallback() {
    let report_with = |host: Option<aterm_pty::ConsoleHostReport>| {
        doctor_checks_with_origin(
            "/bin/sh",
            ShellCheck::Runs,
            None,
            &unmeasured(),
            &no_debugger(),
            &no_devtools(),
            &no_census(),
            &no_recovery(),
            &no_exceptions(),
            &MachineFacts {
                console_host: host,
                ..MachineFacts::absent()
            },
        )
    };
    let (none, code) = report_with(None);
    assert_eq!(code, 0);
    assert!(!none.lines().any(|l| l.starts_with("conpty:")), "{none}");

    let inbox = aterm_pty::ConsoleHostReport {
        kind: aterm_pty::ConsoleHostKind::Inbox,
        fell_back: false,
        line: "Windows console host (aterm.toml conpty = \"inbox\")".to_string(),
    };
    let (r, code) = report_with(Some(inbox));
    assert_eq!(code, 0);
    let rows: Vec<&str> = r.lines().collect();
    let at = rows
        .iter()
        .position(|l| l.starts_with("conpty:"))
        .expect("the row");
    assert_eq!(
        rows[at],
        "conpty:     ok    Windows console host (aterm.toml conpty = \"inbox\")"
    );
    assert!(rows[at + 1].starts_with("instances: "), "{r}");
    assert!(r.contains("health: OK\n"), "{r}");

    let fell_back = aterm_pty::ConsoleHostReport {
        kind: aterm_pty::ConsoleHostKind::Inbox,
        fell_back: true,
        line: "Windows console host; OpenConsole did not start (x)".to_string(),
    };
    let (r, code) = report_with(Some(fell_back));
    assert_eq!(code, 0, "a fallback never fails: {r}");
    assert_eq!(mark_of(&r, "conpty:"), "note", "{r}");
    assert!(r.contains("health: OK\n"), "a note is not a warning: {r}");

    let openconsole = aterm_pty::ConsoleHostReport {
        kind: aterm_pty::ConsoleHostKind::OpenConsole,
        fell_back: false,
        line: "OpenConsole 1.24.2607.10001 (bundled beside aterm: x)".to_string(),
    };
    let (r, _) = report_with(Some(openconsole));
    assert_eq!(mark_of(&r, "conpty:"), "ok", "{r}");
}

#[test]
fn doctor_checks_pass_and_flag_each_failure() {
    use aterm_containment::consent::{
        DevToolList, DevToolListLabel, DevToolListing, DevToolProbe, DevToolProbeLabel,
    };
    let populated_devtools = DevtoolsFacts {
        facts: Some(atpkg::devtools::Facts {
            subject: Some("com.aterm.aterm".to_string()),
            list: DevToolList {
                listing: DevToolListing::Off,
                label: DevToolListLabel::Read,
                modified_at_s: Some(1),
                rows: Some(2),
            },
            log: DevToolProbe::refused(DevToolProbeLabel::NoRecentRequest),
            other_responsible: None,
            program_check: None,
        }),
        did: None,
    };
    let populated_machine = MachineFacts {
        copies: Some(atpkg::copies::Pruned::Done {
            moved: vec![(
                std::path::PathBuf::from("/Applications/aterm-prev-1.app"),
                aterm_containment::consent::Retired::Moved,
            )],
            kept: Vec::new(),
        }),
        ..MachineFacts::absent()
    };

    let (r, code) = doctor_checks_with_origin(
        "/bin/sh",
        ShellCheck::Runs,
        None,
        &unmeasured(),
        &no_debugger(),
        &no_devtools(),
        &no_census(),
        &no_recovery(),
        &no_exceptions(),
        &MachineFacts::absent(),
    );
    assert_eq!(code, 0, "{r}");
    assert!(r.contains("health: OK"), "{r}");
    assert_eq!(mark_of(&r, "shell:"), "ok", "{r}");
    assert!(
        !r.contains("tty:") && !r.contains("cols") && !r.contains("containment"),
        "{r}"
    );
    for key in ["shell: ", "privacy: ", "debugger: ", "version: "] {
        assert!(
            r.lines().any(|l| l.starts_with(key)),
            "doctor detail missing a {key:?} line\n{r}"
        );
    }

    let (r_with_rows, code) = doctor_checks_with_origin(
        "/bin/sh",
        ShellCheck::Runs,
        None,
        &unmeasured(),
        &no_debugger(),
        &populated_devtools,
        &no_census(),
        &no_recovery(),
        &no_exceptions(),
        &populated_machine,
    );
    assert_eq!(code, 0, "{r_with_rows}");
    assert_eq!(mark_of(&r_with_rows, "devtools:"), "warn", "{r_with_rows}");
    assert_eq!(mark_of(&r_with_rows, "copies:"), "note", "{r_with_rows}");

    for (shell, check) in [
        ("/no/such/shell", ShellCheck::Missing),
        ("/etc/hosts", ShellCheck::NotExecutable),
        ("", ShellCheck::Missing),
    ] {
        let (r, code) = doctor_checks_with_origin(
            shell,
            check,
            None,
            &unmeasured(),
            &no_debugger(),
            &populated_devtools,
            &no_census(),
            &no_recovery(),
            &no_exceptions(),
            &populated_machine,
        );
        assert_eq!(code, 1, "{shell:?}: {r}");
        assert_eq!(mark_of(&r, "shell:"), "FAIL", "{r}");
        assert!(r.contains("health: FAIL"), "{r}");
    }
}

/// `$SHELL` unset is not a failure: a plain `aterm` then starts the
/// account's login shell, else `/bin/sh` (aterm-pty's resolver), so that is
/// the shell doctor checks and names, and says why.
#[cfg(unix)]
#[test]
fn doctor_checks_the_login_shell_or_bin_sh_when_shell_is_unset() {
    use aterm_pty::ShellOrigin;
    assert_eq!(
        unix_shell(("/bin/zsh".into(), ShellOrigin::Env)),
        ("/bin/zsh".to_string(), None)
    );
    assert_eq!(
        unix_shell(("/usr/bin/fish".into(), ShellOrigin::Passwd)),
        (
            "/usr/bin/fish".to_string(),
            Some(
                "$SHELL is unset or empty, so aterm starts your login shell from the password database"
            )
        )
    );
    let (shell, origin) = unix_shell(("/bin/sh".into(), ShellOrigin::Default));
    assert_eq!(shell, "/bin/sh");
    let (r, code) = doctor_checks_with_origin(
        &shell,
        driver::shell_check(&shell),
        origin,
        &unmeasured(),
        &no_debugger(),
        &no_devtools(),
        &no_census(),
        &no_recovery(),
        &no_exceptions(),
        &MachineFacts::absent(),
    );
    assert_eq!(code, 0, "{r}");
    assert_eq!(mark_of(&r, "shell:"), "ok", "{r}");
    assert!(origin.is_some(), "the row says why /bin/sh");
}

/// The driver's shell check tells a missing path from one that cannot run.
#[cfg(unix)]
#[test]
fn shell_check_names_the_cause() {
    assert_eq!(driver::shell_check("/bin/sh"), ShellCheck::Runs);
    assert_eq!(
        driver::shell_check("/no/such/aterm-shell"),
        ShellCheck::Missing
    );
    assert_eq!(driver::shell_check("/"), ShellCheck::NotExecutable);
    assert_eq!(driver::shell_check("/etc/hosts"), ShellCheck::NotExecutable);
    assert_eq!(driver::shell_check("/etc/hosts/zsh"), ShellCheck::Missing);
}

#[test]
fn doctor_leaves_out_rows_for_probes_this_platform_lacks() {
    let (r, code) = doctor_checks_with_origin(
        "/bin/sh",
        ShellCheck::Runs,
        None,
        &PrivacyFacts::not_measured(ProbeLabel::UnsupportedPlatform),
        &DebuggerFacts::not_measured(DebuggerProbeLabel::UnsupportedPlatform),
        &no_devtools(),
        &no_census(),
        &no_recovery(),
        &no_exceptions(),
        &MachineFacts::absent(),
    );
    assert_eq!(code, 0, "{r}");
    assert!(!r.contains("privacy:") && !r.contains("debugger:"), "{r}");
    assert!(!r.contains("macOS only"), "{r}");
    assert!(r.contains("instances:"), "{r}");
}

#[test]
fn doctor_shell_row_carries_an_origin_label_only_when_given() {
    let shell_row = |report: &str| -> String {
        report
            .lines()
            .find(|l| l.starts_with("shell:"))
            .unwrap_or_else(|| panic!("no shell: row in\n{report}"))
            .to_string()
    };
    let (unlabelled, code) = doctor_checks_with_origin(
        "/bin/sh",
        ShellCheck::Runs,
        None,
        &unmeasured(),
        &no_debugger(),
        &no_devtools(),
        &no_census(),
        &no_recovery(),
        &no_exceptions(),
        &MachineFacts::absent(),
    );
    assert_eq!(code, 0);
    assert_eq!(
        shell_row(&unlabelled),
        "shell:      ok    /bin/sh (executable)",
        "no label, no origin suffix"
    );

    let (labelled, code) = doctor_checks_with_origin(
        r"C:\Program Files\PowerShell\7\pwsh.exe",
        ShellCheck::Runs,
        Some("a new window's default shell, the platform default (pwsh on PATH)"),
        &unmeasured(),
        &no_debugger(),
        &no_devtools(),
        &no_census(),
        &no_recovery(),
        &no_exceptions(),
        &MachineFacts::absent(),
    );
    assert_eq!(code, 0);
    assert_eq!(mark_of(&labelled, "shell:"), "ok", "{labelled}");
    assert!(
        shell_row(&labelled)
            .ends_with("a new window's default shell, the platform default (pwsh on PATH)"),
        "{labelled}"
    );

    let (failed, code) = doctor_checks_with_origin(
        "nosuch",
        ShellCheck::Missing,
        Some("a new window's default shell, from aterm.toml shell = \"nosuch\""),
        &unmeasured(),
        &no_debugger(),
        &no_devtools(),
        &no_census(),
        &no_recovery(),
        &no_exceptions(),
        &MachineFacts::absent(),
    );
    assert_eq!(code, 1);
    assert_eq!(mark_of(&failed, "shell:"), "FAIL", "{failed}");
    assert!(
        shell_row(&failed)
            .ends_with("a new window's default shell, from aterm.toml shell = \"nosuch\""),
        "{failed}"
    );
}

/// Windows: `doctor` and `show-config` report the shell a window tab runs
/// (this tab's inside one, a new window's default outside), labelled, never
/// this process's `$SHELL`/`%COMSPEC%`.
#[cfg(windows)]
#[test]
fn windows_doctor_and_show_config_report_the_windows_shell() {
    let window_shell = driver::WindowShell::resolve();
    let (doctor, _) = doctor_report(true);
    let shell_line = doctor
        .lines()
        .find(|l| l.starts_with("shell: "))
        .expect("a shell row");
    assert!(shell_line.contains(&window_shell.program), "{shell_line}");
    let in_a_tab = std::env::var_os(aterm_types::domain::ENV_TAB_SHELL)
        .is_some_and(|program| !program.is_empty());
    let whose = if in_a_tab {
        "— this tab's shell"
    } else {
        "— a new window's default shell"
    };
    assert!(shell_line.contains(whose), "{shell_line}");
    assert!(
        shell_line.contains(&window_shell.origin_sentence()),
        "{shell_line}"
    );
    let show = show_config_report();
    assert!(
        show.contains(&format!("shell={}\n", window_shell.program)),
        "{show}"
    );
    assert!(
        show.contains(&format!("shell_origin={}\n", window_shell.origin_token())),
        "{show}"
    );
    if let Ok(posix_shell) = std::env::var("SHELL")
        && posix_shell.starts_with('/')
    {
        assert!(!doctor.contains(&posix_shell), "{doctor}");
        assert!(!show.contains(&posix_shell), "{show}");
    }
}

/// Where a shell's aterm and the app opened from Finder read different files, the paragraph
/// names both and the step that makes them one (round 13); the control, no split, is the
/// paragraph alone.
#[test]
fn explain_config_names_the_app_s_file_when_it_is_another() {
    let home = std::path::Path::new("/h");
    let file = std::path::Path::new("/h/dots/aterm/aterm.toml");
    let alone = config_path_paragraph_with(Some(file), Ok(None));
    assert!(alone.contains("/h/dots/aterm/aterm.toml"), "{alone}");
    assert!(!alone.contains("ln -s"), "{alone}");
    let split = atpkg::config_split::Split {
        here: std::path::PathBuf::from("/h/dots/aterm"),
        app: std::path::PathBuf::from("/h/.config/aterm"),
        app_xdg: None,
        differs: vec!["update.enabled".to_owned()],
        compared: true,
        unify: Some((
            std::path::PathBuf::from("/h/.config/aterm"),
            std::path::PathBuf::from("/h/dots/aterm"),
        )),
    };
    let both = config_path_paragraph_with(Some(file), Ok(Some((&split, home))));
    assert!(both.starts_with(alone.trim_end_matches('\n')), "{both}");
    let flat = both.split_whitespace().collect::<Vec<_>>().join(" ");
    // A path under the home reads `~/…` joined by `/` on every platform (`Split::shown`).
    for said in [
        "~/dots/aterm/aterm.toml",
        "~/.config/aterm/aterm.toml",
        "update.enabled",
        "ln -s /h/dots/aterm /h/.config/aterm",
    ] {
        assert!(flat.contains(said), "{said:?} in {both}");
    }
    assert!(
        both.lines().all(|line| line.chars().count() <= 100),
        "{both}"
    );
    let cause = "launchd lookup did not complete";
    let unread = config_path_paragraph_with(Some(file), Err(cause));
    assert!(unread.contains(cause), "{unread}");
    assert!(!unread.contains("/h/.config/aterm"), "{unread}");
    assert!(!unread.contains("ln -s"), "{unread}");
}

#[test]
fn explain_config_names_the_resolved_config_path() {
    let r = explain_config_report();
    let path = aterm_types::dirs::aterm_config_path()
        .expect("APPDATA or HOME is set on a test box")
        .display()
        .to_string();
    assert!(
        r.contains(&format!("\n\naterm.toml: {path}\n")),
        "explain-config missing the resolved path {path:?}\n{r}"
    );
}

#[test]
fn the_privacy_row_is_a_note_and_never_moves_the_exit_code() {
    let ok = |privacy: &PrivacyFacts| {
        doctor_checks_with_origin(
            "/bin/sh",
            ShellCheck::Runs,
            None,
            privacy,
            &no_debugger(),
            &no_devtools(),
            &no_census(),
            &no_recovery(),
            &no_exceptions(),
            &MachineFacts::absent(),
        )
    };

    for (facts, mark) in [
        (
            measured(FdaState::Granted, DrClass::Identity, "aterm"),
            "ok",
        ),
        (
            measured(FdaState::Denied, DrClass::Identity, "iTerm2"),
            "note",
        ),
        (
            measured(FdaState::Granted, DrClass::Cdhash, "aterm (dev)"),
            "note",
        ),
        (unmeasured(), "note"),
    ] {
        let (r, code) = ok(&facts);
        assert_eq!(code, 0, "a privacy row never fails doctor\n{r}");
        assert!(r.contains("health: OK"), "{r}");
        assert_eq!(mark_of(&r, "privacy:"), mark, "{r}");
    }

    let (r, code) = doctor_checks_with_origin(
        "/no/such/shell",
        ShellCheck::Missing,
        None,
        &unmeasured(),
        &no_debugger(),
        &no_devtools(),
        &no_census(),
        &no_recovery(),
        &no_exceptions(),
        &MachineFacts::absent(),
    );
    assert_eq!(code, 1, "{r}");
    assert!(r.contains("health: FAIL"), "{r}");
    assert_eq!(mark_of(&r, "privacy:"), "note", "{r}");
}

/// The `recovery:` row: an empty ledger is ok and says where the record will write; a
/// recorded kill is a note that names it, never a fail; no log directory is
/// "not measured", never "none".
#[cfg(unix)]
#[test]
fn the_recovery_row_reports_the_ledger_and_never_fails() {
    use aterm_update::recovery_ledger::{
        EndClass, LaunchKind, Pressure, PrevRun, Row, Tri, Uptime,
    };
    let check = |recovery: RecoveryFacts| {
        doctor_checks_with_origin(
            "/bin/sh",
            ShellCheck::Runs,
            None,
            &unmeasured(),
            &no_debugger(),
            &no_devtools(),
            &no_census(),
            &recovery,
            &no_exceptions(),
            &MachineFacts::absent(),
        )
    };
    let (r, code) = check(no_recovery());
    assert_eq!(code, 0, "{r}");
    assert_eq!(mark_of(&r, "recovery:"), "ok", "{r}");

    let killed = Row {
        at: 1_790_500_000,
        pid: 2,
        started: 1_790_499_990,
        started_us: None,
        build: 1_790_400_000,
        version: "0.94.0".into(),
        launch: LaunchKind::Cold,
        prev: PrevRun {
            pid: Some(1),
            build: Some(1_790_305_290),
            version: Some("0.93.0".into()),
            class: EndClass::Killed,
            uptime: Some(Uptime {
                secs: 7200,
                at_least: true,
            }),
            stall: Tri::Yes,
            pressure: Pressure::Normal,
            trial: 0,
        },
        keeper: None,
        restores: None,
    };
    let path = std::path::PathBuf::from("/logs/recovery-ledger.log");
    let read = |rows, skipped| aterm_update::recovery_ledger::Checked { rows, skipped };
    let (r, code) = check(Ok((read(vec![killed], 0), path.clone())));
    assert_eq!(code, 0, "a past kill never fails doctor: {r}");
    assert_eq!(mark_of(&r, "recovery:"), "note", "{r}");
    assert!(
        r.contains("1790305290"),
        "the row names the killed build: {r}"
    );
    assert_eq!(r.matches("recovery:").count(), 1, "{r}");

    let (r, code) = check(Err("no log folder, so there is no ledger".to_owned()));
    assert_eq!(code, 0, "{r}");
    assert_eq!(mark_of(&r, "recovery:"), "note", "{r}");
    assert_eq!(r.matches("recovery:").count(), 1, "{r}");

    // A row a newer aterm wrote is skipped and named; the row is a note, measured.
    let (r, code) = check(Ok((read(Vec::new(), 2), path)));
    assert_eq!(code, 0, "{r}");
    assert_eq!(mark_of(&r, "recovery:"), "note", "{r}");
    assert!(
        r.contains("2 ledger rows this build cannot read, skipped"),
        "{r}"
    );
    let recovery = r
        .lines()
        .find(|l| l.starts_with("recovery:"))
        .unwrap_or_default();
    assert!(!recovery.contains("not measured"), "{r}");
}

#[test]
fn the_instances_row_names_what_nothing_reaches_and_never_fails() {
    use aterm_ctl::census::{Census, Program, Unreached};
    let check = |census: Result<Census, String>| {
        doctor_checks_with_origin(
            "/bin/sh",
            ShellCheck::Runs,
            None,
            &unmeasured(),
            &no_debugger(),
            &no_devtools(),
            &census,
            &no_recovery(),
            &no_exceptions(),
            &MachineFacts::absent(),
        )
    };
    let census = |instances: usize, unreached: Vec<Unreached>| {
        Ok(Census {
            dir: "/run/aterm".to_string(),
            instances,
            unreached,
            programs: Vec::new(),
        })
    };

    for n in [0, 2] {
        let (r, code) = check(census(n, Vec::new()));
        assert_eq!(code, 0, "{r}");
        assert_eq!(mark_of(&r, "instances:"), "ok", "{r}");
    }

    let orphan = Unreached {
        pid: 67548,
        age: "11d 22h".to_string(),
        command: "./aterm-before --headless".to_string(),
        socketless: false,
    };
    let deliberate = Unreached {
        pid: 68000,
        age: "5s".to_string(),
        command: "aterm --headless --no-control-sock".to_string(),
        socketless: true,
    };
    let (r, code) = check(census(3, vec![orphan, deliberate]));
    assert_eq!(
        code, 0,
        "an unreachable instance is a note, not a failure\n{r}"
    );
    assert!(r.contains("health: OK"), "{r}");
    assert_eq!(mark_of(&r, "instances:"), "note", "{r}");
    assert!(
        r.contains("pid 67548") && r.contains("pid 68000"),
        "each unreached instance is named: {r}"
    );

    let (r, code) = check(Err("the process table could not be read".to_string()));
    assert_eq!(code, 0, "{r}");
    assert_eq!(
        mark_of(&r, "instances:"),
        "note",
        "a census that was not taken never reads as none\n{r}"
    );

    let program = |pid: u32, name: &str, adopted: bool| Program {
        pid,
        name: name.to_string(),
        age: "3h 05m".to_string(),
        adopted,
        build: adopted,
        deleted: false,
        unsupervised: false,
        token: None,
    };
    let with_programs = |programs: Vec<Program>| {
        Ok(Census {
            dir: "/run/aterm".to_string(),
            instances: 1,
            unreached: Vec::new(),
            programs,
        })
    };
    let (r, code) = check(with_programs(vec![
        program(71100, "aterm link serve", false),
        program(71101, "aterm link serve", false),
    ]));
    assert_eq!(code, 0, "{r}");
    assert_eq!(mark_of(&r, "instances:"), "ok", "{r}");
    assert!(
        r.contains("2 other aterm programs run as you: aterm link serve ×2"),
        "{r}"
    );
    assert!(
        !r.contains("pid 71100"),
        "a program whose launcher lives is counted, not named: {r}"
    );

    let (r, code) = check(with_programs(vec![
        program(71100, "aterm link serve", false),
        program(71102, "aterm link broker", true),
        Program {
            build: false,
            ..program(71103, "aterm pkg update", true)
        },
    ]));
    assert_eq!(code, 0, "{r}");
    assert_eq!(
        mark_of(&r, "instances:"),
        "ok",
        "an adopted program is a fact, not a leftover: aterm detaches passes of its own\n{r}"
    );
    assert!(r.contains("pid 71102 `aterm link broker`"), "{r}");
    assert!(r.contains("pid 71103 `aterm pkg update`"), "{r}");
    assert!(
        r.contains("its launcher exited and launchd adopted it"),
        "{r}"
    );
    assert!(!r.contains("nothing supervises"), "{r}");
    assert!(r.contains("from a build directory"), "{r}");
    assert!(
        r.contains(
            "3 other aterm programs run as you: aterm link broker ×1, aterm link serve ×1, \
             aterm pkg update ×1"
        ),
        "{r}"
    );
    assert!(
        r.contains(
            "pid 71102 `aterm link broker` (up 3h 05m): its launcher exited and launchd \
             adopted it; from a build directory\n"
        ),
        "{r}"
    );

    let (r, code) = check(with_programs(vec![
        Program {
            build: true,
            deleted: true,
            ..program(71104, "aterm link broker", false)
        },
        Program {
            build: true,
            ..program(71105, "aterm link serve", false)
        },
        Program {
            deleted: true,
            ..program(71106, "aterm", false)
        },
    ]));
    assert_eq!(code, 0, "{r}");
    assert_eq!(
        mark_of(&r, "instances:"),
        "ok",
        "a deleted binary is a fact\n{r}"
    );
    assert!(
        r.contains(
            "pid 71104 `aterm link broker` (up 3h 05m): from a build directory; running a \
             deleted binary"
        ),
        "a build's program whose binary is gone with its worktree is named, adopted or not: \
         {r}"
    );
    assert!(
        !r.contains("pid 71105"),
        "a build's program whose launcher lives is counted, not named: {r}"
    );
    assert!(
        !r.contains("pid 71106"),
        "an installed program whose binary an update replaced is counted, not named: {r}"
    );

    let many: Vec<Program> = (0..8)
        .map(|i| program(72000 + i, &format!("aterm v{i}"), false))
        .collect();
    let (r, _) = check(with_programs(many));
    assert!(
        r.contains("aterm v5 ×1, …"),
        "at most six names, then a cut: {r}"
    );
    assert!(!r.contains("aterm v6"), "{r}");
}

/// A link broker launchd adopted, as the leftover view's reader gives its facts: running a
/// build under a verify run's roots, holding its listener, leading its own group with no
/// terminal; `runs` says how the run records tie it.
fn adopted_broker(runs: Vec<aterm_vitals::left::RunTie>) -> aterm_vitals::left::Facts {
    aterm_vitals::left::Facts {
        pid: 71102,
        ppid: 1,
        lineage: Some(aterm_vitals::left::Lin {
            uniqueid: 310,
            parent: 1,
            pgid: 71102,
            launcher_version: Some(100),
            tty: None,
        }),
        token: Some("71102@1792098840004410:331".to_owned()),
        exe: Some("/w/wt-a/target/debug/aterm".to_owned()),
        exe_under_root: !runs.is_empty(),
        thread_ran: Some(true),
        listener: Some(true),
        sandboxed: Some(false),
        by_design: Some(false),
        aterm: true,
        runs,
        tty_master_held: Some(false),
        ..aterm_vitals::left::Facts::default()
    }
}

/// The dead verify run whose run id an adopted broker carries, its executable under the
/// run's roots.
fn dead_runs_tie() -> aterm_vitals::left::RunTie {
    aterm_vitals::left::RunTie {
        id: "41983-1792163800331520".to_owned(),
        run: true,
        tree: true,
        dead: true,
        ..aterm_vitals::left::RunTie::default()
    }
}

/// launchd's pid version in these facts: the broker's launcher was another process.
const LAUNCHD_VERSION: Option<i32> = Some(1026);

/// An unsupervised broker a dead verify run's record proves its own (W-run with C-tree)
/// gets the token the leftover view offers, and the `instances:` row says to end it with
/// `aterm ctl end` and that token; the fabric's own broker is named as adopted only. No row
/// prints a pid to `kill`.
#[test]
fn an_unsupervised_broker_from_a_dead_run_is_named_with_its_token() {
    use aterm_ctl::census::{BROKER, Census, Program, supervise};
    let facts = adopted_broker(vec![dead_runs_tie()]);
    let token = super::offered_token(&facts, LAUNCHD_VERSION, 9000);
    assert_eq!(token.as_deref(), Some("71102@1792098840004410:331"));
    let program = |pid: u32| Program {
        pid,
        name: BROKER.to_owned(),
        age: "10h 02m".to_owned(),
        adopted: true,
        build: true,
        deleted: false,
        unsupervised: false,
        token: None,
    };
    let mut census = Census {
        dir: "/run/aterm".to_owned(),
        instances: 1,
        unreached: Vec::new(),
        programs: vec![program(71101), program(71102)],
    };
    supervise(&mut census, Some(71101), &|pid| {
        (pid == 71102).then(|| token.clone()).flatten()
    });
    let (mark, fact) = super::instances_row(&Ok(census));
    assert_eq!(mark, Mark::Ok, "{fact}");
    assert!(
        fact.contains(
            "pid 71102 `aterm link broker` (up 10h 02m): its launcher exited and launchd \
             adopted it; unsupervised: it is not the one broker found serving the fabric's \
             socket; from a build directory; end it with `aterm ctl end \
             71102@1792098840004410:331`"
        ),
        "{fact}"
    );
    assert!(
        fact.contains(
            "pid 71101 `aterm link broker` (up 10h 02m): its launcher exited and launchd \
             adopted it; from a build directory\n"
        ),
        "the fabric's own broker is not unsupervised: {fact}"
    );
    assert!(!fact.contains("kill"), "{fact}");
}

/// An unsupervised broker no witness proves (no run record ties it, it writes no graph
/// entry) is offered no token: it holds a listener and leads its own group, so the view names
/// it and offers nothing, and nothing is printed to end it with. NEGATIVE CONTROL: give every
/// one its identity token, and this one gets one.
#[test]
fn an_unsupervised_broker_no_witness_proves_gets_no_token() {
    assert_eq!(
        super::offered_token(&adopted_broker(Vec::new()), LAUNCHD_VERSION, 9000),
        None
    );
    let live = aterm_vitals::left::RunTie {
        dead: false,
        ..dead_runs_tie()
    };
    assert_eq!(
        super::offered_token(&adopted_broker(vec![live]), LAUNCHD_VERSION, 9000),
        None,
        "a live run's tie proves nothing yet"
    );
    let mut census = aterm_ctl::census::Census {
        dir: "/run/aterm".to_owned(),
        instances: 1,
        unreached: Vec::new(),
        programs: vec![aterm_ctl::census::Program {
            pid: 71102,
            name: aterm_ctl::census::BROKER.to_owned(),
            age: "1h 00m".to_owned(),
            adopted: true,
            build: false,
            deleted: false,
            unsupervised: false,
            token: None,
        }],
    };
    aterm_ctl::census::supervise(&mut census, None, &|_| {
        super::offered_token(&adopted_broker(Vec::new()), LAUNCHD_VERSION, 9000)
    });
    let (_, fact) = super::instances_row(&Ok(census));
    assert!(fact.contains("unsupervised"), "{fact}");
    assert!(!fact.contains("aterm ctl end"), "{fact}");
}

#[test]
fn the_front_door_verbs_name_a_program() {
    use aterm_ctl::census::Verb as Census;
    for word in ["link", "ctl", "doctor", "help", "hook"] {
        assert_eq!(front_door_verb(word), Some(Census::Program), "{word}");
    }
    for flag in [
        "-h",
        "--help",
        "-V",
        "--version",
        "--completions",
        "--completions=zsh",
    ] {
        assert_eq!(
            front_door_verb(flag),
            Some(Census::Program),
            "the front door answers {flag} before it routes a window"
        );
    }
    for word in ["new-tab", "new-window", "split-pane"] {
        assert_eq!(front_door_verb(word), Some(Census::Window), "{word}");
    }
    for word in ["frobnicate", "--headless", "--completion", ""] {
        assert_eq!(front_door_verb(word), None, "{word}");
    }
    for (name, first) in [
        ("aterm-ctl", "new-window"),
        ("atpkg", "new-tab"),
        ("abrew", "--real"),
        ("atpkg-abrew", ""),
        ("agh", ""),
        ("aterm-fleet", ""),
        ("aterm-drive", ""),
        ("aterm-askpass", ""),
        ("aterm-machines-askpass", "password: "),
        ("aterm-link", "broker"),
        ("aterm-open-url", "https://claude.ai/x"),
        ("amacs", ""),
    ] {
        assert_eq!(
            front_door_alias(name, first),
            Some(Census::Program),
            "the front door hands {name} to its own program"
        );
    }
    assert_eq!(front_door_alias("aterm-gui", ""), Some(Census::Window));
    assert_eq!(
        front_door_alias("aterm-windowed", "new-tab"),
        Some(Census::Window)
    );
    for (name, first) in [
        ("aterm", "new-window"),
        ("aterm-cli", ""),
        ("a-renamed-copy", ""),
        ("aterm-gui", aterm_containment::consent::OPEN_PROBE_VERB),
    ] {
        assert_eq!(
            front_door_alias(name, first),
            None,
            "{name} {first}: the front door's own route"
        );
    }
}

/// `doctor_report` from a test binary, outside an `.app` bundle, measures nothing: the
/// privacy, debugger, devtools and Spotlight rows read `not measured`, and the stale-copy
/// prune does not run.
#[cfg(target_os = "macos")]
#[test]
fn doctor_run_from_a_test_binary_measures_nothing() {
    let (r, _) = doctor_report(true);
    let row = |label: &str| {
        r.lines()
            .find(|l| l.starts_with(label))
            .unwrap_or_else(|| panic!("no {label} row in\n{r}"))
    };
    for label in ["privacy:", "debugger:", "devtools:"] {
        assert_eq!(mark_of(&r, label), "note", "{r}");
        assert!(
            row(label).contains("not measured"),
            "the fence let a {label} verdict through\n{r}"
        );
    }
    assert!(
        row("copies:").contains("not checked"),
        "a test binary must never prune\n{r}"
    );
    assert!(
        row("spotlight:").contains("not measured (a test binary)"),
        "{r}"
    );
}

#[test]
fn the_spotlight_row_is_a_note_at_worst() {
    let report = |spotlight| {
        doctor_checks_with_origin(
            "/bin/sh",
            ShellCheck::Runs,
            None,
            &unmeasured(),
            &no_debugger(),
            &DevtoolsFacts::absent(),
            &no_census(),
            &no_recovery(),
            &no_exceptions(),
            &MachineFacts {
                spotlight,
                ..MachineFacts::absent()
            },
        )
    };
    let lit = crate::spotlight::Spotlit {
        docs_kept: None,
        builds: vec![("~/w/target".into(), 12)],
        xpy: Vec::new(),
        rustup: None,
    };
    let (r, code) = report(Some(Ok(lit)));
    assert_eq!(code, 0, "{r}");
    assert_eq!(mark_of(&r, "spotlight:"), "note", "{r}");
    assert!(r.contains("~/w/target 12"), "{r}");
    assert!(r.contains("health: OK\n"), "{r}");
    let clean = crate::spotlight::Spotlit {
        docs_kept: None,
        builds: vec![],
        xpy: Vec::new(),
        rustup: None,
    };
    let (r, _) = report(Some(Ok(clean)));
    assert_eq!(mark_of(&r, "spotlight:"), "ok", "{r}");
    let (r, _) = report(None);
    assert!(!r.contains("spotlight:"), "{r}");
}

/// The `lan:` row's mark and whether it is measured: ok only for a current install whose
/// socket answers, a warning for a current one that does not answer, a measured note for an
/// owed install, and an unmeasured note for one the running broker adopts by itself and for
/// a reading that could not be made. NEGATIVE CONTROL: off macOS (no fact) there is no row,
/// in the report too.
#[test]
fn the_lan_row_marks_the_install_and_whether_the_socket_answers() {
    let row = |install, answers| {
        let rows = super::lan_rows(&MachineFacts {
            lan: Some(super::LanFact { install, answers }),
            ..MachineFacts::absent()
        });
        assert_eq!(rows.len(), 1);
        (rows[0].mark, rows[0].unmeasured)
    };
    let install = |current, owed| {
        Some(Ok(super::LanInstall {
            current,
            owed,
            words: String::new(),
        }))
    };
    assert_eq!(row(install(true, false), true), (Mark::Ok, false));
    assert_eq!(row(install(true, false), false), (Mark::Warn, false));
    assert_eq!(row(install(false, true), false), (Mark::Note, false));
    assert_eq!(row(install(false, false), true), (Mark::Note, true));
    assert_eq!(row(Some(Err(String::new())), true), (Mark::Note, true));
    assert_eq!(row(None, false), (Mark::Note, true));
    assert!(super::lan_rows(&MachineFacts::absent()).is_empty());
    let report = doctor_checks_with_origin(
        "/bin/sh",
        ShellCheck::Runs,
        None,
        &unmeasured(),
        &no_debugger(),
        &DevtoolsFacts::absent(),
        &no_census(),
        &no_recovery(),
        &no_exceptions(),
        &MachineFacts::absent(),
    )
    .0;
    assert!(!report.contains("lan:"), "{report}");
}

#[test]
fn the_syspolicyd_row_warns_while_the_pool_is_full() {
    let report = |program_check| {
        doctor_checks_with_origin(
            "/bin/sh",
            ShellCheck::Runs,
            None,
            &unmeasured(),
            &no_debugger(),
            &DevtoolsFacts::absent(),
            &no_census(),
            &no_recovery(),
            &no_exceptions(),
            &MachineFacts {
                program_check,
                ..MachineFacts::absent()
            },
        )
    };
    let reading = |workers, at_limit| {
        Some(aterm_uds::program_check::Look::Read(
            aterm_uds::program_check::Reading {
                daemon: aterm_uds::program_check::Daemon {
                    pid: 690,
                    start_s: 1,
                    start_us: 0,
                },
                workers,
                at_limit,
                limit: Some(90),
            },
        ))
    };
    let (r, code) = report(reading(4, false));
    assert_eq!(code, 0, "{r}");
    assert_eq!(mark_of(&r, "syspolicyd:"), "ok", "{r}");
    assert!(r.contains("4 pool threads against a limit of 90"), "{r}");
    let (r, code) = report(reading(95, true));
    assert_eq!(code, 0, "{r}");
    assert_eq!(mark_of(&r, "syspolicyd:"), "warn", "{r}");
    assert!(r.contains("restarts"), "{r}");
    assert!(r.contains("at the kernel's thread limit"), "{r}");
    let (r, _) = report(reading(100, false));
    assert_eq!(mark_of(&r, "syspolicyd:"), "ok", "{r}");
    let (r, _) = report(Some(aterm_uds::program_check::Look::Unread));
    assert_eq!(mark_of(&r, "syspolicyd:"), "note", "{r}");
    let (r, _) = report(Some(aterm_uds::program_check::Look::NoneRunning));
    assert_eq!(mark_of(&r, "syspolicyd:"), "ok", "{r}");
    let (r, _) = report(None);
    assert!(!r.contains("syspolicyd:"), "{r}");
}

/// The `services:` row names each of macOS's services past the bar and the restart aterm
/// knows for it, or says none exists; under the bar it names the largest; once read, it
/// says what aterm keeps from fseventsd. NEGATIVE CONTROL: a look-alike of fseventsd at
/// another image gets no restart, and a row that could not read claims nothing.
#[test]
fn the_services_row_names_a_service_past_the_bar_and_its_restart() {
    use aterm_uds::services::{Reading, Service};
    let report = |services| {
        doctor_checks_with_origin(
            "/bin/sh",
            ShellCheck::Runs,
            None,
            &unmeasured(),
            &no_debugger(),
            &DevtoolsFacts::absent(),
            &no_census(),
            &no_recovery(),
            &no_exceptions(),
            &MachineFacts {
                services,
                ..MachineFacts::absent()
            },
        )
    };
    let service = |name: &str, path: &str, footprint: u64| Service {
        name: name.into(),
        path: Some(path.into()),
        live: 1,
        footprint,
        ..Service::default()
    };
    let fseventsd = aterm_vitals::services::FSEVENTSD.path;
    let read = |services| {
        Some(Ok(Reading {
            total: 128 << 30,
            services,
            ..Reading::default()
        }))
    };
    let (r, code) = report(read(vec![
        service("fseventsd", fseventsd, 152_000_000_000),
        service("mds", "/System/mds", 1_330_000_000),
    ]));
    assert_eq!(code, 0, "{r}");
    assert_eq!(mark_of(&r, "services:"), "warn", "{r}");
    assert!(
        r.contains("fseventsd holds 141.6 GiB, past the 8.0 GiB any service needs"),
        "{r}"
    );
    assert!(
        r.contains("restart it: `aterm init --only init.fseventsd` (one password prompt)"),
        "{r}"
    );
    let (r, _) = report(read(vec![service("fseventsd", "/tmp/fseventsd", 40 << 30)]));
    assert!(r.contains("aterm knows no safe restart for it"), "{r}");
    let (r, _) = report(read(vec![service("mds", "/System/mds", 1_330_000_000)]));
    assert_eq!(mark_of(&r, "services:"), "ok", "{r}");
    assert!(r.contains("the largest, mds, holds 1.2 GiB"), "{r}");
    assert!(r.contains("whose file events fseventsd drops"), "{r}");
    let with_shim = |root: &str| {
        doctor_checks_with_origin(
            "/bin/sh",
            ShellCheck::Runs,
            None,
            &unmeasured(),
            &no_debugger(),
            &DevtoolsFacts::absent(),
            &no_census(),
            &no_recovery(),
            &no_exceptions(),
            &MachineFacts {
                services: read(vec![service("mds", "/System/mds", 1_330_000_000)]),
                shim_cache: Some(root.into()),
                ..MachineFacts::absent()
            },
        )
        .0
    };
    let quiet = with_shim("/Users//u/Library/Caches/aterm/.ubd/cargo-build.noindex");
    assert!(
        quiet.contains("prevented: aterm's build cache, "),
        "{quiet}"
    );
    let old = with_shim("/Users//u/Library/Caches/aterm/cargo-build.noindex");
    assert!(
        old.contains("prevented in part: ")
            && old.contains("the targo shim on PATH still builds in /Users//u/")
            && !old.contains("aterm's build cache, "),
        "an old shim's cache is claimed for nothing: {old}"
    );
    let (r, _) = report(Some(Err("unread".into())));
    assert_eq!(mark_of(&r, "services:"), "note", "{r}");
    assert!(
        !r.contains("prevented:"),
        "nothing claimed of an unread row: {r}"
    );
    let (r, _) = report(None);
    assert!(!r.contains("services:"), "{r}");
}

#[test]
fn the_fabric_row_is_a_counted_warning_never_a_failure() {
    let report = |fabric: Option<(Mark, String)>| {
        doctor_checks_with_origin(
            "/bin/sh",
            ShellCheck::Runs,
            None,
            &unmeasured(),
            &no_debugger(),
            &no_devtools(),
            &no_census(),
            &no_recovery(),
            &no_exceptions(),
            &MachineFacts {
                fabric: fabric.map(|(mark, fact)| (mark, fact, false, None)),
                ..MachineFacts::absent()
            },
        )
    };
    let (none, code) = report(None);
    assert_eq!(code, 0);
    assert!(!none.lines().any(|l| l.starts_with("fabric:")), "{none}");
    assert!(!none.contains("fabric"), "{none}");

    let down = "the broker at /r/bus.sock does not answer (connect: refused) — `aterm \
                fabric doctor` names the fix";
    let (r, code) = report(Some((Mark::Warn, down.to_string())));
    assert_eq!(code, 0, "a broker that does not answer fails no shell\n{r}");
    let rows: Vec<&str> = r.lines().collect();
    let at = rows
        .iter()
        .position(|l| l.starts_with("fabric:"))
        .expect("the row");
    assert!(rows[at - 1].starts_with("instances: "), "{r}");
    assert_eq!(rows[at], format!("fabric:     warn  {down}"));
    assert!(r.contains("health: OK — 1 warning (fabric)\n"), "{r}");

    let (r, code) = report(Some((Mark::Ok, "the broker answers".to_string())));
    assert_eq!(code, 0);
    assert_eq!(mark_of(&r, "fabric:"), "ok", "{r}");
    assert!(r.contains("health: OK\n"), "{r}");
}

#[cfg(unix)]
#[test]
fn the_fabric_weights_are_never_a_failure() {
    use aterm_link::fabric::SummaryLevel;
    assert_eq!(fabric_mark(SummaryLevel::Ok), Mark::Ok);
    assert_eq!(fabric_mark(SummaryLevel::Note), Mark::Note);
    assert_eq!(fabric_mark(SummaryLevel::Warn), Mark::Warn);
    for level in [SummaryLevel::Ok, SummaryLevel::Note, SummaryLevel::Warn] {
        assert!(!fabric_mark(level).is_fail());
    }
}

/// THE LEFTOVERS ROW IS WITHHELD WHILE THE CHECK IS FULL: its reads take each candidate's
/// marker variables, an environment read, which would wait on a program the full pool holds
/// mid-start, so the row says why and its probe is never asked. NEGATIVE CONTROL: read it
/// anyway, and the probe is asked.
#[cfg(unix)]
#[test]
fn leftovers_row_is_withheld_while_the_check_is_full() {
    use aterm_uds::program_check::{Daemon, Look, Reading};
    let full = || {
        Some(Look::Read(Reading {
            daemon: Daemon {
                pid: 690,
                start_s: 1,
                start_us: 0,
            },
            workers: 95,
            at_limit: true,
            limit: Some(90),
        }))
    };
    let asked = std::sync::atomic::AtomicUsize::new(0);
    let leftovers = || {
        asked.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(super::doctor_leftovers::Leftovers::default())
    };
    let ports = super::FactReads {
        program_check: &full,
        devtools: &|_, _| (DevtoolsFacts::absent(), None),
        spotlight: &|| None,
        services: &|| None,
        privacy: &unmeasured,
        debugger: &no_debugger,
        instances: &no_census,
        fabric: &|_| None,
        awake: &|| None,
        offered: &|_| None,
        leftovers: &leftovers,
        person_here: &|| false,
    };
    let rows = super::facts_rows_over(false, &|label| label == "leftovers:", &ports).rows;
    let row = rows
        .iter()
        .find(|r| r.label == "leftovers:")
        .expect("the row is said, withheld");
    assert!(
        row.fact
            .starts_with("not measured (macOS's program check (syspolicyd) is at the kernel's"),
        "{}",
        row.fact
    );
    assert!(row.unmeasured);
    assert_eq!(asked.load(std::sync::atomic::Ordering::SeqCst), 0);
}

/// While the program check's pool is at the kernel's limit, doctor lists no
/// program's command line, and the reason the census and the fabric rows give carries
/// the reading itself.
#[cfg(unix)]
#[test]
fn a_full_program_check_withholds_the_argument_reads() {
    use aterm_uds::program_check::{Daemon, Look, Reading};
    let reading = |workers, at_limit| {
        Look::Read(Reading {
            daemon: Daemon {
                pid: 690,
                start_s: 1,
                start_us: 0,
            },
            workers,
            at_limit,
            limit: Some(90),
        })
    };
    let why = args_read_withheld(Some(&reading(95, true))).expect("a full pool withholds");
    assert!(
        why.contains("95 pool threads against a limit of 90")
            && why.contains("every program's command line"),
        "{why}"
    );
    for look in [
        Some(&reading(4, false)),
        Some(&reading(100, false)),
        Some(&Look::Unread),
        Some(&Look::NoneRunning),
        None,
    ] {
        assert_eq!(args_read_withheld(look), None, "{look:?}");
    }
}

/// Doctor's `devtools:` facts carry this user's programs macOS's program check holds now, read
/// from the kernel beside the list (a test binary's list reads `not measured`; the count is
/// read all the same).
#[cfg(target_os = "macos")]
#[test]
fn doctor_reads_the_live_wait_beside_the_list() {
    let facts = super::devtools_facts(false, false)
        .0
        .facts
        .expect("macOS has a Developer Tools list");
    assert!(facts.program_check.is_some(), "{facts:?}");
}

#[test]
fn the_devtools_row_is_a_counted_warning_and_says_what_doctor_did() {
    use aterm_containment::consent::{
        DevToolList, DevToolListLabel, DevToolListing, DevToolProbe, DevToolProbeLabel,
    };
    let list = |listing| DevToolList {
        listing,
        label: DevToolListLabel::Read,
        modified_at_s: Some(1),
        rows: Some(2),
    };
    let devtools = |listing, did: Option<&str>| DevtoolsFacts {
        facts: Some(atpkg::devtools::Facts {
            subject: Some("com.aterm.aterm".to_string()),
            list: list(listing),
            log: DevToolProbe::refused(DevToolProbeLabel::NoRecentRequest),
            other_responsible: None,
            program_check: None,
        }),
        did: did.map(str::to_string),
    };
    let machine = MachineFacts {
        copies: Some(atpkg::copies::Pruned::Done {
            moved: vec![(
                std::path::PathBuf::from("/Applications/aterm-prev-1.app"),
                aterm_containment::consent::Retired::Moved,
            )],
            kept: Vec::new(),
        }),
        console_host: None,
        fabric: None,
        spotlight: None,
        program_check: None,
        services: None,
        shim_cache: None,
        network: None,
        awake: None,
        lan: None,
        leftovers: None,
    };
    let report = |d: &DevtoolsFacts| {
        doctor_checks_with_origin(
            "/bin/sh",
            ShellCheck::Runs,
            None,
            &unmeasured(),
            &no_debugger(),
            d,
            &no_census(),
            &no_recovery(),
            &no_exceptions(),
            &machine,
        )
    };
    // The one hand-over word a row carries (nobody at the console, where no pane may
    // open), so this pins where it lands, not its spelling.
    let off = devtools(DevToolListing::Off, None).facts.unwrap();
    let (did, due) =
        super::devtools_hand_over_due(true, &off, false, true, || Some("a remote shell"));
    assert!(!due, "away from the console nothing is handed over");
    let did = did.expect("the row says why");
    let did = did.as_str();
    let (r, code) = report(&devtools(DevToolListing::Off, Some(did)));
    assert_eq!(code, 0, "a warning never fails: {r}");
    assert_eq!(mark_of(&r, "devtools:"), "warn", "{r}");
    assert!(r.contains(did), "what doctor did is said:\n{r}");
    assert!(r.contains("1 warning (devtools)"), "{r}");
    assert_eq!(mark_of(&r, "copies:"), "note", "{r}");
    assert!(r.contains("aterm-prev-1.app"), "{r}");
    let (r, code) = report(&devtools(DevToolListing::On, None));
    assert_eq!(code, 0, "{r}");
    assert_eq!(mark_of(&r, "devtools:"), "ok", "{r}");
    assert!(r.contains("health: OK\n"), "{r}");
    let (r, _) = report(&no_devtools());
    assert!(!r.contains("devtools:"), "{r}");
}

/// A scripted [`super::DevtoolsReach`]: the window answers `answer`, a decline on record
/// dates `declined_at`, `before_wait` runs as the wait starts, and every act is logged in
/// order instead of done (nothing is asked, run, opened, written, waited for or printed).
struct Scripted {
    answer: super::WindowAnswer,
    /// What the window's permission walk answers.
    walk: super::WindowAnswer,
    /// The window is the app macOS holds responsible for doctor (aterm's own tab).
    responsible: bool,
    declined_at: Option<u64>,
    /// The wait sees the switch go on.
    switched: bool,
    /// What asking macOS about aterm again ends in.
    refreshed: Result<(), atpkg::devtools::RefreshFailed>,
    /// This user's programs macOS's program check holds once the switch reads on.
    held: Option<aterm_uds::process::HeldNow>,
    before_wait: Box<dyn Fn()>,
    log: std::cell::RefCell<Vec<String>>,
}

impl Scripted {
    fn new(answer: super::WindowAnswer) -> Self {
        Self {
            answer,
            walk: super::WindowAnswer::Reply("ERR unknown verb (try: help)".to_string()),
            responsible: true,
            declined_at: None,
            switched: true,
            refreshed: Ok(()),
            held: None,
            before_wait: Box::new(|| {}),
            log: std::cell::RefCell::new(Vec::new()),
        }
    }

    /// The hand-over for `facts`: `(did, opened, log)`.
    fn run(&self, facts: &atpkg::devtools::Facts) -> (String, bool, Vec<String>) {
        let ((did, opened), log) = self.with(|reach| super::devtools_hand_over(facts, reach));
        (did, opened, log)
    }

    fn with<R>(&self, act: impl FnOnce(&super::DevtoolsReach<'_>) -> R) -> (R, Vec<String>) {
        let note = |entry: String| self.log.borrow_mut().push(entry);
        let reach = super::DevtoolsReach {
            ask_walk: &|subjects| {
                note(format!("walk {subjects}"));
                self.walk.clone()
            },
            ask_window: &|| {
                note("ask".to_string());
                self.answer.clone()
            },
            list_terminal: &|| {
                note("spctl".to_string());
                Ok(())
            },
            open_pane: &|| {
                note("open".to_string());
                Ok(())
            },
            note_opened: &|| note("marker".to_string()),
            declined_at: &|| self.declined_at,
            await_on: &|| {
                (self.before_wait)();
                note("wait".to_string());
                self.switched
            },
            held_programs: &|| {
                note("held".to_string());
                self.held
            },
            refresh_verdict: &|| {
                note("refresh".to_string());
                self.refreshed.clone()
            },
            window_responsible: &|| self.responsible,
            open_fda_pane: &|| {
                note("open fda".to_string());
                Ok(())
            },
            await_fda_on: &|| {
                note("wait fda".to_string());
                self.switched
            },
            local_day: &|at| super::local_day_words(at, Some(0)),
            say: &|words| note(format!("say {words}")),
        };
        let out = act(&reach);
        (out, self.log.take())
    }
}

/// THE HAND-OVER IS DUE ONLY FOR A PERSON AT THE MAC'S CONSOLE, READING STDOUT (S5,
/// SAFE-1): a gather with `fix` leaves it due when aterm is not granted, the console's
/// presence reads a person, and stdout is a terminal. `--read-only` and `--json` (no
/// `fix`), a granted list (neither reads the presence), and a stdout that is no terminal
/// (a pipe, a file) leave nothing due and say nothing. Away from the console (an SSH
/// session, a coding agent's tab with a terminal of its own, no terminal to ask at), or
/// with the program check at its thread limit, nothing is due and the row says why.
/// NEGATIVE CONTROL: a hand-over judged by stdout and SSH alone is due for an agent's tab.
#[test]
fn the_hand_over_is_due_only_for_a_person_at_the_macs_own_terminal() {
    use aterm_containment::DevToolListing;
    let absent = devtools_facts_with(read_list(DevToolListing::Absent, 2));
    let granted = devtools_facts_with(read_list(DevToolListing::On, 2));
    assert_eq!(granted.grant(), aterm_containment::DevToolGrant::Granted);
    let here = || None;
    assert_eq!(
        super::devtools_hand_over_due(true, &absent, false, true, here),
        (None, true)
    );
    assert_eq!(
        super::devtools_hand_over_due(true, &absent, false, false, here),
        (None, false),
        "a stdout that is no terminal"
    );
    for (fix, facts) in [(false, &absent), (true, &granted)] {
        assert_eq!(
            super::devtools_hand_over_due(fix, facts, false, true, || unreachable!(
                "no hand-over could be due, so nobody's presence is read"
            )),
            (None, false),
            "fix={fix} grant={:?}",
            facts.grant()
        );
    }
    for (away, check_full, words) in [
        (
            Some("a coding agent started this"),
            false,
            "a coding agent started this",
        ),
        (
            Some("this is a remote shell"),
            false,
            "this is a remote shell",
        ),
        (None, true, "program check is at its thread limit"),
    ] {
        let (did, due) = super::devtools_hand_over_due(true, &absent, check_full, true, || away);
        assert!(!due, "{words}");
        assert!(did.is_some_and(|did| did.contains(words)), "{words}");
    }
}

/// Counting stand-ins for every probe [`super::facts_rows_over`] makes, each meeting the
/// others at `met` (a barrier of every threaded probe, or none), and the reads made.
struct CountedReads {
    met: Option<std::sync::Barrier>,
    reads: std::sync::Mutex<Vec<&'static str>>,
}

impl CountedReads {
    fn new(met: Option<std::sync::Barrier>) -> Self {
        Self {
            met,
            reads: std::sync::Mutex::new(Vec::new()),
        }
    }

    fn read(&self, what: &'static str) {
        self.reads.lock().unwrap().push(what);
        if let Some(met) = &self.met {
            met.wait();
        }
    }

    /// The rows `reads` names, over these stand-ins, and the probes they read (sorted).
    fn rows(&self, reads: &dyn Fn(&str) -> bool) -> (Vec<&'static str>, Vec<&'static str>) {
        let ports = super::FactReads {
            program_check: &|| None,
            devtools: &|_, _| {
                self.read("devtools");
                (DevtoolsFacts::absent(), None)
            },
            spotlight: &|| {
                self.read("spotlight");
                None
            },
            services: &|| {
                self.read("services");
                None
            },
            privacy: &|| {
                self.read("privacy");
                unmeasured()
            },
            debugger: &|| {
                self.read("debugger");
                no_debugger()
            },
            instances: &|| {
                self.read("instances");
                no_census()
            },
            fabric: &|_| {
                self.read("fabric");
                None
            },
            awake: &|| {
                self.read("awake");
                None
            },
            offered: &|_| {
                self.read("offered");
                None
            },
            leftovers: &|| {
                self.read("leftovers");
                Err("not read".to_owned())
            },
            person_here: &|| false,
        };
        let rows = super::facts_rows_over(false, reads, &ports).rows;
        let mut read = self.reads.lock().unwrap().clone();
        read.sort_unstable();
        (rows.iter().map(|row| row.label).collect(), read)
    }
}

/// THE `awake:` ROW SAYS WHAT MACOS HOLDS, NOT WHAT WAS SAVED: a mug that runs with no
/// assertion, and an on mode with no mug, are warnings that carry the recorded why; a
/// person's off is a note; only a listed assertion is ok. NEGATIVE CONTROL: no reading, no
/// row.
#[test]
fn the_awake_row_reads_the_assertion() {
    use aterm_acaffeine::held::Held;
    let row = |held: Held| {
        let facts = MachineFacts {
            awake: Some(held),
            ..MachineFacts::absent()
        };
        let rows = super::awake_rows(&facts);
        assert_eq!(rows.len(), 1);
        let row = rows.into_iter().next().expect("one row");
        assert_eq!(row.label, "awake:");
        (row.mark, row.fact, row.unmeasured)
    };
    assert_eq!(row(Held::Held { pid: 42 }).0, Mark::Ok);
    let (mark, fact, _) = row(Held::NotHeld {
        pid: 42,
        why: Some("macOS refused the keep-awake activity".to_owned()),
    });
    assert_eq!(mark, Mark::Warn);
    assert!(
        fact.contains("macOS refused the keep-awake activity"),
        "{fact}"
    );
    let (mark, fact, _) = row(Held::Stopped {
        why: Some("the menu bar refused the mug".to_owned()),
    });
    assert_eq!(mark, Mark::Warn);
    assert!(fact.contains("the menu bar refused the mug"), "{fact}");
    assert_eq!(row(Held::Off).0, Mark::Note);
    let paused = |lingering| Held::Paused {
        pid: 42,
        why: aterm_acaffeine::model::Pause::LidClosed,
        lingering,
    };
    let (mark, fact, _) = row(paused(false));
    assert_eq!(mark, Mark::Note, "a closed lid is no failure: {fact}");
    assert!(fact.contains("lid is closed"), "{fact}");
    assert!(fact.contains("aterm acaffeine lid on"), "{fact}");
    assert_eq!(row(paused(true)).0, Mark::Warn, "still listed while paused");
    let (mark, _, unmeasured) = row(Held::Unread {
        pid: 42,
        why: "the power manager did not answer".to_owned(),
    });
    assert_eq!((mark, unmeasured), (Mark::Note, true));
    assert!(super::awake_rows(&MachineFacts::absent()).is_empty());
}

/// `--ONLY NARROWS WHAT DOCTOR'S OWN ROWS READ`: the help's "shell usability alone" check
/// reads only the shell (no Developer Tools `log show`, no Spotlight `mdfind`, no
/// codesign); a word naming one row reads that row's probe alone. NEGATIVE CONTROL: with
/// no words every probe reads once.
#[test]
fn doctors_own_rows_read_only_what_the_words_name() {
    let shell_only = CountedReads::new(None);
    let (rows, read) =
        shell_only.rows(&|label| super::doctor::reads(&["base.shell.starts".to_owned()], label));
    assert_eq!(rows, ["shell:"]);
    assert_eq!(read, Vec::<&str>::new(), "the shell alone is read");

    let spotlight = CountedReads::new(None);
    let (rows, read) = spotlight
        .rows(&|label| super::doctor::reads(&["base.machine.spotlight".to_owned()], label));
    assert_eq!(rows, ["shell:"], "a Spotlight that read nothing has no row");
    assert_eq!(read, ["spotlight"]);

    let awake = CountedReads::new(None);
    let (_, read) =
        awake.rows(&|label| super::doctor::reads(&["base.machine.awake".to_owned()], label));
    assert_eq!(read, ["awake"]);

    let every = CountedReads::new(None);
    let (rows, read) = every.rows(&|label| super::doctor::reads(&[], label));
    assert_eq!(
        read,
        [
            "awake",
            "debugger",
            "devtools",
            "fabric",
            "instances",
            "leftovers",
            "privacy",
            "services",
            "spotlight"
        ],
        "each probe once"
    );
    assert!(
        rows.contains(&"instances:") && rows.contains(&"exceptions:"),
        "{rows:?}"
    );
}

/// DOCTOR'S OWN PROBES READ AT ONCE, NOT ONE AFTER ANOTHER: each threaded probe waits on
/// one barrier of them all, which only a gather that starts every probe before it joins
/// any passes. The one-minute wait is the hang detector a serial gather trips (its first
/// probe's wait never returns).
#[test]
fn doctors_own_probes_read_at_once() {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let all = CountedReads::new(Some(std::sync::Barrier::new(9)));
        let _ = tx.send(all.rows(&|label| super::doctor::reads(&[], label)).1);
    });
    let read = rx
        .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
        .expect("the probes never met: one waited for another to finish");
    assert_eq!(read.len(), 9, "{read:?}");
}

/// Where no thread starts, doctor's own probes read in the caller instead: the same rows,
/// each probe once. NEGATIVE CONTROL: the gather that has its threads reads the same.
#[test]
fn doctors_own_probes_read_in_the_caller_where_no_thread_starts() {
    let every = |label: &str| super::doctor::reads(&[], label);
    let threaded = CountedReads::new(None).rows(&every);
    let here = crate::beside::tests::refusing(|| CountedReads::new(None).rows(&every));
    assert_eq!(here, threaded);
    assert_eq!(here.1.len(), 9, "{here:?}");
}

fn devtools_facts_with(list: aterm_containment::DevToolList) -> atpkg::devtools::Facts {
    use aterm_containment::consent::{DevToolProbe, DevToolProbeLabel};
    atpkg::devtools::Facts {
        subject: Some("com.aterm.aterm".to_string()),
        list,
        log: DevToolProbe::refused(DevToolProbeLabel::NoRecentRequest),
        other_responsible: None,
        program_check: None,
    }
}

fn read_list(
    listing: aterm_containment::DevToolListing,
    rows: u32,
) -> aterm_containment::DevToolList {
    aterm_containment::DevToolList {
        listing,
        label: aterm_containment::DevToolListLabel::Read,
        modified_at_s: Some(1),
        rows: Some(rows),
    }
}

/// THE REPORT IS OUT, ON STDOUT, BEFORE THE HAND-OVER WAITS (S5): `aterm doctor` prints
/// its whole report before it hands anything over, so a wait for the switch holds no row
/// back and ctrl-c during it leaves the report printed. A NOT READY report (exit 1) goes
/// to stdout like a green one, and stderr carries only the hand-over's lines; a refusal
/// goes to stderr. The scripted wait panics unless the report is already out. NEGATIVE
/// CONTROL: the order the gather had (the hand-over first, the report after) trips it.
#[test]
fn the_report_is_out_on_stdout_before_the_hand_over_waits() {
    use super::WindowAnswer;
    use aterm_containment::DevToolListing;
    use std::cell::RefCell;
    use std::rc::Rc;
    let absent = devtools_facts_with(read_list(DevToolListing::Absent, 2));
    let report = "shell:      ok    /bin/sh (executable)\n\
                  readiness: NOT READY \u{2014} 1 satisfied, 0 not applicable, 0 pending \
                  activation, 1 unresolved\n";
    let waits_after = |out: &Rc<RefCell<String>>| {
        let mut script = Scripted::new(WindowAnswer::Unreachable);
        let out = Rc::clone(out);
        script.before_wait = Box::new(move || {
            assert!(
                out.borrow().contains("readiness: NOT READY"),
                "the hand-over waited before the report was out"
            );
        });
        script
    };

    let stdout = Rc::new(RefCell::new(String::new()));
    let stderr = RefCell::new(String::new());
    let script = waits_after(&stdout);
    let waited = std::cell::Cell::new(false);
    let hand_over = |missing: &super::Missing| {
        let facts = missing.devtools.as_ref().expect("Developer Tools is due");
        let ((), log) = script.with(|reach| super::devtools_hand_over_after(facts, reach));
        waited.set(log.iter().any(|entry| entry == "wait"));
        for line in log.iter().filter_map(|entry| entry.strip_prefix("say ")) {
            stderr.borrow_mut().push_str(&format!("{line}\n"));
        }
    };
    let ends = super::doctor::Ends {
        stdout: &|text| {
            stdout.borrow_mut().push_str(text);
            Ok(())
        },
        stderr: &|text| stderr.borrow_mut().push_str(text),
        hand_over: &hand_over,
    };
    let due = super::Missing {
        fda: false,
        devtools: Some(absent.clone()),
    };
    let status = super::doctor::answer_over(report, 1, true, Some(&due), &ends);
    assert_eq!(status, 1, "the report's own code");
    assert!(waited.get(), "no window: doctor waits for the switch");
    assert_eq!(*stdout.borrow(), report, "the whole report, on stdout");
    let said = stderr.take();
    assert!(!said.contains("readiness:"), "{said}");
    assert!(said.contains(super::DEVTOOLS_WAIT_LINE), "{said}");
    assert!(said.ends_with("; you switched it on\n"), "{said}");

    // A refusal is stderr's, and nothing is handed over without a hand-over due.
    stdout.borrow_mut().clear();
    let refusal = "aterm doctor: --only typo: no item, family or layer has that name\n";
    assert_eq!(
        super::doctor::answer_over(refusal, 2, false, None, &ends),
        2
    );
    assert_eq!(
        (stdout.borrow().as_str(), stderr.take()),
        ("", refusal.to_owned())
    );

    // NEGATIVE CONTROL: the hand-over before the report trips the scripted wait.
    let early = Rc::new(RefCell::new(String::new()));
    let first = waits_after(&early);
    let tripped = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        first.with(|reach| super::devtools_hand_over_after(&absent, reach));
    }));
    assert!(tripped.is_err(), "the check must trip on the old order");
}

/// Once the switch reads on, doctor asks macOS about aterm again. A prime macOS's backed-up
/// queue still holds has not settled, and doctor says so in those words, never that it
/// "could not": the grant may well take once the queue drains. A prime that failed says it
/// could not, with why.
#[test]
fn a_prime_that_did_not_settle_says_so_and_never_could_not() {
    use super::WindowAnswer;
    use aterm_containment::DevToolListing;
    use atpkg::devtools::RefreshFailed;
    let absent = devtools_facts_with(read_list(DevToolListing::Absent, 2));
    let said_after_refresh = |refreshed| {
        let mut scripted = Scripted::new(WindowAnswer::Unreachable);
        scripted.refreshed = refreshed;
        let (_, _, log) = scripted.run(&absent);
        let at = log
            .iter()
            .position(|entry| entry == "refresh")
            .unwrap_or_else(|| panic!("refresh in {log:?}"));
        log[at + 1..]
            .iter()
            .filter_map(|entry| entry.strip_prefix("say ").map(str::to_owned))
            .collect::<Vec<_>>()
    };
    let unsettled = said_after_refresh(Err(RefreshFailed::NotSettled(None)));
    assert_eq!(
        unsettled,
        [
            "aterm doctor: asking macOS about aterm again has not settled: macOS's queue is \
             backed up"
        ]
    );
    let kept = "aterm pkg __devtools-prime: not settled: macOS's queue is backed up; kept \
                /tmp/aterm-devtools-prime-ab since aterm-devtools-ab never showed it ran";
    let named = said_after_refresh(Err(RefreshFailed::NotSettled(Some(kept.into()))));
    assert_eq!(
        named,
        [format!(
            "aterm doctor: asking macOS about aterm again has not settled: {kept}"
        )],
        "the prime's own words name the copy it kept"
    );
    assert!(
        unsettled
            .iter()
            .chain(&named)
            .all(|line| !line.contains("could not")),
        "{unsettled:?} {named:?}"
    );
    assert_eq!(
        said_after_refresh(Err(RefreshFailed::Failed("codesign: denied".into()))),
        ["aterm doctor: could not make macOS ask about aterm again: codesign: denied"]
    );
    assert!(
        said_after_refresh(Ok(())).is_empty(),
        "a refresh that took says nothing"
    );
}

/// Doctor's prime is itself a new program, so once the switch reads on it reads the queue
/// again and starts none while it reads backed up, by the jobs engine's two halves over its
/// two counts: three of this user's programs held at macOS's program check at once, or one
/// held at the report's count and at this one. It says why in the devtools row's words and
/// what asks instead. Two held at the switch alone, one held at the report alone, or a count
/// that does not read, prove no queue, and doctor asks.
/// NEGATIVE CONTROL: a prime that does not read the queue starts behind three held programs,
/// and one that reads the switch's count alone starts behind one held at both.
#[test]
fn doctor_starts_no_prime_behind_a_backed_up_queue() {
    use super::WindowAnswer;
    use aterm_containment::DevToolListing;
    use aterm_uds::process::HeldNow;
    let one = HeldNow {
        count: 1,
        oldest_ms: 20 * 60_000,
    };
    let run_after = |before: Option<HeldNow>, held: Option<HeldNow>| {
        let mut absent = devtools_facts_with(read_list(DevToolListing::Absent, 2));
        absent.program_check = before;
        let mut scripted = Scripted::new(WindowAnswer::Unreachable);
        scripted.held = held;
        let (did, _, log) = scripted.run(&absent);
        assert!(did.ends_with("; you switched it on"), "{did}");
        let at = log
            .iter()
            .position(|entry| entry == "held")
            .unwrap_or_else(|| panic!("the queue is read in {log:?}"));
        assert!(
            log[..at].iter().any(|entry| entry == "wait"),
            "read once the switch reads on: {log:?}"
        );
        log[at + 1..].to_vec()
    };
    let queue = HeldNow {
        count: 3,
        oldest_ms: 21 * 60_000,
    };
    assert_eq!(
        run_after(None, Some(queue)),
        [
            "say aterm doctor: did not ask macOS about aterm again: now 3 programs of this user \
             wait for macOS's program check, the oldest started 21 m ago, and that ask is a \
             new program that would wait behind them; macOS asks again when aterm next starts"
        ],
        "no prime behind three held at once"
    );
    let later = HeldNow {
        count: 1,
        oldest_ms: 21 * 60_000,
    };
    assert_eq!(
        run_after(Some(one), Some(later)),
        [
            "say aterm doctor: did not ask macOS about aterm again: now 1 program of this user \
             waits for macOS's program check; it started 21 m ago, and that ask is a new \
             program that would wait behind them; macOS asks again when aterm next starts"
        ],
        "no prime behind one held at both readings"
    );
    let two = HeldNow {
        count: 2,
        oldest_ms: 21 * 60_000,
    };
    for (before, held) in [
        (None, Some(two)),
        (Some(HeldNow::default()), Some(two)),
        (Some(one), Some(HeldNow::default())),
        (Some(queue), None),
    ] {
        assert_eq!(
            run_after(before, held),
            ["refresh"],
            "{before:?} then {held:?} proves no queue"
        );
    }
}

/// `devtools_hand_over` asks the running window first (`now`) and opens System Settings
/// itself: with one line and no wait beside the window's panel, which confirms the switch;
/// with the whole banner and a wait where no panel is up. Every other answer is the
/// window's decision, and nothing opens.
#[test]
fn doctor_asks_the_window_first_and_opens_only_where_the_window_leaves_it() {
    use super::{DEVTOOLS_WAIT_LINE, WindowAnswer};
    use aterm_containment::DevToolListing;
    use atpkg::devtools as dt;
    let skips = aterm_containment::SpikeEvidence::MEASURED.devtool_skips_first_run_check;
    let absent = devtools_facts_with(read_list(DevToolListing::Absent, 2));
    let reply = |line: &str| WindowAnswer::Reply(line.to_string());
    let said = |log: &[String]| -> String {
        log.iter()
            .filter_map(|entry| entry.strip_prefix("say "))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let opened_words = dt::handled_words(&dt::Handled::Opened {
        listed_terminal: None,
        step: dt::Step::Add,
    });

    // No window: the whole banner, then the page, the marker and the wait.
    let (did, opened, log) = Scripted::new(WindowAnswer::Unreachable).run(&absent);
    assert!(opened);
    assert_eq!(did, format!("{opened_words}; you switched it on"));
    let banner = said(&log);
    for words in [
        dt::HELPER_TITLE,
        dt::why(skips),
        dt::HELPER_HOW,
        dt::only_aterm(false),
        dt::trade(skips),
        "Status: aterm Settings \u{25b8} Security",
        DEVTOOLS_WAIT_LINE,
    ] {
        assert!(banner.contains(words), "{words:?} in\n{banner}");
    }
    let at = |what: &str| {
        log.iter()
            .position(|entry| entry.starts_with(what))
            .unwrap_or_else(|| panic!("{what} in {log:?}"))
    };
    assert_eq!(log[0], "ask", "the window is asked first");
    assert!(
        at("say aterm doctor: Add") < at("open"),
        "the banner first: {log:?}"
    );
    assert!(
        at("open") < at("marker") && at("marker") < at("wait"),
        "{log:?}"
    );
    assert!(
        at("wait") < at("refresh"),
        "the switch read on: macOS is asked about aterm again: {log:?}"
    );
    // NEGATIVE CONTROL: a wait that never sees the switch (Enter, or the two minutes)
    // asks macOS nothing, and the did-line says so.
    let mut unswitched = Scripted::new(WindowAnswer::Unreachable);
    unswitched.switched = false;
    let (did, _, log) = unswitched.run(&absent);
    assert!(log.contains(&"wait".to_string()), "{log:?}");
    assert!(!log.contains(&"refresh".to_string()), "{did}: {log:?}");
    assert!(
        did.ends_with("; not switched on while doctor waited"),
        "{did}"
    );

    // The window's panel is up and confirms the switch: doctor opens the page and says
    // one line, the step with its + route and the panel, and waits for nothing.
    let (did, opened, log) = Scripted::new(reply("OK shown")).run(&absent);
    assert!(opened, "{did}");
    assert_eq!(log, ["ask", "open", "marker"], "no banner and no wait");
    for words in [
        "System Settings is open at Developer Tools",
        dt::HELPER_STEP,
        dt::PLUS_ROUTE,
        "The aterm window confirms",
    ] {
        assert!(did.contains(words), "{words:?} in {did}");
    }
    assert!(!did.contains('\n'), "one line: {did}");

    // No panel is up: none could be shown, the window is older than the verb, the request
    // went out and no reply came, or the window's last reading disagrees with doctor's a
    // moment ago. The whole banner, the page and the wait.
    for answer in [
        reply("OK unshown"),
        reply("ERR unknown verb (try: help)"),
        reply("OK granted"),
        reply("OK unknown"),
        WindowAnswer::Unanswered("did not answer within 10s".to_string()),
    ] {
        let (did, opened, log) = Scripted::new(answer.clone()).run(&absent);
        assert!(opened, "{answer:?}: {did}");
        assert!(said(&log).contains(dt::HELPER_HOW), "{answer:?}: {log:?}");
        assert!(log.contains(&"wait".to_string()), "{answer:?}: {log:?}");
    }
    // A decline on record does not hold back a person who asked here.
    let mut declined_here = Scripted::new(WindowAnswer::Unreachable);
    declined_here.declined_at = Some(1_790_812_800);
    let (did, opened, _) = declined_here.run(&absent);
    assert!(opened, "a person asked: {did}");

    // Everything else is the window's decision: nothing opens here.
    let mut declined = Scripted::new(reply("OK declined"));
    declined.declined_at = Some(1_790_812_800);
    for (script, words) in [
        (
            declined,
            "did nothing: you said Not now on 2026-10-01; aterm Settings \u{25b8} Security \
             \u{25b8} Open Developer Tools\u{2026} opens the page",
        ),
        (
            Scripted::new(reply("OK recent")),
            "within the last minute aterm opened the page, or the aterm window",
        ),
        (Scripted::new(reply("OK unstable")), "ad hoc or dev build"),
        (Scripted::new(reply("OK listed")), "nothing to add"),
        (
            Scripted::new(reply("ERR headless: no page")),
            "the aterm instance doctor reached is headless",
        ),
        (
            Scripted::new(reply("ERR main thread stalled 4s; retry")),
            "stalled",
        ),
    ] {
        let (did, opened, log) = script.run(&absent);
        assert!(!opened, "{did}");
        assert!(did.starts_with("did nothing: "), "{did}");
        assert!(did.contains(words), "{words:?} in {did}");
        assert_eq!(log, ["ask"], "{did}");
    }
}

#[test]
fn a_refused_connect_is_no_window_and_a_reply_is_the_windows() {
    use super::{WindowAnswer, window_answer};
    for refused in [
        "ERR auth",
        "ERR auth; the server refused the token in /tmp/aterm/token; re-run",
        "ERR control server busy; retry",
    ] {
        assert_eq!(
            window_answer(Ok(refused.to_string())),
            WindowAnswer::Unreachable,
            "{refused}"
        );
    }
    for answered in [
        "OK queued",
        "OK recent",
        "ERR headless: a headless instance brings no System Settings page up",
        "ERR usage: devtools hand-over [now]",
        "ERR main thread stalled 4s; retry",
    ] {
        assert_eq!(
            window_answer(Ok(answered.to_string())),
            WindowAnswer::Reply(answered.to_string()),
            "{answered}"
        );
    }
    assert_eq!(
        window_answer(Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "took no connection within 10 s; the request was not sent",
        ))),
        WindowAnswer::Unreachable
    );
}

/// A Full Disk Access reading off is no longer only a note pointing at `aterm help
/// permissions`: the row stays a note, and leaves a hand-over for after the report. A grant,
/// a dev build's grant and an unmeasured reading leave none. NEGATIVE CONTROL: the row's
/// mark alone (a note either way) cannot tell them apart.
#[test]
fn a_full_disk_access_reading_off_is_handed_over_after_the_report() {
    use super::{PrivacyNext, privacy_row};
    let (mark, fact, next) = privacy_row(&measured(FdaState::Denied, DrClass::Identity, "aterm"));
    assert_eq!((mark, next), (Mark::Note, PrivacyNext::HandOver), "{fact}");
    for facts in [
        measured(FdaState::Granted, DrClass::Identity, "aterm"),
        measured(FdaState::Granted, DrClass::Cdhash, "aterm (dev)"),
        unmeasured(),
    ] {
        assert_eq!(privacy_row(&facts).2, PrivacyNext::Nothing, "{facts:?}");
    }
    assert_eq!(
        privacy_row(&unmeasured()).0,
        mark,
        "the mark alone is a note for both"
    );
}

/// The gather leaves Full Disk Access due only with `fix`, a reading off and a person here.
#[test]
fn full_disk_access_is_due_only_for_a_person_here_with_fix() {
    let due = |fix: bool, fda: FdaState, here: bool| {
        let privacy = move || measured(fda, DrClass::Identity, "aterm");
        let person = move || here;
        let ports = super::FactReads {
            program_check: &|| None,
            devtools: &|_, _| (DevtoolsFacts::absent(), None),
            spotlight: &|| None,
            services: &|| None,
            privacy: &privacy,
            debugger: &no_debugger,
            instances: &no_census,
            fabric: &|_| None,
            awake: &|| None,
            offered: &|_| None,
            leftovers: &|| Err("not read".to_owned()),
            person_here: &person,
        };
        super::facts_rows_over(fix, &|label| label == "privacy:", &ports)
            .hand_over
            .map(|missing| (missing.fda, missing.devtools.is_some()))
    };
    assert_eq!(due(true, FdaState::Denied, true), Some((true, false)));
    assert_eq!(due(false, FdaState::Denied, true), None, "--read-only");
    assert_eq!(due(true, FdaState::Denied, false), None, "nobody here");
    assert_eq!(due(true, FdaState::Granted, true), None, "granted");
}

/// `aterm doctor` hands what it found missing to the window's permission walk in one
/// request, and then opens and waits for nothing. With no walk to take it (no window, no
/// answer, a window that gave the request up before it took it, a window that reads them
/// held) it opens each page itself and waits for each switch; a window older than the walk
/// gets Developer Tools through its own verb.
#[test]
fn doctor_hands_every_missing_grant_to_the_windows_walk() {
    use super::{Missing, WindowAnswer};
    use aterm_containment::DevToolListing;
    let absent = devtools_facts_with(read_list(DevToolListing::Absent, 2));
    let both = Missing {
        fda: true,
        devtools: Some(absent.clone()),
    };
    let reply = |line: &str| WindowAnswer::Reply(line.to_string());
    let run = |script: &Scripted, missing: &Missing| {
        script.with(|reach| super::permissions_hand_over(missing, reach))
    };

    let mut walking = Scripted::new(WindowAnswer::Unreachable);
    walking.walk = reply("OK walking fda devtools\n");
    let (did, log) = run(&walking, &both);
    assert_eq!(
        log,
        ["walk fda devtools"],
        "one request, nothing opened here"
    );
    assert_eq!(did.len(), 1, "{did:?}");
    assert!(
        did[0].contains("walks you through Full Disk Access, then Developer Tools"),
        "{did:?}"
    );
    let fda_only = Missing {
        fda: true,
        devtools: None,
    };
    let (_, log) = run(&walking, &fda_only);
    assert_eq!(log, ["walk fda"]);

    // A walk already under way over Developer Tools alone: the window reads Full Disk
    // Access held (its reading older than doctor's), so doctor opens that page itself and
    // leaves Developer Tools to the walk. NEGATIVE CONTROL: its answer is `OK walking` too.
    let mut under_way = Scripted::new(WindowAnswer::Unreachable);
    under_way.walk = reply("OK walking devtools\n");
    let (did, log) = run(&under_way, &both);
    assert_eq!(log[0], "walk fda devtools");
    assert!(log.iter().any(|e| e == "open fda"), "{log:?}");
    assert!(!log.iter().any(|e| e == "open" || e == "wait"), "{log:?}");
    assert_eq!(did.len(), 2, "{did:?}");
    assert!(
        did[0].contains("walks you through Developer Tools "),
        "{did:?}"
    );
    assert_eq!(did[1], "opened Full Disk Access; you switched it on");

    for answer in [
        WindowAnswer::Unreachable,
        WindowAnswer::Unanswered("did not answer within 10s".to_string()),
        reply("ERR late: the window did not take the ask within 5s; no walk starts\n"),
        reply("OK granted"),
    ] {
        let mut script = Scripted::new(WindowAnswer::Unreachable);
        script.walk = answer.clone();
        let (did, log) = run(&script, &both);
        assert_eq!(did.len(), 2, "{answer:?}: {did:?}");
        assert_eq!(did[0], "opened Full Disk Access; you switched it on");
        for entry in ["open fda", "wait fda", "open", "wait"] {
            assert!(
                log.iter().any(|e| e == entry),
                "{answer:?}: {entry} in {log:?}"
            );
        }
        assert!(!log.iter().any(|e| e == "ask"), "{log:?}");
    }

    // An older window: Full Disk Access here, Developer Tools through `devtools hand-over
    // now`, whose panel confirms the switch.
    let older = Scripted::new(reply("OK shown"));
    let (did, log) = run(&older, &both);
    assert_eq!(did.len(), 2, "{did:?}");
    assert!(log.iter().any(|e| e == "ask"), "{log:?}");
    assert!(log.iter().any(|e| e == "open fda"), "{log:?}");

    let mut headless = Scripted::new(WindowAnswer::Unreachable);
    headless.walk = reply("ERR headless: a headless instance brings no System Settings page up");
    let (did, log) = run(&headless, &both);
    assert_eq!(log, ["walk fda devtools"]);
    assert!(did[0].contains("headless"), "{did:?}");

    // The owner's Not now, or a walk within the minute: the window's answer stands, and
    // doctor opens nothing past it.
    for (answer, words) in [("OK declined", "Not now"), ("OK recent", "last minute")] {
        let mut held = Scripted::new(WindowAnswer::Unreachable);
        held.walk = reply(answer);
        let (did, log) = run(&held, &both);
        assert_eq!(log, ["walk fda devtools"], "{answer}");
        assert_eq!(did.len(), 1, "{answer}: {did:?}");
        assert!(did[0].contains(words), "{answer}: {did:?}");
    }
}

/// A page the window's answer names `not-now:<page>` the owner's Not now holds back from the
/// walk: doctor opens nothing for it and says so, as for `OK declined`, and leaves the walk
/// the page it names. NEGATIVE CONTROL: a page the answer leaves out with no such word is
/// one the window reads held, and doctor opens it itself.
#[test]
fn doctor_opens_no_page_the_walk_held_back_for_the_owners_not_now() {
    use super::{Missing, WindowAnswer};
    use aterm_containment::DevToolListing;
    let absent = devtools_facts_with(read_list(DevToolListing::Absent, 2));
    let both = Missing {
        fda: true,
        devtools: Some(absent),
    };
    let run = |answer: &str| {
        let mut script = Scripted::new(WindowAnswer::Unreachable);
        script.walk = WindowAnswer::Reply(answer.to_owned());
        script.with(|reach| super::permissions_hand_over(&both, reach))
    };

    let (did, log) = run("OK walking devtools not-now:fda\n");
    assert_eq!(log, ["walk fda devtools"], "nothing opened here");
    assert_eq!(did.len(), 2, "{did:?}");
    assert!(
        did[0].contains("walks you through Developer Tools "),
        "{did:?}"
    );
    assert!(
        did[1].starts_with("did not open Full Disk Access: you said Not now to it"),
        "{did:?}"
    );

    let (did, log) = run("OK walking fda not-now:devtools\n");
    assert_eq!(log, ["walk fda devtools"], "no spctl, page or wait");
    assert!(
        did[0].contains("walks you through Full Disk Access "),
        "{did:?}"
    );
    assert!(
        did[1].starts_with("did not open Developer Tools: you said Not now to it"),
        "{did:?}"
    );

    let (did, log) = run("OK walking accessibility not-now:fda not-now:devtools\n");
    assert_eq!(log, ["walk fda devtools"]);
    assert_eq!(did.len(), 1, "{did:?}");
    assert!(did[0].contains("Not now to them"), "{did:?}");

    let (did, log) = run("OK walking devtools\n");
    assert!(log.iter().any(|e| e == "open fda"), "{log:?}");
    assert_eq!(did[1], "opened Full Disk Access; you switched it on");
}

/// Under another terminal (Terminal.app) the Full Disk Access doctor read missing is that
/// terminal's, and the window's walk turns on its own: doctor opens the page itself and
/// waits for the switch, and only then hands Developer Tools to the walk, never `fda`, so a
/// walk that adds the window's own missing Full Disk Access is not counted as doctor's, and
/// the two pages are never up together. With Full Disk Access alone nothing goes to the
/// window. NEGATIVE CONTROL: in aterm's own tab the one request carries both.
#[test]
fn doctor_opens_the_terminals_full_disk_access_itself_and_walks_only_developer_tools() {
    use super::{Missing, WindowAnswer};
    use aterm_containment::DevToolListing;
    let absent = devtools_facts_with(read_list(DevToolListing::Absent, 2));
    let both = Missing {
        fda: true,
        devtools: Some(absent),
    };
    let mut terminal = Scripted::new(WindowAnswer::Unreachable);
    terminal.responsible = false;
    terminal.walk = WindowAnswer::Reply("OK walking devtools fda\n".to_owned());
    let (did, log) = terminal.with(|reach| super::permissions_hand_over(&both, reach));
    let acts: Vec<&String> = log.iter().filter(|e| !e.starts_with("say ")).collect();
    assert_eq!(acts, ["open fda", "wait fda", "walk devtools"], "{log:?}");
    assert_eq!(did.len(), 2, "{did:?}");
    assert_eq!(did[0], "opened Full Disk Access; you switched it on");
    assert!(
        did[1].contains("walks you through Developer Tools "),
        "{did:?}"
    );

    let fda_only = Missing {
        fda: true,
        devtools: None,
    };
    let (did, log) = terminal.with(|reach| super::permissions_hand_over(&fda_only, reach));
    assert!(!log.iter().any(|e| e.starts_with("walk")), "{log:?}");
    assert_eq!(did, ["opened Full Disk Access; you switched it on"]);

    let mut own = Scripted::new(WindowAnswer::Unreachable);
    own.walk = WindowAnswer::Reply("OK walking fda devtools\n".to_owned());
    let (did, log) = own.with(|reach| super::permissions_hand_over(&both, reach));
    assert_eq!(log, ["walk fda devtools"]);
    assert!(
        did[0].contains("walks you through Full Disk Access, then Developer Tools"),
        "{did:?}"
    );
}

/// Doctor's two waits (Full Disk Access, then the Developer Tools switch) share one stdin
/// reader: each hears an Enter pressed after it began. NEGATIVE CONTROL: the Enter that ended
/// the first wait does not end the second.
#[test]
fn each_wait_hears_only_an_enter_pressed_after_it_began() {
    use std::sync::atomic::Ordering::Relaxed;
    let first = super::enter_pressed();
    assert!(!first.load(Relaxed));
    super::enters().fetch_add(1, Relaxed);
    assert!(first.load(Relaxed), "Enter during the first wait ends it");
    let second = super::enter_pressed();
    assert!(
        !second.load(Relaxed),
        "the first wait's Enter is not the second's"
    );
    super::enters().fetch_add(1, Relaxed);
    assert!(second.load(Relaxed));
}

/// Every `\n` on stdin is an Enter, whatever bytes come before or between: a byte that is
/// not UTF-8 neither counts nor ends the count. NEGATIVE CONTROL: bytes with no `\n` count
/// none.
#[test]
fn a_byte_that_is_not_utf8_does_not_end_the_enter_count() {
    use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
    let lines = AtomicU64::new(0);
    super::count_enters(&b"\xff\n\xc3(\nok\n\n"[..], &lines);
    assert_eq!(lines.load(Relaxed), 4);
    let none = AtomicU64::new(0);
    super::count_enters(&b"\xff\xfe typed, never entered"[..], &none);
    assert_eq!(none.load(Relaxed), 0);
}

/// `aterm init` hands its Permissions phase's grants to the window's walk by their row
/// words, for a person here; with nobody here, nothing asked, or a window that takes none,
/// it says nothing more.
#[test]
fn init_hands_its_grants_to_the_windows_walk() {
    use super::{WindowAnswer, init_walk_over};
    use aterm_types::macos_setup::Access;
    let asked = std::cell::RefCell::new(Vec::new());
    let walking = |words: &str| {
        asked.borrow_mut().push(words.to_owned());
        WindowAnswer::Reply(format!("OK walking {words}"))
    };
    let missing = [Access::FullDiskAccess, Access::Debugger];
    let line = init_walk_over(&missing, true, true, &walking).expect("a walk took them");
    assert_eq!(asked.take(), ["full-disk-access debugger"]);
    assert_eq!(
        init_walk_over(&[Access::FullDiskAccess], true, false, &walking),
        None,
        "nothing walkable asks nothing"
    );
    assert!(
        line.contains("Full Disk Access, then Debugger access")
            || line.contains("Full Disk Access, then Debugger"),
        "{line}"
    );
    assert_eq!(
        init_walk_over(&missing, false, true, &walking),
        None,
        "nobody here"
    );
    assert_eq!(init_walk_over(&[], true, true, &walking), None);
    assert!(asked.take().is_empty(), "neither asks");
    for answer in [
        WindowAnswer::Unreachable,
        WindowAnswer::Reply("OK granted".to_owned()),
        WindowAnswer::Reply("ERR unknown verb (try: help)".to_owned()),
        // A walk already under way over none of these pages.
        WindowAnswer::Reply("OK walking devtools".to_owned()),
        // The owner's Not now holds Full Disk Access back from the walk.
        WindowAnswer::Reply("OK walking devtools not-now:fda".to_owned()),
    ] {
        assert_eq!(
            init_walk_over(&missing, true, true, &|_| answer.clone()),
            None,
            "{answer:?}"
        );
    }
    let under_way = WindowAnswer::Reply("OK walking fda devtools\n".to_owned());
    let line = init_walk_over(&missing, true, true, &|_| under_way.clone()).expect("it takes one");
    assert!(line.contains("through Full Disk Access beside"), "{line}");
}

/// Under another terminal (Terminal.app), Full Disk Access and Accessibility are that
/// terminal's grants, and the window's walk turns on its own: `aterm init` hands the walk
/// only Developer Tools and the debugger, and its line names only those; the Full Disk Access
/// and Accessibility rows keep their own words. NEGATIVE CONTROL: in aterm's own tab (the
/// window is the app macOS holds responsible) it hands all four.
#[test]
fn init_hands_the_walk_only_the_windows_own_grants_under_another_terminal() {
    use super::{WindowAnswer, init_walk_over};
    use aterm_types::macos_setup::Access;
    let asked = std::cell::RefCell::new(Vec::new());
    // The window takes what it is asked and adds its own missing Full Disk Access.
    let walking = |words: &str| {
        asked.borrow_mut().push(words.to_owned());
        WindowAnswer::Reply(format!("OK walking {words} fda accessibility"))
    };
    let missing = [
        Access::FullDiskAccess,
        Access::DeveloperTools,
        Access::Debugger,
        Access::Accessibility,
    ];
    let line = init_walk_over(&missing, true, false, &walking).expect("a walk took some");
    assert_eq!(asked.take(), ["developer-tools debugger"]);
    assert!(line.contains("Developer Tools, then Debugger"), "{line}");
    assert!(
        !line.contains("Full Disk Access") && !line.contains("Accessibility"),
        "the window's own pages are not this run's: {line}"
    );
    assert_eq!(
        init_walk_over(
            &[Access::FullDiskAccess, Access::Accessibility],
            true,
            false,
            &walking
        ),
        None,
        "nothing of the window's to walk"
    );
    assert!(asked.take().is_empty(), "nothing asked");

    let line = init_walk_over(&missing, true, true, &walking).expect("a walk took them");
    assert_eq!(
        asked.take(),
        ["full-disk-access developer-tools debugger accessibility"]
    );
    assert!(line.contains("Full Disk Access, then "), "{line}");
}

#[test]
fn a_decline_is_dated_by_the_local_day() {
    let at = 1_790_812_800;
    assert_eq!(super::local_day_words(at, Some(0)), "2026-10-01");
    assert_eq!(super::local_day_words(at, Some(-7 * 3_600)), "2026-09-30");
    assert_eq!(
        super::local_day_words(at + 3_600, Some(2 * 3_600)),
        "2026-10-01"
    );
    assert_eq!(super::local_day_words(at, None), "2026-10-01 (UTC)");
}

#[test]
fn the_doctor_banner_follows_the_list_it_read() {
    use super::WindowAnswer;
    use aterm_containment::consent::{DevToolProbe, DevToolProbeLabel, DevToolState};
    use aterm_containment::{DevToolList, DevToolListLabel, DevToolListing};
    use atpkg::devtools as dt;
    let said = |log: &[String]| -> String {
        log.iter()
            .filter_map(|entry| entry.strip_prefix("say "))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let shown = || Scripted::new(WindowAnswer::Reply("OK shown".to_string()));

    let empty = devtools_facts_with(read_list(DevToolListing::Absent, 0));
    let (_, opened, log) = Scripted::new(WindowAnswer::Unreachable).run(&empty);
    assert!(opened);
    let spctl = log.iter().position(|e| e == "spctl").expect("spctl ran");
    let banner = log
        .iter()
        .position(|e| e.starts_with("say aterm doctor: Add"))
        .expect("the banner");
    assert!(spctl < banner, "{log:?}");
    assert!(said(&log).contains(dt::only_aterm(true)), "{log:?}");
    // Beside the window's panel the one line says it. NEGATIVE CONTROL: with no `spctl`,
    // the line speaks of no Terminal.
    let (did, opened, log) = shown().run(&empty);
    assert!(opened && log.contains(&"spctl".to_string()), "{log:?}");
    assert!(did.contains(dt::only_aterm(true)), "{did}");
    let absent = devtools_facts_with(read_list(DevToolListing::Absent, 2));
    let (did, _, _) = shown().run(&absent);
    assert!(!did.contains("Terminal"), "{did}");

    let off = devtools_facts_with(read_list(DevToolListing::Off, 2));
    let (_, _, log) = Scripted::new(WindowAnswer::Unreachable).run(&off);
    let banner = said(&log);
    assert!(banner.contains(dt::HELPER_SWITCH_ON), "{banner}");
    assert!(!banner.contains("Drag"), "aterm is listed: {banner}");
    assert!(!log.contains(&"spctl".to_string()), "{log:?}");
    let (did, _, _) = shown().run(&off);
    assert!(did.contains(dt::STEP_SWITCH_ON), "{did}");
    assert!(!did.contains("Drag"), "aterm is listed: {did}");

    let mut unread = devtools_facts_with(DevToolList::refused(DevToolListLabel::NoFullDiskAccess));
    unread.log = DevToolProbe {
        state: DevToolState::Unlisted,
        label: DevToolProbeLabel::Observed,
        requests: Some(3),
        not_allowed: Some(3),
        answered_at_ms: Some(1),
    };
    assert_eq!(unread.grant(), aterm_containment::DevToolGrant::NotGranted);
    let (_, opened, log) = Scripted::new(WindowAnswer::Unreachable).run(&unread);
    assert!(opened);
    assert!(said(&log).contains(dt::HELPER_HOW), "{log:?}");
    assert!(
        !log.contains(&"wait".to_string()),
        "an unread list is not waited on: {log:?}"
    );
}

#[test]
fn the_debugger_row_never_fails_and_prints_the_undo_only_when_on() {
    use DebuggerMember::{Admin, Developer, NotMember};
    use DebuggerMode::{Disabled, Enabled, Unrecognized};
    use SelfHardening::{Hardened, NotHardened};
    let unrecognised_words = DebuggerFacts {
        probe: DebuggerProbe {
            mode: DebuggerMode::Unknown,
            member: Admin,
            label: DebuggerProbeLabel::StatusUnrecognized,
        },
        hardening: Hardened,
    };
    for facts in [
        debugger(Enabled, Admin, Hardened),
        debugger(Enabled, Developer, NotHardened),
        debugger(Enabled, Admin, SelfHardening::Unknown),
        debugger(Disabled, Admin, Hardened),
        debugger(Disabled, Developer, NotHardened),
        debugger(Disabled, NotMember, NotHardened),
        debugger(Disabled, DebuggerMember::Unknown, Hardened),
        debugger(Unrecognized, Admin, Hardened),
        no_debugger(),
        unrecognised_words,
    ] {
        let (r, code) = doctor_checks_with_origin(
            "/bin/sh",
            ShellCheck::Runs,
            None,
            &unmeasured(),
            &facts,
            &no_devtools(),
            &no_census(),
            &no_recovery(),
            &no_exceptions(),
            &MachineFacts::absent(),
        );
        assert_eq!(code, 0, "the debugger row never moves the exit code\n{r}");
        assert!(r.contains("health: OK"), "{r}");
        let line = r
            .lines()
            .find(|l| l.starts_with("debugger:"))
            .unwrap_or_else(|| panic!("no debugger: row in\n{r}"));
        assert_eq!(
            line.contains(DEBUGGER_ACCESS_REVERT),
            facts.probe.mode == Enabled,
            "{line}"
        );
        assert!(
            !line.contains("-enable") && !line.contains("sudo"),
            "{line}"
        );
    }
}

#[test]
fn the_three_marks_render_and_only_one_fails() {
    assert_eq!(Mark::Ok.render(), "ok");
    assert_eq!(Mark::Note.render(), "note");
    assert_eq!(Mark::Fail.render(), "FAIL");
    assert!(!Mark::Ok.is_fail() && !Mark::Note.is_fail() && Mark::Fail.is_fail());
    assert_eq!(Mark::from_ok(true), Mark::Ok);
    assert_eq!(Mark::from_ok(false), Mark::Fail);
}

#[test]
fn show_face_success_path_emits_metrics() {
    let family = aterm_render::list_fonts()
        .into_iter()
        .find(|f| aterm_render::face_info(f).is_some())
        .unwrap_or_else(|| aterm_spec::host::could_not_run("no resolvable system font"));
    let (r, code) = show_face_report(Some(&family));
    assert_eq!(code, 0, "{r}");
    for key in [
        "family=",
        "path=",
        "index=",
        "px=16\n",
        "cell_width=",
        "cell_height=",
        "baseline=",
        "glyph_count=",
    ] {
        assert!(r.contains(key), "show-face missing {key:?}\n{r}");
    }
}

#[test]
fn list_fonts_report_shape() {
    let r = list_fonts_report();
    if r == "(no fonts found)\n" {
        aterm_spec::host::could_not_run("no fonts found on this host to list");
    }
    assert!(r.ends_with('\n'), "must be newline-terminated");
    let lines: Vec<&str> = r.lines().collect();
    assert!(lines.iter().all(|l| !l.is_empty()), "no empty lines");
    assert_eq!(
        lines,
        aterm_render::list_fonts(),
        "must mirror list_fonts()"
    );
}

#[test]
fn tool_dispatch_never_shadows_aterm_surface() {
    assert!(!is_tool_candidate(None));
    assert!(!is_tool_candidate(Some("")));
    for flag in [
        "-h",
        "--help",
        "-V",
        "--version",
        "--sandbox",
        "--containment",
    ] {
        assert!(
            !is_tool_candidate(Some(flag)),
            "{flag} is a flag, not a tool"
        );
    }
    for verb in Verb::ALL {
        assert!(
            !is_tool_candidate(Some(verb.name())),
            "{} is a front-door verb",
            verb.name()
        );
    }
    assert!(!is_tool_candidate(Some("help")));
    for (name, _) in DIAG_COMMANDS {
        assert!(
            !is_tool_candidate(Some(name)),
            "{name} is a diag subcommand"
        );
    }
    for tool in ["ay", "ty", "trust", "trust-mc", "clean", "ny"] {
        assert!(
            is_tool_candidate(Some(tool)),
            "{tool} should be a candidate"
        );
    }
}

#[test]
fn launch_flags_decide_the_run() {
    let run = |containment: Option<&str>, quiet: bool, verbose: bool| CliAction::Run {
        containment: containment.map(str::to_string),
        quiet,
        verbose,
    };
    let cases: &[(&[&str], CliAction)] = &[
        (&[], run(None, false, false)),
        (&["--containment", "user"], run(Some("user"), false, false)),
        (&["--containment=user"], run(Some("user"), false, false)),
        (&["--containment="], run(Some(""), false, false)),
        (&["--containment", "xyz"], run(Some("xyz"), false, false)),
        (&["--sandbox"], run(Some("containment"), false, false)),
        (&["--no-sandbox"], run(Some("user"), false, false)),
        (
            &["--sandbox", "--containment", "user"],
            run(Some("user"), false, false),
        ),
        (
            &["--containment", "user", "--sandbox"],
            run(Some("containment"), false, false),
        ),
        (
            &["--no-sandbox", "--sandbox"],
            run(Some("containment"), false, false),
        ),
        (&["-q"], run(None, true, false)),
        (&["--quiet"], run(None, true, false)),
        (&["--sandbox", "-q"], run(Some("containment"), true, false)),
        (
            &["--quiet", "--containment", "user"],
            run(Some("user"), true, false),
        ),
        (
            &["--verbose", "--sandbox"],
            run(Some("containment"), false, true),
        ),
        (&["--"], run(None, false, false)),
        (&["--sandbox", "--"], run(Some("containment"), false, false)),
    ];
    for (argv, want) in cases {
        assert_eq!(&decide(argv), want, "{argv:?}");
    }
}

#[test]
fn malformed_launch_argv_is_a_usage_error() {
    for argv in [
        &["--containment"][..],
        &["--bogus"],
        &["--", "extra"],
        &["--", "--help"],
        &["--no-sandbox", "show-config"],
    ] {
        assert!(
            matches!(decide(argv), CliAction::Usage(_)),
            "{argv:?}: {:?}",
            decide(argv)
        );
    }
}

#[test]
fn help_and_version_short_and_long() {
    for a in [["-h"], ["--help"]] {
        assert_eq!(decide(&a), CliAction::Help);
    }
    for a in [["-V"], ["--version"]] {
        assert_eq!(decide(&a), CliAction::Version);
    }
}

#[test]
fn version_text_names_the_running_copy_and_any_other() {
    use aterm_stamp::Stamp;
    use aterm_update::which_copy::{OtherCopy, Running, WhichCopy};
    let identity = format!(
        "aterm {}\n{}\n",
        Stamp::UNSTAMPED.app_version,
        aterm_types::identity::ORIGIN_LINE
    );
    assert!(identity.contains("by Andrew Yates") && identity.contains("alab.systems"));
    let anchors = super::trust_anchors_line();
    assert!(
        anchors.starts_with("trusts: master=") && !anchors.contains("channel="),
        "{anchors}"
    );
    assert_eq!(
        super::version_text_with_stamp(None, None),
        format!("{identity}{anchors}")
    );
    assert_eq!(
        super::version_text_with_stamp(None, Some(&Stamp::UNSTAMPED)),
        format!("{identity}{anchors}")
    );
    let stamp = Stamp {
        build_number: "1789432052",
        ..Stamp::UNSTAMPED
    };
    let build = "build: 1789432052\n";
    assert_eq!(
        super::version_text_with_stamp(None, Some(&stamp)),
        format!("{identity}{build}{anchors}")
    );
    let copy = WhichCopy {
        running: std::path::PathBuf::from("/Applications/aterm.app"),
        kind: Running::InstalledApp,
        others: vec![OtherCopy {
            path: std::path::PathBuf::from("/Users//ana/Applications/aterm.app"),
            version: Some("0.60.0".to_string()),
        }],
    };
    assert_eq!(
        super::version_text_with_stamp(Some(&copy), Some(&stamp)),
        format!(
            "{identity}{build}running: /Applications/aterm.app\nanother copy: \
             /Users//ana/Applications/aterm.app (0.60.0) \u{2014} the updater leaves it \
             alone\n{anchors}"
        )
    );
    assert!(version_text(Some(&copy)).starts_with("aterm "));
}

#[test]
fn the_session_inherits_rlimits_in_user_and_caps_in_safety_and_containment() {
    use aterm_containment::ContainmentMode as Cm;
    use aterm_sandbox::Limits;
    assert_eq!(
        session_limits(Cm::User),
        Limits::inherit(),
        "user: must inherit"
    );
    assert_eq!(
        session_limits(Cm::Safety),
        Limits::shell_default(),
        "safety: soft caps"
    );
    assert_eq!(
        session_limits(Cm::Containment),
        Limits::contained(),
        "containment: binding caps"
    );
    assert_eq!(
        Limits {
            binding: false,
            ..Limits::contained()
        },
        Limits::shell_default(),
        "containment binds the same numbers safety sets"
    );
}

/// This process's soft and hard `RLIMIT_NOFILE`, read back from the kernel.
#[cfg(unix)]
fn current_nofile() -> (libc::rlim_t, libc::rlim_t) {
    let mut lim = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: a valid resource id and a valid out-param for the call.
    assert_eq!(unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut lim) }, 0);
    (lim.rlim_cur, lim.rlim_max)
}

/// Read a pty master until the child hangs up (EOF, or EIO on Linux once the
/// slave is closed), bounded by a deadline so a wedged probe fails the test
/// instead of hanging the suite.
#[cfg(unix)]
fn read_until_hangup(master: i32) -> String {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    let mut out = Vec::new();
    let mut buf = [0u8; 512];
    loop {
        let mut p = libc::pollfd {
            fd: master,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one valid pollfd, 50 ms timeout.
        if unsafe { libc::poll(&mut p, 1, 50) } > 0 {
            // SAFETY: `read` fills at most `buf.len()` bytes of this owned buffer.
            let n = unsafe { libc::read(master, buf.as_mut_ptr().cast(), buf.len()) };
            match usize::try_from(n) {
                Ok(0) => break,
                Ok(n) => out.extend_from_slice(&buf[..n]),
                Err(_) => {
                    let errno = std::io::Error::last_os_error().raw_os_error();
                    if errno != Some(libc::EAGAIN) && errno != Some(libc::EINTR) {
                        break;
                    }
                }
            }
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the probe shell did not hang up within 60 s; got {:?}",
            String::from_utf8_lossy(&out)
        );
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// End-to-end check that `ulimit -n` in a spawned `/bin/sh` matches the
/// parent's soft limit in `User` mode, can be raised above the soft cap in
/// `Safety` mode, and is hard-capped in `Containment` mode.
#[cfg(unix)]
#[test]
fn the_session_child_sees_the_posture_of_its_mode() {
    use aterm_containment::ContainmentMode as Cm;

    // SAFETY: a trusted test entry point, before any untrusted input flows.
    let authority = unsafe { aterm_cap::Authority::root_authority() };
    let spawn_cap = authority.grant::<aterm_cap::effects::Spawn>(aterm_cap::Tier::Trusted);
    let sandbox_cap = authority.grant::<aterm_sandbox::Sandbox>(aterm_cap::Tier::Trusted);

    let child_lines = |mode: Cm, script: &str| -> Vec<String> {
        let exec: Vec<std::ffi::OsString> = vec!["/bin/sh".into(), "-c".into(), script.into()];
        let shell = aterm_pty::spawn_shell_with_pid(
            24,
            80,
            &spawn_cap,
            &sandbox_cap,
            &[],
            None,
            None,
            None,
            Some(&exec),
            None,
            None,
            session_limits(mode),
        )
        .expect("the probe shell must spawn");
        let out = read_until_hangup(shell.master);
        // SAFETY: reaping our own child; the master is ours to close.
        unsafe {
            let mut status = 0;
            libc::waitpid(shell.pid, &mut status, 0);
            libc::close(shell.master);
        }
        out.lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect()
    };
    let child_ulimit = |mode: Cm| -> String {
        child_lines(mode, "ulimit -n")
            .into_iter()
            .next()
            .unwrap_or_default()
    };
    let render = |v: libc::rlim_t| {
        if v == libc::RLIM_INFINITY {
            "unlimited".to_string()
        } else {
            v.to_string()
        }
    };

    let (soft, hard) = current_nofile();
    assert_eq!(
        child_ulimit(Cm::User),
        render(soft),
        "user mode: the child keeps the parent's soft limit"
    );
    let cap = aterm_sandbox::Limits::shell_default()
        .open_files
        .expect("the hardened set caps open files");
    assert_eq!(
        child_ulimit(Cm::Safety),
        render(core::cmp::min(cap as libc::rlim_t, hard)),
        "safety mode: the child gets the cap, clamped to the inherited hard ceiling"
    );

    let raise = cap + 1;
    if hard <= raise as libc::rlim_t {
        aterm_spec::host::could_not_run(format!(
            "the inherited hard NOFILE ({hard}) does not exceed {raise}"
        ));
    }
    let script = format!("ulimit -n {raise} 2>/dev/null; ulimit -n; ulimit -Hn");
    assert_eq!(
        child_lines(Cm::Safety, &script),
        [raise.to_string(), raise.to_string()],
        "safety mode: the cap is a soft guardrail the shell's rc can raise"
    );
    assert_eq!(
        child_lines(Cm::Containment, &script),
        [cap.to_string(), cap.to_string()],
        "containment mode: the contained program raised its cap back"
    );
}
