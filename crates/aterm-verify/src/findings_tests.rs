// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn failed_log(name: &str, message: &str) -> String {
    format!(
        "     Running tests/x.rs (target/debug/deps/x-1)\n\nrunning 1 test\n\
         test {name} ... FAILED\n\nfailures:\n\n---- {name} stdout ----\n{message}\n\n\
         failures:\n    {name}\n\ntest result: FAILED. 0 passed; 1 failed; 0 ignored; \
         0 measured; 0 filtered out; finished in 0.01s\n\nerror: test failed, to rerun \
         pass `-p atpkg --test x`\n"
    )
}

#[test]
fn a_failed_test_is_its_own_finding_with_the_command_that_reruns_it() {
    let log = failed_log(
        "tar::ustar_is_pinned",
        "thread 'tar::ustar_is_pinned' panicked at src/tar.rs:9:5:\nassertion failed",
    );
    let found = test_findings("targo test --tests", &log, &Rerun::default());
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].id, "-p atpkg --test x -- tar::ustar_is_pinned");
    assert_eq!(
        found[0].rerun.as_deref(),
        Some("targo --unverified test -p atpkg --test x -- tar::ustar_is_pinned --exact")
    );
    assert_eq!(
        found[0].message,
        [
            "thread 'tar::ustar_is_pinned' panicked at src/tar.rs:9:5:",
            "assertion failed"
        ]
    );
}

#[test]
fn a_refusal_beside_a_failure_is_no_finding() {
    let sentinel = crate::libtest::COULD_NOT_RUN_SENTINEL;
    let refused = failed_log("fish::live", &format!("{sentinel} — no fish on this host"));
    let real = failed_log("tar::ustar_is_pinned", "assertion failed");
    let (found, left) = split_test_findings("t", &format!("{refused}{real}"), &Rerun::default());
    let ids: Vec<&str> = found.iter().map(|f| f.id.as_str()).collect();
    assert_eq!(ids, ["-p atpkg --test x -- tar::ustar_is_pinned"]);
    assert_eq!(left, ["fish::live"]);
    let (found, left) = split_test_findings("t", &refused, &Rerun::default());
    assert_eq!(found.len(), 1);
    assert!(left.is_empty());
}

#[test]
fn a_log_that_cannot_account_for_its_failures_is_one_row_finding() {
    let log = "     Running tests/x.rs (target/debug/deps/x-1)\nerror: test failed, to rerun \
               pass `-p atpkg --test x`\n\nCaused by:\n  process didn't exit successfully \
               (signal: 11, SIGSEGV)\n";
    let found = test_findings("targo test --tests", log, &Rerun::default());
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].id, "targo test --tests");
    assert_eq!(found[0].rerun, None);
    assert!(
        found[0].message.iter().any(|l| l.contains("SIGSEGV")),
        "{found:?}"
    );
}

#[test]
fn a_rerun_sets_what_the_test_child_ran_under() {
    let cmd = Cmd::new("targo")
        .env("TRAIL_LAWS_FULL", "1")
        .env("CARGO_TARGET_DIR", "/t")
        .env("RUSTDOC", "/s/trustdoc")
        .env(crate::docdriver::DRIVER_ENV, "/s/trustdoc")
        .env(crate::docdriver::SKIP_ENV, "plain")
        .env(crate::testrun::SUBPROCESS_SLOTS_ENV, "/s/slots")
        .env("ATERM_PAINT_BIN", "/b/My App/aterm");
    let rerun = Rerun::of(&cmd, Some(3));
    let log = failed_log("laws::full_grid", "boom");
    let found = test_findings("targo test --tests", &log, &rerun);
    assert_eq!(
        found[0].rerun.as_deref(),
        Some(
            "TRAIL_LAWS_FULL=1 ATERM_PAINT_BIN='/b/My App/aterm' RUST_TEST_THREADS=3 \
             targo --unverified test -p atpkg --test x -- laws::full_grid --exact"
        )
    );
    assert_eq!(Rerun::of(&Cmd::new("targo"), None), Rerun::default());
}

/// A FAILED TEST RERUNS AT THE WIDTH AND GRANT ITS BINARY RAN AT, where the plan ran it at one
/// of its own: a failure only one test thread, or only its pool size, shows is shown again by its
/// rerun line. NEGATIVE CONTROL: a binary of the same log that ran at the run's width reruns at
/// that, with no grant, and a grant the run's own command carried is never copied.
#[test]
fn a_failed_test_reruns_at_the_width_its_binary_ran_at() {
    let cmd = Cmd::new("targo")
        .env("TRAIL_LAWS_FULL", "1")
        .env(aterm_cores::GRANT_ENV, "9");
    let rerun = Rerun::of(&cmd, Some(4)).at_widths([("-p atpkg --test x".to_owned(), 1, 3)]);
    let log = format!(
        "{}{}",
        failed_log("one::thread", "boom"),
        failed_log("any::width", "boom").replace("--test x", "--test y")
    );
    let found = test_findings("targo test --tests", &log, &rerun);
    let reruns: Vec<_> = found.iter().filter_map(|f| f.rerun.as_deref()).collect();
    assert_eq!(
        reruns,
        [
            "TRAIL_LAWS_FULL=1 RUST_TEST_THREADS=1 ATERM_TEST_CORES=3 targo --unverified test \
             -p atpkg --test x -- one::thread --exact",
            "TRAIL_LAWS_FULL=1 RUST_TEST_THREADS=4 targo --unverified test -p atpkg --test y \
             -- any::width --exact"
        ]
    );
    let seeded = test_findings("test", &log, &rerun.clone().with_seed(Some(17)));
    assert!(seeded.iter().all(|finding| {
        finding
            .rerun
            .as_deref()
            .is_some_and(|line| line.ends_with(" -Zunstable-options --shuffle-seed=17"))
    }));
}

