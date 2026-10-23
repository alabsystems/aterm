// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Windows console passthrough driver: raw console modes, the ConPTY read loop, an input pump
//! (console key events or a pipe), resize forwarding, and the shell `doctor` and `show-config`
//! report.

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetStdHandle(which: u32) -> isize;
    fn GetConsoleMode(handle: isize, mode: *mut u32) -> i32;
    fn SetConsoleMode(handle: isize, mode: u32) -> i32;
    fn GetConsoleScreenBufferInfo(handle: isize, info: *mut ConsoleScreenBufferInfo) -> i32;
    fn ReadConsoleInputW(handle: isize, buf: *mut InputRecord, len: u32, read: *mut u32) -> i32;
    fn GetFileType(handle: isize) -> u32;
    fn WriteFile(
        handle: isize,
        data: *const u8,
        len: u32,
        written: *mut u32,
        overlapped: *mut core::ffi::c_void,
    ) -> i32;
    fn GetConsoleOutputCP() -> u32;
    fn SetConsoleOutputCP(cp: u32) -> i32;
}

const STD_INPUT_HANDLE: u32 = -10i32 as u32;
const STD_OUTPUT_HANDLE: u32 = -11i32 as u32;
const INVALID_HANDLE_VALUE: isize = -1;
const FILE_TYPE_CHAR: u32 = 0x0002;

const KEY_EVENT: u16 = 0x0001;
const WINDOW_BUFFER_SIZE_EVENT: u16 = 0x0004;

const ENABLE_PROCESSED_INPUT: u32 = 0x0001;
const ENABLE_LINE_INPUT: u32 = 0x0002;
const ENABLE_ECHO_INPUT: u32 = 0x0004;
const ENABLE_WINDOW_INPUT: u32 = 0x0008;
const ENABLE_EXTENDED_FLAGS: u32 = 0x0080;
const ENABLE_VIRTUAL_TERMINAL_INPUT: u32 = 0x0200;
const ENABLE_PROCESSED_OUTPUT: u32 = 0x0001;
const ENABLE_VIRTUAL_TERMINAL_PROCESSING: u32 = 0x0004;
const DISABLE_NEWLINE_AUTO_RETURN: u32 = 0x0008;

const UTF8_CODEPAGE: u32 = 65001;

#[repr(C)]
#[derive(Clone, Copy)]
struct Coord {
    _x: i16,
    _y: i16,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct SmallRect {
    left: i16,
    top: i16,
    right: i16,
    bottom: i16,
}

#[repr(C)]
struct ConsoleScreenBufferInfo {
    _size: Coord,
    _cursor_position: Coord,
    _attributes: u16,
    window: SmallRect,
    _maximum_window_size: Coord,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct KeyEventRecord {
    key_down: i32,
    repeat_count: u16,
    _virtual_key_code: u16,
    _virtual_scan_code: u16,
    unicode_char: u16,
    _control_key_state: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
union EventUnion {
    key: KeyEventRecord,
    _pad: [u32; 4],
}

#[repr(C)]
struct InputRecord {
    event_type: u16,
    event: EventUnion,
}

/// Host console size as `(rows, cols)`, falling back to `(24, 80)`.
pub(crate) fn host_winsize() -> (u16, u16) {
    // SAFETY: out-param query on the process stdout handle; the zeroed struct is a plain POD
    // out-buffer.
    unsafe {
        let h = GetStdHandle(STD_OUTPUT_HANDLE);
        let mut info: ConsoleScreenBufferInfo = std::mem::zeroed();
        if h != INVALID_HANDLE_VALUE && GetConsoleScreenBufferInfo(h, &mut info) != 0 {
            let rows = i32::from(info.window.bottom) - i32::from(info.window.top) + 1;
            let cols = i32::from(info.window.right) - i32::from(info.window.left) + 1;
            if rows > 0 && cols > 0 {
                return (rows as u16, cols as u16);
            }
        }
    }
    (24, 80)
}

/// Whether `path` names a runnable shell.
pub(crate) fn shell_check(path: &str) -> crate::ShellCheck {
    let path = std::path::Path::new(path);
    if path.is_file() {
        crate::ShellCheck::Runs
    } else if path.exists() {
        crate::ShellCheck::NotExecutable
    } else {
        crate::ShellCheck::Missing
    }
}

/// Interactive shell resolution for `aterm doctor` and `show-config` on Windows.
pub(crate) struct WindowShell {
    /// The program `CreateProcessW` is handed.
    pub(crate) program: String,
    /// Which input decided it.
    pub(crate) source: ShellSource,
}

/// Which input decided the shell reported.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ShellSource {
    /// The aterm tab this process runs in ([`aterm_types::domain::ENV_TAB_SHELL`]).
    Tab,
    /// aterm.toml's top-level `shell` key, carrying its value.
    Config(String),
    /// The platform default, plus what aterm.toml said.
    Default(aterm_pty::ShellOrigin, ConfigShell),
}

/// What aterm.toml says about `shell`, read with the parser the window reads the file with.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ConfigShell {
    /// No file, no top-level `shell` key, or an empty one.
    Absent,
    /// `shell = "<name or path>"`.
    Set(String),
    /// The file exists but is unreadable, invalid TOML, or has a non-string `shell`.
    Unusable(String),
}

impl WindowShell {
    /// Resolve the active tab's shell or a new window's default shell.
    pub(crate) fn resolve() -> Self {
        let tab = std::env::var(aterm_types::domain::ENV_TAB_SHELL).ok();
        Self::from_tab(tab).unwrap_or_else(|| Self::resolve_with(config_shell()))
    }

