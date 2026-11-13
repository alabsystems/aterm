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
//! and only `[packages].exclude` or a person's uninstall keeps it out (docs/OWNER_RULINGS.md,
//! 2026-10-27; that page's §11 D45). The name still decides
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
/// installed, as `(program, tool)` (`docs/DESIGN-harness-repos-2026-10-24.md` §9 step 6). A
/// claiming harness also joins `crates/aterm-agent/src/harness/contract.rs`'s `ROSTER` in the
/// same change. kimi-alab claims `relaunch,upgrade`; its process handoff also needs the Kimi
/// program mapping in `aterm-agent-names`.
#[must_use]
pub const fn twin_harness(vendor: &str) -> Option<(&'static str, &'static str)> {
    match vendor.as_bytes() {
        b"grok" => Some(("grok-alab", "grok-alab")),
        b"kimi" => Some(("kimi-alab", "kimi-alab")),
        _ => None,
    }
}

/// The receipt a local harness artifact directory carries (`atpkg-publish build-local`).
pub const LOCAL_BUILD_JSON: &str = "local-build.json";

/// Bound on a [`LOCAL_BUILD_JSON`] receipt read.
const MAX_LOCAL_BUILD_JSON_BYTES: usize = 64 * 1024;

/// Whether `version` has a historical or calendar product core, optionally `+g<hex9..40>`
/// stamped (required when `require_stamp` is true), and matches `source` when provided.
fn harness_version_valid(version: &str, source: Option<&str>, require_stamp: bool) -> bool {
    if version.is_empty() || version.len() > 128 || !version.bytes().all(|b| b.is_ascii_graphic()) {
        return false;
    }
    let (base, stamp) = match version.split_once("+g") {
        Some((base, stamp)) => (base, Some(stamp)),
        None if require_stamp || source.is_some() => return false,
        None => (version, None),
    };
    if aterm_types::release_version::ReleaseVersion::parse(base).is_none() {
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
    let tool = crate::store::ToolName::new(command)?;
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
    // The program file is the store's own spelling of the tool's executable (`<command>.exe` on
    // Windows, where only that runs; the command itself elsewhere).
    let entry = std::path::Path::new("bin").join(tool.exe_file());
    let exe_path = root.join(&entry);
    let (exe_file, meta) = crate::metadata_io::open_regular_with_meta(&exe_path).ok()?;
    if meta.len() == 0 {
        return None;
    }
    if let Some(executables) = v.get("executables") {
        let [exe] = executables.as_array()?.as_slice() else {
            return None;
        };
        let expected_path = format!("bin/{}", tool.exe_file());
        let size = exe.get("size").and_then(aterm_json::Value::as_u64)?;
        let sha256 = exe.get("sha256").and_then(aterm_json::Value::as_str)?;
        if exe.get("path").and_then(aterm_json::Value::as_str) != Some(expected_path.as_str())
            || size != meta.len()
            || !hex_len(sha256, 64)
            || crate::tree::sha256_opened(exe_file).ok().as_deref() != Some(sha256)
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
    // The manifest's command is this tool (`local_build_manifest`); its shim is the tool's own.
    let exe = crate::ops::which(layout, default_harness_tool(program))?
        .canonicalize()
        .ok()?;
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
#[path = "harness_tests.rs"]
mod tests;
