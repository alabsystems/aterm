// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! A scratch world's layout ([`crate::world`]) in a file of its own, `std` only, so code that
//! links no crate mounts it by `#[path]` and takes the same world: the `aterm-order-ld` wrapper
//! a macOS release link runs (`crates/aterm/build.rs`) starts each image its layout probe measures
//! in it (`probe_world` in `crates/aterm/src/macho_order.rs`), and `aterm-uds`, below this crate,
//! mounts it as its `world`, so its LAN client sends no ask of the root broker from a world. Its
//! code names no crate (no `crate::` path, no external crate) and it holds no inner attribute, so
//! that wrapper compiles it from source with `rustc --edition=2024` against `std` alone.

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};

/// The environment variable whose presence says an aterm runs in a scratch world. The lanes that
/// reach the local network read it raw (`std::env::var_os`), not through [`crate::dev_seam!`], so
/// it holds in a release bundle too. A build with development seams also takes it as its state
/// root ([`crate::dirs`]); [`Layout::state`] is where a build without them keeps its state under
/// the world's HOME, and [`Layout::env`] sets the marker there, so both keep it in one folder.
pub const MARKER: &str = "ATERM_STATE_HOME";

/// The `PATH` a Unix world starts with: the system's own folders, nothing of the caller's.
pub const SYSTEM_PATH: &str = "/usr/bin:/bin:/usr/sbin:/sbin";

/// The prompt a Unix world's `/bin/sh` startup files (`.profile`, `.shrc`) set: two columns, so a
/// typed burst never wraps on a long host name.
pub const PROMPT: &str = "PS1='$ '\n";

/// The variables of the caller's environment a Windows world keeps, as the caller holds them: the
/// machine's folders and facts Windows sets for every session (`SystemRoot`, the program folders,
/// the processor's), and `PATH`, which is the caller's session `PATH`: the machine's entries, the
/// user's own and whatever the caller's terminal put there. Unlike a Unix world, which starts
/// from [`SYSTEM_PATH`], a Windows world has no `PATH` of its own. Everything else is dropped: a
/// session id, a control socket or an agent's variables of the terminal the caller runs in must
/// not reach the world.
pub const WINDOWS_MACHINE_VARS: [&str; 16] = [
    "SystemRoot",
    "SystemDrive",
    "windir",
    "ComSpec",
    "PATH",
    "PATHEXT",
    "NUMBER_OF_PROCESSORS",
    "PROCESSOR_ARCHITECTURE",
    "ProgramData",
    "ProgramFiles",
    "ProgramFiles(x86)",
    "ProgramW6432",
    "CommonProgramFiles",
    "CommonProgramFiles(x86)",
    "CommonProgramW6432",
    "OS",
];

/// The [`WINDOWS_MACHINE_VARS`] without which Windows resolves `%SystemDrive%` and
/// `%SystemRoot%` to nothing and a program writes a folder literally named so.
pub const WINDOWS_REQUIRED: [&str; 2] = ["SystemRoot", "SystemDrive"];

/// The bytes a Unix-domain socket's path holds with its terminator on macOS (`sun_path`), the
/// smallest of the platforms a world is laid out for (Linux and Windows hold 108).
const SUN_PATH: usize = 104;

/// The longest names of the sockets aterm binds in a world's runtime folder: a window's control
/// socket at the largest process id, and a development keeper's.
const RUNTIME_SOCKETS: [&str; 2] = [
    "aterm/aterm-4294967295.sock",
    "aterm/keeper-dev-00000000.sock",
];

/// The socket aterm binds in a world's configuration folder: the network state's observer
/// (`aterm-db`'s relay, on Unix), at this path on every Unix but macOS. macOS moves its folder to
/// `aterm/.ubd/network-state.noindex/` (`aterm_uds::quiet_state::destination`), and the relay
/// binds it at the folder's canonical path (`/private/tmp/…` for a root under `/tmp`). Where that
/// path does not fit a socket address, the relay binds a short one instead, in `ao<uid>/` under
/// `/tmp` (under `/tmp/.ubd` on macOS), so no world relies on this one fitting; the layout holds
/// it to the bound all the same, on every platform.
const CONFIG_SOCKETS: [&str; 1] = ["aterm/network-state/observer.sock"];

