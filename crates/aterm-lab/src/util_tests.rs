// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_capacity::{Admit, Fixed, Ledger, Pace, Reading};
use atpkg::build_admit::BuildEnd;

const GIB: u64 = 1 << 30;

/// A seat on `dir`'s scratch ledger and records, on a 24 GiB machine, asked by `pid`.
fn seat<'m>(dir: &std::path::Path, m: &'m Fixed, pid: u32) -> atpkg::build_admit::Seat<'m> {
    atpkg::build_admit::Seat {
        ledger: Some(Ledger::at(dir.join("ledger"))),
        machine: m,
        own: Some(dir.join("targo-memory.json")),
        verify: Some(dir.join("stage-memory.json")),
        mode: Admit::Wait,
        pace: Pace::default(),
        pid,
        marker: None,
    }
}

/// A heavy build claims as a plain one, an alias as the build it is said to run, and a
/// claimed build that succeeded keeps its peak (here its reaped shell's floor, more than 0).
/// NEGATIVE CONTROL: the alias named as nothing else claims nothing, and a build that fails
/// with a peak below what it claimed keeps none.
#[test]
fn a_heavy_build_claims_and_keeps_its_peak_as_a_plain_build_does() {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let m = Fixed::new(Reading::of_gib(24, 20, 14));
    let s = seat(dir.path(), &m, std::process::id().wrapping_add(21));
    let records = || aterm_capacity::book::Records::load(&dir.path().join("targo-memory.json"));
    let dev_app = ["--unverified", "ship", "dev-app"];
    let like = ["--unverified", "build", "--release", "-p", "aterm"];
    let none = super::claim_build_at(&dev_app, None, dir.path(), &s, &mut |_| {});
    assert!(
        matches!(none, Ok(None)),
        "an alias claims nothing by itself"
    );
    let run = |script: &str| {
        let claim = super::claim_build_at(&dev_app, Some(&like), dir.path(), &s, &mut |_| {})
            .expect("admitted")
            .expect("claimed");
        assert_eq!(claim.claim().record().need.what, "targo build -p aterm");
        assert_eq!(claim.claim().record().need.mem, 12 * GIB, "cold");
        let mut cmd = aterm_uds::spawnfd::command("/bin/sh");
        cmd.args(["-c", script]);
        super::status_claimed(&mut cmd, Some(claim)).expect("ran")
    };
    assert_eq!(run("exit 1").code(), Some(1));
    assert!(
        records().rows().is_empty(),
        "a failed build below its claim keeps nothing"
    );
    assert!(run("exit 0").success());
    let kept = records();
    assert_eq!(kept.rows().len(), 1);
    assert_eq!(kept.rows()[0].key.variant.as_deref(), Some("release"));
    assert!(kept.rows()[0].peaks[0].bytes > 0);
}

/// A claimed lab build starts under its claim ([`super::spawn_claimed`]): a stand-in that
/// looks for its parent's claim (a file `<start>-<ppid>-<seq>.claim`) at its start finds it,
/// and is handed that claim's cover marker. NEGATIVE CONTROL (by hand: `spawn_claimed`
/// letting the claim go before it spawns): it finds none, and is handed no marker.
#[test]
fn a_claimed_lab_build_starts_under_its_claim() {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let m = Fixed::new(Reading::of_gib(24, 20, 14));
    let s = seat(dir.path(), &m, std::process::id().wrapping_add(31));
    let args = ["--unverified", "build", "-p", "aterm-lab", "--bin", "x"];
    let claim = super::claim_build_at(&args, None, dir.path(), &s, &mut |_| {})
        .expect("admitted")
        .expect("claimed");
    let ledger = dir.path().join("ledger");
    let marker = claim.claim().record().cover_marker();
    let mut cmd = aterm_uds::spawnfd::command("/bin/sh");
    cmd.args([
        "-c",
        &format!(
            "echo \"${{ATERM_CAPACITY_COVER-unset}}\"; \
             for f in '{}'/*-\"$PPID\"-*.claim; do [ -e \"$f\" ] && {{ echo held; exit 0; }}; \
             done; echo none",
            ledger.display()
        ),
    ])
    .env_remove(aterm_capacity::COVER_ENV)
    .stdout(aterm_uds::spawnfd::Stdio::piped());
    let (child, metered) = super::spawn_claimed(&mut cmd, Some(claim)).expect("spawned");
    let out = super::output_waited(child, metered).expect("its end");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        format!("{marker}\nheld\n")
    );
}

