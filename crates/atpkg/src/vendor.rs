// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Vendor-direct HTTPS row admission ([`check_row`](crate::vendor::check_row),
//! [`crate::dispatch::ApplyStrategy::Shim`], [`crate::dispatch::ApplyStrategy::VendorApp`]).

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::path::{Component, Path, PathBuf};

use crate::flow::FlowError;
use crate::manifest::{Artifact, Program};
use crate::store::ToolName;

/// The hosts a signed `https` row may download from.
pub const VENDOR_HOSTS: &[&str] = &[
    "downloads.claude.ai",
    "github.com",
    "objects.githubusercontent.com",
    "release-assets.githubusercontent.com",
    "releases.openai.com",
];

/// Whether `url` may be fetched on the vendor-direct lane for `program`.
#[must_use]
pub fn vendor_direct_url_allowed(program: &str, url: &str) -> bool {
    crate::vendor_direct::spec(program).is_some_and(|spec| pinned_by(spec, url))
}

fn pinned_by(spec: &crate::vendor_direct::VendorSpec, url: &str) -> bool {
    https_host(url).is_some()
        && spec.url_prefixes.iter().any(|prefix| {
            url.strip_prefix(prefix)
                .is_some_and(|rest| match rest.strip_prefix('?') {
                    Some(query) => query_is(spec.url_query, query),
                    None => !rest.is_empty() && rest.split('/').all(path_component),
                })
        })
}

fn query_is(keys: &[&str], query: &str) -> bool {
    let mut pairs = query.split('&');
    keys.iter().all(|key| {
        pairs
            .next()
            .and_then(|pair| pair.split_once('='))
            .is_some_and(|(k, v)| k == *key && plain_component(v))
    }) && pairs.next().is_none()
}

fn plain_component(s: &str) -> bool {
    !s.is_empty()
        && s != "."
        && s != ".."
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// A path segment: a [`plain_component`] that may also hold `@`, as a GitHub release tag
/// of an npm-scoped package spells it (`@moonshot-ai/kimi-code@2.3.1`). The host is never
/// reached: [`https_host`] ends it at the first `/` and refuses an `@` in it.
fn path_component(s: &str) -> bool {
    s != "."
        && s != ".."
        && !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'@'))
}

/// Whether `program`'s vendor-direct digest document requested at `requested` may be trusted.
#[must_use]
pub fn digest_effective_url_ok(program: &str, requested: &str, effective: &str) -> bool {
    vendor_direct_url_allowed(program, requested)
        && crate::vendor_direct::spec(program)
            .is_some_and(|spec| spec.digest_doc_admitted(requested, effective))
}

/// The staging lanes an `https` row may name in `payload`.
pub const PAYLOADS: &[&str] = &[
    "raw-binary",
    "tar-gz-binary",
    "tar-gz",
    "tar-zst",
    "zip",
    "dmg",
];

const BINARY_PAYLOADS: &[&str] = &["raw-binary", "tar-gz-binary"];

/// The vendor-direct lane's npm binary package (grok's `@xai-official/grok-<plat>`): a gzip'd
/// tar whose one `package/bin/<exe>.br` is the binary, Brotli-packed
/// ([`crate::extract::stage_npm_brotli_binary`]). Only a row the lane builds from npm's signed
/// documents names it; [`PAYLOADS`] leaves it out, so no index row can.
pub(crate) const NPM_BROTLI_BINARY: &str = "npm-brotli-binary";

/// Whether `payload` stages exactly one binary into `bin/`: [`BINARY_PAYLOADS`], and the
/// vendor-direct lane's [`NPM_BROTLI_BINARY`].
#[must_use]
pub(crate) fn stages_one_binary(payload: &str) -> bool {
    is_binary_payload(payload) || payload == NPM_BROTLI_BINARY
}

const ARCHIVE_PAYLOADS: &[&str] = &["tar-gz", "tar-zst", "zip"];

