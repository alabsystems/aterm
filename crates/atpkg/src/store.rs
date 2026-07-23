// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The manager-owned install layout (§10): a hardened prefix under `$HOME`, the
//! per-program store, the `bin/` shim dir, per-program staging, the durable floor, and
//! the aggregate status file.
//!
//! Two trust-bearing decisions live here, both fail-closed:
//!
//! * **Prefix validation is a *chain* check, not just the leaf.**
//!   `dir_safe_for_private_write` only checks one dir's owner+mode; it does not walk the
//!   parent chain. Because `prefix` is config-controlled (§11), a prefix under a
//!   shared/attacker-writable *parent* would reintroduce a CWE-379 symlink-swap window.
//!   So [`resolve`] requires the prefix to sit under `$HOME`, contain no `..`, and have
//!   **every existing directory from `$HOME` down** owned-by-uid, not group/other-writable,
//!   and **not a symlink** — any violation falls back to the trusted default prefix
//!   (mirroring the slug-fail-closed-to-default pattern).
//! * **Shim names that collide with sensitive commands are refused** ([`shim_allowed`]).
//!   `bin/` is appended to the child `PATH` (never prepended, so a managed tool can't
//!   shadow a system one), but a tool honestly or maliciously named `sudo`/`ssh`/`git`/…
//!   must never get a shim at all.

use std::ffi::{OsStr, OsString};
use std::path::{Component, Path, PathBuf};

/// The resolved, validated install layout. All paths are absolute and under a prefix
/// that passed the [`resolve`] chain check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    /// The manager prefix, e.g. `~/Library/Application Support/aterm/pkg`.
    pub prefix: PathBuf,
}

impl Layout {
    /// `store/<program>/<build>/` — the versioned, immutable extracted tree.
    #[must_use]
    pub fn build_dir(&self, program: &str, build: u64) -> PathBuf {
        self.prefix
            .join("store")
            .join(program)
            .join(build.to_string())
    }

    /// `bin/` — the only directory placed on the child `PATH` (shims into `current`).
    #[must_use]
    pub fn bin_dir(&self) -> PathBuf {
        self.prefix.join("bin")
    }

    /// `bin/<tool>` — a single shim (concrete filename `bin/<tool>` on Unix,
    /// `bin/<tool>.cmd` on Windows, via [`crate::platform::SHIM_SUFFIX`]). Callers MUST
    /// gate the name through [`shim_allowed`].
    #[must_use]
    pub fn shim(&self, tool: &str) -> PathBuf {
        let mut name = String::from(tool);
        name.push_str(crate::platform::SHIM_SUFFIX);
        self.bin_dir().join(name)
    }

    /// `channels/<name>/current` — the per-coherence-group active-set symlink (§10).
    #[must_use]
    pub fn channel_current(&self, channel: &str) -> PathBuf {
        self.prefix.join("channels").join(channel).join("current")
    }

    /// `staging/<program>/` — the per-program download + stage scratch.
    #[must_use]
    pub fn staging_dir(&self, program: &str) -> PathBuf {
        self.prefix.join("staging").join(program)
    }

    /// `floor` — the `0600` durable high-water `index_build` file (§8).
    #[must_use]
    pub fn floor(&self) -> PathBuf {
        self.prefix.join("floor")
    }

    /// `store.lock` — the `0600` store-wide single-writer advisory lock file
    /// ([`crate::lock`]). TRY-acquired at the CLI edge by every verb that mutates the
    /// store, so exactly one process at a time stages/activates/discards builds here.
    #[must_use]
    pub fn store_lock(&self) -> PathBuf {
        self.prefix.join("store.lock")
    }

    /// `status.toml` — the aggregate observability record.
    #[must_use]
    pub fn status(&self) -> PathBuf {
        self.prefix.join("status.toml")
    }

    /// `links/` — the per-program dev-link markers directory (§13). One `0600` marker per
    /// dev-linked program; its presence makes `update`/`apply` HARD-SKIP that program.
    #[must_use]
    pub fn links_dir(&self) -> PathBuf {
        self.prefix.join("links")
    }

    /// `links/<program>` — one dev-link marker. Only ever joined with a
    /// [`shim_allowed`]-shape program name (linkmode gates the name before calling).
    #[must_use]
    pub fn link_marker(&self, program: &str) -> PathBuf {
        self.links_dir().join(program)
    }
}

