// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Per-host, per-user GitHub SSH key and host-key configuration ([`System::write_user_file`]).

use super::{Ctx, Item};
use crate::model::{Probe, Read, User};
use crate::port::{Cmd, System};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

/// The `[github]` table.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Table {
    /// Make and configure `~/.ssh/aterm_github_ed25519`.
    pub key: Option<bool>,
    /// `git config --global user.name` (`{user}`, `{name}` fill).
    pub git_name: Option<String>,
    /// `git config --global user.email`.
    pub git_email: Option<String>,
    /// Rewrite `https://github.com/` to SSH in git's global config.
    pub ssh_urls: Option<bool>,
}

/// The key, relative to the user's home.
pub const KEY_FILE: &str = ".ssh/aterm_github_ed25519";

/// The first line of every block this family writes.
pub const BLOCK_BEGIN: &str = "# >>> aterm-machines github";

/// The last line of every block this family writes.
pub const BLOCK_END: &str = "# <<< aterm-machines github";

/// GitHub's published SSH host keys (`ssh_keys` of `https://api.github.com/meta`, read 2026-10-16).
pub const GITHUB_HOST_KEYS: [&str; 3] = [
    "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOMqqnkVzrm0SdG6UOoqKLsabgH5C9okWi0dh2l9GKJl",
    "ecdsa-sha2-nistp256 AAAAE2VjZHNhLXNoYTItbmlzdHAyNTYAAAAIbmlzdHAyNTYAAABBBEmKSENjQEezOmxkZMy7opKgwFB9nkt5YRrYMjNuG5N87uRgg6CLrbo5wAdT/y6v0mKV0U2w0WZ2YB/++Tpockg=",
    "ssh-rsa AAAAB3NzaC1yc2EAAAADAQABAAABgQCj7ndNxQowgcQnjshcLrqPEiiphnt+VTTvDP6mHBL9j1aNUkY4Ue1gvwnGLVlOhGeYrnZaMgRK6+PKCUXaDbC7qtbW8gIkhL7aGCsOr/C56SJMy/BCZfxd1nWzAOxSDPgVsmerOBYfNqltV9/hWCqBywINIR+5dIg6JTJ72pcEpEjcYgXkE2YEFXV1JHnsKgbLWNlhScqb2UmyRkQyytRLtL+38TGxkxCflmO+5Z8CSSNY7GidjMIZ7Q4zMjA2n1nGrlTDkzwDCsw+wqFPGQA179cnfGWOWRVruj16z6XyvxvjJwbz0wQZ75XK5tKSb7FNyeIEs4TT4jk+S4dhPeAUC5y+bDYirYgM4GC7uEnztnZyaVWQ7B381AK4Qdrwt51ZqExKbQpTUNn+EjqoTwvqNj4kqx5QUCI0ThS/YkOxJCXmPUWZbhjpCg56i+2aB6CmK2JGhn57K5mj0MNdBXA4/WnwH6XoPWJzK5Nyu2zB3nAZp+S5hpQs+p1vN1/wsjk=",
];

/// SHA-256 fingerprints documented by GitHub for [`GITHUB_HOST_KEYS`].
pub const GITHUB_FINGERPRINTS: [&str; 3] = [
    "SHA256:+DiY3wvvV6TuJJhbpZisF/zLDA0zPMSvHdkr4UvCOqU",
    "SHA256:p2QAMXNIC1TJYWeIOttrVc98/R1BUFWu3/LiyKgUfQM",
    "SHA256:uNiVztksCsDhcc0u9e8BujQXVUpKZIDTMczCvj3tD2s",
];

/// git from the Command Line Tools.
pub const CLT_GIT: &str = "/Library/Developer/CommandLineTools/usr/bin/git";

/// Where `xcode-select -s` keeps the developer directory a person chose, which
/// `xcode-select -p` prints.
const XCODE_SELECT_LINK: &str = "/var/db/xcode_select_link";

/// The developer directory `xcode-select -p` names with no choice made and Xcode installed.
const XCODE_DEVELOPER: &str = "/Applications/Xcode.app/Contents/Developer";

/// The distribution's git everywhere but macOS ([`installed_git`]).
pub const SYSTEM_GIT: &str = "/usr/bin/git";

/// The git installed for the runs here: a live build in the account's `atpkg` store first,
/// which the default toolset updates independently of the operating system. Without one,
/// the distribution's git ([`SYSTEM_GIT`]) or, on a Mac, the tools' own ([`CLT_GIT`]), then
/// the selected Xcode's. Never macOS's `/usr/bin/git` shim, which asks a person to install
/// the Command Line Tools. Native links resolve to a nonempty executable target.
/// Read without a child process; `None` when none is there.
#[must_use]
pub fn installed_git(sys: &dyn System) -> Option<PathBuf> {
    installed_git_for(sys, None)
}

