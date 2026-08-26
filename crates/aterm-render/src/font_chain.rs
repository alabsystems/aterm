// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! The FULL glyph-resolution chain as ONE pure, exhaustively-verifiable policy:
//! the tier order, the two INDEPENDENT narrowing switches (the E1 host switch and
//! the font-generation seal), the provisional-`.notdef` gates while a lazy face
//! parses, the runtime give-up, and the record each decision must be recoverable
//! from.
//!
//! # Why this module exists (the 2026-07-24 `❯` U+276F tofu)
//!
//! [`crate::select_face`] is pure and exhaustively verified — but it only covers
//! the CONFIGURED tiers (procedural → primary → broad → symbol → colour). The
//! tofu bug lived strictly BELOW it, in `Renderer::glyph_key_inner`'s give-up
//! path: `seal_admitted_font_sources` set `runtime_discovery = false`, which
//! closed pathname discovery AND the `include_bytes!`'d Symbols Nerd Font, which
//! needs no I/O at all. Every GUI window seals, so a code point a LOADED face
//! covered rendered `.notdef` forever. No proof could see it, because the
//! decision was not in a pure function. This module IS that function, and
//! `glyph_key_inner` is its only production caller.
//!
//! # What is proven here
//!
//! * **P1 CONSERVATION** — [`Resolution::Notdef`] is returned ONLY IF no tier in
//!   the (possibly narrowed) chain covers the code point:
//!   `resolve_chain(..) == Notdef  ⇒  covered & reachable_mask(policy) == 0`.
//! * **P2 TOTALITY / ORDERING** — every tier is consulted AT MOST ONCE, in
//!   declared order, and the walk terminates (no loops, no recursion).
//! * **P3 KEY/RASTER AGREEMENT** — the record a resolution writes
//!   ([`Resolution::recovery_slot`]) is the record the rasterizer reads back from
//!   the emitted key's face ([`slot_for_source`]). The lead's first fix violated
//!   exactly this: the key said `RuntimeFallback` while `parts_for` consulted a
//!   map the decision was never written to, so the glyph rasterized as `.notdef`
//!   under a fallback-labelled key.
//! * **P4 SEAL MONOTONICITY** — sealing may narrow WHICH tiers are in the chain,
//!   but may never turn a code point some I/O-FREE tier covers into `.notdef`:
//!   `resolve_chain(.., sealed) == Notdef  ⇒  covered & io_free_mask(..) == 0`.
//!
//! What is NOT claimed: "no code point is ever tofu". Coverage depends on the
//! installed fonts; a code point nothing covers MUST resolve to `.notdef`, and
//! [`kani_proofs::notdef_is_reachable_and_honest`] keeps that side non-vacuous.

use crate::FaceId;

/// One tier of the glyph-resolution chain, in CONSULTATION ORDER. The
/// discriminants are the bit positions of the coverage bitvector the proofs and
/// the lattice test enumerate.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tier {
    /// Fontless box-drawing/block/braille synthesis (`crate::procedural`).
    Procedural = 0,
    /// The configured primary face's Unicode cmap.
    Primary = 1,
    /// The lazy broad-Unicode fallback CHAIN (`Renderer::fallback_has`).
    Fallback = 2,
    /// The lazy monochrome SYMBOL face (`Renderer::symbol_fallback_has`).
    Symbol = 3,
    /// The colour-emoji face (`Renderer::color_font_has`); the colour-vs-text
    /// decision itself stays in [`crate::select_face`].
    Color = 4,
    /// The runtime system resolver, PATHNAME lane — `RuntimeFallback::resolve`,
    /// which memoizes into `RuntimeFallback::decisions`. Performs font-file I/O,
    /// so a SEALED generation may not consult it.
    RuntimeDecisions = 5,
    /// The runtime resolver's I/O-FREE lane — `RuntimeFallback::resolve_embedded_only`,
    /// the `include_bytes!`'d Symbols Nerd Font, memoized into
    /// `RuntimeFallback::embedded_decisions`. Reachable WHENEVER the runtime tier
    /// is enabled at all, sealed or not. THIS is the tier the tofu bug lost.
    RuntimeEmbedded = 6,
}

impl Tier {
    /// Number of tiers — the exact bound on [`resolve_chain`]'s probe count.
    pub const COUNT: usize = 7;

    /// This tier's bit in a coverage bitvector.
    #[must_use]
    pub const fn bit(self) -> u8 {
        1u8 << (self as u8)
    }
}

