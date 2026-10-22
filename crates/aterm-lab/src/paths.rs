// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Where the lab keeps what it makes, which aterm it tests, whose homes it
//! guards, and the rule that the calling shell's own agent and aterm
//! variables never reach a lab.

use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;

/// The facts every verb shares, resolved once.
#[derive(Debug)]
pub struct Paths {
    /// `$LAB_STATE`, else `<repo>/target/real-claude-lab`: `runs/ logs/ captures/ results/`.
    pub state: PathBuf,
    /// `$LAB_STATE` as given, when it is relative: refused before anything is
    /// made ([`Paths::need_state`]): each call would read it from its own cwd,
    /// which a jailed process may write.
    pub state_relative: Option<String>,
    /// The aterm a NEW lab tests, when one was found (a lab that is up runs
    /// the one its `env.sh` recorded at `up`, which its jail pins).
    pub aterm: Option<PathBuf>,
    /// Why `$LAB_ATERM` was not taken, when it was not.
    pub aterm_note: Option<String>,
    /// The UID account's physical home, independently of `HOME` (the account
    /// database's: [`aterm_uds::account::home`]); empty when it cannot be
    /// resolved safely, which [`Paths::need_home`] refuses. A lab run under a
    /// scratch `HOME`, as every agent run here is, guards this one all the
    /// same ([`Paths::guarded_homes`]) and finds the managed agents in its
    /// package store ([`Paths::store_home`]).
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

/// The variable prefixes a calling shell's aterm, agent or package manager
/// sets: a lab forgets them all, since the ctl client resolves its target from
/// such variables and the lab must never reach the owner's live aterm (nor
/// hand on a real key: Muse Code's `META_API_KEY` beats its login, and
/// `MUSE_*`/`TBH_*` steer it).
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

/// Whether the variable `key` is one a lab forgets: [`FORGET_PREFIXES`], two
/// exact names, and the caller's proxies in any case (`http_proxy`,
/// `ALL_PROXY`, …) with curl's `CURL_HOME`. What the lab runs outside the jail
/// reaches loopback only, and a proxy it inherited would carry those requests,
/// and what they hold, to another host; what it starts in the jail is handed
/// the black hole as its proxy by name.
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

    /// [`Paths::resolve`] with the account database's home (`account`) and
    /// `$HOME` (`home`) given: the seam a test hands a fake account home.
    #[must_use]
    pub fn resolve_with(account: Option<PathBuf>, home: Option<OsString>) -> Paths {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
        let (state, state_relative) = match std::env::var("LAB_STATE") {
            Ok(s) if !s.is_empty() && !s.starts_with('/') => {
                (repo.join("target/real-claude-lab"), Some(s))
            }
            Ok(s) if !s.is_empty() => (PathBuf::from(s), None),
            _ => (repo.join("target/real-claude-lab"), None),
        };
        let state = physical(Path::new(state.to_string_lossy().trim_end_matches('/')));
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
            aterm,
            aterm_note,
            real_home,
            caller_home,
            user,
            exe,
            repo,
        }
    }

    /// Every home the jail's witnesses and scrubs take as the owner's
    /// ([`guarded`]): the account's, and the caller's `HOME` when it names
    /// another (the homes [`crate::jail::render`] fences).
    #[must_use]
    pub fn guarded_homes(&self) -> Vec<String> {
        guarded(&self.real_home, &self.caller_home)
    }

    /// The guarded homes, or the refusal when neither the account database
    /// nor `HOME` names a directory.
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

    /// The home whose package store holds the managed agents: the account's,
    /// whatever `HOME` says (a scratch `HOME` holds no store), else the
    /// caller's.
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

    /// Whether the caller's `HOME` is a scratch one: it names a directory
    /// that is not the account's home (every agent run here sets one).
    #[must_use]
    pub fn scratch_home(&self) -> bool {
        !self.caller_home.is_empty() && self.caller_home != self.real_home
    }

    /// The state dir, or the refusal of a relative `$LAB_STATE`.
    ///
    /// # Errors
    /// `$LAB_STATE` is relative.
    pub fn need_state(&self) -> Result<&Path, String> {
        match &self.state_relative {
            Some(s) => Err(format!(
                "LAB_STATE={s} is relative (each call would read it from its own cwd, which a jailed process may \
                 write, and source the env.sh it found there): give its absolute path"
            )),
            None => Ok(&self.state),
        }
    }

    /// The directory this program runs from: the lab trusts it from outside
    /// the jail (the copy in `runs/<tag>/bin/` aside), so the jail may not write it.
    #[must_use]
    pub fn exe_dir(&self) -> PathBuf {
        self.exe
            .parent()
            .map_or_else(|| PathBuf::from("/"), physical)
    }

    /// This program's own source (`crates/aterm-lab`): the next `targo
    /// --unverified lab` compiles it and runs it outside the jail.
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
/// else its release build. The lab's door runs it under the recorded jail, which
/// pins the file and its parents by their physical paths only: a link on the
/// path as written could be re-pointed from inside the jail, and a relative one
/// names another file from each cwd. So a `$LAB_ATERM` must already be the
/// build's physical path; a found one is taken by its physical path.
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

/// A path the CALLER gave (`LAB_CLAUDE`, `LAB_CODEX`, `twin`'s build), made
/// absolute against the caller's directory: the lab itself works from `/`.
#[must_use]
pub fn from_caller(p: &str) -> String {
    if p.is_empty() || p.starts_with('/') {
        return p.to_string();
    }
    let cwd = caller_dir();
    format!("{}/{p}", cwd.trim_end_matches('/'))
}

/// The directory the lab was started from (its physical path), read once
/// before the lab moves to `/`.
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

/// `path` with its longest existing prefix resolved through links and `..`,
/// the rest as written (nothing is made).
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

/// A lab tag: letters, digits, `_` and `-`, at most 24.
#[must_use]
pub fn valid_tag(t: &str) -> bool {
    !t.is_empty()
        && t.len() <= 24
        && t.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// The homes a lab guards, from the account's home (`account`, the account
/// database's) and the caller's `HOME` (`caller`): the account's first, then
/// the caller's when it names another, each only when known. A superset of
/// either, never a narrowing: a scratch `HOME` adds its own home and takes
/// nothing of the account's away.
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

/// A path that may go into a sandbox profile: no quote, backslash or control character.
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
        // Reject the input without requiring this filesystem to support a
        // non-UTF8 directory name in the first place.
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
    /// package store's home. NEGATIVE CONTROLS: with no account home known
    /// (or one that names no directory) only `$HOME` is guarded and holds the
    /// store, `HOME` the account's own is no scratch home, and with neither
    /// the lab has no home to guard.
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
        // NEGATIVE CONTROLS
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
    /// store, and `caller_home` is `HOME`'s. Only the paths are compared:
    /// nothing under the account's home is read. NEGATIVE CONTROL: the same
    /// facts resolved without the account database (`HOME` alone, the lab
    /// before the fence) name no account home, which the jail refuses.
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
        // NEGATIVE CONTROL
        let env_only = Paths::resolve_with(None, std::env::var_os("HOME"));
        assert!(env_only.real_home.is_empty() && env_only.need_home().is_err());
    }
}
