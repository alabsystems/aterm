// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Lab state paths, binary resolution, home guards, and environment scrubbing.

use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;

/// The facts every verb shares, resolved once.
#[derive(Debug)]
pub struct Paths {
    /// `$LAB_STATE`, else [`default_state`] (`/private/tmp/at.lab-<uid>/<checkout>`, outside every home).
    pub state: PathBuf,
    /// Whether [`Self::state`] is the default ([`default_state`]: `$LAB_STATE` unset or empty), which [`Paths::make_default_state`] makes private.
    pub state_default: bool,
    /// `$LAB_STATE` as given, when it is relative.
    pub state_relative: Option<String>,
    /// The aterm a NEW lab tests, when one was found (a lab that is up runs the one its `env.sh` recorded at `up`, which its jail pins).
    pub aterm: Option<PathBuf>,
    /// Why `$LAB_ATERM` was not taken, when it was not.
    pub aterm_note: Option<String>,
    /// The UID account's physical home, independently of `HOME` (the account database's: [`aterm_uds::account::home`]).
    pub real_home: String,
    /// The calling shell's physical `HOME`, when it names a directory.
    pub caller_home: String,
    /// The owner's login name.
    pub user: String,
    /// This program.
    pub exe: PathBuf,
    /// The checkout this program was built from.
    pub repo: PathBuf,
}

static PATHS: OnceLock<Paths> = OnceLock::new();

/// The shared facts ([`Paths::resolve`] on first use).
pub fn paths() -> &'static Paths {
    PATHS.get_or_init(Paths::resolve)
}

/// The variable prefixes a calling shell's aterm, agent or package manager sets.
pub const FORGET_PREFIXES: &[&str] = &[
    "ATERM",
    "CLAUDE",
    "ANTHROPIC",
    "ATPKG",
    "COPILOT",
    "CODEX",
    "CURSOR",
    "META_",
    "MUSE",
    "TBH_",
];

/// Whether the variable `key` is one a lab forgets.
#[must_use]
pub fn forgotten(key: &str) -> bool {
    let proxy = key.split_once('_').is_some_and(|(head, tail)| {
        !head.is_empty()
            && head.chars().all(|c| c.is_ascii_alphabetic())
            && tail.eq_ignore_ascii_case("proxy")
    });
    key == "AI_AGENT"
        || key == "CLAUDECODE"
        || key == "CURL_HOME"
        || proxy
        || FORGET_PREFIXES.iter().any(|p| {
            key.starts_with(p)
                && key[p.len()..]
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_')
        })
}

impl Paths {
    fn resolve() -> Paths {
        Paths::resolve_with(aterm_uds::account::home(), std::env::var_os("HOME"))
    }

