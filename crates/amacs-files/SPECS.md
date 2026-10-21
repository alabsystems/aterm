<!-- Copyright 2026 Andrew Yates -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# amacs-files: specs proved through Trust/Clean, and what blocks the rest

> Status 2026-10-20 (amacs slices TR3 and F2, merged). **5 specs are proved**, each a
> `trust_contract_fn!` postcondition that cites a Clean theorem over the kernel import of the
> compiled body (`trust_import_amacs_files__<module>__<fn>`), the recipe of
> `docs/PLAN-tests-to-specs-2026-10-15.md` ("The recipe that works"). Four are TR3's. F2
> (admission options, BOM and CRLF kept, `open_path`) added `is_bare_line_feed`, and its
> `decode_utf8` returns the held mapping through TR3's slice pattern and `is_utf8_bom`. Each spec
> has a negative control (§1).
>
> The lane measured the crate at **132/662** before either slice. Apart, TR3 measured
> **153/676** on its final tree (merged with main `03aebc88f9`), and F2 **158/748** on its
> final tree (composed onto main `4a3c4ecfd0`). On the tree that merges both (F2 merged with main
> `6214986664`), `targo --unverified ship trust run -p amacs-files --update-ratchet --obligations`,
> prover `targo 1.99.0-dev (41d799797 2026-10-16) (targo 0.10.0+g41d799797)`
> (`~/toolchains/trust-41d79979`), measured **171/753** and wrote that `floors.tsv` row.
> That is 13 more proved than F2's tree alone, which had neither TR3's three other contracts nor
> its safety fixes (§2); no row was attributed one by one. The failed rows are four, all of them
> named in §3 (`AtomicCommitResult`'s derived `eq`, the two symlink walks,
> `JournalDocumentKey::keyed`), and the log has no `withheld at publication` row.
> Every postcondition row of the five functions reads
> `PROVED (clean-kernel-citation, CONSTRUCTIVE (certified; validation=kernel-rechecked, closure=empty, coverage=unbounded, execution=unmonitored, reflection=unrecorded))`.
>
> TR3's own run: the contracts and safety fixes first measured 153/674; the two rows more on its
> final tree are `DraftJournalReducer::plan_record` and `SaveReducer::begin`, code TR3 did not
> touch, whose one `native-verification-gap` row each became two runtime-checked assertions (an
> absent callee, and std's `Layout::array::inner`). Of its +21 proved: 16 are the contracts'
> postconditions. One is `bounded_read`'s allocation, which failed before. Four are the bounds
> checks of `decode_utf8`'s slice pattern, which replaced a failed `drain` (§2). That prover
> publishes them, and its log has no `withheld at publication` row.
>
> No test was deleted. No existing test sampled one of the five predicates alone. TR3 added three
> tests and F2 fifteen, for the code around them, which the proofs do not cover (§4).

The contract macros are aterm-log's two `trust_contract_fn!` arms and two `verify_island!` arms,
defined once in `src/lib.rs` ahead of the modules. Both key on `cfg(trust_verify)`: the theorems
name `trust_import_*` constants, which exist only in a verifying session. Keying the islands on
`cfg(clean_islands)` instead, mounted from a build script as aterm-bidi does, was measured. It
breaks every `targo --unverified` build with four `AutoBoundHeadOverApplied` rejections ("this
island names the kernel-import namespace, which is populated only by an enabled verification
walk; it cannot elaborate under -Ztrust-verify=off"). So the crate has no build script, and
nothing in it reads `cfg(clean_islands)`. Stock rustc gets plain fns; the `linux-arm` cell lints
and type-checks the crate (§5).

The clauses and theorems spell the 32 MiB limits as the literal 33554432, while the bodies
compare against `DOCUMENT_LIMIT_LEN` and `MAX_INSERT_LEN`, which derive from
`DEFAULT_DOCUMENT_LIMIT` and `MAX_INSERT_BYTES`. No landing check runs the lane, so a const
assertion beside each constant (`const _: () = assert!(DOCUMENT_LIMIT_LEN == 33_554_432 && …)`)
makes an ordinary build fail the moment a constant leaves its spec. Measured: setting both
constants to 16 MiB fails `targo --unverified check -p amacs-files` with two `E0080` "evaluation
panicked: assertion failed" errors. The const items add no lane rows.

Clean parses `a && b && c` as `a && (b && c)`, which is the nesting the E6 reader gives the
compiled body, but the clause parser nests it to the left. A clause over three or more
conjuncts therefore spells the right nesting with parentheses, so the theorem's statement and
the clause's denotation are the same term. Unparenthesized, the audit refused the citation with
a `TypeMismatch` (expected `Bool.and (Bool.and A B) C`, inferred `Bool.and A (Bool.and B C)`).

Clean renders `a != b` in a `bool` body as `!decide (a = b)` (`Bool.not (Decidable.decide …)` in
the goal), and `is_bare_line_feed`'s clause states it as `previous != 13`.

## 1. Proved specs (TSV)

Columns: `id`, `function`, `statement`, `clause`, `theorem`, `verdict` (the lane's row on the
final tree), `negative control` (the body mutation, then what the lane printed). Each control ran
`targo --unverified ship trust run -p amacs-files` on the mutated tree of the slice that wrote the
spec. The file was then restored from a copy saved beforehand, and `cmp` confirmed it was byte for
byte the same. A rejected island taints the crate's Clean environment. So during each control
every citation in the crate failed, not just the mutated one ("cannot be validated because an
earlier Clean island was rejected and the crate-local environment is tainted"). On TR3's tree the
lane total fell to 148 (of 670 or 671). `bare-line-feed`'s control ran on F2's tree before it
merged a newer main, where the unmutated tree measured 153/715 and carried only the
`is_bare_line_feed` and `is_utf8_bom` citations, so its total fell to 150 (of 715, or 714 when the
mutation changed the count).

```tsv
id	function	statement	clause	theorem	verdict	negative control
document-limit-threshold	host::exceeds_document_limit(len: u64) -> bool	∀ len: exceeds_document_limit(len) ⇔ len > 32 MiB = 33554432 (DEFAULT_DOCUMENT_LIMIT). The save preflight (commit_atomic_bytes_with_seed) refuses exactly these buffers before it takes the write lock.	ensures result == (len > 33554432)	exceeds_document_limit_is_threshold: import len = decide (33554432 < len), by simp only [import]	[VERIFIED] (2 obligations: 2 proved); postcondition PROVED (clean-kernel-citation, CONSTRUCTIVE (certified; validation=kernel-rechecked))	`len > DOCUMENT_LIMIT_LEN` → `>=`: "Clean island declaration rejected: InTactic { tactic: "simp", … UnsolvedGoals … ⊢ Decidable.decide (UInt64.ofNat 33554432 ≤ fvar#0) … = decide (Nat.le (Nat.succ …33554432…) …)"; fn row [VIOLATIONS] (2 obligations: 0 proved, 0 runtime-checked, 1 failed, 1 unknown); lane 148/671
utf8-bom-is-ef-bb-bf	host::is_utf8_bom(b0: u8, b1: u8, b2: u8) -> bool	∀ b0 b1 b2: is_utf8_bom ⇔ (b0, b1, b2) = (0xEF, 0xBB, 0xBF), U+FEFF in UTF-8. decode_utf8 takes off exactly that leading mark, and the held encoding remembers it for the save.	ensures result == (b0 == 239 && (b1 == 187 && b2 == 191))	is_utf8_bom_is_ef_bb_bf: import b0 b1 b2 = (decide (b0 = 239) && (decide (b1 = 187) && decide (b2 = 191))), by simp only [import]	[VERIFIED] (4 obligations: 4 proved); postcondition PROVED (clean-kernel-citation, CONSTRUCTIVE (certified; validation=kernel-rechecked))	`b2 == 0xbf` → `0xbe`: "Clean island declaration rejected: InTactic { tactic: "simp", … UnsolvedGoals … ⊢ Bool.and (… fvar#0 = UInt8.ofNat 239 …) (Bool.and (… 187 …) (Decidable.decide (fvar#2 = UInt8.ofNat 190) …)) = and (…239…) (and (…187…) (decide (fvar#2 = UInt8.ofNat 191) …))"; fn row [VIOLATIONS] (4 obligations: 2 proved, 0 runtime-checked, 1 failed, 1 unknown); lane 148/671; on F2's tree the same mutation drew the same rejection and lane 150/714
lower-hex-digit-class	io::is_lower_hex_digit(byte: u8) -> bool	∀ byte: is_lower_hex_digit ⇔ byte ∈ 48..=57 ∪ 97..=102 (`0`-`9`, lowercase `a`-`f`). parse_hex16 (the journal owner id and its lease name) admits only these bytes, so an id has one spelling.	ensures result == ((byte >= 48 && byte <= 57) || (byte >= 97 && byte <= 102))	is_lower_hex_digit_is_class: import byte = ((decide (48 <= byte) && decide (byte <= 57)) || (decide (97 <= byte) && decide (byte <= 102))), by simp only [import]	[VERIFIED] (6 obligations: 6 proved); postcondition PROVED (clean-kernel-citation, CONSTRUCTIVE (certified; validation=kernel-rechecked))	`byte <= b'f'` → `b'g'`: "Clean island declaration rejected: InTactic { tactic: "simp", … UnsolvedGoals … ⊢ Bool.or (…) (Bool.and (… 97 ≤ fvar#0 …) (Decidable.decide (fvar#0 ≤ UInt8.ofNat 103) …)) = or (…) (and (…) (decide (Nat.le … (UInt8.ofNat 102) …)))"; fn row [VIOLATIONS] (5 obligations: 4 proved, 0 runtime-checked, 1 failed, 0 unknown); lane 148/670
journal-edit-order	io::journal_edit_in_order(start: u64, end: u64, previous_end: u64, insert_len: u64) -> bool	∀ args: journal_edit_in_order ⇔ start ≤ end ∧ previous_end ≤ start ∧ insert_len ≤ 32 MiB (MAX_INSERT_BYTES). validate_edits_shape applies it to each edit, with the previous edit's end (0 before the first). So a record's edits are forward, sorted and disjoint, which replay's back-to-front application relies on.	ensures result == (start <= end && (previous_end <= start && insert_len <= 33554432))	journal_edit_in_order_is_shape: import start end_ previous_end insert_len = (decide (start <= end_) && (decide (previous_end <= start) && decide (insert_len <= 33554432))), by simp only [import]	[VERIFIED] (4 obligations: 4 proved); postcondition PROVED (clean-kernel-citation, CONSTRUCTIVE (certified; validation=kernel-rechecked))	`previous_end <= start` → `<`: "Clean island declaration rejected: InTactic { tactic: "simp", … UnsolvedGoals … ⊢ Bool.and (…) (Bool.and (Decidable.decide (fvar#2 < fvar#0) (instDecidableUInt64Lt …)) (…)) = and (…) (and (decide (Nat.le … fvar#2 … fvar#0 …) …) (…))"; fn row [VIOLATIONS] (3 obligations: 2 proved, 0 runtime-checked, 1 failed, 0 unknown); lane 148/671
bare-line-feed	io::is_bare_line_feed(previous: u8, byte: u8) -> bool	∀ previous byte: is_bare_line_feed ⇔ byte = LF (10) ∧ previous ≠ CR (13). line_ending_of holds a file as CRLF only when it has a line feed and this is false at every byte (previous = 0 before the first), so one bare LF keeps a mixed file byte for byte.	ensures result == (byte == 10 && previous != 13)	is_bare_line_feed_is_lf_after_no_cr: import previous byte = (decide (byte = 10) && !decide (previous = 13)), by simp only [import]	[VERIFIED] (3 obligations: 3 proved); postcondition PROVED (clean-kernel-citation, CONSTRUCTIVE (certified; validation=kernel-rechecked))	`previous != b'\r'` → `b'\n'`: "Clean island declaration rejected: InTactic { tactic: "simp", … UnsolvedGoals … ⊢ Bool.and (Decidable.decide (fvar#1 = UInt8.ofNat 10) …) (Bool.not (Decidable.decide (fvar#0 = UInt8.ofNat 10) …)) = and (decide (fvar#1 = UInt8.ofNat 10) …) (Bool.not (decide (fvar#0 = UInt8.ofNat 13) …))"; fn row [VIOLATIONS] (3 obligations: 1 proved, 0 runtime-checked, 1 failed, 1 unknown); lane 150/715
```

The size and offset predicates take `u64`, and their callers convert with `as u64`. E6 imports
only `bool`, `uN` and `iN` carriers, and it refuses a body that holds a cast or a call
(`docs/AUDIT-trust-for-test-migration-2026-10-15.md` §3.3). For the same reason,
`is_lower_hex_digit` compares instead of calling `RangeInclusive::contains`, and says so in an
`#[expect(clippy::manual_range_contains)]`.

## 2. Safety obligations TR3 made provable (TSV)

The baseline is the lane's `--obligations` record at main `3eda2d3d76`. The after column is the
same record on this tree. Behaviour is unchanged for every input the code accepted before,
except at two extremes. A read no longer reserves more than 32 MiB + 1 up front, whatever the
caller's limit and the file's claimed length (row 1). A lock budget the clock cannot add to now
is no ceiling, where it used to panic (row 5). F2 kept every form here; its `decode_utf8` returns
`Decoded` through the same slice pattern.

```tsv
site	before (lane text)	change	after
host::bounded_read	unbounded_allocation FAILED: "unbounded allocation in `Vec::with_capacity` (count `_7`): bulk allocation may reach or exceed 268435456 elements"	The reservation is also capped at READ_RESERVE_CEILING (32 MiB + 1): a caller's limit or a file's claimed length could otherwise ask for any size up front. A longer read still grows the buffer as bytes arrive.	unbounded_allocation PROVED
host::decode_utf8	unsupported_mir FAILED: "`Call::drain::panic-freedom-unverified`: bb3: `drain` panics on an out-of-range argument (`> len`) and its panic-freedom is not modeled"	A slice pattern `[b0, b1, b2, text @ ..]` plus the proved `is_utf8_bom`; the BOM arm decodes `text` into a new String instead of draining 3 bytes. Cost, accepted: an n-byte file with a mark briefly holds 2n - 3 bytes (64 MiB at the limit), no more than the engine's rope build (`TextRope::from(&str)`) then holds beside the admitted String; a file without a mark decodes in place. In-place forms measured on one lane run: `Vec::drain(..3)` under the slice pattern, and `drain(..len().min(3))`, FAILED `Call::drain::panic-freedom-unverified`; `copy_within(3.., 0)` FAILED `Call::copy_within::panic-freedom-unverified`; decoding first and then `String::drain(..3)` FAILED "`string-range-char-boundary-unmodeled` … refused unless every explicit endpoint is `0`"; `VecDeque::from(bytes)` then `drain(..3)` FAILED `Call::drain::panic-freedom-unverified`. Only `retain` with a saturating counter left no refused row, but it is a per-byte closure pass a reader has to decode, so it was not taken.	drain gone; 4 slice_bounds_check PROVED
io::Reader::u8	slice_bounds_check FAILED and hardened_panic_boundary FAILED ("MIR bounds-check assert can panic")	`self.bytes(1)?.first().copied().ok_or(JournalError::Truncated)`	no index left; both rows gone
io::decode_record	3 slice_bounds_check UNKNOWN (the checksum, checked-prefix and payload slices)	`get(..)` with `JournalError::Truncated` (unreachable after the length checks, which stay)	rows gone
host::acquire_advisory_lock_within	unsupported_mir UNKNOWN `time-point-duration-overflow` ("Instant/SystemTime + or - a Duration panics when the result is not representable") and `duration-arith-overflow-unmodeled` (`backoff * 2`)	`Instant::now().checked_add(budget)`, where a budget the clock cannot hold is no ceiling; `backoff.saturating_mul(2)`. This is a pub fn, and `budget = Duration::MAX` used to panic. `a_lock_budget_past_the_clock_waits_without_a_ceiling` holds both arms; with the old `Instant::now() + budget` it fails with "overflow when adding duration to instant".	rows gone
host::commit_atomic_bytes_with_seed	unsupported_mir UNKNOWN `duration-arith-overflow-unmodeled` (`backoff * 2`)	`backoff.saturating_mul(2)` (identical: the backoff is capped at 8 ms)	row gone
io::parse_hex16	closure runtime_checked: absent callee `RangeInclusive::contains`	the byte class is the proved `is_lower_hex_digit`	row gone
```

## 3. Not provable today: the worklist (TSV)

Each row is an important property the crate does not prove. The blocker is the lane's exact text
where it reported one on this crate (prover 41d799797, on the tree of the slice that wrote the row),
or else the gap of `docs/PLAN-tests-to-specs-2026-10-15.md` ("What blocks the rest"). In that
second case no contract was attempted here, and the row says so. The rows from `A held text
encodes back` on are F2's.

```tsv
property	functions	blocker	held today by
A watch never replaces a dirty buffer, and never during a save; same bytes only rebind the baseline	io::reduce_file_watch, io::detect_version_conflict, content_is_equivalent	Gap 1: ObservedFileVersion/FileWatchInput structs in, an enum with a payload out (no E6 import, no clause type). Not attempted; a u8 core plus an enum wrapper would contort a clear reducer, and the PLAN notes that a wrapper is not certified anyway.	file_watch_reducer_reloads_only_clean_idle_documents_and_conforms (Tier-1 bind to native_file_watch_model)
A save is durable only when every step of its proof matches the plan (synced, renamed, directory synced, same len and fingerprint)	io::SaveReducer::complete, io::DraftJournalReducer::complete	Gap 1/2: `&mut self` and struct/enum carriers. Not attempted.	incomplete_or_mismatched_atomic_proof_fails_closed, journal_durability_requires_matching_synced_append_proof
Journal codec: decode(encode(r)) = r; corruption anywhere rejects the whole image	io::encode_journal, io::decode_journal, decode_record, decode_edits	Gap 1 (`&[u8]`, Vec, JournalRecord) and loops (E6 admits none). encode_journal_record's header-length check: assertion UNKNOWN "panic call: core::panicking::assert_failed::<usize, usize>" (a Vec's length is not followed through `extend_from_slice`).	record_round_trip_preserves_snapshot_and_unicode_delta, checksum_corruption_rejects_the_whole_journal_not_a_valid_prefix, truncated_and_unknown_version_records_are_rejected
Replay applies a record's edits only at char boundaries, back to front	io::apply_journal_edits	unsupported_mir UNKNOWN: "`string-range-char-boundary-unmodeled`: String::drain/replace_range/extend_from_within panics when the range is out of order or out of bounds, or when either end is not a char boundary; char boundaries are not modeled"	recovery_rejects_wrong_document_sequence_gap_and_utf8_split
A whole record's edits are sorted and disjoint (the loop around journal-edit-order)	io::validate_edits_shape	Gap 1 (`&[JournalEdit]`) and loops	a_delta_takes_sorted_disjoint_forward_edits_only (added in TR3)
Admission takes off one leading BOM and still refuses invalid UTF-8	host::decode_utf8	Gap 1 (Vec<u8> in, `Decoded` out); its hardened_utf8_reject row is UNKNOWN ("strict UTF-8 conversion can reject valid Unix filenames or byte streams"), which is the intended rule: amacs is UTF-8 only	decoding_takes_off_one_leading_bom_and_still_checks_the_rest (added in TR3)
A journal key is a MAC under the install secret and is never 0	io::JournalDocumentKey::keyed	assertion FAILED "Call::expect::panic-freedom" and hardened_panic_boundary UNKNOWN on `HmacSha256::new_from_slice(..).expect(..)`; the constructor never errs, but its body is outside this crate's bundle ("[trust-absent-callee-assumption] call to absent callee `aterm_digest::hmac_sha256::HmacSha256::new_from_slice`"). Needs an infallible constructor in aterm-digest or callee-ensures composition.	journal tests in aterm-gui (native_document_journal)
Symlink components are refused, and config symlinks are bound and revalidated	host::reject_symlink_components, host::logical_symlink_bindings, host::walk_path_components	unsupported_mir FAILED: "`instance-hypothesis:discharge`: instance discharge: site `<F as core::ops::function::FnMut<(&std::path::Path, &std::fs::Metadata)>>::call_mut` of `amacs_files::host::walk_path_components::<{closure…}>` … resolves to `<{closure…} as FnMut<…>>::call_mut`"; plus Gap 8 (the walk is filesystem effects)	config_symlink_chain_is_bound_and_preserved_while_ordinary_grants_fail_closed, final_symlink_swap_is_a_conflict_and_never_redirects_the_save
The atomic save protocol: same-directory temporary, file sync, rename, directory sync, final observation, under the advisory lock	host::commit_atomic_bytes_with_seed, replace_file, sync_directory, create_unique_temporary, open_write_lock, open_regular_read, canonical_target_path, canonicalize_allow_missing, normalize_process_temp_root	Gap 8 (effects). The lane's hardened boundaries, all UNKNOWN: raw_path_api ("rename is name-based; hardened code needs source and destination direntry identity contracts", "path removal re-resolves a mutable direntry", "OpenOptions::open on a path needs explicit no-follow/create/mode and trust-domain contracts"), path_identity ("canonicalization is not a filesystem identity proof"), permission_change, permission_window, byte_loss (`to_string_lossy` in a temporary's or lock's name), ffi_boundary ("unmodeled FFI call: no summary") in canonicalize_allow_missing	the host tests (save, lock, FIFO, symlink-swap, temporary-collision)
Errors as values: a failure message reaches the reducer unchanged	host::failed, host::atomic_failed, host::atomic_published_unverified (`impl Into<String>`)	unverified assumption: "[trust-instance-hypothesis] call to `<impl Into<String> as core::convert::Into<alloc::string::String>>::into` … the panic-freedom of this open generic dispatch is assumed for every instantiation"; each instance site UNKNOWN `instance-hypothesis:discharge`	-
Derived equality of save outcomes	<host::AtomicCommitResult as PartialEq>::eq	unreachable FAILED: "unreachable code reached" in the derived body	-
The observation and reservation clamps (a read never reserves past the document limit, an observation never reads past it)	host::observation_limit, host::bounded_read	E6: the bodies cast (`usize::try_from`, `as`) and call `min`; the clamps' allocation obligation is solver-proved (§2) but not contracted	save_preflight_bounds_external_growth_and_rejects_oversized_desired_bytes
A held text encodes back to its file's bytes exactly; a file is held as CRLF exactly when it has a line feed and none is bare, and then one CR per line feed comes off; a text saved as CRLF reads back as itself	io::Decoded::hold, io::line_ending_of, io::encode_for_disk, host::decode_utf8	Gap 1 (String, &str, Vec<u8> in; Decoded, an enum, out) and loops (E6 admits none). hold's row: assertion runtime_checked "[trust-absent-callee-assumption] call to absent callee `alloc::str::<impl str>::replace::<&str>` (body not in the lowered bundle) may panic". decode_utf8's own rows are on the BOM row above.	io::tests::held_text_encodes_back_to_the_files_bytes (all 9841 strings of up to 8 of a, CR, LF, with and without a BOM); host::tests::bom_round_trips_byte_exact, crlf_file_round_trips_with_lf_in_buffer, mixed_eol_is_byte_exact
A save encodes with the mapping its buffer was read with, which a save's own proof, or an observation of the same bytes read either way while the buffer is left as it is, never replaces; a reload takes the fresh read's mapping unless its text is the old reading	io::SaveReducer::begin, io::SaveReducer::complete, host::DocumentPersistenceStore::accept_observation, host::DocumentPersistenceStore::accept_reloaded_observation	Gap 1/2: `&mut self` and struct carriers (SavePlan, ObservedFileVersion). Not attempted.	io::tests::a_save_plan_encodes_the_text_with_the_mapping_it_was_read_with; host::tests::a_document_keeps_the_mapping_it_was_read_with_across_saves, a_save_read_back_another_way_is_unchanged, a_reload_takes_the_fresh_reading_and_its_mapping
A version's text matches a check exactly when it is the file's bytes as the document holds them or as a fresh read admits them, so a buffer's own save read back is no change; the journal fingerprint is the fresh reading's	host::fresh_reading, host::ObservedDocumentImage::holds, host::DocumentPersistenceStore::observed_text_matches, host::DocumentPersistenceStore::last_observed_text_matches, host::DocumentPersistenceStore::observed_text_fingerprint	Gap 1 (&[u8], &str, Arc<[u8]> in; an Option of one out) and Gap 2 (struct carriers); fresh_reading calls encode_for_disk and decode_utf8, whose rows are above. Not attempted. fresh_reading's two `.ok()` rows are hardened_error_discard UNKNOWN ("Result::ok discards the error channel"): both errors are unreachable (the text was a String, and its encoding is UTF-8), and `None` there is the safe answer, one reading. rebind takes an enum, not an `impl Fn`: a closure parameter's call was an absent-callee assertion the lane counts as failed.	host::tests::a_save_read_back_another_way_is_unchanged (a new CRLF-only buffer, a mixed one edited to CRLF only, a leading U+FEFF), the_observed_text_fingerprint_is_the_admitted_texts
A missing file admits empty and nothing exists until the first save, which refuses if the file appeared; a missing directory is refused, and a document's save never makes one	host::admit_local_file_opts, host::require_existing_directory, host::commit_atomic_bytes_with_seed, host::lock_and_verify_target, host::replace_file	Gap 8 (filesystem effects). The lane's rows here are absent-callee assumptions ("call to absent callee `amacs_files::host::read_atomic_file_with_config_symlinks` (body not in the lowered bundle) may panic").	host::tests::missing_with_allow_missing_is_empty_and_first_save_creates, first_save_refuses_if_the_file_appeared, missing_without_allow_missing_is_refused, a_missing_document_refreshes_as_missing_until_its_file_appears, a_document_save_makes_no_folder
open_path names an existing file by its realpath, a missing one by its directory's realpath plus its name, and refuses a dangling final link	host::open_path	hardened_path_identity UNKNOWN, twice: "canonicalization is not a filesystem identity proof; compare stable file identity instead"; plus Gap 8	host::tests::open_path_resolves_a_symlinked_parent
Whole-crate rows the lane leaves open	every fn	On the merged tree: 230 unsupported_mir UNKNOWN "allocation(s) … rely on `GlobalAlloc::{alloc,dealloc}` never panicking; DISCHARGED vacuously…"; 221 assertion runtime_checked "[trust-absent-callee-assumption] / [trust-extern-call-assumption]"; 32 "panic freedom: no assertion, `unreachable!`, or panic is reachable" summaries; 10 "native-verification-gap" (19 on TR3's run before its merge with main 03aebc88f9, where the prover then named an absent callee behind nine of them); 11 hardened_error_discard on deliberate `.ok()`s (modified_ns, atomic_conflict, commit_atomic_bytes_with_seed, parse_hex16, fresh_reading)	-
```

## 4. Tests

- **Deleted:** none. The five predicates are new extractions, and no existing test sampled one alone.
- **Added by TR3** (they hold the wrappers and the safety changes, which the proofs do not reach):
  - `host::tests::decoding_takes_off_one_leading_bom_and_still_checks_the_rest`: one leading mark
    comes off, a second stays as U+FEFF, a trailing one stays, a short `EF BB` and invalid bytes
    after the mark are `InvalidUtf8`. Since the merge with F2 it reads the held text of the
    `Decoded` that `decode_utf8` returns.
  - `io::tests::a_delta_takes_sorted_disjoint_forward_edits_only`: the first edit may start
    anywhere, touching edits pass, and out-of-order, overlapping and backward ranges are
    `InvalidEdit`.
  - `host::tests::a_lock_budget_past_the_clock_waits_without_a_ceiling`: `Duration::MAX` takes an
    uncontended lock at once, and a waiter on a held lock takes it once the holder lets go, under
    a one-minute hang detector.
- **Added by F2**, all for the code around the predicates: the thirteen `host::tests` and two
  `io::tests` named in F2's rows of §3. Four came with F2's review fixes. Three were run against a
  mutant, and each mutant failed exactly its test: comparing texts instead of file bytes
  (`a_save_read_back_another_way_is_unchanged`), `accept_reloaded_observation` keeping the mapping
  for either reading (`a_reload_takes_the_fresh_reading_and_its_mapping`), and a document save
  that makes its directory (`a_document_save_makes_no_folder`). The fourth,
  `the_observed_text_fingerprint_is_the_admitted_texts`, had no mutant run.
- `targo --unverified test -p amacs-files` on the merged tree: 59 unit tests pass (41 before
  either slice, 3 from TR3, 15 from F2).

## 5. Re-measuring

- Proofs and floor: `targo --unverified ship trust run -p amacs-files --obligations` (the
  per-obligation record lands beside the log under `target-trust.noindex/logs/`), then `--update-ratchet`
  to raise `crates/aterm-release/trust/floors.tsv`.
- Before committing a contract: `targo --unverified verify test -p aterm-contract-probe`.
- Stock rustc: `targo --unverified verify cells --cell linux-arm` (it reads the whole tree and
  ignores `-p`).
