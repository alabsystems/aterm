// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Rust folds held to the island: the answer sheet over the island's grid, read through
//! P1's `Table` and this module's kernels and folds, and the replaced rules the grid must see.

use super::*;
use crate::revocation::{Remove, Row, Table, current, head_due};

/// The island file, read as text on every target (stock and cross builds included).
const ISLAND: &str = include_str!("../gap_heal_island.rs");

/// The island's grid size (`gridSheetFrom 0 64`, eight views to a word).
const GRID: usize = 512;

/// The island's `gridBefore`: `a < b`, but for epoch records 5 and 6, which are concurrent unless
/// the view is sequential.
const fn grid_before(sequential: bool, a: usize, b: usize) -> bool {
    a < b && !(!sequential && a == 5 && b == 6)
}

/// The causal order of a view, behind a reference.
type Before<'a> = &'a dyn Fn(usize, usize) -> bool;

/// One view of the island's grid (`gridWorld k`): P1's Table over three seats, two epochs of
/// `[everyone]`, the `Wrap`s held and the two rosters.
struct View<'a> {
    rows: &'a [Row],
    removes: &'a [Remove],
    before: Before<'a>,
    table: Table<'a, Before<'a>>,
    k: usize,
}

/// The rules one answer sheet reads a view by: the decision's, or a replaced rule.
#[derive(Clone, Copy)]
struct Rules {
    heals: fn(bool, bool, bool) -> bool,
    gap: fn(bool, bool, bool, bool, bool) -> bool,
    carry_deny: fn(bool, bool) -> bool,
    carrier: fn(Vec<(bool, bool, bool)>) -> bool,
    strands: fn(Vec<(bool, bool)>) -> bool,
}

const DECISION: Rules = Rules {
    heals: heals_omission,
    gap,
    carry_deny: denied,
    carrier: |seats| carrier(seats),
    strands: |heads| strands_any(heads),
};

/// The grid's three seats: actor and recipient `i`; seats 0 and 1 entered at the genesis
/// (record 0), seat 2 at record 7, after both epochs.
const ROWS: [Row; 3] = [
    Row {
        actor: 0,
        recipient: 0,
        entry: 0,
    },
    Row {
        actor: 1,
        recipient: 1,
        entry: 0,
    },
    Row {
        actor: 2,
        recipient: 2,
        entry: 7,
    },
];

/// The grid's `Remove`s (bits 5-6): none; actor 1 at record 10; actor 2 at record 10; actor 2 at
/// record 4, which seat 2's later entry has in its past.
fn grid_removes(k: usize) -> Vec<Remove> {
    let remove = |actor, record| Remove {
        actor,
        record,
        live: true,
        quorum: false,
    };
    match (k / 32) % 4 {
        0 => Vec::new(),
        1 => vec![remove(1, 10)],
        2 => vec![remove(2, 10)],
        _ => vec![remove(2, 4)],
    }
}

/// The number of recipients and epochs of every grid view.
const RECIPIENTS: usize = 3;
const EPOCHS: usize = 2;