/// The chain-narrowing policy: two INDEPENDENT switches plus the code point's
/// Unicode presentation. Keeping them separate fields (never one merged flag) is
/// the fix the tofu bug demanded, and [`kani_proofs::seal_never_hides_an_io_free_tier`]
/// is what holds them apart.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ChainPolicy {
    /// The E1 host switch (`Renderer::set_runtime_font_discovery`): `false` means
    /// NO runtime fallback tier at all, so a miss reaches
    /// `take_missing_font_classes` and a web/wasm host injects the face itself.
    pub runtime_discovery: bool,
    /// The font generation is sealed (`Renderer::seal_admitted_font_sources`):
    /// no PATHNAME discovery on the render thread. Does NOT close the I/O-free
    /// bundled backstop.
    pub sealed: bool,
    /// `aterm_grapheme::is_emoji_presentation(ch)`.
    pub wants_emoji: bool,
    /// The code point is in a PRIVATE USE AREA (U+E000..F8FF or either
    /// supplementary PUA plane), so a DEDICATED symbol face outranks the broad
    /// text chain for it.
    ///
    /// PUA carries no Unicode meaning: a text face that maps one is asserting a
    /// private convention, and the odds it is the SAME convention the author
    /// meant are poor. Measured: adding a Hangul face to the Windows chain made
    /// `arial.ttf` reachable, and arial maps exactly ONE private code point —
    /// U+F301 — so a Nerd Font logo that had always drawn from the bundled
    /// symbol face silently became arial's dot. Consulting Symbol first for PUA
    /// stops a broad face winning a range it covers only incidentally, and
    /// changes nothing for any assigned code point.
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

/// The record the RASTERIZER must find to recover which face a key draws from
/// (`Renderer::fallback_mono_raster`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecoverySlot {
    /// `Renderer::fallback_pick` — which broad-chain entry covered the char.
    FallbackPick,
    /// `Renderer::symbol_fallback` — the single symbol slot.
    SymbolSlot,
    /// `RuntimeFallback::parts_for` — EITHER decision map.
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
    /// `.notdef` for THIS FRAME ONLY while a lazy face parses. The caller MUST
    /// NOT memoize it; the parse's completion bumps `font_epoch` and the forced
    /// repaint re-resolves (see P5 in `aterm_spec::derive::fallback_convergence_model`).
    Provisional,
    /// The honest give-up: nothing in the reachable chain covers the code point.
    Notdef,
}

impl Resolution {
    /// The `GlyphKey::source` this resolution produces.
    #[must_use]
    pub const fn key_source(self) -> FaceId {
        match self {
            Resolution::Face(f) => f,
            Resolution::Runtime(_) => FaceId::RuntimeFallback,
            // Both `.notdef` forms render from the primary face.
            Resolution::Provisional | Resolution::Notdef => FaceId::Primary,
        }
    }

