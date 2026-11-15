// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::path::{Path, PathBuf};

use super::*;
use crate::exec::Run;
use crate::ladder::{Outcome, Report, Severity};

fn failed(output: String) -> Run {
    Run {
        case_observations: None,
        ok: false,
        output,
        code: Some(101),
        spawn_error: None,
        usage: None,
        stopped_at: None,
        hung: None,
    }
}

fn passed() -> Run {
    Run {
        ok: true,
        code: Some(0),
        ..failed(String::new())
    }
}

/// What the 2026-11-13 landing's test compile printed, with its build directory at `build`:
/// `aterm_gui_fabric` was left finished by an interrupted judgment and `aterm_core` was not.
fn measured(build: &Path) -> String {
    let core = build.join("debug/build/aterm-core/4895e5f07226384e/out");
    let fabric = build.join("debug/build/aterm-gui-fabric/a4a6d302d12e5ad2/out");
    format!(
        "   Compiling aterm-gui-fabric v0.0.0 (/w/crates/aterm-gui-fabric)\n\
         error[E0460]: found possibly newer version of crate `aterm_core` which `aterm_gui_fabric` depends on\n \
         --> crates/aterm-gui-fabric/tests/production_clock.rs:8:5\n  \
         |\n\
         8 | use aterm_gui_fabric::{{\n  \
         |     ^^^^^^^^^^^^^^^^\n  \
         |\n  \
         = note: perhaps that crate needs to be recompiled?\n  \
         = note: the following crate versions were found:\n          \
         crate `aterm_core`: {core}/libaterm_core-4895e5f07226384e.rmeta\n          \
         crate `aterm_core`: {core}/libaterm_core-4895e5f07226384e.rlib\n          \
         crate `aterm_gui_fabric`: {fabric}/libaterm_gui_fabric-a4a6d302d12e5ad2.rlib\n\n\
         For more information about this error, try `trustc --explain E0460`.\n\
         error: could not compile `aterm-gui-fabric` (test \"production_clock\") due to 1 previous error\n\
         warning: build failed, waiting for other jobs to finish...\n",
        core = core.display(),
        fabric = fabric.display(),
    )
}

/// A compile the source stops: a mismatched type.
fn type_error() -> String {
    "error[E0308]: mismatched types\n \
     --> crates/aterm-gui-fabric/src/lib.rs:8:5\n  \
     |\n\
     8 |     let n: u32 = \"/w/out/libaterm_core-4895e5f07226384e.rlib\";\n\
     error: could not compile `aterm-gui-fabric` (lib) due to 1 previous error\n"
        .to_owned()
}

fn word(run: &Run) -> Outcome {
    let mut report = Report::new("test compile");
    report.decide_child(run, "targo test --no-run");
    let outcomes: Vec<Outcome> = report.outcomes().map(|(outcome, _)| outcome).collect();
    assert_eq!(outcomes.len(), 1, "one row for one child");
    outcomes[0]
}

#[test]
fn the_measured_interrupted_build_error_is_stale_and_names_its_three_files() {
    let stale = find(&measured(Path::new("/b"))).expect("E0460 is the build directory's");
    assert_eq!(
        stale.error,
        "error[E0460]: found possibly newer version of crate `aterm_core` which \
         `aterm_gui_fabric` depends on"
    );
    assert_eq!(
        stale.artifacts,
        [
            "/b/debug/build/aterm-core/4895e5f07226384e/out/libaterm_core-4895e5f07226384e.rmeta",
            "/b/debug/build/aterm-core/4895e5f07226384e/out/libaterm_core-4895e5f07226384e.rlib",
            "/b/debug/build/aterm-gui-fabric/a4a6d302d12e5ad2/out/libaterm_gui_fabric-a4a6d302d12e5ad2.rlib",
        ]
        .map(PathBuf::from)
    );
}

