// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE DOCTESTS' DOC DRIVER: this binary, standing in for the rustdoc cargo
//! runs, so the doctests stage selects exactly the packages the test compile
//! selected and still runs rustdoc only on the libraries whose docs hold a
//! doctest.
//!
//! WHY NOT `-p` FOR THE DOCUMENTED CRATES ALONE: cargo resolves features over
//! the packages a command selects, so `targo test --doc -p <the crates with a
//! doctest>` enabled fewer features than the test compile's `--workspace` (or
//! the cone's `-p` list) did, and every library it enabled fewer features of,
//! and everything built on one, was a second unit the test compile never built.
//! Measured on the whole tree (2026-10-22, cargo's unit graphs): that argv built
//! 23 units the test compile did not (18 libraries and 5 build-script units).
//! 18 were feature variants, rebuilt by the doctests alone after every change
//! under them, most at opt-level 2: aterm-core without `serde`, aterm-render
//! without its defaults, aterm-types, -grid, -scrollback, -effects, -font,
//! -control, -window and 5 more libraries, and 4 build-script units. Handed the
//! test compile's own selection, cargo resolves what the test compile resolved,
//! and the doctests link the units it built.
//!
//! Cargo then hands its doc driver every library in that selection, one
//! `--test` pass each. For a package the stage names in [`SKIP_ENV`] (the scan,
//! [`crate::docscan`], found no doctest in its sources) this stand-in prints why
//! and exits 0; every other invocation becomes the real driver ([`DRIVER_ENV`],
//! the file cargo would have run) by exec, with cargo's argv and environment, so
//! what rustdoc compiles and runs is what it compiled and ran when cargo spawned
//! it (off unix, which has no exec, the driver runs as a child and its exit code
//! is this process's). Only `RUSTDOC` is set back to the real driver and the two
//! variables below are removed, so no doctest sees this binary.
//!
//! WHAT THE DOCTESTS STILL BUILD ALONE (the same unit graphs): 9 units. 5 serve
//! doctests that run and the `-p` argv built them too (aterm-spec with
//! `anchors`, aterm-spec-anchor and its build-script run, log, profiling). 4 are
//! libraries of skipped packages that nothing else here links, which cargo
//! builds before it hands each to this stand-in: aterm-wasm, -winres, -winsign
//! and document-features. Two sit above hot crates (aterm-wasm above aterm-core,
//! -render, -types and 6 more; aterm-winsign above aterm-types) and rebuild
//! after a change under them, cheaply on macOS, where aterm-wasm's wasm32 half
//! compiles out. Leaving the 4 out of the selection (`--exclude`) would save
//! only those rebuilds, and only where a resolve per run showed their feature
//! requests redundant; otherwise it brings back variants like the ones above.
//! The commits that made this stand-in carry the timings, before and after.
//!
//! FAILS WIDE, as the scan does: only a `--test` pass of a package named in the
//! skip list is skipped. An argument file (cargo's retry of a command line too
//! long to spawn), a missing `CARGO_PKG_NAME`, a package not named or a missing
//! list all run.

use std::ffi::{OsStr, OsString};

/// The real doc driver, an absolute path: the file cargo would have run in this
/// binary's place (the stage's `cargo_doc_driver`), so no PATH lookup of the
/// stand-in's can pick another. Set, it is what makes this binary the stand-in
/// (`main.rs`).
pub const DRIVER_ENV: &str = "ATERM_VERIFY_DOC_DRIVER";

/// The packages whose doctest pass is skipped, by package name, separated by
/// [`SKIP_SEPARATOR`].
pub const SKIP_ENV: &str = "ATERM_VERIFY_DOC_SKIP";

/// Between two package names in [`SKIP_ENV`]: a package name never holds one.
pub const SKIP_SEPARATOR: char = ',';

/// [`SKIP_ENV`]'s value for `packages`.
#[must_use]
pub fn skip_list(packages: &[String]) -> String {
    packages.join(&SKIP_SEPARATOR.to_string())
}

/// Is this invocation a doctest pass of a package in `skip`? Cargo spells a
/// doctest pass with a `--test` argument of its own and names the package in
/// `CARGO_PKG_NAME`; anything else, or anything this cannot read, is no.
#[must_use]
pub fn skips(args: &[OsString], package: Option<&OsStr>, skip: Option<&OsStr>) -> bool {
    let (Some(package), Some(skip)) = (package, skip.and_then(OsStr::to_str)) else {
        return false;
    };
    args.iter().any(|a| a == "--test")
        && skip
            .split(SKIP_SEPARATOR)
            .any(|name| !name.is_empty() && OsStr::new(name) == package)
}

