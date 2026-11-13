// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn a_toolchains_lib_leads_the_loader_path_and_keeps_what_was_inherited() {
    let lib = Path::new("/r/toolchains/stable/lib");
    let joined = |var, inherited: Option<&str>, home: Option<&str>| {
        loader_path(var, lib, inherited.map(OsStr::new), home.map(OsStr::new))
    };
    let dyld = "DYLD_FALLBACK_LIBRARY_PATH";
    assert_eq!(
        joined(dyld, Some("/a:/b"), Some("/h")),
        "/r/toolchains/stable/lib:/a:/b"
    );
    for unset in [None, Some("")] {
        assert_eq!(
            joined(dyld, unset, Some("/h")),
            "/r/toolchains/stable/lib:/h/lib:/usr/local/lib:/usr/lib"
        );
    }
    assert_eq!(
        joined(dyld, None, None),
        "/r/toolchains/stable/lib:/usr/local/lib:/usr/lib"
    );
    let ld = "LD_LIBRARY_PATH";
    assert_eq!(joined(ld, None, Some("/h")), "/r/toolchains/stable/lib");
    assert_eq!(joined(ld, Some("/a"), None), "/r/toolchains/stable/lib:/a");
}

#[test]
fn the_loader_path_is_the_lib_beside_the_toolchains_bin() {
    let at = |rustc: &str| Toolchain {
        name: "stable".into(),
        cargo: PathBuf::from("/r/toolchains/stable/bin/cargo"),
        rustc: PathBuf::from(rustc),
        clippy: false,
    };
    let Some(var) = LOADER_VAR else {
        assert_eq!(at("/r/toolchains/stable/bin/rustc").loader_env(), None);
        return;
    };
    let (named, value) = at("/r/toolchains/stable/bin/rustc")
        .loader_env()
        .expect("a toolchain's rustc");
    assert_eq!(named, var);
    assert!(
        value
            .to_string_lossy()
            .starts_with("/r/toolchains/stable/lib"),
        "{value:?}"
    );
    assert_eq!(
        at("rustc").loader_env(),
        None,
        "no toolchain around a bare rustc"
    );
}

const HOST: &str = "aarch64-apple-darwin";
const PIN: &str = "1.97.1";

/// A rustup holding `toolchains` in its listing's order, each with its stds and whether it
/// has clippy, whose drivers are files under `dir`; it records every question.
struct Held {
    dir: aterm_tempfile::TempDir,
    toolchains: Vec<(&'static str, Vec<&'static str>, bool)>,
    asked: std::cell::RefCell<Vec<String>>,
}

impl Held {
    fn new(toolchains: Vec<(&'static str, Vec<&'static str>, bool)>) -> Self {
        Self {
            dir: aterm_tempfile::tempdir().unwrap(),
            toolchains,
            asked: std::cell::RefCell::default(),
        }
    }

    fn held(&self, name: &str) -> Option<&(&'static str, Vec<&'static str>, bool)> {
        self.toolchains.iter().find(|(t, _, _)| *t == name)
    }
}

impl Ask for Held {
    fn ask(&self, args: &[&str]) -> Result<String, String> {
        self.asked.borrow_mut().push(args.join(" "));
        match args {
            ["toolchain", "list"] => Ok(self
                .toolchains
                .iter()
                .enumerate()
                .map(|(i, (t, _, _))| {
                    format!("{t}-{HOST}{}\n", if i == 0 { " (default)" } else { "" })
                })
                .collect()),
            ["target", "list", "--installed", "--toolchain", name] => self
                .held(name)
                .map(|(_, stds, _)| stds.iter().map(|s| format!("{s}\n")).collect())
                .ok_or_else(|| format!("toolchain '{name}' is not installed")),
            ["which", "--toolchain", name, tool] => {
                let (_, _, clippy) = self
                    .held(name)
                    .ok_or_else(|| format!("toolchain '{name}' is not installed"))?;
                if *tool == "cargo-clippy" && !clippy {
                    return Err(format!("'{tool}' is not installed for '{name}'"));
                }
                let path = self.dir.path().join(name).join("bin").join(tool);
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(&path, "").unwrap();
                Ok(format!("{}\n", path.display()))
            }
            other => Err(format!("unexpected rustup question {other:?}")),
        }
    }
}

const EVERY: [&str; 7] = crate::provision::CELL_TARGETS;

/// EVERY LANE RIDES THE PIN, NEVER THE FIRST TOOLCHAIN THAT HAS THE STD: with `stable` listed
/// first (rustup's own order) and carrying every cell's std and clippy, each cell's std is
/// served by the pin's own drivers. NEGATIVE CONTROL: a choice that takes the first listed
/// toolchain carrying the std answers `stable` here, and the name and path checks fail.
#[test]
fn every_lane_rides_the_pin_though_stable_is_listed_first_with_every_std() {
    let rustup = Held::new(vec![
        ("stable", EVERY.to_vec(), true),
        (PIN, EVERY.to_vec(), true),
        ("nightly", EVERY.to_vec(), true),
    ]);
    let stock = stock(&rustup, PIN, HOST).unwrap();
    for triple in EVERY {
        let served = stock.serving(triple, true).unwrap();
        assert_eq!(served.name, PIN, "{triple}");
        assert!(
            served.cargo.starts_with(rustup.dir.path().join(PIN))
                && served.rustc.starts_with(rustup.dir.path().join(PIN)),
            "{triple}: {served:?}"
        );
    }
    let asked = rustup.asked.borrow();
    assert!(
        asked
            .iter()
            .filter(|q| *q != "toolchain list")
            .all(|q| q.contains(&format!("--toolchain {PIN}"))),
        "nothing but the pin is asked about: {asked:#?}"
    );
}

