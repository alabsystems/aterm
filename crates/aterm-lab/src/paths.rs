// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Lab state paths, binary resolution, home guards, and environment scrubbing.

use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;

/// The facts every verb shares, resolved once.
#[derive(Debug)]
pub struct Paths {
    /// `$LAB_STATE`, else [`default_state`].
    pub state: PathBuf,
    /// Whether [`Self::state`] is the default ([`default_state`]: `$LAB_STATE` unset or empty).
    pub state_default: bool,
    /// `$LAB_STATE` as given, when it is relative.
    pub state_relative: Option<String>,
    /// The aterm a NEW lab tests.
    pub aterm: Option<PathBuf>,
    /// Why `$LAB_ATERM` was not taken, when it was not.
    pub aterm_note: Option<String>,
    /// The UID account's physical home.
    pub real_home: String,
    /// The calling shell's physical `HOME`, when it names a directory.
    pub caller_home: String,
    /// The owner's login name.
    pub user: String,
    /// This program.
    pub exe: PathBuf,
    /// The checkout this program runs from, read when it runs ([`checkout`]).
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

/// Whether the variable `key` is one a lab forgets: every one a calling shell's aterm, agent or
/// package manager sets ([`FORGET_PREFIXES`]), but the directory where the ceiling over this run
/// reads its waits for the lane or for room ([`aterm_toolchain::waits::VAR`]), which verify sets
/// for this run alone.
#[must_use]
pub fn forgotten(key: &str) -> bool {
    if key == aterm_toolchain::waits::VAR {
        return false;
    }
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
        let repo = checkout();
        let fallback = || default_state(crate::sys::getuid(), &physical(&repo));
        let (state, state_relative, state_default) = match std::env::var_os("LAB_STATE") {
            Some(os) if !os.is_empty() => {
                let raw = Path::new(&os);
                if !raw.is_absolute() {
                    (fallback(), Some(os.to_string_lossy().into_owned()), false)
                } else if let Some(s) = os.to_str() {
                    let trimmed = s.trim_end_matches('/');
                    let path = if trimmed.is_empty() { "/" } else { trimmed };
                    (physical(Path::new(path)), None, false)
                } else {
                    (PathBuf::from(os), None, false)
                }
            }
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

    /// The guarded homes.
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
    /// # Errors
    /// `$LAB_STATE` is relative, or the default is not this user's own private directory.
    pub fn need_state(&self) -> Result<&Path, String> {
        let state = self.state_named()?;
        self.make_default_state()?;
        Ok(state)
    }

    fn state_named(&self) -> Result<&Path, String> {
        if let Some(s) = &self.state_relative {
            return Err(format!(
                "LAB_STATE={s} is relative (each call would read it from its own cwd, which a jailed process may \
                 write, and source the env.sh it found there): give its absolute path"
            ));
        }
        if self.state.to_str().is_none() {
            return Err(
                "LAB_STATE is not valid UTF-8: the sandbox profile quotes it as UTF-8 text".into(),
            );
        }
        Ok(&self.state)
    }

    /// The state dir.
    /// # Errors
    /// `$LAB_STATE` is the home or lies under it.
    /// Or `$LAB_STATE` encloses a guarded home or [`state_root`], or is not valid UTF-8.
    pub fn need_state_outside(&self, homes: &[String]) -> Result<&Path, String> {
        let state = self.state_named()?;
        let real = physical(state);
        let inside = |h: &&String| {
            let home = physical(Path::new(h));
            [state, real.as_path()]
                .iter()
                .any(|s| s.starts_with(h) || s.starts_with(&home))
        };
        if let Some(h) = homes.iter().find(inside) {
            return Err(format!(
                "LAB_STATE={} sits inside the home {h}, whose contents the lab's jail fences: the fence refuses \
                 each directory from that home down to a run, which the harness's tape store opens one at a time, so \
                 a lab there keeps no tape. Leave LAB_STATE unset (the default, {}, is outside every home) or name \
                 a directory outside every home",
                state.display(),
                default_state(crate::sys::getuid(), &physical(&self.repo)).display()
            ));
        }
        let encloses = |h: &&String| {
            let home = physical(Path::new(h));
            [state, real.as_path()]
                .iter()
                .any(|s| Path::new(h).starts_with(s) || home.starts_with(s))
        };
        if let Some(h) = homes.iter().find(encloses) {
            return Err(format!(
                "LAB_STATE={} encloses the home {h}, whose contents the lab's jail fences: reopening LAB_STATE \
                 would reopen {h}. Leave LAB_STATE unset (the default, {}, is outside every home) or name a \
                 directory outside every home",
                state.display(),
                default_state(crate::sys::getuid(), &physical(&self.repo)).display()
            ));
        }
        let states = state_root(crate::sys::getuid());
        let states_real = physical(&states);
        if [state, real.as_path()]
            .iter()
            .any(|s| states.starts_with(s) || states_real.starts_with(s))
        {
            return Err(format!(
                "LAB_STATE={} encloses {}, whose subdirectories hold every checkout's default lab state: reopening \
                 LAB_STATE would reopen other checkouts' runs and logs. Leave LAB_STATE unset (the default, {}, \
                 is per-checkout) or name a directory of its own",
                state.display(),
                states.display(),
                default_state(crate::sys::getuid(), &physical(&self.repo)).display()
            ));
        }
        Ok(state)
    }

    /// Ensure the default state directory is private when [`Self::state_default`] is true.
    /// # Errors
    /// The quiet parent is unavailable, or either state directory cannot be made or is not
    /// this user's own private directory.
    pub fn make_default_state(&self) -> Result<(), String> {
        if !self.state_default {
            return Ok(());
        }
        let temporary = Path::new("/private/tmp");
        let quiet = temporary.join(aterm_tempfile::QUIET);
        if aterm_tempfile::quiet_in(temporary) != quiet {
            return Err(format!(
                "the lab's quiet state parent {} is unavailable",
                quiet.display()
            ));
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

    /// This program's own source (`crates/aterm-lab` in [`Self::repo`]).
    #[must_use]
    pub fn crate_dir(&self) -> PathBuf {
        physical(&self.repo.join(CRATE_DIR))
    }

    /// The aterm under test, or the refusal that says how to get one.
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

/// This crate's directory in a checkout.
pub(crate) const CRATE_DIR: &str = "crates/aterm-lab";

/// The checkout this program runs from, read when it runs (a build directory reused at another
/// path serves that path's checkout): the nearest directory holding this crate's manifest
/// ([`CRATE_DIR`]) at or above `CARGO_MANIFEST_DIR`, which `targo --unverified lab` (a `targo
/// run`) and `targo test` set to this crate's directory; else at or above the directory the run
/// started in ([`caller_dir`]: [`crate::sys::confine_env`] has left the working directory at `/`
/// by now), where verify starts its lab lanes (the checkout's root, with `CARGO_MANIFEST_DIR`
/// removed); else that directory itself.
fn checkout() -> PathBuf {
    let caller = PathBuf::from(caller_dir());
    let manifest = std::env::var_os("CARGO_MANIFEST_DIR")
        .filter(|d| !d.is_empty())
        .map(PathBuf::from);
    checkout_from(manifest.into_iter().chain([caller.clone()])).unwrap_or(caller)
}

/// [`checkout`] from each of `starts` in turn: the first nearest directory at or above one that
/// holds [`CRATE_DIR`]'s manifest, if any does.
fn checkout_from(starts: impl IntoIterator<Item = PathBuf>) -> Option<PathBuf> {
    let manifest_file = Path::new(CRATE_DIR).join("Cargo.toml");
    starts.into_iter().find_map(|start| {
        start
            .ancestors()
            .find(|dir| dir.join(&manifest_file).is_file())
            .map(Path::to_path_buf)
    })
}

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
    let target = plain_target(repo);
    let found = [
        exe.parent().map(|d| d.join("aterm")),
        Some(target.join("debug/aterm")),
        Some(target.join("release/aterm")),
    ]
    .into_iter()
    .flatten()
    .find(|c| crate::sys::executable(&c.to_string_lossy()));
    (found.map(|c| std::fs::canonicalize(&c).unwrap_or(c)), None)
}

/// Where a plain build of the checkout at `repo` puts its final artifacts: the target Cargo
/// selects there (`CARGO_TARGET_DIR`, a configured `target-dir`), else the workspace's
/// `target`. A configuration that cannot be read selects nothing Cargo could build into.
#[must_use]
pub fn plain_target(repo: &Path) -> PathBuf {
    aterm_toolchain::build_cache::cargo_target_hint(repo)
        .ok()
        .flatten()
        .unwrap_or_else(|| repo.join("target"))
}

/// A path the CALLER gave (`LAB_CLAUDE`, `LAB_CODEX`, `twin`'s build), made absolute against
/// the caller's directory.
#[must_use]
pub fn from_caller(p: &str) -> String {
    if p.is_empty() || p.starts_with('/') {
        return p.to_string();
    }
    let cwd = caller_dir();
    format!("{}/{p}", cwd.trim_end_matches('/'))
}

/// The directory the lab was started from (its physical path).
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

/// `path` with its longest existing prefix resolved through links and `..`.
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

/// Where the default state dirs of every checkout of this user sit, outside homes and quiet.
#[must_use]
pub fn state_root(uid: u32) -> PathBuf {
    PathBuf::from(format!("/private/tmp/.ubd/at.lab-{uid}"))
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

/// `dir` made (mode 0700) when missing.
/// # Errors
/// It cannot be made, or it is a link, not a directory, another user's, or its mode cannot be
/// narrowed.
pub fn private_dir(dir: &Path, uid: u32) -> Result<(), String> {
    use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _};
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
        crate::util::set_dir_mode_nofollow(dir, 0o700).map_err(|e| {
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

/// The homes a lab guards.
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
#[path = "paths_tests.rs"]
mod tests;
