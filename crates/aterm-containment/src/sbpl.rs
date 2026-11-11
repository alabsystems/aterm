// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! macOS Seatbelt (SBPL) sandbox profile generation for the containment actuator.

use crate::capability::{FsCapability, NetworkCapability};
use crate::policy::Capabilities;
use std::path::{Path, PathBuf};

/// Fixed absolute path to `/usr/bin/sandbox-exec`.
pub const SANDBOX_EXEC_PATH: &str = aterm_uds::os_path!("sandbox-exec");

/// Defines [`MACH_LOOKUP_ALLOWLIST`] and [`BASE_PROFILE`] from one list of
/// global names, so the names the profile allows and the names its proofs read
/// are the same list by construction.
macro_rules! base_profile_allowing {
    ($($name:literal,)+) => {
        /// Global Mach service names a contained process is permitted to look up.
        pub const MACH_LOOKUP_ALLOWLIST: &[&str] = &[$($name,)+];

        /// Base SBPL profile denying network access and restricting Mach lookups.
        pub const BASE_PROFILE: &str = concat!(
            "(version 1)(allow default)(deny network*)",
            "(deny mach-priv-task-port)(allow mach-priv-task-port (target self))",
            "(deny mach-lookup)(allow mach-lookup",
            $(" (global-name \"", $name, "\")",)+
            ")",
        );
    };
}

base_profile_allowing!(
    "com.apple.system.opendirectoryd.libinfo",
    "com.apple.system.opendirectoryd.membership",
);

/// Canonical writable root paths for [`FsCapability::TmpOnly`] containment.
pub const WRITABLE_ROOTS: &[&str] = &["/private/tmp", "/private/var/tmp", "/dev"];

/// Parent directory of per-user macOS `$TMPDIR` roots (`/private/var/folders`).
pub const USER_TEMP_PARENT: &str = "/private/var/folders";

/// The user's own `$TMPDIR` as a canonical write root, admitted only in the one
/// shape macOS gives it, `<USER_TEMP_PARENT>/<xx>/<id>/T`, or as that folder's quiet
/// folder `T/.ubd` ([`user_temp_root`]), which a test run's `TMPDIR` names: then only
/// the quiet folder is writable.
fn admitted_user_temp(temp: &Path) -> Option<PathBuf> {
    let canon = std::fs::canonicalize(temp).ok()?;
    let t = user_temp_root(&canon);
    let bucket = t.parent()?.parent()?;
    (t.file_name()? == "T" && bucket.parent()? == Path::new(USER_TEMP_PARENT)).then_some(canon)
}

/// `temp` without a last [`aterm_tempfile::QUIET`] component: `…/T/.ubd` is `…/T`, any
/// other path itself.
pub(crate) fn user_temp_root(temp: &Path) -> &Path {
    match (temp.file_name(), temp.parent()) {
        (Some(name), Some(parent)) if name == aterm_tempfile::QUIET => parent,
        _ => temp,
    }
}

/// SBPL rule denying `user-preference-write` under [`FsCapability::TmpOnly`].
pub const PREFERENCE_WRITE_DENY: &str = "(deny user-preference-write)";

/// Shell history files under `$HOME` permitted for write under [`FsCapability::TmpOnly`].
pub const HISTORY_FILES: &[&[(&str, &str)]] = &[
    &[
        (".zsh_history", "REGULAR-FILE"),
        (".zsh_history.new", "REGULAR-FILE"),
        (".zsh_history.LOCK", "SYMLINK"),
    ],
    &[(".bash_history", "REGULAR-FILE")],
];

/// The write-confinement rules of a `TmpOnly` profile: deny every write, then allow
/// [`WRITABLE_ROOTS`], the user's `$TMPDIR` where [`admitted_user_temp`] admits
/// it, and, with a home, the [`HISTORY_FILES`] under it.
fn push_write_confinement(
    profile: &mut String,
    user_temp: Option<&Path>,
    canon_home: Option<&Path>,
) {
    profile.push_str(PREFERENCE_WRITE_DENY);
    profile.push_str("(deny file-write*)(allow file-write*");
    for root in WRITABLE_ROOTS {
        push_subpath_clause(profile, Path::new(root));
    }
    if let Some(temp) = user_temp.and_then(admitted_user_temp) {
        push_subpath_clause(profile, &temp);
    }
    if let Some(home) = canon_home {
        push_history_allows(profile, home);
    }
    profile.push(')');
}

