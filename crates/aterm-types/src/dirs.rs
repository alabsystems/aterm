// Copyright 2026 Andrew Yates, Inc.
// SPDX-License-Identifier: Apache-2.0

//! Platform directory resolution from environment variables and platform conventions, with
//! no external dependencies.

use std::ffi::OsStr;
use std::path::PathBuf;

/// Return the user's home directory.
#[must_use]
pub fn home_dir() -> Option<PathBuf> {
    #[cfg(unix)]
    {
        home_from_env(std::env::var_os("HOME").as_deref())
            .or_else(|| passwd_home_dir(current_uid()))
    }
    #[cfg(windows)]
    {
        std::env::var_os("USERPROFILE")
            .map(PathBuf::from)
            .filter(|home| home.is_absolute())
    }
    #[cfg(not(any(unix, windows)))]
    {
        None
    }
}

#[cfg(unix)]
fn current_uid() -> u32 {
    // SAFETY: `getuid()` is always safe — no failure mode, no args.
    unsafe { libc_getuid() }
}

#[cfg(unix)]
unsafe extern "C" {
    #[link_name = "getuid"]
    fn libc_getuid() -> u32;
}

/// `$HOME` as a home, or `None`: an empty or relative value (which would make every path
/// cwd-relative) falls through to the passwd lookup like an unset one.
#[cfg(unix)]
fn home_from_env(value: Option<&std::ffi::OsStr>) -> Option<PathBuf> {
    value.map(PathBuf::from).filter(|home| home.is_absolute())
}

/// Development seam ([`crate::dev_seam!`]) naming [`home_dir`]'s passwd fallback file.
pub const PASSWD_FILE_SEAM: &str = "ATERM_PASSWD_FILE";

/// The home directory for `uid` from `/etc/passwd`, read as bytes: the format needs no UTF-8
/// decode, and a stray legacy byte in another user's GECOS field cannot fail the lookup.
#[cfg(unix)]
fn passwd_home_dir(uid: u32) -> Option<PathBuf> {
    use std::io::Read as _;
    const MAX_PASSWD_BYTES: u64 = 16 * 1024 * 1024;
    let (file, meta) =
        crate::fs_restricted::open_regular_nonblock_with_meta(&passwd_file()).ok()?;
    if meta.len() > MAX_PASSWD_BYTES {
        return None;
    }
    let mut contents =
        Vec::with_capacity(usize::try_from(meta.len().saturating_add(1)).unwrap_or(0));
    file.take(MAX_PASSWD_BYTES + 1)
        .read_to_end(&mut contents)
        .ok()?;
    if contents.len() as u64 > MAX_PASSWD_BYTES {
        return None;
    }
    home_from_passwd(&contents, uid)
}

/// The account file [`passwd_home_dir`] reads: `/etc/passwd`, or in a development build
/// the file [`PASSWD_FILE_SEAM`] names when it is set.
#[cfg(unix)]
fn passwd_file() -> PathBuf {
    crate::dev_seam!(PASSWD_FILE_SEAM).map_or_else(|| PathBuf::from("/etc/passwd"), PathBuf::from)
}

/// Extract the home dir for `uid` from passwd-format bytes.
#[cfg(unix)]
#[cfg_attr(trust_verify, trust::skip)]
fn home_from_passwd(contents: &[u8], uid: u32) -> Option<PathBuf> {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;
    let uid_str = uid.to_string();
    for line in contents.split(|&b| b == b'\n') {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        let fields: Vec<&[u8]> = line.splitn(7, |&b| b == b':').collect();
        let (Some(&uid_field), Some(&home)) = (fields.get(2), fields.get(5)) else {
            continue;
        };
        if uid_field == uid_str.as_bytes() {
            let home_path = PathBuf::from(OsStr::from_bytes(home));
            if home_path.is_absolute() {
                return Some(home_path);
            }
        }
    }
    None
}

/// The `aterm.toml` path loaded by the window and written by `--write-config`.
#[must_use]
pub fn aterm_config_path() -> Option<PathBuf> {
    resolve_aterm_config_path(
        config_home().as_deref(),
        std::env::var_os("APPDATA").as_deref(),
        std::env::var_os("HOME").as_deref(),
    )
}

