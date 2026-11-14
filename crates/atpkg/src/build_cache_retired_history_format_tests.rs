// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Exercise format admission with Git's real key-only and explicit-value records.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Instant;

use super::super::GitCli;
use super::{ELAPSED, Read, Snapshot};

#[test]
fn actual_git_configuration_records_keep_every_format_guard() {
    let scratch = aterm_tempfile::Builder::new()
        .prefix("retired-config-")
        .tempdir()
        .unwrap();
    // No repository is opened here: format uses Git only to parse these exact bytes.
    // Invalid repository settings therefore cannot be refused by an earlier status read.
    let cases = [
        ("[advice]\n\tdetachedHead\n", false, Some(40)),
        ("[advice]\n\tdetachedHead\n", true, Some(40)),
        ("[core]\n\trepositoryFormatVersion = 0\n", true, Some(40)),
        ("[core]\n\trepositoryFormatVersion = 1\n", true, Some(40)),
        ("[extensions]\n\tobjectFormat = sha1\n", true, Some(40)),
        ("[extensions]\n\tobjectFormat = sha256\n", true, Some(64)),
        ("[extensions]\n\trefStorage = files\n", true, Some(40)),
        ("[include]\n\tpath\n", false, None),
        (
            "[includeIf \"gitdir:never-matches\"]\n\tpath\n",
            false,
            None,
        ),
        ("[fsck]\n\tmissingEmail\n", false, Some(40)),
        ("[fsck]\n\tmissingEmail\n", true, None),
        ("[extensions]\n\tworktreeConfig\n", false, None),
        ("[extensions]\n\tobjectFormat\n", false, None),
        ("[extensions]\n\tobjectFormat =\n", false, None),
        ("[extensions]\n\tobjectFormat = unknown\n", false, None),
        ("[extensions]\n\trefStorage\n", false, None),
        ("[extensions]\n\trefStorage =\n", false, None),
        ("[extensions]\n\trefStorage = unknown\n", false, None),
        ("[core]\n\trepositoryFormatVersion\n", false, None),
        ("[core]\n\trepositoryFormatVersion =\n", false, None),
        ("[core]\n\trepositoryFormatVersion = unknown\n", false, None),
        (
            "[extensions]\n\tobjectFormat = sha1\n\tobjectFormat\n",
            false,
            None,
        ),
        (
            "[extensions]\n\trefStorage = files\n\trefStorage\n",
            false,
            None,
        ),
        (
            "[core]\n\trepositoryFormatVersion = 0\n\trepositoryFormatVersion\n",
            false,
            None,
        ),
    ];
    for (config, witness, expected) in cases {
        let snapshot = Snapshot {
            stamps: BTreeMap::new(),
            files: BTreeMap::from([(PathBuf::from("config"), config.as_bytes().to_vec())]),
        };
        let read = Read {
            git: &GitCli,
            checkout: scratch.path(),
            deadline: Instant::now() + ELAPSED,
        };
        let result = read.format(&snapshot, witness);
        match expected {
            Some(width) => assert_eq!(result, Ok(width), "{config:?}, witness={witness}"),
            None => assert!(result.is_err(), "{config:?}, witness={witness}: {result:?}"),
        }
    }
}
