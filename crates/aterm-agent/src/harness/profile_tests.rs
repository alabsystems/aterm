// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for [`super`] — the per-program profiles (design
//! `docs/DESIGN-aterm-wrapper-2026-09-17.md` §6.1-§6.3).
//!
//! The four the stage asks for, each with a negative control:
//!
//! * **the spine classifies a REAL codex session and a REAL emacs session**
//!   from fixtures, with no vendor surface in the path;
//! * **the profile selects correctly when both a forked and a vendor program
//!   are present**, and when only one of them is;
//! * **a capability a program cannot serve reads `unsupported` WITH ITS
//!   REASON**, never silence and never `ok` by omission;
//! * every profile renders a `harness.toml` that [`Contract::admit`] accepts,
//!   so the grammar and the profiles cannot drift.
//!
//! # Where the fixtures come from
//!
//! [`CODEX_STATUS`], [`CODEX_SCREEN`], [`EMACS_STATUS`] and [`EMACS_SCREEN`]
//! were CAPTURED 2026-09-21 from real programs running in a headless aterm
//! (`ATERM_HEADLESS=1 ATERM_COLUMNS=110 ATERM_LINES=32`), driven over its
//! control socket with `send --stdin` and `await idle`, and read back with
//! `status` and `text --json tail=32 trim`. They are verbatim; nothing was
//! hand-written into them and nothing was edited out. The codex screen is its
//! OWN directory-trust gate, which is what a real codex session shows first
//! and is exactly why a frame test for codex would be guesswork.

use std::path::PathBuf;

use super::*;
use crate::harness::observe::{Observer, Sample, StatusSample};
use crate::supervise::screen::parse_text_json;

// ---------------------------------------------------------------------------
// Captured fixtures — real sessions, verbatim
// ---------------------------------------------------------------------------

/// A real `codex` (codex-cli 0.155.1) session's `status`, MEASURED.
const CODEX_STATUS: &str = "OK schema=1 sid=0 subject=cd%20/Users//example/aterm%20&&%20codex \
     subject_source=osc observed=true phase=quiet since_ms=5457 outcome=none exit_code=- \
     signal=- detail=codex confidence=strong reasons=shell_block,stall attribution=live \
     fs_consent=unknown conflict=false revision=16 enabled=true hold=0 fabric=absent \
     fabric_rtt_ms=- fabric_link_age_ms=- identity=- hand=- level=quiet story=0";

/// The same session's screen, MEASURED. Codex's first screen is its own
/// directory-trust gate.
const CODEX_SCREEN: &str = r#"{"rows":["> You are in /Users//example/aterm","","  Do you trust the contents of this directory? Working with untrusted contents comes with higher risk of","  prompt injection. Trusting the directory allows project-local config, hooks, and exec policies to load.","","› 1. Yes, continue","  2. No, quit","","  Press enter to continue"],"cursor":{"row":8,"col":25,"visible":false,"style":"blinking_block"},"dims":{"rows":32,"cols":110},"seq":450,"trimmed":23}"#;

/// A real `emacs -nw -Q` (GNU Emacs 30.2) session's `status`, MEASURED.
const EMACS_STATUS: &str = "OK schema=1 sid=0 subject=TERM=xterm-256color%20emacs%20-nw%20-Q \
     subject_source=osc observed=true phase=running since_ms=556 outcome=none exit_code=- \
     signal=- detail=emacs confidence=strong reasons=shell_block,content_activity \
     attribution=live fs_consent=unknown conflict=false revision=19 enabled=true hold=0 \
     fabric=absent fabric_rtt_ms=- fabric_link_age_ms=- identity=- hand=- level=quiet story=0";

/// The same session's screen, MEASURED. The last row is the Emacs mode line;
/// row 5 is aterm's own DA2 answer (`>41;100;0c`) landing in `*scratch*`,
/// which is what the term file of §6.2 exists to make unnecessary.
const EMACS_SCREEN: &str = r#"{"rows":["File Edit Options Buffers Tools Lisp-Interaction Help",";; This buffer is for text that is not saved, and for Lisp evaluation.",";; To create a file, visit it with ‘C-x C-f’ and enter text in its buffer.","","","41;100;0c","","","","","","","","","","","","","","","","","","","","","","","","","-UUU:**-  F1  *scratch*      All   L5     (Lisp Interaction ElDoc) -------------------------------------------"],"cursor":{"row":5,"col":9,"visible":true,"style":"blinking_block"},"dims":{"rows":32,"cols":110},"seq":55,"trimmed":1}"#;

