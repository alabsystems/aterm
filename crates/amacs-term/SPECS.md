<!-- Copyright 2026 Andrew Yates -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# amacs-term: specs, proofs and the Trust lane

> Status 2026-10-20 (slice TR4). Four specs are proved through Trust/Clean about the compiled
> Rust fns: `native-face`, `no-terminal-exit`, `wait-status` and `for-word-byte`. Each fn carries
> an `ensures` clause that cites a Clean theorem stated over `trust_import_amacs_term__<module>__<fn>`,
> the kernel definition Trust mints from the compiled body. On the installed prover every row of
> each fn is `proved`, solver `clean-kernel-citation`, assurance `Certified`
> (`validation=KernelRechecked`, `axiom_closure=Empty`). Each spec also has a negative control: a
> mutated body makes Clean reject the theorem and the build fail. No test was deleted: every test
> here also checks a wrapper (an enum, a struct, a trait object) or another crate, which no proof
> reaches yet (§3).

Prover: `targo 1.99.0-dev (41d799797 2026-10-16) (targo 0.10.0+g41d799797)`, the seal at
`~/toolchains/trust-current` (`trust-41d79979`), which is what `targo --unverified ship trust run`
uses on this host. Gap numbers are the rows of §7 of
`docs/AUDIT-trust-for-test-migration-2026-10-15.md` (historical internal audit). How the contracts
are written is `docs/PLAN-tests-to-specs-2026-10-15.md`, "The recipe that works".

## 0. Notation used in the statements

- The nine inputs of design §6.1 (`decide::Inputs`): `tty` = `-nw`/`--tty`; `term` =
  `[amacs] open_in = "terminal"`; `parent` = `ATERM_PARENT_SESSION_ID` names a session; `tmux` =
  `TMUX` set; `sty` = `STY` set; `stdin`/`stdout` = that stream is a terminal; `wait` = `--wait`;
  `nowait` = `--no-wait`.
- `Hosted` = `parent ∧ ¬tmux ∧ ¬sty` (an aterm window hosts this terminal with no multiplexer
  between). `Asked` = `tty ∨ term`.