/// Append the [`HISTORY_FILES`] allows under `canon_home`: for each row whose
/// regular-file entries are all absent or plain single-link files (and whose
/// symlink entries are absent or symlinks), every name of the row as
/// ` (require-all (literal "<home>/<name>") (vnode-type <type>))`.
fn push_history_allows(out: &mut String, canon_home: &Path) {
    for row in HISTORY_FILES {
        if row.is_empty()
            || !row.iter().all(|(name, vnode_type)| {
                history_entry_admitted(&canon_home.join(name), vnode_type)
            })
        {
            continue;
        }
        for (name, vnode_type) in *row {
            out.push_str(" (require-all");
            push_literal_clause(out, &canon_home.join(name));
            out.push_str(" (vnode-type ");
            out.push_str(vnode_type);
            out.push_str("))");
        }
    }
}

/// Whether `path` is absent, or matches the expected `vnode_type` without
/// widening write access (for `"REGULAR-FILE"`, a regular file not followed
/// through a symlink with exactly one link; for `"SYMLINK"`, a symlink).
fn history_entry_admitted(path: &Path, vnode_type: &str) -> bool {
    match vnode_type {
        "REGULAR-FILE" => plain_or_absent(path),
        "SYMLINK" => symlink_or_absent(path),
        _ => false,
    }
}

/// Whether `path` is absent, or a regular file (not followed through a symlink)
/// with exactly one link.
fn plain_or_absent(path: &Path) -> bool {
    match std::fs::symlink_metadata(path) {
        Err(e) => e.kind() == std::io::ErrorKind::NotFound,
        Ok(meta) => meta.file_type().is_file() && single_link(&meta),
    }
}

/// Whether `path` is absent, or a symlink.
fn symlink_or_absent(path: &Path) -> bool {
    match std::fs::symlink_metadata(path) {
        Err(e) => e.kind() == std::io::ErrorKind::NotFound,
        Ok(meta) => meta.file_type().is_symlink(),
    }
}

#[cfg(unix)]
fn single_link(meta: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    meta.nlink() == 1
}

#[cfg(not(unix))]
fn single_link(_meta: &std::fs::Metadata) -> bool {
    true
}

/// Credential and secret subdirectories under `$HOME` denied for read and write.
pub const SECRET_SUBDIRS: &[&str] = &[
    ".ssh",
    ".aws",
    ".gnupg",
    ".config/gh",
    ".config/aterm",
    ".config/aterm/secrets",
    ".secrets",
    ".kube",
    ".docker",
    ".config/gcloud",
    ".azure",
    // The release signing identity the secrets vault fills on every Mac (Developer ID key
    // and the shared release ring).
    ".aterm/apple",
    ".aterm/release-ring",
];

/// Credential files under `$HOME` denied as `literal` paths for read and write, with their
/// transient siblings ([`push_siblings_clause`]): git's two `store` files (the secrets vault
/// writes the XDG one), and the release signing key and profile the vault fills beside
/// `~/.aterm/shell.d`, which tabs must still read.
pub const SECRET_LITERAL_FILES: &[&str] = &[
    ".netrc",
    ".git-credentials",
    ".config/git/credentials",
    ".npmrc",
    ".pypirc",
    ".aterm/machine.key",
    ".aterm/release-credentials.toml",
];

/// TCC-protected and sensitive user-data subdirectories under `$HOME` denied for read and write.
pub const PRIVATE_SUBDIRS: &[&str] = &[
    "Documents",
    "Desktop",
    "Downloads",
    "Pictures",
    "Movies",
    "Music",
    "Library/Mail",
    "Library/Messages",
    "Library/Keychains",
    "Library/Cookies",
    "Library/Safari",
    "Library/Application Support/Google/Chrome",
    "Library/Application Support/Chromium",
    "Library/Application Support/Firefox",
    "Library/Application Support/BraveSoftware",
    "Library/Application Support/Microsoft Edge",
];