impl View<'_> {
    const fn flag(&self, bit: usize) -> bool {
        (self.k >> bit) & 1 == 1
    }

    /// Bit 7: epoch 1 has epoch 0 in its past (sequential), else they are concurrent heads.
    const fn sequential(&self) -> bool {
        self.flag(7)
    }

    /// Bits 0-4: epoch `e` wraps recipient `r`; bit 2 is seat 2's `Wrap` in both epochs.
    const fn wrapped(&self, e: usize, r: usize) -> bool {
        self.flag(if r == 2 { 2 } else { 3 * e + r })
    }

    /// Bit 8: this Mac's own roster denies seat 1, the opened roster seat 2.
    const fn local_deny(&self, r: usize) -> bool {
        r == 1 && self.flag(8)
    }

    const fn opened_deny(&self, r: usize) -> bool {
        r == 2 && self.flag(8)
    }

    /// Epoch `e`'s record: 5 or 6.
    const fn record(e: usize) -> usize {
        5 + e
    }

    const fn newest(&self, e: usize) -> bool {
        e < EPOCHS && !(e == 0 && self.sequential())
    }

    /// Both epochs are of `[everyone]` (audience 0).
    const fn is_head(&self, a: usize, e: usize) -> bool {
        self.newest(e) && a == 0
    }

    fn denied(&self, r: usize) -> bool {
        denied(self.local_deny(r), self.opened_deny(r))
    }

    /// Seat row `i` is current: not out, and its recipient not revoked.
    fn seat_current(&self, i: usize) -> bool {
        current(
            !self.table.is_out(i),
            self.table.revoked(self.rows[i].recipient),
        )
    }

    fn wraps_revoked(&self, e: usize) -> bool {
        wraps_revoked((0..RECIPIENTS).map(|r| (self.table.revoked(r), self.wrapped(e, r))))
    }

    fn holds_all(&self, a: usize, r: usize) -> bool {
        (0..EPOCHS).fold(true, |acc, e| {
            acc & (!self.is_head(a, e) || self.wrapped(e, r))
        })
    }

    fn carried(&self, rules: &Rules, a: usize) -> bool {
        (rules.carrier)(
            (0..RECIPIENTS)
                .map(|r| (self.table.current(r), self.denied(r), self.holds_all(a, r)))
                .collect(),
        )
    }

    fn known(&self, e: usize, i: usize) -> bool {
        let entry = self.rows[i].entry;
        (self.before)(entry, Self::record(e)) || entry == Self::record(e)
    }

    fn gap(&self, rules: &Rules, m: usize, e: usize, i: usize) -> bool {
        let heals = (rules.heals)(
            self.newest(e),
            self.wraps_revoked(e),
            self.carried(rules, 0),
        );
        let recipient = self.rows[i].recipient;
        (rules.gap)(
            self.wrapped(e, m),
            self.seat_current(i),
            self.wrapped(e, recipient),
            self.known(e, i),
            heals,
        )
    }

    fn heal(&self, rules: &Rules, m: usize, e: usize, i: usize) -> bool {
        heal_signs(
            self.gap(rules, m, e, i),
            self.denied(self.rows[i].recipient),
        )
    }

    fn carry(&self, rules: &Rules, r: usize) -> bool {
        carried(
            self.table.current(r),
            false,
            (rules.carry_deny)(self.local_deny(r), self.opened_deny(r)),
        )
    }

    fn opens_newest(&self, m: usize, a: usize) -> bool {
        (0..EPOCHS).any(|e| self.is_head(a, e)) && self.holds_all(a, m)
    }

    fn twins(&self, a: usize) -> bool {
        (0..EPOCHS).all(|e1| {
            (0..EPOCHS).all(|e2| {
                !(self.is_head(a, e1) && self.is_head(a, e2))
                    || (0..RECIPIENTS).all(|r| self.wrapped(e1, r) == self.wrapped(e2, r))
            })
        })
    }

    fn due(&self, a: usize) -> bool {
        (0..EPOCHS).any(|e| {
            self.is_head(a, e) && head_due(self.table.gate(Self::record(e)), self.wraps_revoked(e))
        }) || !self.twins(a)
    }

    /// Whether Mac `m`'s removal of `actor` (a plain `Remove` at record 11) strands a newest
    /// epoch of a seat audience: the island's `strandsAny`, its holders read after the `Remove`.
    fn strands(&self, rules: &Rules, m: usize, actor: usize) -> bool {
        let mut removes = self.removes.to_vec();
        removes.push(Remove {
            actor,
            record: 11,
            live: true,
            quorum: false,
        });
        let after = Table::new(self.rows, &removes, self.before);
        let heads = (0..2)
            .flat_map(|a| (0..EPOCHS).map(move |e| (a, e)))
            .filter(|&(a, e)| self.is_head(a, e))
            .map(|(a, e)| {
                let held = holder((0..RECIPIENTS).map(|r| (after.current(r), self.wrapped(e, r))));
                (held, self.opens_newest(m, a))
            })
            .collect();
        (rules.strands)(heads)
    }
}