/// Whether `payload` names a single-binary lane (see [`BINARY_PAYLOADS`]).
#[must_use]
pub fn is_binary_payload(payload: &str) -> bool {
    BINARY_PAYLOADS.contains(&payload)
}

/// Whether `payload` names an archive lane (see [`ARCHIVE_PAYLOADS`]).
#[must_use]
pub fn is_archive_payload(payload: &str) -> bool {
    ARCHIVE_PAYLOADS.contains(&payload)
}

/// The host of an `https://` URL, or `None` when the URL is not admissible at all.
#[must_use]
pub fn https_host(url: &str) -> Option<&str> {
    if url.bytes().any(|b| b <= b' ' || b == 0x7f || b == b'\\') {
        return None;
    }
    let rest = url.strip_prefix("https://")?;
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let host = &rest[..end];
    if host.is_empty()
        || host.contains('@')
        || host.contains(':')
        || host.contains('[')
        || host.contains(']')
    {
        return None;
    }
    Some(host)
}

/// Whether `s` is one bare file-name component.
pub(crate) fn bare_file_name(s: &str) -> bool {
    !s.is_empty()
        && s != "."
        && s != ".."
        && !s.contains('/')
        && !s.contains('\\')
        && !s.bytes().any(|b| b < b' ' || b == 0x7f)
}

fn link_target_ok(target: &str) -> bool {
    if target.is_empty()
        || target.starts_with('/')
        || target.contains('\0')
        || target.contains('\\')
        || target.ends_with('/')
    {
        return false;
    }
    target
        .split('/')
        .all(|seg| !seg.is_empty() && seg != "." && seg != "..")
        && Path::new(target)
            .components()
            .all(|c| matches!(c, Component::Normal(_)))
}

fn refuse(msg: &str) -> FlowError {
    FlowError::VendorRefused(msg.to_string())
}

fn refuse2(head: &str, detail: &str) -> FlowError {
    FlowError::VendorRefused(format!("{head}{detail}"))
}

/// Admit an artifact row before any byte moves, by its `protocol`.
/// # Errors
/// The first field that fails admission.
pub fn check_row(artifact: &Artifact, exposes: &[String]) -> Result<(), FlowError> {
    match artifact.protocol.as_str() {
        "github-release" => check_release_row(artifact),
        "https" => check_https_row(artifact, exposes),
        other => Err(refuse2(
            "protocol must be github-release | https, got ",
            other,
        )),
    }
}

fn check_release_row(artifact: &Artifact) -> Result<(), FlowError> {
    if artifact.kind == "elf-binary"
        && !matches!(
            artifact.target.as_str(),
            "x86_64-unknown-linux-gnu" | "aarch64-unknown-linux-gnu"
        )
    {
        return Err(refuse("elf-binary requires a supported GNU Linux target"));
    }
    if !artifact.asset.is_empty() && !bare_file_name(&artifact.asset) {
        return Err(refuse2(
            "asset must be a bare local file name (no separators, not `.`/`..`): ",
            &artifact.asset,
        ));
    }
    if artifact.payload == "dmg" {
        return Err(refuse(DMG_REFUSED));
    }
    if artifact.payload.is_empty() && artifact.tree_root.is_empty() {
        return Err(refuse(
            "tree_root is required for a release bundle (payload = \"\")",
        ));
    }
    Ok(())
}

const DMG_REFUSED: &str = "payload = dmg is refused until a disk image can be read without \
     attaching it (attaching one raises macOS's removable-volume dialog, which waits for an \
     answer)";