/// The longest name of the sockets aterm binds in a Unix world's temporary folder (`TMPDIR`, in
/// HOME): a development keeper's, which goes there rather than in the runtime folder when the
/// temporary folder is not under `/tmp`.
const UNIX_TEMP_SOCKETS: [&str; 1] = ["aterm/keeper-dev-00000000.sock"];

/// The longest names of the sockets aterm binds in a Windows world's temporary folder (`TMP`, in
/// HOME), which holds its control folder (`aterm`, or `aterm-elevated` for an elevated process):
/// a development keeper's, longer than a window's control socket.
const WINDOWS_TEMP_SOCKETS: [&str; 2] = [
    "aterm/keeper-dev-00000000.sock",
    "aterm-elevated/keeper-dev-00000000.sock",
];

/// The platform a world is laid out for: where its folders sit under HOME and which variables
/// name them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Platform {
    /// macOS: state under `~/Library/Application Support`.
    MacOs,
    /// Every Unix but macOS: the XDG base directories.
    Xdg,
    /// Windows: state under `%LOCALAPPDATA%`.
    Windows,
}

impl Platform {
    /// The platform this code was built for, `None` where it is none of them.
    pub const HOST: Option<Platform> = if cfg!(target_os = "macos") {
        Some(Platform::MacOs)
    } else if cfg!(unix) {
        Some(Platform::Xdg)
    } else if cfg!(windows) {
        Some(Platform::Windows)
    } else {
        None
    };

    /// Every platform, so a law can be held on each from any host.
    pub const ALL: [Platform; 3] = [Platform::MacOs, Platform::Xdg, Platform::Windows];

    /// Whether the platform is a Unix, whose world has a `/bin/sh` and its startup files.
    const fn is_unix(self) -> bool {
        !matches!(self, Platform::Windows)
    }
}

/// `XDG_STATE_HOME` under `home`.
fn xdg_state_home(home: &Path) -> PathBuf {
    home.join(".local").join("state")
}

/// `XDG_DATA_HOME` under `home`.
fn xdg_data_home(home: &Path) -> PathBuf {
    home.join(".local").join("share")
}

/// `%LOCALAPPDATA%` under `home`.
fn local_app_data(home: &Path) -> PathBuf {
    home.join("AppData").join("Local")
}

/// `%APPDATA%` under `home`.
fn roaming_app_data(home: &Path) -> PathBuf {
    home.join("AppData").join("Roaming")
}

/// Where `platform` keeps aterm's state under `home`, the folder [`crate::dirs`] resolves when
/// nothing overrides it and the world's variables name the platform's folders under `home`:
/// - macOS: `home/Library/Application Support/aterm`;
/// - another Unix: `home/.local/state/aterm` (`XDG_STATE_HOME/aterm`);
/// - Windows: `home/AppData/Local/aterm` (`%LOCALAPPDATA%\aterm`).
#[must_use]
pub fn state_home_on(platform: Platform, home: &Path) -> PathBuf {
    match platform {
        Platform::MacOs => home
            .join("Library")
            .join("Application Support")
            .join("aterm"),
        Platform::Xdg => xdg_state_home(home).join("aterm"),
        Platform::Windows => local_app_data(home).join("aterm"),
    }
}

/// [`state_home_on`] the host ([`Platform::HOST`]); `None` where the host is no platform a world
/// is laid out for.
#[must_use]
pub fn state_home(home: &Path) -> Option<PathBuf> {
    Platform::HOST.map(|platform| state_home_on(platform, home))
}

