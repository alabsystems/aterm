// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn exec(at_ms: u64, name: &str) -> Exec {
    Exec {
        pid: 7,
        pgid: 7,
        ..test_exec(at_ms, name)
    }
}

/// A block's window: from C less the lead (never before the last block's end) through D plus the
/// lag, cut before the next block's C. NEGATIVE CONTROL (scratch): a lag that ignores the next C
/// fails the last assertion.
#[test]
fn a_window_runs_from_c_less_the_lead_after_the_last_block_through_d_plus_the_lag() {
    assert_eq!(
        Window::of(None, Some(2), None, None),
        None,
        "no C: nothing counts"
    );
    let w = Window::of(Some(10_000), Some(12_000), None, None).expect("stamped");
    assert_eq!((w.from, w.to), (10_000 - LEAD_MS, Some(12_000 + LAG_MS)));
    let w = Window::of(Some(10_000), Some(9_000), Some(9_799), None).expect("stamped");
    assert_eq!(
        (w.from, w.to),
        (9_800, Some(10_000 + LAG_MS)),
        "the last block's window bounds it; a D stamped before C never ends it first"
    );
    let w = Window::of(Some(10_000), None, Some(10_500), None).expect("stamped");
    assert_eq!(
        w.from, 10_000,
        "an end stamped after C never moves it past C"
    );
    let w = Window::of(Some(10_000), None, None, None).expect("stamped");
    assert!(w.holds(10_000 - LEAD_MS) && w.holds(u64::MAX) && !w.holds(10_000 - LEAD_MS - 1));
    let w = Window::of(Some(10_000), Some(12_000), None, Some(12_002)).expect("stamped");
    assert_eq!(w.to, Some(12_001), "the next block's C cuts the lag");
}

/// AN EXEC COUNTS FOR ONE BLOCK AT MOST: two blocks typed ahead (block 1's C two ms after block
/// 0's D, its exec just after), and blocks typed far apart, with and without a D: every ms
/// either block's window holds, the other's does not. NEGATIVE CONTROL (scratch): a window
/// whose lag ignores the next block's C (the window before H1) holds block 1's exec in both.
#[test]
fn consecutive_windows_never_overlap() {
    const T: u64 = 1_760_000_000_000;
    let block = |id: u64, a: u64, c: Option<u64>, d: Option<u64>| {
        let mut b = aterm_types::OutputBlock::new(id, 0, 0);
        b.prompt_time_ms = Some(a);
        b.command_exec_start_time_ms = c;
        b.command_end_time_ms = d;
        b
    };
    for (first, second) in [
        (
            block(0, T, Some(T + 10), Some(T + 20)),
            block(1, T + 21, Some(T + 22), Some(T + 40)),
        ),
        (
            block(0, T, Some(T + 10), Some(T + 20)),
            block(1, T + 20, Some(T + 20), None),
        ),
        (
            block(0, T, Some(T + 10), None),
            block(1, T + 30, Some(T + 31), None),
        ),
        (
            block(0, T, Some(T + 10), Some(T + 5_000)),
            block(1, T + 9_000, Some(T + 9_500), None),
        ),
        (block(0, T, None, None), block(1, T + 3, Some(T + 4), None)),
    ] {
        let w0 = Window::of_block(
            &first,
            None,
            second.prompt_time_ms,
            second.command_exec_start_time_ms,
        );
        let w1 = Window::of_block(&second, Some(&first), None, None).expect("stamped");
        for at in T..T + 10_000 {
            assert!(
                !(w0.is_some_and(|w| w.holds(at)) && w1.holds(at)),
                "{at} in both: {w0:?} {w1:?}"
            );
        }
        if let Some(c) = second.command_exec_start_time_ms {
            assert!(w1.holds(c), "block 1 counts its own C: {w1:?}");
            assert!(w1.holds(c + 2), "and its exec: {w1:?}");
        }
    }
}

/// NEGATIVE CONTROL (scratch): dropping the newest exec when full fails the oldest's assertion.
#[test]
fn ran_keeps_the_newest_execs_and_reads_them_by_window() {
    let ran = Ran::default();
    ran.note(Exec {
        names: ExecNames::default(),
        ..exec(5, "x")
    });
    for at in 0..(RAN_MAX as u64 + 2) {
        ran.note(exec(100 + at, "sleep"));
    }
    let all = ran.within(Some(Window { from: 0, to: None }));
    assert_eq!(
        all.len(),
        RAN_MAX,
        "at most RAN_MAX, the nameless one never kept"
    );
    assert_eq!(all[0].at_ms, 102, "the oldest went first");
    let some = ran.within(Some(Window {
        from: 110,
        to: Some(111),
    }));
    assert_eq!(some.iter().map(|e| e.at_ms).collect::<Vec<_>>(), [110, 111]);
    assert!(ran.within(None).is_empty(), "no window: nothing");
}

/// An exec placed where the shell's output stood, as a job's, its shell group's, or by its
/// stamp alone ([`Spot`]).
const fn spot(filled: usize, queued: Option<usize>, job: bool) -> Spot {
    Spot {
        filled,
        queued,
        job,
    }
}

/// The parse of one batch of `len` bytes ending at `end`: it stops at each cut `ran` holds for
/// it and reads `running(at)` there, then reads the batch's end, as the parse stage does.
fn parse(ran: &Ran, end: u64, len: usize, running: impl Fn(usize) -> Option<u64>, now: u64) {
    let mut places = Places::default();
    ran.places_in(end, len, &mut places);
    let mut off = 0;
    while off < len {
        let to = places.next_cut(off).map_or(len, |cut| cut.min(len));
        places.parsed(to, running(to));
        off = to;
    }
    ran.parsed_at(end, len, &places, now);
}

/// AN EXEC IS PLACED WHERE THE SHELL'S OUTPUT STOOD WHEN IT WAS HEARD: it waits until the parse
/// stage has parsed the bytes the gather held and the PTY queued then (so the C the shell wrote
/// before it, and no byte after it, is read first), and the block running exactly there places
/// it: a job's in that block's job, even when the same batch goes on to the block's D and the
/// next command's C (typed ahead), its shell group's as the shell's, one with no queue read by
/// its stamp alone; one heard where the parse stage already is counts at once, placed by the
/// block running there. NEGATIVE CONTROLS (scratch): `hear` stamping at once fails the first
/// assertion; placing every exec by its batch's end (no cut) gives `make` block 8, failing the
/// second.
#[test]
fn an_exec_is_placed_where_the_shells_output_stood_when_it_was_heard() {
    let ran = Ran::default();
    let every = || -> Vec<(u64, Option<String>, During)> {
        ran.within(Some(Window { from: 0, to: None }))
            .into_iter()
            .map(|e| {
                (
                    e.at_ms,
                    e.names.image.as_deref().map(String::from),
                    e.during,
                )
            })
            .collect()
    };
    let named = |at: u64, name: &str, during: During| (at, Some(name.to_owned()), during);
    ran.hear_at(exec(0, "make"), spot(10, Some(5), true), 50);
    assert_eq!(every(), [], "held while the bytes before it are unparsed");
    let end = ran.handed_on(12);
    parse(&ran, end, 12, |_| Some(7), 55);
    assert_eq!(
        every(),
        [],
        "the PTY held bytes no batch had read: it waits"
    );
    let end = ran.handed_on(8);
    assert_eq!(end, 20);
    parse(&ran, end, 8, |at| Some(if at <= 3 { 7 } else { 8 }), 60);
    assert_eq!(
        every(),
        [named(60, "make", During::Job(Some(7)))],
        "placed in the block running where it was heard, not the block the batch ends in"
    );
    ran.hear_at(exec(0, "cat"), spot(0, Some(0), false), 70);
    assert_eq!(
        every()[1],
        named(70, "cat", During::Shell(Some(8))),
        "nothing unread: placed at once, by the block the parse stage stopped in"
    );
    ran.hear_at(exec(0, "ls"), spot(0, None, true), 75);
    assert_eq!(every()[2], named(75, "ls", During::Window), "no queue read");
    ran.hear_at(exec(0, "date"), spot(4, Some(0), true), 80);
    let end = ran.handed_on(4);
    parse(&ran, end, 4, |_| None, 85);
    assert_eq!(
        every()[3],
        named(85, "date", During::Job(None)),
        "heard after the block's D: no block's"
    );
}

