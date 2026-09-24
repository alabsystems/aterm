// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The seamless-handoff carry: plain data the outgoing process ships and the
//! successor re-seeds (`MessageCenter::carried` / `seed_carried`). Strings
//! and ints only — the host serialises it with whatever the handoff already
//! uses; nothing here knows a format.

/// One live message as carried: every field a string or a number, every
/// intent as its [`crate::model::Intent::encode`] form, the hold as its
/// [`crate::model::Hold::encode`] word.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CarriedMessage {
    /// The parent's id — the successor keeps it and raises `next_id` past it.
    pub id: u64,
    /// The wall clock at ingress, from the parent.
    pub unix_ms: u64,
    /// The tag word; an unknown one drops the row on seed.
    pub tag: String,
    /// The severity word; an unknown one drops the row on seed.
    pub severity: String,
    /// The glyph; outside the closed set it becomes the fallback.
    pub glyph: char,
    /// The title.
    pub title: String,
    /// The detail lines.
    pub detail: Vec<String>,
    /// Encoded intents; an unknown one is dropped silently.
    pub actions: Vec<String>,
    /// `default` | `for:<secs>` | `live:<secs>` | `standing` | `ask:<secs>`.
    pub hold: String,
    /// The supersede key.
    pub key: Option<String>,
    /// The meter fill, when metered.
    pub fill_permille: Option<u16>,
    /// The meter's stats (volatile, but the successor's first frame shows
    /// the parent's last).
    pub stats: String,
    /// The meter was busy ([`crate::model::Meter::busy`]); an older build's
    /// carry reads as still.
    pub busy: bool,
    /// Whether the row was on glass when the parent froze.
    pub on_glass: bool,
    /// Whether the band painted `detail[0]` beside the title
    /// (`Message::excerpt`); an older parent's rows carry `true`, and paint as
    /// they did.
    pub excerpt: bool,
    /// The words the row finishes with (`Message::finished`); an older
    /// parent's rows carry none and derive them from the title.
    pub finished: Option<String>,
}

/// Everything the successor needs: the next id and the live rows in glass
/// order (on-glass rows first). No log lines cross: the parent writes its
/// own record to the end and flushes its writer before it execs; the
/// successor's record begins at Commit (design §3.7, revised 2026-09-22 —
/// a carry captured before the freeze shipped nothing, and the lines
/// posted under the freeze died with the parent).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Carry {
    /// The parent's next id.
    pub next_id: u64,
    /// The live rows.
    pub live: Vec<CarriedMessage>,
}

impl Carry {
    /// `true` when nothing is carried.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.live.is_empty()
    }
}
