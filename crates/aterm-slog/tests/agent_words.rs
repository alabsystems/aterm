// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The agent words `stopped` and a bare `wall`: in the `word=` vocabulary, typed verdicts of
//! their own, and no step of the tracker's.

use aterm_slog::vocab::WORD_WORDS;
use aterm_slog::{
    Actor, BoxWord, Conf, Fields, Key, Kind, Log, Mark, Open, Src, Stamp, TurnTracker, Verdict,
    WallKind,
};

fn at(ms: u64) -> Stamp {
    Stamp::new(1_760_000_000_000 + ms, ms)
}

fn need_words(log: &Log) -> Vec<&'static str> {
    log.state().needs.iter().flatten().map(|n| n.word).collect()
}

/// D13, D14: `stopped` and a wall with no kind are agent words. A `word=` field takes each, so
/// the log row and the fold's phase read the word and not `unknown`; each is a verdict of its
/// own that reads back as its word; and the tracker, which has a step only for a wall with a
/// kind, leaves an open box as it was on either. NEGATIVE CONTROLS: a word outside the
/// vocabulary is refused by the field and reads `unknown` as a verdict, a wall of an unknown
/// kind is still `unknown` (no kind is guessed), a wall with a kind is still that wall, and
/// `busy` does end the box the two words left open.
#[test]
fn stopped_and_wall_are_agent_words() {
    for (w, v) in [("stopped", Verdict::Stopped), ("wall", Verdict::Walled)] {
        assert!(WORD_WORDS.contains(&w), "{w} is a word= word");
        assert!(
            Fields::new().word(Key::Word, w).is_ok(),
            "word={w} is taken"
        );
        let read = Verdict::from_agent_word(w, None);
        assert_eq!(read, v);
        assert_ne!(read, Verdict::Unknown);
        assert_eq!(read.word(), w, "{w} reads back");
    }
    assert!(
        Fields::new().word(Key::Word, "wall:new-kind").is_err(),
        "CONTROL: a word outside the vocabulary is refused"
    );
    assert_eq!(
        Verdict::from_agent_word("wall:new-kind", None),
        Verdict::Unknown,
        "CONTROL: an unknown wall kind is not the bare wall"
    );
    assert_eq!(
        Verdict::from_agent_word("wall:usage-session", None),
        Verdict::Wall(WallKind::UsageSession),
        "CONTROL: a wall with a kind is unchanged"
    );
    assert_eq!(
        Verdict::from_agent_word("wall:unresponsive", Some("stopped")),
        Verdict::Unresponsive,
        "CONTROL: the input stall is unchanged"
    );

    let mut log = Log::new(0);
    log.open_session(
        Kind::Spawn,
        Src::Aterm,
        Conf::Exact,
        Actor::Aterm,
        Fields::new(),
        at(0),
    )
    .expect("the session opens");
    let run = log
        .open(
            Open::new(Kind::Agent, Src::Status, Conf::Strong)
                .fields(Fields::new().tok(Key::Agent, "claude").expect("agent")),
            at(0),
        )
        .expect("the agent run opens");
    for (ms, w) in [(10, "wall"), (20, "stopped")] {
        log.mark(
            Mark::new(Kind::Agent, Src::Harness, Conf::Claim)
                .at(run)
                .fields(Fields::new().word(Key::Word, w).expect("word")),
            at(ms),
        )
        .expect("the agent mark");
        assert_eq!(log.state().phase.word, w, "the fold's phase reads {w}");
    }

    let mut tr = TurnTracker::new(run);
    tr.observe(&mut log, Verdict::Busy, None, at(100));
    tr.observe(
        &mut log,
        Verdict::Prompt(BoxWord::from_detail("bash")),
        None,
        at(200),
    );
    let standing = need_words(&log);
    assert_eq!(standing.len(), 1, "PRECONDITION: the box's need stands");
    assert!(standing[0].starts_with("approval:"), "{standing:?}");
    tr.observe(&mut log, Verdict::Stopped, None, at(300));
    tr.observe(&mut log, Verdict::Walled, None, at(400));
    assert_eq!(
        need_words(&log),
        standing,
        "neither word closes the open box nor opens a wall step"
    );
    tr.observe(&mut log, Verdict::Busy, None, at(500));
    assert!(
        need_words(&log).is_empty(),
        "CONTROL: a word that ends the box does end it"
    );
}
