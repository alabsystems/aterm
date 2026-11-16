// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// A NOTE IS WORDS AND A NUMBER, ITS EMPTY FIELDS LEFT OUT, AND A NOTE TOO LONG CUTS ITS
/// CLASSES: the word is `c=…,m=…,n=…`; a note with nothing to say is empty; every cause (175
/// bytes with `c=`) fits whole, and so does every class (222 bytes with `m=`) beside one cause;
/// every cause and every class at once is 402 bytes, so the classes become `m=many` (186 bytes),
/// and with the number at its largest too the note is 195 bytes, the longest there is, within
/// [`NOTE_MAX`]. What a runner reads back is admitted only as that alphabet, at most
/// [`NOTE_MAX`] bytes.
/// NEGATIVE CONTROLS: a word that never cuts gives the 402-byte whole where
/// `c=<every cause>,m=many,n=2` is wanted; a `note_word` that returns the value unchecked admits
/// every refused value below, the first of them the empty one (`Some("")`).
#[test]
fn a_note_is_plain_words_and_cuts_its_classes() {
    const CAUSES: &str = "start+applied+recovered+local+activation+input-repair+input-refresh\
                          +identity+routes+clock+recheck+retry-failed+retry-unfinished\
                          +retry-identity+retry-wave+first+timer+network";
    const CLASSES: &str = "git-dir+admin-dir+config+config-worktree+head+index+packed-refs\
                           +shallow+pointer+refs-dir+refs-heads-dir+current-ref+objects-dir\
                           +objects-fanout+alternates+gitmodules+root+shape+added+removed+source\
                           +first+resumed+admission";
    assert_eq!((CAUSES.len(), CLASSES.len()), (173, 220));
    assert_eq!(StartNote::default().word(), "");
    let note = StartNote {
        causes: Causes::RECHECK.with(Causes::RETRY_FAILED),
        moved: MarkMoved::GIT_DIR.with(MarkMoved::INDEX),
        moves: 2,
    };
    assert_eq!(note.word(), "c=recheck+retry-failed,m=git-dir+index,n=2");
    let causes = StartNote {
        causes: Causes::ALL,
        ..StartNote::default()
    };
    assert_eq!(causes.word(), format!("c={CAUSES}"));
    let classes = StartNote {
        causes: Causes::TIMER,
        moved: MarkMoved::ALL,
        moves: 1,
    };
    assert_eq!(classes.word(), format!("c=timer,m={CLASSES},n=1"));
    let every = StartNote {
        moved: MarkMoved::ALL,
        moves: 2,
        ..causes
    };
    assert_eq!(every.rendered(CAUSES, CLASSES).len(), 402);
    assert_eq!(every.word(), format!("c={CAUSES},m=many,n=2"));
    let full = StartNote {
        causes: Causes::ALL,
        moved: MarkMoved::ALL,
        moves: u32::MAX,
    };
    assert_eq!(full.word(), format!("c={CAUSES},m=many,n=4294967295"));
    assert_eq!(full.word().len(), 195, "the longest note there is");
    for note in [causes, classes, every, full] {
        assert!(note.word().len() <= NOTE_MAX);
        assert_eq!(note_word(OsStr::new(&note.word())), Some(note.word()));
    }

    for refused in [
        "",
        "c=x y",
        "c=x [y]",
        "c=X",
        "c=x\n",
        "c=caf\u{e9}",
        &"a".repeat(NOTE_MAX + 1),
    ] {
        assert_eq!(note_word(OsStr::new(refused)), None, "{refused:?}");
    }
    assert_eq!(
        note_word(OsStr::new(&"a".repeat(NOTE_MAX))),
        Some("a".repeat(NOTE_MAX))
    );
}

/// A RUNNER COMMAND CARRIES ITS START'S NOTE ONLY WHILE THAT START IS BEING MADE: built inside
/// [`noting`] it has exactly [`START_NOTE_ENV`] set to the note's word; built before, after, or
/// under a note with nothing to say, it sets no variable at all.
/// NEGATIVE CONTROL: a `noting` that does not restore the thread's note leaves the nested empty
/// note in place, so the command the outer start then builds carries no variable (`[]`).
#[test]
fn a_runner_built_while_noting_carries_the_note_and_no_other() {
    let exe = std::path::Path::new("/Applications/aterm.app/Contents/MacOS/aterm");
    let envs = |command: &aterm_uds::spawnfd::Command| -> Vec<(String, Option<String>)> {
        command
            .get_envs()
            .map(|(key, value)| {
                (
                    key.to_string_lossy().into_owned(),
                    value.map(|value| value.to_string_lossy().into_owned()),
                )
            })
            .collect()
    };
    let build = || {
        super::super::runner(
            exe,
            super::super::WINDOW,
            super::super::RepositoryWork::Repair,
        )
    };
    assert_eq!(envs(&build()), []);
    let note = StartNote {
        causes: Causes::TIMER,
        moved: MarkMoved::HEAD,
        moves: 1,
    };
    let inside = noting(&note, || {
        let nested = noting(&StartNote::default(), build);
        assert_eq!(envs(&nested), [], "a note with nothing to say sets nothing");
        build()
    });
    assert_eq!(
        envs(&inside),
        [(
            START_NOTE_ENV.to_owned(),
            Some("c=timer,m=head,n=1".to_owned())
        )]
    );
    assert_eq!(envs(&build()), [], "the note ended with its start");
    let requested = noting(&note, || {
        super::super::runner_requested(
            exe,
            super::super::SUBSCRIBER,
            super::super::RepositoryRequest::Repair,
        )
    });
    assert_eq!(envs(&requested).len(), 1);
}