fn sample(status_line: &str, screen_json: &str) -> Sample {
    Sample {
        status: StatusSample::parse_line(status_line).expect("the captured status line parses"),
        grid: Some(parse_text_json(screen_json).expect("the captured screen parses")),
        offscreen: None,
        search: None,
    }
}

fn rows(screen_json: &str) -> Vec<String> {
    parse_text_json(screen_json)
        .expect("the captured screen parses")
        .rows
}

// ---------------------------------------------------------------------------
// The spine, on programs that are not Claude Code
// ---------------------------------------------------------------------------

/// THE STAGE'S CENTRAL CLAIM: the stage-1 spine, unchanged, classifies a real
/// codex session and a real emacs session. Nothing vendor-side is read — the
/// only inputs are aterm's own `status` reply and its own parsed grid.
#[test]
fn the_spine_classifies_a_real_codex_and_a_real_emacs_session() {
    for (label, status, screen, want) in [
        ("codex", CODEX_STATUS, CODEX_SCREEN, "codex"),
        ("emacs", EMACS_STATUS, EMACS_SCREEN, "emacs"),
    ] {
        let mut obs = Observer::new();
        let events = obs.on_sample(&sample(status, screen), 1_000);
        assert!(
            !events.is_empty(),
            "{label}: the spine emitted nothing for a live session"
        );
        assert_eq!(
            obs.program(),
            Some(want),
            "{label}: the spine did not name the program"
        );
        // And the profile layer agrees, from the same two fields.
        let st = StatusSample::parse_line(status).expect("status parses");
        let (profile, how) = identify(st.detail.as_deref(), &rows(screen))
            .unwrap_or_else(|| panic!("{label}: nothing identified the program"));
        assert_eq!(profile.id, want, "{label}");
        assert_eq!(how, Named::Detail, "{label}: detail= is rank 1");
    }
}

/// The ADOPTED case, which is the one that killed the first design: `detail=`
/// is absent and the grid alone must name the program. Emacs earns its own
/// frame test here; codex deliberately has none and reads `None`, which is
/// the honest answer rather than a guess.
#[test]
fn a_session_with_no_detail_is_named_by_the_grid_where_a_frame_test_exists() {
    let emacs = rows(EMACS_SCREEN);
    let (profile, how) = identify(None, &emacs).expect("the mode line names emacs");
    assert_eq!(profile.id, "emacs");
    assert_eq!(how, Named::Frame);

    // NEGATIVE CONTROL: codex's real screen names nothing without `detail=`.
    assert!(
        identify(None, &rows(CODEX_SCREEN)).is_none(),
        "codex has no frame test and must not be guessed at"
    );
    // NEGATIVE CONTROL: an empty grid names nothing.
    assert!(identify(None, &[]).is_none());
}

/// The mode-line test says no to everything that is not an Emacs mode line,
/// including the two shapes most likely to collide with it.
#[test]
fn the_emacs_frame_test_refuses_everything_that_is_not_a_mode_line() {
    assert!(has_emacs_modeline(&rows(EMACS_SCREEN)));

    let no: &[&str] = &[
        "",
        "---",
        "--------------------------------------------------",
        "- a bullet that runs long enough to pass the width test -----",
        "─────────────────────────────────────────────────────────────",
        ";; a transcript row that merely mentions -UUU:**- somewhere in it",
        "-UUU:**- short",
        // The two shapes a terminal actually shows that START with `-`.
        "-rw-r--r--  1 owner  staff   12345 Sep 21 12:34 some-file-name.rs",
        "--- a/crates/aterm-agent/src/harness/profile.rs ------------------",
    ];
    for row in no {
        assert!(
            !has_emacs_modeline(&[(*row).to_string()]),
            "must not read as a mode line: {row:?}"
        );
    }
    // A real Claude Code composer screen is not an emacs screen — and the
    // control's own control: the shipped frame test DOES see it, so this
    // negative is not vacuous.
    let claude = crate::supervise::prompt::fixtures::composer("? for shortcuts");
    assert!(!has_emacs_modeline(&claude));
    assert!(has_composer_frame(&claude), "the control's control");
    assert_eq!(
        identify(None, &claude).map(|(p, how)| (p.id, how)),
        Some(("claude", Named::Frame))
    );
}