/// A JOB'S EXEC COUNTS FOR ITS OWN BLOCK ALONE, and stands in for it; one of the shell's own
/// group counts by its stamp where the shell runs no jobs (pwsh), standing in only in the block
/// it was placed in, and elsewhere counts only there, never standing in; a stamp alone counts in
/// the window and never stands in. NEGATIVE CONTROL (scratch): a job's exec counted by its stamp
/// fails the first assertion.
#[test]
fn an_exec_counts_by_its_place_and_only_its_jobs_stand_in() {
    let w = Some(Window {
        from: 100,
        to: Some(200),
    });
    let at = |at_ms: u64, during: During| Exec {
        during,
        ..exec(at_ms, "x")
    };
    assert_eq!(
        at(150, During::Job(Some(4))).counts(5, w, true),
        (false, false)
    );
    assert_eq!(
        at(150, During::Job(Some(5))).counts(5, w, true),
        (true, true)
    );
    assert_eq!(
        at(900, During::Job(Some(5))).counts(5, w, true),
        (true, true)
    );
    assert_eq!(
        at(150, During::Job(None)).counts(5, w, false),
        (false, false)
    );
    assert_eq!(
        at(150, During::Shell(Some(5))).counts(5, w, false),
        (true, true)
    );
    assert_eq!(
        at(150, During::Shell(None)).counts(5, w, false),
        (true, false)
    );
    assert_eq!(
        at(150, During::Shell(Some(5))).counts(5, w, true),
        (true, false)
    );
    assert_eq!(
        at(150, During::Shell(Some(4))).counts(5, w, true),
        (false, false)
    );
    assert_eq!(
        at(900, During::Shell(Some(5))).counts(5, w, false),
        (false, false)
    );
    assert_eq!(at(150, During::Window).counts(5, w, true), (true, false));
    assert_eq!(at(99, During::Window).counts(5, w, true), (false, false));
}

/// A SHELL'S OWN GROUP STANDS IN BY WHAT THE SHELL IS, NEVER BY WHAT RAN BEFORE (H2 land, the
/// reviewer's rows): in a fresh session of a shell that runs jobs (zsh, bash, fish, and one aterm
/// does not know), an exec of the shell's own group (a command substitution's, a fish handler's)
/// placed in a block counts there and never stands in, before any job is heard as after; in a
/// pwsh session one stands in for its block, and still does after a nested job-control shell ran
/// a job of its own. NEGATIVE CONTROL (scratch): the session-wide flag learned from the first job
/// heard (`jobs |= spot.job`, false until then) fails the first assertion with `date`, and the
/// last with none.
#[test]
fn a_shell_groups_exec_stands_in_by_what_the_shell_is_never_by_what_ran_before() {
    let w = Some(Window { from: 0, to: None });
    let stand_in = |ran: &Ran, block: u64| {
        ran.heard(block, w, None)
            .stand_in(|_| false)
            .map(String::from)
    };
    let counted = |ran: &Ran, block: u64| ran.heard(block, w, None).execs.len();
    // The parse stage stands in `block`; an exec heard with nothing unread is placed there.
    let heard_in = |ran: &Ran, block: u64, name: &str, job: bool| {
        let end = ran.handed_on(1);
        parse(ran, end, 1, |_| Some(block), 10);
        ran.hear_at(exec(0, name), spot(0, Some(0), job), 20);
    };
    let zsh = Ran::default();
    heard_in(&zsh, 3, "date", false);
    assert_eq!(
        stand_in(&zsh, 3),
        None,
        "a fresh session: its group never stands in"
    );
    assert_eq!(counted(&zsh, 3), 1, "it counts in its block");
    heard_in(&zsh, 4, "sleep", true);
    heard_in(&zsh, 5, "date", false);
    assert_eq!(
        stand_in(&zsh, 4).as_deref(),
        Some("sleep"),
        "a job stands in"
    );
    assert_eq!(stand_in(&zsh, 5), None, "and after a job, the same");
    let pwsh = Ran::default();
    pwsh.runs_jobs(false);
    heard_in(&pwsh, 3, "ls", false);
    assert_eq!(
        stand_in(&pwsh, 3).as_deref(),
        Some("ls"),
        "pwsh runs it in its group"
    );
    heard_in(&pwsh, 4, "make", true);
    heard_in(&pwsh, 5, "ls", false);
    assert_eq!(
        stand_in(&pwsh, 5).as_deref(),
        Some("ls"),
        "a nested shell's job changes nothing"
    );
}

/// What confirms and what only steers: a bare word may equal the canonical file's name or
/// macOS's `p_name`, both the kernel's; an exec name read off macOS (a Linux `comm`, which the
/// process sets) is never read, and the paths and `PATH` an exec was given confirm nothing on
/// their own. NEGATIVE CONTROL (scratch): a handed path counted as a name fails the steered
/// assertion.
#[test]
fn an_image_shows_its_file_name_and_only_a_name_the_kernel_keeps_confirms() {
    let both = ExecNames {
        image: Some("whoami".into()),
        exec: Some("id".into()),
        handed: Box::new(["/usr/bin/QZP7R2".into()]),
        env_path: Some("/opt/QZP7R3".into()),
        ..ExecNames::default()
    };
    assert_eq!(both.shown(), Some("whoami"), "the canonical file's name");
    assert_eq!(both.iter().collect::<Vec<_>>(), ["whoami", "id"]);
    let steered = ExecNames {
        handed: Box::new(["/usr/bin/QZP7R1".into()]),
        ..ExecNames::default()
    };
    assert!(steered.is_empty(), "a path it was given confirms nothing");
    let path = ExecNames::of_path(Path::new("/usr/bin/sleep"));
    assert_eq!(path.shown(), Some("sleep"));
    assert!(ExecNames::of_path(Path::new("")).is_empty());
}

/// A FILE OF MANY NAMES STANDS IN BY THE NAME IT WAS EXEC'D BY (H2's live corpus, fish):
/// macOS's developer-tool shim is one file under dozens of hard links (`python3`, `git`,
/// `make`), and `proc_pidpath` names whichever link the kernel holds for it, so after fish's
/// prompt ran `git` a `#!/usr/bin/env python3` script's stand-in read `git`. A linked file's
/// stand-in is the kernel's exec name when it is whole, else none, never another link's name; a
/// file of one name shows its canonical name, never the link it was exec'd through. NEGATIVE
/// CONTROL (scratch): `shown` reading the canonical name whatever the links fails the first
/// assertion with `git`.
#[test]
fn a_file_of_many_names_stands_in_by_the_name_it_was_execd_by() {
    let shim = |exec: &str| ExecNames {
        image: Some("git".into()),
        exec: Some(exec.into()),
        linked: true,
        ..ExecNames::default()
    };
    assert_eq!(
        shim("python3").shown(),
        Some("python3"),
        "the link it was exec'd by"
    );
    assert_eq!(
        shim("qzfifteenbytes1").shown(),
        None,
        "an exec name the kernel may have cut is none, never another link's name"
    );
    assert_eq!(
        ExecNames {
            image: Some("sleep".into()),
            exec: Some("qzlink".into()),
            ..ExecNames::default()
        }
        .shown(),
        Some("sleep"),
        "a file of one name shows its canonical name"
    );
}

