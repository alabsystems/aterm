// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Seamless-handoff carry data (`MessageCenter::carried` / `seed_carried`).

/// One live message as carried across seamless handoff.
#[allow(
    clippy::struct_excessive_bools,
    reason = "a plain wire record: each flag is an independent fact the handoff ships"
)]
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
    /// Original posted severity when different from the current mark.
    pub posted_severity: Option<String>,
    /// The glyph; outside the closed set it becomes the fallback.
    pub glyph: char,
    /// The title.
    pub title: String,
    /// Short title for the band (`Message::short_title`).
    pub short_title: Option<String>,
    /// The detail lines.
    pub detail: Vec<String>,
    /// Encoded intents; an unknown one is dropped silently.
    pub actions: Vec<String>,
    /// The hold, as [`crate::model::Hold::encode`] spells it.
    pub hold: String,
    /// The supersede key.
    pub key: Option<String>,
    /// The meter fill, when metered.
    pub fill_permille: Option<u16>,
    /// Meter stats from the parent's last frame.
    pub stats: String,
    /// Whether the meter was busy ([`crate::model::Meter::busy`]).
    pub busy: bool,
    /// Whether the row was on glass when the parent froze.
    pub on_glass: bool,
    /// Whether the band painted `detail[0]` beside the title (`Message::excerpt`).
    pub excerpt: bool,
    /// Words the row finishes with (`Message::finished`).
    pub finished: Option<String>,
    /// Whether the row is a record about a previous run (`Message::retrospective`).
    pub retrospective: bool,
    /// Whether the row is wire-owned ([`crate::model::Origin::wire_owned`]).
    pub wire: bool,
    /// Duplicate posts folded into the row (`Live::repeats`, 1 = posted once).
    pub repeats: u32,
}

/// Next message id and live rows in glass order (on-glass rows first).
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
    pub const fn is_empty(&self) -> bool {
        self.live.is_empty()
    }
}
