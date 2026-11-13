// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The host's side of the Fable limit's move onto Opus: the line, the cause and the refusals.

use super::*;
use crate::supervise::policy::turn_end::{AccountAt, PermissionTo, RouteTo};

const SESSION: &str = "0b5e7a11-f100-4a2b-9c3d-5e6f70819a2b";
const START: &str = "Thu Sep 24 01:02:03 2026";

fn words(list: &[&str]) -> Vec<String> {
    list.iter().map(|w| (*w).to_string()).collect()
}

fn to_opus() -> Restart {
    Restart::Model {
        to: FABLE_FALLBACK.to_string(),
    }
}

fn back_to(model: Option<&str>) -> Restart {
    Restart::ModelBack {
        to: model.map(str::to_string),
    }
}

/// A Claude Code process launched as `argv`, in tab `s-1`, with no host reads behind it.
fn claude(argv: &[&str]) -> Snapshot {
    Snapshot {
        lifetime: None,
        launch_env: None,
        auth_observed: true,
        route: crate::harness::accounts::AuthRoute::Outside,
        profile: None,
        tab: "s-1".to_string(),
        pid: 4242,
        start: START.to_string(),
        shell: 4241,
        program: PathBuf::from("/opt/claude"),
        argv: words(argv),
        session: Some(SESSION.to_string()),
        cwd: "/w".to_string(),
        version: Some("2.1.310".to_string()),
        run: AgentRun::Claude,
        dialect: Some(Dialect::Zsh),
        login: None,
        home: None,
        claude_dir: None,
    }
}

/// The same process read as a Muse Code session: another program than Claude Code.
fn muse(argv: &[&str]) -> Snapshot {
    Snapshot {
        run: AgentRun::Muse(MuseRun {
            data: PathBuf::from("/fixture/muse"),
            env: Vec::new(),
            workspace: None,
            lock: None,
            started_ms: 0,
            block: None,
        }),
        ..claude(argv)
    }
}

/// THE MODEL LINE PUTS ONE `--model` RIGHT AFTER `argv[0]` AND TAKES EVERY OTHER OUT: a launch with
/// none gains `--model <to>`; `--model X` and `--model=X` are replaced, every repeat with them;
/// `None` leaves none; every other word keeps its order, `--resume` and a `--` tail with it. A
/// `--model` written as another option's value, or past `--`, is not the launch's and stays.
#[test]
fn a_model_line_names_one_model_after_argv0_and_keeps_every_other_word() {
    let opus = Some(FABLE_FALLBACK);
    let cases: [(&[&str], Option<&str>, &[&str]); 11] = [
        (&["claude"], opus, &["claude", "--model", FABLE_FALLBACK]),
        (
            &["claude", "--verbose", "--model", "fable", "--add-dir", "/x"],
            opus,
            &[
                "claude",
                "--model",
                FABLE_FALLBACK,
                "--verbose",
                "--add-dir",
                "/x",
            ],
        ),
        (
            &["claude", "--model=claude-fable-5-2", "--verbose"],
            opus,
            &["claude", "--model", FABLE_FALLBACK, "--verbose"],
        ),
        (
            &["claude", "--model", "a", "--verbose", "--model=b"],
            opus,
            &["claude", "--model", FABLE_FALLBACK, "--verbose"],
        ),
        (
            &["claude", "--resume", SESSION, "--model", "fable"],
            opus,
            &["claude", "--model", FABLE_FALLBACK, "--resume", SESSION],
        ),
        (
            &["claude", "--model", "fable", "--", "--model", "x"],
            opus,
            &["claude", "--model", FABLE_FALLBACK, "--", "--model", "x"],
        ),
        (
            &["claude", "--append-system-prompt", "--model=sonnet"],
            opus,
            &[
                "claude",
                "--model",
                FABLE_FALLBACK,
                "--append-system-prompt",
                "--model=sonnet",
            ],
        ),
        (
            &["claude", "--model", FABLE_FALLBACK, "--verbose"],
            None,
            &["claude", "--verbose"],
        ),
        (
            &["claude", "--model", "a", "--model=b", "--", "--model", "c"],
            None,
            &["claude", "--", "--model", "c"],
        ),
        (
            &["claude", "--model=claude-opus-5-5", "--verbose"],
            Some("claude-fable-5-2"),
            &["claude", "--model", "claude-fable-5-2", "--verbose"],
        ),
        (&["claude", "--verbose"], None, &["claude", "--verbose"]),
    ];
    for (argv, model, want) in cases {
        assert_eq!(
            model_argv(&words(argv), model),
            words(want),
            "{argv:?} -> {model:?}"
        );
    }
    assert!(
        model_argv(&[], opus).is_empty(),
        "no argv[0], no line to name a model on"
    );
}

