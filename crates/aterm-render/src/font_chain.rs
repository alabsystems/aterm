// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Pure glyph-resolution tier chain, narrowing policy, and recovery-slot mapping.

use crate::FaceId;

/// One tier of the glyph-resolution chain, in consultation order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tier {
    /// Fontless box-drawing/block/braille synthesis (`crate::procedural`).
    Procedural = 0,
    /// The configured primary face's Unicode cmap.
    Primary = 1,
    /// The lazy broad-Unicode fallback chain (`Renderer::fallback_has`).
    Fallback = 2,
    /// The lazy monochrome symbol face (`Renderer::symbol_fallback_has`).
    Symbol = 3,
    /// The colour-emoji chain (`Renderer::color_font_has`).
    Color = 4,
    /// The runtime system resolver's pathname lane (`RuntimeFallback::resolve`).
    RuntimeDecisions = 5,
    /// The runtime resolver's I/O-free bundled lane (`RuntimeFallback::resolve_embedded_only`).
    RuntimeEmbedded = 6,
    /// The last-resort fontless symbol synthesis (`crate::procedural::covers_symbol`).
    Synthetic = 7,
}

impl Tier {
    /// Number of tiers — the exact bound on [`resolve_chain`]'s probe count.
    pub const COUNT: usize = 8;

    /// This tier's bit in a coverage bitvector.
    #[inline]
    #[must_use]
    pub const fn bit(self) -> u8 {
        1u8 << (self as u8)
    }
}

/// Chain-narrowing policy switches and code-point presentation flags.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ChainPolicy {
    /// Whether runtime fallback discovery is enabled (`Renderer::set_runtime_font_discovery`).
    pub runtime_discovery: bool,
    /// Whether the font generation is sealed against pathname discovery.
    pub sealed: bool,
    /// `aterm_grapheme::is_emoji_presentation(ch)`.
    pub wants_emoji: bool,
    /// Whether the code point is in a Private Use Area ([`is_private_use`]).
    pub prefers_symbol: bool,
}

/// Which per-code-point decision map a runtime resolution was recorded in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuntimeLane {
    /// `RuntimeFallback::decisions` (written by `resolve`).
    Decisions,
    /// `RuntimeFallback::embedded_decisions` (written by `resolve_embedded_only`).
    EmbeddedDecisions,
}

/// Record the rasterizer reads to recover which face a key draws from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecoverySlot {
    /// `Renderer::fallback_pick` — which broad-chain entry covered the char.
    FallbackPick,
    /// `Renderer::symbol_chain_pick` — which symbol-chain entry covered the char.
    SymbolSlot,
    /// `RuntimeFallback::parts_for`: either decision map.
    RuntimeDecision,
}

/// The chain's verdict for one code point.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resolution {
    /// Resolved on a configured tier; the key carries this face.
    Face(FaceId),
    /// Resolved by the runtime tier on `lane` ([`FaceId::RuntimeFallback`]).
    Runtime(RuntimeLane),
    /// Provisional `.notdef` for this frame only while a lazy face parses.
    Provisional,
    /// The honest give-up: nothing in the reachable chain covers the code point.
    Notdef,
}

impl Resolution {
    /// The `GlyphKey::source` this resolution produces.
    #[inline]
    #[must_use]
    pub const fn key_source(self) -> FaceId {
        match self {
            Resolution::Face(f) => f,
            Resolution::Runtime(_) => FaceId::RuntimeFallback,
            Resolution::Provisional | Resolution::Notdef => FaceId::Primary,
        }
    }

    /// The record this resolution wrote, if the rasterizer needs one to recover the face.
    #[inline]
    #[must_use]
    pub const fn recovery_slot(self) -> Option<RecoverySlot> {
        match self {
            Resolution::Face(FaceId::Fallback) => Some(RecoverySlot::FallbackPick),
            Resolution::Face(FaceId::SymbolFallback) => Some(RecoverySlot::SymbolSlot),
            Resolution::Runtime(_) => Some(RecoverySlot::RuntimeDecision),
            _ => None,
        }
    }
}

/// Returns the [`RecoverySlot`] required to rasterize a key with `source`.
#[inline]
#[must_use]
pub const fn slot_for_source(source: FaceId) -> Option<RecoverySlot> {
    match source {
        FaceId::Fallback => Some(RecoverySlot::FallbackPick),
        FaceId::SymbolFallback => Some(RecoverySlot::SymbolSlot),
        FaceId::RuntimeFallback => Some(RecoverySlot::RuntimeDecision),
        FaceId::Primary
        | FaceId::BoldPrimary
        | FaceId::Procedural
        | FaceId::ColorEmoji
        | FaceId::ColorEmojiMono
        | FaceId::DisplayMix => None,
    }
}

/// Coverage and background-parse oracle for [`resolve_chain`].
pub trait ChainProbe {
    /// Does `tier` cover this code point?
    fn covers(&mut self, tier: Tier) -> bool;
    /// Returns whether `tier`'s lazy face is still parsing on the background thread.
    fn parse_pending(&mut self, tier: Tier) -> bool;
}