    /// The record this resolution WROTE, if the rasterizer needs one to recover
    /// the face. `None` = the raster needs no per-code-point recovery.
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

/// The record `Renderer::fallback_mono_raster` READS for a key with this face —
/// the mirror of [`Resolution::recovery_slot`]. Both sides of P3 in one place, so
/// a future tier cannot add a writer without an obvious reader.
#[must_use]
pub const fn slot_for_source(source: FaceId) -> Option<RecoverySlot> {
    match source {
        FaceId::Fallback => Some(RecoverySlot::FallbackPick),
        FaceId::SymbolFallback => Some(RecoverySlot::SymbolSlot),
        FaceId::RuntimeFallback => Some(RecoverySlot::RuntimeDecision),
        // `DisplayMix` needs no recovery record: the mix face is recomputed from
        // the code point itself (`display_mix_face_index`, a pure function of `ch`),
        // so there is no per-code-point write for a reader to disagree with. It
        // is also unreachable from `resolve_chain` — the mix is intercepted
        // ahead of the chain in `glyph_key_inner`, never resolved as a tier.
        FaceId::Primary
        | FaceId::BoldPrimary
        | FaceId::Procedural
        | FaceId::ColorEmoji
        | FaceId::ColorEmojiMono
        | FaceId::DisplayMix => None,
    }
}

/// The coverage oracle. The renderer implements it with its real lazy probes
/// (each tier's face is loaded only when every higher tier has missed); the
/// proofs implement it with a symbolic bitvector. `resolve_chain` calls each
/// method at most once per tier, so the laziness is a property of the ORDER, not
/// of the caller's discipline.
pub trait ChainProbe {
    /// Does `tier` cover this code point?
    fn covers(&mut self, tier: Tier) -> bool;
    /// Is `tier`'s lazy face still parsing on the background thread? Asked only
    /// for [`Tier::Fallback`] and [`Tier::Symbol`], and only AFTER that tier's
    /// `covers` has already missed.
    fn parse_pending(&mut self, tier: Tier) -> bool;
}

/// The tiers `policy` admits — the declared chain, independent of any code point.
#[must_use]
pub const fn reachable_mask(policy: ChainPolicy) -> u8 {
    let configured = Tier::Procedural.bit()
        | Tier::Primary.bit()
        | Tier::Fallback.bit()
        | Tier::Symbol.bit()
        | Tier::Color.bit();
    if !policy.runtime_discovery {
        // The E1 host switch: no runtime tier at all, so a miss can reach
        // `take_missing_font_classes` and the host injects the face.
        configured
    } else if policy.sealed {
        // The seal closes PATHNAME discovery only. The compiled-in face stays.
        configured | Tier::RuntimeEmbedded.bit()
    } else {
        configured | Tier::RuntimeDecisions.bit() | Tier::RuntimeEmbedded.bit()
    }
}

/// The tiers that need NO font-file I/O — everything except the pathname lane.
/// Sealing may not cost a code point a tier in here (P4).
#[must_use]
pub const fn io_free_mask(policy: ChainPolicy) -> u8 {
    reachable_mask(policy) & !Tier::RuntimeDecisions.bit()
}

/// Walk the chain. Pure: every fact comes from `probe`, every policy bit from

/// Whether `ch` sits in a Private Use Area — the BMP block plus both
/// supplementary planes. Used by [`ChainPolicy::prefers_symbol`]; see that
/// field for why PUA reorders the chain.
#[must_use]
pub const fn is_private_use(ch: char) -> bool {
    matches!(ch, '\u{E000}'..='\u{F8FF}' | '\u{F0000}'..='\u{FFFFD}' | '\u{100000}'..='\u{10FFFD}')
}
/// `policy`, and no tier is consulted twice.
pub fn resolve_chain<P: ChainProbe + ?Sized>(probe: &mut P, policy: ChainPolicy) -> Resolution {
    if probe.covers(Tier::Procedural) {
        return Resolution::Face(FaceId::Procedural);
    }
    if probe.covers(Tier::Primary) {
        return Resolution::Face(FaceId::Primary);
    }
    // The broad chain is probed IN ORDER (P2), but its answer does not win
    // outright for a PRIVATE-USE point: see `ChainPolicy::prefers_symbol`.
    let fallback_covers = probe.covers(Tier::Fallback);
    if fallback_covers && !policy.prefers_symbol {
        return Resolution::Face(FaceId::Fallback);
    }
    // The broad face is still parsing: render `.notdef` for this frame WITHOUT
    // memoizing and WITHOUT cascading into the synchronous loaders below. Only
    // reachable when the broad chain did NOT already answer.
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
    // ONE colour-vs-text policy place — the already-proven presentation gate.
    let color_has = probe.covers(Tier::Color);
    let face = crate::select_face(false, false, false, false, color_has, policy.wants_emoji);
    if !matches!(face, FaceId::Primary) {
        return Resolution::Face(face);
    }
    // ---- the give-up path: the two INDEPENDENT gates, deliberately not merged ----
    if !policy.runtime_discovery {
        return Resolution::Notdef;
    }
    if !policy.sealed && probe.covers(Tier::RuntimeDecisions) {
        return Resolution::Runtime(RuntimeLane::Decisions);
    }
    // ALWAYS last, sealed or not: the I/O-free compiled-in backstop. On the
    // unsealed path `RuntimeFallback::discover` already ends with it, so this is
    // a no-op there — and it stays correct if that ordering ever changes.
    if probe.covers(Tier::RuntimeEmbedded) {
        return Resolution::Runtime(RuntimeLane::EmbeddedDecisions);
    }
    Resolution::Notdef
}

/// A symbolic/enumerable coverage oracle: coverage as a 7-bit vector, the two
/// lazy-parse flags as 2 bits, plus the PROBE LOG that makes P2 checkable.
/// Shared by the proofs and the Tier-1 lattice test so both check the same thing.
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
    fn covers(&mut self, tier: Tier) -> bool {
        let i = tier as i8;
        if i <= self.last {
            self.ordered = false;
        }
        self.last = i;
        self.calls = self.calls.saturating_add(1);
        (self.covered >> (tier as u8)) & 1 == 1
    }

