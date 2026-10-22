// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! The full glyph-resolution chain as one pure, exhaustively checked policy: the tier order,
//! the two independent narrowing switches (the host's runtime-discovery switch and the
//! font-generation seal), the provisional-`.notdef` gates while a lazy face parses, the
//! runtime give-up, and the record each decision must be recoverable from.
//!
//! [`crate::select_face`] covers only the configured tiers (procedural → primary → broad →
//! symbol → colour); the give-up path below them lives here, so a seal that also closed the
//! I/O-free Symbols Nerd Font would break a checked law. `Renderer::glyph_key_inner` is the
//! only production caller.
//!
//! # Laws
//!
//! * **P1 conservation**: [`Resolution::Notdef`] only if no tier in the (possibly narrowed)
//!   chain covers the code point:
//!   `resolve_chain(..) == Notdef  ⇒  covered & reachable_mask(policy) == 0`.
//! * **P2 totality / ordering**: every tier is consulted at most once, in declared order, and
//!   the walk terminates.
//! * **P3 key/raster agreement**: the record a resolution writes
//!   ([`Resolution::recovery_slot`]) is the record the rasterizer reads back from the emitted
//!   key's face ([`slot_for_source`]); otherwise a fallback-labelled key rasterizes `.notdef`.
//! * **P4 seal monotonicity**: sealing may narrow which tiers are in the chain, but never turns
//!   a code point an I/O-free tier covers into `.notdef`:
//!   `resolve_chain(.., sealed) == Notdef  ⇒  covered & io_free_mask(..) == 0`.
//!
//! Not claimed: "no code point is ever tofu". A code point nothing covers must resolve to
//! `.notdef`. `tests/font_chain_conservation.rs` checks P1–P4 over the whole 16384-case input
//! space (`chain_laws_hold_over_the_complete_input_space`).

use crate::FaceId;

/// One tier of the glyph-resolution chain, in consultation order. The discriminants are the
/// bit positions of the coverage bitvector `tests/font_chain_conservation.rs` enumerates.
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
    /// The colour-emoji face (`Renderer::color_font_has`); the colour-vs-text
    /// decision itself stays in [`crate::select_face`].
    Color = 4,
    /// The runtime system resolver's pathname lane (`RuntimeFallback::resolve`, memoized into
    /// `RuntimeFallback::decisions`). Performs font-file I/O, so a sealed generation may not
    /// consult it.
    RuntimeDecisions = 5,
    /// The runtime resolver's I/O-free lane (`RuntimeFallback::resolve_embedded_only`): the
    /// `include_bytes!`'d Symbols Nerd Font, memoized into `RuntimeFallback::embedded_decisions`.
    /// Reachable whenever the runtime tier is enabled at all, sealed or not.
    RuntimeEmbedded = 6,
    /// The last-resort fontless symbol synthesis (`crate::procedural::covers_symbol`): media
    /// controls, geometric shapes, check marks, drawn from the cell geometry when every face
    /// above missed (no face on a bare Linux covers U+23F5), or when the only cover is the
    /// colour face's monochrome silhouette of a default-text point (see `resolve_chain`).
    /// I/O-free, so reachable under every policy. On a host with no runtime tier
    /// `glyph_key_inner` still records the missing class, so the host can inject a face that
    /// outranks it.
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