/// `detail=` is matched on the command's HEAD, basename-wise, and on nothing
/// else. A path still names its program; a lookalike does not.
#[test]
fn the_detail_word_is_a_basename_and_never_a_guess() {
    for (detail, want) in [
        ("emacs", Some("emacs")),
        ("/opt/homebrew/bin/emacs", Some("emacs")),
        ("emacs -nw -Q", Some("emacs")),
        ("codex", Some("codex")),
        ("acx", Some("codex")),
        ("claude", Some("claude")),
        ("emacsclient", Some("emacs")),
        ("emacs-lisp-mode", None),
        ("my-claude-wrapper", None),
        ("", None),
    ] {
        let got = identify(Some(detail), &[]).map(|(p, _)| p.id);
        assert_eq!(got, want, "detail={detail:?}");
    }
}

// ---------------------------------------------------------------------------
// Selection: the fork and the vendor
// ---------------------------------------------------------------------------

/// Design §6.3: `wraps_any` order is the ONLY fork-selection mechanism. With
/// both present the fork wins; with only the vendor present the vendor wins;
/// with neither, nothing is selected and the program runs plain.
#[test]
fn the_profile_selects_the_fork_when_both_are_present() {
    let both = |name: &str| matches!(name, "acx" | "codex");
    let picked = select(&CODEX, &mut { both }).expect("a target is selected");
    assert_eq!(picked.program, "acx");
    assert!(picked.forked);
    assert_eq!(picked.rank, 0);

    let vendor_only = |name: &str| name == "codex";
    let picked = select(&CODEX, &mut { vendor_only }).expect("the vendor is selected");
    assert_eq!(picked.program, "codex");
    assert!(!picked.forked, "the vendor row is not a fork");
    assert_eq!(picked.rank, 1);

    // NEGATIVE CONTROL: an unusable family selects nothing.
    let none = |_: &str| false;
    assert_eq!(select(&CODEX, &mut { none }), None);

    // A fork that is INSTALLED but not aligned is not usable, and the caller
    // is what says so — the same function, a different world.
    let installed_but_unaligned = |name: &str| name == "codex";
    assert_eq!(
        select(&CODEX, &mut { installed_but_unaligned })
            .map(|s| s.program)
            .unwrap(),
        "codex"
    );
}

/// A profile with no fork row never reports one, whatever is installed.
#[test]
fn a_profile_with_no_fork_never_reports_one() {
    for profile in [&CLAUDE, &EMACS] {
        let any = |_: &str| true;
        let picked = select(profile, &mut { any }).expect("its one name is selected");
        assert!(!picked.forked, "{}", profile.id);
        assert_eq!(picked.rank, 0);
        assert!(profile.forks.is_empty(), "{}", profile.id);
    }
}

// ---------------------------------------------------------------------------
// Capabilities a program cannot serve
// ---------------------------------------------------------------------------

/// THE STAGE'S SECOND CLAIM: a capability a program cannot serve reads
/// `unsupported` and CARRIES ITS REASON. Checked exhaustively, so a reason
/// can never be added to one row and forgotten on another.
#[test]
fn an_unservable_capability_reads_unsupported_with_its_reason() {
    let mut unsupported = 0usize;
    for profile in PROFILES {
        for row in profile.caps {
            let serve = profile.serves(row.id, profile.wraps_any[profile.wraps_any.len() - 1]);
            match serve {
                Serve::Ok => assert_eq!(serve.reason(), None, "{} {}", profile.id, row.id),
                Serve::Degraded(why) | Serve::Unsupported(why) => {
                    assert!(
                        why.len() > 24 && !why.ends_with('.'),
                        "{} {}: the reason must be a sentence, not a token: {why:?}",
                        profile.id,
                        row.id
                    );
                }
            }
            if matches!(serve, Serve::Unsupported(_)) {
                unsupported += 1;
                assert!(!serve.runs(), "{} {}", profile.id, row.id);
                assert_eq!(serve.word(), "unsupported");
                assert!(
                    serve
                        .row(row.id)
                        .starts_with(&format!("{}: unsupported — ", row.id))
                );
            }
        }
    }
    assert!(
        unsupported >= 6,
        "the three profiles must actually differ; only {unsupported} unsupported rows"
    );
}