    fn parse_pending(&mut self, tier: Tier) -> bool {
        match tier {
            Tier::Fallback => self.pending & 1 == 1,
            Tier::Symbol => (self.pending >> 1) & 1 == 1,
            _ => false,
        }
    }
}

/// The runtime tier's two decision maps, abstracted to one cell each (P3's
/// second half). `record` is the resolver side, `parts_for` the rasterizer side.
/// The pre-fix `parts_for` read only [`Self::decisions`]; a decision recorded on
/// the embedded lane was then unrecoverable and the glyph drew `.notdef` under a
/// `RuntimeFallback` key.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DecisionCells {
    /// `Some(face)` = a cached decision; the inner `None` = a cached MISS.
    pub decisions: Option<Option<u8>>,
    pub embedded_decisions: Option<Option<u8>>,
}

impl DecisionCells {
    /// Record one lane's decision (the memoizing tail of `resolve` /
    /// `resolve_embedded_only`).
    pub fn record(&mut self, lane: RuntimeLane, face: Option<u8>) {
        match lane {
            RuntimeLane::Decisions => self.decisions = Some(face),
            RuntimeLane::EmbeddedDecisions => self.embedded_decisions = Some(face),
        }
    }

    /// `RuntimeFallback::parts_for`: recover the face index for a key, lane-AGNOSTIC.
    #[must_use]
    pub const fn parts_for(&self) -> Option<u8> {
        match self.decisions {
            Some(Some(i)) => Some(i),
            Some(None) => None,
            None => match self.embedded_decisions {
                Some(Some(i)) => Some(i),
                _ => None,
            },
        }
    }
}

/// Trust-toolchain (trust-mc / `#[kani::proof]`) proofs of the resolution-chain
/// laws over the FULL symbolic coverage/policy domain. CONFIG-FREE (no
/// `#[kani::unwind]` / `#[kani::stub]` / `#[kani::solver]`, no loops, no heap),
/// so `KANI_CRATE=aterm-render scripts/verify-kani-proofs.sh` discharges them.
///
/// Bounds and why they are complete, not a sample: the chain has exactly
/// [`Tier::COUNT`] = 7 tiers, so coverage is a 7-bit symbolic vector (2^7);
/// the lazy-parse state is 2 bits (2^2); the policy is 3 bools (2^3). 4096
/// states — the WHOLE input space of the decision, with no abstraction gap on
/// the policy side. The abstraction is only "a face either covers a code point
/// or it does not", which is exactly what every real probe returns.
#[cfg(kani)]
mod kani_proofs {
    use super::{
        BitProbe, ChainPolicy, DecisionCells, Resolution, RuntimeLane, Tier, io_free_mask,
        reachable_mask, resolve_chain, slot_for_source,
    };

    fn any_policy() -> ChainPolicy {
        ChainPolicy {
            runtime_discovery: kani::any(),
            sealed: kani::any(),
            wants_emoji: kani::any(),
        }
    }

    fn any_covered() -> u8 {
        let covered: u8 = kani::any();
        kani::assume(covered < 1 << Tier::COUNT);
        covered
    }

    fn any_pending() -> u8 {
        let pending: u8 = kani::any();
        kani::assume(pending < 4);
        pending
    }

    /// P1 CONSERVATION — `.notdef` is returned ONLY IF no tier the policy admits
    /// covers the code point. This alone is the proof that would have caught the
    /// U+276F tofu: sealed + `RuntimeEmbedded` covered + `.notdef` returned is a
    /// counterexample.
    #[kani::proof]
    fn notdef_only_when_no_reachable_tier_covers() {
        let covered = any_covered();
        let policy = any_policy();
        let mut probe = BitProbe::new(covered, any_pending());
        if resolve_chain(&mut probe, policy) == Resolution::Notdef {
            kani::assert(
                covered & reachable_mask(policy) == 0,
                "gave up on a code point some reachable tier covers",
            );
        }
    }

    /// P1' — a PROVISIONAL `.notdef` is returned only while a lazy face is
    /// actually parsing, and a give-up is never returned while one is (the
    /// give-up would otherwise be memoized against a chain that is still growing).
    #[kani::proof]
    fn provisional_iff_a_lazy_parse_is_in_flight() {
        let covered = any_covered();
        let pending = any_pending();
        let policy = any_policy();
        let mut probe = BitProbe::new(covered, pending);
        let r = resolve_chain(&mut probe, policy);
        if r == Resolution::Provisional {
            kani::assert(pending != 0, "provisional without a parse in flight");
        }
        if r == Resolution::Notdef {
            kani::assert(pending == 0, "gave up while a lazy face was still parsing");
        }
    }

