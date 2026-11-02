// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! ALAB HARNESS PROGRAMS: `<agent>-alab`, one program per vendor agent, each in its own
//! repo with its own version, running the vendor's unmodified CLI as its child
//! (`docs/DESIGN-harness-repos-2026-10-24.md`).
//!
//! The one-word rule (that page's §1) makes a harness's repo, program and holder id one
//! word ending in [`SUFFIX`], so the name alone says what a program is and no list has to
//! be kept. codex-alab is not one of them here: its program is `codex`, which ALab builds
//! from source (repo `codex-alab`, [`crate::alab::ALAB_AGENTS`]).
//!
//! A harness is in the default set like every program the index lists
//! ([`crate::manifest::Index::installable`]): the passes install it unasked and keep it current,
//! and only `[packages].exclude` or a person's uninstall keeps it out (owner ruling 2026-10-27,
//! "everything must be ON BY DEFAULT in atpkg!!!", that page's §11 D45). The name still decides
//! two things: no `alab-<tool>` alias is laid for a harness (D13), and only the harness
//! [`twin_harness`] names runs a vendor's sessions.

/// The suffix the one-word rule gives every harness program.
pub const SUFFIX: &str = "-alab";

/// Whether `program` names an ALab harness program: a non-empty word, then [`SUFFIX`].
#[must_use]
pub const fn is_harness(program: &str) -> bool {
    let name = program.as_bytes();
    let tail = SUFFIX.as_bytes();
    if name.len() <= tail.len() {
        return false;
    }
    let at = name.len() - tail.len();
    let mut i = 0;
    while i < tail.len() {
        if name[at + i] != tail[i] {
            return false;
        }
        i += 1;
    }
    true
}

/// The harness a vendor program's `agents/` twin hands its session to once that harness is
/// installed, as `(program, tool)`: the harness that serves aterm's harness contract 3 for that
/// agent (`crates/aterm-agent/src/harness/contract.rs`, `ROSTER`;
/// `docs/DESIGN-harness-repos-2026-10-24.md` §9 step 6). Only grok-alab today: a harness joins
/// here in the aterm change that rosters it, never before (its claim would be refused and it
/// would only run the vendor in its place).
#[must_use]
pub const fn twin_harness(vendor: &str) -> Option<(&'static str, &'static str)> {
    match vendor.as_bytes() {
        b"grok" => Some(("grok-alab", "grok-alab")),
        _ => None,
    }
}

/// One harness package installed in a store, as `aterm harness versions` lists it
/// (`docs/DESIGN-harness-repos-2026-10-24.md` §5): an `-alab` harness program, or an agent
/// ALab builds itself whose active build is an ALab build ([`crate::alab::ALAB_AGENTS`],
/// codex-alab), each either a live store build or a dev link.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Installed {
    /// The harness's own name: the program for an `-alab` harness, the repo (`codex-alab`)
    /// for an agent ALab builds.
    pub harness: String,
    /// The store program that holds it (`grok-alab`, `codex`).
    pub program: String,
    /// The live store build, `None` for a dev link.
    pub build: Option<u64>,
    /// The version its signed manifest named (`<build>.version`,
    /// [`crate::store::recorded_version`]; an ALab build's own package version,
    /// [`crate::alab::package_version`]), or a dev link's package version where it has one;
    /// `None` where nothing records one, never a guess.
    pub version: Option<String>,
    /// The checkout a dev link names.
    pub linked: Option<std::path::PathBuf>,
}