/// [`installed_git`] restricted to the system / Apple Developer paths without `atpkg`'s store
/// (the fallback of [`installed_git`] and the negative-control baseline in tests).
#[must_use]
pub fn installed_git_without_atpkg(sys: &dyn System) -> Option<PathBuf> {
    if !sys.macos() {
        return git_executable_target(sys, Path::new(SYSTEM_GIT));
    }
    if let Some(git) = git_executable_target(sys, Path::new(CLT_GIT)) {
        return Some(git);
    }
    selected_developer_dir(sys)
        .ok()
        .flatten()
        .into_iter()
        .chain([PathBuf::from(XCODE_DEVELOPER)])
        .map(|dir| dir.join("usr/bin/git"))
        .find_map(|git| git_executable_target(sys, &git))
}

/// The system selection, resolved beside its link and through an application's developer
/// contents. Inspect through the port: its `stat` does not follow a final symbolic link.
fn selected_developer_dir(sys: &dyn System) -> std::io::Result<Option<PathBuf>> {
    let link = Path::new(XCODE_SELECT_LINK);
    let dir = match sys.read_link(link) {
        Ok(dir) if dir.is_absolute() => dir,
        Ok(dir) => link
            .parent()
            .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::InvalidInput))?
            .join(dir),
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::InvalidInput
            ) =>
        {
            return Ok(None);
        }
        Err(error) => return Err(error),
    };
    aterm_types::clt_shim::developer_content_dir_with(&dir, |contents| {
        let real = sys.canonicalize(contents)?;
        sys.stat(&real).map(|stat| stat.dir && !stat.symlink)
    })
    .map(Some)
}

/// The nonempty executable target used by Git discovery and settings readers.
/// A macOS install shim is never an admitted target.
pub(crate) fn git_executable_target(sys: &dyn System, path: &Path) -> Option<PathBuf> {
    let real = sys.canonicalize(path).ok()?;
    if sys.macos() && real == Path::new(SYSTEM_GIT) {
        return None;
    }
    let stat = sys.stat(&real).ok()?;
    (!stat.dir && !stat.symlink && stat.len > 0 && stat.mode & 0o111 != 0).then_some(real)
}

/// What a child's `DEVELOPER_DIR` names on a Mac whose `/usr/bin` developer shims would ask a
/// person to install the tools ([`shims_ask_for_tools`]): a path that cannot exist, under the
/// empty, root-owned `/var/empty`, so a shim it runs (`python3`, `make`, `clang`, `git`) fails at
/// once (`xcrun: error: missing DEVELOPER_DIR path: …`, measured on macOS 27.0) instead.
pub const NO_DEVELOPER_DIR: &str = "/var/empty/aterm-no-developer-tools";

/// Whether macOS's `/usr/bin` developer shims would ask a person to install the Command Line
/// Tools here, Apple's dialog on their screen, because no developer directory holds the tools:
/// not the one `xcode-select -s` chose, else neither Xcode's nor the tools' own, as `xcrun`
/// looks. Never off macOS, which has no shims. Read without a child process.
#[must_use]
pub fn shims_ask_for_tools(sys: &dyn System) -> bool {
    if !sys.macos() {
        return false;
    }
    let holds_tools = |dir: &PathBuf| {
        sys.canonicalize(&dir.join("usr/bin"))
            .and_then(|real| sys.stat(&real))
            .is_ok_and(|stat| stat.dir && !stat.symlink)
    };
    match selected_developer_dir(sys) {
        Ok(Some(dir)) => !holds_tools(&dir),
        Ok(None) => ![XCODE_DEVELOPER, crate::items::developer::CLT_DIR]
            .into_iter()
            .map(PathBuf::from)
            .any(|dir| holds_tools(&dir)),
        Err(_) => true,
    }
}

/// [`installed_git`] for `user_home`'s store when given, otherwise this process's account.
/// A named account never borrows another account's managed build; native Git is the fallback.
#[must_use]
pub fn installed_git_for(sys: &dyn System, user_home: Option<&Path>) -> Option<PathBuf> {
    installed_git_reading(sys, user_home).map(GitInstallation::program)
}

/// The admitted Git command and the immutable payload its package selects. They are the
/// same path for native Git. A managed shim can be repaired without replacing its payload.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitExecution {
    /// The command setup actually starts, including a managed package's shim.
    pub program: PathBuf,
    /// The selected package payload, or the native executable itself.
    pub payload: PathBuf,
}