/// The tiers `policy` admits — the declared chain, independent of any code point.
#[inline]
#[must_use]
pub const fn reachable_mask(policy: ChainPolicy) -> u8 {
    let configured = Tier::Procedural.bit()
        | Tier::Primary.bit()
        | Tier::Fallback.bit()
        | Tier::Symbol.bit()
        | Tier::Color.bit()
        | Tier::Synthetic.bit();
    if !policy.runtime_discovery {
        configured
    } else if policy.sealed {
        configured | Tier::RuntimeEmbedded.bit()
    } else {
        configured | Tier::RuntimeDecisions.bit() | Tier::RuntimeEmbedded.bit()
    }
}

/// Returns the bitmask of reachable tiers that require no font-file I/O.
#[inline]
#[must_use]
pub const fn io_free_mask(policy: ChainPolicy) -> u8 {
    reachable_mask(policy) & !Tier::RuntimeDecisions.bit()
}

/// Returns whether `ch` is in a Unicode Private Use Area.
#[inline]
#[must_use]
pub const fn is_private_use(ch: char) -> bool {
    matches!(ch, '\u{E000}'..='\u{F8FF}' | '\u{F0000}'..='\u{FFFFD}' | '\u{100000}'..='\u{10FFFD}')
}
/// Resolves a code point across the tier chain using `probe` and `policy`.
#[inline]
pub fn resolve_chain<P: ChainProbe + ?Sized>(probe: &mut P, policy: ChainPolicy) -> Resolution {
    if probe.covers(Tier::Procedural) {
        return Resolution::Face(FaceId::Procedural);
    }
    if probe.covers(Tier::Primary) {
        return Resolution::Face(FaceId::Primary);
    }
    let fallback_covers = probe.covers(Tier::Fallback);
    if fallback_covers && !policy.prefers_symbol {
        return Resolution::Face(FaceId::Fallback);
    }
    if !fallback_covers && probe.parse_pending(Tier::Fallback) {
        return Resolution::Provisional;
    }
    if probe.covers(Tier::Symbol) {
        return Resolution::Face(FaceId::SymbolFallback);
    }
    if fallback_covers {
        return Resolution::Face(FaceId::Fallback);
    }
    if probe.parse_pending(Tier::Symbol) {
        return Resolution::Provisional;
    }
    let color_has = probe.covers(Tier::Color);
    let face = crate::select_face(false, false, false, false, color_has, policy.wants_emoji);
    if matches!(face, FaceId::ColorEmojiMono) && probe.covers(Tier::Synthetic) {
        return Resolution::Face(FaceId::Procedural);
    }
    if !matches!(face, FaceId::Primary) {
        return Resolution::Face(face);
    }
    if !policy.runtime_discovery {
        return synthetic_or_notdef(probe);
    }
    if !policy.sealed && probe.covers(Tier::RuntimeDecisions) {
        return Resolution::Runtime(RuntimeLane::Decisions);
    }
    if probe.covers(Tier::RuntimeEmbedded) {
        return Resolution::Runtime(RuntimeLane::EmbeddedDecisions);
    }
    synthetic_or_notdef(probe)
}

/// The chain's final tier, shared by both give-up exits: the fontless symbol synthesis, else
/// `.notdef`. It is the highest tier index, so probing it last keeps P2's order on either exit.
#[inline]
fn synthetic_or_notdef<P: ChainProbe + ?Sized>(probe: &mut P) -> Resolution {
    if probe.covers(Tier::Synthetic) {
        Resolution::Face(FaceId::Procedural)
    } else {
        Resolution::Notdef
    }
}

/// Bitvector-backed [`ChainProbe`] that records consultation order and count.
#[derive(Clone, Copy, Debug)]
pub struct BitProbe {
    /// Bit `t` set == tier `t` covers the code point.
    pub covered: u8,
    /// Bit 0 = broad parse in flight, bit 1 = symbol parse in flight.
    pub pending: u8,
    /// Highest tier index consulted so far (`-1` = none).
    pub last: i8,
    /// How many probes were made.
    pub calls: u8,
    /// `true` iff every consulted tier was strictly after the previous one.
    pub ordered: bool,
}

impl BitProbe {
    #[inline]
    #[must_use]
    pub const fn new(covered: u8, pending: u8) -> Self {
        Self {
            covered,
            pending,
            last: -1,
            calls: 0,
            ordered: true,
        }
    }
}

impl ChainProbe for BitProbe {
    #[inline]
    fn covers(&mut self, tier: Tier) -> bool {
        let i = tier as i8;
        if i <= self.last {
            self.ordered = false;
        }
        self.last = i;
        self.calls = self.calls.saturating_add(1);
        (self.covered >> (tier as u8)) & 1 == 1
    }

    #[inline]
    fn parse_pending(&mut self, tier: Tier) -> bool {
        match tier {
            Tier::Fallback => self.pending & 1 == 1,
            Tier::Symbol => (self.pending >> 1) & 1 == 1,
            _ => false,
        }
    }
}
