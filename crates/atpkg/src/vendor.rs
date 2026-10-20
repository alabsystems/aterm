// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Per-protocol row admission, the system-satisfaction probe, and the PATH-shadow probe.
//!
//! A `protocol = "https"` row is a roster-signed manifest row pinning a vendor's own download
//! (`url`) by `sha256`, `size` and `tree_root`: the client downloads straight from the
//! vendor, verifies against the signed digests, and shims the result
//! ([`crate::dispatch::ApplyStrategy::Shim`]) or lays its `.app`
//! ([`crate::dispatch::ApplyStrategy::VendorApp`]). Nothing third-party is mirrored onto the
//! index owner's hosting (Claude Code's license forbids redistribution). The vendor host is
//! only a transport; [`check_row`] narrows where a signed row may point and refuses shapes
//! the stage lanes could mis-stage, before any byte moves.
//!
//! [`system_satisfied`] holds a default-set member declaring `system = "<bin>"` satisfied by
//! a binary of that name already on `PATH` outside the managed `bin/` and the store.
//! [`shadowing_binary_on_path`] is its mirror for managed members: a binary of an exposed
//! name ahead of the managed `bin/` is what runs, and the state says so
//! (`managed <build> — SHADOWED by <path>`: a warning, never a fault; the user owns `PATH`).
//!
//! The walk is cross-platform: `PATH` splits on the platform separator, only absolute
//! entries count, and on Windows a bare name resolves through `PATHEXT` as `cmd.exe` does
//! ([`windows_lookup_names`]).

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::path::{Component, Path, PathBuf};

use crate::flow::FlowError;
use crate::manifest::{Artifact, Program};
use crate::store::ToolName;

/// The hosts a signed `https` row may download from: exact host match over an `https://`
/// URL, no ports, no userinfo, no subdomain wildcards.
///
/// Defense in depth, not the trust root: the signed row is the authority; this list only
/// narrows where a signed row may point, so a publisher-key compromise cannot redirect
/// clients to an arbitrary host. Bump it in a client release when a vendor moves.
pub const VENDOR_HOSTS: &[&str] = &[
    "downloads.claude.ai",
    "github.com",
    "objects.githubusercontent.com",
    "release-assets.githubusercontent.com",
    "releases.openai.com",
];

/// Whether `url` may be fetched on the vendor-direct lane for `program`: an admissible
/// `https` URL ([`https_host`]) that starts with one of that program's compiled prefixes
/// ([`crate::vendor_direct::VendorSpec::url_prefixes`]) and goes on with either `/`-separated
/// segments or, for a program that declares a query
/// ([`crate::vendor_direct::VendorSpec::url_query`]), a `?` and exactly those keys, in order,
/// each `=` a value. Every segment and value is non-empty `[A-Za-z0-9._-]` and never
/// `.`/`..`. No other query, fragment, percent-escape (curl collapses an encoded dot
/// segment), backslash or empty segment: none of the pinned URLs needs one, and each is a way
/// to re-point the request.
#[must_use]
pub fn vendor_direct_url_allowed(program: &str, url: &str) -> bool {
    crate::vendor_direct::spec(program).is_some_and(|spec| pinned_by(spec, url))
}

/// [`vendor_direct_url_allowed`] under one table row.
fn pinned_by(spec: &crate::vendor_direct::VendorSpec, url: &str) -> bool {
    https_host(url).is_some()
        && spec.url_prefixes.iter().any(|prefix| {
            url.strip_prefix(prefix)
                .is_some_and(|rest| match rest.strip_prefix('?') {
                    Some(query) => query_is(spec.url_query, query),
                    None => !rest.is_empty() && rest.split('/').all(plain_component),
                })
        })
}

/// Whether `query` is exactly `keys`, in order, each `<key>=<plain value>` — so an empty
/// `keys` admits no query at all.
fn query_is(keys: &[&str], query: &str) -> bool {
    let mut pairs = query.split('&');
    keys.iter().all(|key| {
        pairs
            .next()
            .and_then(|pair| pair.split_once('='))
            .is_some_and(|(k, v)| k == *key && plain_component(v))
    }) && pairs.next().is_none()
}