/// Emacs is the extreme case and the point of the stage: five of the eight
/// Claude functions are simply not things emacs has, and each says why.
#[test]
fn emacs_names_what_it_cannot_do_rather_than_going_quiet() {
    for id in [
        "rm-approve",
        "usage-hud",
        "model-failover",
        "accounts",
        "limits",
    ] {
        let serve = EMACS.serves(id, "emacs");
        assert!(
            matches!(serve, Serve::Unsupported(_)),
            "emacs {id} must be unsupported, got {}",
            serve.word()
        );
        assert!(serve.reason().is_some_and(|w| !w.is_empty()));
    }
    // And the two that DO work with zero hooks — which is the central law
    // holding on a program that has no hooks to remove.
    assert_eq!(EMACS.serves("disk", "emacs"), Serve::Ok);
    assert_eq!(EMACS.serves("introspect", "emacs"), Serve::Ok);
    assert!(EMACS.serves("liveness", "emacs").runs());
    assert_eq!(EMACS.channels.hooks, Hooks::None);
}

/// The fork upgrades exactly the two capabilities design §6.3 names, and
/// nothing else — the decision rule made checkable.
#[test]
fn the_fork_upgrades_exactly_the_capabilities_the_rule_allows() {
    let upgraded: Vec<&str> = CODEX
        .caps
        .iter()
        .filter(|c| CODEX.serves(c.id, "acx") != CODEX.serves(c.id, "codex"))
        .map(|c| c.id)
        .collect();
    assert_eq!(
        upgraded,
        vec!["rm-approve", "liveness", "exact-blocks", "hooks-trusted"]
    );
    assert_eq!(CODEX.serves("exact-blocks", "acx"), Serve::Ok);
    assert!(matches!(
        CODEX.serves("exact-blocks", "codex"),
        Serve::Unsupported(_)
    ));
    // Every upgraded row is one the §6.3 rule actually admits.
    assert_eq!(
        needs_fork(&Asks {
            unemitted_signal: true,
            ..Asks::default()
        }),
        Some("needs a signal the vendor build does not emit")
    );
    assert_eq!(
        needs_fork(&Asks {
            hand_trusted_hook: true,
            ..Asks::default()
        }),
        Some("needs a hook the user would otherwise trust by hand")
    );
    assert_eq!(
        needs_fork(&Asks {
            decision_channel: true,
            ..Asks::default()
        }),
        Some("changes the program's own decision channel")
    );
    // NEGATIVE CONTROL: everything reachable through -c/--profile/rules stays
    // in the harness.
    assert_eq!(needs_fork(&Asks::default()), None);
}

/// A capability id no profile declares reads `unsupported` with the reason
/// named — never `ok` by omission.
#[test]
fn an_undeclared_capability_is_unsupported_and_not_silently_ok() {
    for profile in PROFILES {
        let serve = profile.serves("teleportation", profile.id);
        assert_eq!(serve, Serve::Unsupported(UNDECLARED), "{}", profile.id);
        assert!(!serve.runs());
    }
}

/// No profile claims OSC 133, and every profile therefore bounds a turn with
/// `status`. This is the measurement the design rests on, pinned so a future
/// edit that quietly flips it fails here.
#[test]
fn no_shipped_profile_claims_osc_133() {
    for profile in PROFILES {
        assert!(!profile.channels.osc133, "{}", profile.id);
        assert_eq!(
            profile.channels.turns,
            TurnEvidence::Phase,
            "{}",
            profile.id
        );
        assert_eq!(profile.channels.turns.as_str(), "phase");
        assert!(
            matches!(
                profile.serves("exact-blocks", profile.id),
                Serve::Unsupported(_)
            ),
            "{}",
            profile.id
        );
    }
    assert_eq!(TurnEvidence::Blocks.as_str(), "blocks");
}

// ---------------------------------------------------------------------------
// The rendered contract
// ---------------------------------------------------------------------------

const DIGEST: &str = "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