/// Observe the selected command and payload through the same ordinary admission as
/// [`installed_git`], without starting a program or granting execution authority.
#[must_use]
pub fn installed_git_execution(sys: &dyn System) -> Option<GitExecution> {
    installed_git_reading(sys, None).map(GitInstallation::execution)
}

/// The selected Git's admitted executable identity, without starting it. A managed build's
/// immutable payload path changes when activation replaces the build behind its stable shim.
#[must_use]
pub fn installed_git_identity(sys: &dyn System) -> Option<PathBuf> {
    installed_git_execution(sys).map(|git| git.payload)
}

enum GitInstallation {
    Native(PathBuf),
    Managed { shim: PathBuf, payload: PathBuf },
}

impl GitInstallation {
    fn program(self) -> PathBuf {
        match self {
            Self::Native(path) | Self::Managed { shim: path, .. } => path,
        }
    }

    fn execution(self) -> GitExecution {
        match self {
            Self::Native(path) => GitExecution {
                program: path.clone(),
                payload: path,
            },
            Self::Managed { shim, payload } => GitExecution {
                program: shim,
                payload,
            },
        }
    }
}

fn installed_git_reading(sys: &dyn System, user_home: Option<&Path>) -> Option<GitInstallation> {
    let native = || installed_git_without_atpkg(sys).map(GitInstallation::Native);
    if let Some(home) = user_home {
        return atpkg_installed_git(sys, home).or_else(native);
    }
    if let Some(home) = sys.account_home()
        && let Some(git) = atpkg_installed_git(sys, &home)
    {
        return Some(git);
    }
    native()
}

fn atpkg_prefix_for(sys: &dyn System, home: &Path) -> PathBuf {
    let cfg = home.join(".config/aterm/aterm.toml");
    if let Ok(bytes) = sys.read_file(&cfg)
        && let Ok(text) = std::str::from_utf8(&bytes)
        && let Some(configured) = aterm_toolchain::configured_prefix_in(text, home)
    {
        return configured;
    }
    if sys.macos() {
        home.join("Library/Application Support/aterm/pkg")
    } else {
        aterm_toolchain::default_atpkg_prefix(home)
    }
}

fn physical(sys: &dyn System, store_git: &Path, current: &Path) -> Option<String> {
    let link = sys.read_link(current).ok()?;
    let build = if link.is_absolute() {
        let rel = link.strip_prefix(store_git).ok()?;
        let mut comps = rel.components();
        let std::path::Component::Normal(name) = comps.next()? else {
            return None;
        };
        if comps.next().is_some() {
            return None;
        }
        name.to_str()?.to_owned()
    } else {
        let mut comps = link.components();
        let std::path::Component::Normal(name) = comps.next()? else {
            return None;
        };
        if comps.next().is_some() {
            return None;
        }
        name.to_str()?.to_owned()
    };
    (!build.is_empty() && build.bytes().all(|b| b.is_ascii_digit())).then_some(build)
}

fn atpkg_installed_git(sys: &dyn System, home: &Path) -> Option<GitInstallation> {
    if !home.is_absolute() {
        return None;
    }
    let prefix = atpkg_prefix_for(sys, home);
    let store_git = prefix.join("store/git");
    let build = physical(sys, &store_git, &store_git.join("current"))?;
    let reg_file = |p: &Path| sys.stat(p).is_ok_and(|s| !s.dir && !s.symlink);
    let exec_file = |p: &Path| {
        sys.stat(p)
            .is_ok_and(|s| !s.dir && !s.symlink && (s.mode & 0o111) != 0)
    };
    if !reg_file(&store_git.join(format!("{build}.ready"))) {
        return None;
    }
    let payload = store_git.join(&build).join("bin/git");
    if !exec_file(&payload) {
        return None;
    }
    sys.admit_runtime(&store_git.join(&build)).ok()?;
    let shim = prefix.join("bin/git");
    exec_file(&shim).then_some(GitInstallation::Managed { shim, payload })
}

/// [`installed_git`], or where none is installed the one it looks for first ([`CLT_GIT`],
/// elsewhere [`SYSTEM_GIT`]; each caller reads that first).
#[must_use]
pub fn git_program(sys: &dyn System) -> PathBuf {
    program_or_default(sys, installed_git(sys))
}

/// The git a command runs as `user`: only that account's managed build or native Git.
#[must_use]
pub fn git_program_for_user(sys: &dyn System, user: &User) -> PathBuf {
    program_or_default(sys, installed_git_for(sys, Some(Path::new(&user.home))))
}

