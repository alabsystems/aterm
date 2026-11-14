// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

/// A session id as aterm mints one, from a number.
fn sid(n: u64) -> String {
    format!("s-{n:020x}")
}

fn start(session: u64, workspace: &Path, at_ms: u64) -> Start {
    Start {
        session: sid(session),
        workspace: workspace.to_path_buf(),
        build: None,
        pid: 7,
        at_ms,
    }
}

/// A scratch folder (canonical, as a workspace is recorded) holding a state folder, and the
/// record's path inside that.
fn scratch() -> (aterm_tempfile::TempDir, PathBuf, PathBuf) {
    let directory = aterm_tempfile::tempdir().unwrap();
    let base = directory.path().canonicalize().unwrap();
    let state = base.join("state");
    std::fs::create_dir(&state).unwrap();
    let record = state.join(RECORD);
    (directory, base, record)
}

fn lines_of(record: &Path) -> Vec<String> {
    std::fs::read_to_string(record)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect()
}

/// Every line of `record`, each read as a start.
fn starts_of(record: &Path) -> Vec<Start> {
    lines_of(record)
        .iter()
        .map(|line| Start::parse(line).unwrap_or_else(|| panic!("{line}")))
        .collect()
}

#[test]
fn a_start_reads_back_from_its_line_and_a_line_naming_no_tab_or_no_absolute_path_is_refused() {
    let (_directory, base, _) = scratch();
    let plain = Start {
        session: sid(0x0123_4567_89ab_cdef),
        workspace: base.join("work tree"),
        build: Some(base.join("cache").join("it's \"here\"\nnow")),
        pid: 4242,
        at_ms: 1_731_450_000_000,
    };
    let line = plain.line().unwrap();
    assert!(line.ends_with('\n'));
    assert_eq!(line.matches('\n').count(), 1, "one line: {line:?}");
    assert_eq!(Start::parse(&line), Some(plain.clone()));
    assert_eq!(
        Start::parse(line.trim_end_matches('\n')),
        Some(plain.clone())
    );
    let unknown = Start {
        build: None,
        ..plain.clone()
    };
    let line = unknown.line().unwrap();
    assert!(!line.contains("\"build\""), "{line}");
    assert_eq!(Start::parse(&line), Some(unknown.clone()));

    // What a writer refuses to spell.
    for session in [
        "",
        "x",
        "s-",
        "s-123",
        "s-0123456789ABCDEF0123",
        "s-0123456789abcdef01234",
        "t-0123456789abcdef0123",
    ] {
        let named = Start {
            session: session.to_owned(),
            ..plain.clone()
        };
        assert_eq!(named.line(), None, "{session:?}");
    }
    let relative = Start {
        workspace: PathBuf::from("work"),
        ..plain.clone()
    };
    assert_eq!(relative.line(), None);
    let relative = Start {
        build: Some(PathBuf::from("target")),
        ..plain.clone()
    };
    assert_eq!(relative.line(), None);
    let long = Start {
        workspace: base.join("w".repeat(MAX_LINE)),
        ..plain.clone()
    };
    assert_eq!(long.line(), None);
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt as _;
        let mut bytes = base.as_os_str().as_bytes().to_vec();
        bytes.extend_from_slice(b"/\xff");
        let raw = Start {
            workspace: PathBuf::from(std::ffi::OsStr::from_bytes(&bytes)),
            ..plain.clone()
        };
        assert_eq!(raw.line(), None);
    }

    // What a reader skips, and what it reads past.
    let session = sid(1);
    let workspace = aterm_json::to_string(base.to_str().unwrap()).unwrap();
    let said =
        |rest: &str| format!("{{\"session\":\"{session}\",\"workspace\":{workspace}{rest}}}");
    let whole = said(",\"v\":1,\"pid\":3,\"at_ms\":9");
    assert_eq!(
        Start::parse(&whole),
        Some(Start {
            session: session.clone(),
            workspace: base.clone(),
            build: None,
            pid: 3,
            at_ms: 9,
        }),
        "the control: {whole}"
    );
    assert_eq!(
        Start::parse(&said(
            ",\"v\":1,\"pid\":3,\"at_ms\":9,\"build\":null,\"tool\":\"x.py\",\"more\":[1,2]"
        )),
        Start::parse(&whole),
        "an unknown key is read past, and a null build is none"
    );
    for skipped in [
        said(",\"v\":2,\"pid\":3,\"at_ms\":9"),
        said(",\"pid\":3,\"at_ms\":9"),
        said(",\"v\":1,\"at_ms\":9"),
        said(",\"v\":1,\"pid\":3"),
        said(",\"v\":1,\"pid\":-3,\"at_ms\":9"),
        said(",\"v\":1,\"pid\":3,\"at_ms\":9,\"build\":\"target\""),
        said(",\"v\":1,\"pid\":3,\"at_ms\":9,\"build\":7"),
        "{\"v\":1,\"session\":\"x\",\"workspace\":\"/w\",\"pid\":3,\"at_ms\":9}".to_owned(),
        format!("{{\"v\":1,\"session\":\"{session}\",\"workspace\":\"w\",\"pid\":3,\"at_ms\":9}}"),
        format!("{{\"v\":1,\"session\":\"{session}\",\"pid\":3,\"at_ms\":9}}"),
        whole[..whole.len() - 1].to_owned(),
        "[1]".to_owned(),
        String::new(),
    ] {
        assert_eq!(Start::parse(&skipped), None, "{skipped}");
    }
}

