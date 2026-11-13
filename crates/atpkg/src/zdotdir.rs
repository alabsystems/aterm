// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The folder zsh reads its `.zshrc` from: `$ZDOTDIR`, else `$HOME`.
//!
//! zsh reads `$ZDOTDIR/.zshrc`, and never `~/.zshrc`, when `ZDOTDIR` is set. A pass run from a
//! shell sees `ZDOTDIR` only when that shell exported it, and a pass the app runs from Finder or
//! launchd sees no shell's environment at all (round 13 of the install audits). zsh itself takes
//! `ZDOTDIR` from its environment and then from the zshenv files it reads before any other
//! (`/etc/zshenv`, then `$ZDOTDIR/.zshenv`, which is `~/.zshenv` while it is unset), the files a
//! person sets it in. So with none in this process's environment the answer is zsh's own: with
//! no zshenv file nothing sets it and the folder is `$HOME`; with one, zsh runs them
//! (`zsh -c`, which reads the zshenv files and no other startup file) and prints the `ZDOTDIR`
//! they leave. A `ZDOTDIR` first set in a `.zprofile` or `/etc/zshrc` is not seen.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// An absolute spelling with `.` and `..` reduced, before any filesystem query. A later walk
/// still reads the original components: reducing `link/..` must not erase a link's meaning.
fn lexical(path: &Path) -> Option<PathBuf> {
    use std::path::Component;
    if !path.is_absolute() {
        return None;
    }
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                out.push(component.as_os_str())
            }
        }
    }
    Some(out)
}

/// Relative components to walk; roots and prefixes can enter only through an admitted home.
fn relative_parts(path: &Path) -> Option<std::collections::VecDeque<OsString>> {
    use std::path::Component;
    path.components()
        .filter_map(|component| match component {
            Component::CurDir => None,
            Component::Normal(part) => Some(Some(part.to_os_string())),
            Component::ParentDir => Some(Some(OsString::from(".."))),
            Component::RootDir | Component::Prefix(_) => Some(None),
        })
        .collect()
}

/// Prove an inherited path stays under a redirected home without first following an escaping
/// target. Only HOME itself is canonicalized; descendant links are read as text before their
/// targets are walked. Missing descendants are allowed, and a `..` returning to an existing
/// prefix resumes checked walking there. A link cycle or unreadable component proves nothing.
fn under_home(
    home: &Path,
    dir: &Path,
    home_spelling: &Path,
    named: &Path,
    canonical_home: Option<&Path>,
) -> bool {
    let root = canonical_home.unwrap_or(home_spelling);
    if !named.starts_with(home_spelling) && !named.starts_with(root) {
        return false;
    }
    let relative = |path: &Path| {
        path.strip_prefix(home)
            .or_else(|_| path.strip_prefix(home_spelling))
            .or_else(|_| path.strip_prefix(root))
            .ok()
            .map(Path::to_path_buf)
    };
    let Some(mut pending) = relative(dir).and_then(|path| relative_parts(&path)) else {
        return false;
    };
    let mut resolved = root.to_path_buf();
    let mut links = 0;
    while let Some(part) = pending.pop_front() {
        if part == ".." {
            if resolved == root {
                return false;
            }
            resolved.pop();
            continue;
        }
        let next = resolved.join(&part);
        match std::fs::symlink_metadata(&next) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                links += 1;
                if links > 32 {
                    return false;
                }
                let Ok(target) = std::fs::read_link(&next) else {
                    return false;
                };
                let target_relative = if target.is_absolute() {
                    let Some(normalized) = lexical(&target) else {
                        return false;
                    };
                    if !normalized.starts_with(home_spelling) && !normalized.starts_with(root) {
                        return false;
                    }
                    let Some(path) = relative(&target) else {
                        return false;
                    };
                    resolved = root.to_path_buf();
                    path
                } else {
                    let Some(normalized) = lexical(&resolved.join(&target)) else {
                        return false;
                    };
                    if !normalized.starts_with(root) {
                        return false;
                    }
                    target
                };
                let Some(mut parts) = relative_parts(&target_relative) else {
                    return false;
                };
                parts.append(&mut pending);
                pending = parts;
            }
            Ok(metadata) if metadata.is_dir() => resolved = next,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => resolved = next,
            Ok(_) | Err(_) => return false,
        }
    }
    true
}

