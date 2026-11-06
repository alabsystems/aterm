// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE HOLD IN THE AGENT'S OWN WORDS (`docs/DESIGN-harness-autopilot-2026-10-15.md` §14.4 and
//! §14.17 item 1, slice 4b): beside why Land holds ([`super::acts::LandHeld`], its legs and
//! counts), what the agent itself said is left and the files git found. The Open leg is the
//! agent's own answer to the land check (`land-check@v1`, slice 13a), so its quote is that
//! answer, item by item as the agent wrote them ([`land_check::listed_items`]), never the word
//! reader's family names; the repository leg names the changed and untracked files the probe
//! read ([`super::repo_work::RepoWork::paths`]).
//!
//! The loop tells it beside the hold (`IdleHost::land_said`, before `land_held`, and only where
//! either moved), so an item the agent rewords within one word family refreshes its quote though
//! the hold's legs and counts stay equal. Nothing the loop decides reads it back. Its words stay
//! in-process (limit 5): the window's glass, through the presentation sanitizer at store.

use crate::supervise::policy::land_check;

/// The most items of the agent's list a hold quotes ([`HoldSaid::open`]).
pub const QUOTED_ITEMS: usize = 4;

/// The most bytes one quoted item keeps: an item past it is cut at a word's edge, with `…`.
pub const ITEM_BYTES: usize = crate::supervise::policy::turn_end::SAID_CAP;

/// The most characters of the first item the chip's title, the hover and a worker's tip quote:
/// past it the item is cut at a word's edge, with `…`, and the rest counted (`(+3 more)`).
pub const SHORT_CHARS: usize = 48;

/// What leads the Open leg's quotes on every surface (`open in its report: “…”`), and its
/// words where the agent's list was not told (a codex-alab's own hold, `meta set harness_land`).
pub const OPEN_IN_REPORT: &str = "open in its report";

/// What a hold that only a person's need holds says in place of `not landed yet: needs you`
/// (§14.4, said once): the rest's own words ([`land_check::WAITS_ON_YOU`]).
pub const WAITS_ON_YOU: &str = land_check::WAITS_ON_YOU;

/// What a need's entry says the harness does beside it while it works on (§12.2.1).
pub const CARRYING_ON: &str = "carrying on";

/// What a need's entry says where nothing carries on beside it: the loop types nothing more
/// (the stop for replies that repeat with no work holds it, under any mode), or Land rests on the
/// need or landed beside it. Apart from the hold's own [`WAITS_ON_YOU`], which says Land has not
/// landed.
pub const NEED_WAITS: &str = "waits on you";

/// WHAT A NEED'S ENTRY SAYS THE HARNESS DOES BESIDE IT (§12.2.1; the second review of the
/// post-landing fixes, 2026-11-01): the menu bar's entry (`<program> needs you: … — <tail>`) and
/// the page's state line (`needs you: …; <tail>`) end with these words, which
/// the window reads back off the entry. The loop types nothing while the stop for replies that
/// repeat holds it (the post-landing review, 2026-11-02), and Land types nothing while it rests on
/// the need or after the work landed beside it, so there the work waits on the person.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum NeedTail {
    /// The harness works on past the need: Cruise, or Land with more than the need left, its
    /// typing not stopped.
    #[default]
    CarryingOn,
    /// The work waits on the person: the loop's typing stopped at a reply that repeats, or under
    /// Land it rests on the need or landed.
    WaitsOnYou,
}

impl NeedTail {
    /// Every tail, for a reader that strips whichever the entry carries.
    pub const ALL: [Self; 2] = [Self::CarryingOn, Self::WaitsOnYou];

