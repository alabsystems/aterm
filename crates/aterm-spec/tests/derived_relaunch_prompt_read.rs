// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-0 of `RelaunchPromptRead`, the relaunch line's read of a returned prompt. The
//! invariant is proved at `Buggy = 0` and caught at `Buggy = 1`; the liveness is proved under
//! its fairness and broken by `ElsewhereReadAsTyping` alone; that schedule and the wrapped
//! typing it resembles are walked. Tier-1 is `aterm-agent`'s
//! `the_real_prompt_read_conforms_to_the_model`.

use aterm_spec::derive::{relaunch_prompt_read_liveness, relaunch_prompt_read_model};
use aterm_spec::{interp, verify};

#[test]
fn relaunch_prompt_read_proves_and_catches_the_invariant_and_the_liveness() {
    let model = relaunch_prompt_read_model();
    let fired = interp::fired_actions(&model);
    for action in [
        "MarkHere",
        "MarkElsewhere",
        "Unmarked",
        "Stamped",
        "Unstamped",
        "EndsInBlank",
        "EndsInText",
        "Key",
        "Read",
        "WaitTyping",
        "WaitElsewhere",
        "WaitUnmarked",
    ] {
        assert!(fired.contains(action), "{action} fires: {fired:?}");
    }
    verify::prove_and_catch_scalar(
        &model,
        "relaunch prompt read: never typed after a person's keys at a marked prompt, only at \
         a prompt a mark or its shape attests, and `typing` said only with a person's keys \
         behind it",
    );
    verify::liveness_proves_and_catches_tiered(
        &model,
        &relaunch_prompt_read_liveness(),
        "RelaunchPromptRead: a prompt nobody keyed is typed at",
    );
}

/// The shell's mark names another row, the host stamps a person's input, nobody keyed: the
/// line goes in. Under `ElsewhereReadAsTyping` the same world never types: the read is taken
/// for typing, and every try after it waits.
#[test]
fn relaunch_prompt_read_types_at_tab_ones_prompt_and_tab_ones_rule_never_does() {
    let m = relaunch_prompt_read_model();
    let mut s = m.init_state();
    for action in ["MarkElsewhere", "Stamped", "EndsInBlank"] {
        assert!(m.fire(action, &mut s), "{action}: {s:?}");
    }
    assert!(
        m.action_enabled("Read", &s),
        "nobody keyed: the line may go in"
    );
    assert!(m.fire("Read", &mut s));
    assert_eq!(s["typed"], 1);

    let buggy = verify::buggy_baseline_with(&m, "ElsewhereReadAsTyping");
    let mut s = buggy.init_state();
    for action in [
        "MarkElsewhere",
        "Stamped",
        "EndsInBlank",
        "ElsewhereReadAsTyping",
    ] {
        assert!(buggy.fire(action, &mut s), "{action}: {s:?}");
    }
    assert!(
        !buggy.action_enabled("Read", &s),
        "read as typing, no later try types"
    );
    assert!(
        !buggy.check_invariant("TypingIsAPersonsWhereStamped", &s),
        "and the wait says `typing` with nobody behind it: {s:?}"
    );
    let stuck = interp::nonprogress_under(&buggy, &relaunch_prompt_read_liveness())
        .expect("tab #1's rule leaves a prompt nobody keyed untyped for good");
    let entry = stuck.entry();
    assert_eq!(
        (
            entry["mark"],
            entry["person"],
            entry["misread"],
            entry["typed"]
        ),
        (2, 0, 1, 0),
        "the stall is a prompt nobody keyed, read as typing:\n{stuck}"
    );
}

/// Wrapped typing: the mark names the row above the cursor because a person's command wrapped
/// past the prompt's row. Their keys stand, so no read types, and neither tempting fix may:
/// trusting the screen over the mark types onto their command, and so does the shape rule when
/// their command ends in a blank.
#[test]
fn relaunch_prompt_read_never_types_onto_wrapped_typing() {
    let m = relaunch_prompt_read_model();
    let mut s = m.init_state();
    for action in ["MarkElsewhere", "Stamped", "EndsInBlank", "Key"] {
        assert!(m.fire(action, &mut s), "{action}: {s:?}");
    }
    assert!(!m.action_enabled("Read", &s), "a person's keys stand: wait");
    for fix in ["ElsewhereTypedBlind", "ElsewhereByShape"] {
        let buggy = verify::buggy_baseline_with(&m, fix);
        let mut b = s.clone();
        assert!(buggy.fire(fix, &mut b), "{fix}");
        assert!(
            !buggy.check_invariant("NeverOverAPersonAtAMarkedPrompt", &b),
            "{fix} types onto a person's wrapped command: {b:?}"
        );
    }
}

/// No mark, no blank, nobody keyed: nothing says the shell is at a prompt (a command it runs
/// holds the terminal, as `claude; read`'s builtin does, or a shell with no integration draws a
/// prompt with no trailing blank), so the line waits and says `unmarked`, not `typing`.
/// `UnmarkedTypedByStamp` (the stamp standing in for the prompt's shape) types into it.
#[test]
fn relaunch_prompt_read_never_types_on_the_stamp_alone() {
    let m = relaunch_prompt_read_model();
    let mut s = m.init_state();
    for action in ["Unmarked", "Stamped", "EndsInText"] {
        assert!(m.fire(action, &mut s), "{action}: {s:?}");
    }
    assert!(!m.action_enabled("Read", &s), "no prompt is attested: wait");
    assert!(!m.action_enabled("WaitTyping", &s), "and not as typing");
    assert!(m.fire("WaitUnmarked", &mut s));
    assert_eq!(s["said"], 3);
    assert!(m.check_invariant("TypingIsAPersonsWhereStamped", &s));

    let buggy = verify::buggy_baseline_with(&m, "UnmarkedTypedByStamp");
    let mut b = buggy.init_state();
    for action in ["Unmarked", "Stamped", "EndsInText", "UnmarkedTypedByStamp"] {
        assert!(buggy.fire(action, &mut b), "{action}: {b:?}");
    }
    assert!(
        !buggy.check_invariant("OnlyAtAnAttestedPrompt", &b),
        "the stamp alone types where nothing says a prompt is: {b:?}"
    );
}