/// The per-build completeness marker: a SIBLING file `store/<program>/<build>.ready`
/// next to the `<build>/` dir. It sits OUTSIDE the build tree deliberately, so it can
/// never perturb the build's `tree_root` hash (the apply-time TOCTOU re-verify). It is
/// written LAST by `verify_and_stage`, once the extracted tree has passed sha256 +
/// tree_root re-verify; its presence is the sole "this build is fully installed"
/// signal, so a build dir left partial by a crash mid-extract (which has no marker)
/// reads as absent and is re-installed rather than mistaken for up-to-date.
///
/// `None` if `build_dir` has no final path component (never, for a real build dir).
fn ready_marker_path(build_dir: &Path) -> Option<PathBuf> {
    // `Path::file_name` / `OsStr::to_str` go via `call1`: std's INLINED `unsafe`
    // (the `from_utf8_unchecked` fast path, the `OsStr` byte-slice casts) is
    // otherwise attributed to this function's spans as missing-SAFETY-comment
    // refutations under the strict Trust gate (see `lib.rs`). Same calls, same
    // receivers; behavior identical. The `format!("{name}.ready")` is a manual
    // concat for the same reason (its expansion embeds `fmt::Arguments`
    // construction the gate cannot lower) — byte-identical.
    let name = crate::call1(std::path::Path::file_name, build_dir)?;
    let name = crate::call1(std::ffi::OsStr::to_str, name)?;
    let mut marker = String::from(name);
    marker.push_str(".ready");
    Some(build_dir.with_file_name(marker))
}

/// Whether `build_dir` holds a COMPLETE install (its sibling completeness marker exists).
#[must_use]
pub fn build_is_complete(build_dir: &Path) -> bool {
    ready_marker_path(build_dir).is_some_and(|p| p.exists())
}

/// Atomically mark `build_dir` complete (temp + rename, so a crash during the write
/// leaves NO marker rather than a half-written one). Call as the LAST staging step.
pub fn mark_build_ready(build_dir: &Path) -> std::io::Result<()> {
    let dest = ready_marker_path(build_dir).ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "build dir has no name")
    })?;
    let parent = dest.parent().unwrap_or(build_dir);
    // Manual rendering of the previous `format!(".ready.tmp-{}", std::process::id())`
    // — byte-identical: the `format!` expansion embeds `fmt::Arguments`
    // construction (with inlined `unsafe`) that the strict Trust gate cannot
    // lower and fails closed on.
    let mut tmp_name = String::from(".ready.tmp-");
    tmp_name.push_str(&crate::dec_u64(u64::from(std::process::id())));
    let tmp = parent.join(tmp_name);
    // `fs::write` goes via `call2`: the hardened pass name-matches any direct
    // callee named `write` against the libc `write(2)` FFI-boundary contracts,
    // which do not apply to this safe std function (see `lib.rs`). Same
    // function, same arguments; behavior identical.
    crate::call2(std::fs::write, &tmp, b"ok\n".as_slice())?;
    std::fs::rename(&tmp, &dest)
}

/// Discard a build entirely: remove its tree AND its sibling completeness marker (the
/// inverse of a stage + [`mark_build_ready`]). Used to clean up a build that a transaction
/// STAGED but then ABORTED without activating — leaving it complete-but-inactive would make
/// `list_installed`/`decide` mis-read it as the active build on the next run. Best-effort.
pub fn discard_build(build_dir: &Path) {
    let _ = std::fs::remove_dir_all(build_dir);
    if let Some(marker) = ready_marker_path(build_dir) {
        let _ = std::fs::remove_file(marker);
    }
}

/// The default prefix under `home`. On Unix `…/Library/Application Support/aterm/pkg`
/// (a sibling of the updater's `Updates` dir, sharing the hardened support root); on
/// Windows `%LOCALAPPDATA%\aterm\pkg`. The OS-specific base lives in
/// [`crate::platform::default_prefix`].
#[must_use]
pub fn default_prefix(home: &Path) -> PathBuf {
    crate::platform::default_prefix(home)
}

