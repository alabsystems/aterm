// Copyright The winit contributors
// SPDX-License-Identifier: Apache-2.0
//
// Derived from winit 0.30.13 src/platform_impl/linux/x11/dnd.rs,
// modified by the aterm project in 2026 (see the repository NOTICE).

use super::*;

#[test]
fn uri_list_decodes_paths_after_framing_and_preserves_literal_uri_text() {
    let paths = uri_list_entries(
        b"#comment\r\nfile:///tmp/a%0D%0Afile:///tmp/b%20\r\nfile:///tmp/%23c\r\n",
    )
    .map(file_uri_path)
    .collect::<Result<Vec<_>, _>>()
    .expect("two local paths");
    assert_eq!(paths, [PathBuf::from("/tmp/a\r\nfile:///tmp/b "), PathBuf::from("/tmp/#c")]);
    assert!(matches!(
        file_uri_path(b"%66ile:///tmp/f"),
        Err(DndDataParseError::UnexpectedProtocol(_))
    ));
    assert!(matches!(
        file_uri_path(b"file://%2Ftmp/f"),
        Err(DndDataParseError::HostnameSpecified(_))
    ));
    assert!(matches!(file_uri_path(b"file:///tmp/%FF"), Err(DndDataParseError::InvalidUtf8(_))));
    assert!(matches!(file_uri_path(b"file:///tmp/a%00b"), Err(DndDataParseError::NulInPath)));
}

#[test]
fn a_list_keeps_its_local_files_and_skips_the_rest() {
    let list = b"https://example.com/cat.png\r\nfile:///tmp/Screenshot%20from%202026.png\r\n\
                 trash:///old.txt\r\nfile:///tmp/%FF\r\nfile://host/etc/hosts\r\n\
                 sftp://host/home/f\r\nfile:///tmp/a%00b\r\nfile:///tmp/missing\r\n";
    assert_eq!(local_paths(list), [
        PathBuf::from("/tmp/Screenshot from 2026.png"),
        PathBuf::from("/tmp/missing")
    ]);
    assert!(local_paths(b"https://example.com/\r\ntrash:///x\r\n").is_empty());
    assert!(local_paths(b"").is_empty());
}