thread_local! {
    /// How [`super::Metered::end`] ended each claim this thread ended, in turn.
    pub(super) static ENDED: std::cell::RefCell<Vec<BuildEnd>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// This thread's claim ends since the last call.
fn ended() -> Vec<BuildEnd> {
    ENDED.with(|ended| std::mem::take(&mut *ended.borrow_mut()))
}

/// A claimed lab build's claim ends ([`super::Metered::end`], which writes the row of a claim
/// held in this user's own ledger) whatever its wait came to ([`super::wait_claimed`],
/// [`super::output_waited`]): a build this process lost leaves the wait with `ECHILD`, and its
/// claim ends as an end nothing is known of (`outcome=unknown`).
/// A debugger's attach is such a loss; here the test reaps the child behind its handle's back,
/// which leaves the handle's wait the same `ECHILD` (macOS hands out pids in turn, so the pid
/// names no other child by then). NEGATIVE CONTROL: the same build, kept, ends its claim with
/// the status its wait read.
#[test]
fn a_claimed_build_whose_wait_fails_still_ends_its_claim() {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let m = Fixed::new(Reading::of_gib(24, 20, 14));
    let s = seat(dir.path(), &m, std::process::id().wrapping_add(41));
    let args = ["--unverified", "build", "-p", "aterm-lab", "--bin", "x"];
    let start = |piped: bool| {
        let claim = super::claim_build_at(&args, None, dir.path(), &s, &mut |_| {})
            .expect("admitted")
            .expect("claimed");
        let mut cmd = aterm_uds::spawnfd::command("/bin/sh");
        cmd.args(["-c", "echo out; echo err >&2; exit 3"]);
        if piped {
            cmd.stdout(aterm_uds::spawnfd::Stdio::piped())
                .stderr(aterm_uds::spawnfd::Stdio::piped());
        } else {
            cmd.stdout(aterm_uds::spawnfd::Stdio::null())
                .stderr(aterm_uds::spawnfd::Stdio::null());
        }
        super::spawn_claimed(&mut cmd, Some(claim)).expect("spawned")
    };
    let lose = |child: &aterm_uds::spawnfd::Child| {
        let pid = i32::try_from(child.id()).expect("a pid");
        let mut raw = 0;
        // SAFETY: `raw` is a live local `waitpid` writes; the pid is this test's own child.
        #[expect(
            unsafe_code,
            reason = "waitpid(2): reap the child behind its handle's back"
        )]
        let reaped = unsafe { libc::waitpid(pid, &raw mut raw, 0) };
        assert_eq!(reaped, pid);
    };
    let exit_3 = |end: &BuildEnd| matches!(end, BuildEnd::Status(s) if s.code() == Some(3));
    // Ends an earlier test left on this thread (libtest runs a test on its own thread when it
    // can spawn none) are not this test's.
    ended();

    let (mut child, metered) = start(false);
    assert_eq!(
        super::wait_claimed(&mut child, metered)
            .expect("kept")
            .code(),
        Some(3)
    );
    let (child, metered) = start(true);
    let out = super::output_waited(child, metered).expect("kept");
    assert_eq!(
        (out.status.code(), &out.stdout[..]),
        (Some(3), &b"out\n"[..])
    );
    let kept = ended();
    assert!(kept.len() == 2 && kept.iter().all(exit_3), "{kept:?}");

    let (mut child, metered) = start(false);
    lose(&child);
    let waited = super::wait_claimed(&mut child, metered).expect_err("lost");
    assert_eq!(waited.raw_os_error(), Some(libc::ECHILD), "{waited}");
    let (child, metered) = start(true);
    lose(&child);
    let waited = super::output_waited(child, metered).expect_err("lost");
    assert_eq!(waited.raw_os_error(), Some(libc::ECHILD), "{waited}");
    assert_eq!(ended(), [BuildEnd::Unknown, BuildEnd::Unknown]);
}