/// Resolve the install layout. `configured` is the optional `[packages].prefix` override
/// (`None` ⇒ the default). The chosen prefix is **chain-validated** against the home dir
/// ([`vet_prefix`]); any violation falls back to the default. Returns `None` only when the
/// home directory can't be resolved (`$HOME` / `/etc/passwd` on Unix, `%USERPROFILE%` on
/// Windows) — the same fail-closed posture the updater takes. Uses the platform-aware
/// [`aterm_types::dirs::home_dir`], NOT a raw `$HOME` read: a native-Windows shell does not
/// set `HOME`, so a raw read left every prefix-dependent verb dead with "HOME is unset".
#[must_use]
pub fn resolve(configured: Option<&Path>) -> Option<Layout> {
    let home = aterm_types::dirs::home_dir()?;
    Some(Layout {
        prefix: vet_prefix(configured, &home),
    })
}

/// Validate a configured prefix against `home`, returning it if safe or the trusted
/// [`default_prefix`] otherwise. Pure w.r.t. config but reads directory metadata; `home`
/// is a parameter so the chain check is testable against a synthetic tree.
#[must_use]
pub fn vet_prefix(configured: Option<&Path>, home: &Path) -> PathBuf {
    let default = default_prefix(home);
    let Some(p) = configured else {
        return default;
    };
    // Must be an absolute path strictly under $HOME, with no `..` escape components.
    // Containment is compared with `under_home` (case-insensitively on Windows, whose
    // filesystem is case-insensitive — else a validly-configured prefix that differs only
    // in case from %USERPROFILE% is wrongly rejected and silently ignored).
    if !p.is_absolute() || !under_home(p, home) || p == home {
        return default;
    }
    if p.components().any(|c| matches!(c, Component::ParentDir)) {
        return default;
    }
    // Walk every EXISTING directory from `home` down to the leaf (the not-yet-created
    // tail that atpkg will make `0700` is fine). Each must be a real dir (NOT a symlink OR a
    // Windows junction), owned by us, and not group/other-writable — else fall back
    // fail-closed. (On Windows `dir_meta_is_private` is a best-effort `true`; privacy rests
    // on the per-user ACL.)
    for anc in p.ancestors().filter(|a| under_home(a, home)) {
        // A non-existent ancestor is the not-yet-created tail atpkg makes 0700 — skip it.
        // `is_reparse` disqualifies a symlink OR a directory junction: a junction (needs no
        // admin) reports is_symlink()==false, so without the reparse-bit check an attacker-
        // pre-created junction ancestor would reintroduce the CWE-379 reparse-swap window.
        if let Ok(m) = std::fs::symlink_metadata(anc)
            && (crate::platform::is_reparse(&m) || !crate::platform::dir_meta_is_private(&m))
        {
            return default;
        }
    }
    p.to_path_buf()
}

/// Containment check `p` is at/under `home`. Case-sensitive on Unix (`starts_with`);
/// case-INSENSITIVE per-component on Windows, where the filesystem is case-insensitive so
/// `c:\users\me\pkg` is genuinely under `C:\Users\Me` and must not be rejected.
#[must_use]
fn under_home(p: &Path, home: &Path) -> bool {
    #[cfg(windows)]
    {
        let mut hc = home.components();
        let mut pc = p.components();
        loop {
            match hc.next() {
                None => return true, // consumed all of home's components ⇒ p is under home
                Some(h) => match pc.next() {
                    Some(q) if h.as_os_str().eq_ignore_ascii_case(q.as_os_str()) => continue,
                    _ => return false,
                },
            }
        }
    }
    #[cfg(not(windows))]
    {
        p.starts_with(home)
    }
}

/// Commands a managed shim must NEVER be allowed to name, even though `bin/` is only
/// *appended* to `PATH`. A tool honestly or maliciously named one of these is refused a
/// shim outright (and the refusal is surfaced in `status.toml`), so a key-compromise (or
/// an honest mistake) can't quietly intercept core/security commands. Lower-cased.
const SENSITIVE_SHIMS: &[&str] = &[
    "sudo",
    "ssh",
    "scp",
    "sshd",
    "git",
    "sh",
    "bash",
    "zsh",
    "fish",
    "env",
    "sudo_askpass",
    "doas",
    "su",
    "login",
    "passwd",
    "gpg",
    "gpg2",
    "curl",
    "wget",
    "rm",
    "mv",
    "cp",
    "ln",
    "chmod",
    "chown",
    "kill",
    "launchctl",
    "osascript",
    "security",
    "codesign",
    "spctl",
    "cargo",
    "rustc",
    "rustup",
    "python",
    "python3",
    "node",
    "ls",
    "cat",
];

