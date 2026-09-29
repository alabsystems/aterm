// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Windows console driver for the `aterm` passthrough binary — the ConPTY
//! twin of `driver_unix.rs` behind the same four-function surface.
//!
//! Raw mode is the VT console-mode pair: `ENABLE_VIRTUAL_TERMINAL_INPUT` on
//! stdin (the console then synthesizes full VT escape sequences as character
//! streams, so arrows/F-keys arrive as bytes — the same transparency as the
//! unix byte pipe) and `ENABLE_VIRTUAL_TERMINAL_PROCESSING` on stdout.
//! Keystrokes arrive as `ReadConsoleInputW` records on an input thread, and
//! `WINDOW_BUFFER_SIZE_EVENT` is the SIGWINCH analogue — it rides the same
//! single input source the way SIGWINCH rides the unix poll loop: the ConPTY
//! is resized promptly (it must repaint), exactly as the unix `TIOCSWINSZ` is.
//! Nothing in this process models the screen.
//! Direct `unsafe extern "system"` kernel32 FFI only — the approved std-only
//! pattern; no ConPTY calls live here (those are aterm-pty's seam).

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

/// UTF-8 (`chcp 65001`): ConPTY output is UTF-8 and `WriteFile` to a console
/// decodes bytes in the OUTPUT codepage, so the session pins it (restored on
/// drop by [`RawGuard`]).
const UTF8_CODEPAGE: u32 = 65001;

// Layout-only fields (present so the C struct sizes/offsets are right, never
// read on our side) carry a leading underscore.
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

/// The `INPUT_RECORD.Event` C union. Only the KEY_EVENT arm is read; the
/// `pad` arm pins the union to the C size (16 bytes — MOUSE_EVENT_RECORD,
/// the largest member, ties KEY_EVENT_RECORD at 16).
#[repr(C)]
#[derive(Clone, Copy)]
union EventUnion {
    key: KeyEventRecord,
    _pad: [u32; 4],
}

/// `INPUT_RECORD`: a 2-byte tag + (after alignment padding) the event union.
#[repr(C)]
struct InputRecord {
    event_type: u16,
    event: EventUnion,
}

/// Ask the console for its window size; fall back to 24x80 (same as unix).
pub(crate) fn host_winsize() -> (u16, u16) {
    // SAFETY: out-param query on the process stdout handle; the zeroed struct
    // is a plain POD out-buffer.
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

/// Whether stdout is a console (`doctor`'s tty check): `GetConsoleMode`
/// succeeds only on a real console handle — the classic isatty analogue
/// (redirected pipes/files fail it).
pub(crate) fn stdout_is_tty() -> bool {
    // SAFETY: mode query on the process stdout handle with a valid out-param.
    unsafe {
        let h = GetStdHandle(STD_OUTPUT_HANDLE);
        let mut mode = 0u32;
        h != INVALID_HANDLE_VALUE && GetConsoleMode(h, &mut mode) != 0
    }
}

/// Whether `path` names a runnable shell. Windows has no `access(X_OK)`;
/// execute permission is an ACL/PATHEXT question, so this is the same honest
/// downgrade aterm-dev uses: the file must exist.
pub(crate) fn shell_is_executable(path: &str) -> bool {
    std::path::Path::new(path).is_file()
}

/// The interactive shell the WINDOW spawns on this machine, and where it came
/// from — what `aterm doctor` and `show-config` report on Windows.
///
/// Before 2026-09-22 both reported this PROCESS's `$SHELL`, falling back to
/// `%COMSPEC%`: measured inside a pwsh 7 tab, `aterm doctor` said
/// `shell: C:\WINDOWS\system32\cmd.exe (executable)` and from Git Bash it said
/// `bash.exe` — the shell the CLI was typed into, never the one a tab gets.
/// `aterm-pty`'s Windows spawn reads neither variable (`%SHELL%` is a POSIX
/// path in MSYS shells; see its `windows::shell`), so the report is the
/// spawn's own answer.
///
/// INSIDE A TAB that answer is the tab's: the window hands every shell tab the
/// program it runs ([`aterm_types::domain::ENV_TAB_SHELL`]), so a window
/// started with `--shell cmd` reports cmd.exe as "this tab's shell". Measured
/// 2026-09-27 on 0.95.0, before the hand-off: `aterm doctor` in such a tab
/// named pwsh as "the window's shell" — a window's `--shell` flag is invisible
/// to a CLI process. OUTSIDE a tab it is what a NEW window would run, from the
/// spawn's resolver over the one input a CLI process can see: aterm.toml's
/// `shell`, and with none set the platform default — `pwsh`, then
/// `powershell`, then `%COMSPEC%`, then `cmd.exe`.
pub(crate) struct WindowShell {
    /// The program `CreateProcessW` is handed: an absolute path when the name
    /// resolved (or was given as one), the bare name verbatim when it did not —
    /// which [`shell_is_executable`] then reports as missing, exactly as the
    /// spawn would fail.
    pub(crate) program: String,
    /// Which input decided it.
    pub(crate) source: ShellSource,
}

/// Which input decided the shell reported.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ShellSource {
    /// The aterm tab this process runs in said which shell it runs
    /// ([`aterm_types::domain::ENV_TAB_SHELL`]).
    Tab,
    /// aterm.toml's top-level `shell` key, carrying its value.
    Config(String),
    /// Nothing named one: the platform default, plus what aterm.toml said —
    /// [`ConfigShell::Unusable`] is worth a word, because the window then runs
    /// on defaults after logging a problem the user may not have seen.
    Default(aterm_pty::ShellOrigin, ConfigShell),
}