/// A MOVE ONTO OPUS IS MADE ONLY OF A CLAUDE CODE LAUNCHED ON FABLE OR ON NO NAMED MODEL: its line
/// is the launch's with `--model claude-opus-5-5` in the place of any `--model` it had. A launch
/// naming another model (Sonnet, Opus, the fallback itself) is refused as the launch's model; a
/// move onto any model but the fallback, or of another program, is refused as a model move.
#[test]
fn a_move_onto_opus_is_made_only_of_a_claude_launched_on_fable_or_no_model() {
    let moved = |argv: &[&str]| model_move_argv(&to_opus(), &claude(argv));
    assert_eq!(
        moved(&["/opt/claude"]),
        Ok(Some(words(&["/opt/claude", "--model", FABLE_FALLBACK]))),
        "no --model: Claude Code's own settings chose Fable"
    );
    for fable in [
        &["/opt/claude", "--model", "fable"][..],
        &["/opt/claude", "--model", "Fable"],
        &["/opt/claude", "--model", "claude-fable-5-2"],
        &["/opt/claude", "--model=claude-fable-5-2"],
    ] {
        assert_eq!(
            moved(&[fable, &["--verbose"]].concat()),
            Ok(Some(words(&[
                "/opt/claude",
                "--model",
                FABLE_FALLBACK,
                "--verbose"
            ]))),
            "{fable:?}"
        );
    }
    for other in [
        &["/opt/claude", "--model", "claude-sonnet-4-6"][..],
        &["/opt/claude", "--model=sonnet"],
        &["/opt/claude", "--model", "opus"],
        &["/opt/claude", "--model", FABLE_FALLBACK],
        &[
            "/opt/claude",
            "--model",
            "fable",
            "--model",
            "claude-sonnet-4-6",
        ],
    ] {
        assert_eq!(moved(other), Err(REFUSED_LAUNCH_MODEL), "{other:?}");
    }
    for to in ["claude-sonnet-4-6", "opus", "fable", ""] {
        assert_eq!(
            model_move_argv(
                &Restart::Model { to: to.to_string() },
                &claude(&["/opt/claude"])
            ),
            Err(REFUSED_MODEL_MOVE),
            "onto {to:?}"
        );
    }
    assert_eq!(
        model_move_argv(&to_opus(), &muse(&["/opt/muse"])),
        Err(REFUSED_MODEL_MOVE),
        "another program than Claude Code"
    );
}

/// THE RETURN AT THE RESET IS MADE ONLY OF A LINE RUNNING THE FALLBACK: it puts the launch's own
/// `--model` back (or none, where the launch named none) and keeps the rest of the line. A line
/// with no `--model`, another model, or the bare `opus` alias is not the move's own and is refused,
/// as is another program.
#[test]
fn the_return_is_made_only_of_a_line_on_the_fallback_and_puts_the_launchs_model_back() {
    let on_opus = ["/opt/claude", "--model", FABLE_FALLBACK, "--verbose"];
    assert_eq!(
        model_move_argv(&back_to(None), &claude(&on_opus)),
        Ok(Some(words(&["/opt/claude", "--verbose"]))),
        "a launch that named no model gets none back"
    );
    assert_eq!(
        model_move_argv(&back_to(Some("claude-fable-5-2")), &claude(&on_opus)),
        Ok(Some(words(&[
            "/opt/claude",
            "--model",
            "claude-fable-5-2",
            "--verbose"
        ]))),
        "the launch's own model, as it typed it"
    );
    assert_eq!(
        model_move_argv(
            &back_to(Some("fable")),
            &claude(&["/opt/claude", "--model=claude-opus-5-5"])
        ),
        Ok(Some(words(&["/opt/claude", "--model", "fable"]))),
        "the joined spelling of the fallback"
    );
    for not_ours in [
        &["/opt/claude"][..],
        &["/opt/claude", "--model", "claude-sonnet-4-6"],
        &["/opt/claude", "--model", "opus"],
        &["/opt/claude", "--model", "fable"],
        &["/opt/claude", "--", "--model", FABLE_FALLBACK],
    ] {
        assert_eq!(
            model_move_argv(&back_to(None), &claude(not_ours)),
            Err(REFUSED_NOT_ON_FALLBACK),
            "{not_ours:?}"
        );
    }
    assert_eq!(
        model_move_argv(&back_to(None), &muse(&on_opus)),
        Err(REFUSED_NOT_ON_FALLBACK),
        "another program"
    );
}