fn program_or_default(sys: &dyn System, installed: Option<PathBuf>) -> PathBuf {
    installed.unwrap_or_else(|| native_git(sys).into())
}

/// The native git [`installed_git_without_atpkg`] looks for first: [`CLT_GIT`] on a Mac,
/// [`SYSTEM_GIT`] elsewhere.
fn native_git(sys: &dyn System) -> &'static str {
    if sys.macos() { CLT_GIT } else { SYSTEM_GIT }
}

/// For a test that runs this host's real native git, as the code it checks does with no managed
/// build: the path of the one [`installed_git_without_atpkg`] looks for first ([`CLT_GIT`] on a
/// Mac, [`SYSTEM_GIT`] elsewhere), which [`git_program`] then runs, or COULD NOT RUN on a host
/// without it (a Mac before the Command Line Tools, a distribution with no git installed), never
/// a spawn error read as a defect.
#[cfg(test)]
pub(crate) fn need_native_git() -> &'static str {
    let git = native_git(&crate::port::Live::new());
    if !Path::new(git).is_file() {
        aterm_uds::host::could_not_run(format!(
            "no {git} here (this host's native git, which this code runs)"
        ));
    }
    git
}

/// [`need_native_git`] for a test of code that reads a clone only where git cannot fetch an
/// object lazily, through `--no-lazy-fetch` or, where git refuses the option, the
/// `GIT_NO_LAZY_FETCH` it was seen to obey ([`crate::checkout::NoLazyFetch::of`], as the code
/// asks): how this host's native git does, or COULD NOT RUN where it does neither (a git before
/// 2.39.4 without the May 2024 security fixes).
///
/// # Panics
/// The question could not be asked of a git that is there (its runs did not start or finish,
/// no scratch folder could be made, or a step setting it up failed): a defect, not this host's
/// lack.
#[cfg(test)]
pub(crate) fn need_native_git_barring_lazy_fetch() -> crate::checkout::NoLazyFetch {
    let git = need_native_git();
    let live = crate::port::Live::new();
    let off = crate::checkout::NoLazyFetch::of_git(&live, &|| crate::port::Cmd::new(git))
        .unwrap_or_else(|why| panic!("{git}: {why}"));
    off.unwrap_or_else(|| {
        let version = live
            .run_complete_stdout(&crate::port::Cmd::new(git).arg("--version"))
            .map_or_else(|_| String::new(), |ran| ran.out());
        aterm_uds::host::could_not_run(format!(
            "{git} ({}) neither takes --no-lazy-fetch nor obeys GIT_NO_LAZY_FETCH, one of \
             which this code needs (git 2.44 has both, the 2.39.4 security release the variable)",
            version.trim()
        ))
    })
}

/// The item git's settings wait for.
pub const CLT_ITEM: &str = "developer.command-line-tools";

/// `ssh-keygen`, a real binary on every macOS (not a shim).
pub const SSH_KEYGEN: &str = aterm_uds::os_path!("ssh-keygen");

/// Reads a user's file.
const CAT: &str = aterm_uds::os_path!("cat");

/// Makes `~/.ssh`.
const MKDIR: &str = aterm_uds::os_path!("mkdir");

/// Removes the key, for the revert line.
const RM: &str = aterm_uds::os_path!("rm");

/// The git config key that rewrites `https://github.com/` to SSH.
const SSH_URL_KEY: &str = "url.git@github.com:.insteadOf";

/// The prefix rewritten.
const HTTPS_PREFIX: &str = "https://github.com/";

/// `path` inside `user`'s home.
fn in_home(user: &User, rel: &str) -> String {
    format!("{}/{rel}", user.home.trim_end_matches('/'))
}

/// `user`'s private key.
#[must_use]
pub fn key_path(user: &User) -> String {
    in_home(user, KEY_FILE)
}

/// `user`'s public key.
#[must_use]
pub fn pub_path(user: &User) -> String {
    format!("{}.pub", key_path(user))
}

/// The comment the key carries and the title the controller registers it under.
#[must_use]
pub fn key_title(host: &str, user: &str) -> String {
    format!("aterm-machines:{host}:{user}")
}

/// Extracts the key type and base64 body of a valid OpenSSH public key line: a blob whose
/// first field names the line's type and that carries at least one non-empty key field after it.
#[must_use]
pub fn key_material(line: &str) -> Option<(String, String)> {
    let mut words = line.split_whitespace();
    let kind = words.next()?;
    let body = words.next()?;
    let blob = aterm_codec::base64::decode(body).ok()?;
    let (named, rest) = ssh_field(&blob)?;
    let (key, _) = ssh_field(rest)?;
    (named == kind.as_bytes() && !key.is_empty()).then(|| (kind.to_owned(), body.to_owned()))
}