/// The real read of a hard-linked image ([`read_names`]): a copy of `cat`, signed ad hoc, run
/// by its second link, reads as a file of several names whose exec name is that link, so it
/// stands in by it. Its names are read, and its folder goes, only once it echoed a line
/// ([`aterm_uds::deadline::cat_echoes`]): a name the kernel shows at `_dyld_start` is no proof
/// macOS finished assessing the copy's first exec. NEGATIVE CONTROL (scratch): `image_file`
/// answering no links fails the first assertion.
#[cfg(target_vendor = "apple")]
#[test]
fn a_hard_linked_image_reads_as_linked_and_shows_the_link_it_ran_by() {
    use aterm_uds::spawnfd::Stdio;
    let mut kept = aterm_uds::deadline::KeptUntilRan::new(
        aterm_tempfile::tempdir().expect("a scratch dir").keep(),
    );
    let first = kept.path().join("qzlinka");
    let second = kept.path().join("qzlinkb");
    aterm_uds::deadline::ad_hoc_copy(Path::new("/bin/cat"), &first);
    std::fs::hard_link(&first, &second).expect("a second link");
    let _ = std::fs::metadata(&first).expect("the first link, looked up last");
    let mut child = aterm_uds::spawnfd::command(&second)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn the second link");
    let fed = aterm_uds::deadline::cat_echoes(&mut child);
    kept.ran();
    let names = read_names(i32::try_from(child.id()).expect("a pid"), &mut |_| false);
    drop(fed);
    aterm_uds::deadline::kill_within(&mut child, aterm_spec::wait::MUST_HAPPEN);
    assert!(names.linked, "a file of two names: {names:?}");
    assert_eq!(names.shown(), Some("qzlinkb"), "{names:?}");
    assert_eq!(names.file, file_id(&first), "one file under both names");
}

/// A `KERN_PROCARGS2` answer as the kernel lays one out: argc, the exec path, its padding,
/// the argument words and the environment.
fn procargs2(exec: &str, argv: &[&str], env: &[&str]) -> Vec<u8> {
    let argc = i32::try_from(argv.len()).expect("argc");
    let mut buf = argc.to_ne_bytes().to_vec();
    buf.extend_from_slice(exec.as_bytes());
    buf.extend_from_slice(&[0; 4]);
    for word in argv.iter().chain(env) {
        buf.extend_from_slice(word.as_bytes());
        buf.push(0);
    }
    buf
}

/// What one exec's `KERN_PROCARGS2` read steers by: its `PATH`, and the absolute paths it was
/// given as its file (the exec path and `argv[0]`), never an argument word after `argv[0]` (a
/// path handed to a program, or to an interpreter by the kernel's `#!` handling, is never the
/// program), a relative word, a path longer than `PATH_MAX` or a `PATH` longer than
/// `ENV_PATH_MAX`. NEGATIVE CONTROL (scratch): reading the first argument words again (as before
/// H1's landing) fails the second assertion with `/tmp/QZP7P4/x`.
#[test]
fn procargs_steer_by_the_path_and_the_paths_an_exec_was_given_as_its_file() {
    const PATH: &str = "PATH=/usr/bin:/bin:/opt/homebrew/bin/";
    let (path, handed) = steers_of_procargs(&procargs2(
        "/u/jsmith/.local/bin/claude",
        &["claude", "--resume", "QZP7P1"],
        &["HOME=/u/jsmith", PATH],
    ));
    assert_eq!(
        handed.iter().map(|h| &**h).collect::<Vec<_>>(),
        ["/u/jsmith/.local/bin/claude"],
        "the path the shell's lookup handed the kernel"
    );
    assert_eq!(path.as_deref(), Some(&PATH[5..]));
    let (_, handed) = steers_of_procargs(&procargs2(
        "/bin/bash",
        &["/bin/sh", "/w/QZP7P3/deploy.sh", "/tmp/QZP7P4/x"],
        &[],
    ));
    assert_eq!(
        handed.iter().map(|h| &**h).collect::<Vec<_>>(),
        ["/bin/bash", "/bin/sh"],
        "the exec path and argv[0], never the script or an argument the interpreter was handed"
    );
    let long = format!("/{}", "a".repeat(PATH_MAX));
    let (path, handed) = steers_of_procargs(&procargs2(
        &long,
        &[&long, &long],
        &[&format!("PATH=/usr/bin:{}", "b".repeat(ENV_PATH_MAX))],
    ));
    assert_eq!(
        (path, handed),
        (None, Box::default()),
        "nothing too long to name a file steers"
    );
    assert_eq!(
        steers_of_procargs(b"\x01"),
        (None, Box::default()),
        "a short answer names nothing"
    );
}

/// PRIV4-2: what an exec's record may have been handed as the `#!` script the kernel handed an
/// interpreter: a word with a `/` among the first [`SCRIPT_WORDS`] after `argv[0]` (after the
/// `#!` line's own words, which macOS splits), never a flag, a word with no `/` (a token, a
/// `-c` command), a word with a control character, a later word, one longer than `PATH_MAX`,
/// or more than `HANDED_MAX`; the steers of the same record keep naming only the exec path and
/// `argv[0]`. A word kept only steers a lookup of the file it names, which confirms a script
/// only where it is that script's file. NEGATIVE CONTROL (scratch): keeping every argument word
/// fails the first assertion, keeping `--token`.
#[test]
fn procargs_hand_an_interpreter_its_script_and_no_other_word() {
    let scripts = |argv: &[&str]| {
        scripts_of_procargs(&procargs2("/bin/sh", argv, &[]))
            .iter()
            .map(|s| s.path.to_string())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        scripts(&["/bin/sh", "./d/QZP7P5.sh", "--token", "QZP7P6"]),
        ["./d/QZP7P5.sh"],
        "the script the kernel handed /bin/sh"
    );
    assert_eq!(
        scripts(&["/usr/bin/env", "-S", "python3", "-u", "/opt/x/QZP7P7"]),
        ["/opt/x/QZP7P7"],
        "after an env line's own words"
    );
    assert_eq!(
        scripts(&["sh", "-c", "sleep 1"]),
        Vec::<String>::new(),
        "a -c command with no path"
    );
    assert_eq!(
        scripts(&["sh", "-c", "cd /tmp\n./QZP7P8.sh"]),
        Vec::<String>::new(),
        "a word with a control character names no script file"
    );
    assert_eq!(
        scripts(&["a", "b", "c", "d", "e", "/opt/x/QZP7P9"]),
        Vec::<String>::new(),
        "past SCRIPT_WORDS"
    );
    assert_eq!(
        scripts(&["a", "/x/1", "/x/2", "/x/3"]),
        ["/x/1", "/x/2"],
        "at most HANDED_MAX"
    );
    let long = format!("/{}", "q".repeat(PATH_MAX));
    assert_eq!(
        scripts(&["a", &long, "--/QZP7P10"]),
        Vec::<String>::new(),
        "too long, and a flag"
    );
    let (_, handed) = steers_of_procargs(&procargs2("/bin/sh", &["/bin/sh", "./d/QZP7P5.sh"], &[]));
    assert_eq!(
        handed.iter().map(|h| &**h).collect::<Vec<_>>(),
        ["/bin/sh"],
        "a script path never steers a program word's lookup"
    );
    let slots = |argv: &[&str]| {
        scripts_of_procargs(&procargs2("/bin/sh", argv, &[]))
            .iter()
            .map(|s| (s.path.to_string(), s.before))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        slots(&["sh", "-n", "./d/QZP7P11.sh", "./d/QZP7P12.sh"]),
        [
            ("./d/QZP7P11.sh".to_owned(), words_digest(["-n"])),
            (
                "./d/QZP7P12.sh".to_owned(),
                words_digest(["-n", "./d/QZP7P11.sh"])
            ),
        ],
        "each path keeps the digest of the words before it, never the words"
    );
}