/// EVERY OTHER RESTART ADDS NO MODEL: its line is the process's own argv (`Ok(None)`), whatever
/// `--model` it carries, so only a model move ever changes the line's model.
#[test]
fn every_other_restart_relaunches_on_the_processs_own_line() {
    let others = [
        Restart::Memory,
        Restart::InputLost {
            of: (None, None),
            line: crate::supervise::custody::LineAsk { at_ms: 0, nth: 0 },
        },
        Restart::Account {
            to: "work-2".to_string(),
            at: AccountAt::Wall,
        },
        Restart::Route { to: RouteTo::Token },
        Restart::Permission {
            to: PermissionTo::Bypass,
            conversation: SESSION.to_string(),
        },
    ];
    for argv in [
        &["/opt/claude"][..],
        &["/opt/claude", "--model", "fable"],
        &["/opt/claude", "--model", FABLE_FALLBACK],
        &["/opt/claude", "--model", "claude-sonnet-4-6"],
    ] {
        for why in &others {
            assert_eq!(
                model_move_argv(why, &claude(argv)),
                Ok(None),
                "{why:?} {argv:?}"
            );
        }
    }
    for why in &others {
        assert_eq!(
            model_move_argv(why, &muse(&["/opt/muse"])),
            Ok(None),
            "{why:?}"
        );
    }
}

/// THE TWO MODEL CAUSES: a move is `model:<to>`, a return `model-back:<model>` (empty where the
/// launch named none); each is a relaunch that ended its own agent, and each record is the asked
/// restart's own only for its own kind (and, for a move, its own model), never another restart's.
#[test]
fn a_model_move_and_its_return_have_causes_of_their_own() {
    let moved = restart_cause(&to_opus());
    assert_eq!(moved, format!("model:{FABLE_FALLBACK}"));
    assert_eq!(
        restart_cause(&back_to(Some("claude-fable-5-2"))),
        "model-back:claude-fable-5-2"
    );
    let back = restart_cause(&back_to(None));
    assert_eq!(back, "model-back:");
    for cause in [&moved, &back] {
        assert!(relaunch_cause(cause), "{cause}");
        assert!(ends_its_agent(cause), "{cause}");
    }
    assert!(
        !moved.starts_with(CAUSE_MODEL_BACK) && !back.starts_with(CAUSE_MODEL),
        "neither prefix reads the other's cause"
    );

    let ask = RestartAsk::of(&to_opus(), 600);
    let record = |cause: &str| St {
        cause: cause.to_string(),
        ask: ask.word().to_string(),
        tab: "s-1".to_string(),
        ..St::default()
    };
    assert!(its_own(&to_opus(), &ask, &record(&moved)));
    assert!(
        !its_own(
            &Restart::Model {
                to: "claude-sonnet-4-6".to_string()
            },
            &ask,
            &record(&moved)
        ),
        "a move onto another model is not this record's"
    );
    assert!(!its_own(&to_opus(), &ask, &record(&back)));
    assert!(!its_own(&to_opus(), &ask, &record(CAUSE_MEMORY)));
    let ask_back = RestartAsk::of(&back_to(None), 600);
    let record_back = |cause: &str| St {
        ask: ask_back.word().to_string(),
        ..record(cause)
    };
    assert!(its_own(&back_to(None), &ask_back, &record_back(&back)));
    assert!(its_own(
        &back_to(None),
        &ask_back,
        &record_back("model-back:claude-fable-5-2")
    ));
    assert!(!its_own(&back_to(None), &ask_back, &record_back(&moved)));
    assert!(!its_own(&Restart::Memory, &ask, &record(&moved)));
}