/// A lab process that holds a claim of its own (a lane's VM) builds unclaimed and says so: a
/// second claim would wait on its own. NEGATIVE CONTROL: once it lets go, the build claims.
#[test]
fn a_lab_holding_a_claim_of_its_own_builds_unclaimed() {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let m = Fixed::new(Reading::of_gib(24, 20, 14));
    let me = std::process::id();
    let vm = aterm_capacity::admit_with(
        &Ledger::at(dir.path().join("ledger")),
        &m,
        aterm_capacity::Need::new("lab vm boot").mem(4 * GIB),
        Admit::Wait,
        Pace::default(),
        &mut |_| {},
    )
    .expect("the VM's claim");
    let s = seat(dir.path(), &m, me);
    let args = ["--unverified", "build", "-p", "aterm-lab", "--bin", "x"];
    let mut said = Vec::new();
    let got = super::claim_build_at(&args, None, dir.path(), &s, &mut |l| {
        said.push(l.to_owned());
    });
    assert!(matches!(got, Ok(None)), "{got:?}");
    assert!(said.iter().any(|l| l.contains("lab vm boot")), "{said:?}");
    drop(vm);
    let got = super::claim_build_at(&args, None, dir.path(), &s, &mut |_| {});
    assert!(matches!(got, Ok(Some(_))), "{got:?}");
}

#[test]
fn a_nested_targo_starts_at_the_checkout_without_what_cargo_said_about_the_lab() {
    // Inject the inherited names: a runner need not provide Cargo's build-script OUT_DIR.
    let names = [
        "CARGO_PKG_NAME",
        "CARGO_MANIFEST_DIR",
        "OUT_DIR",
        "PATH",
        "HOME",
    ];
    let mut cmd = super::targo(&["--unverified", "build"]);
    for name in names {
        cmd.env(name, "inherited");
    }
    super::clear_cargo_run_env(&mut cmd, names.map(std::ffi::OsString::from));
    let removed: Vec<String> = cmd
        .get_envs()
        .filter(|(_, v)| v.is_none())
        .map(|(k, _)| k.to_string_lossy().into_owned())
        .collect();
    for name in ["CARGO_PKG_NAME", "CARGO_MANIFEST_DIR", "OUT_DIR"] {
        assert!(removed.iter().any(|r| r == name), "{name} in {removed:?}");
    }
    assert!(
        !removed.iter().any(|r| r == "PATH" || r == "HOME"),
        "{removed:?}"
    );
    assert_eq!(
        cmd.get_current_dir(),
        Some(crate::paths::paths().repo.as_path())
    );
    let args: Vec<_> = cmd
        .get_args()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let verb = args
        .iter()
        .position(|a| a == "build")
        .expect("build command");
    assert!(args[..verb].iter().any(|a| a == "--unverified"), "{args:?}");
}