/// The rule behind [`aterm_config_path`], pure for testing.
#[must_use]
pub fn resolve_aterm_config_path(
    xdg_config_home: Option<&OsStr>,
    appdata: Option<&OsStr>,
    home: Option<&OsStr>,
) -> Option<PathBuf> {
    resolve_aterm_config_dir(xdg_config_home, appdata, home).map(|dir| dir.join("aterm.toml"))
}

/// Why `aterm.toml` (the file a path or its symlink ends at) is refused, by the one rule every
/// reader of it holds (the window's settings, the `[swarm] enabled` switch, Commons' policy):
/// the file must be owned by this user (`me`) or by root, with no group or other write bit and
/// no setuid or setgid bit (`mode`). A file root owns and nobody else can write is one
/// home-manager links from its read-only store; a file another user owns, or any user can write
/// past its owner, could turn sharing on or off for this user. `None` where the file passes.
#[must_use]
pub fn aterm_config_refusal(owner: u32, mode: u32, me: u32) -> Option<String> {
    if owner != me && owner != 0 {
        return Some(format!("owned by uid {owner}, not this user or root"));
    }
    let why = if mode & 0o022 != 0 {
        "lets other users write it"
    } else if mode & 0o6000 != 0 {
        "carries a set-id bit"
    } else {
        return None;
    };
    Some(format!(
        "mode {:o} {why}; run chmod 600 on it",
        mode & 0o7777
    ))
}

/// [`aterm_config_refusal`] of the file `meta` describes (the one a read opened, a symlink
/// followed), against this process's effective user.
#[cfg(all(unix, not(target_family = "wasm")))]
#[must_use]
pub fn aterm_config_file_refusal(meta: &std::fs::Metadata) -> Option<String> {
    use std::os::unix::fs::MetadataExt as _;
    aterm_config_refusal(meta.uid(), meta.mode(), aterm_uds::peer::our_uid())
}

/// The most of `aterm.toml` [`read_aterm_config`] reads.
#[cfg(not(target_family = "wasm"))]
pub const ATERM_CONFIG_MAX_BYTES: u64 = 1024 * 1024;

/// A present config could not be admitted or read; neither case means defaults were selected.
#[cfg(not(target_family = "wasm"))]
#[derive(Debug)]
pub enum AtermConfigReadError {
    /// The opened file failed the existing config ownership policy.
    Refused(String),
    /// The file, its metadata, bounded contents or text could not be read.
    Unavailable(std::io::Error),
}

#[cfg(not(target_family = "wasm"))]
impl std::fmt::Display for AtermConfigReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused(reason) => write!(f, "config refused: {reason}"),
            Self::Unavailable(error) => error.fmt(f),
        }
    }
}

#[cfg(not(target_family = "wasm"))]
impl std::error::Error for AtermConfigReadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Refused(_) => None,
            Self::Unavailable(error) => Some(error),
        }
    }
}

/// The admitted bytes of `aterm.toml`, or `None` only when the path is absent.
/// A symlink (a dotfiles checkout, home-manager) is followed. Its opened target must be a
/// regular file of at most [`ATERM_CONFIG_MAX_BYTES`], and on Unix must pass
/// [`aterm_config_refusal`] using the opened handle's metadata. No Windows ownership policy
/// is inferred from Unix modes. On Unix the open does not block on a FIFO.
///
/// # Errors
/// Returns [`AtermConfigReadError::Refused`] for a failed ownership policy, or
/// [`AtermConfigReadError::Unavailable`] for I/O, invalid file type or oversized contents.
/// A dangling link is unavailable, because the settings entry exists but cannot be read.
#[cfg(not(target_family = "wasm"))]
pub fn read_aterm_config(path: &std::path::Path) -> Result<Option<Vec<u8>>, AtermConfigReadError> {
    match aterm_uds::atomic_file::read_regular(path, ATERM_CONFIG_MAX_BYTES) {
        Ok((bytes, meta)) => {
            #[cfg(unix)]
            if let Some(reason) = aterm_config_file_refusal(&meta) {
                return Err(AtermConfigReadError::Refused(reason));
            }
            #[cfg(not(unix))]
            let _ = meta;
            Ok(Some(bytes))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            match std::fs::symlink_metadata(path) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(error) => Err(AtermConfigReadError::Unavailable(error)),
                Ok(_) => Err(AtermConfigReadError::Unavailable(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "an existing config entry could not be opened",
                ))),
            }
        }
        Err(error) => Err(AtermConfigReadError::Unavailable(error)),
    }
}

