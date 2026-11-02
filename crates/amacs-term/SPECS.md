<!-- Copyright 2026 Andrew Yates -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# amacs-term: specs, proofs and the Trust lane

What amacs-term proves through Trust/Clean (§1), what it cannot prove yet and why (§2), and which
tests check what (§3). Each §1 fn carries an `ensures` clause citing a Clean theorem stated over
`trust_import_amacs_term__<module>__<fn>`, the kernel definition Trust mints from the compiled
body. Verdicts and refusals were measured on `targo 1.99.0-dev (41d799797)`, the prover
`crates/aterm-release/trust/floors.tsv` records for this crate. "Gap N" is row N of §7 of
`docs/AUDIT-trust-for-test-migration-2026-10-15.md`; how the contracts are written is
`docs/PLAN-tests-to-specs-2026-10-15.md`, "The recipe that works".

## 0. Notation used in the statements

- The nine inputs of design §6.1 (`decide::Inputs`): `tty` = `-nw`/`--tty`; `term` = `open_in` is
  `Terminal` (the design's `[amacs] open_in = "terminal"`; `run_client` always passes the default,
  `Window`); `parent` = `ATERM_PARENT_SESSION_ID` is set, non-empty and Unicode; `tmux`/`sty` =
  `TMUX`/`STY` is set and non-empty; `stdin`/`stdout` = that stream is a terminal; `wait` =
  `--wait`; `nowait` = `--no-wait`.
- `Hosted` = `parent ∧ ¬tmux ∧ ¬sty` (an aterm window hosts this terminal with no multiplexer
  between). `Asked` = `tty ∨ term`.
- `WAIT_*` (`decide.rs`): `FINISHED` 0, `CLOSED` 1, `ABORTED` 2, `REPLACED` 3, `FAILED` 4.
- `Word` = `[0-9A-Za-z._-]`: the bytes `0x30..=0x39`, `0x41..=0x5A`, `0x61..=0x7A`, `0x2E`,
  `0x5F`, `0x2D`.
- The answers to the terminal face's `Save FILE? (y, n, q)` (`tui.rs`), as the byte each key types
  (`answer_byte`: Backspace is DEL, Escape ESC, `C-g` BEL): `Saves` = `{0x20, 0x21, 0x59, 0x79}`
  (SPC `!` `Y` `y`); `Discards` = `{0x4E, 0x6E, 0x7F}` (`N` `n` DEL); `Cancels` = `{0x07, 0x1B,
  0x51, 0x71}` (BEL ESC `Q` `q`).
- `OSC52_LIMIT` = 65536 bytes, the most aterm takes from one OSC 52 write. A
  `const _: () = assert!(OSC52_LIMIT == 65_536)` beside it fails the build if the constant leaves
  the literal the clause and theorem spell.

## 1. Proved specs (TSV)

Columns: `id`, `fn`, `statement`, `theorem`, `verdict` (measured), `negative control` (mutation,
then what the prover said), `tests`.

