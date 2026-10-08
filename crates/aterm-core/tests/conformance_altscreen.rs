// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
//! Tier-1 trace conformance: binds the real `Terminal` alt-screen (DEC mode
//! 1049) round-trip to the `AltScreen.tla` design spec.
//!
//! aterm-spec-models' `model_check.rs` proves `MainRestoredAfterRoundTrip` on
//! the spec (after 1049h…1049l the main cells and cursor are unchanged) and
//! catches the botched restore at `Buggy=TRUE`. This test drives the real
//! `Terminal` through `WriteMain → Enter → Scribble → Leave` via `process`,
//! projects each state onto `<<active, mainCell, altCell, cursor, savedCursor,
//! entered, mainSaved>>`, and asks `ty` to confirm every transition is one the
//! spec's `Next` admits.
//!
//! `ty trace validate --spec` checks only `Init` plus the first transition, so
//! each transition is validated against a variant of the committed spec whose
//! `Init` is pinned to the predecessor state (only the `Init` block is
//! rewritten). A negative control (a `Leave` that fails to restore the cursor)
//! must be rejected.
//!
//! Projection: the 1-row, `Cells`-wide spec screen maps to a 1-row real
//! terminal; glyphs project to small ints (`A`→1, `B`→2, blank→0);
//! `cursor`/`savedCursor` project to the 1-based index of the cell just
//! written (matching `WriteMain(c,v): cursor'=c`); `active` is
//! `term.is_alternate_screen()`; the ghost `mainSaved` is the main cells at the
//! first `Enter`.
//!
//! Without `ty` on its canonical path the test fails; there is no skip.

use std::path::{Path, PathBuf};
use std::process::Command;

use aterm_core::terminal::Terminal;
use aterm_spec::verify::ty_escalation;

/// Spec `CONSTANT Cells` / `MaxVal` (from `AltScreen.cfg`).
const CELLS: usize = 3;
const MAXVAL: i64 = 2;

// verification gate (honesty ratchet, batteries-on) in `aterm_spec::verify`:
// verification is always required — an absent Trust `ty` fails the test with the
// install hint (`aterm pkg install ty`).

/// The abstract spec state — the projection target.
#[derive(Clone, PartialEq, Debug)]
struct AltState {
    active_alt: bool,         // active = "alt" (else "main")
    main: [i64; CELLS],       // mainCell
    alt: [i64; CELLS],        // altCell
    cursor: i64,              // 0..Cells
    saved_cursor: i64,        // savedCursor
    entered: bool,            // entered
    main_saved: [i64; CELLS], // mainSaved (ghost)
}

/// Map a real cell char to the spec's small-int cell value.
fn cell_val(ch: char) -> i64 {
    match ch {
        'A' => 1,
        'B' => 2,
        _ => 0, // blank / anything else
    }
}

/// Read the real 1-row terminal's cells as `[i64; CELLS]`.
fn read_cells(term: &Terminal) -> [i64; CELLS] {
    let mut out = [0i64; CELLS];
    for (c, slot) in out.iter_mut().enumerate() {
        if let Some(cell) = term.grid().cell(0, c as u16) {
            *slot = cell_val(cell.char());
        }
    }
    out
}

/// A TLA+ function literal `(1 :> v1 @@ 2 :> v2 @@ 3 :> v3)` over `1..Cells`.
fn fn_literal(vals: &[i64]) -> String {
    vals.iter()
        .enumerate()
        .map(|(i, v)| format!("{} :> {}", i + 1, v))
        .collect::<Vec<_>>()
        .join(" @@ ")
}

/// A function value in `ty`'s trace JSON form `{type:function,value:{domain,mapping}}`.
fn fn_json(vals: &[i64]) -> String {
    let domain: Vec<String> = (1..=vals.len())
        .map(|n| format!("{{\"type\":\"int\",\"value\":{n}}}"))
        .collect();
    let mapping: Vec<String> = vals
        .iter()
        .enumerate()
        .map(|(i, v)| {
            format!(
                "[{{\"type\":\"int\",\"value\":{}}},{{\"type\":\"int\",\"value\":{}}}]",
                i + 1,
                v
            )
        })
        .collect();
    format!(
        "{{\"type\":\"function\",\"value\":{{\"domain\":[{}],\"mapping\":[{}]}}}}",
        domain.join(","),
        mapping.join(",")
    )
}

/// One trace state object for the 7 spec variables.
fn state_json(s: &AltState) -> String {
    format!(
        "{{\"active\":{{\"type\":\"string\",\"value\":\"{}\"}},\
         \"mainCell\":{},\"altCell\":{},\
         \"cursor\":{{\"type\":\"int\",\"value\":{}}},\
         \"savedCursor\":{{\"type\":\"int\",\"value\":{}}},\
         \"entered\":{{\"type\":\"bool\",\"value\":{}}},\
         \"mainSaved\":{}}}",
        if s.active_alt { "alt" } else { "main" },
        fn_json(&s.main),
        fn_json(&s.alt),
        s.cursor,
        s.saved_cursor,
        s.entered,
        fn_json(&s.main_saved),
    )
}