/// THE RESUMED PROMPT SAYS WHICH MODEL THE SESSION NOW RUNS AND THAT THE MOVE IS FOR THIS SESSION
/// ONLY: a move names Opus's id and Fable's limit; a return names the launch's model, or says it is
/// the one the session was launched with where the launch named none. Each differs from an
/// unasked exit's words.
#[test]
fn a_model_moves_resumed_prompt_names_the_model_and_its_reach() {
    let moved = resumed_prompt_as("Claude Code", "2.1.310", &restart_cause(&to_opus()));
    assert!(
        moved.contains(&format!("Relaunched on {FABLE_FALLBACK}")),
        "{moved}"
    );
    assert!(
        moved.contains("Fable reached its own usage limit"),
        "{moved}"
    );
    assert!(moved.contains("same account"), "{moved}");
    assert!(moved.contains("for this session only"), "{moved}");
    let named = resumed_prompt_as(
        "Claude Code",
        "2.1.310",
        &restart_cause(&back_to(Some("claude-fable-5-2"))),
    );
    assert!(named.contains("Relaunched on claude-fable-5-2"), "{named}");
    assert!(named.contains("has reset"), "{named}");
    assert!(named.contains("for this session only"), "{named}");
    let unnamed = resumed_prompt_as("Claude Code", "2.1.310", &restart_cause(&back_to(None)));
    assert!(
        unnamed.contains("Relaunched on the model it was launched with"),
        "{unnamed}"
    );
    assert!(unnamed.contains("for this session only"), "{unnamed}");
    let exited = resumed_prompt_as("Claude Code", "2.1.310", "");
    for said in [&moved, &named, &unnamed] {
        assert_ne!(said, &exited);
        assert!(said.contains("2.1.310"), "{said}");
    }
}

/// A MODEL MOVE'S RELAUNCH RETIRED UNISSUED HOLDS NO CLAIM, as every restart's: its own record in
/// flight is claimed (taken, then held while carried) until its native relaunch was retired
/// unissued, when nothing of it is under way.
#[test]
fn a_model_move_whose_own_relaunch_was_retired_unissued_holds_no_claim() {
    let tmp = aterm_tempfile::TempDir::new().expect("tmp");
    let opts = Opts {
        dry_run: false,
        ..opts_in(tmp.path())
    };
    let why = to_opus();
    let ask = RestartAsk::of(&why, 600);
    let report = Report {
        rung: None,
        pid: 4242,
        tab: "s-1".to_string(),
        session: SESSION.to_string(),
        from: "2.1.310".to_string(),
        to: "2.1.310(same)".to_string(),
        step: String::new(),
    };
    let record = |held: bool| St {
        cause: restart_cause(&why),
        ask: ask.word().to_string(),
        tab: "s-1".to_string(),
        phase: upgrade::Phase::Exiting { at_s: 1 },
        native_retired_unissued: held,
        native_coordination: held,
        native_prepare: if held {
            "exact predecessor recipe".to_string()
        } else {
            String::new()
        },
        ..St::default()
    };
    let claims = |held: bool| {
        let st = record(held);
        assert!(its_own(&why, &ask, &st) && st.in_flight());
        save(&opts, SESSION, &st);
        (
            claim_of(&opts, &why, &ask, &report, Met::Claiming(None)),
            claim_of(&opts, &why, &ask, &report, Met::Carried),
        )
    };
    assert_eq!(
        claims(false),
        (RestartClaim::Taken, RestartClaim::Held),
        "the control: its own relaunch in flight is claimed"
    );
    assert_eq!(
        claims(true),
        (RestartClaim::Unclaimed, RestartClaim::Unclaimed),
        "retired unissued, nothing of it is under way"
    );
}

/// A dry run's options over the scratch folder `root`, for tab `s-1`.
fn opts_in(root: &Path) -> Opts {
    Opts {
        home: root.join("home"),
        state: root.join("state"),
        account_roster: None,
        sock: None,
        token: None,
        only_sid: Some("s-1".to_string()),
        dry_run: true,
        human_grace_s: 0,
        hand_back: false,
        background: false,
        aterm_state: None,
        ask: false,
        usage_tap: None,
        instances: super::super::upgrade_drive::Instances::Listed(Vec::new()),
        agents_off: Vec::new(),
        land: false,
        tell: crate::harness::upgrade_drive::Tell::Off,
        press: None,
        timings: crate::harness::Timings::FAST,
    }
}

