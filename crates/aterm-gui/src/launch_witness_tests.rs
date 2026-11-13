// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::human_input::Gesture;

/// A terminal whose host required and authorized the shell's nonce (every
/// aterm-spawned integrated shell), at a signed prompt, and the hex its
/// marks carry.
fn signed_prompt() -> (Terminal, String) {
    let nonce = [0x5Au8; 32];
    let hex: String = nonce.iter().map(|b| format!("{b:02x}")).collect();
    let mut t = Terminal::new(50, 140);
    t.authorize_shell_integration(nonce);
    t.set_require_shell_integration_nonce(true);
    t.process(format!("\x1b]133;A;id={hex}\x07$ \x1b]133;B;id={hex}\x07").as_bytes());
    (t, hex)
}

/// What zsh's preexec writes for `line` once the person's Enter is read
/// (`aterm_shell_integration.zsh`): 633;E, then 133;C, each signed.
fn preexec(line: &str, hex: &str) -> Vec<u8> {
    format!("\x1b]633;E;{line};id={hex}\x07\x1b]133;C;id={hex}\x07").into_bytes()
}

/// Claude Code's folder-trust dialog as it drew it (aterm-phase's capture).
fn trust_dialog() -> String {
    use aterm_phase::prompt::fixtures as f;
    format!("{}\r\n", f::screen(f::TRUST).join("\r\n"))
}

fn enter() -> Gesture {
    Gesture::PlainEnter
}

/// One parse-stage hold over `bytes`, as the reader takes it: the slice
/// and its mark under the lock, then the screen read with it released.
fn hold(term: &Mutex<Terminal>, bytes: &[u8], person: &HumanInputStamp, w: &LaunchWitness) {
    let unread = {
        let mut t = term.lock().expect("term");
        w.process(&mut t, bytes, ClockReading::now(), person)
    };
    if let Some(unread) = unread {
        w.read_screen(unread);
    }
}

/// A person types `line` (a gesture a character) and presses Enter.
fn typed_and_entered(person: &HumanInputStamp, line: &str) {
    for _ in line.chars() {
        person.note(0, Gesture::Other);
    }
    person.note(0, enter());
}

/// THE POSITIVE PATH at the parse stage: `claude⏎` typed and its signed
/// command start parsed records the count of that Enter as `person`, with
/// its id and word, on a screen showing no box — and the dialog Claude
/// Code then draws does not change it, while a gesture counted after the
/// mark's hold makes every later read say `since`.
#[test]
fn a_signed_command_start_records_the_count_at_its_hold() {
    let (t, hex) = signed_prompt();
    let term = Mutex::new(t);
    let (person, w) = (HumanInputStamp::default(), LaunchWitness::default());
    assert_eq!(w.read(0), None, "no mark yet");
    assert_eq!(w.json(0), "null");
    assert_eq!(w.status_fields(0), "launch=none launch_seq=-");
    typed_and_entered(&person, "claude");
    let mut slice = b"claude\r\n".to_vec();
    slice.extend(preexec("claude", &hex));
    hold(&term, &slice, &person, &w);
    let witnessed = Launch {
        id: 1,
        seq: 7,
        word: Some("claude"),
        why: Why::Person,
    };
    assert_eq!(w.read(7), Some(witnessed));
    assert_eq!(
        w.json(7),
        "{\"id\":1,\"seq\":7,\"word\":\"claude\",\"why\":\"person\"}"
    );
    assert_eq!(w.status_fields(7), "launch=person launch_seq=7");
    hold(&term, trust_dialog().as_bytes(), &person, &w);
    assert_eq!(w.read(7), Some(witnessed), "output after the mark");
    person.note(0, Gesture::Other);
    assert_eq!(w.read(8).map(|l| l.why), Some(Why::Since));
    assert_eq!(w.json(8), "{\"why\":\"since\"}");
    assert_eq!(w.status_fields(8), "launch=since launch_seq=7");
}

