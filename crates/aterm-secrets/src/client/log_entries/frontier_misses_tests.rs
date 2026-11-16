// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The misses kept beside the state ([`MISSES_FILE`]): only a miss is written and only a miss is
//! read, and a file that does not read whole is no answer.

use super::*;

fn miss(n: u8, witness: Witness) -> Answer {
    Answer {
        key: WitnessKey {
            applied: [n; 32],
            heads: [n + 1; 32],
            envelope: [n + 2; 32],
            budget: 7,
        },
        order: if witness == Witness::Unsearched {
            [n + 3; 32]
        } else {
            [0; 32]
        },
        witness,
    }
}

/// `file` with its last 32 bytes the digest of the rest, as a writer of the format leaves it.
fn summed(mut file: Vec<u8>) -> Vec<u8> {
    let body = file.len() - 32;
    let sum = Sha256::digest(&file[..body]);
    file[body..].copy_from_slice(&sum);
    file
}

/// What is written is read back, and nothing else is: a file with one bit changed or cut short
/// anywhere is no answer, and so is a well-formed one that claims the applied log is held, that
/// holds more misses than a writer keeps, or whose misses another rule's search found.
#[test]
fn a_misses_file_reads_back_and_a_file_that_is_not_one_is_no_answer() {
    let misses = vec![
        miss(1, Witness::Absent),
        miss(9, Witness::Unsearched),
        miss(17, Witness::Short),
    ];
    let file = encode_misses(&misses);
    assert_eq!(file.len(), MISSES_MAGIC.len() + 3 * MISS_LEN + 32);
    assert_eq!(parse_misses(&file), Some(misses));
    for at in 0..file.len() {
        let mut changed = file.clone();
        changed[at] ^= 1;
        assert_eq!(parse_misses(&changed), None, "byte {at} changed");
        assert_eq!(parse_misses(&file[..at]), None, "cut at {at}");
    }

    // Well-formed, with a digest that matches: the answer's byte is the last of its miss.
    let one = encode_misses(&[miss(1, Witness::Absent)]);
    let answer = one.len() - 32 - 1;
    for (claim, read) in [(0, false), (1, true), (2, true), (3, true), (4, false)] {
        let mut claimed = one.clone();
        claimed[answer] = claim;
        assert_eq!(
            parse_misses(&summed(claimed)).is_some(),
            read,
            "an answer of {claim}: only the three misses are read, never `Held`"
        );
    }
    let mut other_rule = one.clone();
    other_rule[answer - 4..answer].copy_from_slice(&(WITNESS_RULE + 1).to_le_bytes());
    assert_eq!(
        parse_misses(&summed(other_rule)),
        Some(Vec::new()),
        "a miss another rule's search found answers nothing"
    );
    let many: Vec<Answer> = (0..=MAX_MISSES as u8)
        .map(|n| miss(8 * n, Witness::Absent))
        .collect();
    assert_eq!(parse_misses(&encode_misses(&many)), None);
    assert!(parse_misses(&encode_misses(&many[1..])).is_some());
}

/// The file keeps the newest misses, each once; `Held` is never kept; a search cut short is
/// kept for each order of the log it ran over, and a complete answer for the same key replaces
/// them all.
#[test]
fn keeping_a_miss_keeps_the_newest_few_once_and_never_a_held_answer() {
    let home = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(home.path());
    fsx::ensure_private_dir(&paths.dir).unwrap();
    let file = paths.dir.join(MISSES_FILE);
    assert_eq!(read_misses(&paths), Vec::new(), "no file is no answer");

    keep_miss(&paths, miss(1, Witness::Held));
    assert!(!file.exists(), "a held answer was written");

    let misses: Vec<Answer> = (0..MAX_MISSES as u8 + 2)
        .map(|n| miss(8 * n, Witness::Absent))
        .collect();
    for miss in &misses {
        keep_miss(&paths, *miss);
    }
    assert_eq!(read_misses(&paths), misses[2..], "the oldest are dropped");
    let kept = std::fs::read(&file).unwrap();
    keep_miss(&paths, misses[3]);
    keep_miss(&paths, miss(1, Witness::Held));
    assert_eq!(std::fs::read(&file).unwrap(), kept);

    // Two orders of one log, each searched to its budget; then a search that completed.
    let first = miss(200, Witness::Unsearched);
    let second = Answer {
        order: [9; 32],
        ..first
    };
    keep_miss(&paths, first);
    keep_miss(&paths, second);
    let read = read_misses(&paths);
    assert!(read.contains(&first) && read.contains(&second));
    let whole = Answer {
        order: [0; 32],
        witness: Witness::Absent,
        ..first
    };
    keep_miss(&paths, whole);
    let read = read_misses(&paths);
    assert!(read.contains(&whole) && !read.contains(&first) && !read.contains(&second));
    assert_eq!(read.len(), MAX_MISSES - 1);

    // A file that does not read is replaced by the next miss, not an error.
    fsx::write_atomic(&file, b"not a misses file", 0o600).unwrap();
    assert_eq!(read_misses(&paths), Vec::new());
    keep_miss(&paths, first);
    assert_eq!(read_misses(&paths), vec![first]);
}