/// App-container roots under `$HOME` guarded by `kTCCServiceSystemPolicyAppData`.
pub const APP_DATA_ROOTS: &[&str] = &["Library/Containers", "Library/Group Containers"];

/// Generate the SBPL profile for `caps`, or `None` when no OS sandbox is required.
#[must_use]
pub fn profile_for(caps: &Capabilities) -> Option<String> {
    profile_for_scoped(*caps, home_dir().as_deref()).map(|(profile, _)| profile)
}

/// Generate the SBPL profile for `caps` and `home`, returning `(profile, home_scoped)`.
#[must_use]
pub(crate) fn profile_for_scoped(caps: Capabilities, home: Option<&str>) -> Option<(String, bool)> {
    match caps.network {
        NetworkCapability::None => Some(profile_scoped(home, Some(&std::env::temp_dir()), caps.fs)),
        NetworkCapability::Full => None,
    }
}

/// Read non-empty `$HOME` from the environment.
pub(crate) fn home_dir() -> Option<String> {
    match std::env::var("HOME") {
        Ok(h) if !h.is_empty() => Some(h),
        _ => None,
    }
}

/// Build the containment SBPL profile for `home`, `user_temp`, and `fs`.
#[must_use]
pub fn profile_for_home(home: Option<&str>, user_temp: Option<&Path>, fs: FsCapability) -> String {
    profile_scoped(home, user_temp, fs).0
}

/// [`profile_for_home`], and whether the home resolved — i.e. whether the
/// home-scoped read denies are in the profile.
fn profile_scoped(
    home: Option<&str>,
    user_temp: Option<&Path>,
    fs: FsCapability,
) -> (String, bool) {
    let tmp_only = fs == FsCapability::TmpOnly;
    let canon_home = home
        .filter(|h| !h.is_empty())
        .and_then(|h| std::fs::canonicalize(h).ok());

    let mut profile = String::new();
    profile.push_str(BASE_PROFILE);
    if tmp_only {
        push_write_confinement(&mut profile, user_temp, canon_home.as_deref());
    }
    let Some(canon_home) = canon_home else {
        return (profile, false);
    };

    let mut clauses = String::new();
    for sub in SECRET_SUBDIRS {
        push_resolved_clause(&mut clauses, &canon_home, sub, push_subpath_clause);
    }
    for file in SECRET_LITERAL_FILES {
        push_resolved_clause(&mut clauses, &canon_home, file, push_literal_clause);
        push_resolved_clause(&mut clauses, &canon_home, file, push_siblings_clause);
    }
    if tmp_only {
        for sub in PRIVATE_SUBDIRS {
            push_resolved_clause(&mut clauses, &canon_home, sub, push_subpath_clause);
        }
    }

    let mut ancestor_clauses = String::new();
    let mut denied_ancestors: Vec<PathBuf> = Vec::new();
    let mut nested: Vec<&str> = SECRET_SUBDIRS
        .iter()
        .chain(SECRET_LITERAL_FILES.iter())
        .copied()
        .collect();
    if tmp_only {
        nested.extend(PRIVATE_SUBDIRS.iter().copied());
    }
    for entry in nested {
        push_existing_ancestor_write_denies(
            &mut ancestor_clauses,
            &canon_home,
            entry,
            &mut denied_ancestors,
        );
    }

    profile.push_str("(deny file-read* file-write*");
    profile.push_str(&clauses);
    profile.push(')');
    if !ancestor_clauses.is_empty() {
        profile.push_str("(deny file-write*");
        profile.push_str(&ancestor_clauses);
        profile.push(')');
    }
    (profile, true)
}

/// Emit the deny clause(s) for the secret/private entry `<canon_home>/<entry>`,
/// resolving symlinks at the leaf and along every ancestor component as Seatbelt does.
fn push_resolved_clause(
    out: &mut String,
    canon_home: &Path,
    entry: &str,
    push: fn(&mut String, &Path),
) {
    for path in resolved_entry_paths(canon_home, entry) {
        push(out, &path);
    }
}

const MAX_SYMLINK_HOPS: usize = 16;