#[test]
fn the_session_is_the_tabs_parent_variable_and_nothing_else() {
    let tab = sid(0xabc);
    let env = |parent: Option<&str>| {
        let parent = parent.map(str::to_owned);
        move |name: &str| match name {
            "ATERM_PARENT_SESSION_ID" => parent.clone(),
            // The id of an aterm started inside the tab, which no window holds.
            "ATERM_SESSION_ID" => Some(sid(0xdead)),
            _ => None,
        }
    };
    assert_eq!(
        session_of(&env(Some(tab.as_str()))).as_deref(),
        Some(tab.as_str())
    );
    assert_eq!(session_of(&env(None)), None, "outside a tab");
    assert_eq!(session_of(&env(Some("x"))), None);
    assert_eq!(session_of(&env(Some(""))), None);
}

#[test]
fn an_append_is_one_whole_line_and_a_read_is_the_newest_of_each_session_and_workspace() {
    let (_directory, base, record) = scratch();
    let (one, two) = (base.join("one"), base.join("two"));
    assert_eq!(read(&record), Vec::<Start>::new(), "no record yet");
    append(&record, &start(1, &one, 10)).unwrap();
    append(&record, &start(1, &two, 20)).unwrap();
    append(&record, &start(2, &one, 30)).unwrap();
    append(&record, &start(1, &one, 40)).unwrap();
    assert_eq!(lines_of(&record).len(), 4, "every append is a line");
    assert_eq!(
        read(&record),
        vec![start(1, &two, 20), start(2, &one, 30), start(1, &one, 40)],
        "the first line is superseded by the fourth"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(&record).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    // A line this module cannot read is skipped, not an end of the read.
    let mut text = std::fs::read_to_string(&record).unwrap();
    text.push_str("not a line\n{\"v\":2}\n");
    text.push_str(&start(3, &two, 50).line().unwrap());
    std::fs::write(&record, text).unwrap();
    assert_eq!(read(&record).last(), Some(&start(3, &two, 50)));

    // No folder is made for a record, and a start with no line writes nothing.
    let nowhere = base.join("no-state").join(RECORD);
    assert!(append(&nowhere, &start(1, &one, 60)).is_err());
    assert!(!base.join("no-state").exists());
    let before = lines_of(&record);
    let unnamed = Start {
        session: "x".to_owned(),
        ..start(1, &one, 70)
    };
    assert_eq!(
        append(&record, &unnamed).unwrap_err().kind(),
        io::ErrorKind::InvalidInput
    );
    assert_eq!(lines_of(&record), before);
}

/// NEVER THROUGH A LINK, NEVER WAITING: a link at the record's path (to a file or to nothing)
/// and a FIFO there each refuse the append at once, write nothing through it, and read as no
/// record. NEGATIVE CONTROL: the same append to a plain path writes its line.
#[cfg(unix)]
#[test]
fn an_append_never_writes_through_a_link_or_waits_on_a_fifo_at_the_records_path() {
    let (_directory, base, record) = scratch();
    let one = base.join("one");
    let elsewhere = base.join("elsewhere");
    std::fs::write(&elsewhere, "theirs\n").unwrap();
    std::os::unix::fs::symlink(&elsewhere, &record).unwrap();
    assert!(
        append(&record, &start(1, &one, 10)).is_err(),
        "a link to a file"
    );
    assert_eq!(std::fs::read_to_string(&elsewhere).unwrap(), "theirs\n");
    assert_eq!(read(&record), Vec::<Start>::new());
    std::fs::remove_file(&record).unwrap();
    std::os::unix::fs::symlink(base.join("nothing"), &record).unwrap();
    assert!(
        append(&record, &start(1, &one, 10)).is_err(),
        "a dangling link"
    );
    assert!(!base.join("nothing").exists(), "nothing made through it");
    std::fs::remove_file(&record).unwrap();
    {
        use std::os::unix::ffi::OsStrExt as _;
        let path = std::ffi::CString::new(record.as_os_str().as_bytes()).unwrap();
        // SAFETY: a NUL-terminated owned scratch name; no pointer is retained.
        assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
    }
    // A blocking open of a FIFO with no reader would never return: the hang is the failure.
    assert!(append(&record, &start(1, &one, 10)).is_err(), "a FIFO");
    assert_eq!(read(&record), Vec::<Start>::new());
    std::fs::remove_file(&record).unwrap();
    append(&record, &start(1, &one, 10)).unwrap();
    assert_eq!(read(&record), vec![start(1, &one, 10)], "the control");
}

#[test]
fn a_read_of_a_long_record_is_its_end_without_the_line_the_window_cut() {
    let (_directory, base, record) = scratch();
    let mut text = String::new();
    let mut at = 0_u64;
    while (text.len() as u64) <= READ_MAX + 4 * MAX_LINE as u64 {
        at += 1;
        text.push_str(&start(at, &base.join(format!("w{at}")), at).line().unwrap());
    }
    std::fs::write(&record, &text).unwrap();
    let window = text.len() - usize::try_from(READ_MAX).unwrap();
    let cut = text.as_bytes()[window..]
        .iter()
        .position(|byte| *byte == b'\n')
        .unwrap();
    let whole = window + cut + 1;
    let expected: Vec<Start> = text[whole..]
        .lines()
        .map(|line| Start::parse(line).unwrap())
        .collect();
    let got = read(&record);
    assert_eq!(got.len(), expected.len());
    assert_eq!(got, expected);
    assert!(
        got.len() < usize::try_from(at).unwrap() && got.len() > 1,
        "the record's start is past the window: {} of {at}",
        got.len()
    );
    assert_eq!(got.last().map(|start| start.at_ms), Some(at));
}

/// A REWRITE KEEPS LINES BY WHAT THEY SAY ALONE: the newest line of each workspace (whether or
/// not the workspace is still a folder: no path a line names is looked at), and the newest of
/// each session and workspace within [`KEEP_PAIR_FOR`] before the rewrite's clock; a line dated
/// after that clock counts as old. NEGATIVE CONTROLS: the superseded pair, the old pair and the
/// future-dated pair each go.
#[test]
fn a_rewrite_keeps_each_workspaces_newest_line_and_young_pairs_by_what_the_lines_say() {
    let (_directory, base, record) = scratch();
    // None of these is made: a rewrite never asks whether a recorded path exists.
    let (a, b, c, d) = (
        base.join("a"),
        base.join("b"),
        base.join("c"),
        base.join("d"),
    );
    let now = 1_800_000_000_000_u64;
    let day = 24 * 60 * 60 * 1000;
    let lines = [
        // An old session's line of a workspace a later session built in: dropped.
        start(1, &a, now - 30 * day),
        // Superseded in its own session and workspace: dropped.
        start(2, &a, now - 20 * day),
        // The only line of its workspace, which is no folder: it outlives its session.
        start(1, &b, now - 30 * day),
        // The newest line of its workspace, old as it is.
        start(2, &a, now - 10 * day),
        // A session's newest line of a workspace, within the window: kept beside the newer one.
        start(4, &c, now - day),
        // Dated after the rewrite's clock: old, so dropped beside the newer line.
        start(6, &d, now + day),
        start(5, &c, now),
        start(7, &d, now),
    ];
    let text: String = lines.iter().map(|line| line.line().unwrap()).collect();
    let parsed: Vec<(String, Start)> = text
        .lines()
        .map(|line| (line.to_owned(), Start::parse(line).unwrap()))
        .collect();
    let keep = kept(&parsed, Some(now));
    let survivors: Vec<Start> = lines
        .iter()
        .zip(&keep)
        .filter(|(_, keep)| **keep)
        .map(|(line, _)| line.clone())
        .collect();
    assert_eq!(
        survivors,
        vec![
            lines[2].clone(),
            lines[3].clone(),
            lines[4].clone(),
            lines[6].clone(),
            lines[7].clone(),
        ]
    );
    // With no clock, no pair is young: each workspace's newest line alone.
    let survivors: Vec<&Start> = lines
        .iter()
        .zip(kept(&parsed, None))
        .filter(|(_, keep)| *keep)
        .map(|(line, _)| line)
        .collect();
    assert_eq!(
        survivors,
        [&lines[2], &lines[3], &lines[6], &lines[7]],
        "no clock"
    );
    // A rewrite of a record under the cap is none: it stays as it is.
    std::fs::write(&record, &text).unwrap();
    compact(&record, Some(now));
    assert_eq!(lines_of(&record).len(), lines.len(), "under the cap");
}

/// THE REWRITE'S BOUND AND HYSTERESIS: a record past [`CAP`] is rewritten to at most
/// [`KEEP_MAX`] bytes, shedding the oldest young pair lines first and then the oldest
/// workspaces' lines, never the newest; the line that crossed the cap stays. NEGATIVE CONTROL:
/// an append that leaves the record under the cap rewrites nothing, vanished lines included.
#[test]
fn a_record_past_the_cap_is_rewritten_to_half_of_it_newest_kept() {
    let (_directory, base, record) = scratch();
    let now = aterm_time::unix_millis().unwrap();
    // Every line its own workspace: by kind alone all would stay.
    let mut text = String::new();
    let mut at = 0_u64;
    while (text.len() as u64) < CAP - MAX_LINE as u64 {
        at += 1;
        text.push_str(&start(1, &base.join(format!("w{at}")), now).line().unwrap());
    }
    std::fs::write(&record, &text).unwrap();
    let last = start(2, &base.join("last"), now);
    append(&record, &last).unwrap();
    assert_eq!(
        lines_of(&record).len(),
        usize::try_from(at).unwrap() + 1,
        "under the cap nothing is rewritten"
    );
    // Pairs of one workspace, all young, and workspaces past the bound.
    let mut more = String::new();
    while (text.len() + more.len()) as u64 <= CAP {
        at += 1;
        more.push_str(&start(at, &base.join("shared"), now).line().unwrap());
    }
    std::fs::write(&record, format!("{text}{more}")).unwrap();
    append(&record, &last).unwrap();
    let size = std::fs::metadata(&record).unwrap().len();
    assert!(size <= KEEP_MAX, "{size} bytes kept, at most {KEEP_MAX}");
    let kept = starts_of(&record);
    assert_eq!(kept.last(), Some(&last), "the line that crossed the cap");
    let shared: Vec<&Start> = kept
        .iter()
        .filter(|s| s.workspace == base.join("shared"))
        .collect();
    assert_eq!(
        shared.len(),
        1,
        "the young pairs of one workspace went first, its newest stays"
    );
    assert_eq!(shared[0].session, sid(at), "{shared:?}");
    assert!(
        kept.iter().all(|s| s.workspace != base.join("w1")),
        "the oldest workspaces went next"
    );
    // Nothing but the record and its lock is left in the folder: the stage was renamed away.
    let mut left: Vec<_> = std::fs::read_dir(record.parent().unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    left.sort();
    assert_eq!(
        left,
        [
            std::ffi::OsString::from(RECORD),
            std::ffi::OsString::from(LOCK)
        ]
    );
}

/// ONE REWRITER, AND NONE WAITS: while another holds the lock beside the record, an append past
/// the cap writes its line and returns without a rewrite. NEGATIVE CONTROL: once the lock is
/// let go, the next append rewrites.
#[test]
fn an_append_past_the_cap_leaves_the_rewrite_to_the_lock_holder() {
    let (_directory, base, record) = scratch();
    let now = aterm_time::unix_millis().unwrap();
    let stale = start(3, &base.join("w"), now).line().unwrap();
    let over = usize::try_from(CAP).unwrap() / stale.len() + 1;
    std::fs::write(&record, stale.repeat(over)).unwrap();
    let lock = open_private(&record.with_file_name(LOCK), true).unwrap();
    let held = crate::lock::Flock::try_lock(lock).ok().unwrap();
    append(&record, &start(6, &base.join("a"), now)).unwrap();
    assert_eq!(
        lines_of(&record).len(),
        over + 1,
        "held: no rewrite, the line written"
    );
    drop(held);
    append(&record, &start(6, &base.join("a"), now)).unwrap();
    assert_eq!(
        starts_of(&record),
        vec![
            start(3, &base.join("w"), now),
            start(6, &base.join("a"), now)
        ],
        "let go: the next append rewrites"
    );
}

/// A REWRITE JUST RAN: a rewriter that takes the lock and then finds the record at or under the
/// cap (another rewrote it between this one's append and its lock) leaves it as it is, though a
/// rewrite would drop the stale line of a pair. NEGATIVE CONTROL: the same lines past the cap
/// are rewritten to the newest.
#[test]
fn a_rewrite_that_finds_the_record_under_the_cap_once_locked_leaves_it() {
    let (_directory, base, record) = scratch();
    let now = aterm_time::unix_millis().unwrap();
    let stale = start(3, &base.join("w"), now).line().unwrap();
    std::fs::write(&record, stale.repeat(2)).unwrap();
    compact(&record, Some(now));
    assert_eq!(lines_of(&record).len(), 2, "under the cap: no rewrite");
    let over = usize::try_from(CAP).unwrap() / stale.len() + 1;
    std::fs::write(&record, stale.repeat(over)).unwrap();
    compact(&record, Some(now));
    assert_eq!(
        starts_of(&record),
        vec![start(3, &base.join("w"), now)],
        "past the cap: rewritten"
    );
}

/// The words the front child hands the real shim front, as JSON: set, the child test runs.
#[cfg(target_os = "macos")]
const FRONT_WORDS: &str = "ATPKG_TEST_START_FRONT_WORDS";

/// This test image run again as a shim's hop: it places the call as the shim's block would
/// ([`crate::build_cache::placed_build_dir`] in its own directory and environment, or, for the
/// trust lane, `env`: the parent exported the directory) and enters the real front, which
/// becomes `/bin/echo` on the words. A refusal returns before that.
#[cfg(target_os = "macos")]
#[test]
fn start_front_child_if_asked() {
    use crate::build_cache::{BUILD_DIR_ENV, BuildDir, Machine, Tool};
    let Ok(words) = std::env::var(FRONT_WORDS) else {
        return;
    };
    let words: Vec<String> = aterm_json::from_str(&words).unwrap();
    let cwd = std::env::current_dir().unwrap();
    let (placed, handed) = crate::build_cache::registered::with_live_machine(Some(&cwd), |m| {
        let hidden = |name: &str| (name != BUILD_DIR_ENV).then(|| (m.env)(name)).flatten();
        let typed = Machine { env: &hidden, ..*m };
        if matches!(
            crate::build_cache::decide(&words, Tool::Targo, &typed),
            Some(BuildDir::TrustEnv(_))
        ) {
            return ("env", words.clone());
        }
        match crate::build_cache::placed_build_dir(&words, Tool::Targo, m) {
            Some((_, handed)) => ("config", handed),
            None => ("-", words.clone()),
        }
    });
    let mut args: Vec<std::ffi::OsString> = ["--shim", "targo", placed, "/bin/echo"]
        .into_iter()
        .map(std::ffi::OsString::from)
        .collect();
    args.extend(handed.into_iter().map(std::ffi::OsString::from));
    let code = crate::build_admit::run_hidden(&args);
    std::process::exit(if code == std::process::ExitCode::SUCCESS {
        0
    } else {
        75
    });
}

/// A scratch home with one workspace and a state folder, for runs of the real front.
#[cfg(target_os = "macos")]
struct Front {
    _directory: aterm_tempfile::TempDir,
    home: PathBuf,
    source: PathBuf,
    state: PathBuf,
}

#[cfg(target_os = "macos")]
impl Front {
    fn new() -> Self {
        use std::os::unix::fs::DirBuilderExt as _;
        let directory = aterm_tempfile::tempdir().unwrap();
        let home = directory.path().canonicalize().unwrap().join("home");
        let private = |path: &Path| {
            std::fs::DirBuilder::new().mode(0o700).create(path).unwrap();
        };
        // Writable by its owner alone whatever the umask, as a person's home is.
        private(&home);
        let source = home.join("source");
        std::fs::DirBuilder::new()
            .mode(0o755)
            .create(&source)
            .unwrap();
        std::fs::write(source.join("Cargo.toml"), "[workspace]\nmembers = []\n").unwrap();
        let state = home.join("state");
        private(&state);
        Self {
            _directory: directory,
            home,
            source,
            state,
        }
    }

    fn record(&self) -> PathBuf {
        self.state.join(RECORD)
    }

    /// `words` through the real shim front in `cwd`, started in the tab whose session is
    /// `session` (in no tab for `None`), with `env` set too.
    fn run_with(
        &self,
        cwd: &Path,
        words: &[&str],
        session: Option<&str>,
        env: &[(&str, &Path)],
    ) -> std::process::Output {
        let words: Vec<String> = words.iter().map(|word| (*word).to_owned()).collect();
        let mut command = aterm_uds::spawnfd::command(std::env::current_exe().unwrap());
        // Terse progress leaves the exec'd tool's first output line unprefixed.
        command
            .args([
                "--exact",
                "build_start::tests::start_front_child_if_asked",
                "--quiet",
                "--nocapture",
                "--test-threads=1",
            ])
            .current_dir(cwd)
            .env_clear()
            .env("HOME", &self.home)
            .env("ATERM_STATE_HOME", &self.state)
            .env(FRONT_WORDS, aterm_json::to_string(&words).unwrap())
            .env(
                crate::build_admit::LEDGER_ENV,
                self.home.join("ledger.noindex"),
            );
        if let Some(path) = std::env::var_os("PATH") {
            command.env("PATH", path);
        }
        if let Some(session) = session {
            command.env(aterm_types::domain::ENV_PARENT_SESSION_ID, session);
        }
        for (name, value) in env {
            command.env(name, value);
        }
        aterm_uds::deadline::ends_within(&mut command, aterm_spec::wait::MUST_HAPPEN)
    }

    fn run(&self, cwd: &Path, words: &[&str], session: Option<&str>) -> std::process::Output {
        self.run_with(cwd, words, session, &[])
    }

    /// Every line of the record, oldest first, each read as a start.
    fn starts(&self) -> Vec<Start> {
        match std::fs::symlink_metadata(self.record()) {
            Ok(_) => starts_of(&self.record()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(error) => panic!("{error}"),
        }
    }
}

/// Whether the tool the front became printed `words` (among the placement's and the claim's
/// words around them): the build started.
#[cfg(target_os = "macos")]
fn started(out: &std::process::Output, words: &[&str]) -> bool {
    out.status.success()
        && String::from_utf8_lossy(&out.stdout)
            .lines()
            .any(|line| line.contains(&words.join(" ")))
}

/// THE FRONT'S START LINE: a call that produces build output, started in a tab, leaves exactly
/// one line naming the tab's session, the workspace root and its build folder before it
/// becomes the build, however it reached the workspace (its folder, `--manifest-path`, `-m`,
/// `-C`) and whether or not it claims (`run`, and a user alias over it). NEGATIVE CONTROLS: a
/// call that builds nothing, a build outside a tab, one whose tab variable is no session id,
/// and one that names no manifest and finds none (`install x` from a folder with none) each
/// run and leave no line.
#[cfg(target_os = "macos")]
#[test]
fn the_front_writes_one_start_line_for_a_build_in_a_tab() {
    let front = Front::new();
    let tab = sid(0x0123_4567_89ab_cdef);
    let cache =
        crate::build_cache::build_dir(&crate::build_cache::cache_root(&front.home), &front.source);
    let source = front.source.to_str().unwrap();
    let manifest = front.source.join("Cargo.toml");
    let manifest = manifest.to_str().unwrap();
    let elsewhere = front.home.join("elsewhere");
    std::fs::create_dir(&elsewhere).unwrap();
    let mut count = 0;
    // Each call leaves one line more: the workspace, in the tab, with the shared cache's
    // build folder for it where the shim placed the call there.
    let mut leaves_its_line = |cwd: &Path, words: &[&str], build: Option<&PathBuf>| {
        let out = front.run(cwd, words, Some(&tab));
        assert!(started(&out, words), "{words:?}: {out:?}");
        let lines = front.starts();
        count += 1;
        assert_eq!(lines.len(), count, "{words:?}: {lines:?}");
        let line = lines.last().unwrap().clone();
        assert_eq!(line.session, tab, "{words:?}");
        assert_eq!(line.workspace, front.source, "{words:?}");
        assert_eq!(line.build.as_ref(), build, "{words:?}");
        assert!(line.pid > 0 && line.at_ms > 0, "{line:?}");
        line
    };

    // A build in the workspace's own folder, and the same workspace reached by path.
    let build = ["--unverified", "build", "--jobs=1"];
    leaves_its_line(&front.source, &build, Some(&cache));
    let by_path = [
        "--unverified",
        "build",
        "--manifest-path",
        manifest,
        "--jobs=1",
    ];
    leaves_its_line(&elsewhere, &by_path, Some(&cache));
    let by_short = ["--unverified", "build", "-m", manifest, "--jobs=1"];
    leaves_its_line(&elsewhere, &by_short, Some(&cache));
    // The shim's block leaves a `-C` call as typed: its output goes where Targo puts it, inside
    // the workspace, which the window reads from the workspace itself.
    let moved = ["-C", source, "--unverified", "build", "--jobs=1"];
    leaves_its_line(&elsewhere, &moved, None);

    // Calls that compile and never claim: `run`, and an alias over it.
    let run = ["--unverified", "run", "-q"];
    let words: Vec<String> = run.iter().map(|word| (*word).to_owned()).collect();
    assert!(
        crate::build_admit::classify(&words, crate::build_cache::Tool::Targo).is_none(),
        "`run` claims nothing, and still leaves its line"
    );
    leaves_its_line(&front.source, &run, Some(&cache));
    std::fs::create_dir(front.source.join(".cargo")).unwrap();
    std::fs::write(
        front.source.join(".cargo/config.toml"),
        "[alias]\nhello = \"run -q\"\n",
    )
    .unwrap();
    let alias = ["--unverified", "hello"];
    let words: Vec<String> = alias.iter().map(|word| (*word).to_owned()).collect();
    assert!(
        crate::build_admit::classify(&words, crate::build_cache::Tool::Targo).is_none(),
        "an alias over `run` claims nothing"
    );
    let line = leaves_its_line(&front.source, &alias, Some(&cache));

    // Negative controls: each runs, and none leaves a line.
    for (cwd, words, session) in [
        (
            &front.source,
            &["--unverified", "metadata"][..],
            Some(tab.as_str()),
        ),
        (&front.source, &["--version"][..], Some(tab.as_str())),
        (&front.source, &build[..], None),
        (&front.source, &build[..], Some("x")),
        (
            &elsewhere,
            &["--unverified", "install", "x"][..],
            Some(tab.as_str()),
        ),
        (
            &elsewhere,
            &["--unverified", "build", "--jobs=1"][..],
            Some(tab.as_str()),
        ),
    ] {
        let out = front.run(cwd, words, session);
        assert!(started(&out, words), "{words:?} {session:?}: {out:?}");
        assert_eq!(front.starts().len(), count, "{words:?} {session:?}");
    }

    // The window's read: the newest line of the tab's one workspace.
    let read = read(&front.record());
    assert_eq!(read, [line]);
}

/// THE LINE'S BUILD FOLDER IS WHERE THE PLACEMENT PUT THE OUTPUT: a `--target-dir` build names
/// that target directory, not the quiet folder placed under it; the trust lane (`env`: the
/// shim exported the directory) names its target directory the same way; and a call the block
/// left as typed (`-`) names the build directory its own environment gave it. (A `-` call with
/// no such variable names none: the `-C` case of
/// `the_front_writes_one_start_line_for_a_build_in_a_tab`.)
#[cfg(target_os = "macos")]
#[test]
fn a_start_line_names_the_build_folder_its_placement_chose() {
    let front = Front::new();
    let tab = sid(0xb11d);
    let out_dir = front.home.join("out");
    let target = out_dir.to_str().unwrap();
    let quiet = out_dir.join(crate::build_cache::target_build_relative());
    let last = |words: &[&str], env: &[(&str, &Path)]| {
        let out = front.run_with(&front.source, words, Some(&tab), env);
        assert!(started(&out, words), "{words:?}: {out:?}");
        let line = front.starts().last().cloned().unwrap();
        assert_eq!(line.workspace, front.source, "{words:?}");
        line.build
    };

    let config = ["--unverified", "build", "--target-dir", target, "--jobs=1"];
    assert_eq!(last(&config, &[]), Some(out_dir.clone()), "config");
    let trust = ["trust", "build", "--target-dir", target];
    assert_eq!(
        last(&trust, &[(crate::build_cache::BUILD_DIR_ENV, &quiet)]),
        Some(out_dir.clone()),
        "env"
    );
    // A call whose own environment names its build directory, which the block leaves as typed:
    // a fresh folder, which the front admits with the workspace's default final target (an
    // intermediate-only selection needs complete custody of both).
    let own = front.home.join("own-intermediate.noindex");
    let build = ["--unverified", "build", "--jobs=1"];
    assert_eq!(
        last(&build, &[(crate::build_cache::BUILD_DIR_ENV, &own)]),
        Some(own.clone()),
        "-"
    );
    assert_eq!(front.starts().len(), 3);
}

/// A START LINE THAT CANNOT BE WRITTEN NEVER STOPS THE BUILD: with no state folder, with a
/// folder where the record would be, with a link there and with a FIFO there, the front
/// becomes the build with its words, says nothing of the record and writes no line through
/// any of them. NEGATIVE CONTROL: the same call with a plain state folder leaves its line, so
/// each arm did fail to write one.
#[cfg(target_os = "macos")]
#[test]
fn a_start_line_that_cannot_be_written_never_stops_the_build() {
    use std::os::unix::ffi::OsStrExt as _;
    let tab = sid(0xfeed);
    let build = ["--unverified", "build", "--jobs=1"];
    for arm in ["no-folder", "a-folder-in-its-place", "a-link", "a-fifo"] {
        let front = Front::new();
        let theirs = front.home.join("theirs");
        std::fs::write(&theirs, "").unwrap();
        match arm {
            "no-folder" => std::fs::remove_dir(&front.state).unwrap(),
            "a-folder-in-its-place" => std::fs::create_dir(front.record()).unwrap(),
            "a-link" => std::os::unix::fs::symlink(&theirs, front.record()).unwrap(),
            _ => {
                let path = std::ffi::CString::new(front.record().as_os_str().as_bytes()).unwrap();
                // SAFETY: a NUL-terminated owned scratch name; no pointer is retained.
                assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
            }
        }
        let out = front.run(&front.source, &build, Some(&tab));
        assert!(started(&out, &build), "{arm}: {out:?}");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(!stderr.contains("build-starts"), "{arm}: {stderr}");
        assert_eq!(
            std::fs::read(&theirs).unwrap(),
            b"",
            "{arm}: written through"
        );
        let left = std::fs::symlink_metadata(front.record());
        match arm {
            "no-folder" => assert!(left.is_err(), "{arm}: a record was left"),
            "a-folder-in-its-place" => assert_eq!(
                std::fs::read_dir(front.record()).unwrap().count(),
                0,
                "{arm}"
            ),
            "a-link" => assert!(left.unwrap().file_type().is_symlink(), "{arm}"),
            _ => {
                use std::os::unix::fs::FileTypeExt as _;
                assert!(left.unwrap().file_type().is_fifo(), "{arm}");
            }
        }
    }
    let front = Front::new();
    let out = front.run(&front.source, &build, Some(&tab));
    assert!(started(&out, &build), "{out:?}");
    assert_eq!(front.starts().len(), 1, "the control leaves its line");
}
