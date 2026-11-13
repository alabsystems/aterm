// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

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
    let kept = "C:\\Users\\Jos\u{e9}\\caf\u{e9} \u{23fa} \u{2318}K \u{2713} ok\n";
    assert_eq!(ascii_punctuation(kept), kept);
}

/// Off Windows every stream gets the text as it is: the same bytes, not a
/// copy, `·` and `—` included.
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
