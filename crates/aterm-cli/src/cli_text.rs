// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The CLI's text, spelled so that its reader decodes it.

use std::borrow::Cow;

/// `text` as it goes to stdout.
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

/// `text` with aterm's typographic punctuation (`·`, the dashes and minus, `…`,
/// `→`, `←`, `▸`, the curly quotes, `≤`, `≥` and `±`) spelled in ASCII, every
/// other character as it is (`❯`, `×` and `≈` keep their UTF-8 bytes).
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

    /// Whether `stream` is a pipe a shell reads in a legacy code page.
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
#[path = "cli_text_tests.rs"]
mod tests;