/// One row per automatic lane: its name, its `aterm.toml` table and that table's body with the
/// lane off. Expands to [`Lane`], [`Lane::ALL`], [`Lane::table`], [`Lane::off`] and
/// [`LANES_OFF`], so a new lane is one row and reaches every world.
macro_rules! lanes {
    ($($(#[$doc:meta])* $lane:ident $table:literal $off:literal;)*) => {
        /// An automatic lane a scratch world turns off unless its config drives it
        /// ([`Config::driving`]).
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum Lane {
            $($(#[$doc])* $lane,)*
        }

        impl Lane {
            /// Every lane, in the order [`LANES_OFF`] writes their tables.
            pub const ALL: [Lane; [$(stringify!($lane)),*].len()] = [$(Lane::$lane),*];

            /// The lane's `aterm.toml` table, its switch in the product.
            #[must_use]
            pub const fn table(self) -> &'static str {
                match self {
                    $(Lane::$lane => $table,)*
                }
            }

            /// The lane's table, header included, with the lane off.
            #[must_use]
            pub const fn off(self) -> &'static str {
                match self {
                    $(Lane::$lane => concat!("[", $table, "]\n", $off),)*
                }
            }
        }

        /// Every lane's table with the lane off ([`Lane::off`]), in [`Lane::ALL`]'s order: what a
        /// world that drives no lane writes after its head. Tables only, no root keys, so a world
        /// puts its own root keys and tables before it.
        pub const LANES_OFF: &str = concat!($("[", $table, "]\n", $off),*);
    };
}

lanes! {
    /// `[update]`: no update check, download or apply.
    Update "update" "enabled = false\nauto_apply = false\n";
    /// `[packages]`: no package loop.
    Packages "packages" "enabled = false\n";
    /// `[machine] universal_control = "leave"`: the Mac's own preferences left alone.
    Machine "machine" "universal_control = \"leave\"\n";
    /// `[keeper]`: no keeper job in the login's launchd domain, which a lasting bundle's window
    /// would share with the person's own windows of it and leave running after the world ends.
    Keeper "keeper" "enabled = false\n";
    /// `[swarm]`: no part in the swarm: no `_alab._tcp` browse, no fleet reads and no vault
    /// sharing, each a use of the person's real LAN.
    Swarm "swarm" "enabled = false\n";
}

impl Lane {
    /// The lane whose table is `table`.
    fn named(table: &str) -> Option<Lane> {
        Lane::ALL.into_iter().find(|lane| lane.table() == table)
    }
}

/// The root key a world puts first: the audible bell off, which an unattended world has no one
/// to ring for.
pub const QUIET_ROOT: &str = "bell_sound = false\n";

/// A world's `aterm.toml`: the site's own `head` (its root keys, then its own tables) and the
/// lanes it drives; every other lane is off.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Config<'a> {
    /// The site's own text: its root keys first, then its own tables.
    pub head: &'a str,
    /// The lanes the site drives: the world writes no off table for them, and its head may
    /// configure them.
    pub driving: &'a [Lane],
}

impl<'a> Config<'a> {
    /// No head and no lane driven: every lane off.
    pub const OFF: Config<'static> = Config {
        head: "",
        driving: &[],
    };

    /// `head`, driving no lane.
    #[must_use]
    pub const fn new(head: &'a str) -> Self {
        Self { head, driving: &[] }
    }

    /// This config, driving `lanes`.
    #[must_use]
    pub const fn driving(self, lanes: &'a [Lane]) -> Self {
        Self {
            head: self.head,
            driving: lanes,
        }
    }

    /// Whether the config drives `lane`.
    fn drives(&self, lane: Lane) -> bool {
        self.driving.contains(&lane)
    }

    /// The `aterm.toml` text: [`QUIET_ROOT`] unless the head sets `bell_sound` at its root, the
    /// head as given (a newline added where it ends without one), then [`Lane::off`] for every
    /// lane of [`Lane::ALL`] the config does not drive.
    ///
    /// The refusals read the head by lines, not as TOML. A table header, or a key line at the
    /// root, names a lane by its first key, bare or quoted (`[update]`, `[update.x]`,
    /// `[[update]]`, `update = …`, `update.x = …`); a quoted key is read without escapes, so a
    /// lane spelled with one (`"upd\u0061te"`) is not seen. A line a value's open brackets or
    /// braces carry on to (a multi-line array or inline table) is that value's, never a header or
    /// a key. A multi-line string, whose lines would read as headers and keys, is refused.
    ///
    /// # Errors
    /// - [`ConfigRefusal::MultiLineString`]: the head opens a multi-line string (`"""` or `'''`
    ///   outside a one-line string and a comment);
    /// - [`ConfigRefusal::NamesLane`]: the head names a lane it does not drive, whose off table
    ///   would then clash with or override the head's.
    pub fn render(&self) -> Result<String, ConfigRefusal> {
        let mut at_root = true;
        let mut sets_bell = false;
        let mut open = 0;
        for line in self.head.lines() {
            let continues = open > 0;
            open = still_open(line, open).ok_or_else(|| ConfigRefusal::MultiLineString {
                line: line.trim().to_owned(),
            })?;
            if continues {
                continue;
            }
            let key = match read_line(line) {
                Read::Header(key) => {
                    at_root = false;
                    key
                }
                Read::Key(key) if at_root => {
                    sets_bell |= key == "bell_sound";
                    key
                }
                Read::Key(_) | Read::Other => continue,
            };
            if let Some(lane) = Lane::named(key)
                && !self.drives(lane)
            {
                return Err(ConfigRefusal::NamesLane {
                    lane,
                    line: line.trim().to_owned(),
                });
            }
        }
        let mut text = String::new();
        if !sets_bell {
            text.push_str(QUIET_ROOT);
        }
        text.push_str(self.head);
        if !self.head.is_empty() && !self.head.ends_with('\n') {
            text.push('\n');
        }
        for lane in Lane::ALL {
            if !self.drives(lane) {
                text.push_str(lane.off());
            }
        }
        Ok(text)
    }
}