/// A kernel that reads nothing of the machine: no process, no terminal, no background work, and a
/// shell read as zsh.
#[cfg(unix)]
struct Unread;

#[cfg(unix)]
impl Kernel for Unread {
    fn parent(&self, _: u32) -> Option<u32> {
        None
    }
    fn terminal(&self, _: u32) -> Option<(u32, String)> {
        None
    }
    fn process_start(&self, _: u32) -> Option<String> {
        None
    }
    fn restart_background(&self, _: u32) -> Vec<String> {
        Vec::new()
    }
    fn restart_held(&self, _: u32) -> Option<Vec<crate::harness::upgrade::Held>> {
        Some(Vec::new())
    }
    fn shell_dialect(&self, _: u32, _: &[super::super::upgrade_drive::ProcRow]) -> Option<Dialect> {
        Some(Dialect::Zsh)
    }
}

/// A DRY RUN OF A MODEL MOVE SAYS WHICH MODEL IT WOULD RELAUNCH ON, OR WHY IT REFUSES: the move of
/// a Fable launch would restart on `claude-opus-5-5`, the return of a line on it would restart on
/// the launch's model (`-` where it named none), and a launch naming Sonnet, a move onto another
/// model and a return of a line not on the fallback are refused with their own words, before any
/// line is planned. CONTROL: a memory restart of the same process would restart with no model word.
#[cfg(unix)]
#[test]
fn a_dry_run_of_a_model_move_names_the_model_or_the_refusal() {
    // A pid and shell no process holds, so no read finds this test among them.
    const PID: u32 = 4_000_000_001;
    const SHELL: u32 = 4_000_000_002;
    let tmp = aterm_tempfile::TempDir::new().expect("tmp");
    let opts = opts_in(tmp.path());
    let sessions = opts.home.join(".claude/sessions");
    std::fs::create_dir_all(&sessions).expect("sessions");
    std::fs::write(
        sessions.join(format!("{PID}.json")),
        format!(
            r#"{{"pid":{PID},"sessionId":"{SESSION}","cwd":"/w","version":"2.1.310","status":"idle","statusUpdatedAt":1,"procStart":"{START}","kind":"interactive","entrypoint":"cli"}}"#
        ),
    )
    .expect("record");
    let roster = accounts::Roster::default();
    let step = |why: &Restart, argv: &[&str]| {
        let snap = Snapshot {
            pid: PID,
            shell: SHELL,
            program: PathBuf::from(argv[0]),
            ..claude(argv)
        };
        let r = restart_as(
            &opts,
            why,
            &RestartAsk::of(why, 600),
            &roster,
            Ok(snap),
            None,
            &Unread,
        );
        r.report.step
    };
    assert_eq!(
        step(&Restart::Memory, &["/opt/claude"]),
        "would-restart",
        "CONTROL: the process plans a relaunch"
    );
    assert_eq!(
        step(&to_opus(), &["/opt/claude"]),
        format!("would-restart:model={FABLE_FALLBACK}")
    );
    assert_eq!(
        step(&to_opus(), &["/opt/claude", "--model", "claude-fable-5-2"]),
        format!("would-restart:model={FABLE_FALLBACK}")
    );
    assert_eq!(
        step(&to_opus(), &["/opt/claude", "--model", "claude-sonnet-4-6"]),
        REFUSED_LAUNCH_MODEL
    );
    assert_eq!(
        step(
            &Restart::Model {
                to: "claude-sonnet-4-6".to_string()
            },
            &["/opt/claude"]
        ),
        REFUSED_MODEL_MOVE
    );
    let on_opus = ["/opt/claude", "--model", FABLE_FALLBACK];
    assert_eq!(step(&back_to(None), &on_opus), "would-restart:model=-");
    assert_eq!(
        step(&back_to(Some("claude-fable-5-2")), &on_opus),
        "would-restart:model=claude-fable-5-2"
    );
    assert_eq!(
        step(&back_to(None), &["/opt/claude", "--model", "fable"]),
        REFUSED_NOT_ON_FALLBACK
    );
    assert!(
        std::fs::read_dir(&opts.state).map_or(true, |mut d| d.next().is_none()),
        "a dry run saves no record"
    );
}

