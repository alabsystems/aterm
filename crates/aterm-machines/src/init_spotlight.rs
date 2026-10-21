// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Local init's Rust toolchains, kept from feeding Spotlight: rustup installs its
//! `minimal` profile, and no toolchain rustup installed carries `rust-docs`, the offline
//! HTML book no build reads. Measured on a Mac with rustup's `default` profile
//! (2026-10-20): stable's `rust-docs` held 909 MB and Spotlight had indexed 84,921 of its
//! HTML pages, indexed again at each toolchain install or update. A linked toolchain
//! (`rustup toolchain link`, atpkg's `trust`) is not rustup's to change and is left alone.
//! Build intermediates are the build-cache rule's (`aterm_toolchain::build_cache`);
//! `aterm doctor` reports what Spotlight still holds of them.

use crate::items::{Ctx, Item};
use crate::model::{Probe, User};
use crate::port::{Cmd, System};
use std::io;
use std::path::{Path, PathBuf};

/// The rustup profile that installs no documentation.
const MINIMAL: &str = "minimal";

/// Keep this login's Rust toolchains from feeding Spotlight.
///
/// # Errors
/// Init did not resolve exactly one user.
pub fn items(ctx: &Ctx) -> Result<Vec<Box<dyn Item>>, String> {
    let [user] = ctx.users.as_slice() else {
        return Err("aterm init Rust toolchain settings require exactly one local user".into());
    };
    let home = Path::new(&user.home);
    let set = |name: &str| {
        std::env::var_os(name)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    };
    let rustup = Rustup {
        user: user.clone(),
        cargo_home: set("CARGO_HOME").unwrap_or_else(|| home.join(".cargo")),
        rustup_home: set("RUSTUP_HOME").unwrap_or_else(|| home.join(".rustup")),
    };
    Ok(vec![
        Box::new(Profile(rustup.clone())),
        Box::new(NoDocs(rustup)),
    ])
}

/// This login's rustup, where it keeps its toolchains.
#[derive(Debug, Clone)]
struct Rustup {
    user: User,
    cargo_home: PathBuf,
    rustup_home: PathBuf,
}

impl Rustup {
    fn bin(&self) -> PathBuf {
        self.cargo_home.join("bin/rustup")
    }

    /// `rustup <args>` as this user, its homes named so it reads the folders probed.
    fn cmd(&self, args: &[&str]) -> Cmd {
        Cmd::new(self.bin())
            .as_user(&self.user)
            .env("CARGO_HOME", self.cargo_home.to_string_lossy())
            .env("RUSTUP_HOME", self.rustup_home.to_string_lossy())
            .args(args)
    }

    /// The trimmed standard output of a run that succeeded.
    fn read(&self, sys: &dyn System, args: &[&str]) -> Result<String, String> {
        let cmd = self.cmd(args);
        let ran = sys
            .run(&cmd)
            .map_err(|e| format!("{}: {e}", cmd.display()))?;
        if !ran.success() {
            return Err(format!("{}: {}", cmd.display(), ran.describe()));
        }
        Ok(ran.out().trim().to_owned())
    }

    /// `Some(why)` when there is no rustup to ask: nothing to do.
    fn absent(&self, sys: &dyn System) -> Result<Option<String>, String> {
        match sys.stat(&self.bin()) {
            Ok(_) => Ok(None),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                Ok(Some(format!("no rustup at {}", self.bin().display())))
            }
            Err(e) => Err(format!("{}: {e}", self.bin().display())),
        }
    }

    /// The toolchains rustup installed that carry `rust-docs` (its component record,
    /// `lib/rustlib/components`, lists it); linked toolchains are skipped.
    fn with_docs(&self, sys: &dyn System) -> Result<Vec<String>, String> {
        let list = self.read(sys, &["toolchain", "list"])?;
        let mut out = Vec::new();
        for name in list.lines().filter_map(|l| l.split_whitespace().next()) {
            let dir = self.rustup_home.join("toolchains").join(name);
            if sys.stat(&dir).is_ok_and(|s| s.symlink) {
                continue;
            }
            let record = dir.join("lib/rustlib/components");
            let text = match sys.read_file(&record) {
                Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
                Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                Err(e) => return Err(format!("{}: {e}", record.display())),
            };
            if text
                .lines()
                .any(|c| c == "rust-docs" || c.starts_with("rust-docs-"))
            {
                out.push(name.to_owned());
            }
        }
        Ok(out)
    }
}