/// PRIV4-2 (the review's slot): where a `#!` line puts its script in the `argv` of the process
/// that runs it ([`script_slots`]): right after the words its interpreter is handed, which
/// macOS's kernel splits at blanks (`#!/bin/sh -e` hands `-e` then the script) and Linux's hands
/// whole as one word (`-e -x`); for the known `env`, also right after the words that follow
/// its program, as `env` hands them to the program it re-execs (`python3 -u <script>`). A
/// script right after `argv[0]` stands only where its line adds no word, so `sh -n ./x.sh`
/// (`-n` before it) never puts `#!/bin/sh`'s script where the kernel does. NEGATIVE CONTROL
/// (scratch): dropping the `env` slot fails the `env -S` assertion.
#[test]
fn a_shebang_line_puts_its_script_after_its_own_words() {
    let has = |line: &str, env: bool, words: &[&str]| {
        script_slots(line, env).contains(&words_digest(words.iter().copied()))
    };
    assert!(has("/bin/sh", false, &[]), "no word: right after argv[0]");
    assert!(
        !has("/bin/sh", false, &["-n"]),
        "a flag the line never adds"
    );
    assert!(has("/bin/sh -e", false, &["-e"]) && !has("/bin/sh -e", false, &[]));
    assert!(
        has("/bin/bash -e -x", false, &["-e", "-x"]) && has("/bin/bash -e -x", false, &["-e -x"]),
        "split at blanks on macOS, whole on Linux"
    );
    let line = "/usr/bin/env -S python3 -u";
    assert!(
        has(line, true, &["-S", "python3", "-u"]) && has(line, true, &["-u"]),
        "env's own record, and its program's once it re-execs"
    );
    assert!(
        !has(line, false, &["-u"]),
        "an interpreter that is not env re-execs nothing"
    );
    assert!(!has(line, true, &[]));
    assert!(
        script_slots("", false).is_empty(),
        "no interpreter, no slot"
    );
}

/// NEGATIVE CONTROL (scratch): `env_program` taking `-u`'s value for the program fails the last
/// assertion.
#[test]
fn a_shebang_is_the_first_lines_text_after_its_two_bytes() {
    assert_eq!(
        shebang_line(b"#!/usr/bin/env -S python3 -u\nprint(1)\n").as_deref(),
        Some("/usr/bin/env -S python3 -u")
    );
    assert_eq!(shebang_line(b"#!/bin/sh").as_deref(), Some("/bin/sh"));
    assert_eq!(shebang_line(b" #!/bin/sh\n"), None, "the first two bytes");
    assert_eq!(shebang_line(b"\x7fELF"), None);
    assert_eq!(
        env_program(&mut ["-S", "-i", "-u", "QZP7E1", "A=QZP7E2", "python3", "-u"].into_iter()),
        Some("python3"),
        "env's flags, an unset name and an assignment are passed"
    );
}

/// THE REAL FILE SYSTEM, as `execve` finds a file: an executable regular file (through a
/// symlink, with the identity of the file it links to) is a program; a directory, a missing path
/// and a file no one may execute are not; a `#!` script shows its line. NEGATIVE CONTROL
/// (scratch): any path that exists being a program fails the data file's assertion.
#[cfg(unix)]
#[test]
fn the_real_files_find_a_program_by_its_identity_and_nothing_else() {
    use std::os::unix::fs::PermissionsExt;
    let dir = aterm_tempfile::tempdir().expect("a scratch dir");
    let root = dir.path();
    let program = root.join("tool");
    std::fs::write(&program, b"\x7fELF not really").expect("write");
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    let script = root.join("deploy.sh");
    std::fs::write(&script, b"#!/bin/sh\necho hi\n").expect("write");
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    let data = root.join("notes.txt");
    std::fs::write(&data, b"#!/bin/sh\n").expect("write");
    let link = root.join("vi");
    std::os::unix::fs::symlink(&program, &link).expect("link");
    std::fs::create_dir(root.join("QZP7D1")).expect("mkdir");
    let files = RealFiles;
    let id = files.program(&program).expect("a program");
    assert_eq!(files.program(&link), Some(id), "a link is its target");
    assert_eq!(file_id(&link), Some(id));
    assert_eq!(files.shebang(&program), None);
    assert!(files.program(&script).is_some());
    assert_eq!(files.shebang(&script).as_deref(), Some("/bin/sh"));
    assert_eq!(files.program(&data), None, "no execute bit");
    assert_eq!(files.program(&root.join("QZP7D1")), None, "a directory");
    assert_eq!(files.program(&root.join("QZP7D2")), None, "missing");
}

/// PERF3-1, THE RING IS BOUNDED WHATEVER AN EXEC WAS GIVEN: execs named from a record of 1 MiB
/// words (an image and exec name, a handed path and a `PATH` that long), and execs that each
/// showed a distinct `PATH`, leave the ring holding at most a fixed number of bytes per entry
/// plus a few kept `PATH`s; and a read shares the ring's entries, copying no names. NEGATIVE
/// CONTROLS (scratch): stamping an exec's names as read (no bound, as before H1's landing) fails
/// the first bound; keeping every distinct `PATH` fails the second; a read that copies each
/// entry fails the sharing assertion; no cap on the execs waiting for the parse stage fails the
/// last.
#[test]
fn the_ring_holds_a_bounded_entry_whatever_an_exec_was_given() {
    const MIB: usize = 1 << 20;
    let word = format!("/{}", "q".repeat(MIB));
    let (path, handed) = steers_of_procargs(&procargs2(
        &word,
        &[&word, &word],
        &[&format!("PATH={word}")],
    ));
    assert_eq!((path, handed.len()), (None, 0), "a kernel record's");
    let ran = Ran::default();
    for i in 0..RAN_MAX as u64 + 8 {
        ran.note(Exec {
            names: ExecNames {
                image: Some(word[1..].into()),
                exec: Some(word[1..].into()),
                handed: Box::new([word.as_str().into(), "/u/jsmith/bin/QZP7R9".into()]),
                scripts: Box::new([
                    word.as_str().into(),
                    word.as_str().into(),
                    "./QZP7R10".into(),
                ]),
                env_path: Some(Arc::from(format!("{word}:{i}"))),
                file: Some(test_id("x")),
                linked: false,
                any_case: false,
            },
            ..exec(i, "x")
        });
    }
    let per_entry = 2 * NAME_MAX + 2 * HANDED_MAX * PATH_MAX;
    let bound = RAN_MAX * per_entry + PATHS_KEPT * ENV_PATH_MAX;
    assert!(
        ran.held_bytes() <= bound,
        "{} bytes held, bound {bound}",
        ran.held_bytes()
    );
    let paths = Ran::default();
    for i in 0..RAN_MAX as u64 {
        paths.note(Exec {
            names: ExecNames {
                env_path: Some(Arc::from(format!("/u/{i}:{}", "p".repeat(4_000)))),
                ..exec(i, "x").names
            },
            ..exec(i, "x")
        });
    }
    assert!(
        paths.held_bytes() <= RAN_MAX + PATHS_KEPT * ENV_PATH_MAX,
        "{} bytes of PATHs held",
        paths.held_bytes()
    );
    let every = Some(Window { from: 0, to: None });
    let (a, b) = (paths.within(every), paths.within(every));
    assert!(
        a.iter().zip(&b).all(|(a, b)| Arc::ptr_eq(a, b)),
        "a read shares the ring's entries"
    );
    let stalled = Ran::default();
    for i in 0..4 * RAN_MAX as u64 {
        stalled.hear_at(exec(0, "x"), spot(1, Some(0), true), i);
    }
    assert_eq!(
        stalled.waiting(),
        RAN_MAX,
        "a parse stage that never catches up keeps RAN_MAX waiting"
    );
}