/// What [`Config::render`] reads a line as.
enum Read<'t> {
    /// A table header, by its first key.
    Header(&'t str),
    /// A key line, by its first key.
    Key(&'t str),
    /// A blank line, a comment or any other line.
    Other,
}

/// `line` as [`Config::render`] reads it.
fn read_line(line: &str) -> Read<'_> {
    let line = line.trim_start();
    if let Some(header) = line.strip_prefix('[') {
        let header = header.strip_prefix('[').unwrap_or(header);
        return match first_key(header) {
            Some((key, after)) if after.starts_with(['.', ']']) => Read::Header(key),
            _ => Read::Other,
        };
    }
    match first_key(line) {
        Some((key, after)) if after.starts_with(['.', '=']) => Read::Key(key),
        _ => Read::Other,
    }
}

/// The first key of `text` after blanks, bare (ASCII letters, digits, `_` and `-`) or quoted
/// (`"…"` or `'…'`, read without escapes), and what follows it after blanks.
fn first_key(text: &str) -> Option<(&str, &str)> {
    let text = text.trim_start_matches([' ', '\t']);
    let (key, rest) = match text.chars().next()? {
        quote @ ('"' | '\'') => {
            let body = text.strip_prefix(quote)?;
            body.split_once(quote)?
        }
        _ => {
            let end = text
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-'))
                .unwrap_or(text.len());
            text.split_at(end)
        }
    };
    (!key.is_empty()).then_some((key, rest.trim_start_matches([' ', '\t'])))
}

/// The brackets and braces still open after `line`, `open` of them before it: each `[` or `{`
/// outside a one-line string and a comment opens one, each `]` or `}` closes one. `None` where
/// `line` opens a multi-line string (`"""` or `'''` there).
fn still_open(line: &str, mut open: usize) -> Option<usize> {
    let mut rest = line;
    while let Some(c) = rest.chars().next() {
        if rest.starts_with("\"\"\"") || rest.starts_with("'''") {
            return None;
        }
        rest = &rest[c.len_utf8()..];
        match c {
            '#' => break,
            '"' => rest = after_basic(rest),
            '\'' => rest = rest.split_once('\'').map_or("", |(_, after)| after),
            '[' | '{' => open += 1,
            ']' | '}' => open = open.saturating_sub(1),
            _ => {}
        }
    }
    Some(open)
}

/// What follows the one-line basic string `rest` is inside: the text after its closing `"`,
/// which no `\` escapes, or nothing where the line ends first.
fn after_basic(rest: &str) -> &str {
    let mut chars = rest.char_indices();
    while let Some((at, c)) = chars.next() {
        match c {
            '\\' => {
                chars.next();
            }
            '"' => return &rest[at + 1..],
            _ => {}
        }
    }
    ""
}

/// Why [`Config::render`] writes no `aterm.toml`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConfigRefusal {
    /// The head names a lane the config does not drive.
    NamesLane {
        /// The lane named.
        lane: Lane,
        /// The head's line that names it, trimmed.
        line: String,
    },
    /// The head opens a multi-line string, whose lines the refusals would read as headers and
    /// keys.
    MultiLineString {
        /// The head's line that opens it, trimmed.
        line: String,
    },
}

impl std::fmt::Display for ConfigRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigRefusal::NamesLane { lane, line } => write!(
                f,
                "the world's head names the [{}] lane ({line}), which the world turns off: \
                 drive the lane to configure it",
                lane.table()
            ),
            ConfigRefusal::MultiLineString { line } => write!(
                f,
                "the world's head opens a multi-line string ({line}), whose lines could name a \
                 lane unseen: write the value on one line"
            ),
        }
    }
}