/// The chain-narrowing policy: two independent switches plus two facts about the code point
/// (emoji presentation, private use). The switches stay separate fields, never one merged
/// flag; P4 in `tests/font_chain_conservation.rs` holds them apart.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ChainPolicy {
    /// The host switch (`Renderer::set_runtime_font_discovery`): `false` means no runtime
    /// fallback tier at all, so a miss reaches `take_missing_font_classes` and a web/wasm host
    /// injects the face itself.
    pub runtime_discovery: bool,
    /// The font generation is sealed (`Renderer::seal_admitted_font_sources`): no pathname
    /// discovery on the render thread. Does not close the I/O-free bundled backstop.
    pub sealed: bool,
    /// `aterm_grapheme::is_emoji_presentation(ch)`.
    pub wants_emoji: bool,
    /// The code point is in a Private Use Area ([`is_private_use`]), so a dedicated symbol
    /// face outranks the broad text chain for it.
    ///
    /// It is not the whole fix, measured 2026-09-30 on Windows 11: the Nerd Font
    /// now answers from the RUNTIME tier, so a symbol-tier miss still handed
    /// U+F301 back to arial, and Segoe UI Symbol — the Windows symbol face,
    /// which maps 204 of the Nerd Font's private points — answered them with
    /// Microsoft's own glyphs. Those faces
    /// now decline such a point inside their tier's probe
    /// (`FOREIGN_PRIVATE_USE_FACES` in the crate root), which leaves this
    /// order, and every proof below, unchanged. The bundled Cascadia Mono PL
    /// declines its own private-use points the same way, inside the PRIMARY
    /// probe (`bundled::yields_private_use`), and the Windows default, Iosevka
    /// Term Semibold, its two-column characters (`bundled::yields_wide_characters`):
    /// to this module each is only a primary that does not cover them.
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

/// The record the rasterizer must find to recover which face a key draws from
/// (`Renderer::fallback_mono_raster`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecoverySlot {
    /// `Renderer::fallback_pick` — which broad-chain entry covered the char.
    FallbackPick,
    /// `Renderer::symbol_chain_pick`: which symbol-chain entry covered the char. Recomputed,
    /// not memoized (a pure function of the code point and the loaded chain), so writer and
    /// reader cannot drift.
    SymbolSlot,
    /// `RuntimeFallback::parts_for`: either decision map.
    RuntimeDecision,
}

/// The chain's verdict for one code point.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resolution {
    /// Resolved on a configured tier; the key carries this face.
    Face(FaceId),
    /// Resolved by the runtime tier on `lane`; the key carries
    /// [`FaceId::RuntimeFallback`].
    Runtime(RuntimeLane),
    /// `.notdef` for this frame only, while a lazy face parses. The caller must not memoize
    /// it; the parse's completion bumps `font_epoch` and the forced repaint re-resolves
    /// (aterm-gui's `fallback_convergence_action` re-arms it).
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
            // Both `.notdef` forms render from the primary face.
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

/// The record `Renderer::fallback_mono_raster` reads for a key with this face: the mirror of
/// [`Resolution::recovery_slot`], both sides of P3 in one place.
#[inline]
#[must_use]
pub const fn slot_for_source(source: FaceId) -> Option<RecoverySlot> {
    match source {
        FaceId::Fallback => Some(RecoverySlot::FallbackPick),
        FaceId::SymbolFallback => Some(RecoverySlot::SymbolSlot),
        FaceId::RuntimeFallback => Some(RecoverySlot::RuntimeDecision),
        // `DisplayMix` needs no record: the mix face is a pure function of `ch`
        // (`display_mix_face_index`), and `glyph_key_inner` intercepts it ahead of the chain.
        FaceId::Primary
        | FaceId::BoldPrimary
        | FaceId::Procedural
        | FaceId::ColorEmoji
        | FaceId::ColorEmojiMono
        | FaceId::DisplayMix => None,
    }
}

/// The coverage oracle: the renderer's real lazy probes (each tier's face loads only when every
/// higher tier missed), or [`BitProbe`] in the exhaustive test. `resolve_chain` asks each tier
/// at most once, so the laziness is a property of the order, not of the caller.
pub trait ChainProbe {
    /// Does `tier` cover this code point?
    fn covers(&mut self, tier: Tier) -> bool;
    /// Is `tier`'s lazy face still parsing on the background thread? Asked only for
    /// [`Tier::Fallback`] and [`Tier::Symbol`], after that tier's `covers` missed.
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
        // Fontless and I/O-free: no switch can close it.
        | Tier::Synthetic.bit();
    if !policy.runtime_discovery {
        // The host switch (`ChainPolicy::runtime_discovery`).
        configured
    } else if policy.sealed {
        // The seal closes pathname discovery only; the compiled-in face stays.
        configured | Tier::RuntimeEmbedded.bit()
    } else {
        configured | Tier::RuntimeDecisions.bit() | Tier::RuntimeEmbedded.bit()
    }
}