/// One SSH wire string (RFC 4251 §5), a big-endian `u32` length and that many bytes, and the
/// bytes after it; `None` when `bytes` holds less than its length says.
fn ssh_field(bytes: &[u8]) -> Option<(&[u8], &[u8])> {
    let len = usize::try_from(u32::from_be_bytes(bytes.get(..4)?.try_into().ok()?)).ok()?;
    let end = len.checked_add(4)?;
    Some((bytes.get(4..end)?, bytes.get(end..)?))
}

/// `path`'s text, read as `user`: `Absent` when it does not exist.
fn read_as(sys: &dyn System, user: &User, path: &str) -> Read<String> {
    match sys.run_complete_stdout(&Cmd::new(CAT).arg(path).as_user(user)) {
        Ok(ran) if ran.success() => match String::from_utf8(ran.stdout) {
            Ok(text) => Read::Value(text),
            Err(_) => Read::Unusable(format!(
                "reading {path}: file text is not UTF-8; left unchanged"
            )),
        },
        Ok(ran) if ran.err().contains("No such file or directory") => Read::Absent,
        Ok(ran) => Read::Unusable(format!("reading {path}: {}", ran.describe())),
        Err(e) => Read::Unusable(format!("reading {path}: {e}")),
    }
}

/// Make `~/.ssh` (0700) as `user`, when it is missing.
fn ensure_ssh_dir(sys: &dyn System, user: &User) -> Result<(), String> {
    let dir = in_home(user, ".ssh");
    let cmd = Cmd::new(MKDIR)
        .args(["-p", "-m", "700"])
        .arg(&dir)
        .as_user(user);
    match sys.run(&cmd) {
        Ok(ran) if ran.success() => Ok(()),
        Ok(ran) => Err(format!("mkdir {dir}: {}", ran.describe())),
        Err(e) => Err(format!("mkdir {dir}: {e}")),
    }
}

/// A file at `path` that is a link.
fn linked(sys: &dyn System, path: &str) -> Option<String> {
    match sys.stat(Path::new(path)) {
        Ok(s) if s.symlink => Some(format!(
            "{path} is a link; this family writes only a plain file (edit its target by hand)"
        )),
        _ => None,
    }
}

/// `~/.ssh/aterm_github_ed25519`, made by `ssh-keygen` as the user when missing.
struct KeyItem {
    user: User,
    host: String,
}

/// What the key's files hold.
enum KeyState {
    /// Both halves, and the public one parses.
    Whole(String),
    /// No private key.
    Missing,
    /// A private key without a usable public half.
    NoPublic,
}

impl KeyItem {
    fn state(&self, sys: &dyn System) -> Result<KeyState, Probe> {
        let key = key_path(&self.user);
        match sys.stat(Path::new(&key)) {
            Ok(s) if s.symlink || s.dir => {
                return Err(Probe::Blocked {
                    why: format!("{key} is not a plain file; refusing to touch it"),
                });
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(KeyState::Missing),
            Err(e) => {
                return Err(Probe::Unusable {
                    why: format!("{key}: {e}"),
                });
            }
        }
        match read_as(sys, &self.user, &pub_path(&self.user)) {
            Read::Value(text) => {
                let line = text.trim();
                Ok(match key_material(line) {
                    Some((kind, _)) if kind == "ssh-ed25519" => KeyState::Whole(line.to_owned()),
                    _ => KeyState::NoPublic,
                })
            }
            Read::Absent => Ok(KeyState::NoPublic),
            Read::Unusable(why) => Err(Probe::Unusable { why }),
        }
    }
}

impl Item for KeyItem {
    fn id(&self) -> String {
        format!("github.key@{}", self.user.name)
    }

    fn family(&self) -> &'static str {
        "github"
    }

    fn title(&self) -> String {
        format!("GitHub SSH key for {}", self.user.name)
    }

    fn probe(&self, sys: &dyn System, _ctx: &Ctx) -> Probe {
        match self.state(sys) {
            Ok(KeyState::Whole(_)) => Probe::Ok {
                now: format!("~/{KEY_FILE}"),
            },
            Ok(KeyState::Missing) => Probe::Change {
                from: "absent".into(),
                to: format!("a new ed25519 key at ~/{KEY_FILE}"),
            },
            Ok(KeyState::NoPublic) => Probe::Change {
                from: "a private key without its .pub".into(),
                to: "its .pub, derived from the private key".into(),
            },
            Err(probe) => probe,
        }
    }