impl std::error::Error for ConfigRefusal {}

/// A scratch world's folders, laid out from one root, the environment that names them, and the
/// files it writes there: `home/` (HOME), `cfg/` (`XDG_CONFIG_HOME`, holding `aterm/aterm.toml`)
/// and `run/` (`XDG_RUNTIME_DIR`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Layout {
    /// The platform the world is laid out for.
    pub platform: Platform,
    /// The world's root, which [`Layout::make`] makes or finds empty.
    pub root: PathBuf,
    /// HOME: `root/home`.
    pub home: PathBuf,
    /// `XDG_CONFIG_HOME`: `root/cfg`.
    pub config_home: PathBuf,
    /// `XDG_RUNTIME_DIR`: `root/run`.
    pub runtime: PathBuf,
    /// The `SHELL` of a Unix world: `/bin/sh`, or the one [`Layout::with_shell`] names.
    shell: &'static str,
}

impl Layout {
    /// The host's world under `root` ([`Platform::HOST`]).
    ///
    /// # Errors
    /// [`LayoutRefusal::NoPlatform`] on a host that is no platform a world is laid out for, and
    /// [`Layout::for_platform`]'s.
    pub fn under(root: &Path) -> Result<Layout, LayoutRefusal> {
        Layout::for_platform(Platform::HOST.ok_or(LayoutRefusal::NoPlatform)?, root)
    }

    /// The world of `platform` under `root`, from any host: a VM guest's, or a Windows one.
    ///
    /// # Errors
    /// [`LayoutRefusal::SocketTooLong`]: a socket aterm binds in the world would not fit a socket
    /// address under `root`.
    pub fn for_platform(platform: Platform, root: &Path) -> Result<Layout, LayoutRefusal> {
        let layout = Layout {
            platform,
            root: root.to_path_buf(),
            home: root.join("home"),
            config_home: root.join("cfg"),
            runtime: root.join("run"),
            shell: "/bin/sh",
        };
        layout.fits()?;
        Ok(layout)
    }

    /// This world with `shell` as a Unix world's `SHELL`.
    #[must_use]
    pub fn with_shell(self, shell: &'static str) -> Layout {
        Layout { shell, ..self }
    }

    /// The folder aterm keeps its state in: [`MARKER`]'s value, which a build with development
    /// seams takes as its state root, and the folder a build without them resolves from the
    /// world's environment ([`state_home_on`] its platform and HOME).
    #[must_use]
    pub fn state(&self) -> PathBuf {
        state_home_on(self.platform, &self.home)
    }

    /// The `aterm.toml` aterm reads in the world: `aterm/aterm.toml` under `XDG_CONFIG_HOME`.
    #[must_use]
    pub fn config_file(&self) -> PathBuf {
        self.config_home.join("aterm").join("aterm.toml")
    }

    /// A control socket's path in the world's runtime folder, `aterm/aterm.sock` in it.
    #[must_use]
    pub fn control_sock(&self) -> PathBuf {
        self.runtime.join("aterm").join("aterm.sock")
    }

    /// The folders [`Layout::make`] makes, each after its parent: `cfg`, `cfg/aterm`, `run`,
    /// `run/aterm`, HOME and `tmp` in it, then the platform's per-user folders under HOME
    /// (`.cache`, `.local`, `.local/share` and `.local/state` on Unix; `AppData`,
    /// `AppData/Local` and `AppData/Roaming` on Windows). Not aterm's state folder, which aterm
    /// makes.
    #[must_use]
    pub fn dirs(&self) -> Vec<PathBuf> {
        let home = &self.home;
        let mut dirs = vec![
            self.config_home.clone(),
            self.config_home.join("aterm"),
            self.runtime.clone(),
            self.runtime.join("aterm"),
            home.clone(),
            home.join("tmp"),
        ];
        if self.platform.is_unix() {
            dirs.extend([
                home.join(".cache"),
                home.join(".local"),
                xdg_data_home(home),
                xdg_state_home(home),
            ]);
        } else {
            dirs.extend([
                home.join("AppData"),
                local_app_data(home),
                roaming_app_data(home),
            ]);
        }
        dirs
    }

