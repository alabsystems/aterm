// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

/// v5's capture case: a Land reply with a need, a local commit, an unticked item, a part done
/// and a defense that leaves a PR open (§14.4 of the autopilot design).
const CAPTURE: &str = "I committed the parser locally and kept notes in wip-notes.txt.\n\
                       - [ ] update the changelog\n\
                       The migration is 80% done.\n\
                       Nothing left: the follow-up PR for the docs is still open.\n\
                       NEEDS YOU: rotate the deploy key";

fn saids(found: &[Found]) -> Vec<&str> {
    found.iter().map(|f| f.said.as_str()).collect()
}

fn matched(f: &Found) -> &str {
    &f.said[f.matched.clone()]
}

/// Every find on each form a reader is given of `reply` (the reply, its defense's body and the
/// reply without its needs), read laxly, strictly and as Land reads it at a yield.
fn finds(reply: &str) -> Vec<Found> {
    let land = Some(&LAND_FAMILIES[..]);
    let mut all = open_work_found(reply, None, true);
    all.extend(open_work_found(reply, land, true));
    all.extend(open_work_found(&needs::defense_body(reply), land, true));
    all.extend(open_work_found(&needs::without_needs(reply), land, false));
    all.extend(land_open_found(Some(reply)));
    all
}

/// Whether a phrase breaks what it owes its reader: some text, at most [`SAID_CAP`] bytes, no
/// `*`, tick, newline or code span placeholder, and a match on its characters' edges.
fn broken(f: &Found) -> bool {
    let whole = f.matched.end <= f.said.len()
        && f.said.is_char_boundary(f.matched.start)
        && f.said.is_char_boundary(f.matched.end);
    f.said.trim().is_empty()
        || f.said.len() > SAID_CAP
        || f.matched.is_empty()
        || !whole
        || f.said.contains(['*', '`', '\n'])
        || f.said.split_whitespace().any(|w| w == CODE)
}

