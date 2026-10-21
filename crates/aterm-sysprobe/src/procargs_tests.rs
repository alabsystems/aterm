// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The `KERN_PROCARGS2` layout's edge cases, once for every reader of it, on
//! buffers laid out as the kernel lays them; and the live read of this process.

use super::*;

/// A `KERN_PROCARGS2` answer as the kernel lays one out: `argc`, the exec path
/// and its NUL, `pad` more NULs, the words, the environment.
fn frame(argc: i32, exec: &[u8], pad: usize, words: &[&[u8]], env: &[&[u8]]) -> Vec<u8> {
    let mut b = argc.to_ne_bytes().to_vec();
    b.extend_from_slice(exec);
    b.extend(std::iter::repeat_n(0u8, pad + 1));
    for w in words.iter().chain(env) {
        b.extend_from_slice(w);
        b.push(0);
    }
    b
}

fn env_of(p: &ProcArgs2<'_>) -> Vec<Vec<u8>> {
    p.env().map(<[u8]>::to_vec).collect()
}

/// A whole answer: the exec path apart from `argv[0]`, every word (a later empty one and
/// one that is no UTF-8 kept), and the environment after them up to its empty string.
#[test]
fn a_whole_answer_gives_the_path_the_words_and_the_environment() {
    let words: &[&[u8]] = &[b"-zsh", b"-c", b"a\nb", b"", b"\xff"];
    let mut b = frame(5, b"/bin/zsh", 6, words, &[b"HOME=/h", b"K=v"]);
    b.extend_from_slice(b"\0executable_path=/bin/zsh\0");
    let p = parse(&b).expect("a whole answer");
    assert_eq!(p.exec_path, b"/bin/zsh");
    assert_eq!((p.argc, p.argv.as_slice(), p.whole()), (5, words, true));
    assert_eq!(env_of(&p), [b"HOME=/h".to_vec(), b"K=v".to_vec()]);
}

/// The padding after the exec path is any number of NULs, none included, and an empty
/// `argv[0]` is read as more of it: the first word is the first non-empty one.
#[test]
fn the_padding_is_skipped_whatever_its_length() {
    for pad in [0, 1, 3, 7] {
        let b = frame(2, b"/x", pad, &[b"x", b"y"], &[b"A=1"]);
        let p = parse(&b).expect("parses");
        assert_eq!(p.argv, [b"x".as_slice(), b"y"], "pad {pad}");
        assert_eq!(env_of(&p), [b"A=1".to_vec()], "pad {pad}");
    }
    let b = frame(2, b"/x", 2, &[b"", b"y"], &[]);
    let p = parse(&b).expect("parses");
    assert_eq!(p.argv, [b"y".as_slice()], "an empty argv[0] is padding");
    assert!(!p.whole());
}

/// An argc of 0 is no words, whole, and the environment follows the padding; the exec
/// path may be empty.
#[test]
fn an_argc_of_zero_reads_no_words_and_the_environment() {
    let b = frame(0, b"", 3, &[], &[b"A=1"]);
    let p = parse(&b).expect("parses");
    assert_eq!(
        (p.exec_path, p.argv.len(), p.whole()),
        (b"".as_slice(), 0, true)
    );
    assert_eq!(env_of(&p), [b"A=1".to_vec()]);
}

/// A buffer cut inside its words answers the words it holds, terminated ones only, and no
/// environment; a count no buffer of its size could hold reserves nothing and reads as cut.
#[test]
fn a_cut_buffer_answers_the_words_it_holds_and_no_environment() {
    let mut cut = frame(3, b"/bin/sleep", 1, &[b"sleep"], &[]);
    cut.extend_from_slice(b"3");
    let p = parse(&cut).expect("parses");
    assert_eq!(
        (p.argv.as_slice(), p.whole()),
        ([b"sleep".as_slice()].as_slice(), false)
    );
    assert_eq!(p.env().count(), 0);
    let lie = frame(i32::MAX, b"/x", 0, &[b"a"], &[b"K=v"]);
    let p = parse(&lie).expect("parses");
    assert_eq!(p.argv, [b"a".as_slice(), b"K=v"]);
    assert!(!p.whole());
    assert!(p.argv.capacity() <= 1024, "reserved {}", p.argv.capacity());
}

/// No argc, a negative one, or an exec path no NUL ends: no answer.
#[test]
fn a_buffer_without_its_header_is_no_answer() {
    assert_eq!(parse(&[1, 0, 0]), None, "shorter than argc");
    assert_eq!(
        parse(&frame(-1, b"/x", 0, &[b"x"], &[])),
        None,
        "negative argc"
    );
    let mut open = 1i32.to_ne_bytes().to_vec();
    open.extend_from_slice(b"/bin/x");
    assert_eq!(parse(&open), None, "an exec path never ended");
}

