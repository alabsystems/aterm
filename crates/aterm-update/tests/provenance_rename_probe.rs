// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! WHICH filesystem operation tags a `.app` bundle — the measurement behind the
//! updater's untracked-lane rules.
//!
//! `com.apple.provenance` on a bundle's ROOT DIRECTORY is what makes every process
//! LaunchServices launches from it provenance-tracked, even when the executable and
//! every file inside are clean (measured 2026-09-19 on m16: the live
//! `/Applications/aterm.app` had a clean `Contents/MacOS/aterm` and a tagged root, and
//! the launchd job that exec'd that binary wrote a TAGGED file; a byte-identical copy
//! with the root tag stripped wrote a clean one, and so did the same copy with the three
//! shipped-tagged `ShellIntegration` scripts restored — so the scripts are not the
//! carrier and the root is).
//!
//! That makes "which operation tags the root" the question the updater's design turns
//! on, because the updater is a tracked process that moves a bundle into place. This
//! probe answers it for the two operations it uses: a plain `rename(2)` and the APFS
//! `renamex_np(RENAME_SWAP)`.
//!
//! IGNORED by default and macOS-only: it needs `launchctl` to mint the CLEAN inputs (a
//! tracked test process tags everything it creates, so the subject has to be built by an
//! untracked job first), and it measures the host rather than this crate. Run it
//! deliberately:
//!
//! ```text
//! targo --unverified test -p aterm-update --test provenance_rename_probe -- --ignored --nocapture
//! ```

//! # What a fix may NOT use, measured the hard way (2026-09-20)
//!
//! The obvious repair — hand the plain moves to a launchd job running `/bin/mv` — was
//! written, reviewed and REJECTED before it shipped, for reasons this probe's own
//! result explains. `mv(1)` is not `rename(2)`: across file systems it is
//! `rm -f destination && cp -pRP source destination && rm -rf source` (its manual says
//! so), and three of this updater's restore paths are documented as relying on
//! `rename(2)`'s INSTANT `EXDEV` failure — so on an external-volume install that
//! substitution would delete the destination and start a several-hundred-megabyte copy
//! where the code expects an immediate error, and a failure mid-copy leaves neither
//! bundle. A launchd job also outlives the parent's deadline, so a timed-out move can
//! still land on the real destination after the caller has given up and cleaned up.
//!
//! A correct fix therefore has to: take the launchd lane only when `same_volume` already
//! holds (leaving `EXDEV` to fail fast as today); reuse the hardened runner the `ditto`
//! lane already has (nonce label, the `mkdir "$st.once"` one-shot claim, bounded
//! `launchctl` calls, budget accounting, sidecar cleanup) rather than a second weaker
//! copy of it; have the JOB re-check the destination it is about to overwrite; and also
//! cover the promote rename in `copy_into_isolated_destination`, which is an eighth
//! carrier on the DMG and cross-volume lanes.

#![cfg(target_os = "macos")]

use std::path::{Path, PathBuf};
use std::process::Command;

/// Does `path` carry `com.apple.provenance`?
///
/// PANICS rather than answering on a failed read. "Could not look" and "clean" are the
/// same value in every convenient predicate, and a probe whose whole job is to measure
/// the difference must not quietly adopt the collapse it exists to expose: an `xattr`
/// that exits non-zero — a missing path, a permission error — would otherwise make this
/// whole measurement pass vacuously.
fn tagged(path: &Path) -> bool {
    let out = Command::new("/usr/bin/xattr")
        .arg("-l")
        .arg(path)
        .output()
        .expect("xattr runs");
    assert!(
        out.status.success(),
        "xattr could not inspect {} ({}): {}",
        path.display(),
        out.status,
        String::from_utf8_lossy(&out.stderr).trim()
    );
    String::from_utf8_lossy(&out.stdout).contains("com.apple.provenance")
}