    /// [`Paths::resolve`] with the account database's home (`account`) and `$HOME` (`home`) given.
    #[must_use]
    pub fn resolve_with(account: Option<PathBuf>, home: Option<OsString>) -> Paths {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
        let fallback = || default_state(crate::sys::getuid(), &physical(&repo));
        let (state, state_relative, state_default) = match std::env::var("LAB_STATE") {
            Ok(s) if !s.is_empty() && !s.starts_with('/') => (fallback(), Some(s), false),
            Ok(s) if !s.is_empty() => (physical(Path::new(s.trim_end_matches('/'))), None, false),
            _ => (fallback(), None, true),
        };
        let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("aterm-lab"));
        let (aterm, aterm_note) = find_aterm(&exe, &repo);
        let real_home = home_path(account.as_deref());
        let caller_home = home_path(home.as_deref().map(Path::new));
        let user = crate::sys::login_name()
            .or_else(|| std::env::var("USER").ok().filter(|u| !u.is_empty()))
            .unwrap_or_else(|| crate::sys::getuid().to_string());
        Paths {
            state,
            state_relative,
            state_default,
            aterm,
            aterm_note,
            real_home,
            caller_home,
            user,
            exe,
            repo,
        }
    }

    /// Every home the jail's witnesses and scrubs take as the owner's ([`guarded`]).
    #[must_use]
    pub fn guarded_homes(&self) -> Vec<String> {
        guarded(&self.real_home, &self.caller_home)
    }

    /// The guarded homes, or the refusal when neither the account database nor `HOME` names a directory.
    ///
    /// # Errors
    /// No home to guard.
    pub fn need_guarded_homes(&self) -> Result<Vec<String>, String> {
        let homes = self.guarded_homes();
        if homes.is_empty() {
            return Err(
                "neither the account database nor HOME names a home directory: the jail protects the owner's home, \
                 so one must be known"
                    .into(),
            );
        }
        Ok(homes)
    }

    /// The home whose package store holds the managed agents.
    #[must_use]
    pub fn store_home(&self) -> &str {
        if self.real_home.is_empty() {
            &self.caller_home
        } else {
            &self.real_home
        }
    }

    /// [`Paths::store_home`], or the refusal when there is none.
    ///
    /// # Errors
    /// Neither the account database nor `HOME` names a directory.
    pub fn need_store_home(&self) -> Result<&str, String> {
        match self.store_home() {
            "" => Err("neither the account database nor HOME names a home directory, so no package store can be found".into()),
            home => Ok(home),
        }
    }

    /// Whether the caller's `HOME` is a scratch one.
    #[must_use]
    pub fn scratch_home(&self) -> bool {
        !self.caller_home.is_empty() && self.caller_home != self.real_home
    }

    /// The state dir to make things in.
    ///
    /// # Errors
    /// `$LAB_STATE` is relative, or the default is not this user's own private directory.
    pub fn need_state(&self) -> Result<&Path, String> {
        let state = self.state_named()?;
        self.make_default_state()?;
        Ok(state)
    }

    /// The state dir, or the refusal of a relative `$LAB_STATE`: a check that
    /// makes nothing.
    fn state_named(&self) -> Result<&Path, String> {
        match &self.state_relative {
            Some(s) => Err(format!(
                "LAB_STATE={s} is relative (each call would read it from its own cwd, which a jailed process may \
                 write, and source the env.sh it found there): give its absolute path"
            )),
            None => Ok(&self.state),
        }
    }

    /// The state dir, or the refusal of one inside a home the jail fences (`homes`, [`Self::guarded_homes`]).
    ///
    /// # Errors
    /// `$LAB_STATE` is the home or lies under it.
    pub fn need_state_outside(&self, homes: &[String]) -> Result<&Path, String> {
        let state = self.state_named()?;
        let real = physical(state);
        let inside = |h: &&String| {
            let home = physical(Path::new(h));
            [state, real.as_path()]
                .iter()
                .any(|s| s.starts_with(h) || s.starts_with(&home))
        };
        match homes.iter().find(inside) {
            Some(h) => Err(format!(
                "LAB_STATE={} sits inside the home {h}, whose contents the lab's jail fences: the fence refuses \
                 each directory from that home down to a run, which the harness's tape store opens one at a time, so \
                 a lab there keeps no tape. Leave LAB_STATE unset (the default, {}, is outside every home) or name \
                 a directory outside every home",
                state.display(),
                default_state(crate::sys::getuid(), &physical(&self.repo)).display()
            )),
            None => Ok(state),
        }
    }

    /// Ensure the default state directory is private when [`Self::state_default`] is true.
    ///
    /// # Errors
    /// Either directory cannot be made, or is not this user's own private directory.
    pub fn make_default_state(&self) -> Result<(), String> {
        if !self.state_default {
            return Ok(());
        }
        let root = self
            .state
            .parent()
            .ok_or_else(|| format!("{} has no parent", self.state.display()))?;
        for dir in [root, self.state.as_path()] {
            private_dir(dir, crate::sys::getuid()).map_err(|why| {
                format!(
                    "the lab's default state dir: {why}; set LAB_STATE to an absolute directory outside every \
                     home"
                )
            })?;
        }
        Ok(())
    }

    /// The directory this program runs from.
    #[must_use]
    pub fn exe_dir(&self) -> PathBuf {
        self.exe
            .parent()
            .map_or_else(|| PathBuf::from("/"), physical)
    }

    /// This program's own source (`crates/aterm-lab`).
    #[must_use]
    pub fn crate_dir(&self) -> PathBuf {
        physical(Path::new(env!("CARGO_MANIFEST_DIR")))
    }

    /// The aterm under test, or the refusal that says how to get one.
    ///
    /// # Errors
    /// None was found, or `$LAB_ATERM` is not executable.
    pub fn need_aterm(&self) -> Result<&Path, String> {
        match &self.aterm {
            Some(a) if crate::sys::executable(&a.to_string_lossy()) => Ok(a),
            other => {
                if let Some(n) = &self.aterm_note {
                    eprintln!("aterm-lab: {n}");
                }
                let given = std::env::var("LAB_ATERM").ok().filter(|v| !v.is_empty());
                Err(format!(
                    "no aterm to test{}: build one with 'targo --unverified build -p aterm' (target/debug/aterm; \
                     an optimized build needs --features dev-seams for the lab's jail, not for instance, \
                     presence-flap or codex-alab-proof), or set LAB_ATERM to the build's physical path",
                    given
                        .or_else(|| other.as_ref().map(|a| a.display().to_string()))
                        .map(|a| format!(" at LAB_ATERM={a}"))
                        .unwrap_or_default()
                ))
            }
        }
    }

    /// The owner's home (the jail protects what is under it), or the refusal.
    ///
    /// # Errors
    /// The UID account's home is unavailable, root, relative or not UTF-8.
    pub fn need_home(&self) -> Result<&str, String> {
        if self.real_home.is_empty() {
            return Err(
                "the UID account's home cannot be resolved safely: the jail must protect it independently of HOME".into(),
            );
        }
        Ok(&self.real_home)
    }

    /// `runs/<tag>/`.
    #[must_use]
    pub fn run_dir(&self, tag: &str) -> PathBuf {
        self.state.join("runs").join(tag)
    }
}