/// Resolve a zshenv path without querying a protected target first. Metadata and link reads
/// are injected so a scratch test can deny a target query, rather than merely observing that
/// the later shell callback was skipped.
fn zshenv_safe(
    home: &Path,
    file: &Path,
    metadata: impl FnMut(&Path) -> std::io::Result<std::fs::Metadata>,
    read_link: impl FnMut(&Path) -> std::io::Result<PathBuf>,
) -> Result<(), String> {
    let canonical_home = std::fs::canonicalize(home).unwrap_or_else(|_| home.to_path_buf());
    let (_, metadata) = crate::protected::resolve_path(&canonical_home, file, metadata, read_link)
        .map_err(|why| why.to_string())?;
    if metadata.is_some_and(|meta| meta.is_file()) {
        Ok(())
    } else {
        Err("it is not a regular zshenv file".to_owned())
    }
}

/// Where zsh reads `.zshrc` from.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum ZshDir {
    /// `$HOME`: nothing sets `ZDOTDIR`, or it names `$HOME`.
    #[default]
    Home,
    /// `$ZDOTDIR`, a folder other than `$HOME`.
    At(PathBuf),
    /// Not learned, and why, in words for the person.
    Unknown(String),
}

impl ZshDir {
    /// The folder under `home`'s account, or `None` when it is [`ZshDir::Unknown`].
    #[must_use]
    pub fn dir(&self, home: &Path) -> Option<PathBuf> {
        match self {
            Self::Home => Some(home.to_path_buf()),
            Self::At(dir) => Some(dir.clone()),
            Self::Unknown(_) => None,
        }
    }
}

/// The system zshenv files zsh reads before `$ZDOTDIR/.zshenv` (`/etc/zsh/zshenv` is
/// Debian's).
pub(crate) const SYSTEM_ZSHENV: [&str; 2] = ["/etc/zshenv", "/etc/zsh/zshenv"];

/// How long zsh's zshenv files get to answer: the login-PATH probe's budget.
#[cfg(not(test))]
const ASK_BUDGET: std::time::Duration = std::time::Duration::from_secs(5);
/// Unit tests: the hang detector, since every zsh these tests ask answers.
#[cfg(test)]
const ASK_BUDGET: std::time::Duration = aterm_spec::wait::MUST_HAPPEN;

/// TEST SEAM ([`aterm_types::dev_seam!`]): a count of milliseconds, [`ask_zsh`] waits that long
/// for zsh where it is longer than [`ASK_BUDGET`], so a test whose dev `atpkg` must hear zsh's
/// answer waits out only its hang detector on a loaded machine.
pub const ASK_WITHIN_ENV: &str = "ATPKG_ZSH_ASK_WITHIN_MS";

/// [`ask_zsh`]'s wait for zsh: [`ASK_BUDGET`], or the longer [`ASK_WITHIN_ENV`] a debug build
/// was given.
fn ask_within() -> std::time::Duration {
    aterm_types::dev_seam!(ASK_WITHIN_ENV)
        .and_then(|ms| ms.to_str()?.parse::<u64>().ok())
        .map_or(ASK_BUDGET, |ms| {
            ASK_BUDGET.max(std::time::Duration::from_millis(ms))
        })
}

/// The answer marker's prefix; each query adds a fresh nonce to distinguish startup output.
const MARK: &str = "atpkg-zdotdir:";

