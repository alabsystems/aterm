// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

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
        text.lines()
            .any(|l| l == "exceptions: note   2 open: disk low; update held (aterm exceptions)"),
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

/// A row too long for one line keeps its first clause and its one fix, in at most `room`
/// characters, none at all when there is no room. NEGATIVE CONTROL: a line that fits is left
/// whole, and one with no fix is cut.
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
    assert_eq!(
        shorten("anything", 0),
        "",
        "no room holds nothing, not even the `\u{2026}`"
    );
    assert_eq!(shorten("anything", 1), "\u{2026}");
    let tight = shorten(&long, 50);
    assert!(
        tight.ends_with(" \u{2014} fix: open a new aterm tab"),
        "{tight}"
    );
    assert_eq!(tight.chars().count(), 50, "{tight}");
    assert_eq!(
        shorten(
            "no accessibility for Terminal, because of a long reason \u{2014} fix: switch on aterm!",
            46
        ),
        "no accessibility for \u{2026} \u{2014} fix: switch on aterm!",
        "a fix that would fill the row leaves the clause its 22 characters"
    );
    assert_eq!(
        shorten(
            "no accessibility for Terminal \u{2014} fix: switch on aterm row",
            28
        ),
        "fix: switch on aterm row",
        "a row with no room for the clause gives the fix the whole room, not its 20"
    );
    assert_eq!(
        shorten(
            "no accessibility for Terminal \u{2014} fix: switch on aterm row",
            23
        ),
        "fix: switch on aterm r\u{2026}",
        "a fix longer than the whole room is cut to it"
    );
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