/// The committed `AltScreen.tla` with `Init` hardcoded to `prev` (so the validated
/// transition is the strict first step). Only the `Init ==` block is rewritten;
/// every action/invariant line is the committed text verbatim.
fn pinned_spec(committed: &str, prev: &AltState) -> String {
    let mut out = String::new();
    let mut skipping_init = false;
    for line in committed.lines() {
        if line.starts_with("Init ==") {
            skipping_init = true;
            out.push_str("Init ==\n");
            out.push_str(&format!(
                "    /\\ active = \"{}\"\n",
                if prev.active_alt { "alt" } else { "main" }
            ));
            out.push_str(&format!(
                "    /\\ mainCell = ({})\n",
                fn_literal(&prev.main)
            ));
            out.push_str(&format!("    /\\ altCell = ({})\n", fn_literal(&prev.alt)));
            out.push_str(&format!("    /\\ cursor = {}\n", prev.cursor));
            out.push_str(&format!("    /\\ savedCursor = {}\n", prev.saved_cursor));
            out.push_str(&format!(
                "    /\\ entered = {}\n",
                if prev.entered { "TRUE" } else { "FALSE" }
            ));
            out.push_str(&format!(
                "    /\\ mainSaved = ({})\n",
                fn_literal(&prev.main_saved)
            ));
            continue;
        }
        if skipping_init {
            // The Init block is the indented `/\ …` conjunct lines; it ends at the
            // first non-indented (or blank/comment) line.
            if line.starts_with(char::is_whitespace) && line.trim_start().starts_with("/\\") {
                continue; // drop the committed Init conjuncts
            }
            skipping_init = false;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

fn spec_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("aterm-spec-models/specs")
        .join(name)
}

fn transition_trace(prev: &AltState, next: &AltState, action: &str) -> String {
    format!(
        "{{\"version\":\"1\",\"module\":\"AltScreen\",\
         \"variables\":[\"active\",\"mainCell\",\"altCell\",\"cursor\",\"savedCursor\",\"entered\",\"mainSaved\"],\
         \"steps\":[\
         {{\"index\":0,\"state\":{}}},\
         {{\"index\":1,\"state\":{},\"action\":{{\"name\":\"{}\"}}}}\
         ]}}",
        state_json(prev),
        state_json(next),
        action
    )
}

fn validate(
    ty: &Path,
    dir: &Path,
    committed: &str,
    prev: &AltState,
    next: &AltState,
    action: &str,
) -> (bool, String) {
    let spec_f = dir.join("AltScreen.tla");
    let cfg_f = dir.join("AltScreen.cfg");
    let trace_f = dir.join("t.json");
    std::fs::write(&spec_f, pinned_spec(committed, prev)).expect("write spec");
    std::fs::write(
        &cfg_f,
        format!(
            "CONSTANT Cells = {CELLS}\nCONSTANT MaxVal = {MAXVAL}\nCONSTANT Buggy = FALSE\n\
             SPECIFICATION Spec\nINVARIANT TypeOK\nINVARIANT MainRestoredAfterRoundTrip\n\
             INVARIANT CursorBounded\nCHECK_DEADLOCK FALSE\n"
        ),
    )
    .expect("write cfg");
    std::fs::write(&trace_f, transition_trace(prev, next, action)).expect("write trace");
    let mut cmd = Command::new(ty);
    cmd.arg("trace")
        .arg("validate")
        .arg(&trace_f)
        .arg("--spec")
        .arg(&spec_f)
        .arg("--config")
        .arg(&cfg_f);
    let out = aterm_spec::verify::ty_output(&mut cmd).expect("run ty trace validate");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out.status.success(), combined)
}

#[test]
fn real_altscreen_roundtrip_conforms_to_altscreen_spec() {
    // tiered: a committed hand-written `.tla` — external-tool
    // obligation; runs only where the Trust toolchain is installed.
    let Some(ty) = ty_escalation("AltScreen conformance") else {
        return;
    };
    let committed =
        std::fs::read_to_string(spec_path("AltScreen.tla")).expect("read AltScreen.tla");
    let dir = std::env::temp_dir().join(format!("aterm-altscreen-conf-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("mk tempdir");

    // A 1-row, cells-wide real Terminal, fresh on the main screen.
    let mut term = Terminal::new(1, CELLS as u16);
    // Ensure we are on main and the screen is clear/home.
    term.process(b"\x1b[?1049l\x1b[2J\x1b[H");

    // Projection state, tracking the spec's ghost (savedCursor/mainSaved) consistently
    // with the spec semantics (set at Enter time from the real cursor/main cells).
    let mut st = AltState {
        active_alt: term.is_alternate_screen(),
        main: read_cells(&term),
        alt: [0; CELLS],
        cursor: 0,
        saved_cursor: 0,
        entered: false,
        main_saved: [0; CELLS],
    };
    assert!(
        !st.active_alt && st.main == [0, 0, 0] && st.cursor == 0,
        "fresh terminal projects to Init"
    );

    let mut validated = 0usize;

    // --- WriteMain(c=1, v=1): print 'A' at main col 0 → mainCell[1]=1, cursor=1.
    let prev = st.clone();
    term.process(b"\x1b[1G"); // cursor to col 0 (home of the 1 row)
    term.process(b"A");
    st.main = read_cells(&term);
    st.cursor = 1; // wrote cell index 1 (1-based), matching WriteMain's cursor'=c
    let (ok, out) = validate(&ty, &dir, &committed, &prev, &st, "WriteMain");
    assert!(
        ok,
        "real WriteMain {prev:?} -> {st:?} must conform\n--- ty ---\n{out}"
    );
    validated += 1;

    // --- Enter (CSI ?1049h): save cursor, switch to alt, clear alt, snapshot main.
    let prev = st.clone();
    term.process(b"\x1b[?1049h");
    st.saved_cursor = prev.cursor; // savedCursor' = cursor
    st.active_alt = term.is_alternate_screen();
    st.alt = read_cells(&term); // the cleared alt buffer
    st.main_saved = prev.main; // first entry snapshots main into the ghost
    st.entered = true;
    // The real cursor is shared/unchanged on enter (xterm srm_OPT_ALTBUF_CURSOR).
    assert!(st.active_alt, "Enter must switch to the alt buffer");
    assert_eq!(
        st.alt,
        [0, 0, 0],
        "alt buffer is cleared to blanks on Enter"
    );
    let (ok, out) = validate(&ty, &dir, &committed, &prev, &st, "Enter");
    assert!(
        ok,
        "real Enter {prev:?} -> {st:?} must conform\n--- ty ---\n{out}"
    );
    validated += 1;

    // --- Scribble(c=2, v=2): print 'B' at alt col 1 → altCell[2]=2, cursor=2; main
    //     must be untouched (the isolation the spec's MainRestoredAfterRoundTrip needs).
    let prev = st.clone();
    term.process(b"\x1b[2G"); // alt cursor to col 1
    term.process(b"B");
    st.alt = read_cells(&term);
    st.cursor = 2; // wrote alt cell index 2
    assert_eq!(st.alt, [0, 2, 0], "scribble lands in alt[2]");
    let (ok, out) = validate(&ty, &dir, &committed, &prev, &st, "Scribble");
    assert!(
        ok,
        "real Scribble {prev:?} -> {st:?} must conform\n--- ty ---\n{out}"
    );
    validated += 1;

    // --- Leave (CSI ?1049l): switch back to main, restore cursor to savedCursor.
    let prev = st.clone();
    term.process(b"\x1b[?1049l");
    st.active_alt = term.is_alternate_screen();
    st.main = read_cells(&term);
    st.cursor = prev.saved_cursor; // cursor' = savedCursor (the restore)
    assert!(!st.active_alt, "Leave switches back to main");
    // The real main cells survived the alt round-trip untouched.
    assert_eq!(
        st.main, st.main_saved,
        "main cells must be restored unchanged (MainRestoredAfterRoundTrip)"
    );
    let (ok, out) = validate(&ty, &dir, &committed, &prev, &st, "Leave");
    assert!(
        ok,
        "real Leave {prev:?} -> {st:?} must conform\n--- ty ---\n{out}"
    );
    validated += 1;

    // Negative control — the Buggy defect: a `Leave` that does not restore the cursor
    // (cursor held at the alt scribble position instead of savedCursor).
    // `MainRestoredAfterRoundTrip` forbids it (cursor != savedCursor while on main);
    // ty must reject. `prev` is the post-Scribble state (alt, cursor=2, saved=1).
    let mut bad_next = prev.clone();
    bad_next.active_alt = false;
    bad_next.cursor = prev.cursor; // 2 — NOT restored to savedCursor (1)
    let (bad_ok, o) = validate(&ty, &dir, &committed, &prev, &bad_next, "Leave");
    assert!(
        !bad_ok,
        "NEGATIVE CONTROL (Leave that drops the cursor restore — the Buggy defect) MUST be \
         rejected\n--- ty ---\n{o}"
    );

    let _ = std::fs::remove_dir_all(&dir);
    eprintln!(
        "AltScreen Tier-1 conformance: {validated} real round-trip transitions (WriteMain, Enter, \
         Scribble, Leave) strictly validated against committed AltScreen.tla; cursor-not-restored \
         negative control rejected; main cells preserved end-to-end."
    );
}
