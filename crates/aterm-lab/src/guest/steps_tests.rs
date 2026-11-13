// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn args(words: &[&str]) -> Vec<String> {
    words.iter().map(|w| (*w).to_owned()).collect()
}

/// A scratch directory under the test's temp dir, removed when dropped.
struct Scratch(aterm_tempfile::TempDir);

impl Scratch {
    fn new(tag: &str) -> Scratch {
        let dir = aterm_tempfile::Builder::new()
            .prefix(&format!("guest-{tag}-"))
            .suffix(".noindex")
            .tempdir()
            .expect("scratch dir");
        std::fs::create_dir_all(dir.path().join("seed")).unwrap();
        std::fs::create_dir_all(dir.path().join("joiner")).unwrap();
        Scratch(dir)
    }
}

/// Each file reads as the joiner holds it: the seed's bytes, other bytes, or none; its time is
/// the joiner's file's own, and no value reaches the output.
#[test]
fn arrivals_say_when_each_file_landed_and_whether_it_is_the_seeds() {
    let s = Scratch::new("arrivals");
    let (seed, joiner) = (s.0.path().join("seed"), s.0.path().join("joiner"));
    std::fs::write(seed.join("gh_access_token"), "nux-synthetic-github-1").unwrap();
    std::fs::write(joiner.join("gh_access_token"), "nux-synthetic-github-1").unwrap();
    std::fs::write(seed.join("ai-accounts.csv"), "claude,a@example.invalid\n").unwrap();
    std::fs::write(joiner.join("ai-accounts.csv"), "claude,b@example.invalid\n").unwrap();
    std::fs::write(seed.join("gh_access_token_alabsystems"), "nux-synthetic-p").unwrap();
    let mut out = Vec::new();
    arrivals(
        &args(&[
            seed.to_str().unwrap(),
            joiner.to_str().unwrap(),
            "gh_access_token",
            "gh_access_token_alabsystems",
            "ai-accounts.csv",
        ]),
        &mut out,
    )
    .unwrap();
    let text = String::from_utf8(out).unwrap();
    let lines: Vec<Vec<&str>> = text.lines().map(|l| l.split(' ').collect()).collect();
    assert_eq!(lines.len(), 3, "{text}");
    assert_eq!((lines[0][0], lines[0][2]), ("gh_access_token", "same"));
    assert_eq!(
        lines[1],
        ["gh_access_token_alabsystems", "none", "absent"],
        "{text}"
    );
    assert_eq!((lines[2][0], lines[2][2]), ("ai-accounts.csv", "differs"));
    let landed = std::fs::metadata(joiner.join("gh_access_token"))
        .unwrap()
        .modified()
        .unwrap();
    assert_eq!(lines[0][1], epoch_secs(landed));
    assert!(
        !text.contains("synthetic") && !text.contains("example"),
        "no value is printed: {text}"
    );
    assert!(arrivals(&args(&["/s", "/j"]), &mut Vec::new()).is_err());
}

#[test]
fn a_files_time_is_epoch_seconds_to_the_nanosecond() {
    let at = UNIX_EPOCH + Duration::new(1_761_650_000, 5_000_000);
    assert_eq!(epoch_secs(at), "1761650000.005000000");
}

/// The enrolment names a home and its authority, and nothing else.
#[cfg(unix)]
#[test]
fn enrol_names_a_home_and_its_authority() {
    assert!(enrol(&args(&["/m"]), &mut Vec::new()).is_err());
    assert!(enrol(&args(&["/m", "/s", "/x"]), &mut Vec::new()).is_err());
}

/// A genesis names a home and the new file it writes, and nothing else.
#[cfg(unix)]
#[test]
fn a_genesis_names_a_home_and_its_file() {
    assert!(genesis(&args(&["/s"]), &mut Vec::new()).is_err());
    assert!(genesis(&args(&["/s", "/g", "/x"]), &mut Vec::new()).is_err());
}

/// A repair names two homes and its rounds, and an authority only when it names one; its
/// rounds are bounded before anything opens a custody.
#[cfg(unix)]
#[test]
fn a_repair_names_two_homes_its_rounds_and_maybe_an_authority() {
    let three = args(&["/j", "/s", "8"]);
    let (home, peer, rounds, authority) = repair_words(&three).unwrap();
    assert_eq!(
        (home, peer, rounds, authority),
        (Path::new("/j"), Path::new("/s"), 8, None)
    );
    let four = args(&["/j", "/s", "8", "/a"]);
    let (.., authority) = repair_words(&four).unwrap();
    assert_eq!(authority, Some(Path::new("/a")));
    for words in [
        &["/j", "/s"][..],
        &["/j", "/s", "0"],
        &["/j", "/s", "33"],
        &["/j", "/s", "many"],
        &["/j", "/s", "8", "/a", "/x"],
    ] {
        assert!(repair_words(&args(words)).is_err(), "{words:?}");
    }
    assert!(repair(&args(&["/j"]), &mut Vec::new()).is_err());
}

/// What a repair took is HOME's own counts: control records, table operations with their
/// conflicts, and historical keys; a round with no completed table round adds control alone.
#[cfg(unix)]
#[test]
fn a_repair_sums_what_its_home_took() {
    let mut took = Took::default();
    let table = aterm_db::replica::Report {
        received: 16,
        conflicts_received: 1,
        keys_received: 2,
        sent: 99,
        ..aterm_db::replica::Report::default()
    };
    took.add(
        Report {
            sent: 7,
            received: 3,
            deferred: 0,
        },
        Some(table),
    );
    took.add(
        Report {
            sent: 0,
            received: 1,
            deferred: 4,
        },
        None,
    );
    assert_eq!(
        took,
        Took {
            control: 4,
            rows: 17,
            keys: 2
        }
    );
}

/// The browse's words are read before anything runs, and each transcript has its own file.
#[test]
fn a_browse_names_its_seconds_its_folder_and_at_least_one_service() {
    assert!(browse(&args(&["3"])).is_err());
    assert!(browse(&args(&["3", "/n"])).is_err());
    assert!(browse(&args(&["three", "/n", "_alab._tcp"])).is_err());
    assert!(dns_sd(&args(&["2", "/n/out.txt"])).is_err());
    assert!(
        dns_sd(&args(&[
            "two",
            "/n/out.txt",
            "-L",
            "x",
            "_alab._tcp",
            "local."
        ]))
        .is_err()
    );
    assert_eq!(
        transcript(Path::new("/n"), "_alab-vault._tcp"),
        PathBuf::from("/n/heard_alab-vault._tcp.txt")
    );
}
