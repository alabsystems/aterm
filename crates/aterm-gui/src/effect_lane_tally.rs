// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What armed a window's effect-lane slot, and how many slots each term has had served.
//!
//! The planner (`WindowState::plan_terminal_effect_lane_with_recording`) names the term that
//! armed each fresh slot it returns; a slot it keeps keeps its own term, and a park forgets it.
//! Serving a due slot (`WindowState::service_due_terminal_effect_tick`) books one fire under
//! that term, and asks the window for one frame. A present that takes a pending slot before it
//! is due books nothing. `trail status` prints the pending slot's term as `lane_term=` and the
//! window's tally as `lane_fires=`.

/// The planner's term that armed an effect-lane slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LaneTerm {
    /// The decoration hold, last extended by the load-shed fade alone
    /// (`WindowState::note_shed_fade_animating`).
    Fade,
    /// The decoration hold, last extended by another site (a song, a word, a streak, a dog).
    Deco,
    /// The resident pet's coarse offer.
    Pet,
    /// The celebration's next event.
    Celebration,
    /// The cursor bodies' settle.
    Body,
    /// The glow: its frame train, or its coarse next change.
    Glow,
    /// The cursor's other frame-train dependents: the trail, the flier, the pet's train, the
    /// word decorations, the live cursor bodies.
    CursorFx,
    /// The output streak's next change.
    Streak,
    /// The static cursor cat's next change.
    StaticCat,
}

impl LaneTerm {
    /// Every term, in the order [`LaneTally::line`] prints them.
    pub(crate) const ALL: [Self; 9] = [
        Self::Fade,
        Self::Deco,
        Self::Pet,
        Self::Celebration,
        Self::Body,
        Self::Glow,
        Self::CursorFx,
        Self::Streak,
        Self::StaticCat,
    ];

    /// The term's word in `trail status`.
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Fade => "fade",
            Self::Deco => "deco",
            Self::Pet => "pet",
            Self::Celebration => "celebration",
            Self::Body => "body",
            Self::Glow => "glow",
            Self::CursorFx => "cursor_fx",
            Self::Streak => "streak",
            Self::StaticCat => "static_cat",
        }
    }

    const fn index(self) -> usize {
        self as usize
    }
}

/// One window's served effect-lane slots, counted by the term that armed each.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct LaneTally {
    fires: [u32; LaneTerm::ALL.len()],
}

impl LaneTally {
    /// One served slot that `term` armed.
    pub(crate) const fn book(&mut self, term: LaneTerm) {
        let slot = &mut self.fires[term.index()];
        *slot = slot.saturating_add(1);
    }

    /// The served slots `term` armed.
    pub(crate) const fn fires(&self, term: LaneTerm) -> u32 {
        self.fires[term.index()]
    }

    /// Every term's count, `fade:N,deco:N,…` in [`LaneTerm::ALL`]'s order.
    pub(crate) fn line(&self) -> String {
        LaneTerm::ALL
            .iter()
            .map(|term| format!("{}:{}", term.name(), self.fires(*term)))
            .collect::<Vec<_>>()
            .join(",")
    }
}