/// PERF3-2: an exec's `KERN_PROCARGS2` record is read for its steers only when it is the first
/// heard of its process (the exec the shell's own lookup handed the kernel), never again for
/// the process's own re-execs (`/bin/sh`'s stub, `env`), and not for a process that maps the
/// file the newest read one did while both wait for the same place in the output: that one
/// shares the read one's steers. A new place, or another file, is read again. NEGATIVE
/// CONTROLS (scratch): reading the steers for every exec, sharing them whatever the file or
/// across places, and never sharing them each fail the first assertion; a shared exec left
/// without the read one's steers fails the second.
#[cfg(unix)]
#[test]
fn an_execs_steers_are_read_once_per_process_and_once_per_place_of_one_file() {
    static ASKED: Mutex<Vec<(i32, bool)>> = Mutex::new(Vec::new());
    fn read(pid: i32, steer: Steering<'_>) -> ExecNames {
        let file = FileId {
            dev: 1,
            ino: u64::from(pid % 2 == 0 && pid != 62_008),
        };
        let steered = steer(Some(file));
        ASKED
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push((pid, steered));
        ExecNames {
            image: Some(format!("p{}", file.ino).into()),
            file: Some(file),
            env_path: steered.then(|| format!("/QZP7STEER{pid}").into()),
            ..ExecNames::default()
        }
    }
    const SH: i32 = 62_000;
    let ran = Arc::new(Ran::default());
    let feed = RanFeed::reading(Arc::clone(&ran), SH, read);
    let here = spot(0, Some(0), true);
    feed.exec(62_001, 62_001, here);
    feed.exec(62_001, 62_001, here);
    feed.exec(62_002, 62_001, here);
    feed.exec(SH, SH, here);
    feed.exec(62_004, 62_004, here);
    feed.exec(62_006, 62_006, here);
    ran.handed_on(1);
    feed.exec(62_010, 62_010, here);
    feed.exec(62_008, 62_008, here);
    let asked: Vec<(i32, bool)> = ASKED
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .iter()
        .copied()
        .filter(|(pid, _)| (SH..SH + 20).contains(pid))
        .collect();
    assert_eq!(
        asked,
        [
            (62_001, true),
            (62_001, false),
            (62_002, true),
            (62_004, false),
            (62_006, false),
            (62_010, true),
            (62_008, true),
        ],
        "the first exec of each process, never the shell's, and one per place of one file"
    );
    parse(&ran, 1, 1, |_| None, 7);
    let shown: Vec<(i32, Option<String>)> = ran
        .within(Some(Window { from: 0, to: None }))
        .iter()
        .map(|e| (e.pid, e.names.env_path.as_deref().map(String::from)))
        .filter(|(pid, _)| [62_004, 62_006, 62_010].contains(pid))
        .collect();
    assert_eq!(
        shown,
        [
            (62_004, Some("/QZP7STEER62002".into())),
            (62_006, Some("/QZP7STEER62002".into())),
            (62_010, Some("/QZP7STEER62010".into())),
        ],
        "a shared exec shows the read one's steers"
    );
}

/// A dataless file (a cloud placeholder) is never opened for its `#!` line: reading it would
/// download it. NEGATIVE CONTROL (scratch): ignoring the flag fails the second assertion.
#[test]
fn a_dataless_files_head_is_never_read() {
    assert!(local_flags(0));
    assert!(!local_flags(SF_DATALESS));
    assert!(!local_flags(SF_DATALESS | 0x20));
}

/// A test's file system whose answers can change (a rebuilt file) and which counts every
/// question it is asked.
#[derive(Debug, Default)]
struct Counted {
    table: Mutex<Table>,
    asked: std::sync::atomic::AtomicUsize,
}

impl Counted {
    fn asked(&self) -> usize {
        self.asked.load(std::sync::atomic::Ordering::SeqCst)
    }
}

impl Files for Counted {
    fn program(&self, path: &Path) -> Option<FileId> {
        self.asked.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Files::program(&*self.table.lock().unwrap_or_else(|p| p.into_inner()), path)
    }

    fn shebang(&self, path: &Path) -> Option<Box<str>> {
        self.asked.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Files::shebang(&*self.table.lock().unwrap_or_else(|p| p.into_inner()), path)
    }

    fn directory(&self, path: &Path) -> bool {
        self.asked.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Files::directory(&*self.table.lock().unwrap_or_else(|p| p.into_inner()), path)
    }
}

/// A lookup worker that cannot start must leave file-based confirmation pending without
/// asking the file system on the reader. The same selection is used by production default.
#[test]
fn lookup_with_no_worker_never_queries_the_file_system() {
    let files = Arc::new(Counted {
        table: Mutex::new(Table::default().program("/w/alias", "vim")),
        ..Counted::default()
    });
    let ran = Ran::looked_up(
        Arc::clone(&files) as Arc<dyn Files>,
        None,
        Resolving::for_worker(None, Duration::ZERO),
    );
    let heard = Heard::of(&ran, ran_of(&["vim"]), Some("/w"), Some("/usr/bin"));
    let got = heard.look(Word::Path("./alias"));
    assert_eq!(
        files.asked(),
        0,
        "an unavailable worker cannot move I/O onto the reader"
    );
    assert_eq!(got, Look::Pending);
    assert_eq!(
        crate::session_status::command_detail("vim notes", &heard).as_deref(),
        Some("vim"),
        "kernel-confirmed names still need no file lookup"
    );
}

/// A shell selects the first executable in each PATH, so a later alias of an observed image
/// cannot confirm a word shadowed by an executable whose identity was never observed.
#[test]
fn lookup_honors_the_first_executable_on_each_path() {
    for (files, want) in [
        (
            Table::default()
                .program("/first/sentinel", "unheard")
                .program("/second/sentinel", "vim"),
            Look::Nothing,
        ),
        (
            Table::default()
                .program("/first/sentinel", "vim")
                .program("/second/sentinel", "unheard"),
            Look::Ran,
        ),
        (
            Table::default().program("/second/sentinel", "vim"),
            Look::Ran,
        ),
    ] {
        let ran = Ran::with_files(Box::new(files), None);
        let heard = Heard::of(&ran, ran_of(&["vim"]), Some("/w"), Some("/first:/second"));
        assert_eq!(heard.look(Word::Bare("sentinel")), want);
        // The word's own name only when confirmed; else the block's job's program stands in
        // (H2), never the word.
        let reading = crate::session_status::command_reading("sentinel", &heard);
        assert_eq!(
            reading.as_ref().map(|r| (r.text.as_str(), r.own)),
            Some(if want == Look::Ran {
                ("sentinel", true)
            } else {
                ("vim", false)
            })
        );
    }
}

/// An unsupported PATH directory may shadow any later absolute directory. Only an earlier
/// supported executable or the independently handed exec path may confirm the word.
#[test]
fn lookup_stops_before_an_unsupported_path_directory() {
    for (path, confirmed) in [
        (":/second", false),
        (".:/second", false),
        ("relative:/second", false),
        ("/absent::/second", false),
        ("/absent:.:/second", false),
        ("/absent:relative:/second", false),
        ("/second:", true),
        ("/second:.", true),
        ("/second:relative", true),
        ("/absent:/second:relative", true),
    ] {
        let files = Table::default()
            .program("/second/sentinel", "vim")
            .program("/usr/bin/env", "real-env")
            .script("/w/via-env", "/usr/bin/env sentinel");
        let ran = Ran::with_files(Box::new(files), None);
        let heard = Heard::of(
            &ran,
            handed_scripts(ran_of(&["vim"]), &["./via-env"]),
            Some("/w"),
            Some(path),
        );
        assert_eq!(
            heard.look(Word::Bare("sentinel")),
            if confirmed { Look::Ran } else { Look::Nothing },
            "bare word on {path}"
        );
        assert_eq!(
            heard.look(Word::Path("./via-env")),
            if confirmed { Look::Ran } else { Look::Script },
            "env program on {path}"
        );
        let reading = crate::session_status::command_reading("sentinel", &heard);
        assert_eq!(
            reading.as_ref().map(|r| (r.text.as_str(), r.own)),
            Some(if confirmed {
                ("sentinel", true)
            } else {
                ("vim", false)
            }),
            "detail on {path}: the word only when confirmed, else its job's program standing in"
        );
        let mut observed = ran_of(&["vim"]);
        observed[0].names.handed = Box::new(["/second/sentinel".into()]);
        let handed = Heard::of(&ran, observed, Some("/w"), Some(path));
        assert_eq!(
            handed.look(Word::Bare("sentinel")),
            Look::Ran,
            "an independently handed exec path on {path}"
        );
    }
}

