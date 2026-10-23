// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The engine's file-URI codec (design D-22; slice N1).
//!
//! One canonical `file://` spelling with percent-encoding, so the client and
//! the server name a document the same way; the host's document admission
//! wraps it.
//!
//! The spelling: `file://` and the absolute path, every byte outside
//! `A-Z a-z 0-9 / - _ . ~ :` percent-encoded (`%20` for a space), so a URI is
//! one whitespace-free token on the control wire. A Windows drive path is
//! written with one URI root slash (`file:///C:/Users/...`, RFC 8089). Decoding
//! takes an empty or `localhost` authority only, refuses a query or fragment
//! (`?`, `#`) and a NUL, and answers an absolute path or an error: the codec
//! names a file, it never opens one.

use std::path::{Path, PathBuf};

/// Why a URI does not name a local absolute path, or a path has no URI.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UriError {
    /// Not a `file://` URI.
    UnsupportedScheme,
    /// A host other than the empty one or `localhost`, or a UNC path: the
    /// document host is local-only.
    RemoteAuthority,
    /// A query or fragment, a missing path, or an encoded NUL.
    Malformed,
    /// A bad percent escape, or bytes that are not UTF-8 (a path the wire
    /// cannot spell).
    InvalidEncoding,
    /// The decoded path is not absolute.
    NotAbsolute,
}

impl std::fmt::Display for UriError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::UnsupportedScheme => "only local file: documents are supported",
            Self::RemoteAuthority => "remote file URI authorities are not allowed",
            Self::Malformed => "malformed file URI",
            Self::InvalidEncoding => "file URI has invalid percent encoding",
            Self::NotAbsolute => "document path must be absolute",
        })
    }
}

impl std::error::Error for UriError {}

/// The absolute path a `file://` URI names.
pub fn file_uri_to_path(uri: &str) -> Result<PathBuf, UriError> {
    let rest = uri
        .strip_prefix("file://")
        .ok_or(UriError::UnsupportedScheme)?;
    if rest.contains(['?', '#']) {
        return Err(UriError::Malformed);
    }
    let (authority, encoded_path) = if rest.starts_with('/') {
        ("", rest)
    } else {
        rest.split_once('/')
            .map(|(authority, _path)| (authority, &rest[authority.len()..]))
            .ok_or(UriError::Malformed)?
    };
    if !authority.is_empty() && !authority.eq_ignore_ascii_case("localhost") {
        return Err(UriError::RemoteAuthority);
    }
    let decoded = percent_decode(encoded_path)?;
    if decoded.contains('\0') {
        return Err(UriError::Malformed);
    }
    #[cfg(windows)]
    let decoded = windows_drive_path_text(&decoded)?;
    let path = PathBuf::from(decoded);
    if !path.is_absolute() {
        return Err(UriError::NotAbsolute);
    }
    Ok(path)
}

/// RFC 8089 drive paths carry one URI root slash (`/C:/...`) which is not a
/// Windows path root. Remove exactly that slash; all other leading-slash forms
/// keep their meaning and are then accepted or refused by `Path`. A
/// `//server/...` path names a remote share even with an empty URI authority
/// (`file:////server/share`), so it is refused. Applied by
/// [`file_uri_to_path`] on Windows; public so every host can test the rule.
pub fn windows_drive_path_text(decoded: &str) -> Result<String, UriError> {
    let bytes = decoded.as_bytes();
    if bytes.len() >= 3 && bytes[0] == b'/' && bytes[1].is_ascii_alphabetic() && bytes[2] == b':' {
        return Ok(decoded[1..].to_string());
    }
    if decoded.starts_with("//") {
        return Err(UriError::RemoteAuthority);
    }
    Ok(decoded.to_string())
}

fn percent_decode(value: &str) -> Result<String, UriError> {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'%' {
            decoded.push(bytes[index]);
            index += 1;
            continue;
        }
        if index + 2 >= bytes.len() {
            return Err(UriError::InvalidEncoding);
        }
        let high = nibble(bytes[index + 1]).ok_or(UriError::InvalidEncoding)?;
        let low = nibble(bytes[index + 2]).ok_or(UriError::InvalidEncoding)?;
        decoded.push(high << 4 | low);
        index += 3;
    }
    String::from_utf8(decoded).map_err(|_| UriError::InvalidEncoding)
}

