// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The CLI's text, spelled so that its reader decodes it.
//!
//! aterm's CLI text is UTF-8 (the `·` of the origin line, the manual's `—`), and
//! that is what it stays everywhere but one place: a pipe a Windows shell made
//! while the console is on a legacy code page. Measured 2026-09-28 on Windows 11
//! (aterm tabs under ConPTY, console output code page 437, the OEM default, which
//! aterm leaves as it found it), by the work this module was ported from
//! (6ad1a274d8) and not re-run since: in pwsh, `aterm --version | Select-Object
//! -First 2` printed `by Andrew Yates ┬╖ ALab ┬╖ alab.systems`, and `aterm
//! --version | findstr ALab` printed the same in pwsh and in cmd. pwsh decodes a
//! native command's output in the console's code page (`[Console]::OutputEncoding`),
//! and findstr hands the bytes to the console, which decodes them in it too.
//! Unpiped output was always right: std writes a console with `WriteConsoleW`.
//!
//! The console-wide fixes and the transcoding one were measured that day too, and
//! each broke output that was right:
//! - the console on UTF-8 (`[Console]::OutputEncoding` in the pwsh integration,
//!   or code page 65001 at spawn) broke legacy OEM output: `cmd /c type` of a
//!   code-page-437 file printed `caf� na�ve �` for `café naïve ·`;
//! - the text in the console's code page (`·` as 0xFA) fixed `Select-Object`,
//!   `findstr` and `more`, and broke every UTF-8 reader behind the same pipe,
//!   which pwsh also creates: `aterm --version | rg ALab` and Git's `| head`
//!   printed `�`/`□` for `·`, pwsh 7.4+ `aterm … > f.txt` (raw bytes) read back
//!   with `Get-Content` showed U+FFFD, and a harness capturing `pwsh -Command
//!   "aterm … | Select-Object …"` received 0xFA, which is not UTF-8. The pipe's
//!   creator is the same process for all of these readers, so aterm cannot tell
//!   them apart.
//!
//! What every one of those readers decodes alike is ASCII. So into a pipe that
//! pwsh, Windows PowerShell or cmd created (`shell_decodes_code_page`), under a
//! console on a legacy code page, aterm's own typographic punctuation is spelled
//! in ASCII (`ascii_punctuation`: `·` and `—` as `-`, `…` as `...`, `→` as
//! `->`), the way atpkg's cmd wrappers already write `—` as `-` for the same
//! console. Everything else stays UTF-8, byte for byte. Every other reader keeps
//! the text unchanged: a Git Bash pipeline (created by `bash`), a program that
//! captures aterm itself (the harness's own pipe), a file cmd redirects to, a
//! console on UTF-8 (65001), no console at all, and every stream off Windows,
//! where `for_stdout` and `for_stderr` hand the text back as it is.
//!
//! What this leaves: a UTF-8 reader behind a shell's pipe (`| rg`, pwsh 7.4+
//! `> f.txt`) reads `-` where the text says `·`, plainer but right; and a
//! character with no ASCII spelling (a screen glyph the manual shows, `⏺`, `⌘`;
//! a path's `é`) is still UTF-8 and still reads as mojibake to a code-page
//! reader, as every UTF-8 program's output does there. A console on UTF-8
//! (`[Console]::OutputEncoding = [Text.UTF8Encoding]::new()` in a profile, or
//! `chcp 65001` in cmd) gets the text unchanged into every pipe.
//!
//! This module only spells the text; the caller writes it, through the writer it
//! already uses (`aterm_uds::stdout` for an answer, so a reader that went away
//! still ends the answer, never the process).

use std::borrow::Cow;

/// `text` as it goes to stdout: unchanged, except into a pipe a Windows shell
/// reads in a legacy code page, where its punctuation is ASCII (module doc).
#[inline]
pub(crate) fn for_stdout(text: &str) -> Cow<'_, str> {
    #[cfg(windows)]
    if windows::shell_reads_in_code_page(&std::io::stdout()) {
        return Cow::Owned(ascii_punctuation(text));
    }
    Cow::Borrowed(text)
}

/// `text` as it goes to stderr: `for_stdout`'s rule, read off stderr's handle.
#[inline]
pub(crate) fn for_stderr(text: &str) -> Cow<'_, str> {
    #[cfg(windows)]
    if windows::shell_reads_in_code_page(&std::io::stderr()) {
        return Cow::Owned(ascii_punctuation(text));
    }
    Cow::Borrowed(text)
}