/// A custom executable named env proves no env argument semantics. The known env file and
/// a differently named link to that identity may confirm the program its shebang runs.
#[test]
fn lookup_env_programs_require_the_known_interpreter_identity() {
    let mut got = Vec::new();
    for name in ["sentinel", "legit", "env-link"] {
        let files = Table::default()
            .program("/usr/bin/env", "real-env")
            .program("/unrelated/env", "custom-env")
            .program("/alias/launcher", "real-env")
            .program("/usr/bin/python3", "python3")
            .script("/w/sentinel", "/unrelated/env python3")
            .script("/w/legit", "/usr/bin/env python3")
            .script("/w/env-link", "/alias/launcher python3");
        let word = format!("./{name}");
        let observed = handed_scripts(ran_of(&["python3"]), &[&word]);
        let oracle = files.ran_by_identity(name, &observed, "/w");
        let ran = Ran::with_files(Box::new(files), None);
        let heard = Heard::of(&ran, observed, Some("/w"), Some("/usr/bin"));
        let reading = crate::session_status::command_reading(&word, &heard)
            .map(|reading| (reading.text, reading.own));
        got.push((heard.look(Word::Path(&word)), reading, oracle));
    }
    assert_eq!(
        got,
        [
            (Look::Script, Some(("python3".into(), false)), false),
            (Look::Ran, Some(("legit".into(), true)), true),
            (Look::Ran, Some(("env-link".into(), true)), true),
        ]
    );
}

/// THE FILE SYSTEM IS ASKED ONCE PER WINDOW, ON THE WORKER: `make && ./app` swept again and
/// again while make runs asks nothing more; once the app (rebuilt meanwhile, a new file) runs,
/// the new image in the window has the word looked up again, and it is confirmed. NEGATIVE
/// CONTROLS (scratch): no memo fails the count; a memo that ignores the window's images (the
/// old build's answer kept) never confirms the rebuilt app.
#[test]
fn a_lookup_is_made_once_per_window_and_again_for_a_new_image() {
    let files = Arc::new(Counted {
        table: Mutex::new(
            Table::default()
                .program("/usr/bin/make", "make")
                .program("/w/app", "app-1"),
        ),
        ..Counted::default()
    });
    let worker = Worker::start().expect("a worker thread");
    let ran = Ran::on_worker(
        Arc::clone(&files) as Arc<dyn Files>,
        worker,
        aterm_spec::wait::MUST_HAPPEN,
    );
    let look = |execs: &[&str]| {
        let execs = execs.iter().map(|n| exec(0, n)).collect();
        Heard::of(&ran, execs, Some("/w"), Some("/usr/bin")).look(Word::Path("./app"))
    };
    assert_eq!(look(&["make"]), Look::Nothing, "the app has not run");
    let asked = files.asked();
    assert!(asked > 0, "the first sweep asked");
    for _ in 0..3 {
        assert_eq!(look(&["make"]), Look::Nothing);
    }
    assert_eq!(files.asked(), asked, "the same window asks nothing again");
    let rebuilt = std::mem::take(&mut *files.table.lock().unwrap_or_else(|p| p.into_inner()));
    *files.table.lock().unwrap_or_else(|p| p.into_inner()) = rebuilt.program("/w/app", "app-2");
    assert_eq!(
        look(&["make", "app-2"]),
        Look::Ran,
        "the rebuilt app's image is looked up again"
    );
}

/// A test's file system that answers nothing until it is opened (a mount that stalls).
#[derive(Debug, Default)]
struct Stalled {
    open: Mutex<bool>,
    opened: std::sync::Condvar,
    /// How many questions reached it.
    entered: std::sync::atomic::AtomicUsize,
    table: Table,
}

impl Stalled {
    fn wait_open(&self) {
        self.entered
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let open = self.open.lock().unwrap_or_else(|p| p.into_inner());
        let _open = self
            .opened
            .wait_timeout_while(open, aterm_spec::wait::MUST_HAPPEN, |open| !*open)
            .unwrap_or_else(|p| p.into_inner());
    }

    /// It answers from now on.
    fn open(&self) {
        *self.open.lock().unwrap_or_else(|p| p.into_inner()) = true;
        self.opened.notify_all();
    }
}

impl Files for Stalled {
    fn program(&self, path: &Path) -> Option<FileId> {
        self.wait_open();
        Files::program(&self.table, path)
    }

    fn shebang(&self, path: &Path) -> Option<Box<str>> {
        self.wait_open();
        Files::shebang(&self.table, path)
    }

    fn directory(&self, path: &Path) -> bool {
        self.wait_open();
        Files::directory(&self.table, path)
    }
}

/// THE READER NEVER WAITS ON THE FILE SYSTEM: with a file system that stalls, a reader (the
/// status sweep, on the UI thread, under the session's term lock) gets `Pending` once its wait
/// for the worker ends (here, at once), and so does every read after it; once the file system
/// answers, a read confirms the word. NEGATIVE CONTROL (scratch): the lookup made on the reading
/// thread blocks the reader until the stall ends (the stand-in's one-minute hang detector), and
/// the first assertion fails.
#[test]
fn a_reader_never_waits_on_a_stalled_file_system() {
    let files = Arc::new(Stalled {
        table: Table::default()
            .program("/usr/bin/vi", "vim")
            .program("/usr/bin/vim", "vim"),
        ..Stalled::default()
    });
    let worker = Worker::start().expect("a worker thread");
    let ran = Ran::on_worker(Arc::clone(&files) as Arc<dyn Files>, worker, Duration::ZERO);
    let heard = Heard::of(&ran, vec![exec(0, "vim")], Some("/w"), Some("/usr/bin"));
    assert_eq!(heard.look(Word::Bare("vi")), Look::Pending, "stalled");
    assert_eq!(heard.look(Word::Bare("vi")), Look::Pending, "still");
    *files.open.lock().unwrap_or_else(|p| p.into_inner()) = true;
    files.opened.notify_all();
    aterm_spec::wait::until_within(aterm_spec::wait::MUST_HAPPEN, "the worker's answer", || {
        (heard.look(Word::Bare("vi")) == Look::Ran).then_some(())
    });
}

/// A WORD WHOSE LOOKUP IS NOT ANSWERED GETS NO STAND-IN (H2 land, the reviewer's row): an
/// AUTO_CD path whose block's job ran a zoxide-shaped `chpwd` hook's program reads none while
/// the file system stalls, and none once it answers that the word is a directory, so the hook's
/// program is never written (an amend only moves a detail up, never back to none); a bare word
/// that names no file (an alias) reads none while unanswered and its job's program once the
/// lookup says it names nothing. NEGATIVE CONTROL (scratch): `reading` letting
/// [`Look::Pending`] through to the stand-in fails the first assertion with `zoxide`.
#[test]
fn a_word_whose_lookup_is_pending_gets_no_stand_in() {
    use crate::session_status::command_reading;
    let files = Arc::new(Stalled {
        table: Table::default().dir("/w/QZPd"),
        ..Stalled::default()
    });
    let worker = Worker::start().expect("a worker thread");
    let ran = Ran::on_worker(Arc::clone(&files) as Arc<dyn Files>, worker, Duration::ZERO);
    let heard = Heard::of(&ran, vec![exec(0, "zoxide")], Some("/w"), Some("/usr/bin"));
    assert_eq!(
        command_reading("./QZPd", &heard),
        None,
        "a directory, unanswered"
    );
    assert_eq!(command_reading("qzl", &heard), None, "an alias, unanswered");
    assert_eq!(heard.look(Word::Path("./QZPd")), Look::Pending, "stalled");
    *files.open.lock().unwrap_or_else(|p| p.into_inner()) = true;
    files.opened.notify_all();
    aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "the worker's answers",
        || {
            (heard.look(Word::Path("./QZPd")) == Look::Cd
                && heard.look(Word::Bare("qzl")) == Look::Nothing)
                .then_some(())
        },
    );
    assert_eq!(
        command_reading("./QZPd", &heard),
        None,
        "a directory: no stand-in"
    );
    let alias = command_reading("qzl", &heard).expect("the alias's job's program");
    assert_eq!((alias.text.as_str(), alias.own), ("zoxide", false));
}

/// A scripted Windows job reader: each listing in turn, then none; it asserts it is asked for
/// the session's shell.
fn scripted_job(shell: i32, listings: Vec<Vec<(u32, &str)>>) -> ListJob {
    let mut listings: std::collections::VecDeque<Vec<(u32, PathBuf)>> = listings
        .into_iter()
        .map(|l| {
            l.into_iter()
                .map(|(pid, p)| (pid, PathBuf::from(p)))
                .collect()
        })
        .collect();
    Box::new(move |asked, _| {
        assert_eq!(asked, shell, "the job of the session's own shell");
        listings.pop_front().unwrap_or_default()
    })
}