/// rustup installs its `minimal` profile, so a toolchain installed later brings no docs.
#[derive(Debug)]
struct Profile(Rustup);

impl Item for Profile {
    fn id(&self) -> String {
        format!("init.spotlight.rustup-profile@{}", self.0.user.name)
    }

    fn family(&self) -> &'static str {
        "spotlight"
    }

    fn title(&self) -> String {
        "rustup installs its minimal profile (no offline docs for Spotlight)".into()
    }

    fn probe(&self, sys: &dyn System, _ctx: &Ctx) -> Probe {
        match self.0.absent(sys) {
            Ok(Some(why)) => return Probe::NotApplicable { why },
            Ok(None) => {}
            Err(why) => return Probe::Unusable { why },
        }
        match self.0.read(sys, &["show", "profile"]) {
            Err(why) => Probe::Unusable { why },
            Ok(p) if p == MINIMAL => Probe::Ok {
                now: "rustup's profile is minimal".into(),
            },
            Ok(p) => Probe::Change {
                from: format!("rustup's profile is {p}, which installs rust-docs"),
                to: "minimal".into(),
            },
        }
    }

    fn apply(&self, sys: &dyn System, _ctx: &Ctx) -> Result<(), String> {
        self.0.read(sys, &["set", "profile", MINIMAL]).map(drop)
    }

    fn revert(&self) -> Option<String> {
        Some(format!("{} set profile default", self.0.bin().display()))
    }
}

/// No toolchain rustup installed carries `rust-docs`.
#[derive(Debug)]
struct NoDocs(Rustup);

impl Item for NoDocs {
    fn id(&self) -> String {
        format!("init.spotlight.rust-docs@{}", self.0.user.name)
    }