#[test]
fn each_stale_class_is_found_and_no_error_of_the_source_is() {
    let compiled = "error: could not compile `d` (lib) due to 1 previous error\n";
    for (error, artifacts) in [
        (
            "error[E0464]: multiple candidates for `rlib` dependency `x` found\n  \
             = note: candidate #1: /b/a b/libx-0123456789abcdef.rlib\n  \
             = note: candidate #2: /b/a b/libx-fedcba9876543210.rlib\n",
            vec![
                "/b/a b/libx-0123456789abcdef.rlib",
                "/b/a b/libx-fedcba9876543210.rlib",
            ],
        ),
        (
            "error[E0786]: found invalid metadata files for crate `x` which `d` depends on\n  \
             = note: failed to mmap file '/b/libx-0123456789abcdef.rmeta': Invalid argument\n",
            vec!["/b/libx-0123456789abcdef.rmeta"],
        ),
        (
            "error[E0463]: can't find crate for `x` which `d` depends on\n",
            vec![],
        ),
        (
            "\u{1b}[1m\u{1b}[91merror[E0460]\u{1b}[0m: found possibly newer version of crate `x` which `d` depends on\n          \
             crate `x`: /b/libx-0123456789abcdef.rlib\n",
            vec!["/b/libx-0123456789abcdef.rlib"],
        ),
    ] {
        let stale = find(&format!("{error}{compiled}")).unwrap_or_else(|| panic!("{error}"));
        assert_eq!(
            stale.artifacts,
            artifacts.into_iter().map(PathBuf::from).collect::<Vec<_>>(),
            "{error}"
        );
        assert_eq!(
            find(error),
            None,
            "the same text is no failed compile's without Cargo's line for one: {error}"
        );
    }
    // A cross cell's compile says its diagnostics as Cargo's JSON messages.
    let json = |code: &str, rendered: &str| {
        format!(
            "{{\"reason\":\"compiler-message\",\"message\":{{\"code\":{{\"code\":\"{code}\"}},\
             \"level\":\"error\",\"rendered\":\"{rendered}\"}}}}\n{compiled}"
        )
    };
    let stale = find(&json(
        "E0460",
        "error[E0460]: found possibly newer version of crate `x` which `d` depends on\\n  \
         = note: the following crate versions were found:\\n          \
         crate `x`: /b/libx-0123456789abcdef.rlib\\n",
    ))
    .expect("a JSON message's rendered diagnostic is read");
    assert_eq!(
        stale.artifacts,
        [PathBuf::from("/b/libx-0123456789abcdef.rlib")]
    );
    assert_eq!(
        find(&json("E0308", "error[E0308]: mismatched types\\n")),
        None
    );
    // The negative controls: what only the source, or the toolchain it asks for, can cause.
    for source in [
        type_error(),
        format!(
            "error[E0463]: can't find crate for `core`\n  \
             = note: the `wasm32-unknown-unknown` target may not be installed\n{compiled}"
        ),
        format!("error[E0463]: can't find crate for `serde`\n{compiled}"),
        format!("error[E0432]: unresolved import `x::gone`\n{compiled}"),
    ] {
        assert_eq!(find(&source), None, "{source}");
    }
}

#[test]
fn a_stale_compile_is_could_not_run_and_a_type_error_is_still_a_finding() {
    let stale = failed(measured(Path::new("/b")));
    assert_eq!(word(&stale), Outcome::Fail(Severity::CouldNotRun));
    let why = stale.environment_failure().expect("the directory's");
    assert!(
        why.contains("libaterm_gui_fabric-a4a6d302d12e5ad2.rlib") && why.contains("E0460"),
        "the row names the error and the artifact: {why}"
    );
    let source = failed(type_error());
    assert_eq!(source.environment_failure(), None);
    assert_eq!(word(&source), Outcome::Fail(Severity::GateFailed));
}

/// A build directory holding the measured units' files, a unit nothing names, and beside it a
/// toolchain's library the compiler may name too.
struct Built {
    _scratch: aterm_tempfile::TempDir,
    build: PathBuf,
    named: [PathBuf; 3],
    unnamed: PathBuf,
    toolchain: PathBuf,
}

impl Built {
    fn new() -> Self {
        let scratch = aterm_tempfile::tempdir().expect("a scratch directory");
        let build = aterm_uds::runs::canonical(scratch.path()).join("build");
        let core = build.join("debug/build/aterm-core/4895e5f07226384e/out");
        let fabric = build.join("debug/build/aterm-gui-fabric/a4a6d302d12e5ad2/out");
        let named = [
            core.join("libaterm_core-4895e5f07226384e.rmeta"),
            core.join("libaterm_core-4895e5f07226384e.rlib"),
            fabric.join("libaterm_gui_fabric-a4a6d302d12e5ad2.rlib"),
        ];
        let unnamed = fabric.join("libaterm_gui_fabric-a4a6d302d12e5ad2.rmeta");
        let toolchain = aterm_uds::runs::canonical(scratch.path())
            .join("toolchain/lib/libstd-0123456789abcdef.rlib");
        let built = Self {
            _scratch: scratch,
            build,
            named,
            unnamed,
            toolchain,
        };
        built.write();
        built
    }