/// An environment's `ZDOTDIR` as `home`'s zsh folder: an absolute path at or under `home`, or
/// anywhere when `home` is `account`, the account's own home as the OS records it. Empty or
/// relative, it names no folder. A process whose `HOME` was pointed away from the account's home
/// (a scratch install, a test's home) inherits the `ZDOTDIR` of the shell that started it, which
/// is the account's own, so outside the redirected `home` it is not taken: wiring that folder's
/// `.zshrc` would edit the account's real shell from a scratch run. zsh's own answer
/// ([`resolve`]) still finds a `ZDOTDIR` outside `home` that `home`'s zshenv files set.
#[must_use]
pub(crate) fn from_env(
    home: &Path,
    account: Option<&Path>,
    value: Option<OsString>,
) -> Option<PathBuf> {
    let dir = value
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())?;
    let home_spelling = lexical(home)?;
    let named = lexical(&dir)?;
    if account.is_some_and(|account| account == home) {
        return Some(dir);
    }
    // HOME and the OS account record are admitted roots. Never canonicalize the inherited
    // candidate: a descendant link can name a protected outside directory before it is refused.
    let canonical_home = std::fs::canonicalize(home).ok();
    let own_account = account.is_some_and(|account| {
        std::fs::canonicalize(account)
            .ok()
            .zip(canonical_home.as_ref())
            .is_some_and(|(account, home)| account == *home)
    });
    (own_account
        || under_home(
            home,
            &dir,
            &home_spelling,
            &named,
            canonical_home.as_deref(),
        ))
    .then_some(dir)
}

/// The rule: `env`, this process's `ZDOTDIR` as [`from_env`] takes it, when there is one; else
/// `$HOME` when no zshenv file exists (`zshenv`, the ones that do, in zsh's order); else `ask`, zsh's own run of
/// those files, unless one resolves under a folder macOS guards with a consent dialog, which no
/// atpkg pass opens (the rc consent fence).
pub(crate) fn resolve(
    home: &Path,
    env: Option<PathBuf>,
    zshenv: &[PathBuf],
    ask: impl FnOnce() -> Result<PathBuf, String>,
) -> ZshDir {
    if let Some(dir) = env {
        return named(home, dir);
    }
    if zshenv.is_empty() {
        return ZshDir::Home;
    }
    for file in zshenv {
        if let Err(why) = zshenv_safe(
            home,
            file,
            |path| std::fs::symlink_metadata(path),
            |path| std::fs::read_link(path),
        ) {
            return ZshDir::Unknown(format!(
                "{}: {why}; zsh was not asked which folder it reads .zshrc from",
                file.display()
            ));
        }
    }
    match ask() {
        Err(why) => ZshDir::Unknown(why),
        Ok(value) if value.as_os_str().is_empty() => ZshDir::Home,
        Ok(value) if value.is_absolute() => named(home, value),
        Ok(value) => ZshDir::Unknown(format!(
            "zsh's zshenv files set ZDOTDIR to {value:?}, a relative path that names no fixed \
             folder"
        )),
    }
}

fn named(home: &Path, dir: PathBuf) -> ZshDir {
    if dir == home {
        ZshDir::Home
    } else {
        ZshDir::At(dir)
    }
}

/// The zshenv paths under `home`'s account not known to be missing, in zsh's order.
/// Inspection errors stay in the inventory so [`resolve`] can refuse an unknown startup path
/// instead of assuming no file sets `ZDOTDIR` and wiring the wrong `.zshrc`.
#[must_use]
pub(crate) fn zshenv_files(home: &Path) -> Vec<PathBuf> {
    SYSTEM_ZSHENV
        .iter()
        .map(PathBuf::from)
        .chain(std::iter::once(home.join(".zshenv")))
        .filter(|file| {
            !matches!(
                std::fs::symlink_metadata(file),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound
            )
        })
        .collect()
}

/// The zsh that answers: the account's login shell when it is zsh, else the system's.
fn account_zsh() -> Option<PathBuf> {
    crate::hooks::login_shell()
        .filter(|shell| shell.file_name().is_some_and(|name| name == "zsh"))
        .or_else(|| {
            ["/bin/zsh", "/usr/bin/zsh"]
                .into_iter()
                .map(PathBuf::from)
                .find(|zsh| zsh.is_file())
        })
}