/// Each premise's negative control: a box on screen at the mark is
/// `boxed`; a first word no rule covers (a wrapper, a path to another
/// program, an assignment) is `other`; a last gesture that was not a plain
/// Enter (a letter after it, or none at all: a controller's line) is
/// `typed`; an unsigned session and a slice with no command start record
/// nothing; a newer mark replaces an older one.
#[test]
fn every_premise_missing_is_a_word_other_than_person() {
    let at_mark = |before: &str, line: &str, gestures: &[Gesture]| {
        let (mut t, hex) = signed_prompt();
        t.process(before.as_bytes());
        let term = Mutex::new(t);
        let (person, w) = (HumanInputStamp::default(), LaunchWitness::default());
        for g in gestures {
            person.note(0, *g);
        }
        hold(&term, &preexec(line, &hex), &person, &w);
        w.read(person.seq()).map(|l| l.why)
    };
    let enter_last = [Gesture::Other, enter()];
    let echo = |line: &str| format!("{line}\r\n");
    assert_eq!(
        at_mark(&echo("claude"), "claude", &enter_last),
        Some(Why::Person)
    );
    let codex = "/opt/bin/codex --yolo";
    assert_eq!(at_mark(&echo(codex), codex, &enter_last), Some(Why::Person));
    assert_eq!(
        at_mark(
            &format!("claude\r\n{}", trust_dialog()),
            "claude",
            &enter_last
        ),
        Some(Why::Boxed),
        "a dialog on screen at the mark"
    );
    for line in [
        "exec claude",
        "FOO=1 claude",
        "env claude",
        "claudette",
        "/bin/ls",
    ] {
        assert_eq!(
            at_mark(&echo(line), line, &enter_last),
            Some(Why::Other),
            "{line}"
        );
    }
    assert_eq!(
        at_mark(&echo("claude"), "claude", &[enter(), Gesture::Other]),
        Some(Why::Typed),
        "a key after the Enter"
    );
    assert_eq!(
        at_mark(&echo("claude"), "claude", &[]),
        Some(Why::Typed),
        "no gesture: a controller's line"
    );
    let term = Mutex::new(Terminal::new(24, 80));
    let (person, w) = (HumanInputStamp::default(), LaunchWitness::default());
    person.note(0, enter());
    hold(
        &term,
        b"\x1b]133;A\x07$ \x1b]133;B\x07claude\r\n\x1b]633;E;claude\x07\x1b]133;C\x07",
        &person,
        &w,
    );
    assert_eq!(w.read(1), None, "unsigned");
    let (t, hex) = signed_prompt();
    let term = Mutex::new(t);
    hold(&term, b"claude", &person, &w);
    assert_eq!(w.read(1), None, "no command start");
    hold(&term, &preexec("claude", &hex), &person, &w);
    assert_eq!(w.read(1).map(|l| (l.id, l.why)), Some((1, Why::Person)));
    hold(&term, b"\x1b[2J working", &person, &w);
    assert_eq!(
        w.read(1).map(|l| l.id),
        Some(1),
        "output is no command start"
    );
    person.note(0, enter());
    hold(
        &term,
        format!("\x1b]133;D;0;id={hex}\x07\x1b]133;A;id={hex}\x07$ \x1b]133;B;id={hex}\x07")
            .as_bytes(),
        &person,
        &w,
    );
    hold(&term, &preexec("ls", &hex), &person, &w);
    assert_eq!(
        w.read(2).map(|l| (l.id, l.seq, l.why)),
        Some((2, 2, Why::Other))
    );
}

/// THE LAUNCH'S INSTANT is every mark's, whatever its word: a command no
/// rule covers (`sh run-agy.sh`, a controller's line) is started at
/// the wall clock its hold read, with the person count loaded there; a
/// newer command's start replaces both.
#[test]
fn every_command_start_notes_its_instant_and_count() {
    let (t, hex) = signed_prompt();
    let term = Mutex::new(t);
    let (person, w) = (HumanInputStamp::default(), LaunchWitness::default());
    assert_eq!(w.started(), None, "no mark yet");
    hold(&term, b"sh run-agy.sh", &person, &w);
    assert_eq!(w.started(), None, "no command start");
    typed_and_entered(&person, "x");
    let before = now_unix_ms();
    hold(&term, &preexec("sh run-agy.sh", &hex), &person, &w);
    let after = now_unix_ms();
    let started = w.started().expect("a start");
    assert_eq!(started.seq, 2, "the count its hold loaded");
    assert!(
        (before..=after).contains(&started.at_ms),
        "{before} <= {} <= {after}",
        started.at_ms
    );
    assert_eq!(w.read(2).map(|l| l.why), Some(Why::Other));
    person.note(0, Gesture::Other);
    assert_eq!(w.started(), Some(started));
    hold(
        &term,
        format!("\x1b]133;D;0;id={hex}\x07\x1b]133;A;id={hex}\x07$ \x1b]133;B;id={hex}\x07")
            .as_bytes(),
        &person,
        &w,
    );
    hold(&term, &preexec("agy", &hex), &person, &w);
    let next = w.started().expect("the next start");
    assert_eq!(next.seq, 3);
    assert!(next.at_ms >= started.at_ms);
    let term = Mutex::new(Terminal::new(24, 80));
    let w = LaunchWitness::default();
    hold(
        &term,
        b"\x1b]133;A\x07$ \x1b]133;B\x07agy\r\n\x1b]633;E;agy\x07\x1b]133;C\x07",
        &person,
        &w,
    );
    assert_eq!(w.started(), None, "unsigned");
}

/// The mark is taken IN the hold that parsed the command start, never
/// after it: a gesture counted between that hold and the screen's
/// reading after the lock is outside the mark (`since`), and a mark whose
/// reading is still owed reads `pending`, never `person`.
#[test]
fn the_count_is_the_holds_and_an_unread_mark_is_pending() {
    let (mut t, hex) = signed_prompt();
    let (person, w) = (HumanInputStamp::default(), LaunchWitness::default());
    person.note(0, enter());
    let unread = w
        .process(
            &mut t,
            &preexec("codex", &hex),
            ClockReading::now(),
            &person,
        )
        .expect("a screen to read");
    assert_eq!(w.read(1).map(|l| l.why), Some(Why::Pending));
    assert_eq!(w.json(1), "{\"why\":\"pending\"}");
    person.note(0, Gesture::Other);
    w.read_screen(unread);
    assert_eq!(
        w.read(2),
        Some(Launch {
            id: 1,
            seq: 1,
            word: Some("codex"),
            why: Why::Since
        })
    );
}