/// All distinct paths `<canon_home>/<entry>` resolves to across symlink hops at
/// each component (the final canonical target first when it exists, then the
/// lexical path, then any intermediate or dangling-symlink targets).
fn resolved_entry_paths(canon_home: &Path, entry: &str) -> Vec<PathBuf> {
    let lexical = canon_home.join(entry);
    let mut paths = Vec::new();
    let push_unique = |paths: &mut Vec<PathBuf>, candidate: PathBuf| {
        if !paths.iter().any(|p| p == &candidate) {
            paths.push(candidate);
        }
    };
    if let Ok(canonical) = std::fs::canonicalize(&lexical) {
        push_unique(&mut paths, canonical);
    }
    push_unique(&mut paths, lexical);

    let components: Vec<&std::ffi::OsStr> = Path::new(entry)
        .components()
        .filter_map(|c| match c {
            std::path::Component::Normal(part) => Some(part),
            _ => None,
        })
        .collect();
    let with_tail = |base: &Path, tail: &[&std::ffi::OsStr]| -> PathBuf {
        let mut out = base.to_path_buf();
        for part in tail {
            out.push(part);
        }
        out
    };
    let mut cur = canon_home.to_path_buf();
    let mut hops = 0;
    for (idx, part) in components.iter().enumerate() {
        cur.push(part);
        let rem = &components[idx + 1..];
        push_unique(&mut paths, with_tail(&cur, rem));
        cur = resolve_symlink_node(cur, &mut hops);
        push_unique(&mut paths, with_tail(&cur, rem));
    }
    paths
}

/// Resolve `node` through `canonicalize` when its target exists, or by stepping
/// through `read_link` (bounded by `hops < MAX_SYMLINK_HOPS`) when the leaf or an
/// intermediate symlink target does not exist yet.
fn resolve_symlink_node(mut node: PathBuf, hops: &mut usize) -> PathBuf {
    if let Ok(canon) = std::fs::canonicalize(&node) {
        return canon;
    }
    while *hops < MAX_SYMLINK_HOPS {
        let Ok(target) = std::fs::read_link(&node) else {
            break;
        };
        *hops += 1;
        let base = node.parent().unwrap_or_else(|| Path::new("/"));
        node = step_symlink_target(base, &target, hops);
        if let Ok(canon) = std::fs::canonicalize(&node) {
            return canon;
        }
    }
    node
}

fn step_symlink_target(base: &Path, target: &Path, hops: &mut usize) -> PathBuf {
    let mut cur = if target.is_absolute() {
        PathBuf::from("/")
    } else {
        base.to_path_buf()
    };
    for comp in target.components() {
        match comp {
            std::path::Component::RootDir => cur = PathBuf::from("/"),
            std::path::Component::CurDir | std::path::Component::Prefix(_) => {}
            std::path::Component::ParentDir => {
                cur = resolve_symlink_node(cur, hops);
                cur.pop();
            }
            std::path::Component::Normal(part) => {
                cur.push(part);
                cur = resolve_symlink_node(cur, hops);
            }
        }
    }
    cur
}

/// Append ` (literal "<node>")` write-deny operands for the existing ancestor
/// nodes of `entry` whose rename would relocate the protected leaf, deduped
/// against `denied` (see [`profile_for_home`]).
fn push_existing_ancestor_write_denies(
    out: &mut String,
    canon_home: &Path,
    entry: &str,
    denied: &mut Vec<PathBuf>,
) {
    for path in resolved_entry_paths(canon_home, entry) {
        push_ancestor_literals(out, canon_home, &path, denied);
    }
}

/// Write-deny each existing directory node strictly between `canon_home` and
/// `path`'s leaf, shallow-first, deduped against `denied`.
fn push_ancestor_literals(
    out: &mut String,
    canon_home: &Path,
    path: &Path,
    denied: &mut Vec<PathBuf>,
) {
    let mut nodes: Vec<&Path> = path
        .ancestors()
        .skip(1)
        .take_while(|a| a.starts_with(canon_home) && *a != canon_home)
        .collect();
    nodes.reverse();
    for node in nodes {
        let node = node.to_path_buf();
        if denied.iter().any(|d| d == &node) {
            continue;
        }
        if std::fs::symlink_metadata(&node).is_ok() {
            push_literal_clause(out, &node);
            denied.push(node);
        }
    }
}