fn check_https_row(artifact: &Artifact, exposes: &[String]) -> Result<(), FlowError> {
    match artifact.kind.as_str() {
        "binary" | "app-bundle" => {}
        other => {
            return Err(refuse2(
                "an https row's kind must be binary | app-bundle, got ",
                other,
            ));
        }
    }
    let Some(host) = https_host(&artifact.url) else {
        return Err(refuse2(
            "url must be https:// on a bare vendor host (no port, no userinfo): ",
            &artifact.url,
        ));
    };
    if !VENDOR_HOSTS.contains(&host) {
        return Err(refuse2(
            "url host is not an allow-listed vendor host: ",
            host,
        ));
    }
    if !PAYLOADS.contains(&artifact.payload.as_str()) {
        return Err(FlowError::VendorRefused(format!(
            "payload must be {}, got {}",
            PAYLOADS.join(" | "),
            artifact.payload
        )));
    }
    if artifact.kind == "app-bundle" && artifact.payload != "dmg" {
        return Err(refuse2(
            "kind = app-bundle over https needs payload = dmg, got ",
            &artifact.payload,
        ));
    }
    if artifact.kind == "binary" && artifact.payload == "dmg" {
        return Err(refuse(
            "payload = dmg is the app-bundle lane; a binary row cannot name it",
        ));
    }
    if !bare_file_name(&artifact.asset) {
        return Err(refuse2(
            "asset must be a bare local file name (no separators, not `.`/`..`): ",
            &artifact.asset,
        ));
    }
    if is_binary_payload(&artifact.payload) {
        if artifact.entry.is_empty() {
            return Err(refuse2(
                "a non-empty entry is required by payload ",
                &artifact.payload,
            ));
        }
        if ToolName::new(&artifact.entry).is_none() {
            return Err(refuse2(
                "entry is not an admissible tool name: ",
                &artifact.entry,
            ));
        }
        if !exposes.iter().any(|e| e == &artifact.entry) {
            return Err(refuse2("entry is not in exposes: ", &artifact.entry));
        }
    } else if !artifact.entry.is_empty() {
        return Err(refuse2(
            "entry is only meaningful for raw-binary | tar-gz-binary, but payload is ",
            &artifact.payload,
        ));
    }
    if artifact.strip_components != 0 && !is_archive_payload(&artifact.payload) {
        return Err(refuse2(
            "strip_components applies to archive payloads only, not ",
            &artifact.payload,
        ));
    }
    if artifact.payload == "tar-gz-binary" && !artifact.links.is_empty() {
        return Err(refuse("links do not apply to payload tar-gz-binary"));
    }
    check_links(&artifact.links, exposes)?;
    if artifact.size == 0 {
        return Err(refuse("size must be > 0 (it is the exact download cap)"));
    }
    if !aterm_codec::hex::is_hex(&artifact.sha256, 64) {
        return Err(refuse2(
            "sha256 must be 64 hex characters, got ",
            &artifact.sha256,
        ));
    }
    if artifact.tree_root.is_empty() {
        return Err(refuse("tree_root is required for an https row"));
    }
    if artifact.payload == "dmg" {
        return Err(refuse(DMG_REFUSED));
    }
    Ok(())
}

fn check_links(links: &BTreeMap<String, String>, exposes: &[String]) -> Result<(), FlowError> {
    for (name, target) in links {
        if ToolName::new(name).is_none() {
            return Err(refuse2("links key is not an admissible tool name: ", name));
        }
        if !exposes.iter().any(|e| e == name) {
            return Err(refuse2("links key is not in exposes: ", name));
        }
        if !link_target_ok(target) {
            return Err(refuse2(
                "links target must be a relative, `..`-free path inside the staged tree: ",
                target,
            ));
        }
    }
    Ok(())
}

fn under_prefix(prefix: &Path, prefix_real: &Path, path: &Path) -> bool {
    path.starts_with(prefix) || path.starts_with(prefix_real)
}

