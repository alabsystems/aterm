// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::io::{Read as _, Write as _};

#[test]
fn a_loaded_image_outlives_the_first_path_sample_without_guessing_unknown_reads() {
    let old = Some((1, 2, 3, 4));
    let new = Some((1, 5, 3, 4));
    assert!(
        replaced(new, new, Some(false)),
        "replaced before first sample"
    );
    assert!(replaced(old, new, Some(true)), "later in-place rewrite");
    assert!(!replaced(new, new, Some(true)));
    assert!(!replaced(new, new, None), "unreadable loaded image");
    assert!(!replaced(None, new, None), "no usable initial evidence");
    assert!(
        !replaced(old, None, Some(false)),
        "no successor at the path"
    );
    assert!(
        replaced(old, new, None),
        "a known later disk change still counts"
    );
    let started = initial_stamp(new, Some(false));
    assert_eq!(started, None, "never adopt a replacement as the baseline");
    assert!(
        !replaced(started, old, Some(true)),
        "the old image returned"
    );
    assert_eq!(initial_stamp(old, None), old, "unavailable-image fallback");
}

#[test]
fn later_path_replacement_is_seen_but_a_missing_successor_is_not() {
    let root = aterm_tempfile::tempdir().unwrap();
    let path = root.path().join("program");
    std::fs::write(&path, b"old").unwrap();
    let watch = ProgramWatch {
        started: metadata(&path).as_ref().map(stamp),
        path: Some(path.clone()),
        image: None,
    };
    assert!(!watch.replaced());
    let next = root.path().join("next");
    std::fs::write(&next, b"new").unwrap();
    std::fs::rename(&next, &path).unwrap();
    assert!(watch.replaced());
    std::fs::rename(&path, root.path().join("moved")).unwrap();
    assert!(!watch.replaced());
}

/// The parent waits for proof of exec before replacing the file. In the first arm this
/// process then takes its *first* path sample, after replacement, while executing the old
/// image. A stamp-only watch cannot detect it. The other arm checks a later replacement.
#[test]
fn executable_watch_child() {
    let Some(mode) = std::env::var_os("ATERM_TEST_PROGRAM_WATCH") else {
        return;
    };
    let before = (mode == "after-watch").then(ProgramWatch::current);
    println!("\nprogram-watch-ready");
    std::io::stdout().flush().unwrap();
    let mut go = [0];
    std::io::stdin().read_exact(&mut go).unwrap();
    let watch = before.unwrap_or_else(ProgramWatch::current);
    assert!(
        watch.replaced(),
        "the process still maps the replaced executable"
    );
}

#[test]
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn an_exec_replaced_before_the_first_watch_is_not_adopted_as_its_baseline() {
    use aterm_uds::deadline::{KeptUntilRan, ad_hoc_copy, prints};
    use aterm_uds::spawnfd::{Child, Stdio, command};
    const CHILD: &str = "program_watch::tests::executable_watch_child";
    struct Reap(Child);
    impl Drop for Reap {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    for mode in ["before-watch", "after-watch"] {
        let root = aterm_tempfile::tempdir().unwrap();
        let mut kept = KeptUntilRan::new(root.keep());
        let program = kept.path().join("watch-child");
        let next = kept.path().join("watch-next");
        let exe = std::env::current_exe().unwrap();
        ad_hoc_copy(&exe, &program);
        ad_hoc_copy(&exe, &next);
        let mut child = Reap(
            command(&program)
                .args(["--exact", CHILD, "--nocapture"])
                .env("ATERM_TEST_PROGRAM_WATCH", mode)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .spawn()
                .unwrap(),
        );
        prints(
            &mut child.0,
            "program-watch-ready",
            aterm_spec::wait::MUST_HAPPEN,
        );
        kept.ran();
        std::fs::rename(&next, &program).unwrap();
        child.0.stdin.take().unwrap().write_all(b"x").unwrap();
        let status = aterm_spec::wait::until_within(
            aterm_spec::wait::MUST_HAPPEN,
            "the replaced executable's watch",
            || child.0.try_wait().unwrap(),
        );
        assert!(status.success(), "{mode}: {status}");
    }
}