/// What a skipped pass prints in rustdoc's place: the log's reason for a
/// `Doc-tests` header with no `test result:` under it.
#[must_use]
pub fn skip_note(package: &OsStr) -> String {
    format!(
        "aterm-verify: no doctest in {}'s docs (verify's scan), so rustdoc was not run on it",
        package.to_string_lossy()
    )
}

/// The stand-in: skip the pass ([`skips`]) or become the real driver. Returns
/// only an exit code: `0` for a skip, `127` when the driver could not be run
/// (its reason on stderr), `2` when [`DRIVER_ENV`] is unset. Off unix a driver
/// that ran ends this process with its own exit code (`become_driver`).
#[must_use]
pub fn main(args: &[OsString]) -> i32 {
    let Some(driver) = std::env::var_os(DRIVER_ENV) else {
        eprintln!("aterm-verify: the doc-driver stand-in needs {DRIVER_ENV}");
        return 2;
    };
    let package = std::env::var_os("CARGO_PKG_NAME");
    if skips(
        args,
        package.as_deref(),
        std::env::var_os(SKIP_ENV).as_deref(),
    ) {
        println!("{}", skip_note(package.as_deref().unwrap_or_default()));
        return 0;
    }
    let mut command = std::process::Command::new(&driver);
    aterm_uds::spawn::set_flags(&mut command, 0);
    command
        .args(args)
        .env("RUSTDOC", &driver)
        .env_remove(DRIVER_ENV)
        .env_remove(SKIP_ENV);
    let err = become_driver(&mut command);
    eprintln!(
        "aterm-verify: could not run the doc driver {}: {err}",
        driver.to_string_lossy()
    );
    127
}

/// Replace this process with the driver: cargo's descriptors (its jobserver
/// among them) are rustdoc's, and an exec keeps the whole table. Returns the
/// error that prevented it.
#[cfg(unix)]
fn become_driver(command: &mut aterm_uds::spawnfd::StdCommand) -> std::io::Error {
    use std::os::unix::process::CommandExt as _;
    command.exec()
}

/// Off unix there is no exec: run the driver and leave with its exit code.
#[cfg(not(unix))]
fn become_driver(command: &mut aterm_uds::spawnfd::StdCommand) -> std::io::Error {
    match command.status() {
        Ok(status) => std::process::exit(status.code().unwrap_or(1)),
        Err(e) => e,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(a: &[&str]) -> Vec<OsString> {
        a.iter().map(OsString::from).collect()
    }

    /// A doctest pass as cargo spells it: the package's crate as `--test`'s
    /// input, beside the `--test-*` options that are not the flag.
    fn pass() -> Vec<OsString> {
        argv(&[
            "--edition=2024",
            "--crate-name",
            "plain",
            "--test",
            "crates/plain/src/lib.rs",
            "--test-run-directory",
            "/r/crates/plain",
            "--test-args",
            "--quiet",
        ])
    }

    #[test]
    fn only_a_doctest_pass_of_a_named_package_is_skipped() {
        let skip = OsString::from(skip_list(&["other".into(), "plain".into()]));
        let s = Some(skip.as_os_str());
        assert!(skips(&pass(), Some(OsStr::new("plain")), s));
        // A package the stage did not name runs: the list names what the scan
        // cleared, never what it kept.
        assert!(!skips(&pass(), Some(OsStr::new("documented")), s));
        // Exact names only: no prefix, suffix or empty entry matches.
        for name in ["plai", "plainer", "lain", ""] {
            assert!(!skips(&pass(), Some(OsStr::new(name)), s), "{name:?}");
        }
        let gappy = OsString::from(",,plain,");
        assert!(skips(&pass(), Some(OsStr::new("plain")), Some(&gappy)));
        assert!(!skips(&pass(), Some(OsStr::new("")), Some(&gappy)));
    }

    #[test]
    fn whatever_the_stand_in_cannot_read_runs() {
        let skip = OsString::from("plain");
        let s = Some(skip.as_os_str());
        let plain = Some(OsStr::new("plain"));
        // No `--test` flag of its own: a version query, or a pass whose argv
        // cargo moved into an argument file.
        assert!(!skips(&argv(&["-vV"]), plain, s));
        assert!(!skips(&argv(&["@/tmp/cargo-args"]), plain, s));
        assert!(!skips(
            &argv(&["--test-args", "--quiet", "--test-run-directory", "/r"]),
            plain,
            s
        ));
        // No package name, no list, or a list that is not UTF-8.
        assert!(!skips(&pass(), None, s));
        assert!(!skips(&pass(), plain, None));
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt as _;
            let raw = OsStr::from_bytes(b"plain,\xff");
            assert!(!skips(&pass(), plain, Some(raw)));
        }
    }
}
