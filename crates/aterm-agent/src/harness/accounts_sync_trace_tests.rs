// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The sync's reads under a trace: the default profile's address as a standing, and the local
//! source reader's stamps.

use super::*;
use aterm_uds::trace::{self, Moved, Now};

/// A profile as Claude Code writes it: its address, and a count it bumps on every start.
fn profile_text(email: &str, starts: u32) -> String {
    format!(
        "{{\"numStartups\":{starts},\"oauthAccount\":{{\"accountUuid\":\"one\",\
         \"emailAddress\":\"{email}\"}}}}"
    )
}

/// A traced read of the default profile's address records a standing of the address alone, and
/// no stamp: Claude Code's rewrite on every start leaves it standing, another address moves it.
/// It answers as the untraced read does.
///
/// NEGATIVE CONTROL (2026-11-15): `signed_in_email` answering
/// `profile_email(path, &read_local_source_text)` with the scope armed (its read recorded as a
/// stamp, no standing) failed at the standings' record, left `[]`, right
/// `[".../.claude.json"]`; restored, the file touched.
#[test]
fn a_profile_rewrite_that_keeps_its_address_leaves_a_traced_email_read_standing() {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let path = dir.path().join(".claude.json");
    std::fs::write(&path, profile_text("a@example.com", 1)).expect("the profile");

    let (email, trace) = trace::scope(|| signed_in_email(&path));
    assert_eq!(email.as_deref(), Some("a@example.com"));
    assert_eq!(email, profile_email(&path, &read_local_source_text));
    assert_eq!(trace.standings().collect::<Vec<_>>(), [path.as_path()]);
    assert_eq!(trace.files().count(), 0, "{trace:?}");
    assert_eq!(trace.unkept(), None, "{trace:?}");
    let stands = || trace.stands(Now::at(0), std::time::Instant::now());
    assert_eq!(stands(), Ok(()));

    std::fs::write(&path, profile_text("a@example.com", 40)).expect("a start counted");
    assert_eq!(stands(), Ok(()), "a rewrite that kept the address moved it");
    std::fs::write(&path, profile_text("b@example.com", 40)).expect("another address");
    assert_eq!(
        stands(),
        Err(Moved::Standing(path.clone())),
        "another address signed in left the trace standing"
    );
}

/// The local source reader records the stamp of what it opened, before its open, and that
/// nothing was at a path it found empty; a rewrite of what it read moves the trace.
///
/// NEGATIVE CONTROL (2026-11-15): `read_local_text_bounded` without its
/// `trace::fs::opening(path)` failed at the files' record, left `[]`, right
/// `[(".../absent.toml", false), (".../fleet-accounts.toml", true)]`; restored, the file
/// touched.
#[test]
fn the_local_source_reader_records_its_file_and_a_miss() {
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let fleet = dir.path().join("fleet-accounts.toml");
    let absent = dir.path().join("absent.toml");
    std::fs::write(&fleet, "[[accounts]]\n").expect("the fleet");

    let ((read, missing), trace) = trace::read_as_if_later(Duration::from_secs(60), || {
        trace::scope(|| {
            (
                read_local_source_text(&fleet),
                read_local_source_text(&absent),
            )
        })
    });
    assert_eq!(read.expect("the fleet read"), "[[accounts]]\n");
    assert_eq!(
        missing.map_err(|error| error.kind()),
        Err(io::ErrorKind::NotFound)
    );
    assert_eq!(
        trace
            .files()
            .map(|(path, stamp)| (path.to_path_buf(), stamp.is_some()))
            .collect::<Vec<_>>(),
        [(absent.clone(), false), (fleet.clone(), true)]
    );
    assert_eq!(trace.unkept(), None, "{trace:?}");
    let stands = || trace.stands(Now::at(0), std::time::Instant::now());
    assert_eq!(stands(), Ok(()));
    std::fs::write(&fleet, "[[accounts]]\nemail = \"a@example.com\"\n").expect("a new row");
    assert_eq!(stands(), Err(Moved::File(fleet.clone())));
}