/// Each exec of a ran log, by pid and name, oldest first.
fn heard_names(ran: &Ran) -> Vec<(i32, String)> {
    ran.within(Some(Window { from: 0, to: None }))
        .iter()
        .map(|e| {
            (
                e.pid,
                e.names.image.as_deref().unwrap_or_default().to_owned(),
            )
        })
        .collect()
}

/// A WINDOWS SESSION'S PROGRAMS ARE HEARD FROM ITS JOB (J0-1, gap G1-g19): a job listed twice
/// as [the shell 100, the console host 101, 200 running `PING.EXE`] notes ping once, and the
/// shell's and the console host's pids nothing; a block whose typed word is `ping` reads ping
/// (the image's name in any case, as cmd and pwsh find it), and a pid reused by another image is
/// heard again. NEGATIVE CONTROL (in the test): a session with no feed, today's no-source arm,
/// reads only builtins for the same block. NEGATIVE CONTROLS (scratch, `J01_NC`): names compared
/// byte for byte (`case`), the console host heard (`host`), and a pid heard once whatever it
/// maps (`pid`) each fail it.
#[test]
fn a_windows_jobs_programs_are_heard_once_each_and_name_their_typed_word() {
    use crate::session_status::command_detail;
    const PS: &str = r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe";
    const HOST: &str = r"C:\Windows\System32\conhost.exe";
    const PING: &str = r"C:\Windows\System32\PING.EXE";
    let ran = Arc::new(Ran::default());
    let job = vec![(100, PS), (101, HOST), (200, PING)];
    let mut feed = RanJob::listing(
        Arc::clone(&ran),
        100,
        Some(101),
        scripted_job(
            100,
            vec![
                job.clone(),
                job,
                vec![(200, r"C:\Windows\System32\ipconfig.exe")],
            ],
        ),
    );
    feed.list_at(5_000);
    feed.list_at(5_050);
    assert_eq!(
        heard_names(&ran),
        [(200, "PING".to_owned())],
        "ping once, and neither the shell nor the console host"
    );
    let window = Window::of(Some(4_900), None, None, None);
    let block = ran.heard(7, window, Some(r"C:\Users\jsmith"));
    assert_eq!(
        command_detail("ping -n 1 localhost", &block).as_deref(),
        Some("ping"),
        "the typed word names the image that ran"
    );
    assert_eq!(
        command_detail("Ping.exe -n 1 localhost", &block).as_deref(),
        Some("Ping"),
        "in any case, with its extension"
    );
    assert_eq!(command_detail("tracert localhost", &block), None);
    feed.list_at(5_100);
    assert_eq!(
        heard_names(&ran),
        [(200, "PING".to_owned()), (200, "ipconfig".to_owned())],
        "a pid reused by another image is heard again"
    );
    let silent = Ran::default();
    let block = silent.heard(7, window, Some(r"C:\Users\jsmith"));
    assert_eq!(
        command_detail("ping -n 1 localhost", &block),
        None,
        "with no source, no program"
    );
    assert_eq!(
        command_detail(r"cd C:\Users", &block).as_deref(),
        Some("cd"),
        "only builtins"
    );
}

