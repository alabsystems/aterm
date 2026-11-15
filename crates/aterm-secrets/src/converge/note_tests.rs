// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// A NOTE IS WORDS AND NUMBERS, ITS EMPTY FIELDS LEFT OUT, AND IT CUTS ITS CLASSES FIRST, THEN
/// ITS CAUSES: the word is `c=…,h=…,hc=…,w=…,m=…,n=…`; a note with nothing to say is empty;
/// every cause (186 bytes with `c=`) fits whole with small numbers, and so does every class
/// (222 bytes with `m=`) beside one cause; every cause and every class at once is 431 bytes, so
/// the classes become `m=many` (215 bytes); with every number at its largest that is still 266,
/// so the causes become `c=many` too, and the numbers stay (86 bytes). What a runner reads back
/// is admitted only as that alphabet, at most [`NOTE_MAX`] bytes.
/// NEGATIVE CONTROLS: a word that cuts the causes first is still 251 bytes with every class, so
/// it cuts both: `c=many,h=3,hc=40,w=15000,m=many,n=2` where `c=<every cause>,…,m=many,n=2` is
/// wanted; a `note_word` that returns the value unchecked admits every refused value below, the
/// first of them the empty one (`Some("")`).
#[test]
fn a_note_is_plain_words_and_cuts_its_classes_then_its_causes() {
    const CAUSES: &str = "start+applied+recovered+local+activation+input-repair+input-refresh\
                          +identity+routes+clock+recheck+retry-failed+retry-unfinished+retry-held\
                          +retry-identity+retry-wave+first+timer+network";
    const CLASSES: &str = "git-dir+admin-dir+config+config-worktree+head+index+packed-refs\
                           +shallow+pointer+refs-dir+refs-heads-dir+current-ref+objects-dir\
                           +objects-fanout+alternates+gitmodules+root+shape+added+removed+source\
                           +first+resumed+admission";
    assert_eq!((CAUSES.len(), CLASSES.len()), (184, 220));
    assert_eq!(StartNote::default().word(), "");
    let note = StartNote {
        causes: Causes::RECHECK.with(Causes::RETRY_HELD),
        held: 11,
        waited_ms: 52_000,
        moved: MarkMoved::GIT_DIR.with(MarkMoved::INDEX),
        moves: 2,
        ..StartNote::default()
    };
    assert_eq!(
        note.word(),
        "c=recheck+retry-held,h=11,w=52000,m=git-dir+index,n=2"
    );
    let causes = StartNote {
        causes: Causes::ALL,
        held: 3,
        held_cpu_ms: 40,
        waited_ms: 15_000,
        ..StartNote::default()
    };
    assert_eq!(causes.word(), format!("c={CAUSES},h=3,hc=40,w=15000"));
    let classes = StartNote {
        causes: Causes::TIMER,
        moved: MarkMoved::ALL,
        moves: 1,
        ..StartNote::default()
    };
    assert_eq!(classes.word(), format!("c=timer,m={CLASSES},n=1"));
    let every = StartNote {
        moved: MarkMoved::ALL,
        moves: 2,
        ..causes
    };
    assert_eq!(every.rendered(CAUSES, CLASSES).len(), 431);
    assert_eq!(
        every.word(),
        format!("c={CAUSES},h=3,hc=40,w=15000,m=many,n=2")
    );
    let full = StartNote {
        causes: Causes::ALL,
        held: u32::MAX,
        held_cpu_ms: u64::MAX,
        waited_ms: u64::MAX,
        moved: MarkMoved::ALL,
        moves: u32::MAX,
    };
    assert_eq!(full.rendered(CAUSES, "many").len(), 266);
    assert_eq!(
        full.word(),
        "c=many,h=4294967295,hc=18446744073709551615,w=18446744073709551615,m=many,n=4294967295"
    );
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
        held: 2,
        held_cpu_ms: 9,
        waited_ms: 10_000,
        ..StartNote::default()
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
            Some("c=timer,h=2,hc=9,w=10000".to_owned())
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

/// A REAP READS A RUNNER AS `Last::of` DOES, AND GIVES THE CPU ITS MEASURED WAIT FOUND: for a
/// runner that exited 0, 1, 2, 75 or 76 or was killed, [`reap`] gives what [`Last::of`] gives a
/// twin that ended the same way (75 held, 76 unfinished, 0 ended, anything else failed) and
/// takes the runner; where this target measures a wait (64-bit Darwin and Linux) it gives the
/// runner's CPU with it, and elsewhere none. A runner that still runs reads as running, with no
/// CPU, and stays in custody. Each runner has exited, unreaped, before it is read
/// ([`Child::wait_exited`]), so nothing here waits on time.
/// NEGATIVE CONTROL: a reap that maps its measured wait's status by a copy of its own, one that
/// reads 76 as failed, gives `Failed` where [`Last::of`] gives `Unfinished`.
#[cfg(any(target_vendor = "apple", target_os = "linux"))]
#[test]
fn a_reap_reads_a_runner_as_last_of_does_with_the_cpu_its_measured_wait_found() {
    let sh = aterm_uds::deadline::stand_in(std::path::Path::new("/bin/sh"));
    let ended = |script: &str| {
        let mut child = aterm_uds::spawnfd::command(&sh)
            .args(["-c", script])
            .spawn()
            .unwrap();
        child.wait_exited().unwrap();
        Some(child)
    };
    let measured = cfg!(target_pointer_width = "64");
    for (script, last) in [
        ("exit 0", Last::Ended),
        ("exit 1", Last::Failed),
        ("exit 2", Last::Failed),
        ("exit 75", Last::Held),
        ("exit 76", Last::Unfinished),
        ("kill -9 $$", Last::Failed),
    ] {
        let mut twin = ended(script);
        assert_eq!(Last::of(&mut twin), last, "{script}");
        let mut runner = ended(script);
        let (read, cpu) = reap(&mut runner);
        assert_eq!(read, last, "{script}: as Last::of reads its twin");
        assert_eq!(cpu.is_some(), measured, "{script}: {cpu:?}");
        assert!(runner.is_none(), "{script}: an ended runner is taken");
    }

    let mut running = Some(
        aterm_uds::spawnfd::command(&sh)
            .args(["-c", "read line; exit 0"])
            .stdin(aterm_uds::spawnfd::Stdio::piped())
            .spawn()
            .unwrap(),
    );
    assert_eq!(reap(&mut running), (Last::Running, None));
    let child = running.as_mut().expect("a running runner stays in custody");
    drop(child.stdin.take());
    child.wait_exited().unwrap();
    let (read, cpu) = reap(&mut running);
    assert_eq!((read, cpu.is_some()), (Last::Ended, measured));
    assert_eq!(reap(&mut None), (Last::None, None));
}
