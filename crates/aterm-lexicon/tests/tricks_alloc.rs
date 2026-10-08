// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Exact allocation regression for the kitty-command lookup.
//!
//! `TrickLexicon::classify` runs on the typing path — once per word boundary
//! the person types — so after its caller-owned fold buffer has warmed it must
//! allocate NOTHING, on every lane: a plain hit, a marked surface, a raw
//! no-space-script run, the elongation fold's two retries, and a miss.
//!
//! Counted by `lexicon_alloc.rs`'s allocator, which counts only the THREAD
//! that armed it — so this pin and `scan_scratch_alloc.rs` share one binary
//! without bleeding into each other's count.

use aterm_lexicon::{Trick, TrickLexicon, TrickRole};

use crate::allocations_during;

/// A vocabulary that exercises the two lanes the English seed cannot: a
/// MARKED surface (the marks-required gate walks the token) and a raw
/// no-space-script run. English rides the embedded file.
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
    // (table, token, expected): every lane, hits and misses alike. The long
    // miss is the widest thing the fold buffer ever has to hold, so it is
    // also what the warmup pass sizes the buffer with.
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
    // Warmup: grow the caller's buffer to the widest token, and check every
    // lane really does what the comment says (a lane that silently missed
    // would make the count below vacuous).
    for (table, token, expected) in cases {
        assert_eq!(table.classify(token, &mut scratch), expected, "{token:?}");
    }

    let allocations = allocations_during(|| {
        for (table, token, expected) in cases {
            // `assert_eq!` would format on failure only; the comparison
            // itself allocates nothing.
            assert!(table.classify(token, &mut scratch) == expected);
        }
    });
    assert_eq!(allocations, 0, "a warmed classify must not allocate");
}
