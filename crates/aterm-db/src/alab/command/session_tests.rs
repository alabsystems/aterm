// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn at_terminal(asked: Asked, marker: Option<&'static str>, in_aterm: bool) -> Facts {
    Facts {
        stdin_tty: true,
        stdout_tty: true,
        asked,
        marker,
        in_aterm,
    }
}

fn answered(word: &str) -> Asked {
    Asked::Answered {
        word: word.to_owned(),
        session: None,
    }
}

/// `allowed`, for the session `sid`.
fn allowed_in(sid: &str) -> Asked {
    Asked::Answered {
        word: ALLOWED.to_owned(),
        session: Some(sid.to_owned()),
    }
}

/// A person's own tab reveals, in that tab's session; a spawned, held, driven, agent or remote
/// session refuses, and so does any session without a terminal on both ends, and an `allowed`
/// that names no session.
/// NEGATIVE CONTROL: with `decide`'s `Answered` arm accepting every word, the spawned session
/// reveals: `` spawned: Ok(..) ``.
#[test]
fn the_session_decides_and_only_a_persons_tab_reveals() {
    assert_eq!(
        decide(&at_terminal(allowed_in("s-0a1b"), None, true)).unwrap(),
        Place::Tab("s-0a1b".to_owned())
    );
    // A marker in the environment does not overrule the session: aterm strips them anyway.
    assert!(decide(&at_terminal(allowed_in("s-0a1b"), Some("CLAUDECODE"), true)).is_ok());
    assert!(decide(&at_terminal(answered(ALLOWED), None, true)).is_err());
    for word in [
        "spawned", "carried", "held", "driven", "agent", "remote", "indirect", "none",
    ] {
        let refused = decide(&at_terminal(answered(word), None, true));
        assert!(refused.is_err(), "{word}: {refused:?}");
        assert!(refused.unwrap_err().to_string().contains(word));
    }
    for (stdin_tty, stdout_tty) in [(false, true), (true, false), (false, false)] {
        let facts = Facts {
            stdin_tty,
            stdout_tty,
            ..at_terminal(allowed_in("s-0a1b"), None, true)
        };
        assert!(decide(&facts).unwrap_err().to_string().contains("terminal"));
    }
}

/// Outside aterm the agent marker stands in; a process that runs under aterm with no instance
/// to ask, and an instance that fails to answer, both refuse.
#[test]
fn outside_aterm_the_marker_stands_in_and_failures_refuse() {
    assert_eq!(
        decide(&at_terminal(Asked::NoInstance, None, false)).unwrap(),
        Place::Outside
    );
    let marked = decide(&at_terminal(Asked::NoInstance, Some("CLAUDECODE"), false));
    assert!(marked.unwrap_err().to_string().contains("CLAUDECODE"));
    assert!(decide(&at_terminal(Asked::NoInstance, None, true)).is_err());
    assert_eq!(
        decide(&at_terminal(answered("none"), None, false)).unwrap(),
        Place::Outside
    );
    assert!(decide(&at_terminal(answered("none"), Some("CODEX_SANDBOX"), false)).is_err());
    assert!(
        decide(&at_terminal(
            Asked::Failed("connect: timed out".to_owned()),
            None,
            true
        ))
        .is_err()
    );
}

/// An aterm image among the ancestors marks the process as running under aterm, whatever
/// its environment says; no other image does.
#[test]
fn an_aterm_ancestor_is_read_from_its_image() {
    use std::path::PathBuf;
    let image = |pid: u32| match pid {
        10 => Some(PathBuf::from("/bin/zsh")),
        20 => Some(PathBuf::from(
            "/Applications/aterm.app/Contents/MacOS/aterm",
        )),
        30 => Some(PathBuf::from("/usr/bin/script")),
        _ => None,
    };
    assert!(aterm_among(&[10, 20, 1000], &image));
    // The negative controls: no aterm ancestor, an unreadable one, and none at all.
    assert!(!aterm_among(&[10, 30], &image));
    assert!(!aterm_among(&[999], &image));
    assert!(!aterm_among(&[], &image));
}

/// Every instance is asked: a withholding word from any decides, an instance that did not
/// answer refuses, and only then does a person's tab elsewhere reveal.
/// NEGATIVE CONTROL: with `combine` taking the first answer, as asking only the nearest
/// ancestor did, the instance whose tab holds the caller goes unheard:
/// `` left: Answered("none") ``.
#[test]
fn every_instance_is_asked_and_one_that_withholds_decides() {
    let failed = || Asked::Failed("connect: refused".to_owned());
    assert_eq!(combine(Vec::new()), Asked::NoInstance);
    assert_eq!(combine(vec![answered("none")]), answered("none"));
    assert_eq!(
        combine(vec![answered("none"), allowed_in("s-77")]),
        allowed_in("s-77"),
        "the instance whose tab holds the caller answers for it, naming its session"
    );
    assert_eq!(
        combine(vec![allowed_in("s-77"), answered("agent")]),
        answered("agent"),
        "an instance nested in an agent's tab: the outer one sees the agent"
    );
    assert_eq!(
        combine(vec![answered("driven"), answered("spawned"), failed()]),
        answered("spawned"),
        "the first withholding word in WORDS' order"
    );
    assert_eq!(combine(vec![allowed_in("s-77"), failed()]), failed());
    assert_eq!(combine(vec![answered("none"), failed()]), failed());
}

/// The answer's grammar: one line, a known verdict.
#[test]
fn a_reveal_answer_is_one_line_with_a_known_verdict() {
    let line = |s: &str| vec![s.to_owned()];
    assert_eq!(
        parse(&line("verdict=allowed session=s-1")),
        allowed_in("s-1")
    );
    assert!(matches!(
        parse(&line("verdict=allowed session=$(x)")),
        Asked::Failed(_)
    ));
    assert_eq!(
        parse(&line("verdict=spawned session=-")),
        answered("spawned")
    );
    assert!(matches!(
        parse(&line("verdict=maybe session=-")),
        Asked::Failed(_)
    ));
    assert!(matches!(parse(&[]), Asked::Failed(_)));
    assert!(matches!(
        parse(&["verdict=allowed".to_owned(), "verdict=allowed".to_owned()]),
        Asked::Failed(_)
    ));
}