/// What aterm.toml says about `shell`, read with the parser the window reads
/// the file with.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ConfigShell {
    /// No file, no top-level `shell` key, or an empty one (the window's resolver
    /// treats `""` as unset).
    Absent,
    /// `shell = "<name or path>"`.
    Set(String),
    /// The file exists but the window cannot take it — unreadable, not TOML, or
    /// `shell` is not a string. The window logs the problem and runs on
    /// defaults (`app_config::load_config` → `Config::stand_in`), so the default
    /// is what this reports too. One case it cannot see: a value ANOTHER key's
    /// type refuses (`tab_title_format = "title_only"` — the window's typed
    /// `Config` rejects the whole file for it) also sends the window to
    /// defaults, and only that `Config`, in a crate this one does not link,
    /// knows every key's type. `aterm --window --validate-config` names it.
    Unusable(String),
}

impl WindowShell {
    /// The live answer: the tab's own shell when this process runs in an aterm
    /// tab, else the two inputs a new window reads, over the spawn's resolver.
    pub(crate) fn resolve() -> Self {
        let tab = std::env::var(aterm_types::domain::ENV_TAB_SHELL).ok();
        Self::from_tab(tab).unwrap_or_else(|| Self::resolve_with(config_shell()))
    }

    /// The tab's own shell, from the value its window handed it — `None` when
    /// there is none (outside a tab, or in a `-e` session, which is handed
    /// none) or it is empty.
    pub(crate) fn from_tab(value: Option<String>) -> Option<Self> {
        value
            .filter(|program| !program.is_empty())
            .map(|program| Self {
                program,
                source: ShellSource::Tab,
            })
    }

    /// The testable half: `config` is what aterm.toml said. Precedence is the
    /// window's — config, then [`aterm_pty::select_shell_with_origin`]'s
    /// defaults.
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
            ShellSource::Default(origin, _) => format!("default:{}", default_word(*origin)),
        }
    }

    /// `doctor`'s label after the verdict — a sentence naming whose shell it is
    /// and the input that chose it, so the row reads `shell: <program>
    /// (executable) — this tab's shell` in a tab and `— a new window's default
    /// shell, from …` outside one: a window launched with `--shell` runs that
    /// instead, and only its own tabs can say so.
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
                    // Unreachable through `resolve_with` (a named shell never
                    // reaches the Default arm), spelled so a new variant cannot
                    // render as nothing.
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