/// Append ` (subpath "<escaped path>")` to `out`.
fn push_subpath_clause(out: &mut String, path: &Path) {
    out.push_str(" (subpath \"");
    push_sbpl_escaped(out, path);
    out.push_str("\")");
}

/// Append ` (literal "<escaped path>")` to `out`.
fn push_literal_clause(out: &mut String, path: &Path) {
    out.push_str(" (literal \"");
    push_sbpl_escaped(out, path);
    out.push_str("\")");
}

/// Append ` (regex "^<dir>/\.?<name>\.")` for the transient siblings of the file at `path`:
/// a writer's temp (`aterm_uds::atomic_file`'s `.<name>.tmp.<pid>.<hex>`, atpkg-keys'
/// `<name>.atpkg-keys.<pid>.<seq>.tmp`) or lock (git's `<name>.lock`), each holding the
/// file's whole value while it is written. Seatbelt judges a path only at open, so a process
/// that opened one reads the value through it once it is renamed onto the denied name.
fn push_siblings_clause(out: &mut String, path: &Path) {
    let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else {
        return;
    };
    let mut pattern = String::from("^");
    push_regex_escaped(&mut pattern, &dir.to_string_lossy());
    pattern.push_str("/\\.?");
    push_regex_escaped(&mut pattern, &name.to_string_lossy());
    pattern.push_str("\\.");
    out.push_str(" (regex \"");
    push_sbpl_escaped_str(out, &pattern);
    out.push_str("\")");
}

/// Append `text` to `out` with each regular-expression metacharacter backslash-escaped, so
/// it matches only itself.
fn push_regex_escaped(out: &mut String, text: &str) {
    for ch in text.chars() {
        if matches!(
            ch,
            '\\' | '.' | '^' | '$' | '|' | '?' | '*' | '+' | '(' | ')' | '[' | ']' | '{' | '}'
        ) {
            out.push('\\');
        }
        out.push(ch);
    }
}

/// Append `path` to `out` as the inside of an SBPL string literal, backslash-
/// escaping `\` and `"` so the emitted `"…"` is always well-formed regardless of
/// the home path.
fn push_sbpl_escaped(out: &mut String, path: &Path) {
    push_sbpl_escaped_str(out, &path.to_string_lossy());
}

/// [`push_sbpl_escaped`] for text.
fn push_sbpl_escaped_str(out: &mut String, text: &str) {
    for ch in text.chars() {
        match ch {
            '\\' | '"' => {
                out.push('\\');
                out.push(ch);
            }
            other => out.push(other),
        }
    }
}

/// Build the fail-closed `sandbox-exec -p <sbpl> <prog> <orig_argv[1..]…>` target and argv.
#[cfg(unix)]
pub fn build_sandbox_wrap(
    wrapper_path: &str,
    sbpl: &str,
    prog: &std::ffi::CString,
    orig_argv: &[std::ffi::CString],
) -> std::io::Result<(std::ffi::CString, Vec<std::ffi::CString>)> {
    let exec_bin = std::ffi::CString::new(wrapper_path.as_bytes())
        .map_err(|_| std::io::Error::other("sandbox-exec path not representable"))?;
    // SAFETY: `exec_bin` is a valid NUL-terminated absolute path.
    let present = unsafe { libc::access(exec_bin.as_ptr(), libc::X_OK) } == 0;
    if !present {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!(
                "OS sandbox demanded but {wrapper_path} is missing/not executable — refusing \
                 to spawn an unsandboxed shell (fail-closed, ATERM_DESIGN §5.6)"
            ),
        ));
    }
    let sbpl_c = std::ffi::CString::new(sbpl.as_bytes()).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "SBPL profile has interior NUL",
        )
    })?;
    let mut argv_out: Vec<std::ffi::CString> =
        Vec::with_capacity(orig_argv.len().saturating_add(3).min(4096));
    argv_out.push(std::ffi::CString::new("sandbox-exec").unwrap_or_else(|_| exec_bin.clone()));
    argv_out.push(std::ffi::CString::from(c"-p"));
    argv_out.push(sbpl_c);
    argv_out.push(prog.clone());
    argv_out.extend(orig_argv.iter().skip(1).cloned());
    Ok((exec_bin, argv_out))
}

#[cfg(test)]
#[path = "sbpl_tests.rs"]
mod tests;