    /// P2 TOTALITY / ORDERING — every tier is consulted at most once, strictly in
    /// declared order, and the walk terminates within [`Tier::COUNT`] probes.
    #[kani::proof]
    fn each_tier_is_consulted_at_most_once_in_order() {
        let mut probe = BitProbe::new(any_covered(), any_pending());
        let _ = resolve_chain(&mut probe, any_policy());
        kani::assert(probe.ordered, "a tier was re-probed or probed out of order");
        kani::assert(
            probe.calls as usize <= Tier::COUNT,
            "the chain walk exceeded its tier count",
        );
    }

    /// P3 (a) KEY/RASTER AGREEMENT — the record a resolution writes is the record
    /// the rasterizer reads back from the key it emits.
    #[kani::proof]
    fn every_key_names_the_record_the_rasterizer_reads() {
        let mut probe = BitProbe::new(any_covered(), any_pending());
        let r = resolve_chain(&mut probe, any_policy());
        if let Some(slot) = r.recovery_slot() {
            kani::assert(
                slot_for_source(r.key_source()) == Some(slot),
                "the key's face recovers from a different record than the resolution wrote",
            );
        }
    }

    /// P3 (b) LANE-AGNOSTIC RECOVERY — a decision recorded on EITHER runtime lane
    /// is recoverable by `parts_for`. The pre-fix reader saw only `decisions`, so
    /// the sealed (embedded) lane's decision was lost and the glyph drew `.notdef`
    /// under a `RuntimeFallback`-labelled key.
    #[kani::proof]
    fn a_recorded_runtime_decision_is_always_recoverable() {
        let face: u8 = kani::any();
        kani::assume(face < 4);
        let embedded: bool = kani::any();
        let lane = if embedded {
            RuntimeLane::EmbeddedDecisions
        } else {
            RuntimeLane::Decisions
        };
        let mut cells = DecisionCells::default();
        cells.record(lane, Some(face));
        kani::assert(
            cells.parts_for() == Some(face),
            "a recorded runtime decision was not recoverable by the rasterizer",
        );
    }

    /// P4 SEAL MONOTONICITY (mask form) — sealing may narrow the chain, but a
    /// sealed give-up implies NO I/O-free tier covered the code point. Under a
    /// sealed policy this is P1 restricted to the tiers the seal cannot touch, and
    /// it is exactly the obligation the shipped bug violated.
    #[kani::proof]
    fn sealing_never_hides_an_io_free_tier() {
        let covered = any_covered();
        let pending = any_pending();
        let base = any_policy();
        let sealed = ChainPolicy {
            sealed: true,
            ..base
        };
        let mut probe = BitProbe::new(covered, pending);
        if resolve_chain(&mut probe, sealed) == Resolution::Notdef {
            kani::assert(
                covered & io_free_mask(sealed) == 0,
                "sealing turned a code point an I/O-free tier covers into `.notdef`",
            );
        }
    }

    /// P4 SEAL MONOTONICITY (differential form) — for the SAME code point, if the
    /// sealed chain gives up then the unsealed chain either gave up too, or only
    /// resolved through the pathname lane the seal legitimately closes. Nothing
    /// else may be lost by sealing.
    #[kani::proof]
    fn sealing_only_costs_the_pathname_lane() {
        let covered = any_covered();
        let pending = any_pending();
        let base = any_policy();
        let unsealed = ChainPolicy {
            sealed: false,
            ..base
        };
        let sealed = ChainPolicy {
            sealed: true,
            ..base
        };
        let before = resolve_chain(&mut BitProbe::new(covered, pending), unsealed);
        let after = resolve_chain(&mut BitProbe::new(covered, pending), sealed);
        if after == Resolution::Notdef {
            kani::assert(
                before == Resolution::Notdef
                    || before == Resolution::Runtime(RuntimeLane::Decisions),
                "sealing downgraded a resolved code point to `.notdef`",
            );
        }
    }

    /// NON-VACUITY — `.notdef` IS reachable (the honest miss), so the proofs above
    /// are not trivially true of a resolver that never gives up.
    #[kani::proof]
    fn notdef_is_reachable_and_honest() {
        let policy = any_policy();
        let mut probe = BitProbe::new(0, 0);
        kani::assert(
            resolve_chain(&mut probe, policy) == Resolution::Notdef,
            "a code point NOTHING covers must still resolve to `.notdef`",
        );
    }
}
