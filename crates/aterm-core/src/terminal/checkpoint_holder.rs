// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! The holder fence's sub-projection of a
//! [`TerminalCheckpoint`](super::TerminalCheckpoint): the
//! [`HolderFence`](super::HolderFence) and the main grid's span anchors, beside
//! the command marks they share the absolute row numbering with.
//!
//! These ride the keyframes the crash journal and the seamless rungs restore
//! from; no tail record carries them (an older reader stops at an unknown
//! kind). A replay does not observe holder cuts; output before host adoption
//! taints its carried current mark and disables the old command-start seed,
//! including across an intermediate checkpoint.
//!
//! A restore installs the fence only whole: a repr whose spans do not name
//! the anchors it carries, or that breaks a bound, installs as no fence at
//! all, so a forged or damaged carry can only leave a card bare.

use aterm_grid::{Anchor, AnchorId, AnchorRole, AnchorSet, MAX_ANCHORS};

use super::Terminal;
use super::holder_fence::{HolderFence, MAX_HOLDER_MARKS, MAX_HOLDER_SPANS, Mark, Seal, Span};

/// One anchor slot of the main grid's set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct HolderAnchorRepr {
    /// The absolute row.
    pub row: u64,
    /// An End (`true`) or a Start.
    pub end: bool,
    /// The slot holds an anchor.
    pub live: bool,
}

/// One span: anchor slot indices, the end absent on an open span.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct HolderSpanRepr {
    /// The Start anchor's slot.
    pub start: u16,
    /// The End anchor's slot, absent while the span is open.
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "Option::is_none")
    )]
    pub end: Option<u16>,
}

/// A suspended mark's seal.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct HolderSealRepr {
    /// `(absolute row, fingerprint)`, ascending.
    pub rows: Vec<(u64, u64)>,
    /// The screen's absolute top when it was sealed.
    pub base: u64,
    /// The anchor set's seal epoch when it was sealed.
    pub epoch: u64,
}

/// One drawer's mark.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct HolderMarkRepr {
    /// The drawer's token (never `0`).
    pub token: u64,
    /// Its spans, oldest first.
    pub spans: Vec<HolderSpanRepr>,
    /// Nothing on the main screen is admitted.
    #[cfg_attr(feature = "serde", serde(default))]
    pub tainted: bool,
    /// The alternate screen is wholly its.
    #[cfg_attr(feature = "serde", serde(default))]
    pub alt_fresh: bool,
    /// Its seal, while suspended.
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "Option::is_none")
    )]
    pub seal: Option<HolderSealRepr>,
    /// The fence clock when it was suspended.
    #[cfg_attr(feature = "serde", serde(default))]
    pub suspended_at: u64,
}

/// The holder fence and the main grid's anchors. `Default` (no mark, no
/// anchor and no seed uncertainty) is a session in which no drawer ran; the
/// wire omits it. An otherwise empty uncertain carry keeps its one bit.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct HolderRepr {
    /// The current mark's index.
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "Option::is_none")
    )]
    pub current: Option<u8>,
    /// The fence clock.
    #[cfg_attr(feature = "serde", serde(default))]
    pub clock: u64,
    /// Every mark.
    #[cfg_attr(feature = "serde", serde(default))]
    pub marks: Vec<HolderMarkRepr>,
    /// The main grid's anchor slots, in slot order.
    #[cfg_attr(feature = "serde", serde(default))]
    pub anchors: Vec<HolderAnchorRepr>,
    /// Some motion lost the anchors since the fence last absorbed it.
    #[cfg_attr(feature = "serde", serde(default))]
    pub anchors_lost: bool,
    /// The anchor set's seal epoch.
    #[cfg_attr(feature = "serde", serde(default))]
    pub seal_epoch: u64,
    /// Output arrived while a restored fence awaited host adoption. The
    /// carried command's C cannot seed a range in this restored lineage.
    /// Older checkpoints omit it; older readers ignore this new qualification.
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "core::ops::Not::not")
    )]
    pub seed_untrusted: bool,
}

impl HolderRepr {
    /// Whether no mark, anchor or seed uncertainty needs carrying.
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// Capture `fence` over `anchors`, an anchor below `floor` (the oldest row
    /// the checkpoint carries) clamped to it, as the restored grid will hold
    /// it.
    fn capture(fence: &HolderFence, anchors: &AnchorSet, floor: u64) -> Self {
        // No mark, anchor or seed uncertainty: nothing a restore needs (the
        // clock orders only marks, the seal epoch only seals).
        if fence.is_empty() && anchors.is_empty() && !fence.seed_untrusted {
            return Self::default();
        }
        let slot = |id: AnchorId| id.index();
        Self {
            current: fence.current,
            clock: fence.clock,
            marks: fence
                .marks
                .iter()
                .map(|m| HolderMarkRepr {
                    token: m.token,
                    spans: m
                        .spans
                        .iter()
                        .map(|s| HolderSpanRepr {
                            start: slot(s.start),
                            end: s.end.map(slot),
                        })
                        .collect(),
                    tainted: m.tainted,
                    alt_fresh: m.alt_fresh,
                    seal: m.seal.as_ref().map(|seal| HolderSealRepr {
                        rows: seal.rows.clone(),
                        base: seal.base,
                        epoch: seal.epoch,
                    }),
                    suspended_at: m.suspended_at,
                })
                .collect(),
            anchors: anchors
                .slots()
                .iter()
                .map(|a| HolderAnchorRepr {
                    row: if a.live { a.row.max(floor) } else { a.row },
                    end: a.role == AnchorRole::End,
                    live: a.live,
                })
                .collect(),
            anchors_lost: anchors.lost(),
            seal_epoch: anchors.seal_epoch(),
            seed_untrusted: fence.seed_untrusted,
        }
    }

