// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{
    PassVerdict, SeedMarkers, SeedRetire, Wake, classify_pass_exit, pass_said_detail,
    read_pass_stderr, read_seed_markers, seed_retire, seed_retire_event,
};

fn drive(script: &str) -> (SeedMarkers, bool, Vec<Wake>) {
    let (seen, code, posted) = drive_code(script);
    (seen, code == Some(0), posted)
}

fn drive_code(script: &str) -> (SeedMarkers, Option<i32>, Vec<Wake>) {
    let mut child = aterm_uds::spawnfd::command("/bin/sh")
        .arg("-c")
        .arg(script)
        .stdout(aterm_uds::spawnfd::Stdio::piped())
        .stderr(aterm_uds::spawnfd::Stdio::null())
        .spawn()
        .expect("the fake atpkg child spawns");
    let mut posted = Vec::new();
    let seen = read_seed_markers(
        std::io::BufReader::new(child.stdout.take().expect("piped stdout")),
        |event| posted.push(event),
    );
    let code = child.wait().ok().and_then(|s| s.code());
    (seen, code, posted)
}

#[test]
fn an_announced_pass_that_exits_zero_is_not_a_failure() {
    let (seen, ok, posted) = drive(
        "echo 'atpkg: net-starting: installing 2 program(s) over the network: claude, codex \
         (~410 MB to download)'; exit 0",
    );
    assert!(seen.saw_start, "the announcement must be seen");
    assert!(!seen.saw_terminal, "this child prints no terminal marker");
    assert!(ok, "the child exited 0");
    assert!(
        matches!(posted.as_slice(), [Wake::PkgSeedStarted { .. }]),
        "the stream itself raises only the announcement: {posted:?}"
    );
    let verdict = seed_retire(seen, ok, true);
    assert_eq!(
        verdict,
        SeedRetire::Finished,
        "an announced pass that exited ZERO over a stocked store did not fail"
    );
    assert!(
        !matches!(
            seed_retire_event(verdict, ""),
            Some(Wake::PkgSeedFailed { .. })
        ),
        "the banner must not say the install failed"
    );
}

#[test]
fn a_child_that_dies_after_announcing_still_reports_a_failure() {
    let (seen, ok, _) = drive("echo 'atpkg: net-starting: installing 9 program(s)'; exit 7");
    assert!(seen.saw_start && !seen.saw_terminal);
    assert!(!ok);
    assert_eq!(seed_retire(seen, ok, true), SeedRetire::Failed);
    assert!(matches!(
        seed_retire_event(seed_retire(seen, ok, true), "atpkg: the store lock is held"),
        Some(Wake::PkgSeedFailed { .. })
    ));
}

#[test]
fn the_quiet_steady_state_raises_nothing() {
    let (seen, ok, posted) =
        drive("echo 'atpkg: the store already holds the ALab toolset — nothing to seed'; exit 0");
    assert!(!seen.saw_start && !seen.saw_terminal);
    assert!(ok);
    assert!(
        posted.is_empty(),
        "a plain sentence is not a marker: {posted:?}"
    );
    for stocked in [true, false] {
        assert_eq!(
            seed_retire(seen, ok, stocked),
            SeedRetire::Nothing,
            "no announcement, no card — whatever the store holds"
        );
        assert!(seed_retire_event(seed_retire(seen, ok, stocked), "").is_none());
    }
}

#[test]
fn the_positive_terminal_answers_the_announcement() {
    let (seen, ok, posted) = drive(
        "echo 'atpkg: net-starting: installing 2 program(s)'; \
         echo 'atpkg: seed-done: the pass finished; 12 ALab program(s) are installed'; exit 0",
    );
    assert!(seen.saw_start, "the announcement opened");
    assert!(
        seen.saw_terminal,
        "`seed-done:` is a TERMINAL — the absence the GUI used to read is gone"
    );
    assert!(matches!(
        posted.as_slice(),
        [Wake::PkgSeedStarted { .. }, Wake::PkgSeedDone { .. }]
    ));
    assert_eq!(seed_retire(seen, ok, true), SeedRetire::Nothing);

    let (seen, ok, posted) = drive(
        "echo 'atpkg: net-starting: installing 2 program(s) over the network: claude, codex'; \
         echo 'atpkg: net-installed: claude, codex'; exit 0",
    );
    assert!(
        seen.saw_start && seen.saw_terminal,
        "`net-installed:` is a TERMINAL too"
    );
    assert!(matches!(
        posted.as_slice(),
        [Wake::PkgSeedStarted { .. }, Wake::PkgSeed { .. }]
    ));
    assert_eq!(seed_retire(seen, ok, true), SeedRetire::Nothing);
}

