// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Generates the 36 `JoinWith<Rhs>` impls from a single source-of-truth
//! matrix encoding the lattice join in §3.1 of
//! `designs/2026-04-19-provenance-framework.md`.
//!
//! Output: `$OUT_DIR/join_table.rs` — included via `include!` from lib.rs.
//! Per the design (§12 Risks), generating via build.rs keeps the table in
//! one place, prevents accidental table-lib drift, and keeps incremental
//! compiles fast (no proc-macro).

use std::env;
use std::fs;
use std::io::Write;
use std::path::PathBuf;

/// The 6 origin marker names. Index ordering matches `OriginTag` discriminants
/// so index lookups compose with `OriginTag as u8`.
const ORIGINS: [&str; 6] = [
    "Host",
    "ConfigFile",
    "User",
    "Ai",
    "NetworkUntrusted",
    "Pty",
];

/// Encodes the §3.1 join table. `table[lhs][rhs] = output origin index`.
///
/// Row/column order matches `ORIGINS`. Hand-lifted from the design document;
/// a unit test re-asserts every cell against the runtime join function so
/// table drift is caught at `cargo test` time.
///
/// Invariants enforced by the consistency test in `lib.rs`:
///   * commutative: table\[a\]\[b\] == table\[b\]\[a\]
///   * idempotent: table\[a\]\[a\] == a
///   * Host is identity: table\[Host\]\[x\] == x
///   * Pty is absorbing: table\[Pty\]\[x\] == Pty
#[rustfmt::skip]
const JOIN_TABLE: [[usize; 6]; 6] = [
    //                        Host  Config  User   Ai    NetU   Pty
    /* Host             */  [   0,    1,    2,    3,    4,    5 ],
    /* ConfigFile       */  [   1,    1,    2,    3,    4,    5 ],
    /* User             */  [   2,    2,    2,    3,    4,    5 ],
    /* Ai               */  [   3,    3,    3,    3,    4,    5 ],
    /* NetworkUntrusted */  [   4,    4,    4,    4,    4,    5 ],
    /* Pty              */  [   5,    5,    5,    5,    5,    5 ],
];

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR is set by cargo"));
    let out_path = out_dir.join("join_table.rs");
    let mut f = fs::File::create(&out_path).expect("create join_table.rs");

    writeln!(
        f,
        "// Auto-generated from build.rs; do not edit. See design §3.1.\n"
    )
    .unwrap();

    // Emit 36 `JoinWith` impls.
    for (lhs_i, &lhs) in ORIGINS.iter().enumerate() {
        for (rhs_i, &rhs) in ORIGINS.iter().enumerate() {
            let out_i = JOIN_TABLE[lhs_i][rhs_i];
            let out = ORIGINS[out_i];
            writeln!(
                f,
                "impl JoinWith<{rhs}> for {lhs} {{ type Output = {out}; }}",
            )
            .unwrap();
        }
    }

    // Emit a const table so runtime code can cross-check the type-level impls.
    writeln!(f, "\n/// Runtime mirror of the §3.1 join table.").unwrap();
    writeln!(
        f,
        "/// Indexed by `OriginTag as usize` on both axes. Returns a concrete"
    )
    .unwrap();
    writeln!(f, "/// `OriginTag` (never `Top` — see §3.2).").unwrap();
    writeln!(f, "pub(crate) const JOIN_TABLE_RT: [[OriginTag; 6]; 6] = [").unwrap();
    for row in JOIN_TABLE.iter() {
        write!(f, "    [").unwrap();
        for (i, &cell) in row.iter().enumerate() {
            if i > 0 {
                write!(f, ", ").unwrap();
            }
            write!(f, "OriginTag::{}", ORIGINS[cell]).unwrap();
        }
        writeln!(f, "],").unwrap();
    }
    writeln!(f, "];").unwrap();

    // Emit OriginsCompatible impls: A dominates B per §3 Hasse diagram.
    // Implementation: A dominates B iff join(A, B) == B (widening to B from A
    // preserves B, i.e. A is at least as trusted as B). This derives from the
    // lattice's "widest-origin-wins" definition.
    writeln!(f, "\n// OriginsCompatible<Required> impls.").unwrap();
    writeln!(
        f,
        "// `A: OriginsCompatible<B>` iff A dominates B (A is at least as trusted as B)."
    )
    .unwrap();
    for (a_i, &a) in ORIGINS.iter().enumerate() {
        for (b_i, &b) in ORIGINS.iter().enumerate() {
            // A dominates B iff join(A, B) == B.
            let join_ab = JOIN_TABLE[a_i][b_i];
            if join_ab == b_i {
                writeln!(f, "impl OriginsCompatible<{b}> for {a} {{}}").unwrap();
            }
        }
    }
}