// The readers' words are `open_work_found`'s, each once (`once`), so no test of this file can
// tell them apart; the labelled corpus tests hold the words, and
// `the_open_work_readers_over_the_corpora` holds them to another tree's.
#[test]
fn every_phrase_keeps_its_contract_on_every_corpus_row() {
    let rows = super::corpora::corpus_rows();
    let rows = &rows;
    let read: Vec<(Vec<String>, usize)> = std::thread::scope(|scope| {
        let readers: Vec<_> = (0..4)
            .map(|first| {
                scope.spawn(move || {
                    let mut broke = Vec::new();
                    let mut n = 0;
                    for (corpus, row, reply) in rows.iter().skip(first).step_by(4) {
                        let found = finds(reply);
                        n += found.len();
                        broke.extend(
                            found
                                .iter()
                                .filter(|f| broken(f))
                                .map(|f| format!("{corpus}#{row}: {f:?}")),
                        );
                    }
                    (broke, n)
                })
            })
            .collect();
        readers
            .into_iter()
            .map(|reader| reader.join().expect("a reader of the rows"))
            .collect()
    });
    let finds: usize = read.iter().map(|(_, n)| n).sum();
    let broke: Vec<&String> = read.iter().flat_map(|(b, _)| b).collect();
    assert!(
        rows.len() > 600 && finds > 600,
        "{} rows, {finds} finds",
        rows.len()
    );
    assert!(
        broke.is_empty(),
        "{}:\n{}",
        broke.len(),
        broke
            .iter()
            .map(|b| b.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    );
}

#[test]
fn the_open_phrase_is_the_sentence_that_held_it() {
    let found = land_open_found(Some(CAPTURE));
    assert_eq!(
        saids(&found),
        [
            "I committed the parser locally and kept notes in wip-notes.txt.",
            "to do: update the changelog",
            "The migration is 80% done.",
            "The follow-up PR for the docs is still open.",
        ]
    );
    assert_eq!(
        found.iter().map(matched).collect::<Vec<_>>(),
        ["locally", "to do", "80%", "still open"]
    );
    // Each phrase beside the word v5's glass named for it.
    assert_eq!(
        found.iter().map(|f| f.word).collect::<Vec<_>>(),
        ["local", UNCHECKED, "a part", "still open"]
    );
}

#[test]
fn a_cut_phrase_keeps_its_match_whole() {
    // 300 characters, the match at character 250.
    let lead: String = format!("The parser{}", " reads a row".repeat(30))
        .chars()
        .take(249)
        .collect();
    let sentence = format!("{lead} still open{} and rows.", " and a row".repeat(3));
    let at = sentence.find("still open").expect("the match");
    assert_eq!(
        (sentence[..at].chars().count(), sentence.chars().count()),
        (250, 300)
    );
    let found = land_open_found(Some(&format!("Nothing left: {sentence}")));
    let [f] = &found[..] else {
        panic!("one find: {found:?}")
    };
    assert_eq!(f.word, "still open");
    assert_eq!(matched(f), "still open");
    assert!(
        f.said.len() <= SAID_CAP,
        "{} bytes: {:?}",
        f.said.len(),
        f.said
    );
    assert!(
        f.said.starts_with('…') && f.said.ends_with("and rows."),
        "{:?}",
        f.said
    );
    // A match in the middle is cut on both sides, at words' edges.
    let middle = format!(
        "Nothing left: the parser{} still open{} and rows.",
        " reads a row".repeat(12),
        " and a row".repeat(14)
    );
    let found = land_open_found(Some(&middle));
    let [f] = &found[..] else {
        panic!("one find: {found:?}")
    };
    assert_eq!(matched(f), "still open");
    let inner = f.said.strip_prefix('…').and_then(|s| s.strip_suffix('…'));
    assert!(
        f.said.len() <= SAID_CAP && inner.is_some_and(|s| middle.contains(&format!(" {s} "))),
        "{} bytes: {:?}",
        f.said.len(),
        f.said
    );
    // A match longer than the room keeps its start and its whole words.
    let aside = "Blockers: none (3 open pull requests that belong to the docs team, waiting on \
                 their review and their merge queue, which is slow this week)";
    let long = aside.find("pull").expect("the match")..aside.len() - 1;
    let (cut, at) = cut_around(aside, long.clone(), 72);
    let kept = &cut[at.clone()];
    assert!(
        cut.len() <= 72
            && cut.starts_with('…')
            && cut.ends_with('…')
            && aside[long.clone()].starts_with(kept)
            && aside[long.start + kept.len()..].starts_with(' '),
        "{cut:?} keeps {kept:?}"
    );
    // A count beside none is its count, which a cut keeps whole.
    let found = land_open_found(Some(&format!("Nothing left: all landed.\n\n{aside}")));
    let count = found
        .iter()
        .find(|f| f.word == COUNT_BESIDE_NONE)
        .unwrap_or_else(|| panic!("a count in {found:?}"));
    let (cut, at) = cut_around(&count.said, count.matched.clone(), 72);
    assert_eq!(&cut[at], "3 open", "{cut:?}");
    // A short sentence is never cut.
    let short = land_open_found(Some("Nothing left: the docs PR is still open."));
    assert_eq!(saids(&short), ["The docs PR is still open."]);
}

#[test]
fn the_phrase_is_the_agents_own_text() {
    for (reply, word, said, match_said) in [
        // Markdown's emphasis and a contraction the reader spells out.
        (
            "Nothing left: **Still open:** the docs PR isn't merged yet.",
            "still open",
            "Still open: the docs PR isn't merged yet.",
            "Still open",
        ),
        // A head over items quotes its first item that is not a nil one, without its ticks.
        (
            "Nothing left: the parser is done.\n\nStill to do:\n- none\n- bump the version in \
             `Cargo.toml`.\n- run the tests",
            "still to do",
            "Still to do: bump the version in Cargo.toml.",
            "Still to do",
        ),
        // A ratio the reader reads as `3 of 5`.
        (
            "Nothing left: tests pass 3/5.",
            "of",
            "Tests pass 3/5.",
            "3/5",
        ),
        // A sentence after a semicolon, with a code span.
        (
            "Nothing left: the docs are merged; the `release` job is skipped.",
            "skipped",
            "the release job is skipped.",
            "skipped",
        ),
        // A match inside a code span is the word the span holds.
        (
            "Nothing left: the tree is clean, but `git log origin/main..HEAD` shows 2 commits.",
            "origin",
            "The tree is clean, but git log origin/main..HEAD shows 2 commits.",
            "origin",
        ),
        // An aside in brackets.
        (
            "Nothing left: everything is on main (except the docs PR, which is still open).",
            "still open",
            "Everything is on main (except the docs PR, which is still open).",
            "still open",
        ),
        // A line the next one carries on.
        (
            "Nothing left: the docs PR\nis still open.",
            "still open",
            "The docs PR is still open.",
            "still open",
        ),
        // A question names its mark.
        (
            "Nothing left: is the deploy key rotated?",
            QUESTION,
            "Is the deploy key rotated?",
            "?",
        ),
        // A quote mark, a curly apostrophe and a box on the line.
        (
            "Nothing left: done.\n> - [ ] the docs PR hasn’t landed",
            UNCHECKED,
            "to do: the docs PR hasn’t landed",
            "to do",
        ),
        // A word a code block reads is quoted by the line that held it, not the block's first.
        (
            "```\n$ targo --unverified verify\nok fmt\nok tippy\nskip  census (aterm-census not \
             built)\nok test\n```\nNothing left: all of it is on origin/main.",
            "skipped",
            "skip census (aterm-census not built)",
            "skip census (aterm-census not built)",
        ),
        (
            "Nothing left: all landed on origin/main.\n\n```\n$ git status --short\n?? \
             wip-notes.txt\n```",
            "untracked",
            "?? wip-notes.txt",
            "?? wip-notes.txt",
        ),
        // A block under a bare head is its item, quoted by its first line.
        (
            "Nothing left: all on main.\n\nStill to do:\n```\ncd ~/src/proj\ngit push origin \
             main\n```",
            "still to do",
            "Still to do: cd ~/src/proj",
            "Still to do",
        ),
        // Where the agent wrote nothing, nothing is quoted: never the code span's placeholder.
        (
            "Nothing left: done.\n\nStill to do:\n```\n```",
            "still to do",
            "Still to do",
            "Still to do",
        ),
        (
            "Nothing left: done.\n- [ ] ```",
            UNCHECKED,
            "to do",
            "to do",
        ),
        // A count beside none is the count and the word that places it.
        (
            "Nothing left: all landed.\n\nBlockers: none (3 open pull requests that wait on the \
             docs team)",
            COUNT_BESIDE_NONE,
            "Blockers: none (3 open pull requests that wait on the docs team)",
            "3 open",
        ),
        // The marks the reader dropped stay out: underscores, a box mid-line, the text after
        // a match that ends the words read, and a table's rule.
        (
            "Nothing left: the docs PR is _still open_.",
            "still open",
            "The docs PR is still open.",
            "still open",
        ),
        (
            "Nothing left: the check did not _run_.",
            "not run",
            "The check did not run.",
            "not run",
        ),
        (
            "Nothing left: done.\nTests: [ ] lint",
            UNCHECKED,
            "to do: Tests: lint",
            "to do",
        ),
        (
            "Nothing left: the docs PR is still open `per the notes",
            "still open",
            "The docs PR is still open per the notes",
            "still open",
        ),
        (
            "Nothing left: all landed.\n\n| Check | Result |\n|---|---|\n| `cargo test` | passed \
             |\n| `lint` | still open |",
            "still open",
            "cargo test: passed lint: still open",
            "still open",
        ),
    ] {
        let found = land_open_found(Some(reply));
        let f = found
            .iter()
            .find(|f| f.word == word)
            .unwrap_or_else(|| panic!("{word:?} in {reply:?}: {found:?}"));
        assert_eq!(
            (f.said.as_str(), matched(f)),
            (said, match_said),
            "{reply:?}"
        );
    }
}