/// Every profile renders a `harness.toml` the packaging grammar ADMITS, and
/// the admitted contract says what the profile said. This is the seam between
/// stage 6 and stage 7, and it is a test rather than a convention.
#[test]
fn every_profile_renders_an_admissible_contract() {
    for profile in PROFILES {
        let target = profile.wraps_any[profile.wraps_any.len() - 1];
        let contract = profile
            .contract(target, DIGEST)
            .unwrap_or_else(|e| panic!("{}: {e}", profile.id));
        assert_eq!(contract.abi, super::super::align::HARNESS_ABI);
        assert_eq!(contract.policy, profile.policy);
        assert_eq!(contract.patch, profile.patch);
        assert_eq!(contract.probe_set, DIGEST);
        assert_eq!(contract.wraps_any, profile.wraps_any);
        assert_eq!(contract.tools.len(), profile.tools.len(), "{}", profile.id);
        // Only the runnable capabilities are declared, and all of them are.
        let declared: Vec<&str> = contract
            .capabilities
            .iter()
            .map(|c| c.id.as_str())
            .collect();
        assert_eq!(declared, profile.runnable(target), "{}", profile.id);
        for cap in &contract.capabilities {
            let row = profile.cap(&cap.id).expect("a declared cap has a row");
            assert_eq!(cap.needs, row.needs, "{} {}", profile.id, cap.id);
            assert_eq!(cap.max_level, row.max_level, "{} {}", profile.id, cap.id);
        }
        // NEGATIVE CONTROL: nothing a profile cannot serve leaks into the
        // contract.
        for row in profile.caps {
            if !profile.serves(row.id, target).runs() {
                assert!(
                    !declared.contains(&row.id),
                    "{}: {} is unsupported but declared",
                    profile.id,
                    row.id
                );
            }
        }
    }
}

/// The contract renders against a FORKED target too, and the fork's extra
/// capabilities appear there and only there.
#[test]
fn the_forked_contract_declares_what_only_the_fork_can_serve() {
    let vendor = CODEX.contract("codex", DIGEST).expect("vendor contract");
    let forked = CODEX.contract("acx", DIGEST).expect("forked contract");
    let ids =
        |c: &Contract| -> Vec<String> { c.capabilities.iter().map(|x| x.id.clone()).collect() };
    assert!(!ids(&vendor).contains(&String::from("exact-blocks")));
    assert!(ids(&forked).contains(&String::from("exact-blocks")));
}

/// The claude profile's shim is design §6.1's, byte for byte, and it renders
/// with both substitutions resolved.
#[test]
fn the_claude_shim_renders_with_both_substitutions() {
    let contract = CLAUDE.contract("claude", DIGEST).expect("admitted");
    assert_eq!(
        contract.shim.env,
        vec![(String::from("DISABLE_UPDATES"), String::from("1"))]
    );
    let rendered = contract
        .shim
        .render(&PathBuf::from("/store/tree"), &PathBuf::from("/state"))
        .expect("the rendered shim is re-admitted");
    assert_eq!(
        rendered.args,
        vec![
            String::from("--plugin-dir"),
            String::from("/store/tree/plugin"),
            String::from("--settings"),
            String::from("/state/settings.json"),
        ]
    );
}

/// The codex prelude carries only `-c`/`-p`, resolves `${HARNESS_ROOT}` in
/// the notify command, and never names a flag outside the closed vocabulary.
#[test]
fn the_codex_prelude_is_config_only_and_resolves_its_tree_path() {
    let contract = CODEX.contract("codex", DIGEST).expect("admitted");
    for token in contract.shim.args.iter().filter(|a| a.starts_with('-')) {
        assert!(
            matches!(token.as_str(), "-c" | "-p"),
            "codex must reach the program through config alone, not {token:?}"
        );
    }
    let rendered = contract
        .shim
        .render(&PathBuf::from("/store/tree"), &PathBuf::from("/state"))
        .expect("rendered");
    assert!(rendered.args.iter().all(|a| !a.contains("${")));
    // Codex sets CODEX_HOME dynamically, after the PTY sanitizer — never in
    // the signed shim env.
    assert!(contract.shim.env.is_empty());
}

/// The `notify` command — the first trust-free channel — cannot ride in the
/// contract's `[shim] args`, because the grammar refuses a string carrying a
/// quote and a `notify` value is a TOML array of strings. It rides the
/// rendered config instead, and the tree path arrives there.
///
/// The first assertion is the CONSTRAINT itself, pinned: if the contract
/// grammar ever admits a quote, this test says so rather than leaving the
/// workaround in place unexplained.
#[test]
fn the_notify_command_rides_the_rendered_config_and_not_the_shim_args() {
    let quoted = "abi = 1\nwraps_any = [\"codex\"]\ncapabilities = [\"introspect\"]\n\n\
         [shim]\nargs = [\"-c\", \"notify=[\\\"x\\\"]\"]\n\n\
         [[capability]]\nid = \"introspect\"\n";
    assert!(
        Contract::admit(quoted).is_err(),
        "the contract grammar must still refuse a quoted string"
    );

    assert!(!CODEX.args.iter().any(|a| a.contains("notify")));
    assert!(CODEX_CONFIG.contains("notify = [\"${HARNESS_ROOT}/bin/claude-harness\""));
    let laid = render_tree_text(
        CODEX_CONFIG,
        &PathBuf::from("/store/tree"),
        &PathBuf::from("/state"),
    );
    assert!(laid.contains("notify = [\"/store/tree/bin/claude-harness\", \"codex-notify\"]"));
    assert!(!laid.contains("${"), "nothing unsubstituted may be laid");
    // The SIGNED bytes keep the token, so tree_root still covers what was
    // published.
    assert!(CODEX_CONFIG.contains("${HARNESS_ROOT}"));
}