    /// Write every file again, as a compile that builds the units does.
    fn write(&self) {
        for path in self.named.iter().chain([&self.unnamed, &self.toolchain]) {
            std::fs::create_dir_all(path.parent().expect("a parent")).expect("its directory");
            std::fs::write(path, b"artifact").expect("the file");
        }
    }

    /// The measured error, its notes naming the toolchain's library too.
    fn stale(&self) -> String {
        let found = "\n\nFor more information";
        let text = measured(&self.build);
        assert!(text.contains(found), "{text}");
        text.replace(
            found,
            &format!(
                "\n          crate `std`: {}{found}",
                self.toolchain.display()
            ),
        )
    }
}

#[test]
fn a_stale_stop_is_made_once_more_after_exactly_the_named_files_are_removed() {
    let built = Built::new();
    let mut calls = 0;
    let run = retried(
        std::slice::from_ref(&built.build),
        || {
            calls += 1;
            if calls == 1 {
                failed(built.stale())
            } else {
                assert!(
                    built.named.iter().all(|path| !path.exists()),
                    "the second call starts with the named files gone"
                );
                passed()
            }
        },
        |run| run,
    );
    assert_eq!(calls, 2, "one retry");
    assert!(run.ok, "the second call's result is the row's");
    assert!(run.output.starts_with(RETRIED), "{}", run.output);
    assert!(
        find(&built.stale())
            .expect("stale")
            .artifacts
            .contains(&built.toolchain),
        "the error names the toolchain's library too"
    );
    assert!(
        built.unnamed.exists() && built.toolchain.exists(),
        "a file the compiler did not name stays, and so does one outside the build directory"
    );
}

#[test]
fn a_second_stale_stop_is_could_not_run_naming_the_artifact_and_no_third_call() {
    let built = Built::new();
    let mut calls = 0;
    let run = retried(
        std::slice::from_ref(&built.build),
        || {
            calls += 1;
            built.write();
            failed(built.stale())
        },
        |run| run,
    );
    assert_eq!(calls, 2, "one retry, never a second");
    assert_eq!(word(&run), Outcome::Fail(Severity::CouldNotRun));
    let why = run.environment_failure().expect("the directory's");
    assert!(
        why.contains("again after the files it named were removed")
            && why.contains("libaterm_core-4895e5f07226384e.rlib"),
        "{why}"
    );
}

#[test]
fn a_source_error_is_made_once_removes_nothing_and_is_a_finding() {
    let built = Built::new();
    let mut calls = 0;
    let run = retried(
        std::slice::from_ref(&built.build),
        || {
            calls += 1;
            failed(type_error())
        },
        |run| run,
    );
    assert_eq!(calls, 1, "a finding is never run again");
    assert!(built.named.iter().all(|path| path.exists()));
    assert_eq!(word(&run), Outcome::Fail(Severity::GateFailed));
}

#[test]
fn a_stale_stop_that_names_nothing_under_the_build_directory_is_not_made_again() {
    let built = Built::new();
    for (roots, output) in [
        // The run keeps no record of its build directories: nothing may be removed.
        (Vec::new(), built.stale()),
        // The error names no file at all.
        (
            vec![built.build.clone()],
            "error[E0463]: can't find crate for `x` which `d` depends on\n\
             error: could not compile `d` (lib) due to 1 previous error\n"
                .to_owned(),
        ),
    ] {
        let mut calls = 0;
        let run = retried(
            &roots,
            || {
                calls += 1;
                failed(output.clone())
            },
            |run| run,
        );
        assert_eq!(calls, 1);
        assert!(built.named.iter().all(|path| path.exists()));
        assert_eq!(word(&run), Outcome::Fail(Severity::CouldNotRun));
        assert!(!run.output.contains(RETRIED));
    }
}