    fn apply(&self, sys: &dyn System, _ctx: &Ctx) -> Result<(), String> {
        ensure_ssh_dir(sys, &self.user)?;
        let key = key_path(&self.user);
        match self.state(sys) {
            Ok(KeyState::Whole(_)) => Ok(()),
            Ok(KeyState::Missing) => {
                let cmd = Cmd::new(SSH_KEYGEN)
                    .args(["-t", "ed25519", "-N", "", "-C"])
                    .arg(key_title(&self.host, &self.user.name))
                    .arg("-f")
                    .arg(&key)
                    .as_user(&self.user);
                match sys.run(&cmd) {
                    Ok(ran) if ran.success() => Ok(()),
                    Ok(ran) => Err(format!("ssh-keygen: {}", ran.describe())),
                    Err(e) => Err(format!("ssh-keygen: {e}")),
                }
            }
            Ok(KeyState::NoPublic) => {
                let cmd = Cmd::new(SSH_KEYGEN)
                    .args(["-y", "-f"])
                    .arg(&key)
                    .as_user(&self.user);
                let line = match sys.run(&cmd) {
                    Ok(ran) if ran.success() => ran.out().trim().to_owned(),
                    Ok(ran) => return Err(format!("ssh-keygen -y: {}", ran.describe())),
                    Err(e) => return Err(format!("ssh-keygen -y: {e}")),
                };
                let Some((kind, body)) = key_material(&line) else {
                    return Err(format!("ssh-keygen -y printed {line:?}, not a public key"));
                };
                let title = key_title(&self.host, &self.user.name);
                let text = format!("{kind} {body} {title}\n");
                sys.write_user_file(
                    &self.user,
                    Path::new(&pub_path(&self.user)),
                    text.as_bytes(),
                    0o644,
                )
                .map_err(|e| format!("{}: {e}", pub_path(&self.user)))
            }
            Err(Probe::Blocked { why } | Probe::Unusable { why }) => Err(why),
            Err(other) => Err(format!("{other:?}")),
        }
    }

    fn revert(&self) -> Option<String> {
        Some(
            Cmd::new(RM)
                .args(["-f", &key_path(&self.user), &pub_path(&self.user)])
                .as_user(&self.user)
                .display(),
        )
    }

    /// The public key line, which the controller registers.
    fn detail(&self, sys: &dyn System, _ctx: &Ctx) -> Option<String> {
        match self.state(sys) {
            Ok(KeyState::Whole(line)) => Some(line),
            _ => None,
        }
    }
}

/// Where a block goes in its file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Place {
    /// First: `~/.ssh/config` takes the first value it reads for each option.
    Top,
    /// Last: `known_hosts` lines are read in any order.
    Bottom,
}

/// The block in `text`, and the text without it.
fn split_block(text: &str) -> Result<(Option<String>, String), String> {
    let mut lines = text.split_inclusive('\n');
    let mut rest = String::new();
    let mut block: Option<String> = None;
    while let Some(line) = lines.next() {
        if line.trim_end() != BLOCK_BEGIN || block.is_some() {
            rest.push_str(line);
            continue;
        }
        let mut found = line.to_owned();
        let mut closed = false;
        for inner in lines.by_ref() {
            found.push_str(inner);
            if inner.trim_end() == BLOCK_END {
                closed = true;
                break;
            }
        }
        if !closed {
            return Err(format!("`{BLOCK_BEGIN}` has no `{BLOCK_END}` after it"));
        }
        block = Some(found);
    }
    Ok((block, rest))
}

/// `rest` with `block` put back at `place`.
fn join_block(rest: &str, block: &str, place: Place) -> String {
    match place {
        Place::Top => format!("{block}{rest}"),
        Place::Bottom if rest.is_empty() || rest.ends_with('\n') => format!("{rest}{block}"),
        Place::Bottom => format!("{rest}\n{block}"),
    }
}

/// The `~/.ssh/config` block.
#[must_use]
pub fn ssh_config_block() -> String {
    format!(
        "{BLOCK_BEGIN}\n\
         # Written by aterm machines; the lines between these markers are replaced.\n\
         Host github.com\n\
         \x20 HostName github.com\n\
         \x20 User git\n\
         \x20 IdentityFile ~/{KEY_FILE}\n\
         \x20 IdentitiesOnly yes\n\
         Host *\n\
         {BLOCK_END}\n"
    )
}