    /// The tab's own shell from the value its window handed it.
    pub(crate) fn from_tab(value: Option<String>) -> Option<Self> {
        value
            .filter(|program| !program.is_empty())
            .map(|program| Self {
                program,
                source: ShellSource::Tab,
            })
    }

    /// The testable half: `config` is what aterm.toml said.
    pub(crate) fn resolve_with(config: ConfigShell) -> Self {
        let named: Option<(String, ShellSource)> = match &config {
            ConfigShell::Set(v) => Some((v.clone(), ShellSource::Config(v.clone()))),
            ConfigShell::Absent | ConfigShell::Unusable(_) => None,
        };
        let (name, source) = match named {
            Some((name, source)) => (Some(name), Some(source)),
            None => (None, None),
        };
        let (program, origin) =
            aterm_pty::select_shell_with_origin(name.as_deref().map(std::ffi::OsStr::new));
        Self {
            program: program.to_string_lossy().into_owned(),
            source: source.unwrap_or(ShellSource::Default(origin, config)),
        }
    }

    /// `show-config`'s `shell_origin=` value: one token a script can switch on.
    pub(crate) fn origin_token(&self) -> String {
        match &self.source {
            ShellSource::Tab => "tab".to_string(),
            ShellSource::Config(_) => "aterm.toml".to_string(),
            ShellSource::Default(origin, _) => format!("default:{}", origin.word()),
        }
    }