```tsv
id	fn	statement	theorem	verdict	negative control	tests
native-face	decide::native_face	∀ 9 bools: result ⇔ ¬Asked ∧ Hosted ∧ ((stdin ∧ stdout) ∨ wait ∨ nowait). The body reads the rows in the design's order (early returns); the clause is the closed form. Consequence: an agent's tool call (no terminal) opens a view, and so can hang on one, only when it passed --wait or --no-wait.	native_face_is_its_rows (simp only [import]; cases on the 9 bools; rfl)	11/11 rows proved, clean-kernel-citation, Certified	(stdin_tty && stdout_tty) → (stdin_tty || stdout_tty) in the body: `Clean island declaration rejected: InTactic { tactic: "rfl", chain: "`<;>` > `rfl`", … TacticFailed(TypeMismatch …) }` for native_face_is_its_rows and native_face_and_no_terminal_exit_are_exclusive; every citation then `cannot be validated because an earlier Clean island was rejected and the crate-local environment is tainted`; the fn's rows: 2 postcondition failed, 7 proved; the crate does not compile under the lane. Restored byte for byte (cmp).	tests/decide_table.rs::every_row_of_the_design_table (stays: it checks `decide`, §3)
no-terminal-exit	decide::no_terminal_exit	∀ 9 bools: result ⇔ ¬Asked ∧ Hosted ∧ ¬(stdin ∧ stdout) ∧ ¬wait ∧ ¬nowait: the run exits EXIT_USAGE (2) saying NO_TERMINAL.	no_terminal_exit_is_its_rows (as above)	11/11 rows proved, clean-kernel-citation, Certified	!wait && !no_wait → !wait (forget --no-wait): `InTactic { tactic: "rfl", … TypeMismatch … }` for no_terminal_exit_is_its_rows and the exclusivity theorem; citations tainted; the fn's rows: 2 postcondition failed, 6 proved. Restored byte for byte.	same
faces-exclusive	decide::{native_face,no_terminal_exit}	∀ 9 bools: ¬(native_face ∧ no_terminal_exit), so `decide` reads one table whichever row fn it asks first.	native_face_and_no_terminal_exit_are_exclusive (an island theorem over both imports; kernel-checked whenever Trust verifies the crate, not an `ensures`)	kernel-checked (`Clean island kernel-checked after program admission: 3 declaration(s) registered (native_face_is_its_rows, no_terminal_exit_is_its_rows, native_face_and_no_terminal_exit_are_exclusive)`)	both mutations above make it fail (`rfl` TypeMismatch)	same
wait-status	decide::wait_status	∀ end: u8: result = wait_status_table(end), the table of design §6.5: FINISHED, CLOSED → 0 (go on to the next file); REPLACED → 75 (aterm_ctl::EXIT_REPLACED); every other byte, ABORTED and FAILED among them → 1. Read off the table: status 0 only for a finished or closed view, so an abort never lets `git commit` take the message.	wait_status_is_the_table (simp only [import, wait_status_table])	2/2 rows proved, clean-kernel-citation, Certified	WAIT_FINISHED | WAIT_CLOSED => 0 → WAIT_FINISHED | WAIT_ABORTED => 0 (an abort exits 0): `InTactic { tactic: "simp", chain: "`simp`", … TacticFailed(UnsolvedGoals { count: 1, … }) }`; citations tainted; the fn's rows: 2 unsupported_mir unknown (nothing discharges the clause). Restored byte for byte.	client.rs::{success_writes_zero_bytes_to_the_tty, no_wait_never_waits_and_an_abort_is_exit_1} (stay: they check `edit_files`, §3)
for-word-byte	client::is_for_word_byte	∀ byte: u8: result ⇔ byte ∈ Word. `for_word` keeps exactly these bytes of the parent's file name (read as bytes, never lossily).	is_for_word_byte_is_the_class (simp only [import])	10/10 rows proved, clean-kernel-citation, Certified	b'_' → b' ' (a space in the wire word): `InTactic { tactic: "simp", chain: "`simp`", … UnsolvedGoals { count: 1, … } }`; citations tainted; the fn's rows: 1 postcondition failed, 7 proved, 1 unknown. Restored byte for byte.	client.rs::{for_word_is_the_parents_file_name_as_a_wire_word, the_word_byte_class_is_the_engines_for_word_alphabet} (stay: §3)
save-answer	tui::saves_on	∀ byte: u8: result ⇔ byte ∈ Saves. The question saves the file and closes on exactly these keys.	saves_on_is_the_class (simp only [import])	6/6 rows proved, clean-kernel-citation, Certified	b'!' → b'?' in the body: `Clean island declaration rejected: InTactic { tactic: "simp", chain: "`simp`", … TacticFailed(UnsolvedGoals { count: 1, … }) }` for saves_on_is_the_class; every citation of the crate then tainted; the fn's rows: 2 postcondition failed, 3 proved, 1 unknown; the lane: `RATCHET-REGRESSION(<121) 109 458`, exit 1. Restored byte for byte.	src/tui.rs::a_key_answers_with_the_byte_it_types, tests/tui_loop.rs::dirty_quit_prompts_and_n_discards (stay: §3)
discard-answer	tui::discards_on	∀ byte: u8: result ⇔ byte ∈ Discards. The question closes the file without saving on exactly these keys.	discards_on_is_the_class (simp only [import])	5/5 rows proved, clean-kernel-citation, Certified	0x7f → 0x7e: the same rejection for discards_on_is_the_class; citations tainted; the fn's rows: 2 failed, 2 proved, 1 unknown; `RATCHET-REGRESSION(<121) 109 458`. Restored byte for byte.	same
cancel-answer	tui::cancels_on	∀ byte: u8: result ⇔ byte ∈ Cancels. The question is taken back (`Quit`) and editing goes on, on exactly these keys. Read off the three statements: the classes are disjoint, so no key both saves and discards.	cancels_on_is_the_class (simp only [import])	6/6 rows proved, clean-kernel-citation, Certified	b'Q' → b'R': the same rejection for cancels_on_is_the_class; citations tainted; the fn's rows: 2 failed, 3 proved, 1 unknown; `RATCHET-REGRESSION(<121) 109 458`. Restored byte for byte.	same
osc52-limit	tui::fits_osc52	∀ len: usize: result ⇔ len ≤ 65536. A kill reaches the terminal's clipboard only when aterm would take the write whole; a longer kill stays in the kill ring alone.	fits_osc52_is_the_limit (n : UInt64; simp only [import])	2/2 rows proved, clean-kernel-citation, Certified	`len <= OSC52_LIMIT` → `len < OSC52_LIMIT`: `InTactic { tactic: "simp", … UnsolvedGoals { count: 1, detail: "… ⊢ Decidable.decide (fvar#0 < UInt64.ofNat 65536) … = decide (Nat.le …)" } }` for fits_osc52_is_the_limit; citations tainted; the fn's rows: 1 failed, 1 unknown; `RATCHET-REGRESSION(<121) 110 458`. Restored byte for byte.	tests/tui_loop.rs::kill_writes_osc52_when_enabled (stays: §3)
```

