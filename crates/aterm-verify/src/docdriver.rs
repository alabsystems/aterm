// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE DOCTESTS' DOC DRIVER.

use std::ffi::{OsStr, OsString};

/// The real doc driver, an absolute path.
pub const DRIVER_ENV: &str = "ATERM_VERIFY_DOC_DRIVER";

/// The packages whose doctest pass is skipped, by package name, separated by [`SKIP_SEPARATOR`].
pub const SKIP_ENV: &str = "ATERM_VERIFY_DOC_SKIP";

/// Between two package names in [`SKIP_ENV`]: a package name never holds one.
pub const SKIP_SEPARATOR: char = ',';

/// [`SKIP_ENV`]'s value for `packages`.
#[must_use]
pub fn skip_list(packages: &[String]) -> String {
    packages.join(&SKIP_SEPARATOR.to_string())
}

/// Is this invocation a doctest pass of a package in `skip`? Cargo spells a doctest pass with a
/// `--test` argument of its own and names the package in `CARGO_PKG_NAME`; anything else, or
/// anything this cannot read, is no.
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

/// What a skipped pass prints in rustdoc's place.
#[must_use]
pub fn skip_note(package: &OsStr) -> String {
    format!(
        "aterm-verify: no doctest in {}'s docs (verify's scan), so rustdoc was not run on it",
        package.to_string_lossy()
    )
}

/// The stand-in: skip the pass ([`skips`]) or become the real driver.
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
        assert!(!skips(&pass(), Some(OsStr::new("documented")), s));
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
        assert!(!skips(&argv(&["-vV"]), plain, s));
        assert!(!skips(&argv(&["@/tmp/cargo-args"]), plain, s));
        assert!(!skips(
            &argv(&["--test-args", "--quiet", "--test-run-directory", "/r"]),
            plain,
            s
        ));
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