/// A WINDOWS IMAGE'S NAME (J0-1): its file name after the last `\` or `/`, with `.exe` or
/// `.com` cut in any case, equal to a bare word in any ASCII case; a unix image's name is equal
/// byte for byte, as before. NEGATIVE CONTROL (scratch, `J01_NC=case`): names compared byte for
/// byte fail the third assertion.
#[test]
fn a_windows_images_name_is_its_file_name_in_any_case_and_a_unix_ones_is_exact() {
    let of = |path: &str| ExecNames::of_windows_image(Path::new(path));
    let name = |path: &str| of(path).image.map(String::from);
    assert_eq!(
        name(r"C:\Windows\System32\PING.EXE").as_deref(),
        Some("PING")
    );
    assert_eq!(name("C:/tools/7z.Com").as_deref(), Some("7z"));
    assert_eq!(name(r"C:\tools\make.bat").as_deref(), Some("make.bat"));
    assert_eq!(name(r"C:\tools\.exe").as_deref(), Some(".exe"));
    assert_eq!(name(r"C:\tools\"), None);
    let ping = of(r"C:\Windows\System32\PING.EXE");
    assert!(ping.same("PING", "ping") && ping.same("PING", "Ping"));
    assert!(!ping.same("PING", "pin"));
    let unix = ExecNames::of_path(Path::new("/usr/bin/PING"));
    assert!(unix.same("PING", "PING"));
    assert!(!unix.same("PING", "ping"), "a unix name is exact");
}

/// A WINDOWS JOB IS LISTED AT ITS PACE (J0-1): at the first batch, not again within
/// [`JOB_PACE`] of it, again once that has passed, and at once when a batch carries a block's
/// start or end (an OSC 133/633 C or D, even one cut across two batches) and again at the batch
/// after it, the command's first output or the prompt; a prompt's own marks (A, B) are neither.
/// Instants are set by hand. NEGATIVE CONTROLS (scratch): a batch's C not looked for
/// (`J01_NC=start`) fails the third assertion, a start that leaves the next batch to the pace
/// (`J01_NC=armed`) the fourth, and a batch's D not looked for (J0-1 lander, `J01_NC=end`) the
/// first assertion about an end. Which listing each is ([`JobListing`]) is asserted whole: a
/// pace listing passed as a mark (`Some(JobListing::Mark)` in the pace arm) fails it.
#[test]
fn a_windows_job_is_listed_at_its_pace_and_at_once_at_a_blocks_start_and_end() {
    let listings = Arc::new(Mutex::new(Vec::new()));
    let counted = Arc::clone(&listings);
    let mut feed = RanJob::listing(
        Arc::new(Ran::default()),
        100,
        None,
        Box::new(move |_, listing| {
            counted.lock().unwrap().push(listing);
            Vec::new()
        }),
    );
    let listed = || listings.lock().unwrap().len();
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    feed.batch(b"out", at(0), 0);
    assert_eq!(listed(), 1, "the first batch lists it");
    feed.batch(b"out", at(50), 0);
    assert_eq!(listed(), 1, "within the pace, not again");
    feed.batch(b"\x1b]133;C\x07", at(60), 0);
    assert_eq!(listed(), 2, "a block's start lists it at once");
    feed.batch(b"out", at(70), 0);
    assert_eq!(listed(), 3, "and its command's first output again");
    feed.batch(b"out", at(120), 0);
    assert_eq!(listed(), 3, "the pace runs from the last listing");
    feed.batch(b"out", at(170), 0);
    assert_eq!(listed(), 4, "the pace has passed");
    feed.batch(b"\x1b]13", at(175), 0);
    assert_eq!(listed(), 4);
    feed.batch(b"3;C\x07", at(180), 0);
    assert_eq!(listed(), 5, "a start cut across two batches");
    feed.batch(b"out", at(185), 0);
    assert_eq!(listed(), 6);
    feed.batch(b"out", at(190), 0);
    assert_eq!(listed(), 6, "past the start's two listings, the pace again");
    feed.batch(b"out", at(300), 0);
    assert_eq!(listed(), 7, "the pace has passed again");
    feed.batch(b"\x1b]133;D;0\x07", at(310), 0);
    assert_eq!(listed(), 8, "a block's end lists it at once");
    feed.batch(b"PS> ", at(320), 0);
    assert_eq!(listed(), 9, "and the prompt after it again");
    feed.batch(b"\x1b]63", at(330), 0);
    assert_eq!(listed(), 9);
    feed.batch(b"3;D\x1b\\", at(340), 0);
    assert_eq!(listed(), 10, "an end cut across two batches");
    feed.batch(b"x", at(350), 0);
    assert_eq!(listed(), 11);
    {
        use JobListing::{Mark, Pace};
        assert_eq!(
            *listings.lock().unwrap(),
            [
                Mark, Mark, Mark, Pace, Mark, Mark, Pace, Mark, Mark, Mark, Mark
            ],
            "a listing the pace alone brought never walks below the shell; a gather's first, \
             a block's start or end and the batch after each may"
        );
    }
    feed.batch(b"\x1b]133;A\x07PS> \x1b]133;B\x07", at(360), 0);
    assert_eq!(listed(), 11, "a prompt's marks are no block's start or end");
}

/// A PROGRAM A COMMAND LEAVES RUNNING IS PLACED IN ITS OWN BLOCK (J0-1 lander, review finding
/// 2): a pwsh block starts at wall 1000 (its C) and its first output is listed at 1010; it
/// starts a silent background program at 1050 and ends at 1060 (its D), within the pace of the
/// last listing. The listing at its D stamps the program there, inside the block's window, so
/// the block's word names it and the next block's, typed 1.2 s later and started at 2900, does
/// not, though that block's typing echo and its C list the job again while the program still
/// runs. NEGATIVE CONTROL (scratch, `J01_NC=end`): a D not looked for leaves the program to the
/// next listing, the typing echo at 2200, outside its own block's window (the first detail
/// assertion fails) and inside the next block's, where it would name that block's word.
#[test]
fn a_program_a_command_leaves_running_is_placed_in_its_own_block() {
    use crate::session_status::command_detail;
    const BG: &str = r"C:\tools\qzpbgsvc.exe";
    let ran = Arc::new(Ran::default());
    let mut feed = RanJob::listing(
        Arc::clone(&ran),
        100,
        Some(101),
        scripted_job(
            100,
            vec![
                vec![(100, "pwsh.exe")],
                vec![(100, "pwsh.exe")],
                vec![(100, "pwsh.exe"), (300, BG)],
                vec![(100, "pwsh.exe"), (300, BG)],
                vec![(100, "pwsh.exe"), (300, BG)],
            ],
        ),
    );
    let t0 = Instant::now();
    let mut batch = |bytes: &[u8], ms: u64| {
        feed.batch(bytes, t0 + Duration::from_millis(ms), 1_000 + ms);
    };
    batch(b"\x1b]133;C\x07", 0);
    batch(b"started\r\n", 10);
    batch(b"\x1b]133;D;0\x07\x1b]133;A\x07PS> \x1b]133;B\x07", 60);
    batch(b"qzpbgsvc", 1_200);
    batch(b"\r\n\x1b]133;C\x07", 1_900);
    assert_eq!(
        heard_names(&ran),
        [(300, "qzpbgsvc".to_owned())],
        "heard once, while the job holds it"
    );
    let own = Window::of(Some(1_000), Some(1_060), None, Some(2_900));
    let after = own.and_then(|w| w.to);
    let next = Window::of(Some(2_900), None, after, None);
    let cwd = Some(r"C:\Users\jsmith");
    assert_eq!(
        command_detail("qzpbgsvc", &ran.heard(7, own, cwd)).as_deref(),
        Some("qzpbgsvc"),
        "the block that started it names it"
    );
    assert_eq!(
        command_detail("qzpbgsvc", &ran.heard(8, next, cwd)),
        None,
        "the next block, which did not run it, does not"
    );
}

/// One worker's count, read now.
fn count(counter: &std::sync::atomic::AtomicUsize) -> usize {
    counter.load(std::sync::atomic::Ordering::SeqCst)
}

/// `n` sessions on `worker`, each reader waiting `wait`.
fn sessions_on(files: &Arc<Stalled>, worker: &Arc<Worker>, n: usize, wait: Duration) -> Vec<Ran> {
    (0..n)
        .map(|_| {
            Ran::on_worker(
                Arc::clone(files) as Arc<dyn Files>,
                Arc::clone(worker),
                wait,
            )
        })
        .collect()
}

/// Session `i`'s read of its own bare word `k<i>`, in a window where that program ran.
fn look_own(sessions: &[Ran], i: usize) -> Look {
    let word = format!("k{i}");
    let heard = Heard::of(
        &sessions[i],
        vec![exec(0, &word)],
        Some("/w"),
        Some("/usr/bin"),
    );
    heard.look(Word::Bare(&word))
}

/// `/usr/bin/k<i>` for each of `n`, each its own program.
fn k_table(n: usize) -> Table {
    (0..n).fold(Table::default(), |t, i| {
        t.program(&format!("/usr/bin/k{i}"), &format!("k{i}"))
    })
}

/// A STALLED WORKER COSTS THE UI THREAD ONE WAIT, NOT ONE PER NEW LOOKUP (J0-1, gap H1-g14):
/// with a reader's wait of 50 ms and the worker stuck on the first lookup, more readers than
/// [`JOBS_MAX`] ask their own new word: none queues more than the bound, every reader after the
/// first returns Pending, and only the first began a wait (counted, never timed). Once the file
/// system answers, the next read of the first word gets its answer. NEGATIVE CONTROLS (scratch,
/// `J01_NC`): readers that wait on a stalled worker (`stall`) count `JOBS_MAX + 1` waits; that
/// and an unbounded queue together (`old`, the code before J0-1) count one wait per reader and
/// queue all but the first.
#[test]
fn a_stalled_worker_costs_readers_one_wait_and_queues_at_most_its_bound() {
    const WAIT: Duration = Duration::from_millis(50);
    let n = JOBS_MAX + 4;
    let files = Arc::new(Stalled {
        table: k_table(n),
        ..Stalled::default()
    });
    let worker = Worker::start().expect("a worker thread");
    let sessions = sessions_on(&files, &worker, n, WAIT);
    assert_eq!(
        look_own(&sessions, 0),
        Look::Pending,
        "the first reader waited"
    );
    aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "the worker on the first lookup",
        || (count(&files.entered) > 0).then_some(()),
    );
    for i in 1..n {
        assert_eq!(look_own(&sessions, i), Look::Pending, "reader {i}");
    }
    assert!(
        count(&worker.counts.most_queued) <= JOBS_MAX,
        "queued {}",
        count(&worker.counts.most_queued)
    );
    assert_eq!(
        count(&worker.counts.waits),
        1,
        "one wait, the first reader's"
    );
    files.open();
    aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "the first lookup's answer",
        || (look_own(&sessions, 0) == Look::Ran).then_some(()),
    );
}

/// A FULL LOOKUP QUEUE REFUSES A LOOKUP, WHICH IS ASKED AGAIN (J0-1, gap H1-g14): with the
/// worker held back, more readers than [`JOBS_MAX`] ask their own new word: the queue takes
/// `JOBS_MAX` and refuses the rest, each read Pending at once; once the worker has answered the
/// queued ones, a refused word's next read asks again and gets its answer. NEGATIVE CONTROLS
/// (scratch, `J01_NC`): an unbounded queue (`queue`) queues every reader; a refused lookup
/// marked asked (`asked`) is never asked again.
#[test]
fn a_full_lookup_queue_refuses_a_lookup_and_a_later_read_asks_it_again() {
    let n = JOBS_MAX + 4;
    let files = Arc::new(Stalled {
        table: k_table(n),
        ..Stalled::default()
    });
    let gate = Arc::clone(&files);
    let worker = Worker::start_after(move || gate.wait_open()).expect("a worker thread");
    let sessions = sessions_on(&files, &worker, n, Duration::ZERO);
    for i in 0..n {
        assert_eq!(look_own(&sessions, i), Look::Pending, "reader {i}");
    }
    assert_eq!(count(&worker.counts.posted), JOBS_MAX, "the queue's bound");
    assert!(count(&worker.counts.most_queued) <= JOBS_MAX);
    files.open();
    aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "the queued lookups' answers",
        || (count(&worker.counts.answered) == JOBS_MAX).then_some(()),
    );
    let last = n - 1;
    assert_eq!(look_own(&sessions, last), Look::Pending, "asked again");
    assert_eq!(
        count(&worker.counts.posted),
        JOBS_MAX + 1,
        "a refused word is asked again"
    );
    aterm_spec::wait::until_within(
        aterm_spec::wait::MUST_HAPPEN,
        "the refused word's answer",
        || (look_own(&sessions, last) == Look::Ran).then_some(()),
    );
}