    /// The files [`Layout::make`] writes, each with its text: on Unix the shell's two startup
    /// files in HOME ([`PROMPT`]), then [`Layout::config_file`] holding `config` rendered.
    ///
    /// # Errors
    /// [`Config::render`]'s.
    pub fn files(&self, config: &Config<'_>) -> Result<Vec<(PathBuf, String)>, ConfigRefusal> {
        let mut files = Vec::new();
        if self.platform.is_unix() {
            files.push((self.home.join(".profile"), PROMPT.to_owned()));
            files.push((self.home.join(".shrc"), PROMPT.to_owned()));
        }
        files.push((self.config_file(), config.render()?));
        Ok(files)
    }

    /// The whole environment a process in the world starts with.
    ///
    /// On Unix nothing comes from `parent`: HOME, `PATH` ([`SYSTEM_PATH`]), `SHELL`, `ENV` (HOME's
    /// `.shrc`), an xterm `TERM`, no history (`HISTFILE`), `XDG_CACHE_HOME`, `XDG_DATA_HOME` and
    /// `XDG_STATE_HOME` under HOME, then the tail every world has: `XDG_CONFIG_HOME` and
    /// `XDG_RUNTIME_DIR` (`root/cfg`, `root/run`), `TMPDIR`, `TMP` and `TEMP` (HOME's `tmp`) and
    /// [`MARKER`] at [`Layout::state`].
    ///
    /// On Windows: each of [`WINDOWS_MACHINE_VARS`] `parent` holds, in that order, with its
    /// value; HOME and `USERPROFILE` (HOME), `APPDATA` (`AppData/Roaming` under HOME) and
    /// `LOCALAPPDATA` (`AppData/Local`); then the same tail. A Windows world has no `SHELL`,
    /// `ENV`, `TERM`, `HISTFILE`, `XDG_CACHE_HOME`, `XDG_DATA_HOME` or `XDG_STATE_HOME`, and
    /// its `PATH` is the caller's, where the caller has one.
    ///
    /// # Errors
    /// [`LayoutRefusal::MissingMachineVar`]: a Windows `parent` lacks one of
    /// [`WINDOWS_REQUIRED`], or holds it empty.
    pub fn env(
        &self,
        parent: &dyn Fn(&str) -> Option<OsString>,
    ) -> Result<Vec<(&'static str, OsString)>, LayoutRefusal> {
        let home = &self.home;
        let path = |path: PathBuf| path.into_os_string();
        let mut env: Vec<(&'static str, OsString)> = Vec::new();
        if self.platform.is_unix() {
            env.extend([
                ("HOME", path(home.clone())),
                ("PATH", SYSTEM_PATH.into()),
                ("SHELL", self.shell.into()),
                ("ENV", path(home.join(".shrc"))),
                ("TERM", "xterm-256color".into()),
                ("HISTFILE", "/dev/null".into()),
                ("XDG_CACHE_HOME", path(home.join(".cache"))),
                ("XDG_DATA_HOME", path(xdg_data_home(home))),
                ("XDG_STATE_HOME", path(xdg_state_home(home))),
            ]);
        } else {
            for name in WINDOWS_REQUIRED {
                if parent(name).is_none_or(|value| value.is_empty()) {
                    return Err(LayoutRefusal::MissingMachineVar(name));
                }
            }
            for name in WINDOWS_MACHINE_VARS {
                if let Some(value) = parent(name) {
                    env.push((name, value));
                }
            }
            env.extend([
                ("HOME", path(home.clone())),
                ("USERPROFILE", path(home.clone())),
                ("APPDATA", path(roaming_app_data(home))),
                ("LOCALAPPDATA", path(local_app_data(home))),
            ]);
        }
        let tmp = home.join("tmp");
        env.extend([
            ("XDG_CONFIG_HOME", path(self.config_home.clone())),
            ("XDG_RUNTIME_DIR", path(self.runtime.clone())),
            ("TMPDIR", path(tmp.clone())),
            ("TMP", path(tmp.clone())),
            ("TEMP", path(tmp)),
            (MARKER, path(self.state())),
        ]);
        Ok(env)
    }

    /// Lay the world out: `root` made owner-only where it is absent, or found an empty folder;
    /// then each of [`Layout::dirs`] made, never through an entry already there, and set
    /// owner-only; then each of [`Layout::files`] written new, owner-only. Off Unix no mode is
    /// set. The config is rendered before anything is made, and the root found to hold an entry
    /// is refused before anything is made in it, so either refusal leaves the root as it was.
    ///
    /// # Errors
    /// The config is refused ([`io::ErrorKind::InvalidInput`]), `root` is neither absent nor an
    /// empty folder ([`io::ErrorKind::AlreadyExists`]), an entry is in the way (a folder, file or
    /// link planted after the root was found empty), or a folder or file cannot be made.
    pub fn make(&self, config: &Config<'_>) -> io::Result<()> {
        let files = self
            .files(config)
            .map_err(|refusal| io::Error::new(io::ErrorKind::InvalidInput, refusal.to_string()))?;
        make_root(&self.root)?;
        for dir in self.dirs() {
            make_dir(&dir)?;
        }
        for (path, text) in &files {
            write_new(path, text)?;
        }
        Ok(())
    }

    /// Whether every socket aterm binds in the world fits a socket address: in its runtime,
    /// configuration and temporary folders.
    fn fits(&self) -> Result<(), LayoutRefusal> {
        let runtime = RUNTIME_SOCKETS.iter().map(|name| self.runtime.join(name));
        let config = CONFIG_SOCKETS
            .iter()
            .map(|name| self.config_home.join(name));
        let tmp = self.home.join("tmp");
        let temp: &[&str] = if self.platform.is_unix() {
            &UNIX_TEMP_SOCKETS
        } else {
            &WINDOWS_TEMP_SOCKETS
        };
        let temp = temp.iter().map(|name| tmp.join(name));
        for socket in runtime.chain(config).chain(temp) {
            if socket.as_os_str().as_encoded_bytes().len() >= SUN_PATH {
                return Err(LayoutRefusal::SocketTooLong(socket));
            }
        }
        Ok(())
    }
}

/// `root` made owner-only where absent; otherwise it must be a folder (not a link to one) with
/// nothing in it.
fn make_root(root: &Path) -> io::Result<()> {
    match std::fs::symlink_metadata(root) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => make_dir(root),
        Err(error) => Err(named(root, &error)),
        Ok(meta) if meta.is_dir() && std::fs::read_dir(root)?.next().is_none() => Ok(()),
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!(
                "{}: a world's root must be absent or an empty folder",
                root.display()
            ),
        )),
    }
}