/// The aterm a new lab tests, and why `$LAB_ATERM` was not taken: `$LAB_ATERM`,
/// else the `aterm` beside this program, else this checkout's debug build,
/// else its release build.
fn find_aterm(exe: &Path, repo: &Path) -> (Option<PathBuf>, Option<String>) {
    if let Some(a) = std::env::var_os("LAB_ATERM").filter(|a| !a.is_empty()) {
        let a = PathBuf::from(a);
        if !a.is_absolute() {
            return (
                None,
                Some(format!(
                    "LAB_ATERM={} is relative (read from each call's cwd, which a jailed process may write): give the build's physical path",
                    a.display()
                )),
            );
        }
        if a.exists()
            && let Ok(real) = std::fs::canonicalize(&a)
            && real != a
        {
            return (
                None,
                Some(format!(
                    "LAB_ATERM={} is not its build's physical path (a link, . or .. on it, which a jailed process could re-point): give {}",
                    a.display(),
                    real.display()
                )),
            );
        }
        return (Some(a), None);
    }
    let found = [
        exe.parent().map(|d| d.join("aterm")),
        Some(repo.join("target/debug/aterm")),
        Some(repo.join("target/release/aterm")),
    ]
    .into_iter()
    .flatten()
    .find(|c| crate::sys::executable(&c.to_string_lossy()));
    (found.map(|c| std::fs::canonicalize(&c).unwrap_or(c)), None)
}

/// A path the CALLER gave (`LAB_CLAUDE`, `LAB_CODEX`, `twin`'s build), made absolute against the caller's directory.
#[must_use]
pub fn from_caller(p: &str) -> String {
    if p.is_empty() || p.starts_with('/') {
        return p.to_string();
    }
    let cwd = caller_dir();
    format!("{}/{p}", cwd.trim_end_matches('/'))
}