/// The island's `gridByte` of view `k`, under `rules`.
fn grid_byte(k: usize, rules: &Rules) -> u8 {
    let removes = grid_removes(k);
    let sequential = (k / 128) % 2 == 1;
    let before = move |a: usize, b: usize| grid_before(sequential, a, b);
    let before: Before<'_> = &before;
    let view = View {
        rows: &ROWS,
        removes: &removes,
        before,
        table: Table::new(&ROWS, &removes, before),
        k,
    };
    let bits = [
        view.heal(rules, 0, 0, 1),
        view.heal(rules, 0, 1, 2),
        view.heal(rules, 1, 0, 0),
        view.carried(rules, 0),
        view.due(0),
        view.carry(rules, 1),
        view.strands(rules, 0, 1),
        view.strands(rules, 2, 0),
    ];
    bits.iter()
        .enumerate()
        .fold(0, |byte, (n, &on)| byte | (u8::from(on) << n))
}

/// The island's `gridSheet`: eight views to a word, view `8w`'s byte first.
fn sheet(rules: &Rules) -> Vec<u64> {
    (0..GRID / 8)
        .map(|w| {
            (0..8).fold(0, |word, n| {
                (word << 8) | u64::from(grid_byte(8 * w + n, rules))
            })
        })
        .collect()
}

/// The words the island proves `gridSheet` equal to, by `rfl`.
fn island_sheet() -> Option<Vec<u64>> {
    const STATED: &str = "theorem grid_sheet_is : gridSheet =";
    let rest = &ISLAND[ISLAND.find(STATED)? + STATED.len()..];
    let list = &rest[rest.find('[')? + 1..rest.find(']')?];
    list.split(',')
        .map(|word| u64::from_str_radix(word.trim().strip_prefix("0x")?, 16).ok())
        .collect()
}

#[test]
fn the_island_states_its_grid_size() {
    assert!(ISLAND.contains(&format!(
        "def gridSheet : List Nat := gridSheetFrom 0 {}",
        GRID / 8
    )));
}

#[test]
fn answer_sheet_matches_the_island() {
    let computed = sheet(&DECISION);
    let words: Vec<String> = computed.iter().map(|w| format!("0x{w:016x}")).collect();
    assert!(
        island_sheet().as_ref() == Some(&computed),
        "the Rust folds answer otherwise than the island on the grid; the words they compute are\n{}",
        words.join(", ")
    );
}

/// The heal rule shipped before PF2: an omission heals only where the epoch wraps no revoked
/// recipient, carrier or not (two due heads with no seat holding both locked out).
const fn heals_shipped(newest: bool, wraps_revoked: bool, _carried: bool) -> bool {
    newest && !wraps_revoked
}

/// 0.154.0's gap rule (`OLD_GAP_RULE`): no omission the minter knew is a gap.
const fn gap_old(opened: bool, current: bool, wrapped: bool, known: bool, _heals: bool) -> bool {
    opened && current && !wrapped && !known
}

#[test]
fn the_grid_sees_every_replaced_rule() {
    let island = island_sheet().expect("the island states its answer sheet");
    let mutants = [
        (
            "the shipped heal rule (no carrier read)",
            Rules {
                heals: heals_shipped,
                ..DECISION
            },
        ),
        (
            "0.154.0's gap rule",
            Rules {
                gap: gap_old,
                ..DECISION
            },
        ),
        (
            "a gap rule blind to unknown seats",
            Rules {
                gap: |opened, current, wrapped, _, heals| opened && current && !wrapped && heals,
                ..DECISION
            },
        ),
        (
            "the carry reading the opened roster only",
            Rules {
                carry_deny: |_, opened| opened,
                ..DECISION
            },
        ),
        (
            "a denied carrier",
            Rules {
                carrier: |seats| {
                    seats
                        .into_iter()
                        .any(|(current, _, holds)| current && holds)
                },
                ..DECISION
            },
        ),
        (
            "an unguarded removal",
            Rules {
                strands: |_| false,
                ..DECISION
            },
        ),
    ];
    for (name, rules) in mutants {
        assert_ne!(sheet(&rules), island, "the grid does not see {name}");
    }
}