/// The emacs profile delivers through one loader variable and no argument at
/// all, and the rendered value ends in the trailing colon that keeps emacs'
/// default load-path (MEASURED: load-path length 28 with it).
#[test]
fn the_emacs_profile_delivers_through_emacsloadpath_alone() {
    let contract = EMACS.contract("emacs", DIGEST).expect("admitted");
    assert!(contract.shim.args.is_empty());
    let rendered = contract
        .shim
        .render(&PathBuf::from("/store/tree"), &PathBuf::from("/state"))
        .expect("rendered");
    assert_eq!(
        rendered.env,
        vec![(
            String::from("EMACSLOADPATH"),
            String::from("/store/tree/emacs/lisp:")
        )]
    );
    assert!(!EMACS.reach_note.is_empty(), "the D2 gap must be printable");
}

// ---------------------------------------------------------------------------
// The tree
// ---------------------------------------------------------------------------

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "aterm-harness-profile-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default()
    ));
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

/// The tree files land where §1.3 says, with the bytes the profile declares.
#[test]
fn materialize_lays_the_tree_the_profile_declares() {
    let root = temp_dir("tree");
    let mut total = 0usize;
    for profile in PROFILES {
        let written = materialize(profile, &root).expect("materialized");
        assert_eq!(written.len(), profile.files.len(), "{}", profile.id);
        for (path, file) in written.iter().zip(profile.files) {
            let body = std::fs::read_to_string(path).expect("read back");
            assert_eq!(body, file.body, "{}", file.path);
            assert!(path.ends_with(file.path), "{}", file.path);
            total += 1;
        }
    }
    assert!(total >= 5, "the codex and emacs trees must both be laid");
    assert!(root.join("emacs/lisp/term/xterm-256color.el").is_file());
    assert!(root.join("codex/rules/aterm.rules").is_file());
    std::fs::remove_dir_all(&root).ok();
}

/// NEGATIVE CONTROL: an escaping tree path is refused at the boundary, even
/// though this crate wrote the table. Data is checked where it is used.
#[test]
fn materialize_refuses_a_path_that_escapes_the_root() {
    let root = temp_dir("escape");
    for bad in ["/etc/passwd", "../outside", "a/../../b", "", "a\\b", "a//b"] {
        let profile = Profile {
            files: Box::leak(Box::new([TreeFile {
                path: Box::leak(bad.to_string().into_boxed_str()),
                body: "x",
                exec: false,
            }])),
            ..EMACS
        };
        let err = materialize(&profile, &root).expect_err(&format!("{bad:?} must be refused"));
        assert!(err.contains("safe relative path"), "{bad:?}: {err}");
    }
    std::fs::remove_dir_all(&root).ok();
}