/// The directory the lab was started from (its physical path), read once before the lab moves to `/`.
pub fn caller_dir() -> String {
    static D: OnceLock<String> = OnceLock::new();
    D.get_or_init(|| {
        std::env::current_dir()
            .ok()
            .and_then(|d| std::fs::canonicalize(d).ok())
            .map_or_else(|| "/".to_string(), |d| d.to_string_lossy().into_owned())
    })
    .clone()
}

/// `path` with its longest existing prefix resolved through links and `..`, the rest as written (nothing is made).
#[must_use]
pub fn physical(path: &Path) -> PathBuf {
    let mut head = path.to_path_buf();
    let mut rest: Vec<std::ffi::OsString> = Vec::new();
    loop {
        if let Ok(real) = std::fs::canonicalize(&head)
            && real.is_dir()
        {
            let mut out = real;
            for part in rest.iter().rev() {
                out.push(part);
            }
            return out;
        }
        let Some(name) = head.file_name().map(std::ffi::OsStr::to_os_string) else {
            return path.to_path_buf();
        };
        rest.push(name);
        if !head.pop()
            || head.as_os_str().is_empty()
            || head.components().all(|c| c == Component::CurDir)
        {
            return path.to_path_buf();
        }
    }
}

/// Where the default state dirs of every checkout of this user sit.
#[must_use]
pub fn state_root(uid: u32) -> PathBuf {
    PathBuf::from(format!("/private/tmp/at.lab-{uid}"))
}

/// The state dir a checkout at `repo` uses when `$LAB_STATE` is unset.
#[must_use]
pub fn default_state(uid: u32, repo: &Path) -> PathBuf {
    state_root(uid).join(checkout_key(repo))
}

/// A checkout's directory name under [`state_root`].
#[must_use]
pub fn checkout_key(repo: &Path) -> String {
    use std::os::unix::ffi::OsStrExt as _;
    let name: String = repo
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "._-".contains(c) {
                c
            } else {
                '_'
            }
        })
        .take(16)
        .collect();
    let mut hash: u32 = 0x811c_9dc5;
    for b in repo.as_os_str().as_bytes() {
        hash ^= u32::from(*b);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    let name = if name.is_empty() || name.starts_with('.') {
        format!("lab{name}")
    } else {
        name
    };
    format!("{name}-{hash:08x}")
}

/// `dir` made (mode 0700) when missing, then required to be what a private directory of `uid` is.
///
/// # Errors
/// It cannot be made, or it is a link, not a directory, another user's, or its mode cannot be narrowed.
pub fn private_dir(dir: &Path, uid: u32) -> Result<(), String> {
    use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _, PermissionsExt as _};
    match std::fs::DirBuilder::new().mode(0o700).create(dir) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(format!("{}: cannot be made ({e})", dir.display())),
    }
    let meta = std::fs::symlink_metadata(dir)
        .map_err(|e| format!("{}: cannot be read ({e})", dir.display()))?;
    if meta.file_type().is_symlink() || !meta.is_dir() {
        return Err(format!(
            "{} is not a directory (a link, or a file, stands there)",
            dir.display()
        ));
    }
    if meta.uid() != uid {
        return Err(format!(
            "{} belongs to uid {}, not to this user ({uid})",
            dir.display(),
            meta.uid()
        ));
    }
    if meta.mode() & 0o077 != 0 {
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).map_err(|e| {
            format!(
                "{}: its mode cannot be narrowed to 0700 ({e})",
                dir.display()
            )
        })?;
    }
    Ok(())
}

