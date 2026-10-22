// Copyright 2026 Andrew Yates, Inc.
// SPDX-License-Identifier: Apache-2.0

//! Platform directory resolution from environment variables and platform conventions, with
//! no external dependencies.

use std::ffi::OsStr;
use std::path::PathBuf;

/// Return the user's home directory: `$HOME` (else `/etc/passwd`) on Unix, `%USERPROFILE%` on
/// Windows.
#[must_use]
pub fn home_dir() -> Option<PathBuf> {
    #[cfg(unix)]
    {
        home_from_env(std::env::var_os("HOME").as_deref())
            .or_else(|| passwd_home_dir(current_uid()))
    }
    #[cfg(windows)]
    {
        std::env::var_os("USERPROFILE").map(PathBuf::from)
    }
    // wasm and other targets have no OS home dir.
    #[cfg(not(any(unix, windows)))]
    {
        None
    }
}

#[cfg(unix)]
fn current_uid() -> u32 {
    // SAFETY: getuid() is always safe — no failure mode, no args.
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

/// The development seam ([`crate::dev_seam!`]) naming the file [`home_dir`]'s passwd
/// fallback reads instead of `/etc/passwd`. A test that needs an account the file does
/// not list (macOS keeps local accounts in Directory Services, a container may run an
/// unlisted uid) points it at an empty file on a host whose file lists every account
/// (Linux). A shipped binary reads `/etc/passwd` whatever the environment holds.
pub const PASSWD_FILE_SEAM: &str = "ATERM_PASSWD_FILE";

/// The home directory for `uid` from `/etc/passwd`, read as bytes: the format needs no UTF-8
/// decode, and a stray legacy byte in another user's GECOS field cannot fail the lookup.
#[cfg(unix)]
fn passwd_home_dir(uid: u32) -> Option<PathBuf> {
    let contents = std::fs::read(passwd_file()).ok()?;
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
// Skip: the line split walks std iterators (absent bodies).
#[cfg_attr(trust_verify, trust::skip)]
fn home_from_passwd(contents: &[u8], uid: u32) -> Option<PathBuf> {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;
    let uid_str = uid.to_string();
    // `str::lines` on bytes; the empty piece after a final newline is a short line.
    for line in contents.split(|&b| b == b'\n') {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        // Only fields 2 (uid) and 5 (home) are read; `splitn(7, ..)` gives the `collect` a
        // literal bound for the Trust gate.
        let fields: Vec<&[u8]> = line.splitn(7, |&b| b == b':').collect();
        // Total `get`s, not indexing, for the verifier; a short line is skipped.
        let (Some(&uid_field), Some(&home)) = (fields.get(2), fields.get(5)) else {
            continue;
        };
        if uid_field == uid_str.as_bytes() && !home.is_empty() {
            return Some(PathBuf::from(OsStr::from_bytes(home)));
        }
    }
    None
}

/// The `aterm.toml` path: the file the window loads and hot-reloads, `--write-config` writes,
/// and the CLI's `explain-config` and `doctor` name. Resolved, never created.
#[must_use]
pub fn aterm_config_path() -> Option<PathBuf> {
    resolve_aterm_config_path(
        config_home().as_deref(),
        std::env::var_os("APPDATA").as_deref(),
        std::env::var_os("HOME").as_deref(),
    )
}

/// The rule behind [`aterm_config_path`], pure for testing:
///
/// 1. `$XDG_CONFIG_HOME/aterm/aterm.toml` when set and non-empty, on every platform;
/// 2. Windows only: `%APPDATA%\aterm\aterm.toml` when set and non-empty;
/// 3. `$HOME/.config/aterm/aterm.toml`, macOS included: `aterm.toml` predates the
///    Application Support convention and stays where its users edit it.
///
/// Deliberately not `<OS config dir>/aterm/aterm.toml`: where the two disagree a wrong path
/// is silent (settings never load, the watcher watches nothing). `aterm-gui`'s
/// `app_config::config_path` delegates here.
#[must_use]
pub fn resolve_aterm_config_path(
    xdg_config_home: Option<&OsStr>,
    appdata: Option<&OsStr>,
    home: Option<&OsStr>,
) -> Option<PathBuf> {
    resolve_aterm_config_dir(xdg_config_home, appdata, home).map(|dir| dir.join("aterm.toml"))
}

/// The aterm config directory: the folder holding `aterm.toml` and what sits beside it (the
/// `themes` folder). Resolved, never created.
#[must_use]
pub fn aterm_config_dir() -> Option<PathBuf> {
    resolve_aterm_config_dir(
        config_home().as_deref(),
        std::env::var_os("APPDATA").as_deref(),
        std::env::var_os("HOME").as_deref(),
    )
}

/// `$XDG_CONFIG_HOME` when set and non-empty; else, in a Cargo test binary
/// ([`test_root`]), `<its root>/config`, so a test that names no config folder
/// never reads the person's `aterm.toml` or themes; else `None`. The first rung
/// of every reader of `aterm.toml`'s path.
#[must_use]
pub fn config_home() -> Option<std::ffi::OsString> {
    match std::env::var_os("XDG_CONFIG_HOME").filter(|x| !x.is_empty()) {
        Some(set) => Some(set),
        None => test_root().map(|root| root.join("config").into_os_string()),
    }
}

/// A Cargo test binary's own per-user root ([`aterm_uds::test_binary`]), under
/// which this module's aterm folders answer in a test that names none of its
/// own (`$ATERM_STATE_HOME`, `$XDG_CONFIG_HOME`): on 2026-10-18 the aterm-gui
/// unit tests made directories in the person's state and log folders and read
/// their `aterm.toml`, themes and machine list. `None` in every other process,
/// a shipped one included, and on wasm.
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

/// The rule behind [`aterm_config_dir`], the folder of [`resolve_aterm_config_path`]'s file:
/// `$XDG_CONFIG_HOME/aterm`, else `%APPDATA%\aterm` on Windows, else `$HOME/.config/aterm`
/// (an empty variable counts as unset). One rule for both, so a sibling of `aterm.toml` is
/// never looked for in another tree.
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
///
/// - **macOS**: `$HOME/Library/Application Support`
/// - **Linux**: `$XDG_DATA_HOME` (absolute) or `$HOME/.local/share`
/// - **Windows**: `%LOCALAPPDATA%`
///
/// [`resolve_data_dir`] is the rule; this reads the variables it consults. In a
/// Cargo test binary, `<its root>/data` ([`test_root`]).
#[must_use]
pub fn data_dir() -> Option<PathBuf> {
    match test_root() {
        Some(root) => Some(root.join("data")),
        None => resolve_data_dir(data_platform()),
    }
}

/// The platform inputs [`resolve_data_dir`] and [`resolve_aterm_data_dir`] read, captured for
/// testing.
#[derive(Debug, Default, Clone)]
pub struct DataPlatform {
    /// `$HOME` (`%USERPROFILE%`), as [`home_dir`] gives it; read on macOS and Linux.
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

/// The pure half of [`data_dir`]: the OS data directory per platform.
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
    // wasm and other targets have no OS data dir.
    #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
    {
        let _ = platform;
        None
    }
}

/// aterm's data root: what a window keeps between its own launches (the restore manifest
/// `session.toml`, the crash journals beside it, the cell-metrics cache).
///
/// - in a development build only, the `ATERM_STATE_HOME` seam ([`crate::dev_seam!`],
///   read as [`state_dir`] reads it; the override root itself, and a relative value is
///   refused as `None`, as [`state_dir`] refuses it) — so an instance given its own
///   state root never takes the owner's `session.toml` or claims the owner's crash
///   journals (ruling 406);
/// - else, in a Cargo test binary, its `<root>/state` ([`test_root`]);
/// - otherwise [`data_dir`]`/aterm` (on Linux `$XDG_DATA_HOME`/`.local/share`,
///   NOT [`state_dir`]'s `.local/state`: a shipped path does not move).
///
/// - in a development build only, the `ATERM_STATE_HOME` seam ([`crate::dev_seam!`]) is the
///   root itself (a relative value is `None`, as in [`state_dir`]), so a private instance never
///   takes the owner's `session.toml` or crash journals;
/// - otherwise [`data_dir`]`/aterm` (on Linux the data dir, not [`state_dir`]'s
///   `.local/state`: a shipped path does not move).
#[must_use]
pub fn aterm_data_dir() -> Option<PathBuf> {
    resolve_aterm_data_dir(state_override(), data_platform())
}

/// The pure half of [`aterm_data_dir`]: `override_root` is `$ATERM_STATE_HOME`.
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

/// Whether this process runs under a PRIVATE state root: a development build's
/// `ATERM_STATE_HOME` seam is set ([`crate::dev_seam!`], so a shipped binary answers
/// `false` whatever its environment holds), or it is a Cargo test binary
/// ([`test_root`]), whose state is its own wherever it named none. A relative or empty value counts: it
/// resolves no root ([`aterm_data_dir`], [`state_dir`]), but the process asked for one
/// of its own, so it never stands in for the person whose `$HOME` it shares.
///
/// It gates writes to the machine: the `[machine]` settings (Universal Control, Spotlight) are
/// the Mac's, so the launch's daily apply (`atpkg::machine::launch_apply_due`), a package
/// pass's `[machine]` edit and Settings' Apply stand aside, and a window passes
/// `atpkg::cli::PRIVATE_STATE_ROOT_FLAG` to the passes it spawns (the `atpkg` beside a
/// development build may compile no seam). `aterm pkg machine apply` typed by a person still
/// applies.
#[must_use]
pub fn runs_under_private_state_root() -> bool {
    crate::dev_seam!("ATERM_STATE_HOME").is_some() || test_root().is_some()
}

/// Why this process is not a shell of its user's own account, or `None` when it is: the
/// precondition of every converge item that changes the account's files
/// (docs/DESIGN-init-system-2026-10-20.md §3.2, `Requires::Account`), which `aterm doctor`
/// and `aterm pkg repair` state in their scope. Not under a private state root
/// ([`runs_under_private_state_root`]), not root (`sudo` keeps `$HOME`, so root would write
/// root-owned files into the person's tree), and `$HOME` the account record's home
/// (`getpwuid`), so a scratch `$HOME` (an isolated release launch, a test's child) is never
/// taken for the person's own shell. Off Unix only the private state root counts.
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

/// The pure half of [`not_own_account`]: `private` whether the process runs under a
/// private state root, `euid` its effective uid, `home` its `$HOME` and `account_home` the
/// account record's. The two homes are the same when equal as written or as resolved.
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
    let same = match (home, account_home) {
        (Some(home), Some(account)) => {
            home == account
                || std::fs::canonicalize(home)
                    .is_ok_and(|home| std::fs::canonicalize(account).is_ok_and(|a| a == home))
        }
        _ => false,
    };
    (!same).then_some("its HOME is not this account's home")
}

/// aterm's own STATE root — where it keeps what it owns across launches (the
/// harness state, the document journal, the workers' tasks). ONE rule, so
/// every state reader agrees on the directory:
///
/// - in a development build only, the `ATERM_STATE_HOME` seam ([`crate::dev_seam!`]; must be
///   absolute, a relative value is `None`). A shipped binary is isolated by a scratch `$HOME`
///   (or `$XDG_STATE_HOME` off macOS);
/// - else, in a Cargo test binary, its `<root>/state` ([`test_root`]);
/// - **macOS**: `$HOME/Library/Application Support/aterm`;
/// - **Linux/other Unix**: `$XDG_STATE_HOME/aterm` (absolute) or `$HOME/.local/state/aterm`;
/// - **Windows**: `%LOCALAPPDATA%\aterm`.
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

/// The state root of THE ACCOUNT whose files this process changes, where its converge
/// journal lives (docs/DESIGN-init-system-2026-10-20.md §3.3): an item's journal goes where
/// its account is. That is [`state_dir`], the root every state reader agrees on: a private
/// state root as it is (a test binary's own, a development build's `ATERM_STATE_HOME`),
/// else `$HOME`'s. So a run over a scratch `$HOME` (a test's `aterm` or `atpkg` child, an
/// isolated launch) journals under that scratch, never under the person whose uid it
/// shares. Root alone is different: `sudo` keeps the invoking person's `$HOME`, and root
/// must not write into their tree (a root-owned journal there would refuse every later
/// `aterm init` change), so outside a private state root root's journal is under its own
/// account record's home (`getpwuid`). One rule for every surface that journals: `aterm
/// init`, `aterm machines here`, `aterm doctor` and `aterm pkg repair`.
#[must_use]
pub fn account_state_dir() -> Option<PathBuf> {
    account_state_dir_as(runs_under_private_state_root())
}

/// [`account_state_dir`] with every live read but the private state root's, which is
/// `private`: the uid, the account record's home and [`state_dir`]. A test binary always
/// runs under its own private root, so this is how its test reaches the branch a spawned
/// `aterm doctor` or `atpkg repair` child over a scratch `$HOME` takes.
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

/// The pure half of [`account_state_dir`]: `private` whether the process runs under a
/// private state root, `root` whether it runs as root, `state` what [`state_dir`] resolves
/// for it, and `root_home` root's account record's home. The journal's root is `state`
/// (a relative `ATERM_STATE_HOME` resolves none, and nothing else stands in for it), except
/// for root outside a private state root, whose root is the state directory of
/// `root_home`.
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

/// The pure half of [`state_dir`]: `override_root` is the development seam.
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

/// aterm's per-user log directory, one rule for every process that logs there (the window's
/// and session's `aterm.log`, atpkg's `packages.log`) so the files sit side by side:
///
/// - in a development build only, `$ATERM_STATE_HOME/logs` when that seam is set
///   ([`crate::dev_seam!`]; a relative value is refused as `None`, as in [`state_dir`]);
/// - else, in a Cargo test binary, its `<root>/state/logs` ([`test_root`]);
/// - **macOS**: `$HOME/Library/Logs/aterm` (Console.app's convention);
/// - **Linux/other Unix**: `$XDG_STATE_HOME/aterm/logs` (absolute) or
///   `$HOME/.local/state/aterm/logs`;
/// - **Windows**: `%LOCALAPPDATA%\aterm\logs`, else the home dir's `AppData\Local`.
///
/// Resolution only: the caller creates the directory with the mode it needs.
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

/// The pure half of [`logs_dir`]: `override_root` is `$ATERM_STATE_HOME`.
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
mod tests {
    use super::*;

    /// The account's own shell is the one whose `$HOME` is the account record's home (as
    /// written or as resolved), not root and not under a private state root; each other
    /// case names itself.
    #[cfg(unix)]
    #[test]
    fn the_accounts_own_shell_is_its_home_not_root_and_no_private_root() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let dir = scratch.path();
        let home = dir.join("home");
        std::fs::create_dir(&home).unwrap();
        let link = dir.join("home-link");
        std::os::unix::fs::symlink(&home, &link).unwrap();
        let own = |private, euid, env: &std::path::Path| {
            not_own_account_with(private, euid, Some(env), Some(&home))
        };
        assert_eq!(own(false, 501, &home), None);
        assert_eq!(own(false, 501, &link), None, "the same home, resolved");
        assert_eq!(own(false, 0, &home), Some("it runs as root"));
        assert_eq!(
            own(false, 501, dir),
            Some("its HOME is not this account's home")
        );
        assert_eq!(own(true, 501, &home), Some(NOT_OWN_PRIVATE_ROOT));
        assert_eq!(
            not_own_account_with(false, 501, None, Some(&home)),
            Some("its HOME is not this account's home"),
            "no HOME is no match"
        );
    }

    /// An item's journal goes where its account is. A test's `aterm doctor` or `atpkg
    /// repair` child, its `$HOME` a scratch under the person's uid, journals under that
    /// scratch's state root, the one every state reader resolves; negative control: the
    /// account-record rule (root's, the old rule for every account), evaluated on the same
    /// inputs, sends that journal into the person's own state. Root, whose `$HOME` `sudo`
    /// kept from the person, journals under its own account record's home. A private state
    /// root is used as it is, root's included, and no state root journals nowhere, never in
    /// the account record's. Live, outside a private root, this account (not root)
    /// journals under the state root it resolves, not under its account record's home's.
    #[cfg(unix)]
    #[test]
    fn the_journal_root_is_the_state_root_the_items_act_on_and_roots_own_under_sudo() {
        let state_of = |home: &str| {
            resolve_state_dir(
                None,
                StatePlatform {
                    home: Some(home.into()),
                    ..StatePlatform::default()
                },
            )
        };
        let person = PathBuf::from("/Users//alice");
        let scratch = state_of("/scratch/home");
        assert!(scratch.is_some());
        let child = account_state_dir_with(false, false, scratch.clone(), Some(person.clone()));
        assert_eq!(
            child, scratch,
            "a scratch HOME's run journals in that scratch"
        );
        let record_rule =
            account_state_dir_with(false, true, scratch.clone(), Some(person.clone()));
        assert_eq!(
            record_rule,
            state_of("/Users//alice"),
            "the account record's home is the person's state"
        );
        assert_ne!(child, record_rule);
        // `sudo aterm machines here apply`, alice's HOME kept: root's own home.
        assert_eq!(
            account_state_dir_with(
                false,
                true,
                state_of("/Users//alice"),
                Some("/var/root".into())
            ),
            state_of("/var/root")
        );
        let private = Some(PathBuf::from("/scratch/state"));
        for root in [false, true] {
            assert_eq!(
                account_state_dir_with(true, root, private.clone(), Some("/var/root".into())),
                private
            );
            // A relative `ATERM_STATE_HOME`: no root at all.
            assert_eq!(
                account_state_dir_with(true, root, None, Some(person.clone())),
                None
            );
        }
        assert_eq!(
            account_state_dir_with(false, false, None, Some(person)),
            None,
            "no state root is no journal, never the account record's"
        );
        assert_eq!(account_state_dir_with(false, true, None, None), None);
        // The live rule in this test binary (not root): its own state root.
        assert_eq!(account_state_dir(), state_dir());
        // The live reads outside a private root, the branch a scratch-HOME child takes: the
        // state root this process resolves (here the test binary's scratch), never the
        // state of its account record's home, the person's. Paths only: nothing is written.
        let live = account_state_dir_as(false);
        assert_eq!(
            live,
            state_dir(),
            "a non-root account journals where it acts"
        );
        let record = resolve_state_dir(
            None,
            StatePlatform {
                home: aterm_uds::account::home(),
                ..StatePlatform::default()
            },
        );
        assert!(record.is_some(), "this account has a home on record");
        assert_ne!(
            live, record,
            "the account record's home is the person's state"
        );
    }

    /// An empty or relative `$HOME` is not a home.
    #[cfg(unix)]
    #[test]
    fn an_empty_or_relative_home_is_no_home() {
        use std::ffi::OsStr;
        assert_eq!(home_from_env(None), None);
        assert_eq!(home_from_env(Some(OsStr::new(""))), None);
        assert_eq!(home_from_env(Some(OsStr::new("Library"))), None);
        assert_eq!(home_from_env(Some(OsStr::new("./home"))), None);
        assert_eq!(
            home_from_env(Some(OsStr::new("/Users//someone"))),
            Some(PathBuf::from("/Users//someone"))
        );
    }

    /// The aterm.toml rule: a set `XDG_CONFIG_HOME` wins everywhere, then `%APPDATA%` on
    /// Windows, else `HOME/.config` (macOS included); an empty value is unset.
    #[test]
    fn aterm_config_path_follows_the_one_rule_the_window_loads_by() {
        let xdg = OsStr::new("/xdg");
        let appdata = OsStr::new(r"C:\Users\who\AppData\Roaming");
        let home = OsStr::new("/Users//who");
        let under_home = PathBuf::from(home).join(".config/aterm/aterm.toml");

        assert_eq!(
            resolve_aterm_config_path(Some(xdg), Some(appdata), Some(home)),
            Some(PathBuf::from(xdg).join("aterm").join("aterm.toml")),
            "a set XDG_CONFIG_HOME wins everywhere, APPDATA and HOME notwithstanding"
        );
        assert_eq!(
            resolve_aterm_config_path(Some(OsStr::new("")), None, Some(home)),
            Some(under_home.clone()),
            "an empty XDG_CONFIG_HOME is unset"
        );
        let with_appdata = resolve_aterm_config_path(None, Some(appdata), Some(home));
        if cfg!(windows) {
            assert_eq!(
                with_appdata,
                Some(PathBuf::from(appdata).join("aterm").join("aterm.toml")),
                "Windows: %APPDATA%\\aterm\\aterm.toml"
            );
            assert_eq!(
                resolve_aterm_config_path(None, Some(OsStr::new("")), Some(home)),
                Some(under_home.clone()),
                "an empty APPDATA is unset, and HOME is the last resort"
            );
        } else {
            assert_eq!(
                with_appdata,
                Some(under_home.clone()),
                "APPDATA means nothing off Windows"
            );
        }
        assert_eq!(
            resolve_aterm_config_path(None, None, None),
            None,
            "nothing to resolve from is None, not a panic"
        );
        // The live resolver IS the rule over the live environment, a test
        // binary's root standing in for an unset XDG_CONFIG_HOME.
        assert_eq!(
            aterm_config_path(),
            resolve_aterm_config_path(
                config_home().as_deref(),
                std::env::var_os("APPDATA").as_deref(),
                std::env::var_os("HOME").as_deref(),
            )
        );
    }

    /// The config directory: on every row of the XDG / APPDATA-only / HOME-only / none matrix
    /// it is the folder holding `aterm.toml`, and the themes folder sits beside that file.
    #[test]
    fn the_themes_folder_sits_beside_aterm_toml_on_every_row_of_the_matrix() {
        let xdg = OsStr::new("/xdg");
        let appdata = OsStr::new(r"C:\Users\who\AppData\Roaming");
        let home = OsStr::new("/Users//who");
        let empty = OsStr::new("");
        type Row<'a> = (
            &'a str,
            Option<&'a OsStr>,
            Option<&'a OsStr>,
            Option<&'a OsStr>,
        );
        let rows: [Row<'_>; 6] = [
            ("all three set", Some(xdg), Some(appdata), Some(home)),
            ("XDG only", Some(xdg), None, None),
            ("APPDATA only", None, Some(appdata), None),
            ("HOME only", None, None, Some(home)),
            (
                "empty XDG and APPDATA",
                Some(empty),
                Some(empty),
                Some(home),
            ),
            ("none", None, None, None),
        ];
        for (row, x, a, h) in rows {
            let dir = resolve_aterm_config_dir(x, a, h);
            let file = resolve_aterm_config_path(x, a, h);
            assert_eq!(
                file.as_deref().and_then(std::path::Path::parent),
                dir.as_deref(),
                "{row}: the config dir is the folder aterm.toml is in"
            );
            assert_eq!(
                file.as_ref().map(|f| f.with_file_name("themes")),
                dir.as_ref().map(|d| d.join("themes")),
                "{row}: themes == config_dir/themes, beside aterm.toml"
            );
        }

        let appdata_only = resolve_aterm_config_dir(None, Some(appdata), None);
        if cfg!(windows) {
            assert_eq!(
                appdata_only,
                Some(PathBuf::from(appdata).join("aterm")),
                "Windows with only %APPDATA%: the config dir (and its themes) is there"
            );
        } else {
            assert_eq!(appdata_only, None, "APPDATA means nothing off Windows");
        }
        assert_eq!(
            resolve_aterm_config_dir(None, None, Some(home)),
            Some(PathBuf::from(home).join(".config").join("aterm"))
        );
        assert_eq!(
            resolve_aterm_config_dir(Some(xdg), Some(appdata), Some(home)),
            Some(PathBuf::from(xdg).join("aterm")),
            "a set XDG_CONFIG_HOME wins everywhere"
        );

        // The live resolvers ARE the rule over the live environment (a test
        // binary's root standing in for an unset XDG_CONFIG_HOME), and the live
        // themes folder is the live config dir's `themes`.
        let live = resolve_aterm_config_dir(
            config_home().as_deref(),
            std::env::var_os("APPDATA").as_deref(),
            std::env::var_os("HOME").as_deref(),
        );
        assert_eq!(aterm_config_dir(), live);
        assert_eq!(
            crate::scheme::user_theme_dir(),
            live.map(|d| d.join("themes"))
        );
    }

    /// The state root: an absolute override wins, a relative one is refused, and otherwise the
    /// platform default applies.
    #[test]
    fn state_dir_honours_an_absolute_override_and_refuses_a_relative_one() {
        let platform = StatePlatform {
            home: Some(PathBuf::from("/Users//who")),
            xdg_state_home: Some(PathBuf::from("/xdg/state")),
            local_app_data: Some(PathBuf::from(r"C:\Users\who\AppData\Local")),
        };
        // Per host: `/tmp/...` is not absolute on Windows.
        #[cfg(not(windows))]
        let absolute_override = PathBuf::from("/tmp/aterm-state");
        #[cfg(windows)]
        let absolute_override = PathBuf::from(r"C:\tmp\aterm-state");
        assert_eq!(
            resolve_state_dir(Some(absolute_override.clone()), platform.clone()),
            Some(absolute_override),
            "the override is the root itself, no `aterm` appended"
        );
        assert_eq!(
            resolve_state_dir(Some(PathBuf::from("relative/state")), platform.clone()),
            None,
            "a relative override is refused, never joined onto the cwd"
        );
        let default = resolve_state_dir(None, platform.clone()).expect("a default resolves");
        #[cfg(target_os = "macos")]
        assert_eq!(
            default,
            PathBuf::from("/Users//who/Library/Application Support/aterm")
        );
        #[cfg(all(unix, not(target_os = "macos")))]
        assert_eq!(default, PathBuf::from("/xdg/state/aterm"));
        #[cfg(windows)]
        assert_eq!(default, PathBuf::from(r"C:\Users\who\AppData\Local\aterm"));
        assert!(default.is_absolute());
        #[cfg(all(unix, not(target_os = "macos")))]
        assert_eq!(
            resolve_state_dir(
                None,
                StatePlatform {
                    xdg_state_home: Some(PathBuf::from("relative")),
                    ..platform.clone()
                }
            ),
            Some(PathBuf::from("/Users//who/.local/state/aterm")),
            "a relative XDG_STATE_HOME falls through to the home default"
        );
        assert_eq!(
            resolve_state_dir(None, StatePlatform::default()),
            None,
            "nothing to resolve from is None, not a panic"
        );
    }

    /// The log directory: macOS `~/Library/Logs/aterm`, other Unix the XDG state dir's
    /// `aterm/logs` (a relative XDG value falls through to home), under an absolute override
    /// root, and `None` for a relative override or nothing to resolve from.
    #[test]
    fn logs_dir_follows_each_platforms_convention() {
        let platform = StatePlatform {
            home: Some(PathBuf::from("/Users//who")),
            xdg_state_home: Some(PathBuf::from("/xdg/state")),
            local_app_data: Some(PathBuf::from("C:\\Users\\who\\AppData\\Local")),
        };
        // Per host: `/tmp/...` is not absolute on Windows.
        #[cfg(not(windows))]
        let absolute_override = PathBuf::from("/tmp/aterm-state");
        #[cfg(windows)]
        let absolute_override = PathBuf::from(r"C:\tmp\aterm-state");
        assert_eq!(
            resolve_logs_dir(Some(absolute_override.clone()), platform.clone()),
            Some(absolute_override.join("logs"))
        );
        assert_eq!(
            resolve_logs_dir(Some(PathBuf::from("relative/state")), platform.clone()),
            None
        );
        #[cfg(target_os = "macos")]
        assert_eq!(
            resolve_logs_dir(None, platform.clone()),
            Some(PathBuf::from("/Users//who/Library/Logs/aterm"))
        );
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            assert_eq!(
                resolve_logs_dir(None, platform.clone()),
                Some(PathBuf::from("/xdg/state/aterm/logs"))
            );
            assert_eq!(
                resolve_logs_dir(
                    None,
                    StatePlatform {
                        xdg_state_home: Some(PathBuf::from("relative")),
                        ..platform.clone()
                    }
                ),
                Some(PathBuf::from("/Users//who/.local/state/aterm/logs"))
            );
        }
        let _ = &platform;
        assert_eq!(resolve_logs_dir(None, StatePlatform::default()), None);
    }

    /// The data root: an absolute state-home seam is the root itself, a relative one is
    /// refused, and unset it is the OS data dir's `aterm` on each platform arm.
    #[test]
    fn aterm_data_dir_follows_the_state_home_seam_and_else_the_os_data_dir() {
        let platform = DataPlatform {
            home: Some(PathBuf::from("/Users//who")),
            xdg_data_home: Some(PathBuf::from("/xdg/data")),
            local_app_data: Some(PathBuf::from(r"C:\Users\who\AppData\Local")),
        };
        // Per host: `/tmp/...` is not absolute on Windows.
        #[cfg(not(windows))]
        let absolute_override = PathBuf::from("/tmp/aterm-state");
        #[cfg(windows)]
        let absolute_override = PathBuf::from(r"C:\tmp\aterm-state");
        assert_eq!(
            resolve_aterm_data_dir(Some(absolute_override.clone()), platform.clone()),
            Some(absolute_override.clone()),
            "the override is the root itself, no `aterm` appended"
        );
        assert_eq!(
            resolve_aterm_data_dir(Some(absolute_override.clone()), platform.clone()),
            resolve_state_dir(Some(absolute_override.clone()), StatePlatform::default()),
            "under the seam the data root IS the state root"
        );
        for relative in ["relative/state", "./state", ""] {
            assert_eq!(
                resolve_aterm_data_dir(Some(PathBuf::from(relative)), platform.clone()),
                None,
                "a relative override ({relative:?}) is refused, never the owner's folder"
            );
        }
        let unset = resolve_aterm_data_dir(None, platform.clone());
        assert_eq!(
            unset,
            resolve_data_dir(platform.clone()).map(|data| data.join("aterm")),
            "unset, it is the OS data dir's `aterm`"
        );
        #[cfg(target_os = "macos")]
        assert_eq!(
            unset,
            Some(PathBuf::from(
                "/Users//who/Library/Application Support/aterm"
            ))
        );
        #[cfg(target_os = "linux")]
        {
            assert_eq!(unset, Some(PathBuf::from("/xdg/data/aterm")));
            assert_eq!(
                resolve_aterm_data_dir(
                    None,
                    DataPlatform {
                        xdg_data_home: Some(PathBuf::from("relative")),
                        ..platform.clone()
                    }
                ),
                Some(PathBuf::from("/Users//who/.local/share/aterm")),
                "a relative XDG_DATA_HOME falls through to the home default — the data \
                 dir, never the state dir's `.local/state`"
            );
        }
        #[cfg(windows)]
        assert_eq!(
            unset,
            Some(PathBuf::from(r"C:\Users\who\AppData\Local\aterm"))
        );
        assert_eq!(
            resolve_aterm_data_dir(None, DataPlatform::default()),
            None,
            "nothing to resolve from is None, not a panic"
        );
        // The live resolvers are the rules over the live environment, but in a
        // test binary, which answers under its own root (`test_binary_folders_…`).
        if test_root().is_none() {
            assert_eq!(data_dir(), resolve_data_dir(data_platform()));
        }
    }

    /// In a Cargo test binary every aterm folder this module resolves lies under
    /// the binary's own root unless the test named one, so no test reads or writes
    /// the person's state, logs or config. Negative control: the same rules over
    /// the live environment without the root name the person's folders.
    #[cfg(not(target_family = "wasm"))]
    #[test]
    fn test_binary_folders_lie_under_its_own_root() {
        let root = test_root().expect("this test binary's root");
        let state = crate::dev_seam!("ATERM_STATE_HOME").map(PathBuf::from);
        let want_state = state.clone().unwrap_or_else(|| root.join("state"));
        assert_eq!(state_dir(), Some(want_state.clone()));
        assert_eq!(aterm_data_dir(), Some(want_state.clone()));
        assert_eq!(logs_dir(), Some(want_state.join("logs")));
        assert_eq!(data_dir(), Some(root.join("data")));
        assert!(runs_under_private_state_root());
        let config = std::env::var_os("XDG_CONFIG_HOME")
            .filter(|x| !x.is_empty())
            .map_or_else(|| root.join("config"), PathBuf::from);
        assert_eq!(aterm_config_dir(), Some(config.join("aterm")));
        assert_eq!(aterm_config_path(), Some(config.join("aterm/aterm.toml")));

        let person = resolve_state_dir(
            None,
            StatePlatform {
                home: home_dir(),
                xdg_state_home: std::env::var_os("XDG_STATE_HOME").map(PathBuf::from),
                local_app_data: std::env::var_os("LOCALAPPDATA").map(PathBuf::from),
            },
        );
        if let Some(person) = person {
            assert!(!person.starts_with(root), "{person:?}");
            assert_ne!(state_dir(), Some(person));
        }
    }

    #[cfg(unix)]
    #[test]
    fn passwd_parsing_finds_the_uids_home() {
        let passwd = b"root:x:0:0:root:/root:/bin/bash\n\
                      nobody:x:65534:65534:nobody:/nonexistent:/usr/sbin/nologin\n\
                      testuser:x:1000:1000:Test User:/home/testuser:/bin/zsh\n";
        assert_eq!(home_from_passwd(passwd, 0), Some(PathBuf::from("/root")));
        assert_eq!(
            home_from_passwd(passwd, 1000),
            Some(PathBuf::from("/home/testuser"))
        );
        assert_eq!(home_from_passwd(passwd, 9999), None);
        // An empty home field is no home; a malformed line is skipped.
        let passwd = ["broken:", "x", ":500:500:Broken User::/bin/sh\n"].concat();
        assert_eq!(home_from_passwd(passwd.as_bytes(), 500), None);
        let passwd = b"short:x\n\
                      valid:x:42:42:User:/home/valid:/bin/sh\n\
                      \n";
        assert_eq!(
            home_from_passwd(passwd, 42),
            Some(PathBuf::from("/home/valid"))
        );
    }
}
