// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `TrickLexicon::classify` runs once per typed word boundary, so with its caller-owned fold
//! buffer warmed it allocates nothing on every lane: a plain hit, a marked surface, a raw
//! no-space-script run, the elongation fold's retries, and a miss.

use aterm_lexicon::{Trick, TrickLexicon, TrickRole};

use crate::{allocations_during, peak_bytes_during};

/// The two lanes the English seed lacks: a marked surface (the marks-required gate walks the
/// token) and a raw no-space-script run.
const LANES: &str = r#"
[[trick]]
id    = "sit"
lang  = "es"
words = ["siéntate"]

[[trick]]
id    = "sit"
lang  = "ja"
words = ["おすわり"]
cjk   = true
"#;

#[test]
fn a_warmed_classify_is_allocation_free_on_every_lane() {
    let en = TrickLexicon::shared_en();
    let lanes = TrickLexicon::from_source(LANES, &["all"]).expect("the lane vocabulary parses");
    assert!(lanes.conflicts().is_empty(), "{:?}", lanes.conflicts());

    let plain_sit = Some(TrickRole::Trick {
        trick: Trick::Sit,
        needs_vocative: false,
    });
    // Every lane, hits and misses. The long miss is the widest token the fold buffer holds.
    let cases: [(&TrickLexicon, &str, Option<TrickRole>); 9] = [
        (en, "sit", plain_sit),
        (en, "SIT", plain_sit),
        (en, "kitty", Some(TrickRole::Vocative)),
        (en, "siiiiiiiit", plain_sit),
        (
            en,
            "nooooooooo",
            Some(TrickRole::Trick {
                trick: Trick::Scold,
                needs_vocative: true,
            }),
        ),
        (&lanes, "siéntate", plain_sit),
        (&lanes, "sientate", None),
        (&lanes, "おすわり", plain_sit),
        (
            en,
            "zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz",
            None,
        ),
    ];

    let mut scratch = String::new();
    // Warmup: grows the buffer to the widest token and checks every lane hits as expected (a
    // lane that silently missed would make the count vacuous).
    for (table, token, expected) in cases {
        assert_eq!(table.classify(token, &mut scratch), expected, "{token:?}");
    }

    let allocations = allocations_during(|| {
        for (table, token, expected) in cases {
            assert!(table.classify(token, &mut scratch) == expected);
        }
    });
    assert_eq!(allocations, 0, "a warmed classify must not allocate");
}

/// Every launch compiles the embedded vocabulary (the config worker's `aterm-tricks-compile`
/// thread), and what that heap peaks at stays dirty in the allocator's regions after it returns:
/// a run-time TOML decode of the 360 KB file peaked near 7 MB (docs/measured/
/// idle-memory-2026-10-27.md). Built from the rows the build decoded, it holds little more than
/// the table it returns.
#[test]
fn compiling_the_embedded_vocabulary_peaks_under_2_mib() {
    for langs in [&["en"][..], &["all"][..]] {
        let peak = peak_bytes_during(|| {
            drop(std::hint::black_box(TrickLexicon::with_languages(langs)));
        });
        eprintln!("with_languages({langs:?}) peaked at {peak} bytes");
        assert!(
            peak <= 2 << 20,
            "with_languages({langs:?}) peaked at {peak} bytes"
        );
    }
}
