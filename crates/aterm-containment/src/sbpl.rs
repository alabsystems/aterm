// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! macOS Seatbelt (SBPL) sandbox profile generation for the containment actuator.

use crate::capability::{FsCapability, NetworkCapability};
use crate::policy::Capabilities;
use std::path::{Path, PathBuf};

/// Fixed absolute path to `/usr/bin/sandbox-exec`.
pub const SANDBOX_EXEC_PATH: &str = "/usr/bin/sandbox-exec";

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
/// shape macOS gives it: `<USER_TEMP_PARENT>/<xx>/<id>/T`.
fn admitted_user_temp(temp: &Path) -> Option<PathBuf> {
    let canon = std::fs::canonicalize(temp).ok()?;
    let bucket = canon.parent()?.parent()?;
    (canon.file_name()? == "T" && bucket.parent()? == Path::new(USER_TEMP_PARENT)).then_some(canon)
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
/// history file is absent or a plain regular file with one link, every name of
/// the row as ` (require-all (literal "<home>/<name>") (vnode-type <type>))`.
fn push_history_allows(out: &mut String, canon_home: &Path) {
    for row in HISTORY_FILES {
        let Some((file, _)) = row.first() else {
            continue;
        };
        if !plain_or_absent(&canon_home.join(file)) {
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

/// Whether `path` is absent, or a regular file (not followed through a symlink)
/// with exactly one link.
fn plain_or_absent(path: &Path) -> bool {
    match std::fs::symlink_metadata(path) {
        Err(e) => e.kind() == std::io::ErrorKind::NotFound,
        Ok(meta) => meta.file_type().is_file() && single_link(&meta),
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
];

/// Credential files under `$HOME` denied as `literal` paths for read and write.
pub const SECRET_LITERAL_FILES: &[&str] = &[".netrc", ".git-credentials", ".npmrc", ".pypirc"];

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
/// resolving symlinks as Seatbelt does.
fn push_resolved_clause(
    out: &mut String,
    canon_home: &Path,
    entry: &str,
    push: fn(&mut String, &Path),
) {
    let lexical = canon_home.join(entry);
    if let Ok(canonical) = std::fs::canonicalize(&lexical) {
        push(out, &canonical);
        if canonical != lexical {
            push(out, &lexical);
        }
    } else {
        push(out, &lexical);
        if let Some(resolved) = resolve_existing_prefix(&lexical)
            && resolved != lexical
        {
            push(out, &resolved);
        }
    }
}

/// Resolve symlinks in the deepest existing ancestor of `path` and re-append
/// the missing trailing components, for a path whose leaf does not exist yet.
fn resolve_existing_prefix(path: &Path) -> Option<PathBuf> {
    for ancestor in path.ancestors().skip(1) {
        if let Ok(canon_ancestor) = std::fs::canonicalize(ancestor) {
            let tail = path.strip_prefix(ancestor).ok()?;
            return Some(canon_ancestor.join(tail));
        }
    }
    None
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
    let lexical = canon_home.join(entry);
    let resolved = match std::fs::canonicalize(&lexical) {
        Ok(c) => Some(c),
        Err(_) => resolve_existing_prefix(&lexical),
    };
    push_ancestor_literals(out, canon_home, &lexical, denied);
    if let Some(resolved) = resolved
        && resolved != lexical
    {
        push_ancestor_literals(out, canon_home, &resolved, denied);
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

/// Append `path` to `out` as the inside of an SBPL string literal, backslash-
/// escaping `\` and `"` so the emitted `"…"` is always well-formed regardless of
/// the home path.
fn push_sbpl_escaped(out: &mut String, path: &Path) {
    for ch in path.to_string_lossy().chars() {
        match ch {
            '\\' | '"' => {
                out.push('\\');
                out.push(ch);
            }
            other => out.push(other),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability::NetworkCapability;
    use crate::mode::ContainmentMode;
    use crate::policy::ContainmentPolicy;

    /// Expected-marker helper: render `path` exactly as the emitter's SBPL
    /// string escaping does ([`push_sbpl_escaped`]).
    fn sbpl_str(path: &Path) -> String {
        let mut s = String::new();
        push_sbpl_escaped(&mut s, path);
        s
    }

    /// The TmpOnly write confinement with no home-scoped allow in it.
    fn homeless_write_confinement() -> String {
        let mut s = String::new();
        push_write_confinement(&mut s, None, None);
        s
    }

    #[test]
    fn home_unset_yields_no_home_scoped_rule() {
        let tmp_only = [BASE_PROFILE, &homeless_write_confinement()].concat();
        for home in [None, Some("")] {
            assert_eq!(
                profile_for_home(home, None, FsCapability::HomeReadWrite),
                BASE_PROFILE
            );
            assert_eq!(
                profile_for_home(home, None, FsCapability::TmpOnly),
                tmp_only
            );
            assert!(!profile_scoped(home, None, FsCapability::TmpOnly).1);
        }
        let home = aterm_tempfile::tempdir().unwrap();
        assert!(profile_scoped(home.path().to_str(), None, FsCapability::TmpOnly).1);
    }

    /// `TmpOnly` confines writes: every write denied, the temp roots, `/dev`
    /// and the shell's history files allowed back, and nothing of `$HOME`
    /// beyond those files.
    #[test]
    fn tmp_only_confines_writes_to_the_writable_roots() {
        let home = aterm_tempfile::tempdir().unwrap();
        let canon_home = std::fs::canonicalize(home.path()).unwrap();
        let profile = profile_for_home(canon_home.to_str(), None, FsCapability::TmpOnly);

        let confinement = [
            BASE_PROFILE,
            PREFERENCE_WRITE_DENY,
            "(deny file-write*)(allow file-write*",
        ]
        .concat();
        assert!(
            profile.starts_with(&confinement),
            "TmpOnly must open with the write confinement; got {profile}"
        );
        for root in WRITABLE_ROOTS {
            let expect = format!("(subpath \"{root}\")");
            assert!(
                profile.contains(&expect),
                "missing writable root {expect}; got {profile}"
            );
            #[cfg(target_os = "macos")]
            assert_eq!(
                std::fs::canonicalize(root).ok().as_deref(),
                Some(Path::new(root)),
                "{root} must be written in its canonical form"
            );
        }
        for row in HISTORY_FILES {
            for (name, vnode_type) in *row {
                let expect = history_allow(&canon_home, name, vnode_type);
                assert!(
                    profile.contains(&expect),
                    "missing history allow {expect}; got {profile}"
                );
            }
        }
        assert!(
            !profile.contains("(prefix "),
            "no allow is an open prefix; got {profile}"
        );
        let allow_at = profile.find("(allow file-write*").unwrap();
        let secret_at = profile.find("(deny file-read* file-write*").unwrap();
        assert!(
            allow_at < secret_at,
            "secret deny must come after the write allows"
        );
        let allows = &profile[allow_at..secret_at];
        for forbidden in [&canon_home, &canon_home.join("Library")] {
            for shape in ["subpath", "prefix", "literal"] {
                let allow = format!("({shape} \"{}\")", sbpl_str(forbidden));
                assert!(
                    !allows.contains(&allow),
                    "{allow} must not be writable; got {profile}"
                );
            }
        }
    }

    /// The history allow for `<home>/<name>` exactly as the generator writes it.
    fn history_allow(canon_home: &Path, name: &str, vnode_type: &str) -> String {
        format!(
            "(require-all (literal \"{}\") (vnode-type {vnode_type}))",
            sbpl_str(&canon_home.join(name))
        )
    }

    /// The `(allow file-write* …)` clause of a `TmpOnly` profile.
    fn write_allows(profile: &str) -> &str {
        let at = profile
            .find("(allow file-write*")
            .expect("the write allows");
        let end = at + profile[at..].find(")(").expect("the allow clause ends");
        &profile[at..end]
    }

    /// A history file that is not a plain single-link file gets no allowance
    /// (a `~/.zsh_history` symlink to `$HOME` would otherwise allow all of
    /// `$HOME`): its row is dropped whole; the other shell's row is untouched.
    #[cfg(unix)]
    #[test]
    fn a_history_file_that_is_not_a_plain_file_gets_no_allowance() {
        let zsh_row = HISTORY_FILES[0];
        let bash_row = HISTORY_FILES[1];
        assert_eq!(zsh_row[0].0, ".zsh_history", "precondition");
        let plant: [(&str, fn(&Path, &Path)); 4] = [
            ("symlink to $HOME", |home, hist| {
                std::os::unix::fs::symlink(home, hist).unwrap();
            }),
            ("symlink to ~/.zshrc", |home, hist| {
                std::os::unix::fs::symlink(home.join(".zshrc"), hist).unwrap();
            }),
            ("directory", |_, hist| std::fs::create_dir(hist).unwrap()),
            ("hard link to ~/.zshrc", |home, hist| {
                std::fs::hard_link(home.join(".zshrc"), hist).unwrap();
            }),
        ];
        for (shape, plant) in plant {
            let home = aterm_tempfile::tempdir().unwrap();
            let canon_home = std::fs::canonicalize(home.path()).unwrap();
            std::fs::write(canon_home.join(".zshrc"), "echo original\n").unwrap();
            plant(&canon_home, &canon_home.join(".zsh_history"));

            let profile = profile_for_home(canon_home.to_str(), None, FsCapability::TmpOnly);
            let allows = write_allows(&profile);
            for (name, _) in zsh_row {
                let named = format!("\"{}\"", sbpl_str(&canon_home.join(name)));
                assert!(
                    !allows.contains(&named),
                    "{shape}: {name} must get no allowance; got {allows}"
                );
            }
            for shape_of_home in [&canon_home, &canon_home.join(".zshrc")] {
                let named = format!("\"{}\")", sbpl_str(shape_of_home));
                assert!(
                    !allows.contains(&named),
                    "{shape}: {} must not be writable; got {allows}",
                    shape_of_home.display()
                );
            }
            for (name, vnode_type) in bash_row {
                let expect = history_allow(&canon_home, name, vnode_type);
                assert!(
                    allows.contains(&expect),
                    "{shape}: {expect} must stay allowed; got {allows}"
                );
            }
        }
        let home = aterm_tempfile::tempdir().unwrap();
        let canon_home = std::fs::canonicalize(home.path()).unwrap();
        std::fs::write(canon_home.join(".zsh_history"), ": 0:0;ls\n").unwrap();
        let profile = profile_for_home(canon_home.to_str(), None, FsCapability::TmpOnly);
        let expect = history_allow(&canon_home, ".zsh_history", "REGULAR-FILE");
        assert!(
            write_allows(&profile).contains(&expect),
            "a plain history file keeps {expect}; got {profile}"
        );
        assert!(!plain_or_absent(&canon_home.join(".zsh_history").join("x")));
    }

    /// Only the user's own `$TMPDIR` (`/private/var/folders/<xx>/<id>/T`) is
    /// added to the write roots — never its siblings (`C`, which holds clang's
    /// module cache that later unsandboxed builds read; `0`), never the folders
    /// above it, never a `$TMPDIR` pointed somewhere else.
    #[test]
    fn only_the_users_own_tmpdir_joins_the_write_roots() {
        let home = aterm_tempfile::tempdir().unwrap();
        let canon_home = std::fs::canonicalize(home.path()).unwrap();
        let lookalike = canon_home.join("a/b/T");
        std::fs::create_dir_all(&lookalike).unwrap();
        for elsewhere in [
            canon_home.as_path(),
            Path::new(USER_TEMP_PARENT),
            Path::new("/"),
            lookalike.as_path(),
        ] {
            assert_eq!(
                admitted_user_temp(elsewhere),
                None,
                "{} is not a user's $TMPDIR",
                elsewhere.display()
            );
        }
        let profile = profile_for_home(
            canon_home.to_str(),
            Some(&canon_home),
            FsCapability::TmpOnly,
        );
        let home_allow = format!("(subpath \"{}\")", sbpl_str(&canon_home));
        assert!(
            !write_allows(&profile).contains(&home_allow),
            "a $TMPDIR at $HOME must not make $HOME writable; got {profile}"
        );

        #[cfg(target_os = "macos")]
        {
            let Some(temp) = std::fs::canonicalize(std::env::temp_dir())
                .ok()
                .filter(|t| admitted_user_temp(t).is_some())
            else {
                aterm_spec::host::could_not_run(
                    "$TMPDIR is not a per-user /private/var/folders/…/T here",
                );
            };
            let per_user = temp.parent().unwrap();
            for sibling in ["C", "0"] {
                let sibling = per_user.join(sibling);
                if sibling.exists() {
                    assert_eq!(admitted_user_temp(&sibling), None, "{}", sibling.display());
                }
            }
            assert_eq!(admitted_user_temp(per_user), None, "{}", per_user.display());
            let profile = profile_for_home(canon_home.to_str(), Some(&temp), FsCapability::TmpOnly);
            let allows = write_allows(&profile);
            let expect = format!("(subpath \"{}\")", sbpl_str(&temp));
            assert!(allows.contains(&expect), "missing {expect}; got {allows}");
            let broad = format!("(subpath \"{USER_TEMP_PARENT}\")");
            assert!(
                !allows.contains(&broad),
                "{broad} is too broad; got {allows}"
            );
        }
    }

    #[test]
    fn broader_fs_tiers_get_no_write_confinement() {
        let home = aterm_tempfile::tempdir().unwrap();
        let canon_home = std::fs::canonicalize(home.path()).unwrap();
        std::fs::create_dir_all(canon_home.join("Library")).unwrap();
        let library = format!("(literal \"{}\")", sbpl_str(&canon_home.join("Library")));
        for fs in [FsCapability::ProjectReadWrite, FsCapability::HomeReadWrite] {
            let profile = profile_for_home(canon_home.to_str(), None, fs);
            assert!(
                !profile.contains("(allow file-write*") && !profile.contains("(deny file-write*)"),
                "{fs:?} must not be write-confined; got {profile}"
            );
            assert!(
                !profile.contains(&library),
                "{fs:?} guards no private store; got {profile}"
            );
            assert!(
                profile.contains("(deny file-read* file-write*"),
                "the credential deny is emitted; got {profile}"
            );
        }
    }

    #[test]
    fn full_profile_starts_with_the_base_profile_and_denies_each_secret() {
        let tmp = std::env::temp_dir();
        let canon = std::fs::canonicalize(&tmp).expect("canonicalize temp dir");
        let profile = profile_for_home(canon.to_str(), None, FsCapability::HomeReadWrite);

        assert!(
            profile.starts_with(BASE_PROFILE),
            "full profile must begin with the exact base profile; got {profile}"
        );
        assert!(
            profile.contains("(deny file-read* file-write*"),
            "full profile must deny read AND write of the secret set; got {profile}"
        );

        for sub in SECRET_SUBDIRS {
            let expect = format!("(subpath \"{}\")", sbpl_str(&canon.join(sub)));
            assert!(
                profile.contains(&expect),
                "profile must deny secret subdir {sub} as canonical subpath {expect}; got {profile}"
            );
        }
        for file in SECRET_LITERAL_FILES {
            let expect = format!("(literal \"{}\")", sbpl_str(&canon.join(file)));
            assert!(
                profile.contains(&expect),
                "profile must deny secret file {file} as canonical literal {expect}; got {profile}"
            );
        }
        for sub in PRIVATE_SUBDIRS {
            let leaked = format!("(subpath \"{}\")", sbpl_str(&canon.join(sub)));
            assert!(
                !profile.contains(&leaked),
                "credential-only profile must NOT contain private-data path {sub}; got {profile}"
            );
        }
    }

    #[test]
    fn private_data_set_present_only_when_included() {
        let tmp = std::env::temp_dir();
        let canon = std::fs::canonicalize(&tmp).expect("canonicalize temp dir");
        let off = profile_for_home(canon.to_str(), None, FsCapability::HomeReadWrite);
        let on = profile_for_home(canon.to_str(), None, FsCapability::TmpOnly);

        for sub in PRIVATE_SUBDIRS {
            let expect = format!("(subpath \"{}\")", sbpl_str(&canon.join(sub)));
            assert!(
                on.contains(&expect),
                "TmpOnly profile must deny private dir {sub} as {expect}; got {on}"
            );
            assert!(
                !off.contains(&expect),
                "credential-only profile must NOT deny private dir {sub}; got {off}"
            );
        }
        for sub in SECRET_SUBDIRS {
            let expect = format!("(subpath \"{}\")", sbpl_str(&canon.join(sub)));
            assert!(on.contains(&expect) && off.contains(&expect));
        }
        assert!(on.len() > off.len(), "TmpOnly must add clauses");
    }

    #[test]
    fn full_profile_does_not_deny_the_whole_home() {
        let tmp = std::env::temp_dir();
        let canon = std::fs::canonicalize(&tmp).expect("canonicalize temp dir");
        let profile = profile_for_home(canon.to_str(), None, FsCapability::TmpOnly);
        let bare_subpath = format!("(subpath \"{}\")", sbpl_str(&canon));
        let bare_literal = format!("(literal \"{}\")", sbpl_str(&canon));
        assert!(
            !profile.contains(&bare_subpath) && !profile.contains(&bare_literal),
            "profile must NOT deny the entire home (would break ~/.zshrc etc.); got {profile}"
        );
        let bare_library = format!("(subpath \"{}\")", sbpl_str(&canon.join("Library")));
        assert!(
            !profile.contains(&bare_library),
            "profile must NOT deny all of ~/Library (only specific stores); got {profile}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn secret_paths_are_canonicalized_not_raw() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let real = dir.path().join("home");
        std::fs::create_dir(&real).unwrap();
        let raw = dir.path().join("home-link");
        std::os::unix::fs::symlink(&real, &raw).unwrap();
        let canon = std::fs::canonicalize(&raw).unwrap();
        assert_ne!(canon, raw, "the fixture's link is no canonical path");
        let profile = profile_for_home(raw.to_str(), None, FsCapability::TmpOnly);
        let canon_marker = format!("(subpath \"{}\")", sbpl_str(&canon.join(".ssh")));
        let raw_marker = format!("(subpath \"{}\")", sbpl_str(&raw.join(".ssh")));
        assert!(
            profile.contains(&canon_marker),
            "must emit CANONICAL secret path {canon_marker}; got {profile}"
        );
        assert!(
            !profile.contains(&raw_marker),
            "must NOT emit the non-canonical raw path (Seatbelt would not match it); got {profile}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_secret_store_is_denied_at_its_canonical_target() {
        use std::os::unix::fs::symlink;
        let home = aterm_tempfile::tempdir().unwrap();
        let store = aterm_tempfile::tempdir().unwrap();
        let canon_home = std::fs::canonicalize(home.path()).unwrap();
        let canon_store = std::fs::canonicalize(store.path()).unwrap();
        symlink(store.path(), canon_home.join(".aws")).unwrap();

        let profile = profile_for_home(canon_home.to_str(), None, FsCapability::HomeReadWrite);

        let target_marker = format!("(subpath \"{}\")", canon_store.display());
        assert!(
            profile.contains(&target_marker),
            "symlinked ~/.aws must be denied at its canonical target {target_marker}; got {profile}"
        );
        let lexical_marker = format!("(subpath \"{}/.aws\")", canon_home.display());
        assert!(
            profile.contains(&lexical_marker),
            "lexical ~/.aws deny retained; got {profile}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_ancestor_with_absent_leaf_is_denied_at_resolved_target() {
        use std::os::unix::fs::symlink;
        let home = aterm_tempfile::tempdir().unwrap();
        let store = aterm_tempfile::tempdir().unwrap();
        let canon_home = std::fs::canonicalize(home.path()).unwrap();
        let canon_store = std::fs::canonicalize(store.path()).unwrap();
        symlink(store.path(), canon_home.join(".config")).unwrap();

        let profile = profile_for_home(canon_home.to_str(), None, FsCapability::HomeReadWrite);

        let resolved_prefix = format!("(subpath \"{}/", canon_store.display());
        assert!(
            profile.contains(&resolved_prefix),
            "a secret path under symlinked ~/.config must be denied at its resolved target ({resolved_prefix}); got {profile}"
        );
        let outside = format!("(literal \"{}\")", sbpl_str(&canon_store));
        assert!(
            !profile.contains(&outside),
            "{outside} is outside the home; got {profile}"
        );
    }

    #[test]
    fn nonexistent_home_falls_back_to_the_base_profile() {
        let bogus = "/nonexistent/aterm-no-such-home-xyz";
        assert_eq!(
            profile_for_home(Some(bogus), None, FsCapability::HomeReadWrite),
            BASE_PROFILE
        );
        assert_eq!(
            profile_for_home(Some(bogus), None, FsCapability::TmpOnly),
            [BASE_PROFILE, &homeless_write_confinement()].concat()
        );
    }

    #[test]
    fn nested_secret_ancestor_nodes_are_write_denied_to_block_rename_relocation() {
        let home = aterm_tempfile::tempdir().unwrap();
        let canon_home = std::fs::canonicalize(home.path()).unwrap();
        std::fs::create_dir_all(canon_home.join(".config/gh")).unwrap();
        std::fs::create_dir_all(canon_home.join("Library/Application Support/Google/Chrome"))
            .unwrap();

        let profile = profile_for_home(canon_home.to_str(), None, FsCapability::TmpOnly);

        assert!(
            profile.contains("(deny file-write* (literal"),
            "must emit an ancestor write-deny clause; got {profile}"
        );
        let cfg = format!("(literal \"{}\")", sbpl_str(&canon_home.join(".config")));
        assert!(
            profile.contains(&cfg),
            "must write-deny the .config ancestor node {cfg}; got {profile}"
        );
        for anc in [
            "Library",
            "Library/Application Support",
            "Library/Application Support/Google",
        ] {
            let lit = format!("(literal \"{}\")", sbpl_str(&canon_home.join(anc)));
            assert!(
                profile.contains(&lit),
                "must write-deny ancestor node {anc} as {lit}; got {profile}"
            );
        }
        let home_lit = format!("(literal \"{}\")", sbpl_str(&canon_home));
        assert!(
            !profile.contains(&home_lit),
            "must never write-deny the home node itself; got {profile}"
        );
        let chrome_lit = format!(
            "(literal \"{}\")",
            sbpl_str(&canon_home.join("Library/Application Support/Google/Chrome"))
        );
        assert!(
            !profile.contains(&chrome_lit),
            "the leaf itself is covered by its subpath deny, not re-emitted as an ancestor literal; got {profile}"
        );
    }

    #[test]
    fn nonexistent_nested_ancestor_is_not_write_denied() {
        let home = aterm_tempfile::tempdir().unwrap();
        let canon_home = std::fs::canonicalize(home.path()).unwrap();
        let profile = profile_for_home(canon_home.to_str(), None, FsCapability::TmpOnly);
        assert!(
            !profile.contains("(deny file-write* (literal"),
            "no existing nested ancestor ⇒ no ancestor write-deny clause (would break mkdir -p); got {profile}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_ancestor_store_is_write_denied_at_its_resolved_nodes() {
        use std::os::unix::fs::symlink;
        let home = aterm_tempfile::tempdir().unwrap();
        let canon_home = std::fs::canonicalize(home.path()).unwrap();
        let store = canon_home.join("dotfiles/config");
        std::fs::create_dir_all(&store).unwrap();
        symlink(&store, canon_home.join(".config")).unwrap();

        let lex = format!("(literal \"{}\")", canon_home.join(".config").display());
        let real_store = format!("(literal \"{}\")", store.display());
        let real_parent = format!("(literal \"{}\")", canon_home.join("dotfiles").display());
        for present in [false, true] {
            if present {
                std::fs::create_dir_all(store.join("gh")).unwrap();
            }
            let profile = profile_for_home(canon_home.to_str(), None, FsCapability::HomeReadWrite);
            for node in [&lex, &real_store, &real_parent] {
                assert!(
                    profile.contains(node.as_str()),
                    "{node} must be write-denied (secret present: {present}); got {profile}"
                );
            }
        }
    }

    #[test]
    fn ancestor_write_deny_dedups_shared_parents() {
        let home = aterm_tempfile::tempdir().unwrap();
        let canon_home = std::fs::canonicalize(home.path()).unwrap();
        std::fs::create_dir_all(canon_home.join("Library/Application Support/Google/Chrome"))
            .unwrap();
        std::fs::create_dir_all(canon_home.join("Library/Messages")).unwrap();

        let profile = profile_for_home(canon_home.to_str(), None, FsCapability::TmpOnly);
        let lib_lit = format!("(literal \"{}\")", sbpl_str(&canon_home.join("Library")));
        assert_eq!(
            profile.matches(&lib_lit).count(),
            1,
            "the shared Library ancestor node must be write-denied exactly once; got {profile}"
        );
        std::fs::create_dir_all(canon_home.join(".ssh")).unwrap();
        let profile = profile_for_home(canon_home.to_str(), None, FsCapability::TmpOnly);
        for leaf in [".ssh", ".aws"] {
            let clause = format!("(subpath \"{}\")", sbpl_str(&canon_home.join(leaf)));
            assert_eq!(
                profile.matches(&clause).count(),
                1,
                "{clause}; got {profile}"
            );
        }
    }

    #[test]
    fn sbpl_escaping_quotes_and_backslashes() {
        let mut out = String::new();
        push_sbpl_escaped(&mut out, std::path::Path::new("/a\"b\\c"));
        assert_eq!(out, "/a\\\"b\\\\c");
    }

    /// Unix-only: the profile is scoped under `$HOME` ([`home_dir`]), which a native
    /// Windows process does not have, and only macOS ever applies one.
    #[cfg(unix)]
    #[test]
    fn private_data_is_gated_on_the_fs_capability_not_network() {
        let home = home_dir().unwrap_or_else(|| {
            aterm_spec::host::could_not_run("no HOME: no private-data gate check")
        });
        let canon = std::fs::canonicalize(&home).unwrap_or_else(|e| {
            aterm_spec::host::could_not_run(format!(
                "HOME {home} does not resolve ({e}): no private-data gate check"
            ))
        });
        let private_marker = format!("(subpath \"{}\")", sbpl_str(&canon.join("Documents")));
        let secret_marker = format!("(subpath \"{}\")", sbpl_str(&canon.join(".ssh")));

        let base = ContainmentPolicy::capabilities(ContainmentMode::Containment);

        let full = profile_for(&base).expect("Containment yields a profile");
        assert!(
            full.contains(&secret_marker),
            "secret must be denied; got {full}"
        );
        assert!(
            full.contains(&private_marker),
            "Containment (fs TmpOnly) must deny private data; got {full}"
        );

        let mut broadened = base;
        broadened.fs = FsCapability::HomeReadWrite;
        let prof = profile_for(&broadened).expect("network None still yields a profile");
        assert!(
            prof.contains(&secret_marker),
            "network-denied spawn must still deny credentials; got {prof}"
        );
        assert!(
            !prof.contains(&private_marker),
            "private-data deny must NOT apply when fs != TmpOnly; got {prof}"
        );
    }

    #[test]
    fn non_containment_modes_get_no_sandbox() {
        for mode in [ContainmentMode::User, ContainmentMode::Safety] {
            let caps = ContainmentPolicy::capabilities(mode);
            assert_eq!(
                profile_for(&caps),
                None,
                "{mode} must NOT be OS-sandboxed (network not denied)"
            );
        }
    }

    #[test]
    fn profile_selection_keys_only_on_network_capability_none() {
        let base = ContainmentPolicy::capabilities(ContainmentMode::Containment);
        let mut none = base;
        none.network = NetworkCapability::None;
        assert!(profile_for(&none).is_some());

        let mut full = base;
        full.network = NetworkCapability::Full;
        assert!(profile_for(&full).is_none());
    }

    #[test]
    fn the_app_data_roots_share_no_entry_with_the_private_set() {
        assert_eq!(
            APP_DATA_ROOTS,
            &["Library/Containers", "Library/Group Containers"]
        );
        for root in APP_DATA_ROOTS {
            assert!(
                !PRIVATE_SUBDIRS.contains(root),
                "{root} is an App Data root, not a Seatbelt private-data deny"
            );
        }
    }
}