    fn family(&self) -> &'static str {
        "spotlight"
    }

    fn title(&self) -> String {
        "no rustup toolchain carries rust-docs (offline HTML Spotlight indexes)".into()
    }

    fn probe(&self, sys: &dyn System, _ctx: &Ctx) -> Probe {
        match self.0.absent(sys) {
            Ok(Some(why)) => return Probe::NotApplicable { why },
            Ok(None) => {}
            Err(why) => return Probe::Unusable { why },
        }
        match self.0.with_docs(sys) {
            Err(why) => Probe::Unusable { why },
            Ok(docs) if docs.is_empty() => Probe::Ok {
                now: "no rustup toolchain carries rust-docs".into(),
            },
            Ok(docs) => Probe::Change {
                from: format!("rust-docs installed in {}", docs.join(", ")),
                to: "removed".into(),
            },
        }
    }

    fn apply(&self, sys: &dyn System, _ctx: &Ctx) -> Result<(), String> {
        for name in self.0.with_docs(sys)? {
            self.0.read(
                sys,
                &["component", "remove", "rust-docs", "--toolchain", &name],
            )?;
        }
        Ok(())
    }

    fn revert(&self) -> Option<String> {
        Some(format!(
            "{} component add rust-docs --toolchain <each toolchain it was removed from>",
            self.0.bin().display()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{self, NoJournal};
    use crate::items::defaults::tests::{ctx, user};
    use crate::model::{Mode, Outcome};
    use crate::port::{Fake, Ran};

    const ENV: &str = "alice$ CARGO_HOME=/Users//alice/.cargo RUSTUP_HOME=/Users//alice/.rustup /Users//alice/.cargo/bin/rustup";
    const STABLE: &str =
        "/Users//alice/.rustup/toolchains/stable-aarch64-apple-darwin/lib/rustlib/components";
    const PINNED: &str =
        "/Users//alice/.rustup/toolchains/1.97.1-aarch64-apple-darwin/lib/rustlib/components";
    const WITH_DOCS: &[u8] =
        b"cargo-aarch64-apple-darwin\nrust-docs-aarch64-apple-darwin\nrustc-aarch64-apple-darwin\n";
    const WITHOUT: &[u8] = b"cargo-aarch64-apple-darwin\nrustc-aarch64-apple-darwin\n";

    fn context() -> Ctx {
        ctx(&[user("alice", 501)])
    }

    fn run(sys: &Fake, mode: Mode) -> Vec<crate::model::ItemReport> {
        let alice = user("alice", 501);
        let rustup = Rustup {
            user: alice,
            cargo_home: "/Users//alice/.cargo".into(),
            rustup_home: "/Users//alice/.rustup".into(),
        };
        let items: Vec<Box<dyn Item>> =
            vec![Box::new(Profile(rustup.clone())), Box::new(NoDocs(rustup))];
        engine::run(&items, mode, sys, &context(), &mut NoJournal)
    }

    /// A Mac whose rustup reports `profile` first, with `stable`'s component record in
    /// stable, a pinned toolchain without docs, and atpkg's linked `trust`.
    fn mac(profile: &str, stable: &[u8]) -> Fake {
        let sys = Fake::root();
        sys.file("/Users//alice/.cargo/bin/rustup", b"")
            .on(&format!("{ENV} show profile"), Ran::ok(profile))
            .on(
                &format!("{ENV} toolchain list"),
                Ran::ok("stable-aarch64-apple-darwin\n1.97.1-aarch64-apple-darwin (active, default)\ntrust\n"),
            )
            .file(STABLE, stable)
            .file(PINNED, WITHOUT)
            .link(
                "/Users//alice/.rustup/toolchains/trust",
                "/Users//alice/Library/Application Support/aterm/pkg/rustup/trust",
            );
        sys
    }

    #[test]
    fn a_default_profile_and_stables_docs_are_planned_and_nothing_runs() {
        let sys = mac("default\n", WITH_DOCS);
        let rows = run(&sys, Mode::Plan);
        assert_eq!(rows[0].outcome, Outcome::Planned, "{rows:?}");
        assert_eq!(rows[1].outcome, Outcome::Planned, "{rows:?}");
        assert_eq!(
            rows[1].from.as_deref(),
            Some("rust-docs installed in stable-aarch64-apple-darwin")
        );
        assert!(
            !sys.calls()
                .iter()
                .any(|c| c.contains(" set ") || c.contains(" remove ")),
            "{:?}",
            sys.calls()
        );
    }

    #[test]
    fn apply_sets_the_profile_removes_the_docs_and_reads_both_back() {
        let sys = mac("default\n", WITH_DOCS);
        let set = format!("{ENV} set profile minimal");
        let remove =
            format!("{ENV} component remove rust-docs --toolchain stable-aarch64-apple-darwin");
        sys.on(&format!("{ENV} show profile"), Ran::ok("minimal\n"))
            .on(&set, Ran::ok(""))
            .on(&remove, Ran::ok(""))
            .on_write(&remove, Path::new(STABLE), WITHOUT);
        let rows = run(&sys, Mode::Apply);
        assert_eq!(rows[0].outcome, Outcome::Changed, "{rows:?}");
        assert_eq!(rows[1].outcome, Outcome::Changed, "{rows:?}");
        let calls = sys.calls();
        assert_eq!(calls.iter().filter(|c| **c == set).count(), 1, "{calls:?}");
        assert_eq!(
            calls.iter().filter(|c| **c == remove).count(),
            1,
            "{calls:?}"
        );
        // The linked toolchain and the one without docs are never touched.
        assert!(
            !calls
                .iter()
                .any(|c| c.contains("--toolchain trust") || c.contains("--toolchain 1.97.1")),
            "{calls:?}"
        );
    }

    #[test]
    fn a_removal_that_does_not_read_back_fails() {
        let sys = mac("minimal\n", WITH_DOCS);
        let remove =
            format!("{ENV} component remove rust-docs --toolchain stable-aarch64-apple-darwin");
        sys.on(&remove, Ran::ok(""));
        let rows = run(&sys, Mode::Apply);
        assert_eq!(rows[1].outcome, Outcome::Failed, "{rows:?}");
    }

    #[test]
    fn a_minimal_mac_is_ok_and_one_without_rustup_has_nothing_to_do() {
        let sys = mac("minimal\n", WITHOUT);
        let rows = run(&sys, Mode::Apply);
        assert!(rows.iter().all(|r| r.outcome == Outcome::Ok), "{rows:?}");
        let bare = Fake::root();
        let rows = run(&bare, Mode::Apply);
        assert!(
            rows.iter().all(|r| r.outcome == Outcome::NotApplicable),
            "{rows:?}"
        );
        assert!(bare.calls().is_empty(), "{:?}", bare.calls());
    }
}