/// [`read_aterm_config`] as text.
///
/// # Errors
/// As [`read_aterm_config`], or unavailable with [`std::io::ErrorKind::InvalidData`] where
/// the admitted bytes are not UTF-8.
#[cfg(not(target_family = "wasm"))]
pub fn read_aterm_config_text(
    path: &std::path::Path,
) -> Result<Option<String>, AtermConfigReadError> {
    read_aterm_config(path)?
        .map(|bytes| {
            String::from_utf8(bytes).map_err(|error| {
                AtermConfigReadError::Unavailable(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    error,
                ))
            })
        })
        .transpose()
}

/// The aterm config directory.
#[must_use]
pub fn aterm_config_dir() -> Option<PathBuf> {
    resolve_aterm_config_dir(
        config_home().as_deref(),
        std::env::var_os("APPDATA").as_deref(),
        std::env::var_os("HOME").as_deref(),
    )
}

/// `$XDG_CONFIG_HOME` when set and non-empty.
#[must_use]
pub fn config_home() -> Option<std::ffi::OsString> {
    match std::env::var_os("XDG_CONFIG_HOME").filter(|x| !x.is_empty()) {
        Some(set) => Some(set),
        None => test_root().map(|root| root.join("config").into_os_string()),
    }
}

/// A Cargo test binary's own per-user root ([`aterm_uds::test_binary`]).
#[must_use]
pub fn test_root() -> Option<&'static std::path::Path> {
    #[cfg(not(target_family = "wasm"))]
    {
        aterm_uds::test_binary::scratch_root()
    }
    #[cfg(target_family = "wasm")]
    {
        None
    }
}

/// The state root an instance was given in place of the per-user one: the
/// development seam `$ATERM_STATE_HOME` ([`crate::dev_seam!`]), else a test
/// binary's `<root>/state` ([`test_root`]).
fn state_override() -> Option<PathBuf> {
    match crate::dev_seam!("ATERM_STATE_HOME") {
        Some(root) => Some(PathBuf::from(root)),
        None => test_root().map(|root| root.join("state")),
    }
}

/// The rule behind [`aterm_config_dir`], the folder of [`resolve_aterm_config_path`]'s file.
#[must_use]
pub fn resolve_aterm_config_dir(
    xdg_config_home: Option<&OsStr>,
    appdata: Option<&OsStr>,
    home: Option<&OsStr>,
) -> Option<PathBuf> {
    if let Some(x) = xdg_config_home.filter(|x| !x.is_empty()) {
        return Some(PathBuf::from(x).join("aterm"));
    }
    if cfg!(windows)
        && let Some(appdata) = appdata.filter(|a| !a.is_empty())
    {
        return Some(PathBuf::from(appdata).join("aterm"));
    }
    home.map(|h| PathBuf::from(h).join(".config").join("aterm"))
}

/// Return the user's data directory.
#[must_use]
pub fn data_dir() -> Option<PathBuf> {
    match test_root() {
        Some(root) => Some(root.join("data")),
        None => resolve_data_dir(data_platform()),
    }
}

/// The platform inputs [`resolve_data_dir`] and [`resolve_aterm_data_dir`] read.
#[derive(Debug, Default, Clone)]
pub struct DataPlatform {
    /// `$HOME` (`%USERPROFILE%`), as [`home_dir`] gives it.
    pub home: Option<PathBuf>,
    /// `$XDG_DATA_HOME`, consulted only on Linux (and only when absolute).
    pub xdg_data_home: Option<PathBuf>,
    /// `%LOCALAPPDATA%`, consulted only on Windows.
    pub local_app_data: Option<PathBuf>,
}

/// The live environment as [`DataPlatform`].
fn data_platform() -> DataPlatform {
    DataPlatform {
        home: home_dir(),
        xdg_data_home: std::env::var_os("XDG_DATA_HOME").map(PathBuf::from),
        local_app_data: std::env::var_os("LOCALAPPDATA").map(PathBuf::from),
    }
}