Each negative control is one mutation of the fn's body, run through
`targo --unverified ship trust run -p amacs-term --obligations` and then reverted.

Three rules for the next contract. (1) The cited theorem states the clause exactly as Trust
elaborates it: Rust's `&&`/`||` nest to the left and Clean's to the right, so a left-nested clause
needs explicit parentheses in the theorem and a case split (`simp only` closes only a goal whose
two sides nest alike), while a right-nested clause (as `is_for_word_byte`'s) closes by `simp only`
alone. (2) rustc lexes an island on every compiler: no `≤` or `·` (write `<=`; no focus dots).
(3) An island that names a `trust_import_*` exists only while Trust verifies, so `verify_island!`
expands only under `cfg(trust_verify)` (`src/lib.rs`); gated on `clean_islands`, a plain
`targo --unverified build` refuses every island here with `AutoBoundHeadOverApplied`. The islands
are kernel-checked at every Trust verification (the lane), not at every Trust build.

## 2. Properties not provable today (TSV)

```tsv
id	fn	statement	what blocks it (measured on 41d799797 unless marked)
face-code	decide::decide	The whole design §6.1 table as one scalar fn: face_code(9 bools) -> u8 (0 native, 1 terminal, 2 exit).	Measured with a two-bool probe `probe_face_code(a: bool, b: bool) -> u8` citing a Clean def: `citation … failed the strict Clean statement/certification audit: the postcondition goal could not be built (the elaborated goal does not typecheck (TypeMismatch { expected: Bool, inferred: UInt8, … }))`. When every parameter is `bool`, the goal builder types `result` as `Bool` (aterm-render/SPECS.md reports the same on 150efe4a). Worked around with two bool cores (§1).
decide-wrapper	decide::decide	decide(i) = Native ⇔ native_face(i), Exit(EXIT_USAGE, NO_TERMINAL) ⇔ no_terminal_exit(i), Terminal otherwise.	Gap 1: `Inputs` (a struct with an enum field) and `Face` (an enum carrying a `String`) get no E6 import. The wrapper is three calls; the table test checks it over all 512 inputs.
after-handoff	decide::after_handoff, client::handoff_of	Refused, Unreachable → Terminal; TimedOut → Exit(1, NO_ANSWER); Opened → Native; and the face agrees with `EditErr::falls_back_to_terminal`.	Gap 1 (enum in, enum with a `String` out); the second half is a property of aterm-ctl.
edit-files	client::edit_files	Files are opened one after another, each waited before the next unless --no-wait; on success zero bytes reach the caller's terminal; the run's status is the first nonzero wait_status.	Gap 1 and trait objects (`&mut dyn Window`, `&mut dyn Write`), a loop, `Path`/`String`. The lane row: `assertion: [trust-absent-callee-assumption] call to absent callee `amacs_term::client::target_uri` (body not in the lowered bundle) may panic` (failed).
command-line	client::parse_args, parse_position	The flag grammar, `+LINE[:COL]` positive decimal for the next file only, `--`, and the usage errors.	Gap 1 (`&[OsString]`, `String`, `Result<Command, String>`, `Option<(u32, Option<u32>)>`) and loops over iterators.
for-word	client::for_word	for_word(p) is the name's Word bytes, at most 32, `None` exactly when none.	Gap 1 (`Option<&Path>`, `String`, an iterator chain); its byte class is `for-word-byte` (§1).
target-uri	client::target_uri	The URI decodes back to the realpath (a missing file: its directory's realpath plus its name).	Filesystem effects (`canonicalize`, through amacs-files' `host::open_path` since T3, which the terminal face opens with too), and the codec is amacs's. Lane rows: hardened_path_identity unknown ("canonicalization is not a filesystem identity proof").
wait-status-zero	decide::wait_status	wait_status(e) = 0 → e = FINISHED ∨ e = CLOSED, as a theorem of its own over the import.	Attempted; the island was rejected each time, so it was dropped (the table theorem holds, and the property is read off it): `rw [if_neg …] at h; exact absurd h h1'` → `InTactic { tactic: "exact", … TacticFailed(ProofNotProduced) }`; `revert h; decide` → `InTactic { tactic: "decide", … ProofNotProduced }`; focus dots (`·`, `.`) do not lex or parse in an island.
sigint-130	(none)	C-c in the caller ends the client with status 130 and leaves the view open.	Not a property of code here: the client installs no SIGINT handler, so the shell reports SIGINT's default action as 128 + 2. Nothing to contract.
terminal-loop	tui::Tui::{run, event, input, effect, close, answer, save, suspend}	The terminal face (design §5): every key reaches the engine (`handle_input`) and its effects run in order; a save is amacs-files' atomic save with its compare-and-swap (a file changed on disk is never overwritten without a second `C-x C-s`); a changed file is never closed without the question; files edited in turn share the kill ring and nothing else (`open` forgets what the workspace kept for the new file's document id, which every earlier file of the run had); a continue after a stop reads the terminal's size again and draws one whole frame; the terminal's modes are put back in reverse order on every exit.	Gap 1 and generics: `Tui` and `Buffer` hold the engine's workspace and store, `Tty` is a trait (`T: Tty`), and the effects are enums carrying `String`s. Lane rows: 64 `unsupported_mir` "[trust-instance-hypothesis] call to `amacs_term::tui::Tui::start::<T>`…" and 20 "instance discharge: site `<T as aterm_tty::tty::Tty>…`" (generic calls the prover does not instantiate); `Tui::edit` and `Tui::run` carry the crate's two new `failed` rows, "[trust-absent-callee-assumption] call to absent callee `amacs_term::tui::Tui::open`" and "`…::Tui::start::<T>` (body not in the lowered bundle) may panic" (unproved, not refuted). tests/tui_loop.rs drives it end to end (§3).
terminal-render	render::{layout, frame, draw_body, draw_line, lay_echo, modeline_text}	The frame: the body rows, then the reverse-video modeline, then the echo area, which grows to a quarter of the rows for a long message and never hides the modeline; a tab reaches the next four-column stop; a wide character that does not fit wraps to the next row whole; the cursor sits where the engine's caret is drawn.	Gap 1 (`Screen`, `Style`, the engine's projection and lines, `String`), iterator chains. `layout`'s subtractions are `saturating_sub`/`checked_sub` and `tab_cells` is a match on `column % 4` (no overflow row); the unit tests in src/render.rs and the oracle reads in tests/tui_loop.rs check the rest.
tab-stop	render::tab_cells	∀ column: usize: result = 4 − column mod 4, the cells from a tab at `column` to the next four-column stop.	Gap 3, measured in the finalizer's run (lane on the T3 tree merged with origin/main 4eac0a202d, `--obligations`): the body as a contract, `match column % 4 { 0 => 4, 1 => 3, 2 => 2, _ => 1 }` with `ensures result == tab_cells_spec(column)` and the Clean def mirroring it, was refused: `Clean island declaration rejected: AutoBoundHeadOverApplied { name: "trust_import_amacs_term__render__tab_cells", func_type: "fvar#9223372036854775808", remaining_args: 1 }` (no kernel import: the `%` carries MIR's remainder-by-zero assert, a checked-arithmetic assert E6 refuses). The contract was taken out. The lane proves the fn's one row (`column % 4`, `hardened_panic_boundary`), and `a_tab_reaches_the_next_four_column_stop` checks the cells against the engine's `editor_grapheme_columns`. fragment: `render::tab_cells` F1 (VF5 first target, docs/PLAN-tests-to-specs-2026-10-15.md "F1-ready specs", row 69). Proved via mirror: Clean R216 row `tab_cells` (`RowProved`; the verbatim function is in the artifact and certifies, the harness's copy writes its `match column % 4` as the equivalent `if` chain), with a false-clause control; not yet credited: Trust's compiler mints no calculus row until it pins a VF5 base (E4.7).
echo-limit	render::echo_limit	∀ rows: u16: result = max(rows / 4, 1), the most rows the echo area grows to.	Gap 3, measured in the same run: written as the select `if rows < 4 { 1 } else { rows / 4 }` (`max` is a call, which E6 refuses) with `ensures result == echo_limit_spec(rows)`, it was refused the same way: `AutoBoundHeadOverApplied { name: "trust_import_amacs_term__render__echo_limit", … remaining_args: 1 }` (the `/` carries MIR's division-by-zero assert). The contract was taken out and the body is `(rows / 4).max(1)` again. The lane proves its one row (`rows / 4`, `hardened_panic_boundary`), and `the_layout_keeps_the_echo_area_then_the_modeline` checks it.
answer-byte	tui::answer_byte	The byte a key answers with: the ASCII it types with no chord, DEL for Backspace, ESC for Escape, BEL for `C-g`; none for any other key.	Gap 1 (`&Key`, a `#[non_exhaustive]` enum over `NamedKey` and `char`, and `Option<char>`); its classes are `save-answer`, `discard-answer` and `cancel-answer` (§1).
```

## 3. Test → spec map

Every test stays: each also reaches code no proof does (a wrapper over an enum or a struct, a
trait object, or another crate).

```tsv
file	test	spec	why it stays
tests/decide_table.rs	every_row_of_the_design_table	native-face, no-terminal-exit, after-handoff	checks `decide` and `after_handoff` (enums, a struct) over all 512 inputs
src/client.rs	success_writes_zero_bytes_to_the_tty	edit-files, wait-status	trait objects, file system, the zero-byte terminal
src/client.rs	no_wait_never_waits_and_an_abort_is_exit_1	edit-files, wait-status	the same; also maps every `aterm_ctl::edit::AwaitErr` (a refused and an unsendable wait end the run 1)
src/client.rs	realpath_and_percent_encoding_of_symlinked_and_spaced_paths	target-uri	file system and amacs's codec
src/client.rs	the_command_line_reads_flags_positions_and_files	command-line	Gap 1
src/client.rs	for_word_is_the_parents_file_name_as_a_wire_word	for-word	Gap 1; a name that is not UTF-8
src/client.rs	the_word_byte_class_is_the_engines_for_word_alphabet	for-word-byte	ties the proved class to amacs's request parser, another crate
src/client.rs	a_failed_handoff_takes_the_face_the_control_client_names	after-handoff	enums, and aterm-ctl's reading of the error
src/tui.rs	a_key_answers_with_the_byte_it_types	answer-byte, save-answer, discard-answer, cancel-answer	checks `answer_byte` (an enum in), and its loop over all 256 bytes checks that the three classes are disjoint, which is read off the statements, not a kernel theorem
src/tui.rs	a_position_names_a_line_and_a_character_on_it	terminal-loop	`+LINE:COL` to a byte offset over the engine's lines (Gap 1)
src/tui.rs	relexing_an_edit_matches_a_fresh_lex	terminal-loop	the incremental relex against amacs's lexer, another crate
src/tui.rs	a_paste_breaks_lines_with_line_feeds	terminal-loop	`String` (Gap 1)
src/tui.rs	input_releases_commit_projections_even_after_a_partial_macro_error	terminal-loop	macro error recovery and projection release over the engine's workspace (Gap 1)
src/render.rs	the_layout_keeps_the_echo_area_then_the_modeline	terminal-render	`Layout` holds a `Range` and an `Option` (Gap 1)
src/render.rs	a_long_message_wraps_and_completions_are_cut	terminal-render	`Screen` and `String` (Gap 1)
src/render.rs	a_tab_reaches_the_next_four_column_stop	terminal-render	draws through `Screen` (Gap 1)
src/render.rs	the_modeline_reads_as_emacs_writes_it	terminal-render	`String` (Gap 1)
src/render.rs	a_press_finds_the_byte_drawn_under_it	terminal-render	`DrawnRow` (Gap 1)
tests/tui_loop.rs	type_save_quit_round_trip	terminal-loop, terminal-render	the loop end to end over a `MemTty`, aterm-core's `Terminal` reading the bytes as the screen
tests/tui_loop.rs	isearch_and_kill_yank_in_terminal_profile	terminal-loop	the same; the Terminal profile's `C-s`
tests/tui_loop.rs	dirty_quit_prompts_and_n_discards	terminal-loop, discard-answer	the same; the question in the echo area
tests/tui_loop.rs	resize_repaints_to_new_size	terminal-loop, terminal-render	the same; SIGWINCH and the in-band resize
tests/tui_loop.rs	save_refuses_when_disk_changed	terminal-loop	the same; amacs-files' compare-and-swap, another crate
tests/tui_loop.rs	kill_writes_osc52_when_enabled	terminal-loop, osc52-limit	the same; the OSC 52 bytes
tests/tui_loop.rs	bom_and_crlf_survive_edit_and_save	terminal-loop	the same; amacs-files' held mapping, another crate
tests/tui_loop.rs	a_press_sets_point_and_the_wheel_scrolls	terminal-loop, terminal-render	the same; SGR mouse reports
tests/tui_loop.rs	files_in_turn_share_the_kill_ring_and_nothing_else	terminal-loop	the same over two files: an undo and a pop-mark in the second never reach the first's edits and marks, a yank takes its kill (the engine's workspace, another crate)
tests/tui_loop.rs	finish_saves_and_abort_ends_the_run	terminal-loop, wait-status	the same: `C-c C-c` saves and goes on, `C-c C-k` ends the run 1 and opens no later file
tests/tui_loop.rs	a_stop_redraws_once_at_the_size_it_continues_at	terminal-loop, terminal-render	the same: `C-z` and an outside SIGTSTP, a resize while stopped that no SIGWINCH reports (a `Tty` wrapper over `MemTty`)
tests/tui_loop.rs	a_refused_save_says_why_in_the_save_words	terminal-loop	the loop end to end: amacs-files' save refusal surfaced in the echo area
tests/tui_loop.rs	a_refused_open_says_why_in_the_save_words	terminal-loop	the loop end to end: amacs-files' open refusal surfaced in the echo area
```

`crates/aterm/tests/amacs_pty.rs` runs the built binary as `amacs` under a real pty.
`amacs_edits_a_file_under_a_real_pty` (outside a window, and under tmux inside one) types `hello`,
`C-x C-s`, `C-x C-c` and checks the file and that the terminal's settings are put back;
`sigterm_puts_the_terminal_back` and `a_panic_puts_the_terminal_back` (the debug-only seam
`ATERM_AMACS_INJECT_PANIC`, read through `aterm_types::dev_seam!`) check that a SIGTERM or a panic
mid-edit puts the settings and the main screen back and saves nothing. They cover the `amacs`
alias, `DevTty`, termios and aterm-tty's fatal-signal and panic guard, which no proof here reaches.

## 4. The Trust lane

`targo --unverified ship trust run -p amacs-term` runs `targo trust check -p amacs-term --lib
--allow-l0-gaps` under the lane's prover and memory guard. The floor is the crate's row in
`crates/aterm-release/trust/floors.tsv`; its `memory.tsv` row records no need, so the lane never
refuses a smaller host up front.

The unproved rows, by kind. None is an overflow, an out-of-range index or an `unwrap` on input:
row and column arithmetic is `saturating_*`/`checked_*` or bounded by the screen, and indexing goes
through `.get()`.

| kind | the lane's reason, shortened | real hazard? |
|---|---|---|
| `unsupported_mir` | the `GlobalAlloc` assumption, "DISCHARGED vacuously: no `#[global_allocator]` is registered" | no: one per allocating body |
| `unsupported_mir` | "[trust-instance-hypothesis]" and "instance discharge" on the generic loop (`Tui::start::<T>`, `suspend::<T>`, `refit::<T>`, `<T as Tty>` sites) | no: the prover does not instantiate `T: Tty` (a `DevTty`, or a test's `MemTty`) |
| `unsupported_mir` | `UnboundedAllocation` (`Iterator::collect` in `minibuffer_line`, `<[T]>::resize` with a non-integer count) | no: sizes come from the terminal's rows and the completion list |
| `assertion` runtime_checked | absent callees (bodies not lowered: the engine, amacs-files, aterm-tty, aterm-ctl, aterm-uds, std, `alloc::fmt::format`), bundled std members' internal asserts, and a `panic_fmt` in `Tui::event` | no; the `panic_fmt` is the injected-panic seam, which only a debug or `dev-seams` build reads |
| `assertion` failed | `client::say` (`dyn Write::write_fmt`), `client::edit_files` (absent `target_uri`), `Tui::edit` (absent `Tui::open`), `Tui::run` (absent `Tui::start::<T>`) | no: unproved, not refuted |
| `hardened_ffi_boundary` | "unmodeled FFI call: no summary" in `OsString`/`Path` plumbing | no |
| `hardened_error_discard` | `.ok()` or `unwrap_or_default` in `parse_position`, `parse_args`, `parent_program`, `answer_byte`, `line_column_offset`, `osc52`, `draw_body`, `relex`, `open` | no: each default is the intended meaning |
| `hardened_path_identity` | `std::fs::canonicalize`: "canonicalization is not a filesystem identity proof" | no: the realpath is the name handed to the window (design §6.1), which opens the file through amacs-files' host |
| `hardened_process_semantics` | `std::io::stdout()` and `aterm_uds::stdout::answer_exit` | no: `answer_exit` treats EPIPE as the answer written |
| `hardened_byte_loss` | `parse_args`'s `arg.to_string_lossy()`; `open`'s lossy file name and path | no: the lossy text only matches flags or is shown; files are named by their `OsString`/`PathBuf` |

Re-measure (one `targo` at a time per checkout):

```sh
targo --unverified ship trust run -p amacs-term --obligations     # per-row record: target-trust.noindex/logs/amacs-term@*.log.oblig.tsv
targo --unverified ship trust run -p amacs-term --update-ratchet  # raise the floor
```
