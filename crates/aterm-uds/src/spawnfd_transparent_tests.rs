// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for [`transparent`] descriptor inheritance, error parity, and signal/stdio behavior against `std::process::Command`.

use super::tests::{
    STAGE_ENV, STD_SPAWN_MUTE, park, run_staged_hup_ignored, translator, unflagged_at_or_above,
};
use super::*;
use std::path::PathBuf;

/// Decodes [`crate::host::held_probe`]'s exit status (`3` for held, `0` for clear).
fn answer(status: ExitStatus) -> bool {
    match status.code() {
        Some(3) => true,
        Some(0) => false,
        _ => panic!("the probe did not answer: {status:?}"),
    }
}

/// The shell that exits 3 when it holds `fd`, else 0.
fn holds(fd: RawFd) -> [String; 2] {
    ["-c".to_string(), test_support::held_probe(fd)]
}

/// A [`transparent`] child inherits unflagged descriptors above stdio and excludes `FD_CLOEXEC` ones, whereas [`command`] and [`spawn`] inherit only stdio.
#[test]
fn a_transparent_child_holds_every_descriptor_this_process_hands_on_and_nothing_flagged() {
    let unflagged = unflagged_at_or_above(100);
    let flagged = park(
        std::fs::File::open("/dev/null")
            .expect("open /dev/null")
            .into(),
        100,
    );
    let (fd, home) = (unflagged.as_raw_fd(), flagged.as_raw_fd());
    assert!(fd >= 100 && home >= 100);

    let status = |program: &str, fd| {
        transparent(program)
            .args(holds(fd))
            .status()
            .expect("transparent status")
    };
    assert!(answer(status("/bin/sh", fd)), "fd {fd}: by path");
    assert!(answer(status("sh", fd)), "fd {fd}: searched on PATH");
    let mut child = transparent("/bin/sh")
        .args(holds(fd))
        .spawn()
        .expect("transparent spawn");
    assert!(child.id() > 0);
    assert!(answer(child.wait().expect("wait")), "fd {fd}: spawn");
    assert!(
        !answer(status("/bin/sh", home)),
        "fd {home}: close-on-exec, it stays home"
    );

    assert!(
        !answer(command("/bin/sh").args(holds(fd)).status().expect("status")),
        "command: fd {fd} reached the child"
    );
    let mut lane = LaneCommand::new("/bin/sh");
    lane.args(holds(fd));
    assert!(
        !answer(spawn(lane).expect("spawn").wait().expect("wait")),
        "spawn: fd {fd} reached the child"
    );
    drop((unflagged, flagged));
}

/// [`transparent`] returns the same `io::ErrorKind` as `std::process::Command` on missing, non-executable, or NUL-containing program names.
#[test]
fn a_transparent_spawn_fails_as_std_fails() {
    let translator = translator();
    let mut mute = Vec::new();
    for program in [
        "/nonexistent/aterm-transparent",
        "aterm-transparent-no-such-program",
        "/",
        "nul\0byte",
    ] {
        let ours = transparent(program).status().map_err(|e| e.kind());
        let theirs = std::process::Command::new(program)
            .status()
            .map_err(|e| e.kind());
        assert!(ours.is_err(), "{program:?}: {ours:?}");
        if translator.is_some() && theirs.is_ok_and(|status| status.code() == Some(127)) {
            mute.push(format!("{program:?}"));
            continue;
        }
        assert_eq!(ours, theirs, "{program:?}");
    }
    if let Some(translator) = translator
        && !mute.is_empty()
    {
        println!(
            "NOT RUN under {translator}: spawnfd::transparent_tests::\
             a_transparent_spawn_fails_as_std_fails: std's error for {}: {STD_SPAWN_MUTE}",
            mute.join(", ")
        );
    }
    let mut command = transparent("prog");
    command.arg("a").args(["b", "c"]);
    assert_eq!(command.get_program(), "prog");
    assert!(command.get_args().eq(["a", "b", "c"]));
}