    /// The tail's words.
    #[must_use]
    pub const fn words(self) -> &'static str {
        match self {
            Self::CarryingOn => CARRYING_ON,
            Self::WaitsOnYou => NEED_WAITS,
        }
    }

    /// The tail's words as a sentence (`Waits on you.`): what the chip's description speaks after
    /// the need (`harness_glass::need_spoken`), and what the forever lab hears there.
    #[must_use]
    pub fn sentence(self) -> String {
        let mut chars = self.words().chars();
        chars.next().map_or_else(String::new, |first| {
            format!("{}{}.", first.to_uppercase(), chars.as_str())
        })
    }
}

/// WHAT THE AGENT SAID IS LEFT, AND THE FILES GIT FOUND, at the land act's yield that read the
/// hold ([`HoldSaid::of`]). Empty where the yield read neither.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct HoldSaid {
    /// The agent's list of what is left (its answer to the land check), item by item as it wrote
    /// them: the first [`QUOTED_ITEMS`], each at most [`ITEM_BYTES`], with no curly double quote.
    pub open: Vec<String>,
    /// How many items it listed past [`Self::open`].
    pub more: u16,
    /// The changed or untracked files the repository's probe named: at most
    /// [`super::repo_work::NAMED_PATHS`], each printable ([`super::repo_work::printable`]).
    pub paths: Vec<String>,
}

impl HoldSaid {
    /// The words of a yield whose agent listed `listed` as left (`None`: no list stands) over a
    /// repository whose probe named `paths`.
    #[must_use]
    pub fn of(listed: Option<&str>, paths: &[String]) -> Self {
        let items = listed.map_or_else(Vec::new, land_check::listed_items);
        let more = u16::try_from(items.len().saturating_sub(QUOTED_ITEMS)).unwrap_or(u16::MAX);
        Self {
            open: items
                .iter()
                .take(QUOTED_ITEMS)
                .filter_map(|item| land_check::cut_bytes(item, ITEM_BYTES))
                .collect(),
            more,
            paths: paths.to_vec(),
        }
    }

    /// Whether it holds no word.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.open.is_empty() && self.paths.is_empty()
    }

    /// How many items it names past the first: the rest of [`Self::open`] and [`Self::more`].
    #[must_use]
    pub fn past_first(&self) -> usize {
        self.open.len().saturating_sub(1) + usize::from(self.more)
    }
}

/// `text` in the curly double quotes every surface quotes the agent in.
#[must_use]
pub fn quoted(text: &str) -> String {
    format!("\u{201c}{text}\u{201d}")
}

/// The count of items said past those quoted (` (+3 more)`), or nothing for none.
#[must_use]
pub fn more_words(n: usize) -> String {
    if n == 0 {
        String::new()
    } else {
        format!(" (+{n} more)")
    }
}

/// `text` cut to at most `chars` characters at a word's edge, with `…` at the cut: the first
/// item as the chip's title, the hover and a worker's tip quote it ([`SHORT_CHARS`]).
#[must_use]
pub fn cut_chars(text: &str, chars: usize) -> String {
    if text.chars().count() <= chars {
        return text.to_string();
    }
    let room = chars.saturating_sub(1);
    let end = text
        .char_indices()
        .nth(room)
        .map_or(text.len(), |(at, _)| at);
    let cut = if text[end..].starts_with(' ') {
        end
    } else {
        text[..end].rfind(' ').filter(|&at| at > 0).unwrap_or(end)
    };
    format!(
        "{}\u{2026}",
        text[..cut].trim_end_matches([' ', ',', ';', ':'])
    )
}

/// The files the repository leg names after its counts (`: wip-notes.txt`; `: a, b, c, …` where
/// the probe found `found` files, more than those), or nothing where none was named. A colon, not
/// brackets: the hover already says the hold inside its own (`driven by aterm (land: …)`). The
/// list is Land's prompt's ([`super::repo_work::files_list`]).
#[must_use]
pub fn files_words(paths: &[String], found: usize) -> String {
    if paths.is_empty() {
        return String::new();
    }
    format!(": {}", super::repo_work::files_list(paths, found))
}

#[cfg(test)]
#[path = "hold_said_tests.rs"]
mod tests;