/// The pure half of [`data_dir`].
#[must_use]
pub fn resolve_data_dir(platform: DataPlatform) -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        let _ = (&platform.xdg_data_home, &platform.local_app_data);
        platform.home.map(|h| h.join("Library/Application Support"))
    }
    #[cfg(target_os = "linux")]
    {
        let _ = &platform.local_app_data;
        platform
            .xdg_data_home
            .filter(|p| p.is_absolute())
            .or_else(|| platform.home.map(|h| h.join(".local/share")))
    }
    #[cfg(windows)]
    {
        let _ = (&platform.home, &platform.xdg_data_home);
        platform.local_app_data
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
    {
        let _ = platform;
        None
    }
}

/// Aterm's data root: what a window keeps between its own launches.
#[must_use]
pub fn aterm_data_dir() -> Option<PathBuf> {
    resolve_aterm_data_dir(state_override(), data_platform())
}

/// The pure half of [`aterm_data_dir`].
#[must_use]
pub fn resolve_aterm_data_dir(
    override_root: Option<PathBuf>,
    platform: DataPlatform,
) -> Option<PathBuf> {
    if let Some(root) = override_root {
        // A relative root is refused, as in `resolve_state_dir`.
        return root.is_absolute().then_some(root);
    }
    resolve_data_dir(platform).map(|data| data.join("aterm"))
}

/// Whether this process runs under a private state root.
#[must_use]
pub fn runs_under_private_state_root() -> bool {
    crate::dev_seam!("ATERM_STATE_HOME").is_some() || test_root().is_some()
}

/// Why this process is not a shell of its user's own account, or `None` when it is.
#[must_use]
pub fn not_own_account() -> Option<&'static str> {
    #[cfg(unix)]
    {
        not_own_account_with(
            runs_under_private_state_root(),
            aterm_uds::peer::our_uid(),
            home_dir().as_deref(),
            aterm_uds::account::home().as_deref(),
        )
    }
    #[cfg(not(unix))]
    {
        runs_under_private_state_root().then_some(NOT_OWN_PRIVATE_ROOT)
    }
}

/// [`not_own_account`]'s reason under a private state root.
const NOT_OWN_PRIVATE_ROOT: &str = "it runs under a private state root, as an isolated aterm \
                                    instance does";

/// The pure half of [`not_own_account`].
#[must_use]
pub fn not_own_account_with(
    private: bool,
    euid: u32,
    home: Option<&std::path::Path>,
    account_home: Option<&std::path::Path>,
) -> Option<&'static str> {
    if private {
        return Some(NOT_OWN_PRIVATE_ROOT);
    }
    if euid == 0 {
        return Some("it runs as root");
    }
    let same = home
        .zip(account_home)
        .is_some_and(|(home, account)| same_directory(home, account));
    (!same).then_some("its HOME is not this account's home")
}

/// Whether `a` and `b` name one directory: the same path, or, on Unix, one file system object
/// (device and inode), which also joins the aliases `canonicalize` keeps apart, such as macOS's
/// firmlink `/System/Volumes/Data/Users/<user>` for `/Users/<user>`; elsewhere, one path once
/// both resolve. A path that cannot be read matches only its own spelling.
#[must_use]
pub fn same_directory(a: &std::path::Path, b: &std::path::Path) -> bool {
    if a == b {
        return true;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        std::fs::metadata(a)
            .ok()
            .zip(std::fs::metadata(b).ok())
            .is_some_and(|(a, b)| (a.dev(), a.ino()) == (b.dev(), b.ino()))
    }
    #[cfg(not(unix))]
    {
        std::fs::canonicalize(a).is_ok_and(|a| std::fs::canonicalize(b).is_ok_and(|b| a == b))
    }
}

/// Aterm's own state root.
#[must_use]
pub fn state_dir() -> Option<PathBuf> {
    resolve_state_dir(
        state_override(),
        StatePlatform {
            home: home_dir(),
            xdg_state_home: std::env::var_os("XDG_STATE_HOME").map(PathBuf::from),
            local_app_data: std::env::var_os("LOCALAPPDATA").map(PathBuf::from),
        },
    )
}

/// State root of the account whose files this process changes.
#[must_use]
pub fn account_state_dir() -> Option<PathBuf> {
    account_state_dir_as(runs_under_private_state_root())
}