/// The tiers that need no font-file I/O: everything except the pathname lane. Sealing may
/// not cost a code point a tier in here (P4).
#[inline]
#[must_use]
pub const fn io_free_mask(policy: ChainPolicy) -> u8 {
    reachable_mask(policy) & !Tier::RuntimeDecisions.bit()
}

/// Whether `ch` sits in a Private Use Area (the BMP block or either supplementary plane);
/// see [`ChainPolicy::prefers_symbol`] for why PUA reorders the chain.
#[inline]
#[must_use]
pub const fn is_private_use(ch: char) -> bool {
    matches!(ch, '\u{E000}'..='\u{F8FF}' | '\u{F0000}'..='\u{FFFFD}' | '\u{100000}'..='\u{10FFFD}')
}
/// Walk the chain. Pure: every fact comes from `probe`, every policy bit from `policy`, and no
/// tier is consulted twice.
#[inline]
pub fn resolve_chain<P: ChainProbe + ?Sized>(probe: &mut P, policy: ChainPolicy) -> Resolution {
    if probe.covers(Tier::Procedural) {
        return Resolution::Face(FaceId::Procedural);
    }
    if probe.covers(Tier::Primary) {
        return Resolution::Face(FaceId::Primary);
    }
    // The broad chain is probed in order (P2), but its answer does not win outright for a
    // private-use point (`ChainPolicy::prefers_symbol`).
    let fallback_covers = probe.covers(Tier::Fallback);
    if fallback_covers && !policy.prefers_symbol {
        return Resolution::Face(FaceId::Fallback);
    }
    // The broad face is still parsing and did not answer: `.notdef` for this frame, without
    // memoizing and without cascading into the synchronous loaders below.
    if !fallback_covers && probe.parse_pending(Tier::Fallback) {
        return Resolution::Provisional;
    }
    if probe.covers(Tier::Symbol) {
        return Resolution::Face(FaceId::SymbolFallback);
    }
    // PUA and the symbol face does not have it after all — the broad chain's
    // incidental coverage is better than `.notdef`, so it still wins here.
    if fallback_covers {
        return Resolution::Face(FaceId::Fallback);
    }
    if probe.parse_pending(Tier::Symbol) {
        return Resolution::Provisional;
    }
    // The one colour-vs-text policy place: the presentation gate.
    let color_has = probe.covers(Tier::Color);
    let face = crate::select_face(false, false, false, false, color_has, policy.wants_emoji);
    // A default-text point only the colour face covers would draw its monochrome silhouette.
    // Noto Color Emoji paints ⏸ ⏹ ⏺ ⏭ ⏮ ⏯ on a filled key cap, so that silhouette is a solid
    // square; the synthesis is the better stand-in. Probing Synthetic here, and never again
    // below, keeps P2's order (Color=4 → Synthetic=7).
    if matches!(face, FaceId::ColorEmojiMono) && probe.covers(Tier::Synthetic) {
        return Resolution::Face(FaceId::Procedural);
    }
    if !matches!(face, FaceId::Primary) {
        return Resolution::Face(face);
    }
    // The give-up path: two independent gates, deliberately not merged.
    if !policy.runtime_discovery {
        return synthetic_or_notdef(probe);
    }
    if !policy.sealed && probe.covers(Tier::RuntimeDecisions) {
        return Resolution::Runtime(RuntimeLane::Decisions);
    }
    // Last, sealed or not: the I/O-free compiled-in backstop. Unsealed,
    // `RuntimeFallback::discover` already ends with it, so this is a no-op there.
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

/// An enumerable coverage oracle: coverage as an 8-bit vector, the two lazy-parse flags as 2
/// bits, and the probe log that makes P2 checkable (`tests/font_chain_conservation.rs`).
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
    /// Still true iff every probe was strictly later than the previous one —
    /// i.e. declared order, no tier twice.
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