/// A MODEL MOVE'S LINE KEEPS THE TAP THE FOOTER PROMISED: the move adds
/// `'--model' 'claude-opus-5-5'` to the line, and [`tap_room`] keeps room for it for a launch the
/// move admits. At an ordinary depth and at the deepest directory where [`tap_room`] still
/// promises the tap, the moved line keeps it. NEGATIVE CONTROL: one directory deeper the promise
/// is withheld although the unmoved line still keeps the tap, and the moved line drops it there:
/// a promise judged on the unmoved line alone would have been broken.
#[cfg(unix)]
#[test]
fn a_model_moves_line_keeps_the_tap_the_footer_promised() {
    let tmp = aterm_tempfile::TempDir::new().expect("tmp");
    let home = tmp.path();
    let value = super::super::tap::settings_value_carrying(
        Path::new("/Applications/aterm.app/Contents/MacOS/aterm"),
        true,
    )
    .expect("a value");
    let snap = |cwd: &str| Snapshot {
        shell: NO_SHELL,
        program: PathBuf::from(
            "/Users//example/Library/Application Support/aterm/pkg/agents/claude",
        ),
        argv: words(&[
            "/Users//example/Library/Application Support/aterm/pkg/agents/claude",
            "--dangerously-skip-permissions",
        ]),
        cwd: cwd.to_string(),
        route: crate::harness::accounts::AuthRoute::Stored,
        ..claude(&[])
    };
    let typed = |s: &Snapshot, argv: &[String]| {
        line_tap_kept(
            Dialect::Zsh,
            None,
            Some(s.cwd.as_str()),
            &s.program,
            argv,
            Some(SESSION),
            Some(TAP_ROOM_MODE),
            &[],
            None,
            Some(&value),
        )
    };
    let moved_of = |s: &Snapshot| {
        model_move_argv(&to_opus(), s)
            .ok()
            .flatten()
            .unwrap_or_else(|| panic!("a move of {:?}", s.argv))
    };
    let ordinary = snap("/Users//example/trust");
    assert!(tap_room(home, &ordinary, &value));
    let (line, dropped) =
        typed(&ordinary, &moved_of(&ordinary)).unwrap_or_else(|_| panic!("a moved line"));
    assert!(
        dropped.is_none() && line.contains("--settings"),
        "an ordinary move keeps the tap: {line}"
    );
    assert!(
        line.contains(&format!("'--model' '{FABLE_FALLBACK}'")),
        "{line}"
    );

    // The deepest directory whose line the footer still promises the tap.
    let deepest = (1..upgrade::MAX_LINE)
        .map(|n| format!("/Users//example/{}", "d".repeat(n)))
        .take_while(|cwd| tap_room(home, &snap(cwd), &value))
        .last()
        .expect("some directory has room");
    let edge = snap(&deepest);
    let (line, dropped) = typed(&edge, &moved_of(&edge))
        .unwrap_or_else(|_| panic!("the moved line at the edge still plans"));
    assert!(
        line.contains(&format!("'--model' '{FABLE_FALLBACK}'")),
        "the relaunch is still made on Opus: {line}"
    );
    assert!(
        dropped.is_none() && line.contains("--settings"),
        "at the edge the moved line keeps the tap the footer promised: {line}"
    );
    let deeper = format!("{deepest}d");
    let unmoved = snap(&deeper);
    assert!(
        !tap_room(home, &unmoved, &value),
        "one deeper the promise is withheld"
    );
    let (_, kept) = typed(&unmoved, &unmoved.argv)
        .unwrap_or_else(|_| panic!("the unmoved line one deeper plans"));
    assert!(
        kept.is_none(),
        "the unmoved line one deeper keeps the tap, so the move's reserve is what withholds it"
    );
    let (_, dropped) = typed(&unmoved, &moved_of(&unmoved))
        .unwrap_or_else(|_| panic!("the moved line one deeper still plans"));
    assert!(dropped.is_some(), "one deeper the moved line drops the tap");
}