/// [`account_state_dir`] with every live read but the private state root's, which is
/// `private`: the uid, the account record's home and [`state_dir`].
fn account_state_dir_as(private: bool) -> Option<PathBuf> {
    #[cfg(unix)]
    let (root, root_home) = if aterm_uds::peer::our_uid() == 0 {
        (true, aterm_uds::account::home())
    } else {
        (false, None)
    };
    #[cfg(not(unix))]
    let (root, root_home) = (false, None);
    account_state_dir_with(private, root, state_dir(), root_home)
}

/// The pure half of [`account_state_dir`].
#[must_use]
pub fn account_state_dir_with(
    private: bool,
    root: bool,
    state: Option<PathBuf>,
    root_home: Option<PathBuf>,
) -> Option<PathBuf> {
    if root && !private {
        return resolve_state_dir(
            None,
            StatePlatform {
                home: root_home,
                ..StatePlatform::default()
            },
        );
    }
    state
}

/// The platform inputs [`resolve_state_dir`] reads, captured for testing.
#[derive(Debug, Default, Clone)]
pub struct StatePlatform {
    /// `$HOME` (`%USERPROFILE%`), as [`home_dir`] gives it.
    pub home: Option<PathBuf>,
    /// `$XDG_STATE_HOME`, consulted only on non-macOS Unix.
    pub xdg_state_home: Option<PathBuf>,
    /// `%LOCALAPPDATA%`, consulted only on Windows.
    pub local_app_data: Option<PathBuf>,
}

/// The pure half of [`state_dir`].
#[must_use]
pub fn resolve_state_dir(
    override_root: Option<PathBuf>,
    platform: StatePlatform,
) -> Option<PathBuf> {
    if let Some(root) = override_root {
        // Relative is refused, not resolved: a state root that depends on the
        // cwd is no root at all.
        return root.is_absolute().then_some(root);
    }
    #[cfg(target_os = "macos")]
    {
        let _ = (&platform.xdg_state_home, &platform.local_app_data);
        platform
            .home
            .map(|home| home.join("Library/Application Support/aterm"))
    }
    #[cfg(windows)]
    {
        let _ = (&platform.home, &platform.xdg_state_home);
        platform
            .local_app_data
            .filter(|root| root.is_absolute())
            .map(|root| root.join("aterm"))
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let _ = &platform.local_app_data;
        if let Some(root) = platform.xdg_state_home.filter(|root| root.is_absolute()) {
            return Some(root.join("aterm"));
        }
        platform.home.map(|home| home.join(".local/state/aterm"))
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = platform;
        None
    }
}

/// Aterm's per-user log directory.
#[must_use]
pub fn logs_dir() -> Option<PathBuf> {
    resolve_logs_dir(
        state_override(),
        StatePlatform {
            home: home_dir(),
            xdg_state_home: std::env::var_os("XDG_STATE_HOME").map(PathBuf::from),
            local_app_data: std::env::var_os("LOCALAPPDATA").map(PathBuf::from),
        },
    )
}

/// The pure half of [`logs_dir`].
#[must_use]
pub fn resolve_logs_dir(
    override_root: Option<PathBuf>,
    platform: StatePlatform,
) -> Option<PathBuf> {
    if let Some(root) = override_root {
        return root.is_absolute().then(|| root.join("logs"));
    }
    #[cfg(target_os = "macos")]
    {
        let _ = (&platform.xdg_state_home, &platform.local_app_data);
        platform.home.map(|home| home.join("Library/Logs/aterm"))
    }
    #[cfg(windows)]
    {
        let _ = &platform.xdg_state_home;
        platform
            .local_app_data
            .filter(|root| root.is_absolute())
            .or_else(|| platform.home.map(|h| h.join("AppData").join("Local")))
            .map(|root| root.join("aterm").join("logs"))
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let _ = &platform.local_app_data;
        platform
            .xdg_state_home
            .filter(|root| root.is_absolute())
            .or_else(|| platform.home.map(|home| home.join(".local/state")))
            .map(|root| root.join("aterm/logs"))
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = platform;
        None
    }
}

#[cfg(test)]
#[path = "dirs_tests.rs"]
mod tests;