/// A lab tag: letters, digits, `_` and `-`, at most 24.
#[must_use]
pub fn valid_tag(t: &str) -> bool {
    !t.is_empty()
        && t.len() <= 24
        && t.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// The homes a lab guards, from the account's home (`account`, the account database's) and the caller's `HOME` (`caller`).
#[must_use]
pub fn guarded(account: &str, caller: &str) -> Vec<String> {
    let mut homes: Vec<String> = Vec::new();
    for h in [account, caller] {
        if !h.is_empty() && !homes.iter().any(|k| k == h) {
            homes.push(h.to_string());
        }
    }
    homes
}

/// A home fence must name an existing physical directory exactly, never a
/// lossy replacement of its name or the whole filesystem.
fn home_path(path: Option<&Path>) -> String {
    path.filter(|p| p.is_absolute() && *p != Path::new("/") && p.to_str().is_some())
        .and_then(|p| std::fs::canonicalize(p).ok())
        .filter(|p| p.is_dir() && p != Path::new("/"))
        .and_then(|p| p.to_str().map(str::to_owned))
        .unwrap_or_default()
}

/// A path that may go into a sandbox profile.
#[must_use]
pub fn profile_safe(s: &str) -> bool {
    !s.chars().any(|c| c == '"' || c == '\\' || c.is_control())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forgets_agent_and_aterm_variables_and_keeps_the_rest() {
        for k in [
            "ATERM_SESSION_ID",
            "CLAUDE_CONFIG_DIR",
            "ANTHROPIC_API_KEY",
            "ATPKG_BIN",
            "AI_AGENT",
            "CLAUDECODE",
            "CODEX_HOME",
            "META_API_KEY",
            "MUSE_MODEL",
            "TBH_AUTH_BASE_URL",
        ] {
            assert!(forgotten(k), "{k}");
        }
        for k in [
            "http_proxy",
            "HTTPS_PROXY",
            "ALL_PROXY",
            "all_proxy",
            "No_Proxy",
            "CURL_HOME",
        ] {
            assert!(forgotten(k), "{k}");
        }
        for k in [
            "LAB_ATERM",
            "HOME",
            "PATH",
            "AI_AGENTX",
            "TERM_PROGRAM",
            "LAB_MUSE",
            "METAL_DEVICE_WRAPPER_TYPE",
            "_PROXY",
            "MY1_PROXY",
            "PROXYX",
        ] {
            assert!(!forgotten(k), "{k}");
        }
    }

    #[test]
    fn tags_are_short_plain_words() {
        assert!(valid_tag("lab"));
        assert!(valid_tag("nudge-12345"));
        assert!(!valid_tag(""));
        assert!(!valid_tag("a/b"));
        assert!(!valid_tag(&"x".repeat(25)));
    }

    #[test]
    fn physical_resolves_the_existing_head_and_keeps_the_rest() {
        let tmp = std::env::temp_dir();
        let real = std::fs::canonicalize(&tmp).unwrap();
        assert_eq!(
            physical(&tmp.join("no-such-dir-lab/x")),
            real.join("no-such-dir-lab/x")
        );
        assert_eq!(home_path(Some(&tmp)), real.to_str().unwrap());
        for path in [None, Some(Path::new("/")), Some(Path::new("relative"))] {
            assert!(home_path(path).is_empty());
        }
        assert!(home_path(Some(&tmp.join("no-such-dir-lab/x"))).is_empty());
        use std::os::unix::ffi::OsStrExt as _;
        let owned = aterm_tempfile::tempdir().unwrap();
        let bytes = owned.path().join(std::ffi::OsStr::from_bytes(b"home-\xff"));
        assert!(
            home_path(Some(&bytes)).is_empty(),
            "a fence cannot use a lossy name"
        );
    }

    /// The homes a lab guards are a superset, never a narrowing: the
    /// account's first, then the caller's `HOME` when it names another, each
    /// once, an unknown one left out.
    #[test]
    fn the_guarded_homes_are_the_account_s_and_home_s_each_once() {
        assert_eq!(guarded("/Users//o", "/tmp/s"), ["/Users//o", "/tmp/s"]);
        assert_eq!(guarded("/Users//o", "/Users//o"), ["/Users//o"]);
        assert_eq!(guarded("", "/tmp/s"), ["/tmp/s"]);
        assert_eq!(guarded("/Users//o", ""), ["/Users//o"]);
        assert!(guarded("", "").is_empty());
    }

    /// THE ACCOUNT'S HOME IS RESOLVED FROM THE ACCOUNT DATABASE, NOT `HOME`: a
    /// fake account home handed in through the seam ([`Paths::resolve_with`])
    /// is guarded beside a scratch `HOME`, by its physical path, and is the
    /// package store's home.
    #[test]
    fn a_fake_account_home_is_guarded_beside_a_scratch_home() {
        let top = aterm_tempfile::Builder::new()
            .prefix("lab-homes-")
            .tempdir()
            .unwrap();
        let real = std::fs::canonicalize(top.path()).unwrap();
        let (account, scratch) = (top.path().join("account"), top.path().join("scratch"));
        for d in [&account, &scratch] {
            std::fs::create_dir_all(d).unwrap();
        }
        let (a, s) = (
            real.join("account").display().to_string(),
            real.join("scratch").display().to_string(),
        );
        let p = Paths::resolve_with(Some(account.clone()), Some(scratch.clone().into()));
        assert_eq!(
            (p.real_home.as_str(), p.caller_home.as_str()),
            (a.as_str(), s.as_str())
        );
        assert_eq!(p.guarded_homes(), [a.clone(), s.clone()]);
        assert_eq!(p.need_guarded_homes(), Ok(vec![a.clone(), s.clone()]));
        assert_eq!(p.store_home(), a);
        assert_eq!(p.need_home(), Ok(a.as_str()));
        assert!(p.scratch_home());
        let own = Paths::resolve_with(Some(account.clone()), Some(account.clone().into()));
        assert_eq!(own.guarded_homes(), std::slice::from_ref(&a));
        assert!(
            !own.scratch_home(),
            "HOME the account's own is no scratch home"
        );
        let env_only = Paths::resolve_with(None, Some(scratch.clone().into()));
        assert_eq!(env_only.guarded_homes(), std::slice::from_ref(&s));
        assert_eq!(env_only.store_home(), s);
        assert!(
            env_only.need_home().is_err(),
            "the jail refuses an unknown account home"
        );
        let gone = Paths::resolve_with(Some(real.join("no-such-home")), Some(scratch.into()));
        assert_eq!(gone.guarded_homes(), std::slice::from_ref(&s));
        let none = Paths::resolve_with(None, None);
        assert!(none.need_guarded_homes().is_err() && none.need_store_home().is_err());
    }

    /// THE LAB'S OWN FACTS ([`paths`]) TAKE THE ACCOUNT'S HOME FROM THE
    /// ACCOUNT DATABASE: whatever `HOME` the test runs under (a scratch one,
    /// by this repository's rule, or the account's own), `real_home` is the
    /// home `getpwuid_r` names, by its physical path; it is guarded, holds the
    /// store, and `caller_home` is `HOME`'s.
    #[test]
    fn the_lab_s_own_paths_take_the_account_home_from_the_account_database() {
        let want = home_path(aterm_uds::account::home().as_deref());
        assert!(
            !want.is_empty(),
            "the account database names this account's home"
        );
        let p = paths();
        assert_eq!(p.real_home, want);
        assert!(p.guarded_homes().contains(&want));
        assert_eq!(p.store_home(), want);
        assert_eq!(
            p.caller_home,
            home_path(std::env::var_os("HOME").as_deref().map(Path::new))
        );
        assert_eq!(
            p.scratch_home(),
            !p.caller_home.is_empty() && p.caller_home != want
        );
        let env_only = Paths::resolve_with(None, std::env::var_os("HOME"));
        assert!(env_only.real_home.is_empty() && env_only.need_home().is_err());
    }

    /// THE DEFAULT STATE DIR SITS OUTSIDE EVERY HOME, SHORT AND PER CHECKOUT:
    /// `/private/tmp/at.lab-<uid>/<name>-<hash>`, the name the checkout's own
    /// in safe letters (at most 16), the hash its whole path's, so two
    /// worktrees of one name differ; the longest is 56 bytes.
    #[test]
    fn the_default_state_sits_outside_every_home_short_and_per_checkout() {
        let a = default_state(501, Path::new("/Users//o/Developer/aterm"));
        let b = default_state(501, Path::new("/Users//o/Developer/wt/aterm"));
        for d in [&a, &b] {
            assert!(d.starts_with("/private/tmp/at.lab-501"), "{}", d.display());
            assert!(!d.starts_with("/Users//o"));
            let key = d.file_name().unwrap().to_string_lossy();
            assert!(
                key.starts_with("aterm-") && key.len() == "aterm-".len() + 8,
                "{key}"
            );
        }
        assert_ne!(a, b, "two checkouts named aterm keep two state dirs");
        assert_eq!(a, default_state(501, Path::new("/Users//o/Developer/aterm")));
        assert!(checkout_key(Path::new("/x/my repo!")).starts_with("my_repo_-"));
        assert!(checkout_key(Path::new("/x/.hidden")).starts_with("lab.hidden-"));
        assert!(checkout_key(Path::new("/")).starts_with("lab-"));
        let long = default_state(
            u32::MAX,
            Path::new("/x/a-checkout-whose-name-runs-on-and-on"),
        );
        assert!(
            long.to_string_lossy()
                .starts_with("/private/tmp/at.lab-4294967295/a-checkout-whose-")
        );
        assert_eq!(long.as_os_str().len(), 56, "{}", long.display());
        let p = paths();
        if std::env::var_os("LAB_STATE").is_none_or(|s| s.is_empty()) {
            assert!(p.state_default);
            assert_eq!(
                p.state,
                default_state(crate::sys::getuid(), &physical(&p.repo))
            );
            assert!(p.need_state_outside(&p.guarded_homes()).is_ok());
        }
    }

    /// A scratch tree under the temp dir, removed when it drops.
    struct Tree(PathBuf);

    impl Drop for Tree {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn tree(name: &str) -> Tree {
        let top = std::fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!("lab-paths-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&top);
        std::fs::create_dir_all(&top).unwrap();
        Tree(top)
    }

    fn mode(p: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::symlink_metadata(p).unwrap().permissions().mode() & 0o777
    }

    /// THE DEFAULT STATE DIR IS MADE THIS USER'S PRIVATE DIRECTORY: its root
    /// and its own directory are made 0700, and a looser root of this user's
    /// is narrowed.
    #[test]
    fn the_default_state_is_made_private_and_a_squatted_root_is_refused() {
        use std::os::unix::fs::PermissionsExt as _;
        let t = tree("private");
        let uid = crate::sys::getuid();
        let mut p = Paths::resolve_with(None, None);
        p.state_relative = None;
        p.state_default = true;
        p.state = t.0.join("root/key");
        p.make_default_state().unwrap();
        assert_eq!((mode(&t.0.join("root")), mode(&p.state)), (0o700, 0o700));
        std::fs::set_permissions(t.0.join("root"), std::fs::Permissions::from_mode(0o755)).unwrap();
        p.make_default_state().unwrap();
        assert_eq!(mode(&t.0.join("root")), 0o700, "a looser root is narrowed");
        std::fs::create_dir_all(t.0.join("elsewhere")).unwrap();
        std::os::unix::fs::symlink(t.0.join("elsewhere"), t.0.join("linked")).unwrap();
        p.state = t.0.join("linked/key");
        let err = p.make_default_state().unwrap_err();
        assert!(
            err.contains("not a directory") && err.contains("set LAB_STATE"),
            "{err}"
        );
        assert!(
            !t.0.join("elsewhere/key").exists(),
            "nothing made through the link"
        );
        std::fs::write(t.0.join("file"), "x").unwrap();
        p.state = t.0.join("file/key");
        assert!(
            p.make_default_state()
                .unwrap_err()
                .contains("not a directory")
        );
        let err = private_dir(&t.0.join("root"), uid.wrapping_add(1)).unwrap_err();
        assert!(err.contains("belongs to uid"), "{err}");
        p.state_default = false;
        p.state = t.0.join("linked/key");
        assert!(
            p.make_default_state().is_ok(),
            "a named LAB_STATE is not touched"
        );
    }

    /// THE STATE DIR A VERB MAKES THINGS IN IS MADE PRIVATE FIRST, whichever
    /// verb asks ([`Paths::need_state`]): `fleet-sim` skips the lab's
    /// up-front check and writes its record under `need_state()`, so a link
    /// planted at the default's root in the shared `/private/tmp` is refused
    /// there and nothing is made through it, and a fresh default comes back
    /// made, mode 0700.
    #[test]
    fn the_state_a_verb_makes_things_in_is_made_private_first() {
        let t = tree("need");
        let mut p = Paths::resolve_with(None, None);
        p.state_relative = None;
        p.state_default = true;
        std::fs::create_dir_all(t.0.join("elsewhere")).unwrap();
        std::os::unix::fs::symlink(t.0.join("elsewhere"), t.0.join("linked")).unwrap();
        p.state = t.0.join("linked/key");
        let err = p.need_state().unwrap_err();
        assert!(err.contains("not a directory"), "{err}");
        let results = t.0.join("linked/key/results");
        assert!(
            !t.0.join("elsewhere/key").exists() && !results.exists(),
            "nothing made through the link"
        );
        p.state = t.0.join("root/key");
        assert!(p.need_state_outside(&[]).is_ok());
        assert!(!t.0.join("root").exists(), "a check makes nothing");
        assert_eq!(p.need_state().unwrap(), t.0.join("root/key"));
        assert_eq!((mode(&t.0.join("root")), mode(&p.state)), (0o700, 0o700));
        p.state_default = false;
        p.state = t.0.join("named/key");
        assert_eq!(p.need_state().unwrap(), t.0.join("named/key"));
        assert!(
            !t.0.join("named").exists(),
            "a named LAB_STATE is not made here"
        );
        p.state_relative = Some("rel".into());
        assert!(p.need_state().unwrap_err().contains("is relative"));
    }

    /// A STATE DIR INSIDE A FENCED HOME IS REFUSED: in the account's home,
    /// in a scratch `HOME`, the home itself, and a path outside that leads in
    /// by a link, each naming the home and the default.
    #[test]
    fn a_state_dir_inside_a_home_is_refused_and_one_outside_goes_through() {
        let t = tree("inside");
        let (account, scratch) = (t.0.join("account"), t.0.join("scratch-home"));
        for h in [&account, &scratch] {
            std::fs::create_dir_all(h.join("lab")).unwrap();
        }
        std::os::unix::fs::symlink(account.join("lab"), t.0.join("leads-in")).unwrap();
        let mut p = Paths::resolve_with(Some(account.clone()), Some(scratch.clone().into()));
        p.state_relative = None;
        p.state_default = false;
        let homes = p.guarded_homes();
        for (state, home) in [
            (account.join("lab/state"), &account),
            (scratch.join("lab"), &scratch),
            (account.clone(), &account),
            (t.0.join("leads-in/state"), &account),
        ] {
            p.state = state;
            let err = p.need_state_outside(&homes).unwrap_err();
            assert!(
                err.contains(&format!("sits inside the home {}", home.display()))
                    && err.contains("/private/tmp/at.lab-"),
                "{err}"
            );
        }
        for state in [t.0.join("outside"), t.0.join("account2/state")] {
            p.state = state.clone();
            assert_eq!(p.need_state_outside(&homes), Ok(state.as_path()));
        }
    }
}