/// The `~/.ssh/known_hosts` block: GitHub's three host keys.
#[must_use]
pub fn known_hosts_block() -> String {
    let mut out = format!("{BLOCK_BEGIN}\n");
    for key in GITHUB_HOST_KEYS {
        out.push_str("github.com ");
        out.push_str(key);
        out.push('\n');
    }
    out.push_str(BLOCK_END);
    out.push('\n');
    out
}

/// A marked block in one of the user's files.
struct BlockItem {
    user: User,
    /// `ssh-config` or `known-hosts`.
    what: &'static str,
    /// Relative to the home.
    rel: &'static str,
    block: String,
    place: Place,
    /// The mode of a file this item creates.
    mode: u32,
}

impl BlockItem {
    fn path(&self) -> String {
        in_home(&self.user, self.rel)
    }

    /// The file's current text (empty when absent), or the probe that stops here.
    fn current(&self, sys: &dyn System) -> Result<(Option<String>, String, bool), Probe> {
        let path = self.path();
        if let Some(why) = linked(sys, &path) {
            return Err(Probe::Blocked { why });
        }
        let (text, exists) = match read_as(sys, &self.user, &path) {
            Read::Value(t) => (t, true),
            Read::Absent => (String::new(), false),
            Read::Unusable(why) => return Err(Probe::Unusable { why }),
        };
        let (block, rest) = split_block(&text).map_err(|e| Probe::Blocked {
            why: format!("{path}: {e}; fix it by hand"),
        })?;
        Ok((block, rest, exists))
    }
}

impl Item for BlockItem {
    fn id(&self) -> String {
        format!("github.{}@{}", self.what, self.user.name)
    }

    fn family(&self) -> &'static str {
        "github"
    }

    fn title(&self) -> String {
        match self.what {
            "ssh-config" => format!("github.com in {}'s ~/{}", self.user.name, self.rel),
            _ => format!("GitHub's host keys in {}'s ~/{}", self.user.name, self.rel),
        }
    }

    fn probe(&self, sys: &dyn System, _ctx: &Ctx) -> Probe {
        match self.current(sys) {
            Ok((Some(found), _, _)) if found == self.block => Probe::Ok {
                now: "the marked block".into(),
            },
            Ok((Some(_), _, _)) => Probe::Change {
                from: "a different marked block".into(),
                to: "the marked block".into(),
            },
            Ok((None, _, exists)) => Probe::Change {
                from: if exists { "no marked block" } else { "absent" }.into(),
                to: "the marked block".into(),
            },
            Err(probe) => probe,
        }
    }

    fn apply(&self, sys: &dyn System, _ctx: &Ctx) -> Result<(), String> {
        ensure_ssh_dir(sys, &self.user)?;
        let (_, rest, exists) = match self.current(sys) {
            Ok(c) => c,
            Err(Probe::Blocked { why } | Probe::Unusable { why }) => return Err(why),
            Err(other) => return Err(format!("{other:?}")),
        };
        let path = self.path();
        let mode = match sys.stat(Path::new(&path)) {
            Ok(s) if exists && !s.symlink && !s.dir => s.mode & 0o777,
            _ => self.mode,
        };
        let text = join_block(&rest, &self.block, self.place);
        sys.write_user_file(&self.user, Path::new(&path), text.as_bytes(), mode)
            .map_err(|e| format!("{path}: {e}"))
    }

    fn revert(&self) -> Option<String> {
        Some(format!(
            "delete the lines from `{BLOCK_BEGIN}` to `{BLOCK_END}` in {}",
            self.path()
        ))
    }
}

/// One `git config --global` value.
struct GitItem {
    user: User,
    /// `git-name`, `git-email`, `ssh-urls`.
    what: &'static str,
    key: &'static str,
    value: String,
    /// `--add` to a multi-valued key (`insteadOf`) instead of replacing it.
    add: bool,
    /// The git the last probe found ([`installed_git`]).
    program: Mutex<PathBuf>,
}

impl GitItem {
    fn git(&self) -> Cmd {
        Cmd::new(&*self.program.lock().unwrap_or_else(PoisonError::into_inner))
            .args(["config", "--global"])
            .as_user(&self.user)
    }

    fn read(&self, sys: &dyn System) -> Read<Vec<String>> {
        let get = if self.add { "--get-all" } else { "--get" };
        match sys.run(&self.git().args([get, self.key])) {
            Ok(ran) if ran.success() => Read::Value(ran.out().lines().map(str::to_owned).collect()),
            Ok(ran) if ran.status == Some(1) && ran.stdout.is_empty() && ran.stderr.is_empty() => {
                Read::Absent
            }
            Ok(ran) => Read::Unusable(format!("git config {get} {}: {}", self.key, ran.describe())),
            Err(e) => Read::Unusable(format!("git config {get} {}: {e}", self.key)),
        }
    }
}

