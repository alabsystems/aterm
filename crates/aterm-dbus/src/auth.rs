// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Client-side SASL EXTERNAL handshake without `NEGOTIATE_UNIX_FD`.

use crate::error::{Error, Result};

/// The longest handshake line this side reads from a server.
pub const MAX_LINE: usize = 4096;

/// The command that ends the handshake; message traffic follows it directly.
pub const BEGIN: &[u8] = b"BEGIN\r\n";

/// The client's first bytes for `uid`.
#[must_use]
pub fn first(uid: u32) -> Vec<u8> {
    let mut out = b"\0AUTH EXTERNAL ".to_vec();
    out.extend_from_slice(aterm_codec::hex::encode_lower(uid.to_string().as_bytes()).as_bytes());
    out.extend_from_slice(b"\r\n");
    out
}

/// Parses the server's `AUTH` response line (without `\r\n`) and returns its GUID.
///
/// # Errors
///
/// [`Error::Auth`] for `REJECTED`, `ERROR`, a `DATA` challenge, or a malformed line.
pub fn answer(line: &[u8]) -> Result<String> {
    let text =
        std::str::from_utf8(line).map_err(|_| Error::Auth("server line is not UTF-8".into()))?;
    let (command, rest) = text.split_once(' ').unwrap_or((text, ""));
    match command {
        "OK" if aterm_codec::hex::is_hex(rest, 32) => Ok(rest.to_ascii_lowercase()),
        "OK" => Err(Error::Auth(format!(
            "server GUID is not 32 hex digits: {rest:?}"
        ))),
        "REJECTED" => Err(Error::Auth(format!(
            "EXTERNAL rejected; the server accepts [{rest}]"
        ))),
        "ERROR" => Err(Error::Auth(format!("server error: {rest}"))),
        _ => Err(Error::Auth(format!("unexpected server line: {text:?}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_line_carries_the_decimal_uid_in_hex() {
        assert_eq!(first(1000), b"\0AUTH EXTERNAL 31303030\r\n");
        assert_eq!(first(0), b"\0AUTH EXTERNAL 30\r\n");
    }

    #[test]
    fn only_ok_with_a_guid_is_acceptance() {
        let guid = "8e4f0c2a1b7d6e5f8e4f0c2a1b7d6e5f";
        assert_eq!(answer(format!("OK {guid}").as_bytes()).unwrap(), guid);
        assert_eq!(
            answer(b"OK 8E4F0C2A1B7D6E5F8E4F0C2A1B7D6E5F").unwrap(),
            guid
        );
        for refusal in [
            &b"OK"[..],
            b"OK 8e4f",
            b"OK 8e4f0c2a1b7d6e5f8e4f0c2a1b7d6e5fzz",
            b"REJECTED DBUS_COOKIE_SHA1 ANONYMOUS",
            b"ERROR",
            b"DATA",
            b"AGREE_UNIX_FD",
            b"\xff",
            b"",
        ] {
            assert!(
                matches!(answer(refusal), Err(Error::Auth(_))),
                "{refusal:?}"
            );
        }
    }
}