#[test]
fn archived_scratch_files_keep_times_but_refuse_other_names() {
    use std::os::unix::fs::{PermissionsExt as _, symlink};

    let d = crate::TestScratch::new("archive");
    let input = d.join("scratch");
    let output = d.join("archive");
    std::fs::create_dir_all(input.join("nested")).unwrap();
    let normal = input.join("nested/normal");
    std::fs::write(&normal, "keep this log").unwrap();
    std::fs::set_permissions(&normal, std::fs::Permissions::from_mode(0o640)).unwrap();
    let modified = UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
    std::fs::File::options()
        .write(true)
        .open(&normal)
        .unwrap()
        .set_modified(modified)
        .unwrap();
    let outside = d.join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("private"), "do not archive").unwrap();
    std::fs::hard_link(outside.join("private"), input.join("hard")).unwrap();
    symlink(outside.join("private"), input.join("linked")).unwrap();
    symlink(&outside, input.join("linked-dir")).unwrap();

    keep_tree(&input, &output);
    let copied = output.join("nested/normal");
    assert_eq!(std::fs::read_to_string(&copied).unwrap(), "keep this log");
    let meta = std::fs::metadata(&copied).unwrap();
    assert_eq!(meta.modified().unwrap(), modified);
    assert_eq!(meta.permissions().mode() & 0o777, 0o640);
    assert!(
        !output.join("hard").exists(),
        "a second name is not a lab log"
    );
    assert!(!output.join("linked").exists());
    assert!(!output.join("linked-dir").exists());

    keep_file(
        &input.join("linked-dir/private"),
        &output.join("via-parent"),
    );
    assert!(
        !output.join("via-parent").exists(),
        "no link anywhere on the source path"
    );
    assert_eq!(
        std::fs::read_to_string(outside.join("private")).unwrap(),
        "do not archive"
    );
}

#[test]
fn utc_stamps_are_the_calendar_day() {
    assert_eq!(super::utc_stamp(0), "1970-01-01T00:00:00.000000Z");
    assert_eq!(
        super::utc_stamp(1_792_091_823),
        "2026-10-15T19:17:03.000000Z"
    );
    assert_eq!(super::utc_stamp(951_782_400), "2000-02-29T00:00:00.000000Z");
}

#[test]
fn base64url_round_trips_without_padding() {
    for s in [
        "",
        "f",
        "fo",
        "foo",
        "foob",
        "fooba",
        "foobar",
        "{\"lab\":\"A\"}",
    ] {
        let e = super::b64url(s.as_bytes());
        assert!(!e.contains('='), "{e}");
        assert_eq!(
            super::b64url_decode(&e).as_deref(),
            Some(s.as_bytes()),
            "{s}"
        );
    }
    assert_eq!(super::b64url(b"\xfb\xff"), "-_8");
    assert_eq!(super::b64url_decode("Zg==").as_deref(), Some(&b"f"[..]));
    assert_eq!(super::b64url_decode("Zm8=").as_deref(), Some(&b"fo"[..]));
    for bad in [
        "not base64!",
        "A",
        "ABCDE",
        "Zh",
        "Zm9",
        "Zg=",
        "Zm8==",
        "===",
    ] {
        assert_eq!(super::b64url_decode(bad), None, "{bad:?}");
    }
}

use super::*;

#[test]
fn one_line_folds_control_runs_and_cuts_long_text() {
    assert_eq!(one_line("  a\n\n\tb  ", 70), "a b");
    assert_eq!(one_line("abcdef", 4), "abc~");
    assert_eq!(one_line("abcd", 4), "abcd");
}