#[test]
fn an_excerpt_is_the_errors_first_never_the_progress() {
    assert_eq!(
        excerpt(
            "warning: UNVERIFIED: `targo run` was explicitly authorized\n\
             note: compiled WITHOUT Trust verification — 0 obligations checked\n\
             aterm-census: unknown option \"--all\"\n",
            12
        ),
        ["aterm-census: unknown option \"--all\""],
        "targo's lane banners are never the excerpt"
    );
    let out = "   Compiling a v0.1.0\nwarning: a build script said so\nnote: x\nnote: y\n\
               note: z\n    Checking b v0.1.0\nerror: unused variable: `x`\n  \
               --> src/lib.rs:3:9\n   |\n    Finished dev\n";
    assert_eq!(
        excerpt(out, 12),
        [
            "note: y",
            "note: z",
            "error: unused variable: `x`",
            "  --> src/lib.rs:3:9",
            "   |"
        ]
    );
    assert_eq!(
        excerpt("warning: only this\nmore\n", 12),
        ["warning: only this", "more"],
        "warnings when there is no error"
    );
    assert_eq!(excerpt("one\ntwo\nthree\n", 2), ["two", "three"]);
    let twice = "error[E0433]: no `x`\n  --> src/a.rs:1:1\n   |\n   = n1\n   = n2\n   = n3\n\
                 error[E0433]: no `x`\n  --> src/a.rs:1:1\n   |\n   = n4\n   = n5\n   = n6\n\
                 error[E0433]: no `y`\n  --> src/b.rs:2:2\n";
    assert_eq!(
        excerpt(twice, 12),
        [
            "error[E0433]: no `x`",
            "  --> src/a.rs:1:1",
            "   |",
            "   = n5",
            "   = n6",
            "error[E0433]: no `y`",
            "  --> src/b.rs:2:2"
        ],
        "an error cargo printed twice is shown once"
    );
    assert_eq!(excerpt("", 4), Vec::<String>::new());
}

#[test]
fn actionable_diagnostics_survive_noisy_logger_records() {
    let noise: String = (0..32)
        .map(|i| {
            format!(
                "ERROR rustc_interface::passes checksum unavailable\n\
                 note: dependency {i}\nERROR logger detail {i}\n"
            )
        })
        .collect();
    for diagnostic in [
        "error: method `unused` is never used",
        "error[E0425]: cannot find value `missing`",
        "fatal error: cannot open input",
        "thread 'main' panicked at src/lib.rs:7:9:",
        "✗ FAIL [OB-7] missing obligation",
    ] {
        let output = format!(
            "{noise}\x1b[31m{diagnostic}\x1b[0m\n\
               --> src/lib.rs:7:9\n   |\n\
             error: could not compile `example`\n"
        );
        let found = excerpt(&output, MESSAGE_LINES);
        assert!(found.iter().any(|line| line == diagnostic), "{found:?}");
        assert!(found.iter().any(|line| line.contains("src/lib.rs:7:9")));
        assert!(found.len() <= MESSAGE_LINES);
        assert!(found.iter().all(|line| !line.starts_with("ERROR")));
    }
}

#[test]
fn an_excerpt_reserves_causes_before_context_and_keeps_distinct_locations() {
    let first = "error[E0425]: cannot find value `missing`";
    let output = format!(
        "note: preceding context\n{first}\n  --> src/a.rs:1:1\n   |\n\
         {first}\n  --> src/b.rs:2:2\n   |\n"
    );
    assert_eq!(excerpt(&output, 1), [first]);
    let found = excerpt(&output, MESSAGE_LINES);
    assert_eq!(found.iter().filter(|line| *line == first).count(), 2);
    for source in ["src/a.rs:1:1", "src/b.rs:2:2"] {
        assert!(found.iter().any(|line| line.contains(source)));
    }
    assert!(excerpt(&output, 0).is_empty());
}

#[test]
fn logger_only_failures_still_show_each_distinct_error() {
    let repeated = "ERROR transport could not write";
    let mut output: String = (0..32)
        .map(|i| format!("{repeated}\nnote: attempt {i}\n"))
        .collect();
    let distinct = "ERROR transport could not close";
    output.push_str(distinct);
    let found = excerpt(&output, MESSAGE_LINES);
    assert_eq!(found.iter().filter(|line| *line == repeated).count(), 1);
    assert!(found.iter().any(|line| line == distinct));
    assert!(found.len() <= MESSAGE_LINES);
}

#[test]
fn a_rows_marked_failure_is_in_the_excerpt() {
    let forge = "      \u{2713} [OB-6] held\n\
                 \x20     \u{2717} FAIL [OB-7] `a.rs` has no notice. FIX: add one.\n\
                 \x20       [OB-7] scope: forks\n      \u{2713} [OB-8] held\n\
                 \x20   cargo forge attest: FAILED \u{2014} 1 obligation violation(s).\n";
    assert_eq!(
        excerpt(forge, 12),
        [
            "      \u{2713} [OB-6] held",
            "      \u{2717} FAIL [OB-7] `a.rs` has no notice. FIX: add one.",
            "        [OB-7] scope: forks",
            "      \u{2713} [OB-8] held",
            "    cargo forge attest: FAILED \u{2014} 1 obligation violation(s).",
        ],
        "the row that names the fix is shown, not only the tally after it"
    );
}