/// Whether a pipe created by the process whose image is `creator` is read in a
/// legacy code page: its creator is pwsh, Windows PowerShell or cmd, and the
/// console's output code page (`console_cp`, 0 for no console) is not UTF-8.
#[cfg(any(windows, test))]
fn shell_decodes_code_page(console_cp: u32, creator: Option<&str>) -> bool {
    const UTF8: u32 = 65001;
    let Some(image) = creator.and_then(|path| path.rsplit(['\\', '/']).next()) else {
        return false;
    };
    let shell = ["pwsh.exe", "powershell.exe", "cmd.exe"]
        .iter()
        .any(|name| image.eq_ignore_ascii_case(name));
    shell && console_cp != 0 && console_cp != UTF8
}

/// `text` with aterm's typographic punctuation spelled in ASCII, every other
/// character as it is. The table is the punctuation aterm's CLI text used
/// (`--help`, `--version`, `help` and its topics, `doctor` and `agents`, read on
/// 2026-09-28) with the quote and comparison pairs beside it.
#[cfg(any(windows, test))]
fn ascii_punctuation(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '\u{00b7}' | '\u{2013}' | '\u{2014}' | '\u{2212}' => out.push('-'),
            '\u{2026}' => out.push_str("..."),
            '\u{2192}' => out.push_str("->"),
            '\u{2190}' => out.push_str("<-"),
            '\u{25b8}' => out.push('>'),
            '\u{2018}' | '\u{2019}' => out.push('\''),
            '\u{201c}' | '\u{201d}' => out.push('"'),
            '\u{2264}' => out.push_str("<="),
            '\u{2265}' => out.push_str(">="),
            '\u{00b1}' => out.push_str("+/-"),
            c => out.push(c),
        }
    }
    out
}

#[cfg(windows)]
mod windows {
    //! Direct `unsafe extern "system"` kernel32 FFI, the pattern
    //! `driver_windows.rs` uses.
    use std::os::windows::io::AsRawHandle;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetFileType(handle: isize) -> u32;
        fn GetConsoleOutputCP() -> u32;
        fn GetNamedPipeServerProcessId(pipe: isize, pid: *mut u32) -> i32;
        fn OpenProcess(access: u32, inherit: i32, pid: u32) -> isize;
        fn QueryFullProcessImageNameW(
            process: isize,
            flags: u32,
            name: *mut u16,
            len: *mut u32,
        ) -> i32;
        fn CloseHandle(handle: isize) -> i32;
    }

    const FILE_TYPE_PIPE: u32 = 0x0003;
    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;

    /// Whether `stream` (this process's stdout or stderr) is a pipe a shell reads
    /// in a legacy code page (`super::shell_decodes_code_page`).
    pub(super) fn shell_reads_in_code_page(stream: &impl AsRawHandle) -> bool {
        let handle = stream.as_raw_handle() as isize;
        // SAFETY: a type query on this process's own std handle; a null or
        // invalid handle is no pipe and is never queried.
        if handle == 0 || handle == -1 || unsafe { GetFileType(handle) } != FILE_TYPE_PIPE {
            return false;
        }
        // SAFETY: a plain query with no arguments; 0 when there is no console.
        let console_cp = unsafe { GetConsoleOutputCP() };
        super::shell_decodes_code_page(console_cp, creator_image(handle).as_deref())
    }

    /// The image path of the process that created `pipe`, when it can be read.
    pub(super) fn creator_image(pipe: isize) -> Option<String> {
        let mut pid = 0u32;
        // SAFETY: `pid` is a valid out-param; a handle that is no pipe fails.
        if unsafe { GetNamedPipeServerProcessId(pipe, &mut pid) } == 0 {
            return None;
        }
        // SAFETY: an open by pid for a limited query; 0 is a failure.
        let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if process == 0 {
            return None;
        }
        let mut name = [0u16; 1024];
        let mut len = name.len() as u32;
        // SAFETY: `name` holds `len` u16s and `len` is a valid in/out-param.
        let ok = unsafe { QueryFullProcessImageNameW(process, 0, name.as_mut_ptr(), &mut len) };
        // SAFETY: the handle OpenProcess returned, closed exactly once.
        unsafe { CloseHandle(process) };
        (ok != 0).then(|| String::from_utf16_lossy(&name[..(len as usize).min(name.len())]))
    }
}

