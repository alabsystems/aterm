// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn file_uri_keeps_the_set_it_replaced() {
    for byte in u8::MIN..=u8::MAX {
        let before =
            byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'-' | b'_' | b'.' | b'~' | b':');
        assert_eq!(file_uri_keeps(byte), before, "byte {byte:#04x}");
    }
}

fn absolute(tail: &str) -> PathBuf {
    if cfg!(windows) {
        PathBuf::from(format!(r"C:\{}", tail.replace('/', r"\")))
    } else {
        PathBuf::from(format!("/{tail}"))
    }
}

#[test]
fn paths_round_trip_through_one_whitespace_free_token() {
    for tail in [
        "tmp/plain.txt",
        "tmp/with space/COMMIT_EDITMSG",
        "tmp/100%/x",
        "tmp/a#b?c.md",
        "tmp/caf\u{e9}/\u{65e5}\u{672c}.md",
        "tmp/tab\there",
        "tmp/~user/.config:x",
    ] {
        let path = absolute(tail);
        let uri = path_to_file_uri(&path).unwrap();
        assert!(uri.starts_with("file://"), "{uri}");
        assert!(
            !uri.chars().any(char::is_whitespace) && !uri.contains(['#', '?']),
            "one wire token, no query or fragment: {uri}"
        );
        assert_eq!(file_uri_to_path(&uri).unwrap(), path, "{uri}");
    }
}

#[cfg(not(windows))]
#[test]
fn the_spelling_is_the_hosts_canonical_one() {
    assert_eq!(
        path_to_file_uri(Path::new("/tmp/a b/100%#.md")).unwrap(),
        "file:///tmp/a%20b/100%25%23.md"
    );
    assert_eq!(
        path_to_file_uri(Path::new("/tmp/\u{e9}")).unwrap(),
        "file:///tmp/%C3%A9"
    );
    assert_eq!(
        file_uri_to_path("file://localhost/tmp/%c3%a9").unwrap(),
        PathBuf::from("/tmp/\u{e9}")
    );
}

#[test]
fn decoding_refuses_what_names_no_local_absolute_path() {
    assert_eq!(
        file_uri_to_path("https://example.invalid/x"),
        Err(UriError::UnsupportedScheme)
    );
    assert_eq!(
        file_uri_to_path("file://server/share/x"),
        Err(UriError::RemoteAuthority)
    );
    assert_eq!(
        file_uri_to_path("file:///tmp/x#3"),
        Err(UriError::Malformed)
    );
    assert_eq!(
        file_uri_to_path("file:///tmp/x?y"),
        Err(UriError::Malformed)
    );
    assert_eq!(file_uri_to_path("file://host"), Err(UriError::Malformed));
    assert_eq!(
        file_uri_to_path("file:///tmp/%00"),
        Err(UriError::Malformed)
    );
    assert_eq!(
        file_uri_to_path("file:///tmp/%zz"),
        Err(UriError::InvalidEncoding)
    );
    assert_eq!(
        file_uri_to_path("file:///tmp/%4"),
        Err(UriError::InvalidEncoding)
    );
    assert_eq!(
        file_uri_to_path("file:///tmp/%FF"),
        Err(UriError::InvalidEncoding)
    );
    assert_eq!(
        path_to_file_uri(Path::new("relative/path.txt")),
        Err(UriError::NotAbsolute)
    );
    assert_eq!(
        path_to_file_uri(Path::new("/tmp/bad\0path")),
        Err(UriError::Malformed)
    );
}

/// NEGATIVE CONTROL: percent-encoded backslash UNC prefixes (`file:///%5Cserver%5Cshare`)
/// decode to `"/\\server\\share"`, which Windows treats as a remote UNC path unless
/// [`windows_drive_path_text`] rejects mixed/backslash separator pairs, and `/C:relative` must
/// keep its leading slash so drive-relative paths are not rewritten as drive roots.
#[test]
fn windows_drive_text_loses_only_its_uri_root_slash() {
    assert_eq!(
        windows_drive_path_text("/C:/Users//Ada/My File.toml").unwrap(),
        "C:/Users//Ada/My File.toml"
    );
    assert_eq!(
        windows_drive_path_text(r"/C:\Users\Ada\My File.toml").unwrap(),
        r"C:\Users\Ada\My File.toml"
    );
    assert_eq!(windows_drive_path_text("/z:/").unwrap(), "z:/");
    assert_eq!(
        windows_drive_path_text("/C:relative").unwrap(),
        "/C:relative"
    );
    assert_eq!(windows_drive_path_text("/tmp/x").unwrap(), "/tmp/x");
    for remote in [
        "//server/share/aterm.toml",
        r"/\server\share\aterm.toml",
        r"\\server\share\aterm.toml",
        r"\/server/share/aterm.toml",
    ] {
        assert_eq!(
            windows_drive_path_text(remote),
            Err(UriError::RemoteAuthority),
            "{remote}"
        );
    }
}

#[cfg(windows)]
#[test]
fn windows_drive_paths_round_trip_through_real_path_rules() {
    let path = Path::new(r"C:\Users\Ada\My File.toml");
    let uri = path_to_file_uri(path).unwrap();
    assert_eq!(uri, "file:///C:/Users//Ada/My%20File.toml");
    assert_eq!(file_uri_to_path(&uri).unwrap(), path);
    assert_eq!(
        path_to_file_uri(Path::new(r"\\?\C:\x y")).unwrap(),
        "file:///C:/x%20y"
    );
    assert_eq!(
        path_to_file_uri(Path::new(r"\\server\share\x")),
        Err(UriError::RemoteAuthority)
    );
    assert_eq!(file_uri_to_path("file:///x"), Err(UriError::NotAbsolute));
}