fn is_executable_file(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// What a `PATH` walk should do at a directory the managed prefix owns.
#[derive(Clone, Copy, PartialEq, Eq)]
enum AtManaged {
    /// Skip it and keep walking.
    Skip,
    /// Stop: everything after the managed `bin/` loses to the managed copy.
    Stop,
}

/// The Windows `PATHEXT` a lookup falls back to when the variable is unset or empty.
#[cfg(any(windows, test))]
pub const DEFAULT_PATHEXT: &str = ".COM;.EXE;.BAT;.CMD";

/// The file names a Windows `PATH` lookup of `name` tries in one directory, in order.
#[must_use]
#[cfg(any(windows, test))]
pub fn windows_lookup_names(name: &str, pathext: &str) -> Vec<String> {
    let source = if pathext.trim().is_empty() {
        DEFAULT_PATHEXT
    } else {
        pathext
    };
    let exts: Vec<&str> = source
        .split(';')
        .map(str::trim)
        .filter(|e| e.len() > 1 && e.starts_with('.'))
        .collect();
    if let Some((_, ext)) = name.rsplit_once('.')
        && !ext.is_empty()
        && exts.iter().any(|e| e[1..].eq_ignore_ascii_case(ext))
    {
        return vec![name.to_string()];
    }
    exts.iter()
        .map(|e| {
            let mut s = String::from(name);
            s.push_str(e);
            s
        })
        .collect()
}

fn lookup_candidates(dir: &Path, bin: &str) -> Vec<PathBuf> {
    #[cfg(windows)]
    {
        let pathext = std::env::var("PATHEXT").unwrap_or_default();
        windows_lookup_names(bin, &pathext)
            .into_iter()
            .map(|n| dir.join(n))
            .collect()
    }
    #[cfg(not(windows))]
    {
        vec![dir.join(bin)]
    }
}

fn first_foreign_on_path(
    prefix: &Path,
    bin: &str,
    path_var: Option<&OsStr>,
    at_managed: AtManaged,
) -> Option<PathBuf> {
    if !bare_file_name(bin) {
        return None;
    }
    let path_var = path_var?;
    let prefix_real = std::fs::canonicalize(prefix).unwrap_or_else(|_| prefix.to_path_buf());
    for dir in std::env::split_paths(path_var) {
        // Absolute directories only: a relative entry resolves against the current directory.
        if dir.as_os_str().is_empty() || !dir.is_absolute() {
            continue;
        }
        let managed_dir = under_prefix(prefix, &prefix_real, &dir)
            || std::fs::canonicalize(&dir)
                .is_ok_and(|real| under_prefix(prefix, &prefix_real, &real));
        if managed_dir {
            match at_managed {
                AtManaged::Skip => continue,
                AtManaged::Stop => {
                    if is_managed_bin_dir(prefix, &prefix_real, &dir)
                        || lookup_candidates(&dir, bin)
                            .iter()
                            .any(|c| is_executable_file(c))
                    {
                        return None;
                    }
                    continue;
                }
            }
        }
        for candidate in lookup_candidates(&dir, bin) {
            if !is_executable_file(&candidate) {
                continue;
            }
            if std::fs::canonicalize(&candidate)
                .is_ok_and(|real| under_prefix(prefix, &prefix_real, &real))
            {
                match at_managed {
                    AtManaged::Skip => continue,
                    AtManaged::Stop => return None,
                }
            }
            return Some(candidate);
        }
    }
    None
}

fn is_managed_bin_dir(prefix: &Path, prefix_real: &Path, dir: &Path) -> bool {
    let bin = prefix.join("bin");
    let bin_real = prefix_real.join("bin");
    dir == bin
        || dir == bin_real
        || std::fs::canonicalize(dir).is_ok_and(|real| real == bin || real == bin_real)
}

/// Every foreign executable named `bin` on `path_var`, in `PATH` order.
#[must_use]
pub fn foreign_copies_on_path(prefix: &Path, bin: &str, path_var: Option<&OsStr>) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    if ToolName::new(bin).is_none() || !bare_file_name(bin) {
        return out;
    }
    let Some(path_var) = path_var else {
        return out;
    };
    let prefix_real = std::fs::canonicalize(prefix).unwrap_or_else(|_| prefix.to_path_buf());
    let mut seen: Vec<PathBuf> = Vec::new();
    for dir in std::env::split_paths(path_var) {
        if dir.as_os_str().is_empty() || !dir.is_absolute() {
            continue;
        }
        if under_prefix(prefix, &prefix_real, &dir)
            || std::fs::canonicalize(&dir)
                .is_ok_and(|real| under_prefix(prefix, &prefix_real, &real))
        {
            continue;
        }
        for candidate in lookup_candidates(&dir, bin) {
            if !is_executable_file(&candidate) {
                continue;
            }
            let real = std::fs::canonicalize(&candidate).unwrap_or_else(|_| candidate.clone());
            if under_prefix(prefix, &prefix_real, &real) || seen.contains(&real) {
                continue;
            }
            seen.push(real);
            out.push(candidate);
        }
    }
    out
}