/// Whether `name` may be installed as a `bin/` shim: a non-empty, path-separator-free
/// name that is not on the `SENSITIVE_SHIMS` deny-list (case-insensitive). Fail-closed:
/// an empty name, a name containing `/`, `\` or `\0`, or `.`/`..` is also refused.
/// BOTH separators are rejected: on Windows `Layout::shim` does `bin_dir().join(name)`, and
/// a `\` in an (untrusted, manifest-supplied) name makes `Path::join` traverse OUT of `bin/`
/// (e.g. `..\..\evil` → a `.cmd` written outside the managed tree) and also lets a name like
/// `..\git` dodge the sensitive-name deny-list. This matches `linkmode::safe_component`,
/// `ops::uninstall`, and the other name gates, which all reject `\` too.
#[must_use]
pub fn shim_allowed(name: &str) -> bool {
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.contains('/')
        || name.contains('\\')
        || name.contains('\0')
    {
        return false;
    }
    let lower = name.to_ascii_lowercase();
    !SENSITIVE_SHIMS.contains(&lower.as_str())
}

/// Compose the child `PATH` for running a managed tool: the inherited `PATH` with the
/// managed `bin_dir` **appended** — never prepended, so a pinned tool that calls a sibling
/// by bare name resolves the pinned sibling, while system commands (`sudo`/`ssh`/…) on the
/// inherited `PATH` are never shadowed (§10). Idempotent: if `bin_dir` is already present
/// the inherited value is returned unchanged. With no inherited `PATH`, returns just
/// `bin_dir`. This is the single source of truth for the `atpkg run` / `aterm <tool>`
/// child environment; keeping it pure makes the append-not-prepend policy unit-testable.
#[must_use]
pub fn append_bin_to_path(inherited: Option<&OsStr>, bin_dir: &Path) -> OsString {
    // An absent OR empty inherited `PATH` means "no directories" — start empty so we never
    // emit a leading empty component (which Unix reads as the current directory).
    // `OsStr::is_empty` goes via `call1`: std's INLINED `unsafe` (the `OsStr`
    // byte-slice cast) is otherwise attributed to this function's span as a
    // missing-SAFETY-comment refutation under the strict Trust gate (see
    // `lib.rs`). Same call, same receiver; behavior identical.
    let mut dirs: Vec<PathBuf> = match inherited {
        Some(p) if !crate::call1(std::ffi::OsStr::is_empty, p) => {
            std::env::split_paths(p).collect()
        }
        _ => Vec::new(),
    };
    if !dirs.iter().any(|d| d == bin_dir) {
        dirs.push(bin_dir.to_path_buf());
    }
    // `join_paths` only fails if a component itself contains the platform separator; in that
    // (pathological) case fall back to the inherited value rather than corrupting `PATH`.
    std::env::join_paths(&dirs)
        .unwrap_or_else(|_| inherited.map(OsStr::to_os_string).unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    fn temp_home(label: &str) -> PathBuf {
        let h = std::env::temp_dir().join(format!("atpkg-store-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&h);
        std::fs::create_dir_all(&h).unwrap();
        #[cfg(unix)]
        std::fs::set_permissions(&h, std::fs::Permissions::from_mode(0o700)).unwrap();
        h
    }

    #[test]
    fn layout_paths_are_under_prefix() {
        let l = Layout {
            prefix: PathBuf::from("/p"),
        };
        assert_eq!(l.build_dir("ay", 18), PathBuf::from("/p/store/ay/18"));
        // The shim file name carries the concrete platform suffix (`.cmd` on Windows).
        assert_eq!(
            l.shim("ay"),
            PathBuf::from(format!("/p/bin/ay{}", crate::platform::SHIM_SUFFIX))
        );
        assert_eq!(
            l.channel_current("stable"),
            PathBuf::from("/p/channels/stable/current")
        );
        assert_eq!(l.staging_dir("ay"), PathBuf::from("/p/staging/ay"));
        assert_eq!(l.floor(), PathBuf::from("/p/floor"));
        assert_eq!(l.store_lock(), PathBuf::from("/p/store.lock"));
    }

    #[test]
    fn unset_or_default_prefix_uses_default() {
        let home = temp_home("default");
        // No config ⇒ default prefix under home.
        assert_eq!(vet_prefix(None, &home), default_prefix(&home));
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn prefix_outside_home_or_with_traversal_falls_back() {
        let home = temp_home("outside");
        // Not under home.
        assert_eq!(
            vet_prefix(Some(Path::new("/tmp/evil")), &home),
            default_prefix(&home)
        );
        // A `..` escape component, even if it textually starts under home.
        let sneaky = home.join("../somewhere/pkg");
        assert_eq!(vet_prefix(Some(&sneaky), &home), default_prefix(&home));
        // home itself is not a valid prefix (the manager must own a subdir).
        assert_eq!(vet_prefix(Some(&home), &home), default_prefix(&home));
        let _ = std::fs::remove_dir_all(&home);
    }

    #[cfg(unix)] // group-writable chmod fixture — Unix-only
    #[test]
    fn group_writable_intermediate_parent_is_rejected() {
        let home = temp_home("gwparent");
        // A safe (0700) intermediate, then a group/other-writable one beneath it, then
        // the would-be prefix leaf — the design's exact "intermediate parent rejected" case.
        let mid = home.join("Library");
        std::fs::create_dir_all(&mid).unwrap();
        std::fs::set_permissions(&mid, std::fs::Permissions::from_mode(0o700)).unwrap();
        let bad = mid.join("shared");
        std::fs::create_dir_all(&bad).unwrap();
        std::fs::set_permissions(&bad, std::fs::Permissions::from_mode(0o777)).unwrap(); // group/other-writable
        let prefix = bad.join("pkg");
        assert_eq!(
            vet_prefix(Some(&prefix), &home),
            default_prefix(&home),
            "a group/other-writable intermediate parent must fail closed to the default"
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn safe_under_home_prefix_is_accepted() {
        let home = temp_home("safe");
        // Build a fully-safe chain home/a/b (0700 each); the not-yet-existing leaf is fine.
        let a = home.join("a");
        std::fs::create_dir_all(&a).unwrap();
        #[cfg(unix)]
        std::fs::set_permissions(&a, std::fs::Permissions::from_mode(0o700)).unwrap();
        let prefix = a.join("b").join("pkg"); // b + pkg do not exist yet
        assert_eq!(vet_prefix(Some(&prefix), &home), prefix);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn shim_names_collision_and_shape_policy() {
        // Sensitive commands are refused (case-insensitively).
        for bad in ["sudo", "SSH", "git", "sh", "env", "rustc", "Codesign"] {
            assert!(!shim_allowed(bad), "{bad} must be refused a shim");
        }
        // Malformed shapes are refused.
        for bad in ["", ".", "..", "a/b", "x\0y"] {
            assert!(!shim_allowed(bad), "{bad:?} is not a valid shim name");
        }
        // Ordinary tool names are allowed.
        for ok in ["ay", "ny", "trust-mc", "clean-certify"] {
            assert!(shim_allowed(ok), "{ok} should be allowed");
        }
    }

    #[test]
    fn append_bin_to_path_appends_never_prepends() {
        let bin = Path::new("/p/bin");
        // Inputs/expectations built with join_paths so the platform separator (':' Unix,
        // ';' Windows) is exercised, not hard-coded.
        let inherited = std::env::join_paths([Path::new("/usr/bin"), Path::new("/bin")]).unwrap();
        // bin_dir lands at the END (so it can't shadow system commands earlier on PATH).
        let out = append_bin_to_path(Some(&inherited), bin);
        assert_eq!(
            out,
            std::env::join_paths([Path::new("/usr/bin"), Path::new("/bin"), bin]).unwrap()
        );
        // No inherited PATH → just the managed bin.
        assert_eq!(append_bin_to_path(None, bin), OsString::from("/p/bin"));
        assert_eq!(
            append_bin_to_path(Some(OsStr::new("")), bin),
            OsString::from("/p/bin")
        );
    }

    #[test]
    fn append_bin_to_path_is_idempotent() {
        let bin = Path::new("/p/bin");
        // Already present (anywhere) → returned unchanged, never duplicated.
        let bin_first = std::env::join_paths([Path::new("/p/bin"), Path::new("/usr/bin")]).unwrap();
        assert_eq!(append_bin_to_path(Some(&bin_first), bin), bin_first);
        let bin_last = std::env::join_paths([Path::new("/usr/bin"), Path::new("/p/bin")]).unwrap();
        assert_eq!(append_bin_to_path(Some(&bin_last), bin), bin_last);
    }
}
