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

/// The receipt a local harness artifact directory carries (`atpkg-publish build-local`).
pub const LOCAL_BUILD_JSON: &str = "local-build.json";

/// Bound on a [`LOCAL_BUILD_JSON`] receipt read.
const MAX_LOCAL_BUILD_JSON_BYTES: usize = 64 * 1024;

/// Whether `version` is a canonical harness version (`MAJOR.MINOR.0` or `MAJOR.MINOR.0+g<hex9..40>`,
/// or strictly `+g`-stamped when `require_stamp` is true) and matches `source` when provided.
fn harness_version_valid(version: &str, source: Option<&str>, require_stamp: bool) -> bool {
    if version.is_empty() || version.len() > 128 || !version.bytes().all(|b| b.is_ascii_graphic()) {
        return false;
    }
    let (base, stamp) = match version.split_once("+g") {
        Some((base, stamp)) => (base, Some(stamp)),
        None if require_stamp || source.is_some() => return false,
        None => (version, None),
    };
    let parts: Vec<&str> = base.split('.').collect();
    if parts.len() != 3
        || parts[2] != "0"
        || !parts[..2].iter().all(|s| {
            !s.is_empty()
                && s.bytes().all(|b| b.is_ascii_digit())
                && (s.len() == 1 || !s.starts_with('0'))
                && s.parse::<u64>().is_ok()
        })
    {
        return false;
    }
    if let Some(stamp) = stamp {
        if !(9..=40).contains(&stamp.len())
            || !stamp
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return false;
        }
        if let Some(src) = source
            && (src.len() != 40
                || !src
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                || !src.starts_with(stamp))
        {
            return false;
        }
    }
    true
}