/// The `default:<word>` token per platform-default arm.
fn default_word(origin: aterm_pty::ShellOrigin) -> &'static str {
    match origin {
        aterm_pty::ShellOrigin::Override => "override",
        aterm_pty::ShellOrigin::Pwsh => "pwsh",
        aterm_pty::ShellOrigin::PowerShell => "powershell",
        aterm_pty::ShellOrigin::Comspec => "COMSPEC",
        aterm_pty::ShellOrigin::CmdLiteral => "cmd.exe",
    }
}

/// aterm.toml's `shell`, from the file the window loads
/// (`aterm_types::dirs::aterm_config_path`). A missing file is [`ConfigShell::Absent`];
/// any other read failure is [`ConfigShell::Unusable`], as it is for the window.
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

/// The pure half of [`config_shell`]: the TOP-LEVEL `shell` key of one
/// aterm.toml text, through `aterm_toml` — the parser the window loads the file
/// with, so a duplicate key or a stray table is refused here exactly where the
/// window refuses it. A key inside a table is not the key.
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

/// RAII console raw-mode guard: swaps stdin/stdout into the VT passthrough
/// modes (and the output codepage to UTF-8) and restores the originals on
/// drop, so a panic or early return never leaves the host console raw —
/// the analogue of the unix `set_raw`/`restore` termios pair.
struct RawGuard {
    stdin: isize,
    stdout: isize,
    stdin_orig: Option<u32>,
    stdout_orig: Option<u32>,
    output_cp_orig: u32,
}