/// Stage name for [`a_transparent_run_is_the_std_run_it_replaces`] and [`transparent_mask_probe_stage`].
const SIGNALS_STAGE: &str = "transparent-signals";

/// Prints the process's initial signal mask as `MASK=<signals>` when [`STAGE_ENV`] is [`SIGNALS_STAGE`].
#[test]
fn transparent_mask_probe_stage() {
    if std::env::var(STAGE_ENV).as_deref() != Ok(SIGNALS_STAGE) {
        return;
    }
    println!("\nMASK={}", blocked());
}

/// `sigset_t`: a 32-bit mask on Darwin, 1024 bits on Linux.
#[cfg(target_vendor = "apple")]
type SigSet = u32;
#[cfg(not(target_vendor = "apple"))]
type SigSet = [u64; 16];

const SIGHUP: i32 = 1;
const SIGTERM: i32 = 15;
const SIGUSR1: i32 = if cfg!(target_vendor = "apple") {
    30
} else {
    10
};
const SIGUSR2: i32 = if cfg!(target_vendor = "apple") {
    31
} else {
    12
};
const SIG_BLOCK: i32 = if cfg!(target_vendor = "apple") { 1 } else { 0 };
const SIG_DFL: usize = 0;
const SIG_IGN: usize = 1;

unsafe extern "C" {
    fn signal(sig: i32, handler: usize) -> usize;
    fn pthread_sigmask(how: i32, set: *const SigSet, old: *mut SigSet) -> i32;
}

/// Whether signal `sig` is in `set`.
fn has(set: &SigSet, sig: i32) -> bool {
    let bit = u32::try_from(sig - 1).expect("a signal number");
    #[cfg(target_vendor = "apple")]
    {
        set & (1 << bit) != 0
    }
    #[cfg(not(target_vendor = "apple"))]
    {
        set[usize::try_from(bit / 64).expect("a word")] & (1 << (bit % 64)) != 0
    }
}

/// A set holding `signals`.
fn set_of(signals: &[i32]) -> SigSet {
    #[cfg(target_vendor = "apple")]
    let mut set: SigSet = 0;
    #[cfg(not(target_vendor = "apple"))]
    let mut set: SigSet = [0; 16];
    for &sig in signals {
        let bit = u32::try_from(sig - 1).expect("a signal number");
        #[cfg(target_vendor = "apple")]
        {
            set |= 1 << bit;
        }
        #[cfg(not(target_vendor = "apple"))]
        {
            set[usize::try_from(bit / 64).expect("a word")] |= 1 << (bit % 64);
        }
    }
    set
}

/// The signals this thread blocks, comma-separated.
fn blocked() -> String {
    let mut mask = set_of(&[]);
    // SAFETY: a null `set` only reads this thread's mask into a live local.
    assert_eq!(
        unsafe { pthread_sigmask(SIG_BLOCK, std::ptr::null(), &mut mask) },
        0
    );
    (1..=if cfg!(target_vendor = "apple") {
        32
    } else {
        64
    })
        .filter(|&sig| has(&mask, sig))
        .map(|sig| sig.to_string())
        .collect::<Vec<_>>()
        .join(",")
}

/// Runs `/bin/sh -c <script> sh <out>` via `std` or [`transparent`] and returns `(status, output)`.
fn sh_run(transparent_run: bool, script: &str, out: &std::path::Path) -> (ExitStatus, String) {
    let _ = std::fs::remove_file(out);
    let args = [
        "-c",
        script,
        "sh",
        out.to_str().expect("a UTF-8 scratch path"),
    ];
    let status = if transparent_run {
        transparent("/bin/sh").args(args).status()
    } else {
        std::process::Command::new("/bin/sh").args(args).status()
    }
    .expect("run /bin/sh");
    (status, std::fs::read_to_string(out).unwrap_or_default())
}