    /// Format `doctor`'s label naming whose shell it is and the input that chose it.
    pub(crate) fn origin_sentence(&self) -> String {
        match &self.source {
            ShellSource::Tab => "this tab's shell".to_string(),
            ShellSource::Config(v) => {
                format!("a new window's default shell, from aterm.toml shell = \"{v}\"")
            }
            ShellSource::Default(origin, config) => {
                let arm = match origin {
                    aterm_pty::ShellOrigin::Pwsh => "pwsh on PATH",
                    aterm_pty::ShellOrigin::PowerShell => "powershell on PATH; no pwsh",
                    aterm_pty::ShellOrigin::Comspec => "%COMSPEC%; no pwsh or powershell on PATH",
                    aterm_pty::ShellOrigin::CmdLiteral => {
                        "cmd.exe verbatim; no pwsh, powershell or %COMSPEC%"
                    }
                    aterm_pty::ShellOrigin::Override => "an override",
                };
                let why = match config {
                    ConfigShell::Unusable(why) => {
                        format!(
                            "; aterm.toml is not usable ({why}), so the window runs on defaults"
                        )
                    }
                    ConfigShell::Absent | ConfigShell::Set(_) => String::new(),
                };
                format!("a new window's default shell, the platform default ({arm}){why}")
            }
        }
    }
}

fn config_shell() -> ConfigShell {
    let Some(path) = aterm_types::dirs::aterm_config_path() else {
        return ConfigShell::Absent;
    };
    match std::fs::read_to_string(&path) {
        Ok(text) => config_shell_from_text(&text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => ConfigShell::Absent,
        Err(e) => ConfigShell::Unusable(format!("unreadable: {e}")),
    }
}

/// Parse the top-level `shell` key of one `aterm.toml` text.
pub(crate) fn config_shell_from_text(text: &str) -> ConfigShell {
    let table: aterm_toml::Table = match text.parse() {
        Ok(table) => table,
        Err(e) => return ConfigShell::Unusable(format!("does not parse: {e}")),
    };
    match table.get("shell") {
        None => ConfigShell::Absent,
        Some(value) => match value.as_str() {
            Some("") => ConfigShell::Absent,
            Some(name) => ConfigShell::Set(name.to_string()),
            None => ConfigShell::Unusable("`shell` is not a string".to_string()),
        },
    }
}

/// Read `aterm.toml`'s top-level `conpty` setting, or `None` when absent or unreadable.
pub(crate) fn config_conpty() -> Option<String> {
    let path = aterm_types::dirs::aterm_config_path()?;
    crate::config_conpty_from_text(&std::fs::read_to_string(path).ok()?)
}

struct RawGuard {
    stdin: isize,
    stdout: isize,
    stdin_orig: Option<u32>,
    stdout_orig: Option<u32>,
    output_cp_orig: u32,
}

impl RawGuard {
    fn install() -> Self {
        // SAFETY: mode queries/sets on the process std handles; every call is a plain in/out-param
        // kernel32 console API.
        unsafe {
            let stdin = GetStdHandle(STD_INPUT_HANDLE);
            let stdout = GetStdHandle(STD_OUTPUT_HANDLE);
            let mut mode = 0u32;
            let stdin_orig = (stdin != INVALID_HANDLE_VALUE
                && GetConsoleMode(stdin, &mut mode) != 0)
                .then_some(mode);
            if let Some(orig) = stdin_orig {
                let raw = (orig
                    | ENABLE_VIRTUAL_TERMINAL_INPUT
                    | ENABLE_WINDOW_INPUT
                    | ENABLE_EXTENDED_FLAGS)
                    & !(ENABLE_LINE_INPUT | ENABLE_ECHO_INPUT | ENABLE_PROCESSED_INPUT);
                SetConsoleMode(stdin, raw);
            }
            let mut mode = 0u32;
            let stdout_orig = (stdout != INVALID_HANDLE_VALUE
                && GetConsoleMode(stdout, &mut mode) != 0)
                .then_some(mode);
            if let Some(orig) = stdout_orig {
                SetConsoleMode(
                    stdout,
                    orig | ENABLE_PROCESSED_OUTPUT
                        | ENABLE_VIRTUAL_TERMINAL_PROCESSING
                        | DISABLE_NEWLINE_AUTO_RETURN,
                );
            }
            let output_cp_orig = GetConsoleOutputCP();
            SetConsoleOutputCP(UTF8_CODEPAGE);
            Self {
                stdin,
                stdout,
                stdin_orig,
                stdout_orig,
                output_cp_orig,
            }
        }
    }
}

impl Drop for RawGuard {
    fn drop(&mut self) {
        // SAFETY: restores the modes captured at install time on the same handles; best-effort (a
        // vanished console just fails the calls).
        unsafe {
            if let Some(m) = self.stdin_orig {
                SetConsoleMode(self.stdin, m);
            }
            if let Some(m) = self.stdout_orig {
                SetConsoleMode(self.stdout, m);
            }
            SetConsoleOutputCP(self.output_cp_orig);
        }
    }
}

fn stdout_write_all(handle: isize, mut data: &[u8]) {
    while !data.is_empty() {
        let len = u32::try_from(data.len()).unwrap_or(u32::MAX);
        let mut written = 0u32;
        // SAFETY: `data` is live for the call and `written` is a valid out-param; a zero return is
        // an error (loop exits).
        let ok = unsafe {
            WriteFile(
                handle,
                data.as_ptr(),
                len,
                &mut written,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 || written == 0 {
            break;
        }
        data = &data[written as usize..];
    }
}

fn push_utf16_unit(unit: u16, pending: &mut Option<u16>, out: &mut Vec<u8>) {
    fn push_char(c: char, out: &mut Vec<u8>) {
        let mut buf = [0u8; 4];
        out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
    }
    if let Some(high) = pending.take() {
        if (0xDC00..=0xDFFF).contains(&unit) {
            let c = 0x10000 + ((u32::from(high) - 0xD800) << 10) + (u32::from(unit) - 0xDC00);
            push_char(
                char::from_u32(c).unwrap_or(char::REPLACEMENT_CHARACTER),
                out,
            );
            return;
        }
        push_char(char::REPLACEMENT_CHARACTER, out);
    }
    match unit {
        0xD800..=0xDBFF => *pending = Some(unit),
        0xDC00..=0xDFFF => push_char(char::REPLACEMENT_CHARACTER, out),
        u => push_char(
            char::from_u32(u32::from(u)).unwrap_or(char::REPLACEMENT_CHARACTER),
            out,
        ),
    }
}

fn pump_console_input(stdin: isize, master: i32) {
    // SAFETY: zeroed PODs — every field of every record arm is plain data.
    let mut records: [InputRecord; 64] = unsafe { std::mem::zeroed() };
    let mut pending: Option<u16> = None;
    loop {
        let mut n = 0u32;
        // SAFETY: `records` is a live out-buffer of the declared length; blocking call, `n` is a
        // valid out-param.
        let ok =
            unsafe { ReadConsoleInputW(stdin, records.as_mut_ptr(), records.len() as u32, &mut n) };
        if ok == 0 || n == 0 {
            return;
        }
        let mut bytes: Vec<u8> = Vec::new();
        for rec in &records[..n as usize] {
            match rec.event_type {
                KEY_EVENT => {
                    // SAFETY: the record tag says this arm is the key event.
                    let key = unsafe { rec.event.key };
                    if key.key_down != 0 && key.unicode_char != 0 {
                        for _ in 0..key.repeat_count.max(1) {
                            push_utf16_unit(key.unicode_char, &mut pending, &mut bytes);
                        }
                    }
                }
                WINDOW_BUFFER_SIZE_EVENT => {
                    let (rows, cols) = host_winsize();
                    aterm_pty::resize(master, rows, cols);
                }
                _ => {}
            }
        }
        if !bytes.is_empty() {
            aterm_pty::write_all(master, &bytes);
        }
    }
}

fn pump_piped_input(master: i32) {
    use std::io::Read as _;
    let mut stdin = std::io::stdin().lock();
    let mut buf = [0u8; 8192];
    loop {
        match stdin.read(&mut buf) {
            Ok(0) | Err(_) => return,
            Ok(n) => aterm_pty::write_all(master, &buf[..n]),
        }
    }
}

/// Raw mode, the passthrough loop, resize forwarding, restore, reap.
pub(crate) fn run(shell: aterm_pty::SpawnedShell, verbose: bool) -> i32 {
    let master = shell.master;
    let guard = RawGuard::install();
    let stdout = guard.stdout;
    let stdin = guard.stdin;

    // SAFETY: type query on the process stdin handle.
    let stdin_is_console =
        unsafe { GetFileType(stdin) } == FILE_TYPE_CHAR && guard.stdin_orig.is_some();
    std::thread::spawn(move || {
        if stdin_is_console {
            pump_console_input(stdin, master);
        } else {
            pump_piped_input(master);
        }
    });

    let mut bytes_in: u64 = 0;
    let mut buf = [0u8; 8192];
    loop {
        let r = aterm_pty::read(master, &mut buf);
        if r <= 0 {
            break; // shell exited / ConPTY closed
        }
        let out = &buf[..r as usize];
        stdout_write_all(stdout, out);
        bytes_in += out.len() as u64;
    }

    drop(guard);

    aterm_pty::reap(shell.pid);
    let code = aterm_pty::exit_code(shell.pid).unwrap_or(1);
    aterm_pty::close_master(master);
    if verbose {
        eprintln!("\r\n[aterm] session ended — {bytes_in} bytes passed through.");
    }
    code
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ends_with_exe(program: &str, exe: &str) -> bool {
        program.to_ascii_lowercase().ends_with(exe)
    }

    #[test]
    fn config_shell_reads_the_top_level_key_the_window_reads() {
        assert_eq!(config_shell_from_text(""), ConfigShell::Absent);
        assert_eq!(
            config_shell_from_text("# shell = \"bash\"\nfont_px = 14\n"),
            ConfigShell::Absent,
            "the starter file's commented example is not a setting"
        );
        assert_eq!(
            config_shell_from_text("[privacy]\nshell = \"bash\"\n"),
            ConfigShell::Absent,
            "a `shell` inside a table is not the top-level key"
        );
        assert_eq!(
            config_shell_from_text("font_px = 14\nshell = \"bash\"\n"),
            ConfigShell::Set("bash".to_string())
        );
        assert_eq!(
            config_shell_from_text("shell = 'C:\\Program Files\\Git\\bin\\bash.exe'\n"),
            ConfigShell::Set("C:\\Program Files\\Git\\bin\\bash.exe".to_string()),
            "a literal string keeps its backslashes"
        );
        assert_eq!(
            config_shell_from_text("shell = \"\"\n"),
            ConfigShell::Absent,
            "an empty shell is unset — select_shell filters it the same way"
        );
        assert!(matches!(
            config_shell_from_text("shell = 5\n"),
            ConfigShell::Unusable(why) if why.contains("not a string")
        ));
        assert!(
            matches!(
                config_shell_from_text("shell = \"a\"\nshell = \"b\"\n"),
                ConfigShell::Unusable(why) if why.contains("does not parse")
            ),
            "a duplicate key is refused, as the window refuses it"
        );
        assert!(matches!(
            config_shell_from_text("shell = [[\n"),
            ConfigShell::Unusable(why) if why.contains("does not parse")
        ));
    }

    #[test]
    fn window_shell_follows_the_windows_precedence_and_names_its_source() {
        let config = WindowShell::resolve_with(ConfigShell::Set("cmd".to_string()));
        assert!(
            ends_with_exe(&config.program, "cmd.exe"),
            "{}",
            config.program
        );
        assert_eq!(config.source, ShellSource::Config("cmd".to_string()));
        assert_eq!(config.origin_token(), "aterm.toml");
        assert_eq!(
            config.origin_sentence(),
            "a new window's default shell, from aterm.toml shell = \"cmd\""
        );

        let missing =
            WindowShell::resolve_with(ConfigShell::Set("aterm-no-such-shell-xyz".to_string()));
        assert_eq!(
            missing.program, "aterm-no-such-shell-xyz",
            "an unresolved name is reported verbatim, as CreateProcessW receives it"
        );
        assert_eq!(
            shell_check(&missing.program),
            crate::ShellCheck::Missing,
            "…and the shell check then fails, so doctor says FAIL"
        );

        let unusable =
            WindowShell::resolve_with(ConfigShell::Unusable("does not parse: x".to_string()));
        assert!(
            matches!(
                unusable.source,
                ShellSource::Default(_, ConfigShell::Unusable(_))
            ),
            "{:?}",
            unusable.source
        );
        assert!(
            unusable
                .origin_sentence()
                .contains("aterm.toml is not usable (does not parse: x)"),
            "{}",
            unusable.origin_sentence()
        );
    }

    #[test]
    fn window_shell_default_is_the_spawns_default() {
        let dflt = WindowShell::resolve_with(ConfigShell::Absent);
        let (program, origin) = aterm_pty::select_shell_with_origin(None);
        assert_eq!(dflt.program, program.to_string_lossy());
        assert!(matches!(dflt.source, ShellSource::Default(o, ConfigShell::Absent) if o == origin));
        assert!(
            dflt.origin_token().starts_with("default:"),
            "{}",
            dflt.origin_token()
        );
        assert!(
            dflt.origin_sentence()
                .starts_with("a new window's default shell, the platform default ("),
            "{}",
            dflt.origin_sentence()
        );
        if matches!(
            aterm_pty::classify_shell_name(std::ffi::OsStr::new("pwsh")),
            aterm_pty::ShellResolution::Resolved(_)
        ) {
            assert_eq!(dflt.origin_token(), "default:pwsh");
            assert!(ends_with_exe(&dflt.program, "pwsh.exe"), "{}", dflt.program);
            assert_eq!(shell_check(&dflt.program), crate::ShellCheck::Runs);
        }
    }

    #[test]
    fn inside_a_tab_the_report_is_the_tabs_own_shell() {
        let cmd = r"C:\Windows\system32\cmd.exe".to_string();
        let tab = WindowShell::from_tab(Some(cmd.clone())).expect("a tab said its shell");
        assert_eq!(tab.program, cmd);
        assert_eq!(tab.source, ShellSource::Tab);
        assert_eq!(tab.origin_token(), "tab");
        assert_eq!(tab.origin_sentence(), "this tab's shell");
        assert!(WindowShell::from_tab(None).is_none(), "outside a tab");
        assert!(
            WindowShell::from_tab(Some(String::new())).is_none(),
            "an empty value says nothing"
        );
    }
}
