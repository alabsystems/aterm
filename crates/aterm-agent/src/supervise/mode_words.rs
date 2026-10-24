// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! EVERY MODE'S SENTENCE, ONE TABLE (`docs/DESIGN-harness-autopilot-2026-10-15.md` §12.4.1).
//!
//! The window says each Autopilot's sentence on the chip's title, in the records and on
//! Settings ▸ Harness's mode buttons; the forever lab (`aterm-lab`) judges the chip's title
//! against the same rows. The table lives beside [`Autopilot`] so both read the one source.

use super::Autopilot;

/// One mode's sentence in the form each surface renders it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModeWords {
    /// The mode the sentence says.
    pub mode: Autopilot,
    /// The Autopilot chip's title, after `autopilot: <mode> — `.
    pub chip: &'static str,
    /// The `Autopilot set to <mode>` record's line, after its tab.
    pub record: &'static str,
    /// Settings ▸ Harness's mode button, as its description says what a press does.
    pub button: &'static str,
}

/// EVERY MODE'S SENTENCE, ONE TABLE (§12.4.1), in [`Autopilot::ALL`]'s order.
pub const MODE_WORDS: [ModeWords; 3] = [
    ModeWords {
        mode: Autopilot::Cruise,
        chip: "keeps going for ever, finding more to do",
        record: "the harness keeps going for ever, finding more to do",
        button: "keep going for ever, finding more to do: nothing the agent says ends it",
    },
    ModeWords {
        mode: Autopilot::Land,
        chip: "wrapping up: landing everything on the remote main branch, then stopping",
        record: "the harness wraps it up: it lands everything on the remote main branch, then \
                 stops",
        button: "wrap it up: land everything on the remote main branch, then stop",
    },
    ModeWords {
        mode: Autopilot::Off,
        chip: "hands off",
        record: "the harness keeps its hands off",
        button: "the harness keeps its hands off this session",
    },
];

/// A TIMED CRUISE'S SENTENCES (§12.4.1's third row): Cruise until a time, then Land.
pub const TIMED_CRUISE_CHIP: &str = "keeps going until then, finding more to do";
/// The record of a timed Cruise set.
pub const TIMED_CRUISE_RECORD: &str =
    "the harness keeps going until that time, then wraps it up on the remote main branch";

/// `mode`'s sentences from [`MODE_WORDS`].
#[must_use]
pub const fn mode_words(mode: Autopilot) -> &'static ModeWords {
    match mode {
        Autopilot::Cruise => &MODE_WORDS[0],
        Autopilot::Land => &MODE_WORDS[1],
        Autopilot::Off => &MODE_WORDS[2],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each row says its own mode, in [`Autopilot::ALL`]'s order, and [`mode_words`] finds it.
    #[test]
    fn each_mode_has_its_own_row_in_order() {
        for (i, mode) in Autopilot::ALL.into_iter().enumerate() {
            assert_eq!(MODE_WORDS[i].mode, mode, "row {i}");
            assert_eq!(mode_words(mode).mode, mode, "{}", mode.word());
        }
    }
}