/// The elisp is syntactically loadable and the term file's chain is the one
/// design §6.2 measured: our file loads FIRST, requires stock `term/xterm`
/// itself, sets the capability list BEFORE the init function runs, and lets
/// `terminal-init-xterm` be what runs.
///
/// `emacs --batch` has no tty frame, so it never runs
/// `tty-run-terminal-initialization` by itself; the probe calls it
/// explicitly, which is the only honest way to exercise the chain in batch.
/// Where emacs is not installed this test says so and returns: a skip that is
/// PRINTED is not a silent pass.
#[test]
fn the_emacs_term_file_chain_loads_in_the_measured_order() {
    let Ok(emacs) = which_emacs() else {
        eprintln!(
            "profile: emacs is not installed on this machine — the term-file \
             chain was NOT exercised here (it is MEASURED in the doc comment \
             on EMACS_TERM_FILE)"
        );
        return;
    };
    let root = temp_dir("elisp");
    materialize(&EMACS, &root).expect("materialized");
    let lisp = root.join("emacs/lisp");
    let out = std::process::Command::new(emacs)
        .arg("--batch")
        .arg("--eval")
        .arg(
            "(progn \
               (tty-run-terminal-initialization (selected-frame) \"xterm-256color\") \
               (princ (format \"initted=%s caps=%S harness=%s xterm=%s\\n\" \
                 (terminal-parameter (selected-frame) 'terminal-initted) \
                 xterm-extra-capabilities \
                 (featurep 'aterm-harness) \
                 (featurep 'term/xterm))))",
        )
        .env("EMACSLOADPATH", format!("{}:", lisp.display()))
        .env("HOME", &root)
        .output()
        .expect("emacs --batch runs");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "emacs exited non-zero:\n{text}");
    assert!(
        text.contains("initted=terminal-init-xterm"),
        "stock terminal-init-xterm must be what runs:\n{text}"
    );
    assert!(
        text.contains("modifyOtherKeys") && text.contains("setSelection"),
        "the capability list must be set before the init function:\n{text}"
    );
    assert!(
        !text.contains("getSelection"),
        "getSelection must never be asked for:\n{text}"
    );
    assert!(
        text.contains("harness=t"),
        "the reporter must load from the term file:\n{text}"
    );
    assert!(
        text.contains("xterm=t"),
        "stock term/xterm must be required by our file, since finding ours \
         stops the search:\n{text}"
    );
    std::fs::remove_dir_all(&root).ok();
}

fn which_emacs() -> Result<PathBuf, ()> {
    let path = std::env::var_os("PATH").ok_or(())?;
    std::env::split_paths(&path)
        .map(|d| d.join("emacs"))
        .find(|p| p.is_file())
        .ok_or(())
}

/// The codex tree's shipped bytes say what they are: the config selects the
/// profile the prelude names, the rules file is the trust-free lane, and
/// every hook command carries three arguments so a disabled capability makes
/// it inert (design §4.5 rule (b)).
#[test]
fn the_codex_tree_carries_the_trust_free_lane_and_three_arg_hooks() {
    assert!(CODEX_CONFIG.contains("[profiles.aterm]"));
    assert!(CODEX_CONFIG.contains("check_for_update_on_startup = false"));
    assert!(CODEX_CONFIG.contains("notification_method = \"osc9\""));
    assert!(CODEX.args.contains(&"-p") && CODEX.args.contains(&"aterm"));
    assert!(CODEX_RULES.contains("prefix_rule(pattern=[\"rm\"], decision=\"allow\")"));
    // Every hook command is `<bridge> <event> <capability>` — three words.
    let mut commands = 0usize;
    for line in CODEX_HOOKS.lines() {
        let Some((_, rest)) = line.split_once("\"command\": \"") else {
            continue;
        };
        let cmd = rest.trim_end_matches("\",").trim_end_matches('"');
        assert_eq!(
            cmd.split_whitespace().count(),
            3,
            "a hook command must carry the bridge, the event and the \
             capability id: {cmd:?}"
        );
        assert!(
            cmd.starts_with("${CLAUDE_PLUGIN_ROOT}/hooks/bridge.sh"),
            "{cmd:?}"
        );
        commands += 1;
    }
    assert!(
        commands >= 2,
        "the codex hooks.json must register something"
    );
    // And the harness never asks codex to skip its own trust gate.
    assert!(!CODEX_HOOKS.contains("dangerously"));
}

/// Every profile answers `profile_for` under its id and under every name it
/// wraps, and nothing else.
#[test]
fn profile_for_resolves_ids_and_wrapped_names_only() {
    assert_eq!(profile_for("claude").map(|p| p.id), Some("claude"));
    assert_eq!(profile_for("codex").map(|p| p.id), Some("codex"));
    assert_eq!(profile_for(FORK_ROW).map(|p| p.id), Some("codex"));
    assert_eq!(profile_for("emacs").map(|p| p.id), Some("emacs"));
    assert_eq!(profile_for("vim").map(|p| p.id), None);
    assert_eq!(profile_for("").map(|p| p.id), None);
    for profile in PROFILES {
        for name in profile.wraps_any {
            assert!(profile.wraps(name), "{} {name}", profile.id);
        }
        assert!(!profile.wraps("vim"), "{}", profile.id);
    }
}