#[test]
fn an_informational_row_does_not_answer_an_announcement() {
    let (seen, ok, posted) = drive(
        "echo 'atpkg: net-starting: installing 2 program(s) over the network: claude, codex'; \
         echo 'atpkg: managed-current: claude 2.1.280 (Anthropic latest)'; exit 0",
    );
    assert!(seen.saw_start);
    assert!(
        !seen.saw_terminal,
        "an R6 row says what the machine HAS, not how the pass ended"
    );
    assert_eq!(posted.len(), 2, "{posted:?}");
    assert_eq!(seed_retire(seen, ok, true), SeedRetire::Finished);
}

#[test]
fn a_contended_pass_is_not_a_failed_install() {
    let (seen, code, posted) = drive_code(&format!(
        "echo 'atpkg: seed-busy: another atpkg process holds the store lock \
         — that pass is doing this work and this one stood aside'; exit {}",
        atpkg::lock::CONTENDED_EXIT
    ));
    assert_eq!(
        code,
        Some(75),
        "the contended child exits atpkg's contention code"
    );
    assert!(
        seen.saw_terminal && seen.saw_busy && !seen.saw_start,
        "`seed-busy:` ENDS the pass, says so, and opens nothing: {seen:?}"
    );
    assert!(
        posted.is_empty(),
        "the line is the lane's to log, not an event: {posted:?}"
    );
    assert_eq!(
        classify_pass_exit(code, seen.saw_start, seen.saw_terminal),
        PassVerdict::Busy,
        "a contended pass is deferred, never a failed install"
    );
    let (bare, bare_code, bare_posted) =
        drive_code("echo 'atpkg: the prefix is not writable' >&2; exit 1");
    assert_eq!(bare_code, Some(1));
    assert!(
        bare_posted.is_empty() && bare == SeedMarkers::default(),
        "{bare:?}"
    );
    assert_eq!(
        classify_pass_exit(bare_code, bare.saw_start, bare.saw_terminal),
        PassVerdict::Refused,
        "a real refusal still reaches the card"
    );
}

#[test]
fn an_announced_pass_that_installs_nothing_says_so() {
    let (seen, ok, _) = drive("echo 'atpkg: net-starting: installing 2 program(s)'; exit 0");
    assert_eq!(seed_retire(seen, ok, false), SeedRetire::FinishedEmpty);
    assert!(matches!(
        seed_retire_event(SeedRetire::FinishedEmpty, ""),
        Some(Wake::PkgSeedNothing { detail }) if detail.is_empty()
    ));
    assert!(matches!(
        seed_retire_event(SeedRetire::FinishedEmpty, "  ay: no build for this Mac\n"),
        Some(Wake::PkgSeedNothing { detail }) if detail == "ay: no build for this Mac"
    ));
    assert!(matches!(
        seed_retire_event(SeedRetire::Finished, ""),
        Some(Wake::PkgSeedDone { detail }) if detail.is_empty()
    ));
}

#[test]
fn a_non_utf8_stdout_line_does_not_end_the_marker_reader() {
    let (seen, code, posted) = drive_code(
        "echo 'atpkg: net-starting: installing 2 program(s)'; \
         printf 'brew: \\377\\376 not utf-8\\n'; \
         echo 'atpkg: seed-done: the pass finished; 12 ALab program(s) are installed'; \
         exit 0",
    );
    assert_eq!(code, Some(0));
    assert!(
        seen.saw_start,
        "the announcement before the bad line: {seen:?}"
    );
    assert!(
        seen.saw_terminal,
        "the terminal AFTER one non-UTF-8 line must still be read: {seen:?} {posted:?}"
    );
    assert!(
        matches!(
            posted.as_slice(),
            [Wake::PkgSeedStarted { .. }, Wake::PkgSeedDone { .. }]
        ),
        "both markers posted, the raw line raised nothing: {posted:?}"
    );
}

#[test]
fn a_non_utf8_stderr_byte_does_not_drop_the_refusal() {
    let mut child = aterm_uds::spawnfd::command("/bin/sh")
        .arg("-c")
        .arg(
            "echo 'atpkg: the prefix is not writable: /x' >&2; \
             printf 'brew: \\377\\n' >&2; \
             echo 'atpkg: refusing to mutate the store without the lock' >&2; \
             exit 1",
        )
        .stdout(aterm_uds::spawnfd::Stdio::null())
        .stderr(aterm_uds::spawnfd::Stdio::piped())
        .spawn()
        .expect("the fake atpkg child spawns");
    let said = read_pass_stderr(child.stderr.take().expect("piped stderr"));
    assert_eq!(child.wait().ok().and_then(|s| s.code()), Some(1));
    assert!(
        said.contains("the prefix is not writable")
            && said.contains("refusing to mutate the store"),
        "every sentence around the raw byte is kept: {said:?}"
    );
    assert!(
        pass_said_detail(&said).contains("the prefix is not writable"),
        "the card quotes the refusal: {:?}",
        pass_said_detail(&said)
    );
}