    /// The fence and anchor set this describes, or `None` when it is not one
    /// a fence could have left: a bound broken, a span naming a slot that is
    /// not a live anchor of its role, a slot two spans share or none holds,
    /// an open span off the current mark, a token `0` or held twice.
    fn install(&self) -> Option<(HolderFence, AnchorSet)> {
        if self.marks.len() > MAX_HOLDER_MARKS || self.anchors.len() > MAX_ANCHORS {
            return None;
        }
        if self
            .current
            .is_some_and(|c| usize::from(c) >= self.marks.len())
        {
            return None;
        }
        let slots: Vec<Anchor> = self
            .anchors
            .iter()
            .map(|a| Anchor {
                row: a.row,
                role: if a.end {
                    AnchorRole::End
                } else {
                    AnchorRole::Start
                },
                live: a.live,
            })
            .collect();
        let mut held = vec![false; slots.len()];
        let mut claim = |index: u16, role: AnchorRole| -> Option<AnchorId> {
            let i = usize::from(index);
            let anchor = slots.get(i)?;
            if !anchor.live || anchor.role != role || held[i] {
                return None;
            }
            held[i] = true;
            Some(AnchorId::from_index(index))
        };
        let mut marks = Vec::with_capacity(self.marks.len());
        for (i, m) in self.marks.iter().enumerate() {
            let current = self.current.map(usize::from) == Some(i);
            if m.token == 0
                || m.spans.len() > MAX_HOLDER_SPANS
                || self.marks[..i].iter().any(|o| o.token == m.token)
                || m.spans.iter().filter(|s| s.end.is_none()).count() > usize::from(current)
                || m.seal
                    .as_ref()
                    .is_some_and(|seal| seal.rows.len() > MAX_SEAL_ROWS)
            {
                return None;
            }
            let mut spans = Vec::with_capacity(m.spans.len());
            for s in &m.spans {
                let start = claim(s.start, AnchorRole::Start)?;
                let end = match s.end {
                    Some(end) => Some(claim(end, AnchorRole::End)?),
                    None => None,
                };
                spans.push(Span { start, end });
            }
            marks.push(Mark {
                token: m.token,
                spans,
                tainted: m.tainted,
                alt_fresh: m.alt_fresh,
                seal: m.seal.as_ref().map(|seal| Seal {
                    rows: seal.rows.clone(),
                    base: seal.base,
                    epoch: seal.epoch,
                }),
                suspended_at: m.suspended_at,
            });
        }
        // Every live anchor bounds exactly one span.
        if slots.iter().zip(&held).any(|(a, &h)| a.live != h) {
            return None;
        }
        let anchors = AnchorSet::from_slots(slots, self.anchors_lost, self.seal_epoch)?;
        Some((
            HolderFence {
                current: self.current,
                marks,
                clock: self.clock,
                seed_untrusted: self.seed_untrusted,
            },
            anchors,
        ))
    }
}

/// The most rows a carried seal may name: one per row of the tallest screen.
const MAX_SEAL_ROWS: usize = aterm_grid::MAX_GRID_ROWS as usize;

impl Terminal {
    /// The fence and the main grid's anchors, an anchor below `floor` (the
    /// oldest primary row the checkpoint carries) clamped to it.
    pub(super) fn capture_holder_repr(&self, floor: u64) -> HolderRepr {
        let mut repr = HolderRepr::capture(&self.holder_fence, self.main_grid().anchors(), floor);
        if self
            .holder_carried_at
            .is_some_and(|at| at != self.transient.screen_mutations)
        {
            // A recovery reader may checkpoint its replay before the actual
            // host adopts it. Do not re-arm that stale current mark as an
            // untouched carry at the next restore. Suspended marks stay put.
            repr.seed_untrusted = true;
            if let Some(mark) = repr
                .current
                .and_then(|i| repr.marks.get_mut(usize::from(i)))
            {
                mark.tainted = true;
            }
        }
        repr
    }

    /// Install a carried fence after the grids were restored: its anchors go
    /// to the main grid, keyed under `counter` (the carried main grid's
    /// absolute row counter; a grid numbered otherwise marks them lost). One
    /// that does not install whole leaves no fence and no anchor. The output
    /// count at the install is kept: a carried current mark continues at
    /// the adoption only if no byte came between
    /// ([`Terminal::adopt_holder_after_restore`]).
    pub(super) fn install_holder_repr(&mut self, repr: &HolderRepr, counter: u64) {
        let (mut fence, anchors) = match repr.install() {
            Some(pair) => pair,
            None => {
                // A structurally invalid fence is not seed authority either.
                let fence = HolderFence {
                    seed_untrusted: true,
                    ..HolderFence::default()
                };
                (fence, AnchorSet::default())
            }
        };
        fence.seed_untrusted |= repr.seed_untrusted;
        // Track a restore even when a predecessor carried no mark: its old
        // signed C must not become a seed after an unobserved replay tail.
        self.holder_carried_at = Some(self.transient.screen_mutations);
        let (slot, main) = self.fence_and_main_grid();
        *slot = fence;
        main.install_anchors(anchors, counter);
    }
}