/// The harness packages installed in `layout` ([`Installed`]), ordered by harness name. A
/// vendor build of an agent ALab builds (the vendor's own bytes) is no harness and is not
/// listed.
#[must_use]
pub fn installed(layout: &crate::store::Layout) -> Vec<Installed> {
    let active = crate::ops::active_builds(layout);
    let linked = crate::linkmode::linked_programs(layout);
    let mut programs: std::collections::BTreeSet<&str> = active
        .keys()
        .map(String::as_str)
        .chain(linked.iter().map(String::as_str))
        .collect();
    programs.retain(|p| is_harness(p) || crate::alab::agent(p).is_some());
    let mut out: Vec<Installed> = programs
        .into_iter()
        .filter_map(|program| {
            let agent = crate::alab::agent(program);
            let harness = agent.map_or(program, |a| a.repo).to_string();
            if let Some(checkout) = crate::linkmode::linked_checkout(layout, program) {
                let version = agent.and_then(|_| crate::alab::package_version(&checkout));
                return Some(Installed {
                    harness,
                    program: program.to_string(),
                    build: None,
                    version,
                    linked: Some(checkout),
                });
            }
            let build = *active.get(program)?;
            let version = if agent.is_some() {
                if !crate::alab::build_is_alab(layout, program, build) {
                    return None;
                }
                crate::alab::package_version(&layout.build_dir(program, build))
            } else {
                crate::store::recorded_version(&layout.build_dir(program, build))
            };
            Some(Installed {
                harness,
                program: program.to_string(),
                build: Some(build),
                version,
                linked: None,
            })
        })
        .collect();
    out.sort_by(|a, b| a.harness.cmp(&b.harness));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// THE FIVE HARNESSES ARE HARNESSES AND NOTHING ELSE IS: the vendor programs they wrap,
    /// codex (built from source, not a harness here), the toolchain programs and a bare
    /// suffix are not.
    #[test]
    fn a_harness_is_a_word_then_the_suffix() {
        for h in [
            "claude-code-alab",
            "muse-alab",
            "agy-alab",
            "grok-alab",
            "kimi-alab",
        ] {
            assert!(is_harness(h), "{h}");
        }
        for p in [
            "claude",
            "muse",
            "agy",
            "grok",
            "kimi",
            "codex",
            "codex-old",
            "ay",
            "trust",
            "trust-vc",
            "alab",
            "-alab",
            "alab-ay",
            "muse-alab2",
            "",
        ] {
            assert!(!is_harness(p), "{p:?}");
        }
        const _: () = assert!(is_harness("x-alab") && !is_harness("-alab"));
    }

    /// Only grok's twin hands its session to a harness, grok-alab, which is a harness program;
    /// no other vendor's does yet, and no harness's own name does.
    #[test]
    fn only_grok_hands_its_session_to_grok_alab() {
        assert_eq!(twin_harness("grok"), Some(("grok-alab", "grok-alab")));
        assert!(is_harness("grok-alab"));
        for other in [
            "claude",
            "codex",
            "codex-old",
            "agy",
            "muse",
            "kimi",
            "grok-alab",
            "Grok",
            "",
        ] {
            assert_eq!(twin_harness(other), None, "{other:?}");
        }
    }

    /// A complete, active store build of `program` at `build` that puts `tool` on PATH.
    fn lay(l: &crate::store::Layout, program: &str, build: u64, tool: &str) -> std::path::PathBuf {
        let dir = l.build_dir(program, build);
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        let exe = dir.join("bin").join(tool);
        std::fs::write(&exe, b"#!/bin/sh\nexit 0\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        crate::store::mark_build_ready(&dir).unwrap();
        crate::activate::activate_build(l, &dir).unwrap();
        crate::activate::install_shims(l, &dir, &[tool.to_string()], crate::Aliases::Off).unwrap();
        dir
    }

    /// Make `dir` a codex package naming `version`, an ALab build when `alab`.
    fn codex_package(dir: &std::path::Path, version: &str, alab: bool) {
        std::fs::write(
            dir.join(crate::alab::PACKAGE_JSON),
            format!(
                r#"{{"layoutVersion":1,"version":"{version}","target":"aarch64-apple-darwin","entrypoint":"bin/codex"}}"#
            ),
        )
        .unwrap();
        if alab {
            std::fs::write(dir.join(crate::alab::ALAB_BUILD_JSON), br#"{"source":"0"}"#).unwrap();
        }
    }

    /// THE INSTALLED HARNESS PACKAGES, EACH WITH ITS RECORDED VERSION: a harness build reads its
    /// `.version` record, codex reads its ALab package's version as codex-alab, a dev-linked
    /// harness names its checkout, a harness build staged before the record reads no version
    /// (never a guess); a non-harness program and a codex vendor build are not listed (the
    /// controls: both are active in the same store).
    #[test]
    fn installed_lists_each_harness_package_with_its_recorded_version() {
        let l = crate::testdir::layout("atpkg-harness-installed-");
        assert_eq!(installed(&l), Vec::new(), "an empty store lists none");
        let grok = lay(&l, "grok-alab", 17, "grok-alab");
        crate::store::write_version(&grok, "0.3.0").unwrap();
        lay(&l, "muse-alab", 4, "muse-alab");
        let ay = lay(&l, "ay", 9, "ay");
        crate::store::write_version(&ay, "0.9.0").unwrap();
        let codex = lay(&l, "codex", crate::alab::tests::BUILD, "codex");
        codex_package(&codex, "0.164.0+codex.0.130.0", true);
        let checkout = l.prefix.join("kimi-checkout");
        std::fs::create_dir_all(checkout.join("bin")).unwrap();
        std::fs::write(
            checkout.join("bin").join("kimi-alab"),
            b"#!/bin/sh\nexit 0\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(
                checkout.join("bin").join("kimi-alab"),
                std::fs::Permissions::from_mode(0o755),
            )
            .unwrap();
        }
        crate::linkmode::link(
            &l,
            "kimi-alab",
            &checkout,
            &[std::path::PathBuf::from("bin/kimi-alab")],
        )
        .unwrap();
        let rows = installed(&l);
        /// (harness, program, build, version, linked)
        type Shown<'a> = (&'a str, &'a str, Option<u64>, Option<&'a str>, bool);
        let shown: Vec<Shown<'_>> = rows
            .iter()
            .map(|r| {
                (
                    r.harness.as_str(),
                    r.program.as_str(),
                    r.build,
                    r.version.as_deref(),
                    r.linked.is_some(),
                )
            })
            .collect();
        assert_eq!(
            shown,
            vec![
                (
                    "codex-alab",
                    "codex",
                    Some(crate::alab::tests::BUILD),
                    Some("0.164.0+codex.0.130.0"),
                    false
                ),
                ("grok-alab", "grok-alab", Some(17), Some("0.3.0"), false),
                ("kimi-alab", "kimi-alab", None, None, true),
                ("muse-alab", "muse-alab", Some(4), None, false),
            ]
        );
        let kimi = rows.iter().find(|r| r.harness == "kimi-alab").unwrap();
        assert_eq!(
            kimi.linked
                .as_deref()
                .map(std::fs::canonicalize)
                .transpose()
                .unwrap(),
            Some(std::fs::canonicalize(&checkout).unwrap())
        );

        // A codex build that is not an ALab package is the vendor's, not codex-alab.
        let v = crate::testdir::layout("atpkg-harness-installed-vendor-");
        let vendor = lay(&v, "codex", crate::alab::tests::BUILD, "codex");
        codex_package(&vendor, "0.165.0", false);
        assert_eq!(installed(&v), Vec::new(), "a vendor codex is no harness");
        codex_package(&vendor, "0.165.0", true);
        assert_eq!(
            installed(&v).first().map(|r| r.harness.as_str()),
            Some("codex-alab"),
            "the same build with the ALab record is (control)"
        );
    }
}
