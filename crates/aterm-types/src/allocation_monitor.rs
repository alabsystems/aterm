// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The local owner-authorized read-capability attachment handshake.
//! Queuing an attachment does not establish an observed snapshot or leadership.

/// Admission before the client transfers its single monitor descriptor.
pub const ADMISSION: &str = "OK allocation monitor-fd";
/// The one-byte payload accompanying exactly one `SCM_RIGHTS` descriptor.
pub const FD_MARKER: u8 = b'm';
const QUEUED: &str = "OK allocation queued attachment=";

/// Canonical request line, including its newline. Generation is binding data,
/// never an issuer credential.
#[must_use]
pub fn request(generation: [u8; 16]) -> String {
    let mut line = String::from("allocation attach generation=");
    for byte in generation {
        use std::fmt::Write as _;
        let _ = write!(&mut line, "{byte:02x}");
    }
    line.push('\n');
    line
}

/// Parse only `attach generation=<32 lowercase hexadecimal digits>`. Selectors,
/// routing and exact owner authorization remain the server's independent gates.
#[must_use]
pub fn parse_attach(rest: &str) -> Option<[u8; 16]> {
    let mut words = rest.split_whitespace();
    (words.next()? == "attach").then_some(())?;
    let value = words.next()?.strip_prefix("generation=")?;
    (words.next().is_none()
        && value.len() == 32
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
    .then_some(())?;
    let mut generation = [0; 16];
    for (byte, pair) in generation
        .iter_mut()
        .zip(value.as_bytes().as_chunks::<2>().0)
    {
        let digit = |b: u8| if b <= b'9' { b - b'0' } else { b - b'a' + 10 };
        *byte = digit(pair[0]) * 16 + digit(pair[1]);
    }
    Some(generation)
}

/// The queued-only acknowledgement, including its newline. The receiver must
/// publish an actual observation separately before describing it as observed.
#[must_use]
pub fn queued(attachment: u64) -> String {
    format!("{QUEUED}{attachment}\n")
}

/// Parse a canonical nonzero attachment revision from one trimmed status line.
#[must_use]
pub fn parse_queued(line: &str) -> Option<u64> {
    let value = line.strip_prefix(QUEUED)?;
    (!value.is_empty() && !value.starts_with('0') && value.bytes().all(|b| b.is_ascii_digit()))
        .then_some(())?;
    value.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generation_and_queued_revision_have_one_bounded_canonical_shape() {
        for generation in [[0; 16], [255; 16], [9; 16]] {
            let line = request(generation);
            let rest = line.strip_prefix("allocation ").unwrap();
            assert_eq!(parse_attach(rest), Some(generation));
        }
        for rest in [
            "attach",
            "attach generation=a",
            "attach generation=0000000000000000000000000000000G",
            "attach generation=00000000000000000000000000000000 extra",
            "other generation=00000000000000000000000000000000",
        ] {
            assert!(parse_attach(rest).is_none(), "{rest}");
        }
        assert_eq!(parse_queued(queued(u64::MAX).trim_end()), Some(u64::MAX));
        for value in [
            "0",
            "01",
            "+1",
            "-1",
            "1 extra",
            "18446744073709551616",
            "1\n",
        ] {
            assert!(parse_queued(&format!("{QUEUED}{value}")).is_none());
        }
    }
}