#[test]
fn shell_words_round_trip_through_quote_and_unquote() {
    for s in [
        "plain",
        "with space",
        "it's",
        "/Users//x/Library/Application Support/a b",
        "",
    ] {
        assert_eq!(shell_unquote(&shell_quote(s)), s, "{s:?}");
    }
    assert_eq!(
        shell_unquote(r"/a/Application\ Support/b"),
        "/a/Application Support/b"
    );
    assert_eq!(shell_unquote(r"$'a\nb'"), "a\nb");
    assert_eq!(shell_unquote(r"$'a\x20\x09\x7fb'"), "a \t\x7fb");
    assert_eq!(
        shell_prepend_path("/s/runs/t/shim"),
        "\"/s/runs/t/shim:$PATH\""
    );
    assert_eq!(
        shell_prepend_path(r#"/s/"a"$b`c`\d/shim"#),
        r#""/s/\"a\"\$b\`c\`\\d/shim:$PATH""#
    );
    assert_eq!(
        shell_prepend_path("/s/a\nb/shim"),
        "'/s/a\nb/shim':\"$PATH\""
    );
}

#[test]
fn py_repr_picks_the_quote_python_picks() {
    assert_eq!(py_repr("hi"), "'hi'");
    assert_eq!(py_repr("it's"), "\"it's\"");
    assert_eq!(py_repr("a\nb"), "'a\\nb'");
    assert_eq!(py_repr("❯\u{a0}hi\u{80}\u{2028}"), "'❯\\xa0hi\\x80\\u2028'");
}

/// An executable past the lab's ordinary limit reads whole, as Claude Code's and a debug
/// aterm are. NEGATIVE CONTROL: the ordinary read refuses that file, and a file past the
/// program limit is refused before a byte is read.
#[test]
fn read_program_takes_what_the_ordinary_limit_refuses() {
    let dir = crate::TestScratch::new("prog");
    let file = dir.join("program");
    let sparse = |len| {
        std::fs::File::create(&file).unwrap().set_len(len).unwrap();
    };
    sparse(MAX_LAB_FILE_BYTES + 1);
    assert!(read_regular_file(&file).is_err());
    assert_eq!(
        read_program(&file).unwrap().len() as u64,
        MAX_LAB_FILE_BYTES + 1
    );
    sparse(MAX_LAB_PROGRAM_BYTES + 1);
    assert!(read_program(&file).is_err());
}

#[test]
fn read_regular_file_and_text_refuse_non_regular_entries() {
    let dir = crate::TestScratch::new("regread");
    let file = dir.join("ok.txt");
    std::fs::write(&file, "hello\n").unwrap();
    assert_eq!(read_regular_text(&file).unwrap(), "hello\n");
    assert_eq!(read_regular_file(&file).unwrap(), b"hello\n");
    assert_eq!(read_artifact(&file).unwrap(), b"hello\n");
    let linked = dir.join("linked.txt");
    std::os::unix::fs::symlink(&file, &linked).unwrap();
    assert_eq!(read_regular_text(&linked).unwrap(), "hello\n");
    assert!(read_artifact(&linked).is_err());
    let err = read_regular_file(&dir).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
    let fifo = dir.join("fifo");
    assert!(
        aterm_uds::spawnfd::command("/usr/bin/mkfifo")
            .arg(&fifo)
            .status()
            .unwrap()
            .success()
    );
    let err = read_regular_file(&fifo).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
}

#[test]
fn line_tail_cursor_reads_deltas_holds_partial_lines_and_resets_on_replace() {
    use std::io::Write as _;
    let dir = crate::TestScratch::new("tail");
    let file = dir.join("journal.jsonl");
    let mut cursor = LineTailCursor::default();
    assert_eq!(
        cursor.poll_scratch_text(&file).unwrap(),
        (false, String::new())
    );

    std::fs::write(&file, "line-1\npartial").unwrap();
    assert_eq!(
        cursor.poll_scratch_text(&file).unwrap(),
        (true, "line-1\n".to_string())
    );
    // Idle poll with only the un-terminated trailing line reads 0 complete lines.
    assert_eq!(
        cursor.poll_scratch_text(&file).unwrap(),
        (false, String::new())
    );

    // Completing the partial line + adding a second line yields both without re-reading line-1.
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(&file)
        .unwrap();
    f.write_all(b"-2\nline-3\n").unwrap();
    drop(f);
    assert_eq!(
        cursor.poll_scratch_text(&file).unwrap(),
        (false, "partial-2\nline-3\n".to_string())
    );
    assert_eq!(
        cursor.poll_scratch_text(&file).unwrap(),
        (false, String::new())
    );

    // Atomic replacement (new inode / shorter length) triggers reset = true.
    atomic_write(&file, b"replaced-1\n").unwrap();
    assert_eq!(
        cursor.poll_scratch_text(&file).unwrap(),
        (true, "replaced-1\n".to_string())
    );
}
