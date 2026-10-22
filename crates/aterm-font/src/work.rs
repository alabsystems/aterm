// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

/// One outline query's cumulative expansion budget. Nesting caps bound the
/// stack, but an acyclic DAG can visit its children exponentially often.
/// Charge every glyph entry, TrueType point and CFF instruction across all
/// siblings/subroutines, rather than granting each recursive call a new
/// allowance. Partial sink output is possible before refusal, as for any
/// malformed outline.
pub(crate) struct OutlineWork(Option<u32>);

impl Default for OutlineWork {
    #[inline]
    fn default() -> Self {
        Self(Some(65_536))
    }
}

impl OutlineWork {
    #[inline]
    pub(crate) fn take(&mut self) -> Option<()> {
        #[cfg(all(any(test, debug_assertions), not(trust_verify)))]
        note(|s| s.outline_steps += 1);
        self.0 = self.0?.checked_sub(1);
        self.0.map(|_| ())
    }

    #[inline]
    pub(crate) const fn exhausted(&self) -> bool {
        self.0.is_none()
    }
}

/// One `GSUB` table's walk budget, shared by every lookup the walks of one
/// reader reach (the shaper's lookup parses, the autohinter's style and
/// cluster walks), one unit per lookup, subtable or entry visited: per
/// traversal of the table the reader may make, 8 units per byte of the table
/// and at least 16,384. The budget is aterm's own. HarfBuzz's sanitizer
/// charges bytes rather than entries, 64 operations per byte of the table
/// (`HB_SANITIZE_MAX_OPS_FACTOR`, at least 16,384) with each range it checks
/// spending its length (`check_range`, src/hb-sanitize.hh at c7ef6a2ed5), so
/// it allows a two-byte entry 32 checks per byte of the table where this
/// allows 8. A well-formed table spends about half a unit per byte in one
/// traversal, since each lookup, subtable and entry sits in bytes of its own
/// (at least two); the most on this Mac (2026-10-19, 523 faces with `GSUB`)
/// is 6.02% of the shaper's one traversal (SFIndia) and 0.26 traversals of
/// the autohinter's eight (Hiragino Sans W3). Offsets may alias, though:
/// every lookup of a list can name one lookup and every subtable offset of
/// it one subtable, so a 192 KB table names 32,000 x 32,000 subtables (27 s
/// for one two-character shape, 16 s to build the autohinter's styles,
/// measured 2026-10-19). Once the budget is spent the walk refuses the
/// table, as HarfBuzz drops a table that fails its sanitizer.
pub(crate) struct LayoutWork {
    left: core::cell::Cell<u64>,
    refused: core::cell::Cell<bool>,
}

impl LayoutWork {
    /// The budget for `traversals` traversals of a table of `len` bytes.
    #[inline]
    pub(crate) const fn for_table(len: usize, traversals: u64) -> Self {
        let one = (len as u64).saturating_mul(8);
        let one = if one < 16_384 { 16_384 } else { one };
        Self {
            left: core::cell::Cell::new(one.saturating_mul(traversals)),
            refused: core::cell::Cell::new(false),
        }
    }

    /// Spends `units`; `None`, and refused from then on, when they are not
    /// left.
    #[inline]
    pub(crate) fn spend(&self, units: u64) -> Option<()> {
        match self.left.get().checked_sub(units) {
            Some(left) if !self.refused.get() => {
                self.left.set(left);
                Some(())
            }
            _ => {
                self.refused.set(true);
                None
            }
        }
    }

    /// Whether a walk ran past the budget. Only the hinter asks, so it is
    /// built where the hinter is (`autohint`'s cfg).
    #[cfg(any(feature = "hint", debug_assertions))]
    #[inline]
    pub(crate) fn refused(&self) -> bool {
        self.refused.get()
    }
}

/// What this thread's outline queries and `COLR` paint walks have spent
/// since the last [`take_spent`]: how close a real face comes to the two
/// work budgets, the 65,536 outline steps of [`OutlineWork`] and the 2^16
/// paint tables of a `COLR` walk (TF.5 of
/// docs/AUDIT-third-party-2026-10-15.md). The font oracle recorder's
/// `budgets` mode reads it per glyph. Debug builds only: a release build
/// counts nothing and has no such item, and neither has the Trust lane's
/// build (`trust_verify`), which proves the budgets' code as it ships.
#[cfg(all(any(test, debug_assertions), not(trust_verify)))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Spent {
    /// Outline steps asked for, every one a budget refused included, so a
    /// refused query reads more than 65,536.
    pub outline_steps: u64,
    /// `COLR` paint tables a walk asked to visit, every one the budget
    /// refused included, so a refused walk reads more than 2^16.
    pub paint_tables: u64,
}

#[cfg(all(any(test, debug_assertions), not(trust_verify)))]
std::thread_local! {
    static SPENT: std::cell::Cell<Spent> = const {
        std::cell::Cell::new(Spent {
            outline_steps: 0,
            paint_tables: 0,
        })
    };
}

#[cfg(all(any(test, debug_assertions), not(trust_verify)))]
#[inline]
pub(crate) fn note(charge: impl FnOnce(&mut Spent)) {
    SPENT.with(|cell| {
        let mut spent = cell.get();
        charge(&mut spent);
        cell.set(spent);
    });
}

/// This thread's [`Spent`] since the last call, which starts it again at
/// zero. Debug builds only.
#[cfg(all(any(test, debug_assertions), not(trust_verify)))]
#[must_use]
pub fn take_spent() -> Spent {
    SPENT.with(|cell| cell.replace(Spent::default()))
}