/// The staged half of [`a_transparent_run_is_the_std_run_it_replaces`].
fn staged_signals() {
    // SAFETY: `signal` with `SIG_IGN` or `SIG_DFL`, and a mask change for this
    // thread; each takes plain integers or a live local.
    unsafe {
        signal(SIGHUP, SIG_DFL);
        signal(SIGUSR2, SIG_IGN);
        let block = set_of(&[SIGUSR1, SIGTERM]);
        assert_eq!(pthread_sigmask(SIG_BLOCK, &block, std::ptr::null_mut()), 0);
    }
    let dir = std::env::temp_dir().join(format!("spawnfd-transparent-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let out: PathBuf = dir.join("out");
    let exe = std::env::current_exe().expect("the test binary");
    let exe = exe.to_str().expect("a UTF-8 test binary path");
    #[cfg(target_vendor = "apple")]
    let stat = "{ for n in 7 8 9; do /usr/bin/stat -f %d:%i <&$n; done; }";
    #[cfg(not(target_vendor = "apple"))]
    let stat = "stat -L -c %d:%i /dev/fd/7 /dev/fd/8 /dev/fd/9";
    let cases = [
        ("exit code", "exit 7".to_string()),
        ("death by signal", "kill -HUP $$".to_string()),
        (
            "stdio: the same three open files",
            format!("exec 7<&0 8>&1 9>&2; {stat} >\"$1\""),
        ),
        (
            "dispositions: SIGPIPE at its default, SIGUSR2 still ignored",
            "(yes 2>/dev/null; echo \"yes:$?\" >>\"$1\") | head -c1 >/dev/null; \
             kill -USR2 $$; echo survived >>\"$1\""
                .to_string(),
        ),
        (
            "the signal mask: the spawning thread's",
            format!(
                "exec '{exe}' --exact spawnfd::transparent_tests::transparent_mask_probe_stage \
                 --nocapture --test-threads=1 >\"$1\""
            ),
        ),
    ];
    let read = |what: &str, wrote: String| {
        if what.starts_with("the signal mask") {
            wrote
                .lines()
                .find(|line| line.starts_with("MASK="))
                .unwrap_or_else(|| panic!("the mask probe printed no mask: {wrote:?}"))
                .to_string()
        } else {
            wrote
        }
    };
    for (what, script) in &cases {
        let theirs = sh_run(false, script, &out);
        let theirs = (theirs.0, read(what, theirs.1));
        let ours = sh_run(true, script, &out);
        let ours = (ours.0, read(what, ours.1));
        println!("STAGED {what}: ours={ours:?} std={theirs:?}");
        assert_eq!(ours.0.into_raw(), theirs.0.into_raw(), "{what}: the status");
        assert_eq!(ours.1, theirs.1, "{what}: what it wrote");
    }
    let (status, _) = sh_run(true, &cases[0].1, &out);
    assert_eq!(status.code(), Some(7));
    let (status, _) = sh_run(true, &cases[1].1, &out);
    assert_eq!(status.signal(), Some(SIGHUP));
    let (_, stdio) = sh_run(true, &cases[2].1, &out);
    assert_eq!(stdio.lines().count(), 3, "{stdio:?}");
    let (_, dispositions) = sh_run(true, &cases[3].1, &out);
    assert_eq!(dispositions, "yes:141\nsurvived\n");
    let (_, mask) = sh_run(true, &cases[4].1, &out);
    let mut want = [SIGTERM, SIGUSR1];
    want.sort_unstable();
    let want = format!("MASK={},{}", want[0], want[1]);
    assert!(mask.lines().any(|line| line == want), "{want} in {mask:?}");
    let _ = std::fs::remove_dir_all(&dir);
    println!("STAGED transparent signals ok");
}

/// [`transparent`] matches `std::process::Command` across exit status, signal death, stdio descriptors, signal dispositions, and thread signal masks.
#[test]
fn a_transparent_run_is_the_std_run_it_replaces() {
    const NAME: &str = "spawnfd::transparent_tests::a_transparent_run_is_the_std_run_it_replaces";
    if std::env::var(STAGE_ENV).is_ok() {
        staged_signals();
        return;
    }
    let out = run_staged_hup_ignored(NAME, SIGNALS_STAGE);
    assert!(out.contains("STAGED transparent signals ok"), "{out}");
    print!("{out}");
}