/// A PIN THAT LACKS A PIECE IS UNREADY, NAMING `verify provision`, AND NOTHING STANDS IN: the
/// pin without a cell's std, without clippy, or not held at all is refused though `stable`
/// carries all of it; a lane that only compiles is served without clippy. NEGATIVE CONTROL:
/// a `serving` that skips the std check serves the Windows cell, and its assertion fails.
#[test]
fn a_pin_missing_its_std_its_clippy_or_itself_is_unready_and_names_provision() {
    let rustup = Held::new(vec![
        ("stable", EVERY.to_vec(), true),
        (PIN, vec![HOST, "wasm32-unknown-unknown"], false),
    ]);
    let stock = stock(&rustup, PIN, HOST).unwrap();
    let no_std = stock.serving("x86_64-pc-windows-msvc", false).unwrap_err();
    assert_eq!(no_std.fix, PROVISION);
    assert!(
        no_std.why.contains("`1.97.1`") && no_std.why.contains("x86_64-pc-windows-msvc std"),
        "{no_std}"
    );
    let no_clippy = stock.serving(HOST, true).unwrap_err();
    assert_eq!(no_clippy.fix, PROVISION);
    assert!(no_clippy.why.contains("has no clippy"), "{no_clippy}");
    assert_eq!(
        stock
            .serving("wasm32-unknown-unknown", false)
            .map(|t| t.name.as_str()),
        Ok(PIN),
        "a lane that only compiles needs no clippy"
    );

    let without = Held::new(vec![("stable", EVERY.to_vec(), true)]);
    let absent = super::stock(&without, PIN, HOST).unwrap_err();
    assert_eq!(absent.fix, PROVISION);
    assert!(absent.why.contains("holds no `1.97.1`"), "{absent}");
    assert_eq!(
        *without.asked.borrow(),
        ["toolchain list"],
        "a pin rustup does not list is asked nothing more"
    );
    assert!(
        absent
            .to_string()
            .ends_with("(fix: targo --unverified verify provision rustup)"),
        "{absent}"
    );
}

/// A pin rustup holds only for another host is not held, and rustup is asked nothing more.
#[test]
fn a_pin_held_only_for_another_host_is_unready() {
    let rustup = Held::new(vec![(PIN, EVERY.to_vec(), true)]);
    let other = stock(&rustup, PIN, "x86_64-apple-darwin").unwrap_err();
    assert!(other.why.contains("holds no `1.97.1`"), "{other}");
    assert_eq!(*rustup.asked.borrow(), ["toolchain list"]);
}

/// Unready reads as one line, rustup's own lines joined, with the fix last.
#[test]
fn an_unready_pin_reads_as_one_line_ending_with_its_fix() {
    let unready = Unready::provision(
        "`rustup which` failed: error: toolchain '1.97.1' is not installed\n\
         help: run `rustup toolchain install 1.97.1` to install it\n",
    );
    let said = unready.to_string();
    assert_eq!(said.lines().count(), 1, "{said}");
    assert!(said.ends_with(&format!("(fix: {PROVISION})")), "{said}");
    assert!(said.contains("is not installed help: run"), "{said}");
}

/// The checkout's pin, and a public snapshot's (no `publish/`), is [`STOCK_PIN`], asked of
/// rustup; an engine gate the checkout's reader refuses is unready with the gate's own fix,
/// and rustup is asked nothing.
#[test]
fn the_checkout_and_a_public_snapshot_ride_the_pin_and_a_gate_that_disagrees_is_unready() {
    let repo = aterm_tempfile::manifest_dir().join("../..");
    let public = aterm_tempfile::tempdir().unwrap();
    for tree in [repo.as_path(), public.path()] {
        let rustup = Held::new(vec![("stable", EVERY.to_vec(), true)]);
        let absent = pinned(&rustup, tree).unwrap_err();
        assert!(
            absent.why.contains(&format!("holds no `{STOCK_PIN}`")),
            "{}: {absent}",
            tree.display()
        );
        assert_eq!(absent.fix, PROVISION);
    }

    std::fs::create_dir_all(public.path().join("publish")).unwrap();
    std::fs::write(
        public.path().join(STOCK_PIN_FILE),
        "CHECK_CMD_DEFAULT=\"true\"\n",
    )
    .unwrap();
    let rustup = Held::new(vec![(STOCK_PIN, EVERY.to_vec(), true)]);
    let refused = pinned(&rustup, public.path()).unwrap_err();
    assert!(refused.why.contains("no wasm32 clause"), "{refused}");
    assert!(refused.fix.contains("restore"), "{refused}");
    assert!(
        rustup.asked.borrow().is_empty(),
        "a gate that is refused asks rustup nothing"
    );
}