impl Item for GitItem {
    fn id(&self) -> String {
        format!("github.{}@{}", self.what, self.user.name)
    }

    fn family(&self) -> &'static str {
        "github"
    }

    fn title(&self) -> String {
        format!("git {} for {}", self.key, self.user.name)
    }

    fn probe(&self, sys: &dyn System, _ctx: &Ctx) -> Probe {
        let Some(program) = installed_git_for(sys, Some(Path::new(&self.user.home))) else {
            return Probe::Waits {
                on: CLT_ITEM.to_owned(),
                why: format!(
                    "the Command Line Tools are not installed ({CLT_GIT} is missing, and no \
                     Xcode is selected), and /usr/bin/git would ask a person to install them: \
                     set [developer] command_line_tools = true"
                ),
            };
        };
        *self.program.lock().unwrap_or_else(PoisonError::into_inner) = program;
        match self.read(sys) {
            Read::Value(values)
                if values.contains(&self.value) && (self.add || values.len() == 1) =>
            {
                Probe::Ok {
                    now: self.value.clone(),
                }
            }
            Read::Value(values) => Probe::Change {
                from: values.join(", "),
                to: self.value.clone(),
            },
            Read::Absent => Probe::Change {
                from: "absent".into(),
                to: self.value.clone(),
            },
            Read::Unusable(why) => Probe::Unusable { why },
        }
    }

    fn apply(&self, sys: &dyn System, _ctx: &Ctx) -> Result<(), String> {
        let cmd = if self.add {
            self.git().args(["--add", self.key]).arg(&self.value)
        } else {
            self.git().arg(self.key).arg(&self.value)
        };
        match sys.run(&cmd) {
            Ok(ran) if ran.success() => Ok(()),
            Ok(ran) => Err(format!("git config {}: {}", self.key, ran.describe())),
            Err(e) => Err(format!("git config {}: {e}", self.key)),
        }
    }

    fn revert(&self) -> Option<String> {
        let unset = if self.add {
            self.git().args(["--unset", self.key]).arg(&self.value)
        } else {
            self.git().args(["--unset", self.key])
        };
        Some(unset.display())
    }

    fn after(&self) -> Vec<String> {
        vec![CLT_ITEM.to_owned()]
    }
}

/// The items `table` asks for, per user in `ctx.users`.
///
/// # Errors
/// One line naming what cannot be expanded.
pub fn expand(table: &Table, ctx: &Ctx) -> Result<Vec<Box<dyn Item>>, String> {
    let mut items: Vec<Box<dyn Item>> = Vec::new();
    for user in &ctx.users {
        let fill = |s: &str| {
            crate::baseline::fill_str(
                s,
                &[
                    ("user", user.name.as_str()),
                    ("name", ctx.host_name.as_str()),
                ],
            )
        };
        if table.key == Some(true) {
            items.push(Box::new(KeyItem {
                user: user.clone(),
                host: ctx.host_name.clone(),
            }));
            items.push(Box::new(BlockItem {
                user: user.clone(),
                what: "ssh-config",
                rel: ".ssh/config",
                block: ssh_config_block(),
                place: Place::Top,
                mode: 0o600,
            }));
            items.push(Box::new(BlockItem {
                user: user.clone(),
                what: "known-hosts",
                rel: ".ssh/known_hosts",
                block: known_hosts_block(),
                place: Place::Bottom,
                mode: 0o644,
            }));
        }
        for (what, key, value) in [
            ("git-name", "user.name", &table.git_name),
            ("git-email", "user.email", &table.git_email),
        ] {
            let Some(value) = value else { continue };
            let value = fill(value);
            if value.trim().is_empty() || value.contains('\n') {
                return Err(format!("[github] {key} is {value:?} for {}", user.name));
            }
            items.push(Box::new(GitItem {
                user: user.clone(),
                what,
                key,
                value,
                add: false,
                program: Mutex::new(CLT_GIT.into()),
            }));
        }
        if table.ssh_urls == Some(true) {
            items.push(Box::new(GitItem {
                user: user.clone(),
                what: "ssh-urls",
                key: SSH_URL_KEY,
                value: HTTPS_PREFIX.to_owned(),
                add: true,
                program: Mutex::new(CLT_GIT.into()),
            }));
        }
    }
    Ok(items)
}

#[cfg(test)]
#[path = "github_tests.rs"]
mod tests;