- `WAIT_*` (`decide.rs`): `FINISHED` 0, `CLOSED` 1, `ABORTED` 2, `REPLACED` 3, `FAILED` 4.
- `Word` = the bytes `0x30..=0x39`, `0x41..=0x5A`, `0x61..=0x7A`, `0x2E`, `0x5F`, `0x2D`
  (`[0-9A-Za-z._-]`).

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
```

The negative controls ran with `targo --unverified ship trust run -p amacs-term --obligations` on
the mutated tree, one mutation at a time; after each, the file was copied back and `cmp`
confirmed it byte for byte against the saved original.

Three rules, measured here, for whoever writes the next one. (1) The cited theorem must state
the clause exactly as Trust elaborates it: Rust's `&&`/`||` nest to the left, Clean's to the
right, so a left-nested clause needs explicit parentheses in the theorem (and a case split, since
`simp only` closes only a goal whose two sides nest alike), while a clause written right-nested
(as `is_for_word_byte`'s) closes by `simp only` alone. (2) An island is lexed by rustc on every
compiler: no `≤` or `·` (write `<=`, and no focus dots). (3) An island that names a
`trust_import_*` exists only while Trust verifies, so `verify_island!` is gated on
`cfg(trust_verify)`, as in aterm-log, not on aterm-bidi's `cfg(clean_islands)`: gated on
`clean_islands`, a plain `targo --unverified build` refuses every island here with
`AutoBoundHeadOverApplied { name: "trust_import_amacs_term__decide__native_face", … }`. The
islands are therefore kernel-checked at every Trust verification (the lane), not at every Trust
build, and the crate has no build script.

## 2. Properties not provable today (TSV)

```tsv
id	fn	statement	what blocks it (measured on 41d799797 unless marked)
face-code	decide::decide	The whole design §6.1 table as one scalar fn: face_code(9 bools) -> u8 (0 native, 1 terminal, 2 exit).	Measured with a two-bool probe `probe_face_code(a: bool, b: bool) -> u8` citing a Clean def: `citation … failed the strict Clean statement/certification audit: the postcondition goal could not be built (the elaborated goal does not typecheck (TypeMismatch { expected: Bool, inferred: UInt8, … }))`. When every parameter is `bool`, the goal builder types `result` as `Bool` (aterm-render/SPECS.md reports the same on 150efe4a). Worked around with two bool cores (§1).
decide-wrapper	decide::decide	decide(i) = Native ⇔ native_face(i), Exit(EXIT_USAGE, NO_TERMINAL) ⇔ no_terminal_exit(i), Terminal otherwise.	Gap 1: `Inputs` (a struct with an enum field) and `Face` (an enum carrying a `String`) get no E6 import. The wrapper is three calls; the table test checks it over all 512 inputs.
after-handoff	decide::after_handoff, client::handoff_of	Refused, Unreachable → Terminal; TimedOut → Exit(1, NO_ANSWER); Opened → Native; and the face agrees with `EditErr::falls_back_to_terminal`.	Gap 1 (enum in, enum with a `String` out); the second half is a property of aterm-ctl.
edit-files	client::edit_files	Files are opened one after another, each waited before the next unless --no-wait; on success zero bytes reach the caller's terminal; the run's status is the first nonzero wait_status.	Gap 1 and trait objects (`&mut dyn Window`, `&mut dyn Write`), a loop, `Path`/`String`. The lane row: `assertion: [trust-absent-callee-assumption] call to absent callee `amacs_term::client::target_uri` (body not in the lowered bundle) may panic` (failed).
command-line	client::parse_args, parse_position	The flag grammar, `+LINE[:COL]` positive decimal for the next file only, `--`, and the usage errors.	Gap 1 (`&[OsString]`, `String`, `Result<Command, String>`, `Option<(u32, Option<u32>)>`) and loops over iterators.
for-word	client::for_word	for_word(p) is the name's Word bytes, at most 32, `None` exactly when none.	Gap 1 (`Option<&Path>`, `String`, an iterator chain); its byte class is `for-word-byte` (§1).
target-uri	client::target_uri	The URI decodes back to the realpath (a missing file: its directory's realpath plus its name).	Filesystem effects (`canonicalize`), and the codec is amacs's. Lane rows: hardened_path_identity unknown ("canonicalization is not a filesystem identity proof").
wait-status-zero	decide::wait_status	wait_status(e) = 0 → e = FINISHED ∨ e = CLOSED, as a theorem of its own over the import.	Attempted; the island was rejected each time, so it was dropped (the table theorem holds, and the property is read off it): `rw [if_neg …] at h; exact absurd h h1'` → `InTactic { tactic: "exact", … TacticFailed(ProofNotProduced) }`; `revert h; decide` → `InTactic { tactic: "decide", … ProofNotProduced }`; focus dots (`·`, `.`) do not lex or parse in an island.
sigint-130	(none)	C-c in the caller ends the client with status 130 and leaves the view open.	Not a property of code here: the client installs no SIGINT handler, so the shell reports SIGINT's default action as 128 + 2. Nothing to contract.
```

## 3. Test → spec map (all 8)

No test is deleted. Each one also reaches code the proofs do not (a wrapper over an enum or a
struct, a trait object, or another crate's parser), so each stays until that code is provable.

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
```

## 4. The Trust lane

`targo --unverified ship trust run -p amacs-term` (`targo trust check -p amacs-term --lib
--allow-l0-gaps` under the lane's prover and memory guard), on this host (128 GiB RAM, verifier
ceiling 16.0 GiB):

| tree | proved / total | floor |
|---|---|---|
| origin/main `3eda2d3d76`, before this slice | 21 / 107 | none (`NO-FLOOR`) |
| this slice (`--update-ratchet`) | 55 / 140 | `amacs-term 55 140` recorded in `crates/aterm-release/trust/floors.tsv` |
| this slice merged with origin/main `82aee5d2b1`, then `b465f1f2ba` (`--update-ratchet` on each) | 55 / 140 | `open`: the floor is met and unchanged; only the amacs-term cost row is refreshed |

The 34 new proved rows are the four contracts' postconditions (11 + 11 + 2 + 10), all
`clean-kernel-citation`, `Certified`. The lane's run printed no memory census, so `memory.tsv`
holds a hand-added row in the no-need shape (`amacs-term 16.0 - - - - -`), as amacs-files's does:
it records only the 16.0 GiB verifier ceiling the floor was set at (`HOST: 128 GiB RAM, verifier
ceiling 16.0 GiB per library`). The lane reads that row and keeps it: the `--update-ratchet` run on
the merged tree left `memory.tsv` byte for byte as it was. With no need recorded (`-`), the row
never refuses a smaller host up front (`trust_facts::memory_refusal`).

What is not proved, all in this crate's own code (85 rows on the final tree), by kind, with the
solver's reason:

| kind | status | rows | reason (the lane's text, shortened) | real hazard? |
|---|---|---|---|---|
| `unsupported_mir` | unknown | 32 | "allocation(s) … rely on `GlobalAlloc::{alloc,dealloc}` never panicking; DISCHARGED vacuously: no `#[global_allocator]` is registered" | no: the prover's allocator assumption, one per allocating body |
| `assertion` | runtime_checked | 35 | "[trust-absent-callee-assumption] call to absent callee … (body not in the lowered bundle) may panic" (std, aterm-ctl, aterm-uds, derived `Debug`/`Clone`/`PartialEq` through `dyn fmt::Write` and `Vec`), "[trust-extern-call-assumption] extern print/format/write dispatch `alloc::fmt::format`", "bundled std member `std::path::has_physical_root` carries an internal Assert", and "panic freedom: no assertion, `unreachable!`, or panic is reachable" left runtime-checked | no: callee bodies the prover does not lower |
| `assertion` | failed | 2 | `client::say`: "call to absent callee `<dyn std::io::Write as std::io::Write>::write_fmt` (dynamic dispatch …)"; `client::edit_files`: "call to absent callee `amacs_term::client::target_uri` (body not in the lowered bundle) may panic" | no: unproved, not refuted (no counterexample); `say` writes through `&mut dyn Write` by design (a test's sink, or stderr) |
| `hardened_ffi_boundary` | unknown 7, design_requirement 1 | 8 | "FFI boundary violation in `deref`/`into_iter`/`next`/`from`/`<__trust_try_total>`: unmodeled FFI call: no summary" | no: `OsString`/`Path` plumbing |
| `hardened_error_discard` | unknown | 3 | `parse_position`'s `parse::<u32>().ok()` (a bad number is a usage error, said by the caller), `parse_args`'s `col.map(…).unwrap_or_default()` (no column is the empty suffix, not an error), `parent_program`'s `i32::try_from(pid).ok()` (a parent pid that does not fit a `pid_t` names no parent, so no `for=`) | no: each `None` is the intended meaning |
| `hardened_path_identity` | unknown | 2 | `std::fs::canonicalize`: "canonicalization is not a filesystem identity proof" | no: the realpath is the name the window is handed (design §6.1); the window opens the file through its own admission (amacs-files' host) |
| `hardened_process_semantics` | unknown | 2 | `std::io::stdout()` and `aterm_uds::stdout::answer_exit`: "stdout pipe behavior depends on SIGPIPE/startup semantics" | no: `answer_exit` treats a reader that went away (EPIPE) as the answer written, and says any other write error |
| `hardened_byte_loss` | unknown | 1 | `parse_args`'s `arg.to_string_lossy()` | no: the lossy text only matches flags; a file is named by the original `OsString` |

Two baseline rows marked real, if small, hazards, and the slice removed them:

- `client::for_word` read the parent's file name through `to_string_lossy` (`hardened_byte_loss`).
  It now reads the name's own bytes (`OsStr::as_encoded_bytes`) and keeps the `for-word-byte`
  class. The word is the same for every name: lossy decoding keeps every ASCII byte and turns
  everything else into non-ASCII characters, which the class drops (a test covers a name that is
  not UTF-8).
- `run_client` read `ATERM_PARENT_SESSION_ID` twice: once to decide the face (`var_os`, set and
  nonempty) and again as the session to hand to (`std::env::var(…).unwrap_or_default()`,
  `hardened_error_discard`). A value that was not Unicode passed the first read and became an
  empty session id in the second. Now it is read once, and a value that is not Unicode names no
  session, so the run takes the terminal face. No aterm writes such a value, so every valid input
  behaves as before.

No row of this crate is an overflow, an out-of-range index or an `unwrap` on input: there is no
arithmetic or indexing on input-derived data in it.

Re-measure (one `targo` at a time per checkout):

```sh
targo --unverified ship trust run -p amacs-term --obligations     # per-row record: target-trust.noindex/logs/amacs-term@*.log.oblig.tsv
targo --unverified ship trust run -p amacs-term --update-ratchet  # raise the floor
```

The per-function report (`report.json`, with each row's `solver` and `assurance`) is kept by
`~/toolchains/trust-current/bin/targo trust check -p amacs-term --lib --allow-l0-gaps
--report-dir <dir>`: the lane deletes its own copy once it has read it. Use the lane's prover
binary: a bare `targo trust check` from the atpkg store is a different build, whose trustd
refuses to share the lane's memory socket.
