// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//
// Added by the aterm project in 2026 to its copy of winit 0.30.13
// (see the repository NOTICE).

use super::*;

#[test]
fn parse_uri_list_extracts_file_paths() {
    let data = b"file:///tmp/one.txt\r\nfile:///tmp/two%20three.txt\r\n";
    let paths = parse_uri_list(data);
    assert_eq!(paths.len(), 2);
    assert!(paths[0].ends_with("one.txt"), "got {:?}", paths[0]);
    assert!(paths[1].ends_with("two three.txt"), "got {:?}", paths[1]);

    let paths = parse_uri_list(
        b"file:///tmp/aterm-uri-a%0D%0Afile:///tmp/aterm-uri-b%20\r\nfile:///tmp/%23aterm-uri-c\r\n",
    );
    assert_eq!(paths.len(), 2);
    assert!(paths[0]
        .as_os_str()
        .to_string_lossy()
        .ends_with("aterm-uri-a\r\nfile:///tmp/aterm-uri-b "));
    assert!(paths[1].ends_with("#aterm-uri-c"));
}

#[test]
fn parse_uri_list_skips_comments_hostnames_and_other_schemes() {
    let data = b"#comment\r\nhttp://example.com/x\r\nfile://host/share/f\r\nfile:///ok/f\r\n";
    let paths = parse_uri_list(data);
    assert_eq!(paths.len(), 1);
    assert!(paths[0].ends_with("ok/f"), "got {:?}", paths[0]);
    assert!(parse_uri_list(b"%66ile:///tmp/f\r\nfile://%2Ftmp/f\r\n").is_empty());
}

/// A dropped symlink is the link the source named, not its target. Negative
/// control: canonicalising each path, as this did, yields the target.
#[test]
fn parse_uri_list_keeps_a_symlink_as_named() {
    let dir = aterm_tempfile::tempdir().expect("a scratch dir");
    let target = dir.path().join("target.txt");
    std::fs::write(&target, b"").expect("a file");
    let link = dir.path().join("link.txt");
    std::os::unix::fs::symlink(&target, &link).expect("a symlink");
    let uri = format!("file://{}\r\nfile:///tmp/a%00b\r\n", link.display());
    assert_eq!(parse_uri_list(uri.as_bytes()), [link]);
}

#[test]
fn parse_uri_list_empty_is_empty() {
    assert!(parse_uri_list(b"").is_empty());
    assert!(parse_uri_list(b"\r\n\r\n").is_empty());
}