/// Whether `s` is a lowercase hexadecimal digest of `len` bytes.
fn hex_len(s: &str, len: usize) -> bool {
    s.len() == len
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// A local harness artifact's validated `(version, bin/<command>)` from [`LOCAL_BUILD_JSON`].
fn local_build_manifest(
    root: &std::path::Path,
    program: &str,
) -> Option<(String, std::path::PathBuf)> {
    if !is_harness(program) {
        return None;
    }
    let root_meta = std::fs::symlink_metadata(root).ok()?;
    if !root_meta.file_type().is_dir() || crate::platform::is_reparse(&root_meta) {
        return None;
    }
    let text = crate::metadata_io::read_bounded_regular_utf8(
        &root.join(LOCAL_BUILD_JSON),
        MAX_LOCAL_BUILD_JSON_BYTES,
    )
    .ok()?;
    let v: aterm_json::Value = aterm_json::from_str(&text).ok()?;
    if v.get("schema").and_then(aterm_json::Value::as_u64) != Some(2)
        || v.get("origin").and_then(aterm_json::Value::as_str) != Some("local-build")
        || v.get("program").and_then(aterm_json::Value::as_str) != Some(program)
    {
        return None;
    }
    let command = v.get("command").and_then(aterm_json::Value::as_str)?;
    if command != default_harness_tool(program) {
        return None;
    }
    crate::store::ToolName::new(command)?;
    if let Some(build) = v.get("build_number")
        && build.as_u64().is_none_or(|n| n == 0)
    {
        return None;
    }
    if let Some(policy) = v.get("source_policy")
        && policy.as_str() != Some("clean-main")
    {
        return None;
    }
    if let Some(target) = v.get("target")
        && target.as_str() != Some(crate::cli::current_triple())
    {
        return None;
    }
    if let Some(policy) = v.get("policy")
        && !matches!(policy.as_str(), Some("repository" | "unverified"))
    {
        return None;
    }
    if let Some(profile) = v.get("profile")
        && !matches!(profile.as_str(), Some("release" | "dev"))
    {
        return None;
    }
    if let Some(compiler) = v.get("compiler")
        && !compiler.as_str().is_some_and(|c| hex_len(c, 40))
    {
        return None;
    }
    for key in ["sysroot", "checkout", "logs"] {
        if let Some(path) = v.get(key)
            && !path
                .as_str()
                .is_some_and(|s| std::path::Path::new(s).is_absolute() && !s.contains('\0'))
        {
            return None;
        }
    }
    if let Some(exposes) = v.get("exposes") {
        let [only] = exposes.as_array()?.as_slice() else {
            return None;
        };
        if only.as_str() != Some(command) {
            return None;
        }
    }
    let bin_meta = std::fs::symlink_metadata(root.join("bin")).ok()?;
    if !bin_meta.file_type().is_dir() || crate::platform::is_reparse(&bin_meta) {
        return None;
    }
    let entry = std::path::Path::new("bin").join(command);
    let exe_path = root.join(&entry);
    let meta = std::fs::symlink_metadata(&exe_path).ok()?;
    if !meta.file_type().is_file() || crate::platform::is_reparse(&meta) || meta.len() == 0 {
        return None;
    }
    if let Some(executables) = v.get("executables") {
        let [exe] = executables.as_array()?.as_slice() else {
            return None;
        };
        let expected_path = format!("bin/{command}");
        let size = exe.get("size").and_then(aterm_json::Value::as_u64)?;
        let sha256 = exe.get("sha256").and_then(aterm_json::Value::as_str)?;
        if exe.get("path").and_then(aterm_json::Value::as_str) != Some(expected_path.as_str())
            || size != meta.len()
            || !hex_len(sha256, 64)
            || crate::sha256_file(&exe_path).ok().as_deref() != Some(sha256)
        {
            return None;
        }
    }
    let version = v.get("version").and_then(aterm_json::Value::as_str)?;
    let source = match v.get("source") {
        Some(src) => Some(src.as_str()?),
        None => None,
    };
    harness_version_valid(version, source, true).then(|| (version.to_string(), entry))
}

/// The producer-stamped version a local harness artifact root `root` records in
/// [`LOCAL_BUILD_JSON`] for `program`, or `None` for a bare checkout or mismatched record.
#[must_use]
pub fn local_build_version(root: &std::path::Path, program: &str) -> Option<String> {
    local_build_manifest(root, program).map(|(version, _)| version)
}

/// The recorded local-build version whose validated `bin/<command>` entrypoint is `exe` for
/// `program`, independent of the currently selected store build or development link.
#[must_use]
pub fn local_build_version_of_exe(exe: &std::path::Path, program: &str) -> Option<String> {
    let exe = std::fs::canonicalize(exe).ok()?;
    exe.ancestors().skip(1).take(2).find_map(|root| {
        let (version, entry) = local_build_manifest(root, program)?;
        (std::fs::canonicalize(root.join(entry)).ok().as_deref() == Some(exe.as_path()))
            .then_some(version)
    })
}

/// The default exposed tool name for a harness `program` (`claude-alab` for `claude-code-alab`,
/// `program` for all other `-alab` harnesses).
pub(crate) const fn default_harness_tool(program: &str) -> &str {
    match program.as_bytes() {
        b"claude-code-alab" => "claude-alab",
        _ => program,
    }
}

/// The producer-stamped version of the local harness artifact that `program`'s current
/// development link selects, or `None` when `program` is not a live dev-linked harness artifact.
#[must_use]
pub fn linked_local_build_version(layout: &crate::store::Layout, program: &str) -> Option<String> {
    if !is_harness(program) {
        return None;
    }
    let checkout = crate::linkmode::live_linked_checkout(layout, program).ok()??;
    let (version, entry) = local_build_manifest(&checkout, program)?;
    let tool = entry.file_name()?.to_str()?;
    let exe = crate::ops::which(layout, tool)?.canonicalize().ok()?;
    (std::fs::canonicalize(checkout.join(entry)).ok().as_deref() == Some(exe.as_path()))
        .then_some(version)
}

/// The recorded store version for a ready harness build of `program` whose exposed executable is
/// `exe`, independent of the currently selected store build or development link.
#[must_use]
pub fn store_build_version_of_exe(
    layout: &crate::store::Layout,
    program: &str,
    exe: &std::path::Path,
) -> Option<String> {
    if !is_harness(program) {
        return None;
    }
    let exe = std::fs::canonicalize(exe).ok()?;
    let store = std::fs::canonicalize(layout.prefix.join("store").join(program)).ok()?;
    let rest = exe.strip_prefix(&store).ok()?;
    let mut parts = rest.components();
    let build = parts.next()?.as_os_str().to_str()?;
    let build = build
        .parse::<u64>()
        .ok()
        .filter(|b| b.to_string() == build)?;
    if parts.next()?.as_os_str() != "bin" {
        return None;
    }
    let tool = crate::store::ToolName::new(parts.next()?.as_os_str().to_str()?)?;
    if parts.next().is_some() || tool.as_str() != default_harness_tool(program) {
        return None;
    }
    let dir = layout.build_dir(program, build);
    let dir_meta = std::fs::symlink_metadata(&dir).ok()?;
    let bin_meta = std::fs::symlink_metadata(dir.join("bin")).ok()?;
    let exe_meta = std::fs::symlink_metadata(dir.join("bin").join(tool.as_str())).ok()?;
    if !dir_meta.file_type().is_dir()
        || crate::platform::is_reparse(&dir_meta)
        || !bin_meta.file_type().is_dir()
        || crate::platform::is_reparse(&bin_meta)
        || !exe_meta.file_type().is_file()
        || crate::platform::is_reparse(&exe_meta)
        || exe_meta.len() == 0
        || !crate::store::build_is_complete(&dir)
    {
        return None;
    }
    if let Some(exposes) = crate::store::recorded_exposes(&dir)
        && exposes.as_slice() != [tool]
    {
        return None;
    }
    let version = crate::store::recorded_version(&dir)?;
    harness_version_valid(&version, None, false).then_some(version)
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
                let tool = if agent.is_some() {
                    program
                } else {
                    default_harness_tool(program)
                };
                let version = match crate::ops::which(layout, tool) {
                    Some(_) if agent.is_some() => {
                        crate::alab::linked_package_version(layout, program)
                    }
                    Some(_) => linked_local_build_version(layout, program),
                    None if agent.is_some() => crate::alab::is_alab_package(&checkout)
                        .then(|| crate::alab::package_version(&checkout))
                        .flatten(),
                    None => local_build_version(&checkout, program),
                };
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
                    .filter(|v| harness_version_valid(v, None, false))
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

        // A dev-linked local harness artifact with a valid local-build.json reports its version.
        std::fs::write(
            checkout.join(LOCAL_BUILD_JSON),
            r#"{"schema":2,"origin":"local-build","program":"kimi-alab","command":"kimi-alab","version":"0.1.0+gaa5d5fe00"}"#,
        )
        .unwrap();
        let kimi_stamped = installed(&l)
            .into_iter()
            .find(|r| r.harness == "kimi-alab")
            .unwrap();
        assert_eq!(kimi_stamped.version.as_deref(), Some("0.1.0+gaa5d5fe00"));
        assert_eq!(
            linked_local_build_version(&l, "kimi-alab").as_deref(),
            Some("0.1.0+gaa5d5fe00")
        );
        assert_eq!(linked_local_build_version(&l, "codex"), None);
        assert_eq!(
            local_build_version_of_exe(&checkout.join("bin/kimi-alab"), "kimi-alab").as_deref(),
            Some("0.1.0+gaa5d5fe00")
        );
        assert_eq!(
            local_build_version_of_exe(&checkout.join("bin/kimi-alab"), "grok-alab"),
            None,
            "mismatched program is refused"
        );
        for bad in [
            r#"{"schema":2,"origin":"local-build","program":"kimi-alab","command":"kimi-alab","version":"0.1.0"}"#,
            r#"{"schema":2,"origin":"local-build","program":"kimi-alab","command":"kimi-alab","version":"0.1.0 evil"}"#,
            r#"{"schema":2,"origin":"local-build","program":"kimi-alab","command":"kimi-alab","version":"0.1.2+gaa5d5fe00"}"#,
            r#"{"schema":2,"origin":"local-build","program":"kimi-alab","command":"kimi-alab","source":"1111111111111111111111111111111111111111","version":"0.1.0+gaa5d5fe00"}"#,
            r#"{"schema":2,"origin":"local-build","program":"kimi-alab","command":"kimi-alab","source":123,"version":"0.1.0+gaa5d5fe00"}"#,
            r#"{"schema":2,"origin":"local-build","program":"kimi-alab","command":"kimi-alab","source_policy":"dirty","version":"0.1.0+gaa5d5fe00"}"#,
            r#"{"schema":2,"origin":"local-build","program":"kimi-alab","command":"kimi-alab","target":"another-target","version":"0.1.0+gaa5d5fe00"}"#,
            r#"{"schema":2,"origin":"local-build","program":"kimi-alab","command":"kimi-alab","policy":"signed","version":"0.1.0+gaa5d5fe00"}"#,
            r#"{"schema":2,"origin":"local-build","program":"kimi-alab","command":"kimi-alab","profile":"bench","version":"0.1.0+gaa5d5fe00"}"#,
            r#"{"schema":2,"origin":"local-build","program":"kimi-alab","command":"kimi-alab","compiler":"short","version":"0.1.0+gaa5d5fe00"}"#,
            r#"{"schema":2,"origin":"local-build","program":"kimi-alab","command":"kimi-alab","checkout":"relative/path","version":"0.1.0+gaa5d5fe00"}"#,
            r#"{"schema":2,"origin":"local-build","program":"kimi-alab","command":"kimi-alab","build_number":0,"version":"0.1.0+gaa5d5fe00"}"#,
            r#"{"schema":2,"origin":"local-build","program":"kimi-alab","command":"kimi-alab","build_number":"1","version":"0.1.0+gaa5d5fe00"}"#,
            r#"{"schema":2,"origin":"local-build","program":"kimi-alab","command":"other","version":"0.1.0+gaa5d5fe00"}"#,
            r#"{"schema":2,"origin":"local-build","program":"kimi-alab","command":"kimi-alab","exposes":["other"],"version":"0.1.0+gaa5d5fe00"}"#,
            r#"{"schema":2,"origin":"local-build","program":"kimi-alab","command":"kimi-alab","executables":[{"path":"bin/kimi-alab","size":999,"sha256":"0000000000000000000000000000000000000000000000000000000000000000"}],"version":"0.1.0+gaa5d5fe00"}"#,
            r#"{"schema":2,"origin":"local-build","program":"kimi-alab","command":"kimi-alab","executables":[{"path":"bin/kimi-alab","size":17,"sha256":"0000000000000000000000000000000000000000000000000000000000000000"}],"version":"0.1.0+gaa5d5fe00"}"#,
        ] {
            std::fs::write(checkout.join(LOCAL_BUILD_JSON), bad).unwrap();
            assert_eq!(
                local_build_version(&checkout, "kimi-alab"),
                None,
                "malformed version or source mismatch is refused: {bad}"
            );
        }
        let kimi_sha = crate::sha256_file(&checkout.join("bin/kimi-alab")).unwrap();
        std::fs::write(
            checkout.join(LOCAL_BUILD_JSON),
            format!(
                r#"{{"schema":2,"origin":"local-build","program":"kimi-alab","command":"kimi-alab","target":{:?},"policy":"unverified","profile":"release","compiler":"20565c3255adf151f9eb920e89cdf83c38f84abe","executables":[{{"path":"bin/kimi-alab","size":17,"sha256":{:?}}}],"version":"0.1.0+gaa5d5fe00"}}"#,
                crate::cli::current_triple(),
                kimi_sha
            ),
        )
        .unwrap();
        assert_eq!(
            local_build_version(&checkout, "kimi-alab").as_deref(),
            Some("0.1.0+gaa5d5fe00")
        );

        #[cfg(unix)]
        {
            let sym_checkout = l.prefix.join("kimi-sym-bin");
            std::fs::create_dir_all(&sym_checkout).unwrap();
            std::os::unix::fs::symlink(checkout.join("bin"), sym_checkout.join("bin")).unwrap();
            std::fs::write(
                sym_checkout.join(LOCAL_BUILD_JSON),
                r#"{"schema":2,"origin":"local-build","program":"kimi-alab","command":"kimi-alab","version":"0.1.0+gaa5d5fe00"}"#,
            )
            .unwrap();
            assert_eq!(
                local_build_version(&sym_checkout, "kimi-alab"),
                None,
                "symlinked bin/ directory in local-build root is refused"
            );
        }

        // claude-code-alab's exposed tool is claude-alab, not claude-code-alab.
        let cc_checkout = l.prefix.join("claude-checkout");
        std::fs::create_dir_all(cc_checkout.join("bin")).unwrap();
        let cc_exe = cc_checkout.join("bin/claude-alab");
        std::fs::write(&cc_exe, b"#!/bin/sh\nexit 0\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&cc_exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        std::fs::write(
            cc_checkout.join(LOCAL_BUILD_JSON),
            r#"{"schema":2,"origin":"local-build","program":"claude-code-alab","command":"claude-alab","version":"0.8.0+g123456789"}"#,
        )
        .unwrap();
        crate::linkmode::link(
            &l,
            "claude-code-alab",
            &cc_checkout,
            &[std::path::PathBuf::from("bin/claude-alab")],
        )
        .unwrap();
        assert_eq!(
            linked_local_build_version(&l, "claude-code-alab").as_deref(),
            Some("0.8.0+g123456789")
        );
        let cc_row = installed(&l)
            .into_iter()
            .find(|r| r.harness == "claude-code-alab")
            .unwrap();
        assert_eq!(cc_row.version.as_deref(), Some("0.8.0+g123456789"));

        let cc_store = l.build_dir("claude-code-alab", 5);
        std::fs::create_dir_all(cc_store.join("bin")).unwrap();
        let cc_store_exe = cc_store.join("bin/claude-alab");
        let cc_wrong_exe = cc_store.join("bin/claude-code-alab");
        std::fs::write(&cc_store_exe, b"#!/bin/sh\nexit 0\n").unwrap();
        std::fs::write(&cc_wrong_exe, b"#!/bin/sh\nexit 0\n").unwrap();
        crate::store::mark_build_ready(&cc_store).unwrap();
        crate::store::write_version(&cc_store, "0.8.0+g987654321").unwrap();
        assert_eq!(
            store_build_version_of_exe(&l, "claude-code-alab", &cc_store_exe).as_deref(),
            Some("0.8.0+g987654321")
        );
        assert_eq!(
            store_build_version_of_exe(&l, "claude-code-alab", &cc_wrong_exe),
            None,
            "claude-code-alab exposes claude-alab, not claude-code-alab"
        );
        std::fs::write(
            cc_store.with_extension("exposes"),
            b"exposes v1\nclaude-code-alab\n",
        )
        .unwrap();
        assert_eq!(
            store_build_version_of_exe(&l, "claude-code-alab", &cc_wrong_exe),
            None,
            "a forged exposes sidecar cannot admit claude-code-alab as the exposed tool"
        );
        std::fs::write(
            cc_store.with_extension("exposes"),
            b"exposes v1\nclaude-alab\nother\n",
        )
        .unwrap();
        assert_eq!(
            store_build_version_of_exe(&l, "claude-code-alab", &cc_store_exe),
            None,
            "a multi-entry exposes sidecar is refused for a single-tool harness build"
        );
        std::fs::write(
            cc_store.with_extension("exposes"),
            b"exposes v1\nclaude-alab\n",
        )
        .unwrap();
        assert_eq!(
            store_build_version_of_exe(&l, "claude-code-alab", &cc_store_exe).as_deref(),
            Some("0.8.0+g987654321")
        );
        std::fs::write(cc_checkout.join("bin/claude-code-alab"), b"#!/bin/sh\n").unwrap();
        std::fs::write(
            cc_checkout.join(LOCAL_BUILD_JSON),
            r#"{"schema":2,"origin":"local-build","program":"claude-code-alab","command":"claude-code-alab","version":"0.8.0+g123456789"}"#,
        )
        .unwrap();
        assert_eq!(
            local_build_version(&cc_checkout, "claude-code-alab"),
            None,
            "claude-code-alab local receipt naming command claude-code-alab is refused"
        );
    }
}