/// `zsh -c` in `home` with no `ZDOTDIR` in its environment: the `ZDOTDIR` its zshenv files
/// leave (empty when they set none), within [`ask_within`].
///
/// # Errors
/// Why there is no answer: zsh did not start, did not finish in time, printed no complete reply,
/// or named a path with an embedded NUL or bytes this platform cannot represent.
pub(crate) fn ask_zsh(zsh: &Path, home: &Path) -> Result<PathBuf, String> {
    let nonce = aterm_uds::rand::hex_token::<16>()
        .map_err(|error| format!("no zsh reply marker: {error}"))?;
    let begin = format!("{MARK}{nonce}:begin");
    let end = format!("{MARK}{nonce}:end");
    let mut cmd = aterm_uds::spawnfd::command(zsh);
    cmd.arg("-c")
        .arg(format!(
            "builtin setopt nomultibyte; builtin printf '\\0%s\\0%d\\0%s\\0%s\\0' \
             '{begin}' \"${{#${{ZDOTDIR-}}}}\" \"${{ZDOTDIR-}}\" '{end}'"
        ))
        .env_remove("ZDOTDIR")
        .env("HOME", home)
        .current_dir(home);
    let out = aterm_uds::deadline::output_within(&mut cmd, ask_within()).map_err(|e| {
        format!(
            "{} -c, asked which folder zsh reads .zshrc from, {e}",
            zsh.display()
        )
    })?;
    // A per-query nonce separates this reply from ordinary startup chatter. Its byte count
    // binds the full scalar, so
    // an embedded NUL (even followed by a closing marker) cannot become a shorter pathname.
    let begin = format!("\0{begin}\0").into_bytes();
    let end = format!("\0{end}\0").into_bytes();
    let no_answer = || {
        format!(
            "{} -c printed no ZDOTDIR (a zshenv file ended it first, or its reply was incomplete)",
            zsh.display()
        )
    };
    let start = out
        .stdout
        .windows(begin.len())
        .position(|bytes| bytes == begin)
        .ok_or_else(no_answer)?;
    let tail = &out.stdout[start + begin.len()..];
    let count_end = tail
        .iter()
        .position(|&byte| byte == 0)
        .ok_or_else(no_answer)?;
    let count = std::str::from_utf8(&tail[..count_end])
        .ok()
        .filter(|count| !count.is_empty() && count.bytes().all(|byte| byte.is_ascii_digit()))
        .and_then(|count| count.parse::<usize>().ok())
        .ok_or_else(no_answer)?;
    let tail = &tail[count_end + 1..];
    let value = tail.get(..count).ok_or_else(no_answer)?;
    if value.contains(&0) {
        return Err(format!(
            "{} -c printed an invalid ZDOTDIR containing NUL",
            zsh.display()
        ));
    }
    if !tail[count..].starts_with(&end) {
        return Err(no_answer());
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt as _;
        Ok(PathBuf::from(OsString::from_vec(value.to_vec())))
    }
    #[cfg(not(unix))]
    {
        std::str::from_utf8(value)
            .map(PathBuf::from)
            .map_err(|_| "zsh printed a pathname this platform cannot represent".to_owned())
    }
}

/// zsh's folder for this process's account under `home`: [`resolve`] with this process's
/// `ZDOTDIR` ([`from_env`]) and the account's zsh, once per process and home. A test binary
/// ([`aterm_types::dirs::test_root`]) never runs the runner's zshenv files and takes its
/// environment's `ZDOTDIR` only under its `$HOME`, as if that home were redirected: with none
/// there, its answer is `$HOME`.
#[must_use]
pub fn of_process(home: &Path) -> ZshDir {
    static ANSWER: std::sync::Mutex<Option<(PathBuf, ZshDir)>> = std::sync::Mutex::new(None);
    let mut answer = ANSWER
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some((asked, dir)) = answer.as_ref()
        && asked == home
    {
        return dir.clone();
    }
    let (zshenv, account) = if aterm_types::dirs::test_root().is_some() {
        (Vec::new(), None)
    } else {
        (zshenv_files(home), crate::platform::account_home())
    };
    let env = from_env(home, account.as_deref(), std::env::var_os("ZDOTDIR"));
    let dir = resolve(home, env, &zshenv, || {
        let zsh = account_zsh()
            .ok_or_else(|| "no zsh on this machine to run its zshenv files".to_owned())?;
        ask_zsh(&zsh, home)
    });
    *answer = Some((home.to_path_buf(), dir.clone()));
    dir
}

#[cfg(test)]
#[path = "zdotdir_tests.rs"]
mod tests;