/// Build the subject through an UNTRACKED launchd job, so it starts clean whatever this
/// test process is. Returns once the job has answered.
fn launchd_sh(script: &str, args: &[&Path], done: &Path) {
    let label = format!("systems.alab.probe.rename-{}", std::process::id());
    let _ = std::fs::remove_file(done);
    let mut cmd = Command::new("/bin/launchctl");
    cmd.arg("submit")
        .args(["-l", &label])
        .arg("--")
        .arg("/bin/sh")
        .arg("-c")
        .arg(script)
        .arg("probe");
    for a in args {
        cmd.arg(a);
    }
    let out = cmd.output().expect("launchctl runs");
    assert!(
        out.status.success(),
        "launchctl submit: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let started = std::time::Instant::now();
    while !done.exists() {
        assert!(
            started.elapsed() < std::time::Duration::from_secs(60),
            "the launchd job did not finish"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let _ = Command::new("/bin/launchctl")
        .args(["remove", &label])
        .output();
}

/// `renamex_np(RENAME_SWAP)` — the same call the updater's `sys::rename_swap` makes,
/// spelled here because that module is crate-private and a probe is not a reason to
/// widen it.
fn rename_swap(a: &Path, b: &Path) -> std::io::Result<()> {
    use std::os::unix::ffi::OsStrExt as _;
    let ca = std::ffi::CString::new(a.as_os_str().as_bytes()).expect("no NUL");
    let cb = std::ffi::CString::new(b.as_os_str().as_bytes()).expect("no NUL");
    // SAFETY: both C strings outlive the call; RENAME_SWAP is the documented flag.
    let rc = unsafe { libc::renamex_np(ca.as_ptr(), cb.as_ptr(), libc::RENAME_SWAP) };
    if rc != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

fn scratch() -> PathBuf {
    let d = std::env::temp_dir().join(format!("aterm-rename-probe-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).expect("scratch");
    d
}

/// THE MEASUREMENT. A tracked process moves a clean directory; which inode ends up
/// tagged decides where the updater must hand the work to an untracked job.
#[test]
#[ignore = "measures the host's provenance behaviour; needs launchctl"]
fn which_move_tags_a_clean_directory() {
    let d = scratch();
    // Both subjects are made by an untracked job, so they start clean even though this
    // test process is (almost certainly) tracked.
    launchd_sh(
        // `/bin/sh -c <script> <name> <a> <b>` makes `$0` the name, so the scratch dir
        // is `$1` and the done-marker `$2`.
        "mkdir -p \"$1/a/Contents\" \"$1/b/Contents\" \"$1/plain\" && \
         : > \"$1/a/Contents/f\" && : > \"$1/b/Contents/f\" && : > \"$1/plain/f\" && : > \"$2\"",
        &[&d, &d.join("made")],
        &d.join("made"),
    );
    let (a, b, plain) = (d.join("a"), d.join("b"), d.join("plain"));
    for p in [&a, &b, &plain] {
        assert!(
            !tagged(p),
            "the job's directory starts clean: {}",
            p.display()
        );
    }
    let this_process_is_tracked = {
        let probe = d.join("probe");
        std::fs::write(&probe, b"x").expect("write");
        tagged(&probe)
    };

    // (1) A PLAIN RENAME by this process.
    let moved = d.join("plain-moved");
    std::fs::rename(&plain, &moved).expect("rename");
    let plain_tagged = tagged(&moved);

    // (2) THE APFS ATOMIC EXCHANGE this updater uses for the `.app` swap.
    rename_swap(&a, &b).expect("rename_swap on one volume");
    let (a_tagged, b_tagged) = (tagged(&a), tagged(&b));

    eprintln!(
        "this test process tracked={this_process_is_tracked}\n  \
         plain rename(2) of a clean dir      -> tagged={plain_tagged}\n  \
         renamex_np(RENAME_SWAP) of two dirs -> a tagged={a_tagged}, b tagged={b_tagged}"
    );

    // Under an UNTRACKED test process every answer is false and the measurement says
    // nothing; assert only what a tracked one proves, and say which case ran.
    if this_process_is_tracked {
        assert!(
            plain_tagged,
            "a tracked process tags a clean directory it renames — the law the updater's \
             untracked lane exists for"
        );
        // THE LOAD-BEARING HALF, asserted rather than merely printed: the `.app` swap is
        // `renamex_np(RENAME_SWAP)`, and if THAT tagged, every installed bundle would be
        // tagged by the swap itself and no amount of care earlier in the pipeline could
        // help. It does not, on either side — which is why the swap needs no untracked
        // lane and why the plain moves before it are the only carriers to fix.
        assert!(
            !a_tagged && !b_tagged,
            "renamex_np(RENAME_SWAP) must tag neither side (a={a_tagged}, b={b_tagged}); if \
             this ever fails, the atomic .app swap itself became a carrier and the \
             updater's whole placement path has to move to an untracked job"
        );
    } else {
        eprintln!("  (untracked test process: the assertions above are vacuous)");
    }
    let _ = std::fs::remove_dir_all(&d);
}

/// Make a clean directory tree through an untracked job and hand back its path.
fn clean_dir(d: &Path, name: &str) -> PathBuf {
    let made = d.join(format!("made-{name}"));
    launchd_sh(
        "mkdir -p \"$1/Contents/MacOS\" && : > \"$1/Contents/MacOS/exe\" && : > \"$2\"",
        &[&d.join(name), &made],
        &made,
    );
    let made_path = d.join(name);
    assert!(!tagged(&made_path), "the job's tree starts clean");
    made_path
}

/// THE PROMOTE. `copy_into_isolated_destination` finishes an untracked `ditto` by
/// moving `<attempt>/payload` onto the real destination — a DIFFERENT directory, so the
/// first half of this test pins what the sibling-rename case only implied: a tracked
/// cross-directory `rename(2)` tags the moved root too, which makes that promote the
/// carrier on the DMG and cross-volume lanes.
///
/// The second half pins the REPAIR: create the destination, exchange it with the clean
/// payload, drop the emptied inode. Because `renamex_np(RENAME_SWAP)` tags neither side,
/// the clean inode arrives at the destination still clean while the tagged empty
/// directory this process made is what gets thrown away.
#[test]
#[ignore = "measures the host's provenance behaviour; needs launchctl"]
fn the_swap_promote_lands_a_clean_directory_where_a_rename_would_tag_it() {
    let d = scratch();
    let this_process_is_tracked = {
        let probe = d.join("probe");
        std::fs::write(&probe, b"x").expect("write");
        tagged(&probe)
    };

    // (1) A tracked cross-directory rename — the promote as it stands today.
    let payload = clean_dir(&d, "attempt-a");
    let by_rename = d.join("dest-by-rename");
    std::fs::rename(&payload, &by_rename).expect("rename");
    let rename_tagged = tagged(&by_rename);

    // (2) The same promote via create + exchange + drop, which is the repair.
    let payload = clean_dir(&d, "attempt-b");
    let by_swap = d.join("dest-by-swap");
    std::fs::create_dir(&by_swap).expect("create destination");
    let placeholder_tagged = tagged(&by_swap);
    rename_swap(&payload, &by_swap).expect("rename_swap on one volume");
    std::fs::remove_dir_all(&payload).expect("drop the emptied inode");
    let swap_tagged = tagged(&by_swap);
    let inner_tagged = tagged(&by_swap.join("Contents/MacOS/exe"));

    eprintln!(
        "this test process tracked={this_process_is_tracked}\n  \
         promote by rename(2)        -> tagged={rename_tagged}\n  \
         the placeholder we created  -> tagged={placeholder_tagged}\n  \
         promote by create+swap+drop -> tagged={swap_tagged} (inner file tagged={inner_tagged})"
    );

    if this_process_is_tracked {
        assert!(
            rename_tagged,
            "a tracked cross-directory rename tags the moved root — this is the promote \
             carrier the repair exists to remove"
        );
        assert!(
            placeholder_tagged,
            "the empty directory a tracked process creates is itself tagged; if it were \
             not, the exchange would be pointless because a plain create would already do"
        );
        assert!(
            !swap_tagged,
            "THE REPAIR: create + renamex_np(RENAME_SWAP) + drop must leave the clean \
             payload inode clean at the destination; if this fails the updater cannot \
             place a bundle without a launchd job and the whole approach is void"
        );
        assert!(
            !inner_tagged,
            "nothing inside the payload is touched either"
        );
    } else {
        eprintln!("  (untracked test process: the assertions above are vacuous)");
    }
    let _ = std::fs::remove_dir_all(&d);
}
