// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn an_in_flight_cut_refuses_a_fresh_cut() {
    let journal = JournalHeader {
        format: JOURNAL_FORMAT,
        version: "0.92.0".into(),
        build_number: 1_790_200_000,
        commit: "a".repeat(40),
        done: vec!["lock".into()],
    };
    for kind in [CutKind::Real, CutKind::DryRun] {
        assert!(fresh_cut_journal_triage(Some(&journal), kind).is_err());
    }
}

/// A FILE THAT IS NO CUT JOURNAL IS TOLD FROM A READ THAT FAILED: text that does not parse,
/// a header naming no build, bytes that are not UTF-8, and a path that is no regular file (a
/// directory, a FIFO, a symbolic link, even one to a journal, or a loop: no journal read
/// follows a link) read the same on every retry (Invalid); a read the permissions refuse may
/// clear (Read); no file is no journal. The release decides on the first and retries the
/// second (release::Run::withdraw_signed). NEGATIVE CONTROL: a path that is no regular file
/// was Read, so the release stayed COULD NOT RUN on every retry, told to make it readable.
#[test]
fn a_file_that_is_no_cut_journal_is_told_from_a_read_that_failed() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let path = dir.path().join("cut-state.toml");
    assert!(matches!(JournalHeader::read_classified(&path), Ok(None)));
    for text in [
        &b"format = \"no\n"[..],
        b"version = \"0.191.0\"\ncommit = \"aa\"\n",
        b"version = \"\xff\"\n",
    ] {
        fs::write(&path, text).unwrap();
        let read = JournalHeader::read_classified(&path);
        assert!(
            matches!(read, Err(JournalUnread::Invalid(_))),
            "{:?}: {read:?}",
            String::from_utf8_lossy(text)
        );
    }
    use std::os::unix::fs::PermissionsExt as _;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).unwrap();
    let read = JournalHeader::read_classified(&path);
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(matches!(read, Err(JournalUnread::Read(_))), "{read:?}");

    let journal = dir.path().join("journal.toml");
    fs::write(
        &journal,
        format!(
            "format = {JOURNAL_FORMAT}\nversion = \"0.191.0\"\nbuild_number = 191\n\
             commit = \"{}\"\n",
            "a".repeat(40)
        ),
    )
    .unwrap();
    assert!(matches!(
        JournalHeader::read_classified(&journal),
        Ok(Some(_))
    ));
    fs::remove_file(&path).unwrap();
    for what in [
        "a directory",
        "a FIFO",
        "a link to a journal",
        "a link loop",
    ] {
        match what {
            "a directory" => fs::create_dir(&path).unwrap(),
            "a FIFO" => assert!(command("mkfifo").arg(&path).status().unwrap().success()),
            "a link to a journal" => std::os::unix::fs::symlink(&journal, &path).unwrap(),
            _ => std::os::unix::fs::symlink(&path, &path).unwrap(),
        }
        let read = JournalHeader::read_classified(&path);
        if what == "a directory" {
            fs::remove_dir(&path).unwrap();
        } else {
            fs::remove_file(&path).unwrap();
        }
        assert!(
            matches!(&read, Err(JournalUnread::Invalid(error))
                if error.to_string().contains("not a regular file")),
            "{what}: {read:?}"
        );
    }
}