/// The live read of this test process: its argument words are the ones std was handed,
/// whole, and a pid nobody holds reads as nothing.
#[cfg(target_os = "macos")]
#[test]
fn this_process_reads_back_as_it_was_exec_d() {
    use std::os::unix::ffi::OsStrExt as _;
    let buf = read(std::process::id()).expect("this process's own answer");
    let p = parse(&buf).expect("parses");
    let mine: Vec<Vec<u8>> = std::env::args_os().map(|a| a.as_bytes().to_vec()).collect();
    let read: Vec<Vec<u8>> = p.argv.iter().map(|w| w.to_vec()).collect();
    assert!(p.whole());
    assert_eq!(read, mine);
    assert_eq!(super::read(0), None, "pid 0");
    assert_eq!(super::read(99_999_998), None, "no such pid");
}

/// A record larger than the kernel hands over is no answer, never its tail. An exec given
/// nearly `ARG_MAX` of arguments and environment has a record past `kern.argmax` (its exec
/// path and the kernel's own strings ride beside them), and the kernel refuses a buffer past
/// `kern.argmax` plus the argc (`EINVAL`) and fills a smaller one with the record's LAST bytes:
/// the exec path and the first words are gone, and what follows the argc parses as other
/// words (measured 2026-10-20: `ps` prints such a process as `ptr_munge= ptr_munge=`).
///
/// The record is this test binary's own, re-run ALONE with an environment that fills
/// `ARG_MAX`; the stage proves the record is larger than any buffer the kernel takes.
#[cfg(target_os = "macos")]
#[test]
fn a_record_past_what_the_kernel_hands_over_reads_as_nothing() {
    const NAME: &str = "procargs::tests::a_record_past_what_the_kernel_hands_over_reads_as_nothing";
    const STAGE: &str = "ATERM_UDS_PROCARGS_PAST";
    const DONE: &str = "ALONE a record past the buffer read as nothing";
    let argmax = {
        let mut n: libc::c_int = 0;
        let mut len = std::mem::size_of::<libc::c_int>();
        let mut mib = [libc::CTL_KERN, KERN_ARGMAX];
        // SAFETY: a two-integer key; `n` and `len` are ours; nothing is written.
        let rc = unsafe {
            libc::sysctl(
                mib.as_mut_ptr(),
                2,
                (&raw mut n).cast(),
                &raw mut len,
                std::ptr::null_mut(),
                0,
            )
        };
        assert_eq!(rc, 0, "kern.argmax");
        usize::try_from(n).expect("kern.argmax")
    };
    if std::env::var_os(STAGE).is_none() {
        let exe = std::env::current_exe().expect("the test binary");
        let args = ["--exact", NAME, "--nocapture", "--test-threads=1"];
        let stage = format!("{STAGE}=1");
        // What the spawn counts against `ARG_MAX`: every argument and environment string
        // with its NUL (`argv[0]`, the path, first) and the exec path once more (measured:
        // counting the path once, a filler 16 bytes short is `E2BIG`). The filler takes all
        // but 64 bytes; the kernel's own strings (`executable_path=…`) take the record past
        // what the kernel hands over.
        let path = exe.as_os_str().len() + 1;
        let taken = 2 * path
            + args.iter().map(|a| a.len() + 1).sum::<usize>()
            + stage.len()
            + 1
            + "FILL=".len()
            + 1;
        let fill = "f".repeat(argmax - 64 - taken);
        let out = aterm_uds::deadline::output_until(
            aterm_uds::spawnfd::command(&exe)
                .args(args)
                .env_clear()
                .env(STAGE, "1")
                .env("FILL", fill),
            std::time::Instant::now() + std::time::Duration::from_secs(60),
        )
        .expect("the test, alone, within the hang detector");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success() && stdout.contains(DONE),
            "{:?}\nstdout: {stdout}\nstderr: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
        return;
    }
    let me = libc::c_int::try_from(std::process::id()).expect("pid");
    let mut whole = 0usize;
    let mut mib = [libc::CTL_KERN, KERN_PROCARGS2, me];
    // SAFETY: a size query (null buffer) on a three-integer key; `whole` is ours.
    let rc = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            3,
            std::ptr::null_mut(),
            &raw mut whole,
            std::ptr::null_mut(),
            0,
        )
    };
    assert_eq!(rc, 0, "the record's size");
    assert!(
        whole > argmax + 4,
        "the record ({whole} bytes) must be past what the kernel hands over ({argmax} + 4)"
    );
    let got = read(std::process::id());
    assert_eq!(
        got.as_deref()
            .map(|b| parse(b).map(|p| String::from_utf8_lossy(p.exec_path).into_owned())),
        None,
        "{} bytes read of a {whole}-byte record",
        got.as_ref().map_or(0, Vec::len)
    );
    println!("{DONE}");
}