/// One hex digit's value, either case.
const fn nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// The `file://` URI of an absolute path, percent-encoded so it is one
/// whitespace-free wire token. On Windows the `\\?\` prefix is dropped,
/// separators become `/`, a drive path gains its URI root slash, and a UNC
/// path is refused.
pub fn path_to_file_uri(path: &Path) -> Result<String, UriError> {
    let path = path.to_str().ok_or(UriError::InvalidEncoding)?;
    let mut normalized = if cfg!(windows) {
        path.strip_prefix(r"\\?\")
            .unwrap_or(path)
            .replace('\\', "/")
    } else {
        path.to_string()
    };
    if cfg!(windows)
        && (normalized.starts_with("//")
            || normalized
                .get(..4)
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case("UNC/")))
    {
        return Err(UriError::RemoteAuthority);
    }
    if cfg!(windows) && !normalized.starts_with('/') {
        normalized.insert(0, '/');
    }
    let mut uri = String::from("file://");
    for byte in normalized.bytes() {
        if file_uri_keeps(byte) {
            uri.push(char::from(byte));
        } else {
            push_percent_escape(&mut uri, byte);
        }
    }
    Ok(uri)
}

/// Append `byte` as `%XY`, two upper-case hex digits. Kept apart from the
/// escape decision so each digit's index into `HEX` is provably below 16
/// whatever the caller's branch was.
fn push_percent_escape(uri: &mut String, byte: u8) {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    uri.push('%');
    uri.push(char::from(HEX[usize::from(byte >> 4)]));
    uri.push(char::from(HEX[usize::from(byte & 0x0f)]));
}

trust_contract_fn! {
    /// Whether [`path_to_file_uri`] writes `byte` as itself: an ASCII letter
    /// or digit, or one of `/ - _ . ~ :`. Every other byte, a space and `%`
    /// among them, is percent-encoded, so the URI is one whitespace-free wire
    /// token that decodes back to exactly the path.
    #[expect(
        clippy::manual_range_contains,
        reason = "Trust kernel-imports a bool tree of comparisons, not a call to `RangeInclusive::contains` (E6 refuses calls)"
    )]
    fn file_uri_keeps(byte: u8) -> bool
    clauses [ensures result == ((byte >= 48 && byte <= 57)
        || ((byte >= 65 && byte <= 90)
        || ((byte >= 97 && byte <= 122)
        || (byte == 47 || (byte == 45 || (byte == 95 || (byte == 46 || (byte == 126 || byte == 58))))))))
        by file_uri_keeps_exactly_the_unescaped_set]
    {
        (byte >= b'0' && byte <= b'9')
            || (byte >= b'A' && byte <= b'Z')
            || (byte >= b'a' && byte <= b'z')
            || byte == b'/'
            || byte == b'-'
            || byte == b'_'
            || byte == b'.'
            || byte == b'~'
            || byte == b':'
    }
}

// Spec `file-uri-unescaped-set` (SPECS.md), kernel-checked against the
// compiled body: the set is the clause, byte for byte.
verify_island! {
    clean {
        theorem file_uri_keeps_exactly_the_unescaped_set (byte : UInt8) :
            trust_import_amacs__protocol__uri__file_uri_keeps byte =
              ((decide (48 <= byte) && decide (byte <= 57))
                || ((decide (65 <= byte) && decide (byte <= 90))
                || ((decide (97 <= byte) && decide (byte <= 122))
                || (decide (byte = 47) || (decide (byte = 45) || (decide (byte = 95)
                || (decide (byte = 46) || (decide (byte = 126) || decide (byte = 58))))))))) := by
          simp only [trust_import_amacs__protocol__uri__file_uri_keeps]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_uri_keeps_the_set_it_replaced() {
        for byte in u8::MIN..=u8::MAX {
            let before = byte.is_ascii_alphanumeric()
                || matches!(byte, b'/' | b'-' | b'_' | b'.' | b'~' | b':');
            assert_eq!(file_uri_keeps(byte), before, "byte {byte:#04x}");
        }
    }

    /// An absolute path in this host's spelling.
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
        // Either hex case decodes; `localhost` is the empty authority.
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
    }

    #[test]
    fn windows_drive_text_loses_only_its_uri_root_slash() {
        assert_eq!(
            windows_drive_path_text("/C:/Users//Ada/My File.toml").unwrap(),
            "C:/Users//Ada/My File.toml"
        );
        assert_eq!(windows_drive_path_text("/z:/").unwrap(), "z:/");
        assert_eq!(windows_drive_path_text("/tmp/x").unwrap(), "/tmp/x");
        assert_eq!(
            windows_drive_path_text("//server/share/aterm.toml"),
            Err(UriError::RemoteAuthority)
        );
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
        // A rooted path with no drive is not absolute on Windows.
        assert_eq!(file_uri_to_path("file:///x"), Err(UriError::NotAbsolute));
    }
}