impl RawGuard {
    fn install() -> Self {
        // SAFETY: mode queries/sets on the process std handles; every call is
        // a plain in/out-param kernel32 console API.
        unsafe {
            let stdin = GetStdHandle(STD_INPUT_HANDLE);
            let stdout = GetStdHandle(STD_OUTPUT_HANDLE);
            let mut mode = 0u32;
            let stdin_orig = (stdin != INVALID_HANDLE_VALUE
                && GetConsoleMode(stdin, &mut mode) != 0)
                .then_some(mode);
            if let Some(orig) = stdin_orig {
                // Raw keys + VT input synthesis + resize events; no line
                // buffering, no echo, no ^C cooking (0x03 flows to the shell,
                // like cfmakeraw).
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
        // SAFETY: restores the modes captured at install time on the same
        // handles; best-effort (a vanished console just fails the calls).
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

/// Write `data` to the stdout HANDLE via `WriteFile`, looping like the unix
/// `write_all`. Raw bytes on purpose: std's console writer requires each call
/// to be whole valid UTF-8, which ConPTY read-chunk boundaries cannot
/// guarantee; with the output codepage pinned to UTF-8 the console decodes
/// split sequences correctly across calls.
fn stdout_write_all(handle: isize, mut data: &[u8]) {
    while !data.is_empty() {
        let len = u32::try_from(data.len()).unwrap_or(u32::MAX);
        let mut written = 0u32;
        // SAFETY: `data` is live for the call and `written` is a valid
        // out-param; a zero return is an error (loop exits).
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

/// Append one UTF-16 code unit to `out` as UTF-8, pairing surrogates across
/// calls: `pending` holds a high surrogate whose low half has not arrived yet
/// (a pair CAN straddle two `ReadConsoleInputW` batches). Unpaired surrogates
/// become U+FFFD rather than corrupting the byte stream.
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
        // fall through: `unit` itself still needs handling below.
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

/// Console-input pump (input thread): KEY_EVENT characters → UTF-8 → the PTY
/// input; WINDOW_BUFFER_SIZE_EVENT → prompt ConPTY resize. Returns when the
/// console goes away (the shell then runs to its own exit).
fn pump_console_input(stdin: isize, master: i32) {
    // SAFETY: zeroed PODs — every field of every record arm is plain data.
    let mut records: [InputRecord; 64] = unsafe { std::mem::zeroed() };
    let mut pending: Option<u16> = None;
    loop {
        let mut n = 0u32;
        // SAFETY: `records` is a live out-buffer of the declared length;
        // blocking call, `n` is a valid out-param.
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
                    // Key-down records with a character (with VT input enabled
                    // the console synthesizes escape sequences as char runs;
                    // pure-modifier presses carry NUL and are skipped).
                    if key.key_down != 0 && key.unicode_char != 0 {
                        for _ in 0..key.repeat_count.max(1) {
                            push_utf16_unit(key.unicode_char, &mut pending, &mut bytes);
                        }
                    }
                }
                WINDOW_BUFFER_SIZE_EVENT => {
                    // The SIGWINCH analogue. Resize the ConPTY NOW — it must be
                    // resized promptly so it repaints — reading the live window
                    // rect (the event payload is the BUFFER size, not the
                    // window). `aterm_pty::resize` is thread-safe on Windows per
                    // the seam contract.
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

/// Piped-stdin pump (input thread): plain blocking byte passthrough — the
/// non-tty/protected_spawn case, the direct analogue of the unix
/// `read(STDIN_FILENO)` arm. EOF just ends the pump; the shell runs to its
/// own exit (the unix `fds[0].fd = -1` behavior).
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

/// Raw mode, the passthrough loop, resize forwarding, restore, reap: returns
/// the shell's exit code (non-exit → 1, mirroring the unix `!WIFEXITED`).
///
/// Input runs on a detached thread (it parks in `ReadConsoleInputW`/`read`
/// with no portable cancellation; process exit reclaims it — the same way the
/// unix driver's blocked reader ends). Output runs here: blocking ConPTY
/// reads → host stdout passthrough.
pub(crate) fn run(shell: aterm_pty::SpawnedShell, verbose: bool) -> i32 {
    let master = shell.master;
    let guard = RawGuard::install();
    let stdout = guard.stdout;
    let stdin = guard.stdin;

    // Piped/protected_spawn case: a non-CHAR stdin (pipe/file) cannot be
    // ReadConsoleInputW'd — use plain blocking reads. A CHAR handle that is
    // not a real console (GetConsoleMode failed, e.g. NUL) takes the same
    // fallback.
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
        // shell output -> host console (passthrough). The ConPTY was resized
        // on the input thread; nothing here holds a size.
        let r = aterm_pty::read(master, &mut buf);
        if r <= 0 {
            break; // shell exited / ConPTY closed
        }
        let out = &buf[..r as usize];
        stdout_write_all(stdout, out);
        bytes_in += out.len() as u64;
    }

    // Restore the console before anything else prints.
    drop(guard);

    // Reap and read the exit code BEFORE close_master: close_master drops the
    // session registry entry, after which exit_code/reap can no longer resolve
    // the pid (same ordering as tests/windows_smoke.rs).
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

    /// The top-level `shell` key, and nothing that only looks like it: a
    /// commented-out line, a key inside a table, an empty string (unset to the
    /// window's resolver). A non-string or a file the window cannot parse is
    /// UNUSABLE, because the window then runs on defaults — reporting the
    /// configured name there would name a shell no tab gets.
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

    /// The window's precedence, over the spawn's resolver: aterm.toml outranks
    /// the platform default, and the program is what the spawn would run — an
    /// absolute path for a name that resolves, the bare name verbatim (hence
    /// `not executable or missing`) for one that does not.
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
        assert!(
            !shell_is_executable(&missing.program),
            "…and the executable check then fails, so doctor says FAIL"
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

    /// With nothing named, the report IS the spawn's default (`select_shell`'s
    /// program, its origin arm named) — on this box pwsh on PATH.
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
            assert!(shell_is_executable(&dflt.program));
        }
    }

    /// Inside an aterm tab the report is THAT TAB's shell, as its window handed
    /// it over — a window started with `--shell cmd` said pwsh, "the window's
    /// shell", on 0.95.0 (measured 2026-09-27) — and outside one (nothing
    /// handed, or an empty value) it is a new window's default, as before.
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