/// One path segment or query value: non-empty `[A-Za-z0-9._-]`, never `.` or `..`.
fn plain_component(s: &str) -> bool {
    !s.is_empty()
        && s != "."
        && s != ".."
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// Whether `program`'s vendor-direct digest document requested at `requested` may be
/// trusted having ended at `effective` (curl's effective URL): `requested` passes
/// [`vendor_direct_url_allowed`] for `program`, and the compiled table admits the landing
/// ([`crate::vendor_direct::VendorSpec::digest_doc_admitted`]).
#[must_use]
pub fn digest_effective_url_ok(program: &str, requested: &str, effective: &str) -> bool {
    vendor_direct_url_allowed(program, requested)
        && crate::vendor_direct::spec(program)
            .is_some_and(|spec| spec.digest_doc_admitted(requested, effective))
}

/// The staging lanes an `https` row may name in `payload`. The stage side
/// (`install::verify_and_stage`) implements each; a row naming anything else is refused
/// here, before download. `dmg` is the `kind = "app-bundle"` lane; the others are
/// `kind = "binary"`.
pub const PAYLOADS: &[&str] = &[
    "raw-binary",
    "tar-gz-binary",
    "tar-gz",
    "tar-zst",
    "zip",
    "dmg",
];

/// The single-binary payloads, the only ones an `entry` applies to: the download is the
/// executable (`raw-binary`), or a gzip tar of exactly one regular file at its root
/// (`tar-gz-binary`); either way it is laid at `bin/<entry>`, mode `0755`.
const BINARY_PAYLOADS: &[&str] = &["raw-binary", "tar-gz-binary"];

/// The archive payloads — the only ones `strip_components` applies to. A `tar-gz-binary`
/// is a gzip tar too, but of one file the stage names itself: there is no tree to strip.
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

/// The host of an `https://` URL, or `None` when the URL is not admissible at all: a
/// non-`https` scheme, an empty host, a port, userinfo (`user@host`), or any whitespace /
/// control byte anywhere in the URL (curl gets the URL after a literal `--`, but a row
/// that needs quoting has no business being signed).
#[must_use]
pub fn https_host(url: &str) -> Option<&str> {
    if url.bytes().any(|b| b <= b' ' || b == 0x7f) {
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

/// Whether `s` is one bare file-name component: non-empty, not `.`/`..`, no `/`, no `\`, no
/// control byte. The row's `asset` is joined onto the program's staging directory as the
/// download's local name, so it must never be a path or carry a byte a later consumer could
/// misread. Also the name a `tar-gz-binary` archive's one member must have
/// (`extract::stage_tar_gz_member`).
pub(crate) fn bare_file_name(s: &str) -> bool {
    !s.is_empty()
        && s != "."
        && s != ".."
        && !s.contains('/')
        && !s.contains('\\')
        && !s.bytes().any(|b| b < b' ' || b == 0x7f)
}

/// Whether a `links` target is admissible: a relative path inside the staged tree (non-empty,
/// no leading `/`, no `..`, no `.`/empty components, no NUL, no `\`, which would let `..\`
/// dodge the component check).
fn link_target_ok(target: &str) -> bool {
    if target.is_empty()
        || target.starts_with('/')
        || target.contains('\0')
        || target.contains('\\')
        || target.ends_with('/')
    {
        return false;
    }
    // Segment by segment on the raw string, not `Path::components()`: that iterator
    // normalizes an interior `.` away (`a/./b` reads as `a/b`), and a target the client
    // creates verbatim must be judged verbatim.
    target
        .split('/')
        .all(|seg| !seg.is_empty() && seg != "." && seg != "..")
        && Path::new(target)
            .components()
            .all(|c| matches!(c, Component::Normal(_)))
}

/// Refuse `msg` as a [`FlowError::VendorRefused`].
fn refuse(msg: &str) -> FlowError {
    FlowError::VendorRefused(msg.to_string())
}

/// Refuse with a `<head><detail>` message.
fn refuse2(head: &str, detail: &str) -> FlowError {
    FlowError::VendorRefused(format!("{head}{detail}"))
}

/// Admit an artifact row before any byte moves, by its `protocol`:
///
/// * `github-release` ([`check_release_row`]): only a bare `asset` name and no `dmg` payload
///   (below); the release lane's other gates are the signed `sha256`/`tree_root` at stage
///   time;
/// * `https`: an allow-listed bare host, a known `payload` matching the `kind`
///   (`dmg` ⇔ `app-bundle`), a shimmable exposed `entry` for the single-binary lanes,
///   `strip_components` on archives only, `links` keys exposed and targets in-tree (none on
///   `tar-gz-binary`), a bare `asset` name, `size > 0`, a 64-hex `sha256`, a non-empty
///   `tree_root`; and then no `dmg` (`crate::install`'s `stage_dmg`: attaching an image
///   raises macOS's removable-volume dialog);
/// * anything else (the removed OS-installer protocols included) is refused by name.
///
/// `exposes` is the manifest's signed `exposes` list: the single-binary `entry` and every
/// `links` key must be in it, as those are the names the stage puts under `bin/`.
///
/// Every refusal is a [`FlowError::VendorRefused`] naming the field, so a mis-authored row
/// fails on the authoring machine's own `atpkg install`, never on a user's pass.
///
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

/// The `github-release` half of [`check_row`]: the `asset` is joined onto the program's
/// staging directory as the download's local name, so it must be one bare file name, or an
/// absolute or `..` asset would name a file outside staging that install unlinks (sweeping
/// that directory's `*.part` files) before any download. The flow refuses an escaping staged
/// path a second time.
fn check_release_row(artifact: &Artifact) -> Result<(), FlowError> {
    if !artifact.asset.is_empty() && !bare_file_name(&artifact.asset) {
        return Err(refuse2(
            "asset must be a bare local file name (no separators, not `.`/`..`): ",
            &artifact.asset,
        ));
    }
    // The stage picks its lane by `payload` whatever the protocol, so a release row naming
    // the dmg lane would attach an image as an https one would: refused the same way.
    if artifact.payload == "dmg" {
        return Err(refuse(DMG_REFUSED));
    }
    Ok(())
}

/// Why a row naming the `dmg` lane is refused, whatever its protocol: until a disk image can
/// be read without attaching it, the attach and the read inside it draw macOS's
/// removable-volume dialog where aterm lacks Full Disk Access, and a TCC dialog has no
/// timeout (`install::stage_dmg`).
const DMG_REFUSED: &str = "payload = dmg is refused until a disk image can be read without \
     attaching it (attaching one raises macOS's removable-volume dialog, which waits for an \
     answer)";

/// The `https` half of [`check_row`].
fn check_https_row(artifact: &Artifact, exposes: &[String]) -> Result<(), FlowError> {
    // 0. The kind this protocol can carry: a binary (five payload lanes) or a vendor
    //    `.app` (the dmg lane). Anything else is a shape the stage cannot lay down.
    match artifact.kind.as_str() {
        "binary" | "app-bundle" => {}
        other => {
            return Err(refuse2(
                "an https row's kind must be binary | app-bundle, got ",
                other,
            ));
        }
    }
    // 1. URL: https, exact allow-listed host, no port/userinfo/control bytes.
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
    // 2. Payload lane, and it must agree with the kind: `dmg` is how an app-bundle
    //    arrives and the only way one may; a binary never arrives as a disk image.
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
    // 2b. `asset` is the local staging file name the download lands under
    //     (`staging_dir(program).join(asset)`), never a release asset and never a path.
    if !bare_file_name(&artifact.asset) {
        return Err(refuse2(
            "asset must be a bare local file name (no separators, not `.`/`..`): ",
            &artifact.asset,
        ));
    }
    // 3. A single-binary lane needs an entry that is a shimmable name and an exposed one;
    //    the other lanes must not carry one (a stray entry would be a silent authoring slip).
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
    // 4. strip_components only for archives.
    if artifact.strip_components != 0 && !is_archive_payload(&artifact.payload) {
        return Err(refuse2(
            "strip_components applies to archive payloads only, not ",
            &artifact.payload,
        ));
    }
    // 5. links: every key exposed + shimmable, every target relative and `..`-free; none
    //    on a `tar-gz-binary`, whose stage lays its one file and nothing else.
    if artifact.payload == "tar-gz-binary" && !artifact.links.is_empty() {
        return Err(refuse("links do not apply to payload tar-gz-binary"));
    }
    check_links(&artifact.links, exposes)?;
    // 6. The signed digests must be present and well-formed: the extracted tree is the
    //    only thing the client can re-verify at apply time, so an empty root is a refusal
    //    for this protocol even though a loose release manifest tolerates one.
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
    // 7. The dmg lane itself, last, so every other field of a dmg row is still judged
    //    ([`DMG_REFUSED`]).
    if artifact.payload == "dmg" {
        return Err(refuse(DMG_REFUSED));
    }
    Ok(())
}

/// The `links` half of the https check, split so the map rules read on their own.
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

/// Whether `path` lies under the managed `prefix`, spelled as given or as the filesystem
/// resolves it (`prefix_real`: on macOS a temp prefix under `/var/folders` resolves to
/// `/private/var/folders`, and a symlinked prefix elsewhere entirely). Anything under the
/// prefix is atpkg's own, never a system install.
fn under_prefix(prefix: &Path, prefix_real: &Path, path: &Path) -> bool {
    path.starts_with(prefix) || path.starts_with(prefix_real)
}

/// Whether `path` is a runnable regular file (following symlinks — a Homebrew `gh` is a
/// symlink into the Cellar). On Unix at least one execute bit must be set.
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
    /// Skip it and keep walking — the system probe: a managed copy never satisfies.
    Skip,
    /// Stop: everything after the managed `bin/` loses to the managed copy — the shadow
    /// probe.
    Stop,
}

/// The Windows `PATHEXT` a lookup falls back to when the variable is unset or empty —
/// `cmd.exe`'s own default.
#[cfg(any(windows, test))]
pub const DEFAULT_PATHEXT: &str = ".COM;.EXE;.BAT;.CMD";

/// The file names a Windows `PATH` lookup of `name` tries in one directory, in order
/// (`cmd.exe`'s rule): a name already ending in an extension `pathext` lists (`gh.exe`,
/// `scoop.cmd`, case-insensitively) is tried as spelled and nothing else; a bare name (`gh`)
/// tries `name` + each `pathext` entry in order (`gh.COM`, `gh.EXE`, …). An empty `pathext`
/// reads as [`DEFAULT_PATHEXT`]; entries not starting with `.` are ignored. A file with no
/// extension never matches on Windows. Pure, so the rule is tested on every platform.
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

/// The paths a lookup of `bin` tries under `dir`: on Windows the `PATHEXT` spellings
/// ([`windows_lookup_names`] over the process's `PATHEXT`), elsewhere `dir/bin` alone.
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

/// The shared `PATH` walk behind [`system_binary_on_path`] and [`shadowing_binary_on_path`]:
/// the first executable named `bin` in an absolute `PATH` entry outside the managed `prefix`,
/// whose resolved path is also outside it. `at_managed` decides what a prefix-owned entry
/// does to the walk. `bin` must be one bare file name: it is joined onto every entry.
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
    // `split_paths` is the platform's own rule: `:` on Unix, `;` on Windows (where a
    // quoted entry is unquoted too).
    for dir in std::env::split_paths(path_var) {
        // A relative entry (`.`, `bin`, the empty string) resolves against the current
        // directory: a `gh` dropped into a project checkout must never read as a system
        // install, nor retire the managed copy. Absolute directories only (on Windows,
        // drive-rooted).
        if dir.as_os_str().is_empty() || !dir.is_absolute() {
            continue;
        }
        // The entry as spelled or as it resolves (`~/bin → <prefix>/bin` is the managed
        // bin/ under another name): atpkg's own directory either way.
        let managed_dir = under_prefix(prefix, &prefix_real, &dir)
            || std::fs::canonicalize(&dir)
                .is_ok_and(|real| under_prefix(prefix, &prefix_real, &real));
        if managed_dir {
            match at_managed {
                AtManaged::Skip => continue,
                // Only the managed `bin/` ends the shadow walk unconditionally: the managed
                // copy (or its stub) answers from there and everything after loses. Every
                // other prefix-owned entry (`reroute/`, which an aterm session puts first;
                // `agents/`, first too; a store dir on PATH) is transparent unless it holds
                // the name: then the managed copy runs (Stop), else the walk goes on. A
                // `reroute/` stub may pass through at exec time, so the surfaces that name
                // what runs ask `cli::shadow_in_shell`, which walks the stub's own PATH.
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
            // Follow the hit to where it really lives: a user's own symlink to a store copy
            // is still atpkg's copy; it never satisfies (Skip), and for the shadow probe it
            // is the managed copy running, so nothing after it shadows (Stop).
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

/// Whether `dir` (as spelled, or as it resolves) is the managed `<prefix>/bin`: the one
/// prefix-owned `PATH` entry that ends a shadow walk unconditionally.
fn is_managed_bin_dir(prefix: &Path, prefix_real: &Path, dir: &Path) -> bool {
    let bin = prefix.join("bin");
    let bin_real = prefix_real.join("bin");
    dir == bin
        || dir == bin_real
        || std::fs::canonicalize(dir).is_ok_and(|real| real == bin || real == bin_real)
}

/// Every foreign executable named `bin` on `path_var`, in `PATH` order: the walk of
/// [`system_binary_on_path`] continued past the first hit, de-duplicated by where each hit
/// really lives (a `~/.local/bin/claude` symlinked to `/opt/homebrew/bin/claude` is one
/// copy). `which` prints it after an agent program's managed line as `foreign copies
/// out-ranked: …`. Empty for a name the [`ToolName`] gate refuses, or with no `PATH`.
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

/// Whether the managed `agents/` directory ([`crate::store::Layout::agents_dir`]) is on
/// `path_var` at all, as spelled or as an entry resolves (absolute entries only). The agent
/// programs' shadow wording uses it to tell a `PATH` with no `agents/` (a shell from before
/// the install) from one that has it behind the foreign copy.
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

/// Probe `path_var` (a `PATH` value) for an executable named `bin`, skipping relative entries
/// (they name the current directory, not a system), every directory the managed `prefix`
/// owns, and any hit that resolves into the prefix (a user's symlink to a store copy is still
/// atpkg's). The first remaining hit wins, in `PATH` order.
///
/// The name must be one admissible component ([`ToolName`]): a `system` value with a
/// separator is refused, and a name the shim deny-list refuses (`git`, `cargo`, …) never
/// satisfies a member.
#[must_use]
pub fn system_binary_on_path(
    prefix: &Path,
    bin: &str,
    path_var: Option<&OsStr>,
) -> Option<PathBuf> {
    ToolName::new(bin)?;
    first_foreign_on_path(prefix, bin, path_var, AtManaged::Skip)
}

/// The binary that shadows a managed tool named `bin`: the first executable of that name in
/// an absolute `PATH` entry before the managed `bin/` (or anywhere, when the managed `bin/` is
/// not on `PATH`; an aterm session appends it last), resolving outside the prefix. `None`
/// when the managed copy is the one that runs. Same relative-entry and resolve-into-store
/// rules as [`system_binary_on_path`].
#[must_use]
pub fn shadowing_binary_on_path(
    prefix: &Path,
    bin: &str,
    path_var: Option<&OsStr>,
) -> Option<PathBuf> {
    ToolName::new(bin)?;
    first_foreign_on_path(prefix, bin, path_var, AtManaged::Stop)
}

/// The copy a shell runs when `bin` is typed: the first executable of that name in an
/// absolute `PATH` entry, wherever it lives, the managed prefix included. The
/// relative-entry and `PATHEXT` rules of [`system_binary_on_path`], none of its prefix
/// rules: a program built into aterm ([`crate::builtin`]) has no managed copy for a walk
/// to stop at or skip.
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

/// This process's `PATH`, as the system-satisfaction probes read it — or, inside a test,
/// the value [`with_path_var`] injected for its scope, so a flow test that walks `PATH`
/// never meets the developer's own tools.
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

/// Run `f` with `path` as what [`path_var`] answers on this thread for its duration —
/// `None` is an unset `PATH`. Nested scopes restore the outer one. Test-only.
#[cfg(all(test, unix))]
pub(crate) fn with_path_var<R>(path: Option<&OsStr>, f: impl FnOnce() -> R) -> R {
    let prior = PATH_VAR.with(|p| p.replace(Some(path.map(OsStr::to_os_string))));
    let out = f();
    PATH_VAR.with(|p| {
        *p.borrow_mut() = prior;
    });
    out
}

/// Whether `program` is satisfied by a system install: it declares `system = "<bin>"` and
/// that binary is on this process's `PATH` ([`path_var`]) outside the managed `prefix`.
/// `Some(path)` names the install; `None` means the program is managed here.
#[must_use]
pub fn system_satisfied(prefix: &Path, program: &Program) -> Option<PathBuf> {
    let bin = program.system.as_deref()?;
    system_binary_on_path(prefix, bin, path_var().as_deref())
}

#[cfg(test)]
pub(crate) mod testkit {
    //! The reference `https` row — shared with the lanes' tests.

    use std::collections::BTreeMap;

    use crate::manifest::{Artifact, Cost};

    /// The reference `https` row: Claude Code's raw binary.
    pub fn row() -> Artifact {
        Artifact {
            target: "aarch64-apple-darwin".into(),
            kind: "binary".into(),
            protocol: "https".into(),
            asset: "claude-2.1.231-darwin-arm64".into(),
            sha256: "7b09f01c".repeat(8),
            sha512: String::new(),
            tree_root: "abc".into(),
            size: 230_824_016,
            reloc: "self-contained".into(),
            cost: Cost::default(),
            url: "https://downloads.claude.ai/claude-code-releases/2.1.231/darwin-arm64/claude"
                .into(),
            payload: "raw-binary".into(),
            entry: "claude".into(),
            strip_components: 0,
            links: BTreeMap::new(),
            vendor: "Anthropic PBC".into(),
            provenance: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testkit::row;
    use super::*;

    fn exposes() -> Vec<String> {
        vec!["claude".to_string()]
    }

    fn refused(a: &Artifact, exposes: &[String]) -> String {
        match check_row(a, exposes) {
            Err(FlowError::VendorRefused(m)) => m,
            other => panic!("expected VendorRefused, got {other:?}"),
        }
    }

    /// A `github-release` row with a bare `asset` is admitted whatever its other keys hold:
    /// the release lane has no vendor checks beyond the asset name.
    #[test]
    fn a_github_release_row_is_unchanged_by_admission() {
        let mut a = row();
        a.protocol = "github-release".into();
        a.url = String::new();
        a.payload = String::new();
        a.entry = String::new();
        a.tree_root = String::new();
        a.size = 0;
        a.sha256 = "d".into();
        check_row(&a, &[]).expect("the release lane has no vendor admission");
        // …even with the retired-shape keys hanging off it.
        a.url = "http://evil.example/x".into();
        check_row(&a, &[]).expect("release rows ignore url");
    }

    /// A `github-release` row's `asset` is the download's local name in the staging
    /// directory, so admission holds it to the `https` lane's bare-name rule.
    #[test]
    fn a_github_release_row_with_a_path_asset_is_refused() {
        let mut a = row();
        a.protocol = "github-release".into();
        a.sha256 = "d".into();
        for bad in [
            "/Users//victim/.ssh/authorized_keys",
            "../../../x",
            "a/b",
            "a\\b",
            "..",
            ".",
            "x\ny",
        ] {
            a.asset = bad.into();
            assert!(
                refused(&a, &[]).contains("asset must be a bare local file name"),
                "{bad:?}"
            );
        }
        a.asset = "ty-2973.tar.zst".into();
        check_row(&a, &[]).expect("a bare release asset name is admitted");
    }

    #[test]
    fn an_unknown_protocol_is_refused_by_name() {
        let mut a = row();
        a.protocol = "ftp".into();
        assert!(refused(&a, &exposes()).contains("protocol must be"));
        a.protocol = String::new();
        assert!(refused(&a, &exposes()).contains("protocol must be"));
        a.protocol = "HTTPS".into();
        assert!(refused(&a, &exposes()).contains("protocol must be"));
        // The removed OS-installer protocols are unknown here, refused by name.
        for deleted in ["pkg", "system-pm", "softwareupdate"] {
            a.protocol = deleted.into();
            let m = refused(&a, &exposes());
            assert!(m.contains("protocol must be github-release | https"), "{m}");
            assert!(m.ends_with(deleted), "{m}");
        }
    }

    #[test]
    fn a_well_formed_archive_row_with_links_is_admitted() {
        let mut a = row();
        a.url =
            "https://github.com/cli/cli/releases/download/v2.80.0/gh_2.80.0_macOS_arm64.zip".into();
        a.payload = "zip".into();
        a.entry = String::new();
        a.strip_components = 1;
        a.links.insert("gh".into(), "share/gh/bin/gh".into());
        check_row(&a, &["gh".to_string()]).expect("archive row admits");
    }

    /// A `dmg` row is refused before any download until an image can be read without
    /// attaching it (`install::stage_dmg`): one whose every other field is admitted is
    /// refused for its lane alone, with the reason.
    #[test]
    fn a_well_formed_dmg_row_is_refused_for_its_lane_alone() {
        let mut d = row();
        d.kind = "app-bundle".into();
        d.url = "https://github.com/example/app/releases/download/v1/App-1.0-universal.dmg".into();
        d.payload = "dmg".into();
        d.entry = String::new();
        d.links
            .insert("emacs".into(), "Emacs.app/Contents/MacOS/Emacs".into());
        d.links.insert(
            "emacsclient".into(),
            "Emacs.app/Contents/MacOS/bin/emacsclient".into(),
        );
        let m = refused(&d, &["emacs".to_string(), "emacsclient".to_string()]);
        assert!(m.starts_with("payload = dmg is refused"), "{m}");
        assert!(m.contains("removable-volume dialog"), "{m}");
        // The stage picks the lane by `payload` whatever the protocol: a release row naming
        // it is refused too, and the same row on another lane is admitted.
        let mut r = row();
        r.protocol = "github-release".into();
        r.sha256 = "d".into();
        r.payload = "dmg".into();
        let m = refused(&r, &[]);
        assert!(m.starts_with("payload = dmg is refused"), "{m}");
        r.payload = String::new();
        check_row(&r, &[]).expect("the same release row on the bundle lane");
    }

    /// The kind ⇔ payload pairing over https: an app-bundle arrives only as a dmg, a binary
    /// never does, and no other kind rides this protocol.
    #[test]
    fn https_kind_and_payload_must_agree() {
        let mut app_as_zip = row();
        app_as_zip.kind = "app-bundle".into();
        app_as_zip.payload = "zip".into();
        app_as_zip.entry = String::new();
        assert!(refused(&app_as_zip, &exposes()).contains("needs payload = dmg"));
        let mut bin_as_dmg = row();
        bin_as_dmg.payload = "dmg".into();
        bin_as_dmg.entry = String::new();
        assert!(refused(&bin_as_dmg, &exposes()).contains("app-bundle lane"));
        for other in [
            "sysroot-bundle",
            "cargo-src",
            "installer-pkg",
            "system-package",
            "",
            "vendor-fetch",
        ] {
            let mut a = row();
            a.kind = other.into();
            assert!(
                refused(&a, &exposes()).contains("kind must be binary | app-bundle"),
                "{other:?}"
            );
        }
        // `dmg-app` is no payload.
        let mut old = row();
        old.kind = "app-bundle".into();
        old.payload = "dmg-app".into();
        old.entry = String::new();
        assert!(refused(&old, &exposes()).contains("payload must be"));
    }

    #[test]
    fn refuses_non_https_and_hostile_urls() {
        for bad in [
            "http://downloads.claude.ai/x",
            "ftp://downloads.claude.ai/x",
            "file:///etc/passwd",
            "downloads.claude.ai/x",
            "",
            "https://",
            "https:///x",
            "https://downloads.claude.ai:443/x",
            "https://user@downloads.claude.ai/x",
            "https://user:pw@downloads.claude.ai/x",
            "https://[::1]/x",
            "https://downloads.claude.ai/x y",
            "https://downloads.claude.ai/x\n",
            "https://downloads.claude.ai/x\u{7f}",
        ] {
            let mut a = row();
            a.url = bad.into();
            let m = refused(&a, &exposes());
            assert!(m.contains("url must be https"), "{bad:?}: {m}");
        }
    }

    #[test]
    fn refuses_a_host_outside_the_allow_list_exactly() {
        for bad in [
            "https://evil.example/x",
            "https://downloads.claude.ai.evil.example/x",
            "https://evil.downloads.claude.ai/x",
            "https://DOWNLOADS.CLAUDE.AI/x", // exact match, no case folding
            "https://api.github.com/x",
            "https://raw.githubusercontent.com/x",
        ] {
            let mut a = row();
            a.url = bad.into();
            let m = refused(&a, &exposes());
            assert!(
                m.contains("not an allow-listed vendor host"),
                "{bad:?}: {m}"
            );
        }
        // Every allow-listed host admits, at the root path and a deep one.
        for host in VENDOR_HOSTS {
            for tail in ["", "/", "/a/b/c?x=1#f"] {
                let mut a = row();
                a.url = format!("https://{host}{tail}");
                check_row(&a, &exposes()).unwrap_or_else(|e| panic!("{}: {e}", a.url));
            }
        }
    }

    #[test]
    fn asset_must_be_a_bare_local_file_name() {
        for bad in [
            "",
            ".",
            "..",
            "../claude",
            "dl/claude",
            "/tmp/claude",
            "dl\\claude",
            "claude\0",
            // A newline (or any control byte) in the staging name.
            "a\nb",
            "claude\r",
            "claude\t",
            "claude\u{7f}",
        ] {
            let mut a = row();
            a.asset = bad.into();
            let m = refused(&a, &exposes());
            assert!(
                m.contains("asset must be a bare local file name"),
                "{bad:?}: {m}"
            );
        }
        let mut dotted = row();
        dotted.asset = "gh_2.80.0_macOS_arm64.zip".into();
        check_row(&dotted, &exposes()).expect("dots inside a name are fine");
    }

    #[test]
    fn refuses_an_unknown_payload() {
        for bad in ["", "tar", "tar.gz", "raw", "TAR-GZ", "pkg", "dmg-app"] {
            let mut a = row();
            a.payload = bad.into();
            a.entry = String::new();
            let m = refused(&a, &exposes());
            assert!(m.contains("payload must be"), "{bad:?}: {m}");
        }
    }

    #[test]
    fn raw_binary_needs_a_shimmable_exposed_entry() {
        let mut empty = row();
        empty.entry = String::new();
        assert!(refused(&empty, &exposes()).contains("non-empty entry"));
        let mut sensitive = row();
        sensitive.entry = "sudo".into();
        assert!(refused(&sensitive, &["sudo".to_string()]).contains("not an admissible tool name"));
        let mut sep = row();
        sep.entry = "bin/claude".into();
        assert!(refused(&sep, &exposes()).contains("not an admissible tool name"));
        let mut unexposed = row();
        unexposed.entry = "claude".into();
        assert!(refused(&unexposed, &["codex".to_string()]).contains("not in exposes"));
    }

    /// `tar-gz-binary` is a single-binary lane: a `binary` row with an exposed shimmable
    /// entry admits; no entry, an unexposed one, the app-bundle kind, a strip or a link
    /// is refused. The control: the same link admits on a `raw-binary`.
    #[test]
    fn a_tar_gz_binary_row_is_one_exposed_entry_and_nothing_else() {
        let exposes = ["agy".to_string(), "agy-alias".to_string()];
        let agy = || {
            let mut a = row();
            a.payload = "tar-gz-binary".into();
            a.entry = "agy".into();
            a
        };
        check_row(&agy(), &exposes).expect("one exposed entry admits");
        let mut a = agy();
        a.entry = String::new();
        assert!(refused(&a, &exposes).contains("non-empty entry is required by payload"));
        let mut a = agy();
        a.entry = "codex".into();
        assert!(refused(&a, &exposes).contains("entry is not in exposes"));
        let mut a = agy();
        a.kind = "app-bundle".into();
        assert!(refused(&a, &exposes).contains("needs payload = dmg"));
        let mut a = agy();
        a.strip_components = 1;
        assert!(refused(&a, &exposes).contains("strip_components applies to archive"));
        let mut a = agy();
        a.links.insert("agy-alias".into(), "bin/agy".into());
        assert!(refused(&a, &exposes).contains("links do not apply"));
        a.payload = "raw-binary".into();
        check_row(&a, &exposes).expect("a raw-binary may carry the same link");
    }

    #[test]
    fn entry_is_refused_on_non_raw_payloads() {
        let mut a = row();
        a.payload = "tar-gz".into();
        a.entry = "claude".into();
        assert!(refused(&a, &exposes()).contains("only meaningful for raw-binary"));
    }

    #[test]
    fn strip_components_only_on_archives() {
        let mut raw = row();
        raw.strip_components = 1;
        assert!(refused(&raw, &exposes()).contains("strip_components applies to archive"));
        let mut dmg = row();
        dmg.kind = "app-bundle".into();
        dmg.payload = "dmg".into();
        dmg.entry = String::new();
        dmg.strip_components = 2;
        assert!(refused(&dmg, &exposes()).contains("strip_components applies to archive"));
        for ok in ["tar-gz", "tar-zst", "zip"] {
            let mut a = row();
            a.payload = ok.into();
            a.entry = String::new();
            a.strip_components = 3;
            check_row(&a, &exposes()).unwrap_or_else(|e| panic!("{ok}: {e}"));
        }
    }

    #[test]
    fn links_keys_must_be_exposed_shimmable_names() {
        let mut a = row();
        a.kind = "app-bundle".into();
        a.payload = "dmg".into();
        a.entry = String::new();
        a.links
            .insert("emacs".into(), "Emacs.app/Contents/MacOS/Emacs".into());
        assert!(refused(&a, &["claude".to_string()]).contains("links key is not in exposes"));
        let mut s = row();
        s.kind = "app-bundle".into();
        s.payload = "dmg".into();
        s.entry = String::new();
        s.links.insert("git".into(), "Emacs.app/git".into());
        assert!(
            refused(&s, &["git".to_string()]).contains("links key is not an admissible tool name")
        );
    }

    #[test]
    fn links_targets_must_be_relative_and_dotdot_free() {
        for bad in [
            "/Applications/Emacs.app/Contents/MacOS/Emacs",
            "../outside",
            "Emacs.app/../../etc/passwd",
            "Emacs.app/./Contents",
            "",
            "Emacs.app/Contents/",
            "Emacs.app\\Contents\\MacOS\\Emacs",
            "a\0b",
        ] {
            let mut a = row();
            a.kind = "app-bundle".into();
            a.payload = "dmg".into();
            a.entry = String::new();
            a.links.insert("claude".into(), bad.into());
            let m = refused(&a, &exposes());
            assert!(m.contains("links target must be"), "{bad:?}: {m}");
        }
    }

    #[test]
    fn signed_digests_must_be_present_and_well_formed() {
        let mut zero = row();
        zero.size = 0;
        assert!(refused(&zero, &exposes()).contains("size must be > 0"));
        for bad in [
            "",
            "deadbeef",
            &"z".repeat(64),
            &"a".repeat(63),
            &"a".repeat(65),
        ] {
            let mut a = row();
            a.sha256 = bad.to_string();
            assert!(
                refused(&a, &exposes()).contains("sha256 must be 64 hex"),
                "{bad:?}"
            );
        }
        let mut upper = row();
        upper.sha256 = "7B09F01C".repeat(8);
        check_row(&upper, &exposes()).expect("hex case is not a refusal");
        let mut root = row();
        root.tree_root = String::new();
        assert!(refused(&root, &exposes()).contains("tree_root is required"));
    }

    /// `cmd.exe`'s PATHEXT rule, on every platform.
    #[test]
    fn windows_lookup_names_follow_pathext() {
        assert_eq!(
            windows_lookup_names("gh", ".COM;.EXE;.BAT;.CMD"),
            vec!["gh.COM", "gh.EXE", "gh.BAT", "gh.CMD"]
        );
        assert_eq!(
            windows_lookup_names("gh", ""),
            vec!["gh.COM", "gh.EXE", "gh.BAT", "gh.CMD"],
            "empty PATHEXT is the default"
        );
        assert_eq!(windows_lookup_names("gh.exe", ".COM;.EXE"), vec!["gh.exe"]);
        assert_eq!(
            windows_lookup_names("scoop.CMD", ".EXE;.cmd"),
            vec!["scoop.CMD"]
        );
        assert_eq!(
            windows_lookup_names("gh.tar", ".COM;.EXE"),
            vec!["gh.tar.COM", "gh.tar.EXE"],
            "an extension PATHEXT does not list is part of the name"
        );
        assert_eq!(
            windows_lookup_names("gh", ".EXE;;junk;.;.CMD"),
            vec!["gh.EXE", "gh.CMD"],
            "entries without a leading dot, and a bare dot, are ignored"
        );
        assert_eq!(
            windows_lookup_names("GNU.Emacs", ".EXE"),
            vec!["GNU.Emacs.EXE"]
        );
    }

    // ---- the vendor-direct URL pins ----

    /// The documents and payloads the vendor-direct lane fetches, by program.
    const VENDOR_DIRECT_URLS: &[(&str, &str)] = &[
        (
            "claude",
            "https://downloads.claude.ai/claude-code-releases/latest",
        ),
        (
            "claude",
            "https://downloads.claude.ai/claude-code-releases/2.1.280/manifest.json",
        ),
        (
            "claude",
            "https://downloads.claude.ai/claude-code-releases/2.1.280/manifest.json.sig",
        ),
        (
            "claude",
            "https://downloads.claude.ai/claude-code-releases/2.1.280/darwin-arm64/claude",
        ),
        (
            "claude",
            "https://downloads.claude.ai/claude-code-releases/2.1.280/win32-x64/claude.exe",
        ),
        ("codex", "https://releases.openai.com/codex/channels/latest"),
        (
            "codex",
            "https://releases.openai.com/codex/releases/0.156.0/argument-comment-lint",
        ),
        (
            "codex",
            "https://github.com/openai/codex/releases/download/rust-v0.156.0/codex-package_SHA256SUMS",
        ),
        (
            "codex",
            "https://github.com/openai/codex/releases/download/rust-v0.156.0/\
             codex-package-aarch64-apple-darwin.tar.gz",
        ),
        (
            "agy",
            "https://antigravity-cli-auto-updater-974169037036.us-central1.run.app/manifests/\
             darwin_arm64.json",
        ),
        (
            "agy",
            "https://storage.googleapis.com/antigravity-public/antigravity-cli/\
             1.3.6-5068262057312256/darwin-arm/manifest.json",
        ),
        (
            "agy",
            "https://storage.googleapis.com/antigravity-public/antigravity-cli/\
             1.3.6-5068262057312256/darwin-arm/cli_mac_arm64.tar.gz",
        ),
        ("muse", "https://api.meta.ai/muse-code/channels/muse-stable"),
        (
            "muse",
            "https://lookaside.facebook.com/lookaside/muse/download/?channel=muse\
             &version=1.4.9-R6297.1&file=manifest.json",
        ),
        (
            "muse",
            "https://lookaside.facebook.com/lookaside/muse/download/?channel=muse\
             &version=1.4.9-R6297.1&file=muse-aarch64-macos",
        ),
    ];

    #[test]
    fn the_vendor_direct_pins_admit_the_real_urls() {
        for (program, url) in VENDOR_DIRECT_URLS {
            assert!(vendor_direct_url_allowed(program, url), "{program} {url}");
        }
    }

    /// The pin is per program: no vendor's URLs are admitted for another's
    /// program, and a program with no row is admitted nowhere.
    #[test]
    fn a_vendor_direct_url_is_admitted_only_for_its_own_program() {
        let programs = crate::vendor_direct::VENDORS.iter().map(|s| s.program);
        for (program, url) in VENDOR_DIRECT_URLS {
            for other in programs.clone().chain(["ay", "", "CLAUDE"]) {
                if other != *program {
                    assert!(
                        !vendor_direct_url_allowed(other, url),
                        "{url} admitted for {other:?}"
                    );
                }
            }
        }
    }

    /// Lookalike hosts, other schemes, userinfo, ports, case and trailing-dot spellings,
    /// traversal and escapes, queries and fragments, and anything off the pinned prefixes.
    #[test]
    fn the_vendor_direct_pins_refuse_everything_else() {
        for bad in [
            // lookalike and neighbouring hosts
            "https://downloads.claude.ai.evil.com/claude-code-releases/latest",
            "https://evil.downloads.claude.ai/claude-code-releases/latest",
            "https://downloads.claude.ai.evil.com/",
            "https://releases.openai.com.evil.com/codex/channels/latest",
            "https://objects.githubusercontent.com/openai/codex/releases/download/x",
            "https://api.github.com/repos/openai/codex/releases/latest",
            // scheme
            "http://downloads.claude.ai/claude-code-releases/latest",
            "HTTPS://downloads.claude.ai/claude-code-releases/latest",
            "ftp://downloads.claude.ai/claude-code-releases/latest",
            // userinfo and port
            "https://user@downloads.claude.ai/claude-code-releases/latest",
            "https://downloads.claude.ai@evil.com/claude-code-releases/latest",
            "https://downloads.claude.ai:443/claude-code-releases/latest",
            // case and trailing dot on the host
            "https://DOWNLOADS.CLAUDE.AI/claude-code-releases/latest",
            "https://Downloads.claude.ai/claude-code-releases/latest",
            "https://downloads.claude.ai./claude-code-releases/latest",
            "https://github.com./openai/codex/releases/download/rust-v0.156.0/x",
            // off the prefix on a pinned host
            "https://downloads.claude.ai/other/latest",
            "https://downloads.claude.ai/claude-code-releases",
            "https://downloads.claude.ai/claude-code-releases/",
            "https://github.com/openai/codex-evil/releases/download/x/y",
            "https://github.com/evil/codex/releases/download/rust-v0.156.0/x",
            "https://github.com/openai/codex/releases/latest/download",
            "https://releases.openai.com/codexx/channels/latest",
            "https://releases.openai.com/other/codex/x",
            // traversal and escapes, dot segments in any spelling, wherever they sit
            "https://downloads.claude.ai/claude-code-releases/../../evil",
            "https://downloads.claude.ai/claude-code-releases/%2E%2E/evil",
            "https://downloads.claude.ai/claude-code-releases/.%2e/evil",
            "https://downloads.claude.ai/claude-code-releases/%2e/latest",
            "https://downloads.claude.ai/./claude-code-releases/latest",
            "https://github.com/openai/codex/releases/download/../../x/y/releases/download/z",
            "https://github.com/openai/codex/releases/download/rust-v0.156.0/%2e%2e/x",
            "https://releases.openai.com/codex/../codex/channels/latest",
            "https://downloads.claude.ai/claude-code-releases/2.1.280/../../../x",
            "https://downloads.claude.ai/claude-code-releases/./latest",
            "https://downloads.claude.ai/claude-code-releases/%2e%2e/evil",
            "https://downloads.claude.ai/claude-code-releases/a%2Fb",
            "https://downloads.claude.ai/claude-code-releases/a\\..\\b",
            "https://downloads.claude.ai/claude-code-releases//latest",
            "https://downloads.claude.ai/claude-code-releases/latest/",
            // query strings and fragments
            "https://downloads.claude.ai/claude-code-releases/latest?x=1",
            "https://downloads.claude.ai/claude-code-releases/latest#f",
            "https://releases.openai.com/codex/channels/latest?redirect=https://evil",
            // whitespace and control bytes
            "https://downloads.claude.ai/claude-code-releases/lat est",
            "https://downloads.claude.ai/claude-code-releases/latest\n",
            "",
        ] {
            for program in crate::vendor_direct::VENDORS.iter().map(|s| s.program) {
                assert!(!vendor_direct_url_allowed(program, bad), "{bad:?}");
            }
        }
    }

    /// A row that declares a query grammar admits exactly that query right after a prefix
    /// (Meta's lookaside URLs) and nothing near it; the same row with no grammar admits none.
    #[test]
    fn a_declared_query_is_admitted_exactly() {
        const DOWNLOAD: &str = "https://lookaside.facebook.com/lookaside/muse/download/";
        let row = crate::vendor_direct::VendorSpec {
            url_prefixes: &[DOWNLOAD, "https://api.meta.ai/muse-code/channels/"],
            url_query: &["channel", "version", "file"],
            ..*crate::vendor_direct::spec("claude").unwrap()
        };
        let q = |query: &str| format!("{DOWNLOAD}?{query}");
        let real = [
            q("channel=muse&version=1.4.9-R6297.1&file=manifest.json"),
            q("channel=muse&version=1.4.9-R6297.1&file=muse-aarch64-macos"),
            q("channel=muse&version=1.4.9-R6297&file=muse-x86-macos"),
        ];
        for url in &real {
            assert!(pinned_by(&row, url), "{url}");
        }
        assert!(pinned_by(
            &row,
            "https://api.meta.ai/muse-code/channels/muse-stable"
        ));
        for bad in [
            // an extra, missing, reordered or repeated key
            q("channel=muse&version=1.4.9-R6297.1&file=manifest.json&x=1"),
            q("channel=muse&version=1.4.9-R6297.1"),
            q("version=1.4.9-R6297.1&channel=muse&file=manifest.json"),
            q("channel=muse&channel=muse&version=1.4.9-R6297.1&file=manifest.json"),
            q("channel=muse&version=1.4.9-R6297.1&version=1.4.9-R6297.1&file=manifest.json"),
            q("Channel=muse&version=1.4.9-R6297.1&file=manifest.json"),
            // an empty, missing, dotted, escaped or structured value
            q("channel=&version=1.4.9-R6297.1&file=manifest.json"),
            q("channel&version=1.4.9-R6297.1&file=manifest.json"),
            q("channel=muse&version=1.4.9-R6297.1&file=.."),
            q("channel=muse&version=1.4.9-R6297.1&file=%2e%2e"),
            q("channel=muse&version=1.4.9-R6297.1&file=manifest%2ejson"),
            q("channel=muse&version=1.4.9-R6297.1&file=../manifest.json"),
            q("channel=muse&version=1.4.9-R6297.1&file=a=b"),
            q("channel=muse&version=1.4.9-R6297.1&file=manifest json"),
            // empty pairs, a fragment, an empty query
            q("channel=muse&&version=1.4.9-R6297.1&file=manifest.json"),
            q("channel=muse&version=1.4.9-R6297.1&file=manifest.json&"),
            q("channel=muse&version=1.4.9-R6297.1&file=manifest.json#frag"),
            q(""),
            // a path remainder before the query, or none of the prefix
            format!("{DOWNLOAD}x?channel=muse&version=1.4.9-R6297.1&file=manifest.json"),
            "https://lookaside.facebook.com/lookaside/muse/download?channel=muse\
             &version=1.4.9-R6297.1&file=manifest.json"
                .to_string(),
            "https://lookaside.facebook.com/lookaside/other/download/?channel=muse\
             &version=1.4.9-R6297.1&file=manifest.json"
                .to_string(),
            "https://api.meta.ai/muse-code/channels/muse-stable?channel=muse".to_string(),
        ] {
            assert!(!pinned_by(&row, &bad), "{bad}");
        }
        let no_query = crate::vendor_direct::VendorSpec {
            url_query: &[],
            ..row
        };
        for url in &real {
            assert!(!pinned_by(&no_query, url), "{url}");
        }
    }

    /// OpenAI's release JSON and Anthropic's documents are taken only unredirected (the
    /// compiled table's rule); a GitHub release download may end on GitHub's release-asset
    /// storage and nowhere else; and the request must be pinned for the program it serves.
    #[test]
    fn a_digest_document_lands_only_where_its_host_allows() {
        const CLAUDE: &str =
            "https://downloads.claude.ai/claude-code-releases/2.1.280/manifest.json";
        const OPENAI: &str = "https://releases.openai.com/codex/channels/latest";
        const SUMS: &str = "https://github.com/openai/codex/releases/download/rust-v0.156.0/codex-package_SHA256SUMS";
        // SUMS's measured final hop, signed query and all.
        const STORAGE: &str = "https://release-assets.githubusercontent.com/\
                               github-production-release-asset/965415649/ce906d71?sp=r&sig=x%3D";
        assert!(digest_effective_url_ok("claude", CLAUDE, CLAUDE));
        assert!(digest_effective_url_ok("codex", OPENAI, OPENAI));
        assert!(digest_effective_url_ok("codex", SUMS, SUMS));
        assert!(digest_effective_url_ok("codex", SUMS, STORAGE));
        assert!(digest_effective_url_ok(
            "codex",
            SUMS,
            "https://objects.githubusercontent.com/github-production-release-asset/1/2"
        ));

        // The request must be pinned for the program it serves.
        assert!(!digest_effective_url_ok("claude", OPENAI, OPENAI));
        assert!(!digest_effective_url_ok("codex", CLAUDE, CLAUDE));
        assert!(!digest_effective_url_ok("claude", SUMS, STORAGE));

        for (requested, effective) in [
            // any redirect of Anthropic's documents or OpenAI's JSON, even on their own
            // host: the table pins both to "answered in place"
            (
                CLAUDE,
                "https://downloads.claude.ai/claude-code-releases/2.1.280/other.json",
            ),
            (
                OPENAI,
                "https://releases.openai.com/codex/releases/0.156.0/release.json",
            ),
            (OPENAI, "https://releases.openai.com/codex/channels/latest/"),
            // off-host redirects
            (
                OPENAI,
                "https://github.com/openai/codex/releases/download/x/y",
            ),
            (OPENAI, STORAGE),
            (CLAUDE, STORAGE),
            (CLAUDE, "https://storage.googleapis.com/claude-code-dist/x"),
            (
                SUMS,
                "https://raw.githubusercontent.com/openai/codex/main/x",
            ),
            (SUMS, "https://evil.example/x"),
            (
                SUMS,
                "https://release-assets.githubusercontent.com.evil.com/x",
            ),
            // the effective URL must itself be an admissible https URL
            (OPENAI, "http://releases.openai.com/codex/channels/latest"),
            (
                OPENAI,
                "https://releases.openai.com:8443/codex/channels/latest",
            ),
            (
                OPENAI,
                "https://x@releases.openai.com/codex/channels/latest",
            ),
            (OPENAI, "https://RELEASES.OPENAI.COM/codex/channels/latest"),
            (OPENAI, "https://releases.openai.com./codex/channels/latest"),
            (SUMS, ""),
            // the request itself must be pinned
            (
                "https://releases.openai.com/other/x",
                "https://releases.openai.com/other/x",
            ),
            (
                "https://github.com/evil/repo/releases/download/t/a",
                STORAGE,
            ),
        ] {
            for program in ["claude", "codex"] {
                assert!(
                    !digest_effective_url_ok(program, requested, effective),
                    "{program}: {requested} -> {effective}"
                );
            }
        }
    }

    // ---- the system-satisfaction probe ----

    #[cfg(unix)]
    fn scratch(label: &str) -> crate::testdir::Dir {
        crate::testdir::dir(&format!("atpkg-vendor-{label}-"))
    }

    #[cfg(unix)]
    fn lay_exe(dir: &Path, name: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::create_dir_all(dir).unwrap();
        let p = dir.join(name);
        std::fs::write(&p, b"#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    #[cfg(unix)]
    #[test]
    fn a_system_binary_on_path_satisfies_and_the_managed_copy_never_does() {
        let root = scratch("probe");
        let prefix = root.join("prefix");
        let sys = root.join("usr-local-bin");
        let exe = lay_exe(&sys, "gh");
        // The managed bin/ and a store tree carry the same name — both must be skipped.
        lay_exe(&prefix.join("bin"), "gh");
        lay_exe(&prefix.join("store/gh/1/bin"), "gh");
        let path = std::env::join_paths([
            prefix.join("bin"),
            prefix.join("store/gh/1/bin"),
            sys.clone(),
        ])
        .unwrap();
        assert_eq!(
            system_binary_on_path(&prefix, "gh", Some(&path)),
            Some(exe.clone())
        );
        // Only managed dirs on PATH ⇒ not satisfied.
        let managed_only =
            std::env::join_paths([prefix.join("bin"), prefix.join("store/gh/1/bin")]).unwrap();
        assert_eq!(
            system_binary_on_path(&prefix, "gh", Some(&managed_only)),
            None
        );
        // A symlink elsewhere that resolves into the store is still the managed copy.
        let link_dir = root.join("home-bin");
        std::fs::create_dir_all(&link_dir).unwrap();
        std::os::unix::fs::symlink(prefix.join("store/gh/1/bin/gh"), link_dir.join("gh")).unwrap();
        let via_link = std::env::join_paths([link_dir.clone()]).unwrap();
        assert_eq!(system_binary_on_path(&prefix, "gh", Some(&via_link)), None);
        // A symlink to a genuine system copy (Homebrew's shape) is a system install.
        let brew = root.join("brew-bin");
        std::fs::create_dir_all(&brew).unwrap();
        std::os::unix::fs::symlink(&exe, brew.join("gh")).unwrap();
        let via_brew = std::env::join_paths([brew.clone()]).unwrap();
        assert_eq!(
            system_binary_on_path(&prefix, "gh", Some(&via_brew)),
            Some(brew.join("gh"))
        );
        // A relative PATH entry never counts, even one that resolves (from this process's
        // cwd) to the directory holding the system copy: `cwd/../..` up to `/`, then the
        // system dir's own components.
        let cwd = std::env::current_dir().unwrap();
        let ups: PathBuf =
            std::iter::repeat_n("..", cwd.components().count().saturating_sub(1)).collect();
        let relative = ups.join(sys.strip_prefix("/").unwrap());
        assert!(!relative.is_absolute());
        assert!(
            relative.join("gh").exists(),
            "PRECONDITION: the relative spelling really reaches the system copy ({})",
            relative.display()
        );
        let via_relative = std::env::join_paths([relative.clone()]).unwrap();
        assert_eq!(
            system_binary_on_path(&prefix, "gh", Some(&via_relative)),
            None,
            "a relative PATH entry is the cwd, not a system"
        );
        let dot = std::env::join_paths([PathBuf::from("."), PathBuf::from("bin")]).unwrap();
        assert_eq!(system_binary_on_path(&prefix, "gh", Some(&dot)), None);
        // …while the absolute spelling of the same directory still satisfies.
        let mixed = std::env::join_paths([relative, sys.clone()]).unwrap();
        assert_eq!(
            system_binary_on_path(&prefix, "gh", Some(&mixed)),
            Some(exe.clone())
        );
        // Absent name, non-executable file, no PATH at all, a separator in the name.
        assert_eq!(system_binary_on_path(&prefix, "emacs", Some(&path)), None);
        std::fs::write(sys.join("plain"), b"x").unwrap();
        assert_eq!(system_binary_on_path(&prefix, "plain", Some(&path)), None);
        assert_eq!(system_binary_on_path(&prefix, "gh", None), None);
        assert_eq!(system_binary_on_path(&prefix, "../gh", Some(&path)), None);
        assert_eq!(system_binary_on_path(&prefix, "", Some(&path)), None);
        // A name the shim deny-list refuses never satisfies, even on PATH.
        lay_exe(&sys, "cargo");
        assert_eq!(system_binary_on_path(&prefix, "cargo", Some(&path)), None);
        // The Program-level wrapper: no `system` key ⇒ never satisfied, whatever PATH holds.
        let unmanaged = Program {
            repo: "gh".into(),
            policy: String::new(),
            coherence_group: None,
            system: None,
            unavailable_hint: None,
            requires: vec![],
        };
        assert_eq!(system_satisfied(&prefix, &unmanaged), None);
        let _ = std::fs::remove_dir_all(&root);
    }

    // ---- the shadow probe ----

    /// A hit before the managed `bin/` (or with it absent) is a shadow; a hit after it is
    /// not; the managed dirs and links into the store never are; relative entries never count.
    #[cfg(unix)]
    #[test]
    fn the_shadow_probe_reports_only_what_precedes_the_managed_bin() {
        let root = scratch("shadow");
        let prefix = root.join("prefix");
        let local = root.join("local-bin");
        let exe = lay_exe(&local, "trust");
        lay_exe(&prefix.join("bin"), "trust");
        lay_exe(&prefix.join("store/trust/6808/bin"), "trust");
        // Before the managed bin/: shadowed.
        let before = std::env::join_paths([local.clone(), prefix.join("bin")]).unwrap();
        assert_eq!(
            shadowing_binary_on_path(&prefix, "trust", Some(&before)),
            Some(exe.clone())
        );
        // After it: the managed copy wins, no shadow.
        let after = std::env::join_paths([prefix.join("bin"), local.clone()]).unwrap();
        assert_eq!(
            shadowing_binary_on_path(&prefix, "trust", Some(&after)),
            None
        );
        // Managed bin/ not on PATH at all (a doctor run outside an aterm shell): inside
        // aterm it is appended last, so the foreign copy still precedes it.
        let absent = std::env::join_paths([local.clone()]).unwrap();
        assert_eq!(
            shadowing_binary_on_path(&prefix, "trust", Some(&absent)),
            Some(exe.clone())
        );
        // A store dir on PATH ahead of bin/ is atpkg's own — holding the name, it stops
        // the walk too.
        let store_first =
            std::env::join_paths([prefix.join("store/trust/6808/bin"), local.clone()]).unwrap();
        assert_eq!(
            shadowing_binary_on_path(&prefix, "trust", Some(&store_first)),
            None
        );
        // `reroute/` at PATH[0], where every aterm session puts it, carries only stubs, so
        // it must not hide a foreign `trust` behind it: the walk goes on.
        let reroute = prefix.join("reroute");
        lay_exe(&reroute, "cargo");
        let reroute_first =
            std::env::join_paths([reroute.clone(), local.clone(), prefix.join("bin")]).unwrap();
        assert_eq!(
            shadowing_binary_on_path(&prefix, "trust", Some(&reroute_first)),
            Some(exe.clone()),
            "the reroute dir is transparent for a name it does not carry"
        );
        // `agents/` at PATH[0] carrying the name is the managed copy running: nothing after
        // it shadows; for a name it does not carry it is transparent like `reroute/`.
        let agents = prefix.join("agents");
        lay_exe(&agents, "claude");
        lay_exe(&prefix.join("store/claude/2026091001/bin"), "claude");
        let foreign_claude = lay_exe(&local, "claude");
        let agents_first =
            std::env::join_paths([agents.clone(), local.clone(), prefix.join("bin")]).unwrap();
        assert_eq!(
            shadowing_binary_on_path(&prefix, "claude", Some(&agents_first)),
            None,
            "the agents twin at PATH[0] is the managed copy running"
        );
        assert_eq!(
            shadowing_binary_on_path(&prefix, "trust", Some(&agents_first)),
            Some(exe.clone()),
            "and transparent for every other name"
        );
        // The out-ranked list: every foreign copy, in PATH order, the managed dirs and
        // store-resolving links skipped, duplicates by real location folded.
        let other = root.join("other-bin");
        let other_claude = lay_exe(&other, "claude");
        let dup = root.join("dup-bin");
        std::fs::create_dir_all(&dup).unwrap();
        std::os::unix::fs::symlink(&other_claude, dup.join("claude")).unwrap();
        let wide = std::env::join_paths([
            agents.clone(),
            local.clone(),
            other.clone(),
            dup.clone(),
            prefix.join("bin"),
        ])
        .unwrap();
        assert_eq!(
            foreign_copies_on_path(&prefix, "claude", Some(&wide)),
            vec![foreign_claude.clone(), other_claude.clone()]
        );
        assert!(foreign_copies_on_path(&prefix, "trust-mc", Some(&wide)).is_empty());
        assert!(foreign_copies_on_path(&prefix, "claude", None).is_empty());
        assert!(foreign_copies_on_path(&prefix, "../claude", Some(&wide)).is_empty());
        // A symlink into the store is not a shadow.
        let link_dir = root.join("home-bin");
        std::fs::create_dir_all(&link_dir).unwrap();
        std::os::unix::fs::symlink(
            prefix.join("store/trust/6808/bin/trust"),
            link_dir.join("trust"),
        )
        .unwrap();
        let via_link = std::env::join_paths([link_dir.clone(), prefix.join("bin")]).unwrap();
        assert_eq!(
            shadowing_binary_on_path(&prefix, "trust", Some(&via_link)),
            None
        );
        // A user's `~/bin` that is the managed bin/ through a symlink, ahead of a foreign
        // copy: the managed copy runs, so the foreign one behind it is no shadow.
        let alias = root.join("alias-bin");
        std::os::unix::fs::symlink(prefix.join("bin"), &alias).unwrap();
        let via_alias = std::env::join_paths([alias.clone(), local.clone()]).unwrap();
        assert_eq!(
            shadowing_binary_on_path(&prefix, "trust", Some(&via_alias)),
            None,
            "a symlinked managed bin/ ahead stops the walk"
        );
        // A user's own symlink to the store copy ahead of a foreign copy: same thing.
        let link_then_local = std::env::join_paths([link_dir.clone(), local.clone()]).unwrap();
        assert_eq!(
            shadowing_binary_on_path(&prefix, "trust", Some(&link_then_local)),
            None,
            "a link into the store ahead is the managed copy running"
        );
        // …but the system probe skips the same aliases, and the foreign copy behind them
        // still satisfies.
        assert_eq!(
            system_binary_on_path(&prefix, "trust", Some(&via_alias)),
            Some(exe.clone())
        );
        assert_eq!(
            system_binary_on_path(&prefix, "trust", Some(&link_then_local)),
            Some(exe.clone())
        );
        // A directory wearing the tool's name is never a hit (the Windows `gh.exe`
        // directory trick reads the same way: `is_file` decides, not the name).
        let dir_named = root.join("dir-named");
        std::fs::create_dir_all(dir_named.join("trust")).unwrap();
        let dir_first = std::env::join_paths([dir_named.clone(), prefix.join("bin")]).unwrap();
        assert_eq!(
            shadowing_binary_on_path(&prefix, "trust", Some(&dir_first)),
            None
        );
        assert_eq!(
            system_binary_on_path(&prefix, "trust", Some(&dir_first)),
            None
        );
        // Relative entries never count; a different name is not a shadow; no PATH, none.
        let dot = std::env::join_paths([PathBuf::from("."), prefix.join("bin")]).unwrap();
        assert_eq!(shadowing_binary_on_path(&prefix, "trust", Some(&dot)), None);
        assert_eq!(
            shadowing_binary_on_path(&prefix, "trust-mc", Some(&before)),
            None
        );
        assert_eq!(shadowing_binary_on_path(&prefix, "trust", None), None);
        assert_eq!(
            shadowing_binary_on_path(&prefix, "../trust", Some(&before)),
            None
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// `agents/` on PATH or not, as spelled or as it resolves, absolute entries only.
    #[test]
    fn agents_dir_on_path_sees_the_dir_as_spelled_or_resolved_and_absolute_only() {
        let prefix = crate::testdir::dir("atpkg-vendor-agents-");
        std::fs::create_dir_all(prefix.join("agents")).unwrap();
        let agents = prefix.join("agents");
        assert!(!agents_dir_on_path(&prefix, None));
        let without =
            std::env::join_paths([prefix.join("bin"), PathBuf::from("/usr/bin")]).unwrap();
        assert!(!agents_dir_on_path(&prefix, Some(&without)));
        let with = std::env::join_paths([PathBuf::from("/usr/bin"), agents.clone()]).unwrap();
        assert!(
            agents_dir_on_path(&prefix, Some(&with)),
            "anywhere on PATH counts"
        );
        let relative = std::ffi::OsString::from("agents:/usr/bin");
        assert!(
            !agents_dir_on_path(&prefix, Some(&relative)),
            "a relative entry never counts"
        );
        #[cfg(unix)]
        {
            let link = prefix.join("agents-link");
            std::os::unix::fs::symlink(&agents, &link).unwrap();
            let via_link = std::env::join_paths([link]).unwrap();
            assert!(
                agents_dir_on_path(&prefix, Some(&via_link)),
                "as it resolves"
            );
        }
        let _ = std::fs::remove_dir_all(&prefix);
    }
}