/// Whether the managed `agents/` directory ([`crate::store::Layout::agents_dir`]) is on `path_var`.
#[must_use]
pub fn agents_dir_on_path(prefix: &Path, path_var: Option<&OsStr>) -> bool {
    let Some(path_var) = path_var else {
        return false;
    };
    let agents = prefix.join("agents");
    let agents_real = std::fs::canonicalize(&agents).unwrap_or_else(|_| agents.clone());
    std::env::split_paths(path_var).any(|dir| {
        !dir.as_os_str().is_empty()
            && dir.is_absolute()
            && (dir == agents
                || dir == agents_real
                || std::fs::canonicalize(&dir)
                    .is_ok_and(|real| real == agents || real == agents_real))
    })
}

/// Probe `path_var` (a `PATH` value) for an executable named `bin`.
#[must_use]
pub fn system_binary_on_path(
    prefix: &Path,
    bin: &str,
    path_var: Option<&OsStr>,
) -> Option<PathBuf> {
    ToolName::new(bin)?;
    first_foreign_on_path(prefix, bin, path_var, AtManaged::Skip)
}

/// The binary that shadows a managed tool named `bin`.
#[must_use]
pub fn shadowing_binary_on_path(
    prefix: &Path,
    bin: &str,
    path_var: Option<&OsStr>,
) -> Option<PathBuf> {
    ToolName::new(bin)?;
    first_foreign_on_path(prefix, bin, path_var, AtManaged::Stop)
}

/// The copy a shell runs when `bin` is typed.
#[must_use]
pub fn first_on_path(bin: &str, path_var: Option<&OsStr>) -> Option<PathBuf> {
    if !bare_file_name(bin) {
        return None;
    }
    std::env::split_paths(path_var?)
        .filter(|dir| !dir.as_os_str().is_empty() && dir.is_absolute())
        .flat_map(|dir| lookup_candidates(&dir, bin))
        .find(|candidate| is_executable_file(candidate))
}

#[cfg(test)]
thread_local! {
    static PATH_VAR: std::cell::RefCell<Option<Option<std::ffi::OsString>>> =
        const { std::cell::RefCell::new(None) };
}

/// This process's `PATH`, as the system-satisfaction probes read it.
#[must_use]
pub fn path_var() -> Option<std::ffi::OsString> {
    #[cfg(test)]
    {
        if let Some(injected) = PATH_VAR.with(|p| p.borrow().clone()) {
            return injected;
        }
    }
    std::env::var_os("PATH")
}

/// Run `f` with `path` as what [`path_var`] answers on this thread for its duration.
#[cfg(all(test, unix))]
pub(crate) fn with_path_var<R>(path: Option<&OsStr>, f: impl FnOnce() -> R) -> R {
    let prior = PATH_VAR.with(|p| p.replace(Some(path.map(OsStr::to_os_string))));
    let out = f();
    PATH_VAR.with(|p| {
        *p.borrow_mut() = prior;
    });
    out
}

/// Whether `program` is satisfied by a system install.
#[must_use]
pub fn system_satisfied(prefix: &Path, program: &Program) -> Option<PathBuf> {
    let bin = program.system.as_deref()?;
    system_binary_on_path(prefix, bin, path_var().as_deref())
}

#[cfg(test)]
#[path = "vendor_testkit_tests.rs"]
pub(crate) mod testkit;

#[cfg(test)]
#[path = "vendor_tests.rs"]
mod tests;
