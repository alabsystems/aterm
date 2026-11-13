// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn parse_reads_ps_lines_and_refuses_anything_else() {
    assert_eq!(parse("    1   -\n"), Some((1, false)));
    assert_eq!(parse("   95   C\n"), Some((95, true)));
    assert_eq!(parse("  512   T\n"), Some((512, true)));
    assert_eq!(parse("  512   CT\n"), Some((512, true)));
    assert_eq!(parse(""), None);
    assert_eq!(parse("WQ WQL\n 1 -\n"), None);
    assert_eq!(parse(" 1 -\n 2 -\n"), None);
    assert_eq!(parse(" x -\n"), None);
    assert_eq!(parse(" +1 -\n"), None);
    assert_eq!(parse(" 1 ?\n"), None);
    assert_eq!(parse(" 1\n"), None);
    assert_eq!(parse(" 1 - 3\n"), None);
    // What `ps` prints, exit 0, when the kernel refuses the pool's read: unread, never a pool.
    assert_eq!(parse("   -   -\n"), None);
}

/// `/bin/ps`, the program [`pool_ps`] runs, is root's and setuid. A `ps` without that bit
/// prints `- -` and exits 0 for `syspolicyd` on every run (probed 2026-11-03 with an ad-hoc
/// signed copy, which drops the bit), so every [`look`] would read [`Look::Unread`] while
/// the live test below, which takes `- -` as the kernel's passing refusal, still passed.
#[cfg(target_os = "macos")]
#[test]
fn bin_ps_is_setuid_root() {
    use std::os::unix::fs::MetadataExt as _;
    let ps = std::fs::metadata("/bin/ps").expect("/bin/ps");
    assert_eq!(ps.uid(), 0, "/bin/ps is root's");
    assert_ne!(ps.mode() & 0o4000, 0, "/bin/ps is setuid: {:o}", ps.mode());
}

/// One look as [`look`] makes it ([`look_and_ps`], under this test's hang detector), against
/// the `ps` that look ran. The live daemon reads as a root `syspolicyd` from its image, with
/// the kernel's limit beside it, and its one [`pool_output`] exits 0 with one line: a pool,
/// which the look reads as that daemon's [`Reading`], or `- -`, which it reads as
/// [`Look::Unread`]. Which of the two is not asserted: the kernel's read of another process's
/// workqueue sometimes refuses, more often while that pool is busy (measured 2026-10-29 under
/// load), and `ps` then prints `- -` and exits 0. A `ps` that can never read the pool prints
/// `- -` too, which `bin_ps_is_setuid_root` guards against; a `ps` that lost a keyword or ran
/// on a pid that runs no process exits 1, a `ps` that did not end within the hang detector
/// leaves no output, and both fail here. A failed `ps` passes only when the daemon no longer
/// reads as the one it ran on: that `syspolicyd` ended, so its pid names no process.
#[cfg(target_os = "macos")]
#[test]
fn a_live_look_reads_syspolicyd_from_its_image_and_its_ps_as_a_pool_or_unread() {
    let (look, run) = look_and_ps(Duration::from_secs(60));
    let Some((seen, out)) = run else {
        assert_eq!(look, Look::NoneRunning, "the process table reads");
        return;
    };
    assert!(seen.pid > 1);
    let limit = aterm_sysprobe::workqueue_limits().constrained;
    assert!(limit.is_some_and(|l| l >= 64), "{limit:?}");

    let out = out.expect("ps started and ended within the hang detector");
    let stdout = String::from_utf8_lossy(&out.stdout);
    if !out.status.success() {
        assert_eq!(look, Look::Unread, "a failed ps reads as nothing");
        assert_ne!(
            running(),
            Ok(seen),
            "ps failed on the running syspolicyd ({}): {stdout:?} {:?}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
        return;
    }
    let lines: Vec<&str> = stdout.lines().filter(|l| !l.trim().is_empty()).collect();
    let &[line] = lines.as_slice() else {
        panic!("ps prints one pool line: {stdout:?}");
    };
    match parse(line) {
        Some((workers, at_limit)) => {
            let Look::Read(reading) = look else {
                panic!("a pool line reads as the pool: {line:?} {look:?}");
            };
            assert_eq!(
                (reading.daemon, reading.workers, reading.at_limit),
                (seen, workers, at_limit),
                "the pool of the daemon ps ran on: {line:?}"
            );
            assert!(reading.limit.is_some_and(|l| l >= 64), "{reading:?}");
        }
        None => {
            assert!(
                line.split_whitespace().eq(["-", "-"]),
                "a pool or the refusal: {line:?}"
            );
            assert_eq!(look, Look::Unread, "the refusal reads as nothing");
        }
    }
}