#[cfg(test)]
mod tests {
    use super::{ascii_punctuation, shell_decodes_code_page};

    /// Only a pipe pwsh, Windows PowerShell or cmd created is read in a legacy
    /// code page, and only while the console's code page is not UTF-8.
    #[test]
    fn only_a_shell_pipe_under_a_legacy_code_page_is_read_in_it() {
        for shell in [
            r"C:\Program Files\WindowsApps\Microsoft.PowerShell_7.6.6.0_x64__8wekyb3d8bbwe\pwsh.exe",
            r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe",
            r"C:\WINDOWS\system32\CMD.EXE",
        ] {
            assert!(shell_decodes_code_page(437, Some(shell)), "{shell}");
            assert!(shell_decodes_code_page(932, Some(shell)), "{shell}");
            assert!(!shell_decodes_code_page(65001, Some(shell)), "{shell}");
            assert!(!shell_decodes_code_page(0, Some(shell)), "{shell}");
        }
        // Git Bash, an agent harness, a Rust parent capturing aterm: UTF-8.
        for reader in [
            r"C:\Program Files\Git\usr\bin\bash.exe",
            r"C:\Users\u\AppData\Roaming\npm\node.exe",
            r"C:\tools\codex.exe",
            r"C:\x\pwsh.exe.bak",
            "",
        ] {
            assert!(!shell_decodes_code_page(437, Some(reader)), "{reader}");
        }
        assert!(!shell_decodes_code_page(437, None), "creator unknown");
    }

    /// The punctuation becomes ASCII, which a code-page reader and a UTF-8
    /// reader decode alike; any other character keeps its UTF-8 bytes.
    #[test]
    fn punctuation_is_spelled_in_ascii_and_nothing_else_changes() {
        assert_eq!(
            ascii_punctuation(aterm_types::identity::ORIGIN_LINE),
            "by Andrew Yates - ALab - alab.systems"
        );
        assert_eq!(
            ascii_punctuation(
                "aterm \u{2014} manual \u{2026} a \u{2192} b \u{2190} c \u{2013} \
                 Edit \u{25b8} Reset v\u{2212}1 \u{2018}x\u{2019} \u{201c}y\u{201d} \
                 \u{2264}64 \u{2265}2 \u{00b1}HH\n"
            ),
            "aterm - manual ... a -> b <- c - Edit > Reset v-1 'x' \"y\" <=64 >=2 +/-HH\n"
        );
        // A path's letters and the glyphs the manual shows stay UTF-8.
        let kept = "C:\\Users\\Jos\u{e9}\\caf\u{e9} \u{23fa} \u{2318}K \u{2713} ok\n";
        assert_eq!(ascii_punctuation(kept), kept);
    }

    /// Off Windows every stream gets the text as it is: the same bytes, not a
    /// copy, so `--help`, `--version`, `help` and `doctor` print exactly what they
    /// printed before this module, `·` and `—` included.
    #[cfg(not(windows))]
    #[test]
    fn off_windows_every_stream_gets_the_text_as_it_is() {
        use std::borrow::Cow;
        let text = format!(
            "{} \u{2014} \u{2026} \u{2192}\n",
            aterm_types::identity::ORIGIN_LINE
        );
        for spelled in [super::for_stdout(&text), super::for_stderr(&text)] {
            assert!(
                matches!(spelled, Cow::Borrowed(t) if std::ptr::eq(t, text.as_str())),
                "{spelled:?}"
            );
        }
    }

    /// A pipe this test process makes is one its creator reads, and this process
    /// is no shell: the pipe keeps the text unchanged.
    #[cfg(windows)]
    #[test]
    fn a_pipe_this_process_made_names_this_process_as_its_creator() {
        use std::os::windows::io::AsRawHandle as _;
        let (_reader, writer) = std::io::pipe().expect("pipe");
        let creator = super::windows::creator_image(writer.as_raw_handle() as isize)
            .expect("the creator of a pipe this process made");
        let me = std::env::current_exe().expect("current exe");
        assert!(
            creator.eq_ignore_ascii_case(&me.to_string_lossy()),
            "{creator} vs {}",
            me.display()
        );
        assert!(!shell_decodes_code_page(437, Some(&creator)));
        assert!(!super::windows::shell_reads_in_code_page(&writer));
    }
}
