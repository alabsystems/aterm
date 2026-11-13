// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::fleet_db_reveals;

/// NEGATIVE CONTROL: with `--dir`'s folder read as a dump file, `dump --dir <folder>` (which
/// shows only masked rows) is gated: `` dump --dir /r ``.
#[test]
fn fleet_db_s_plaintext_paths_are_its_show_flag_and_a_dump_to_a_file() {
    let words = |line: &str| -> Vec<String> { line.split(' ').map(str::to_owned).collect() };
    for gated in [
        "emails --show-passwords",
        "get emails a@b --reveal",
        "dump out.json",
        "dump --dir /r out.json",
        "dump out.json --dir /r",
    ] {
        assert!(fleet_db_reveals(&words(gated)).unwrap(), "{gated}");
    }
    for open in ["dump", "dump --dir /r", "emails", "put emails a@b {}"] {
        assert!(!fleet_db_reveals(&words(open)).unwrap(), "{open}");
    }
}