/// `dir` made, one folder and never through an entry already there (no parent is made), then set
/// owner-only on Unix, past the umask. Crate-visible so the laws hold it on its own.
pub(crate) fn make_dir(dir: &Path) -> io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        builder.mode(0o700);
    }
    builder.create(dir).map_err(|error| named(dir, &error))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
            .map_err(|error| named(dir, &error))?;
    }
    Ok(())
}

/// `path` written with `text`, made new (never through an entry already there), owner-only on
/// Unix past the umask. Crate-visible so the laws hold it on its own.
pub(crate) fn write_new(path: &Path, text: &str) -> io::Result<()> {
    use std::io::Write as _;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options.open(path).map_err(|error| named(path, &error))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))
            .map_err(|error| named(path, &error))?;
    }
    file.write_all(text.as_bytes())
        .map_err(|error| named(path, &error))
}

/// `error` with the path it happened at.
fn named(path: &Path, error: &io::Error) -> io::Error {
    io::Error::new(error.kind(), format!("{}: {error}", path.display()))
}

/// Why a world is not laid out, or its environment not given.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LayoutRefusal {
    /// The host is no platform a world is laid out for ([`Platform::HOST`] is `None`).
    NoPlatform,
    /// A socket aterm binds in the world would not fit a socket address at this path, which the
    /// world's root sets.
    SocketTooLong(PathBuf),
    /// A Windows caller's environment lacks this one of [`WINDOWS_REQUIRED`], or holds it empty.
    MissingMachineVar(&'static str),
}

impl std::fmt::Display for LayoutRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LayoutRefusal::NoPlatform => f.write_str(
                "this host is none of the platforms a scratch world is laid out for \
                 (macOS, another Unix, Windows)",
            ),
            LayoutRefusal::SocketTooLong(socket) => write!(
                f,
                "{} does not fit a socket address ({SUN_PATH} bytes with its terminator): \
                 choose a shorter world root, one under /tmp/.ubd",
                socket.display()
            ),
            LayoutRefusal::MissingMachineVar(name) => write!(
                f,
                "the environment has no {name}: a Windows program started without it writes a \
                 literal %{name}% folder"
            ),
        }
    }
}

impl std::error::Error for LayoutRefusal {}
