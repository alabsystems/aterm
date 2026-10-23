<!-- Copyright 2026 Andrew Yates -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# aterm-census: spec recovery from its 188 unit tests

> Status 2026-10-16: this is the crate's migration worklist. Each of the 66 specs below is to be
> proved through Trust/Clean about the compiled Rust function, and the tests it subsumes are then
> deleted. Nothing has been proved or deleted yet. The 7 tests in §4 sample no spec; they are to be
> deleted, and none has been deleted yet.
>
> **Re-measured on seal `150efe4a`, 2026-10-16: `census-key-file-name` and
> `census-launcher-source` are still blocked** (§6 item 1 has the errors). Each was written as a
> `trust_contract_fn!` contract citing a Clean theorem over `trust_import_aterm_census__<fn>` for
> every input, and checked with `targo trust check -p aterm-census`. Trust minted no kernel import
> for either `&str` body, could not lower either `ensures`, and refused the citation because `&str`
> has no clause type. The code is unchanged and both tests stay.

> Status 2026-10-17, after the census came back from main into the tests-and-gates audit: the
> censuses, the guards and their tests are main's again, but the 25 tests the audit deleted as weak
> and main never changed stay deleted, and their rows are gone: main.rs's six argv tests, and
> nineteen lazy_init.rs cases (eighteen folded into the one table `every_reentry_shape_is_judged`,
> which has a row, and `the_depth_rule_still_treats_a_real_declaration_as_bodyless`). The rows of
> the retired `watch-hands` rule, its input-verb table and their five tests, and of the six deleted
> `live_tree_*` tests, are gone too; the §3 gap histogram counts the 65 specs left. Counts
> elsewhere in this file were taken before.

Source: `crates/aterm-census` (read only). Every real `#[test]` fn was read, together with the code
it reaches. Nothing was compiled or run; the claims come from reading the code.

| file (under `crates/aterm-census/src/`) | tests |
|---|---|
| guards/doors.rs | 4 |
| guards/graph.rs | 9 |
| guards/installer.rs | 1 |
| guards/keys.rs | 4 |
| guards/lifeline.rs | 5 |
| guards/mktemp.rs | 1 |
| guards/mod.rs | 6 |
| guards/rules.rs | 16 |
| held_guard.rs | 3 |
| lazy_init.rs | 22 |
| lex.rs | 3 |
| lib.rs | 22 |
| lock_order.rs | 47 |
| main.rs | 6 |
| scan_set.rs | 19 |
| scope_census.rs | 10 |
| wasm_census.rs | 10 |
| **total** | **188** |

A plain text count of `#[test]` gives 218, because 31 copies sit inside string fixtures and doc
text (the held_guard MUST_FIRE/MUST_SILENT raw strings, the cfg-predicate list in lib.rs, and the
wasm_census, lifeline and graph fixtures). A test counter should read the crate's own `lex::lex`
code view rather than the raw text (§5.6).

Result: **66 specs**, covering **181 tests**. **7 tests are not spec samples** (§4).

## 0. Notation used in the statements

- `P` = `crates/aterm-census/src/`. A test id is written `P`-relative as `file::fn`, and a function
  as `module::fn` (`lib::` for the crate root).
- `Lines(s)`: the split of `s` that `str::lines` gives (on `\n`, a trailing `\r` dropped).
  Ident (word) bytes are `[A-Za-z0-9_]`. A **word-boundary hit** of needle `n` in `h` at offset `i`:
  `h[i..]` starts with `n`, and neither `h[i-1]` nor `h[i+|n|]` is a word byte (an absent byte counts
  as a boundary). `match_indices` occurrences are non-overlapping, left to right.
- **`LexRef`**: the reference Rust tokenizer restricted to comments (`//` to end of line; nested
  `/* */`), cooked strings (`"…"` with `\` escapes, `b"…"`, `c"…"`), raw strings (`r#*"…"#*`, `br`,
  `cr`) and char/byte-char literals (`'x'`, one multi-byte UTF-8 char, `'\…'` up to 12 bytes).
  Lifetimes, labels and raw identifiers (`r#ident`) are code. An identifier immediately before a
  literal is a prefix only for `r`/`br`/`cr`/`b`/`c`.
- **Views.** `Code(src)` = `src` with every `LexRef` span blanked byte for byte (each byte except
  `\n` becomes `' '`); a string span with no `\n` keeps its two quote bytes. `Text(src)` = `src`
  with only comment spans blanked. Both are byte-aligned with `src` (spec `census-lex-views`).
- **`Mask_g(t)`**: for a gate predicate `g` over trimmed attribute lines, the per-line keep vector
  `lock_order::unmasked_lines(t, g)`. An item is blanked when its gate line `g`-matches. The item runs
  over any further `#[` lines, then the item line(s), and ends at the first of (all read on the
  literal-masked, comment-stripped code): a line ending in `;`; a line ending in `,` at the gate's
  indent; a dedent below the gate (the item ends on the previous line); a line containing `{`. A `{`
  line is single-line (as many `{` as `}`) or is closed by the first later line with `}` at
  column = indent and no `{`. With no terminator or no matching close the lines are restored
  (fail-open).
- **`Shipped(f)`** = the lines kept by `Mask_{cfg_attr_never_ships}(Code(f))`.
- **`EntailsTest(p)`** over the cfg grammar `P ::= test | all(P,*) | any(P,*) | other`:
  `E(test)` = true; `E(all(cs))` = `cs ≠ []` ∧ some `c` has `E(c)`; `E(any(cs))` = `cs ≠ []` ∧ every
  `c` has `E(c)`; otherwise false. Children come from `split_top_level_commas`.
- **Tree `T`**: a finite map from repo-relative path to contents, standing for the checkout at
  `root` (in tests, the synthetic checkout `synth_tree` writes). A census outcome is
  `CensusOutcome {ok, verdict, log, floors}`. A statement about a census is about `ok` and the
  structured content the log carries (`[OB-n]` tags, `EDGE a -> b` lines, `file:line` spans, counts),
  never about repair prose (§5.2).
- **`ScanSet(T, roots)`**: the cargo normal-dependency closure. It walks path and workspace deps from
  the root crates through `[dependencies]` and `[target.'cfg(..)'.dependencies]` (cfg ignored),
  leaving out dev-dependencies, build-dependencies, proc-macro crates and their deps. An optional dep
  is in exactly when some feature activates it: `default` (unless every edge sets
  `default-features = false`, combined by union over edges), edge `features = [..]`, `dep:x`, and
  `crate/feat` forwarding. Inline and one-table manifest forms are both read.
  `scan_dirs` = the sorted `<crate>/src` dirs of the closure.
- **Graphs.** A cycle is an SCC of size ≥ 2 or a self-edge. The **name-merged call graph** has fn
  names as nodes and an edge `f -> g` iff `g(` appears in a body named `f`.
- **Rules.** For a guard rule `R` and a corpus `C` (`Corpus::of` in tests, `Corpus::read(T)` for
  real), `Run(R, C)` is the `Vec<Finding{rule, path, line, detail}>` that `guards::run_rules` returns
  for `[R]`, sorted by `(path, line)`. `spots(fs)` = `[path:line]`. `PRODUCT`, `ENGINE_CRATES` and the
  other upper-case names are the crate's own const tables, quantified as they stand.
- `CENSUSES`: the 6-row table (mainloop, locks, wasm, scope, lazy-init, guards) in declaration order.
- **Gaps.** `blocking_gaps` lists rows of the gap table in
  `AUDIT-trust-for-test-migration-2026-10-15.md` §7 (historical internal audit). Gap 0 (a seal that publishes
  Clean-discharged rows) is a prerequisite of every row, so it is left out of the column. Seal
  `150efe4a`, built locally, closes it off the machine's `trust` channel
  (`PLAN-tests-to-specs-2026-10-15.md` (historical internal plan)). No spec here is blocked by gap 0 alone:
  every one carries `&str`, `String`, `Vec` or slice data (gap 1) and a non-straight-line body (gap 3).

## 1. Specs (TSV)

Columns: `id`, `kind`, `functions`, `statement`, `subsumes` (primary tests; shared ones in §2),
`rust_features` (what the proof must handle), `blocking_gaps` (audit §7 numbers).

```tsv
id	kind	functions	statement	subsumes	rust_features	blocking_gaps
census-lex-spans-refine	refinement	lex::{spans,cooked_string,raw_string,char_literal,block_comment,utf8_len}	∀ src: spans(src) is the ordered, non-overlapping list of the comment, string and char-literal tokens LexRef finds. Each span has start < end ≤ |src|, spans are sorted by start, and kind ∈ {Comment, Str, Char} is LexRef's class. For a Str, quotes = Some((open, close)) ⇔ the literal is terminated, and then src[open] = src[close] = b'"'. An unterminated literal or comment extends to |src|. Every span boundary is a UTF-8 char boundary of src. Lifetimes ('a), labels ('outer:) and raw identifiers (r#type) produce no span.	lex.rs::lifetimes_labels_and_raw_identifiers_are_code	&str/byte slices, Vec<Span>, while loops with index arithmetic, nested match on (u8, Option<&u8>), a nesting counter in place of recursion	1,3,5,7
census-lex-views	contract	lex::{lex,blank,utf8}	∀ src, with L = lex(src): (a) |L.code| = |L.text| = |src|. (b) ∀ i: src[i] = b'\n' ⇔ L.code[i] = b'\n' ⇔ L.text[i] = b'\n'. (c) L.code = Code(src) and L.text = Text(src) (§0): L.code[i] = src[i] when i lies in no span or at a kept quote, else b' '; L.text[i] = b' ' ⇔ i lies in a Comment span ∧ src[i] ≠ b'\n'. (d) Both views are valid UTF-8, so the lossy arm of utf8 is unreachable.	lex.rs::the_code_view_blanks_every_literal_and_comment_in_place, lex.rs::the_text_view_keeps_literals_and_drops_comments_byte_aligned	Vec<u8> mutation through &mut slices, String::from_utf8, for loops over Vec<Span>	1,2,3,5,7
census-blank-non-code	contract	lex::blank_non_code	∀ src: Lines(blank_non_code(src)) = [l.trim_end() | l ∈ Lines(Code(src))], every output line ends in '\n', so the line count is |Lines(src)|. This holds with no trailing newline and with trailing blank lines.	lex.rs::the_code_view_blanks_every_literal_and_comment_in_place	String building, str::lines, trim_end	1,3,5,7
census-cfg-entails	contract	lib::{cfg_attr_never_ships,cfg_pred_entails_unshipped}	∀ attr: cfg_attr_never_ships(attr) ⇔ attr = "#[cfg(" ++ p ++ ")]" for some p with EntailsTest(p.trim()). Soundness: if cfg_attr_never_ships(attr), then p is false under every cfg valuation with test = false (any target_os, unix, windows, debug_assertions or feature values), so a masked item never compiles into a non-test build.	lib.rs::cfg_predicate_entails_unshipped_only_when_it_really_does	&str strip_prefix/strip_suffix, recursion over a string grammar, iterator any/all, Vec<&str> from split	1,3,5,7
census-split-commas	contract	lib::split_top_level_commas	∀ args: let in_str(i) = parity of '"' before i and depth(i) = running '(' minus ')' outside strings, saturating at 0. split_top_level_commas(args) = the pieces from cutting args at every ',' at i with ¬in_str(i) ∧ depth(i) = 0, each trimmed, empty pieces dropped, order kept. Joining them with ',' gives args back modulo whitespace at the cut points.	lib.rs::top_level_comma_split_ignores_nesting_and_string_literals	char_indices loop, mutable usize/bool state, Vec<&str> of subslices	1,3,5,7
census-parse-fn-def	contract	lib::{parse_fn_def,ident_after_fn}	∀ line: parse_fn_def(line) = Some((indent, name)) ⇔ indent = |line| − |line.trim_start()|, and after a finite sequence of qualifiers (pub, pub(crate|super|self|in ..), const, async, unsafe, default, extern with an optional quoted ABI), each followed by optional whitespace, the rest starts with "fn ", and name is the nonempty maximal ident prefix after "fn " and whitespace. Otherwise None; in particular a line whose first non-qualifier token is not `fn ` (Fn( bounds, `let f = default_fn(x)`) gives None.	lib.rs::parse_fn_def_recognises_qualified_definitions_not_fn_bounds	loop with strip_prefix over a keyword table, split_once, String collect	1,3,5,7
census-term-hop	contract	lib::{term_hop_calls,guard_vars,ident_ending_at}	∀ body: &[String]: (a) guard_vars(body) = {n | some line has a first-found needle N ∈ ["= term_lock(", "= crate::term_lock_ui(", "= term_lock_ui("] and n is the nonempty maximal ident suffix of line[..pos(N)].trim_end()}. (b) term_hop_calls(body) = (H, k). H lists, in line then scan order, every call site m( with m a maximal ident ∈ UNBOUNDED_TERM_METHODS such that either (1) the line contains term_lock( or term_lock_ui( and m follows some ").", or (2) m follows "g." for g ∈ guard_vars(body) with a non-ident byte before g. k counts the sites of the same shape whose m is an OFFLOAD_ALLOWLIST symbol. A receiver that is not a guard never contributes (self.x.resize(, a local `let mut term = Terminal::..`).	lib.rs::term_hop_flags_synchronous_guarded_resize_both_idioms, lib.rs::term_hop_passes_the_offloaded_shape_and_bounded_neighbours, lib.rs::term_hop_ignores_vec_and_local_terminal_resize, lib.rs::guard_vars_captures_the_term_lock_binding, lib.rs::guard_vars_captures_the_term_lock_ui_binding	&[String], BTreeSet<String>, closures capturing &mut Vec, nested while-let loops over find, format!	1,3,5,7,13
census-hold-floors	contract	lib::{CensusOutcome::{hold_floors,finish,green,red}, Floor::{met,at_least,none}, <Floor as Display>::fmt}	∀ o = green/red(title, log, why), ∀ floors F: o.log = log ++ o.verdict ++ "\n"; Floor::met(f) ⇔ f.seen ∈ f.range. After hold_floors, with M = [f ∈ F | ¬f.met()]: if ¬o.ok ∨ M = [] the outcome is unchanged. Otherwise ok = false, verdict = "{title}: FAILED — {|M|} floor(s) missed; the walk itself found no violation", and the log is the old log minus its last line (the old verdict) ++ "  ✗ FAIL [floor] {f}\n" for each f ∈ M in order ++ the new verdict ++ "\n". Display(f) = "{what}: {seen} (an aterm checkout shows {bound}) — {missed}", bound = "at least {min}" when max = usize::MAX, "none" for 0..=0, else "{min}..={max}".	lib.rs::run_censuses_turns_a_missed_floor_red	&mut self on a struct with String/Vec fields, String truncate/push_str, iterator filter/map/collect, RangeInclusive, Display impl	1,2,3,5,7
census-run-censuses	contract	lib::{run_censuses,run_all}	∀ root, selected: &[&Census], out = run_censuses(root, selected): |out| = |selected| and out[i] = hold_floors(r_i), where r_i = (selected[i].run)(root) if that call returns, and r_i = red(name, "=== {name} census ===\n", "the census could not run: …") (log names "{name} census", ok = false) if it panics or its thread cannot spawn. r_i does not depend on the other censuses. run_all(root) zips CENSUSES' names with run_censuses(root, CENSUSES).	lib.rs::run_censuses_keeps_the_given_order_and_turns_a_panic_red, lib.rs::run_censuses_turns_a_missed_floor_red	std::thread::scope, Builder::spawn_scoped, JoinHandle::join with panic capture, fn pointers, Vec zip	1,3,4,5,8,13
census-mainloop-verdict	contract	lib::{run_mainloop_census,mask_unshipped_items,parse_source_fns,callee_names,sweep_markers,collect_closure_markers,collect_out_of_closure_markers,fn_defined_in,marker_sweep_file}	∀ T, with C = ScanSet(T, [aterm-gui]) and G = the name-merged fns of Shipped(non-test aterm-gui/src files): run_mainloop_census(T).ok ⇔ C derives (else SCAN-SET DERIVATION FAILED) ∧ [OB-1] the `// COST: UNBOUNDED(d)` markers in C's src dirs biject with UNBOUNDED_REGISTRY (a registered sink without a marker in C is LOST) ∧ [OB-2] every MAIN_THREAD_ROOTS name has a fn in G ∧ [OB-3] every OFFLOAD_ALLOWLIST entry has a nonempty justification and fn_defined_in(def_file, symbol) ∧ [OB-4] some fn in G makes an allowlisted offload call ∧ [OB-5] no fn reachable from a root has a nonempty term_hop_calls hazard list ∧ [OB-6] no fn reachable from a root calls an UNBOUNDED_REGISTRY symbol ∧ [OB-22] held_guard::scan_tree finds no hit. Each violated obligation's [OB-n] tag is in the log; OB-5 also carries the root -> … -> fn path and the trimmed line. Markers in workspace crates outside C are listed under NOTE [OB-1 scope] with file:line, counted, and never discharge OB-1. Items under a cfg that EntailsTest are invisible; items under any other cfg are visible.	lib.rs::synthetic_offloaded_tree_is_green, lib.rs::synthetic_prefix_resize_shape_is_red_with_path, lib.rs::conjunctive_cfg_test_helper_is_masked_not_graphed, lib.rs::a_disjunctive_cfg_test_gate_is_not_masked_and_stays_red, lib.rs::synthetic_direct_sink_call_is_red_ob6, lib.rs::synthetic_missing_boundary_def_is_red_ob3_and_missing_offload_is_red_ob4, lib.rs::synthetic_missing_root_is_red_ob2, lib.rs::out_of_closure_marker_is_not_swept_and_is_reported, lib.rs::out_of_closure_marker_cannot_satisfy_the_registry, lib.rs::underivable_workspace_fails_the_mainloop_census_closed	fs read_dir/read_to_string over a tree, Path/PathBuf, BTreeMap/BTreeSet BFS, String log building, &dyn Fn predicate	1,3,4,5,7,8,13
census-scan-set-closure	refinement	scan_set::{derive_gui_scan_set,derive_process_scan_set,derive_linked_scan_set,split_target,parse_doc,parse_value,classify_section,dependency_section,dep_edge,load_manifest,normalize_rel,expand_features,external_dependency_names}	∀ T whose manifests lie in the modeled TOML subset: derive_process_scan_set(T, roots) = Ok(S) with S.scan_dirs = ScanSet(T, roots).scan_dirs (sorted, deduplicated) and S.proc_macros = the excluded proc-macro crates as (name, dir); it is Err naming the cause ⇔ a followed path dep has no Cargo.toml (names the path), a closure crate lacks src/ ("no src/ directory"), a normal-section dep is not path, workspace or registry ("cannot soundly classify"), a feature item is neither a feature nor an optional dep ("neither a feature nor an optional dependency"), or a member renames a path dep with `package =` ("renames a path"). Renamed registry deps are understood and not walked. Dev and build sections, cfg-target dev sections included, never contribute. derive_linked_scan_set(T, roots, links) is the same walk except that an edge of a `[target.<key>]` table with ¬links(key) is not followed, and S.unlinked lists each such (crate dir, dep key, key), sorted; derive_process_scan_set is it with links ≡ true (S.unlinked empty).	scan_set.rs::direct_workspace_and_dotted_path_deps_are_followed, scan_set.rs::a_target_table_the_target_cannot_link_is_left_out_of_its_closure, scan_set.rs::dev_and_build_deps_are_excluded, scan_set.rs::cfg_target_path_deps_are_included_platform_independently, scan_set.rs::the_one_table_dependency_form_is_followed_like_the_inline_one, scan_set.rs::optional_deps_follow_default_feature_activation, scan_set.rs::proc_macro_crates_are_classified_out_and_reported, scan_set.rs::missing_dep_manifest_and_missing_src_fail_closed, scan_set.rs::unclassifiable_dep_specs_fail_closed, scan_set.rs::default_features_false_suppresses_and_union_reenables, scan_set.rs::a_renamed_patch_row_is_recorded_under_the_package_it_replaces	recursive-descent TOML-subset parser over &str, enum Value, BTreeMap/BTreeSet fixpoint (feature expansion), Result<_, String>, fs reads	1,3,4,5,7,8
census-scan-set-patch-registry	contract	scan_set::{derive_process_scan_set,load_root_tables,patch_row,classify_patch_target,reviewed_forks}	∀ T, over the rows (key, {package?, path}) of the root [patch.crates-io] table with pkg = package.unwrap_or(key), derivation succeeds only if every row classifies: under vendor/ it needs a `[[fork]]` block in vendor/forge.toml read by reviewed_forks, with census.mode = "scanned" and a census.namespace (else "has NO `[[fork]]` block in vendor/forge.toml"); under crates/ a first-party member is recorded in first_party_patches as (pkg, path) and a review block for it is refused ("points at first-party … ALSO has a `[[fork]]` block"); under crates/ an Absorbed target needs an ABSORBED_CRATES row (else "no ABSORBED_CRATES row") and is in vendored_scanned with its namespace and crate_dir and in absorbed, its src never in scan_dirs; any other path is refused ("under neither `vendor/`" … "fail-closed"). Conversely every reviewed `[[fork]]` block is still a patch row (else "no longer in the root [patch] table") with a Cargo.toml on disk; an ABSORBED_CRATES entry no patch row matches is a renamed member, which needs a Cargo.toml, a retained license and a NOTICE line at its path (else "… STALE") and is in vendored_scanned and absorbed under its manifest's package name; a reviewed fork has its src/ and an absorbed entry every platform-slice path (else "platform slice `<label>` path `<p>` … STALE").	scan_set.rs::scanned_vendored_crate_missing_src_or_slice_path_fails_closed, scan_set.rs::the_review_set_is_the_fork_ledger, scan_set.rs::a_renamed_absorbed_member_is_scanned_namespaced_not_as_a_member, scan_set.rs::an_absorbed_patch_target_is_scanned_namespaced_not_as_a_member, scan_set.rs::the_absorbed_registry_is_fail_closed_both_ways, scan_set.rs::a_first_party_patch_target_is_recorded_and_never_review_registered, scan_set.rs::a_renamed_patch_row_is_recorded_under_the_package_it_replaces, scan_set.rs::a_patch_target_outside_vendor_and_crates_fails_closed, scan_set.rs::a_review_row_for_a_first_party_patch_target_is_refused, scan_set.rs::exclusion_list_is_fail_closed_both_ways	the ledger's TOML subset, const tables of structs with &'static [&str], Vec<(String,String)>, fs existence checks, Result<_, String>	1,3,4,5,7,8
census-classify-patch-target	contract	scan_set::{classify_patch_target,redistribution_evidence,retained_license,notice_names}	∀ T, ∀ p under crates/, with L = an upstream LICENSE* file in p's root (retained_license) and N = a NOTICE line naming p (notice_names): classify_patch_target(pkg, p, T) = Ok(Absorbed) if L ∧ N; Err(e) if exactly one holds, e naming the evidence (the LICENSE path or "NOTICE"); Ok(FirstParty) if neither. redistribution_evidence(T, p) = None ⇔ ¬L ∧ ¬N.	scan_set.rs::a_redistribution_under_crates_is_refused_not_called_first_party, scan_set.rs::a_genuine_first_party_crate_is_not_mistaken_for_a_redistribution	fs read_dir/read_to_string, Option/Result, &Path	1,3,8
census-lock-receiver-identity	contract	lock_order::{acquisitions_on,resolve_receiver,self_rooted_chain,alias_target,mask_literals,impl_spans}	∀ l, ctx = (impls, lineno, aliases): acquisitions_on(l, ctx) returns one RawAcq per acquisition token on mask_literals(l) (.lock(, .read(, .write(, try_* in ACQ_METHODS, or a GUARD_HELPERS call such as term_lock(); a helper's own fn-definition line and any token inside a literal yield none. Its identity, after stripping a trailing '?' run from the receiver, is the first that applies: (1) a helper's registered identity (term_lock → "term"); (2) self.<named_field> → the field; (3) <f>() → f; (4) a single ident aliased in ctx.aliases → its target; (5) inside a nameable impl T covering lineno, self.<tuple-or-dotted path> → "T::<path>" and bare self → "T::self"; (6) else None (UNKNOWN: a.lock(, pair.0.lock(, self.0 outside a nameable impl). alias_target(`let x = &<pure path>;`) is the path's identity, and None for any other RHS (a call, an acquisition, an if-expression).	lock_order.rs::receiver_resolution_handles_fields_accessors_and_unknowns, lock_order.rs::self_receivers_take_the_enclosing_impl_type, lock_order.rs::single_letter_receivers_take_the_path_they_alias, lock_order.rs::a_question_mark_receiver_still_names_its_lock, lock_order.rs::literals_are_masked_before_token_scans	structs of borrowed slices (SiteCtx<'_>), BTreeMap<String,(i32,String)>, Option<String>, byte-level backward scans	1,2,3,5,7
census-impl-self-type	contract	lock_order::{impl_self_type,base_type_name,matching_angle,split_top_level,top_level_words,skip_generics}	∀ impl header h: impl_self_type(h) = Some(N) ⇔ h's self type (after the top-level `for` if any, else after `impl<..>`; `->` inside generics does not close `<`; `unsafe impl` and `where` handled) is nameable with base name N: after stripping &, &'a and mut, its base path segment N (generic args removed) is not a generic parameter of the impl (blanket impl), not in UNQUALIFIABLE_SELF_TYPES (Mutex, Arc, …), not structural ([T], &[T], a tuple), and not a macro fragment ($ty). Otherwise None.	lock_order.rs::impl_headers_resolve_to_a_nameable_self_type_or_none	angle/paren matching over &str, BTreeSet<&str>, Option<String>	1,3,5,7
census-guard-final-value	contract	lock_order::{guard_is_final_value,call_end,match_paren}	∀ l, ∀ acq ∈ acquisitions_on(l, ctx): guard_is_final_value(l, acq) ⇔ the text from the acquisition's closing paren to the statement end is a possibly empty, paren-balanced sequence of GUARD_ADAPTERS calls, `.unwrap()`, `.expect(..)` or `.unwrap_or_else(..)`. That is, the bound value is the guard itself, not a value computed from it.	lock_order.rs::guard_final_value_distinguishes_bound_guard_from_consumed_value	paren matching, iteration over const &[&str]	1,3,5,7
census-return-type-guard	contract	lock_order::return_type_names_a_guard	∀ ret: return_type_names_a_guard(ret) ⇔ some maximal ident run of ret ends with "Guard". MutexGuard, TermGuard, RwLockReadGuard, MappedMutexGuard (under any wrapper) → true; GuardedInput, Guarded<T>, GuardRail, bool → false.	lock_order.rs::a_return_type_must_name_a_guard_not_merely_contain_the_letters	str::split with a char-predicate closure, ends_with	1,3,5,7
census-mask-gated-items	contract	lock_order::{unmasked_lines,mask_items_where,mask_gated_items,mask_cfg_test_items}, lib::{shipped_lines,mask_unshipped_items}, guards::SourceFile::shipped_lines	∀ text, gate g, with K = unmasked_lines(text, g): (a) |K| = |Lines(text)|, and Lines(mask_items_where(text, g))[i] = Lines(text)[i] if K[i], else ""; the output has |Lines(text)| lines, each ending in '\n'. (b) ¬K[i] ⇔ line i lies in an item whose gate line g-matches, under the item-end rules of Mask_g (§0), fail-open restore included; every maximal blanked run has as many '{' as '}' in its literal-masked, comment-stripped code, so masking preserves brace balance. (c) Instances: mask_gated_items(t, gs) is g = (attr ∈ gs); mask_unshipped_items is g = cfg_attr_never_ships; SourceFile::shipped_lines yields exactly the lines i with unmasked_lines(Code(f), cfg_attr_never_ships)[i], in both views, line numbers kept.	lock_order.rs::gated_items_end_at_their_own_shape_not_at_a_brace_count, wasm_census.rs::mask_gates_blank_native_only_blocks_and_test_fns, guards/mod.rs::shipped_lines_drop_test_items_in_both_views	Vec<bool> indexed loops, &dyn Fn(&str)->bool, nested while loops with early break, closures, String building	1,3,5,7,13
census-lock-hold-edges	contract	lock_order::{scan_fn,logical_lines,classify_line,binding_eq,held_call_targets,takes_self,run_lock_order_census}	∀ T, the EDGE set the lock census reports is E = {(A, B) | a blocking (non-try_*) acquisition of identity B happens while a guard of identity A is live}. A is live from its binding (`let` with guard_is_final_value, or a try-let) until the first of drop(g), the end of the enclosing block, a shadowing re-let, or the end of the statement for a consumed value. A try-acquired guard can be a source, never a target. Logical lines join rustfmt chain continuations. One hop: a call f() while A is live adds (A, B) for each B the unique same-namespace definition of f acquires. A field hop self.fld.m() is followed only if fld's declared type is a corpus struct and m names exactly one corpus fn; otherwise it is listed as not followed. Vendored files give identities ns::id. A by-name callee resolves first in the caller's namespace and crosses the boundary only when unshadowed. File-advisory and raw-pointer-read sites never appear in E.	lock_order.rs::synthetic_diamond_without_cycle_is_green, lock_order.rs::synthetic_one_hop_interprocedural_cycle_is_red, lock_order.rs::synthetic_same_field_on_different_types_does_not_merge, lock_order.rs::synthetic_tuple_field_abba_across_two_types_is_red, lock_order.rs::synthetic_drop_and_statement_scope_end_the_hold, lock_order.rs::synthetic_multiline_chain_resolves_receiver_and_nests, lock_order.rs::synthetic_try_lock_is_a_source_but_never_a_target, lock_order.rs::synthetic_file_advisory_lock_joins_no_mutex_edges, lock_order.rs::synthetic_raw_ptr_read_joins_no_mutex_edges, lock_order.rs::synthetic_vendored_identities_are_namespaced_and_never_merge, lock_order.rs::synthetic_cross_boundary_one_hop_edge_is_graphed, lock_order.rs::a_vendored_fn_name_cannot_capture_an_aterm_one_hop_call, lock_order.rs::a_field_hop_is_followed_when_the_field_type_and_the_callee_both_resolve, lock_order.rs::a_field_hop_is_not_followed_when_the_callee_name_is_ambiguous, lock_order.rs::a_field_hop_is_not_followed_when_the_field_type_is_foreign	per-fn scope stack (Vec), BTreeMap ledgers, string scans, interprocedural by-name tables, fs tree walk	1,2,3,4,5,7,8
census-lock-site-classification	invariant	lock_order::{run_lock_order_census,raw_ptr_read_evidence,is_file_binding_rhs,is_raw_ptr_binding_rhs,rhs_extends_raw_ptr,param_is_raw_ptr,match_paren_back}	∀ T, each acquisition site on a Shipped line of the derived scan set falls in exactly one category, and the reported counts equal the category sizes: (1) resolved identity n (ledger counts id(n)); (2) UNKNOWN, a unique per-site node UNKNOWN@file:line, counted separately for aterm and per vendored crate; (3) an audited VOCABULARY_INTERIORS body, rendered with its audit note; (4) OS file-advisory: the receiver is proven a std::fs::File by a live binding whose RHS constructs one (OpenOptions/File::open/create) or an ascribed `: std::fs::File`; Option<File>, a rebinding to a non-File or an out-of-scope binding is not proof; (5) raw-pointer ptr::read: a zero-arg .read() on a chain from a RAW_PTR_METHODS producer, on a live binding proven raw (directly or through .add/.offset of a proven pointer), or on a *const/*mut parameter; without proof the receiver keeps its RwLock resolution; (6) platform slice: a site in a registered platform slice, counted under its label. Categories 2–6 are never graphed, so they never close a cycle.	lock_order.rs::synthetic_unknown_identity_is_counted_never_dropped, lock_order.rs::synthetic_unqualifiable_impl_types_keep_the_per_site_node, lock_order.rs::synthetic_vocabulary_interior_is_audited_not_unknown, lock_order.rs::synthetic_file_advisory_lock_is_categorized_not_unknown, lock_order.rs::synthetic_file_ascription_is_evidence_and_generics_are_not, lock_order.rs::synthetic_file_evidence_is_fail_closed_on_rebind_and_scope, lock_order.rs::synthetic_raw_ptr_reads_are_categorized_not_unknown, lock_order.rs::synthetic_propagated_raw_ptr_binding_is_categorized, lock_order.rs::synthetic_vendored_unknown_is_summarized_per_crate, lock_order.rs::synthetic_platform_slice_sites_are_counted_never_graphed, lock_order.rs::synthetic_cfg_test_items_are_masked	BTreeMap<String,(i32,String)> evidence ledgers with scope depth, enum categories, fs walk	1,2,3,5,7,8
census-lock-order-verdict	contract	lock_order::{run_lock_order_census,tarjan_sccs,returns_guard}	∀ T: run_lock_order_census(T).ok ⇔ (i) ScanSet(T, [aterm-gui]) derives (else SCAN-SET DERIVATION FAILED) ∧ (ii) every GUARD_HELPERS and VOCABULARY_INTERIORS entry is still defined at its def_file and still contains its registered acquisition (else "no longer defined" / "no longer contains the bare-`self` acquisition") ∧ (iii) every fn whose return type names a guard (census-return-type-guard) and which acquires and returns it is registered (else "UNREGISTERED GUARD-RETURNING HELPER"), a free fn included even when named lock, while a method named lock/read/write is exempt whatever its visibility ∧ (iv) at least one acquisition site was read (else ZERO lock-acquisition) ∧ (v) the graph (identities, E of census-lock-hold-edges) has no self-edge ("RE-ENTRANCY SUSPECT") and no SCC of size ≥ 2. tarjan_sccs(adj) is exactly the SCCs of adj. Each cycle prints "CYCLE" with every EDGE a -> b and both file:line sites of each edge, across namespaces. A crate that newly enters the derived closure is scanned with no census change.	lock_order.rs::synthetic_abba_cycle_is_red_naming_both_sites, lock_order.rs::synthetic_self_edge_is_red_reentrancy_suspect, lock_order.rs::synthetic_cross_boundary_abba_is_red_across_the_namespace, lock_order.rs::synthetic_stale_vocabulary_registration_is_red, lock_order.rs::synthetic_unregistered_guard_returning_helper_is_red, lock_order.rs::synthetic_free_fn_with_standard_name_is_not_exempt, lock_order.rs::synthetic_empty_tree_fails_closed, lock_order.rs::synthetic_tree_without_manifests_fails_closed, lock_order.rs::synthetic_stale_helper_registration_is_red, lock_order.rs::synthetic_new_gui_dependency_is_scanned_automatically	Tarjan recursion over Vec<Vec<usize>>, BTreeMap graph building, fs walk, String log	1,3,4,5,7,8
census-lazy-init-verdict	contract	lazy_init::{derive_from_sources,report,collect_cells,collect_field_cells,static_name,declaration_text,collect_touches_and_inits,enclosing_fns,balanced,initializer_callees,direct_touches,derive_edges,reachable,find_cycles,mentions_ident,DerefNames::of,DerefNames::may_mention,receiver_before}	∀ S: &[(path, text)] (pure, no fs), with D = derive_from_sources(S) and O = report(root, |S|, D). D's cells are the static or fn-local `[vis] static X: <..>(OnceLock|OnceCell|Once|LazyLock|..)<..>` declarations (any visibility, a wrapped ctor, `;` inside the type) plus OnceLock-typed struct fields (census-lazy-field-cell-name), on Shipped lines only. A cell's initializer is the closure or fn reference passed to a blocking touch (get_or_init and kin, call_once; turbofish and wrapped receivers allowed), the LazyLock::new closure, or an unqualified fn reference (followed); a qualified Type::new is not followed. X → Y is an edge ⇔ X's initializer reaches a blocking touch of Y through the name-merged call graph (bodyless extern decls own no lines; an array length in a signature does not end it). Any mention of a deref-lazy cell is a touch; get() never is. Then O.ok ⇔ D has ≥ 1 cell (else [OB-21]) ∧ no self-edge (else [OB-19], naming the cell in backticks, the accessor, and FIELD for field cells) ∧ no SCC of ≥ 2 cells (else [OB-20], naming every cell).	lazy_init.rs::every_reentry_shape_is_judged	Vec<(String,String)>, structs of Vec/BTreeMap/BTreeSet, BFS reachability, SCC/cycle search, String log	1,3,4,5,7
census-lazy-field-cell-name	contract	lazy_init::field_cell_name	∀ l: field_cell_name(l) = Some(name) ⇔ l.trim() has the shape `[pub | pub(<..>)] name: <ty>,` with name an identifier, l not a `let` binding, ty not starting with '&' (a borrowed cell is a parameter, not a declaration), and ty's base type a METHOD_LAZY_TYPES cell (OnceLock, OnceCell, …), optionally path-qualified. Otherwise None.	lazy_init.rs::a_borrowed_cell_parameter_is_not_a_declaration	&str strip_prefix/split_once, Option<String>	1,3,5
census-lazy-find-method-call	contract	lazy_init::find_method_call	∀ hay, method, from: find_method_call(hay, method, from) = the first occurrence at or after from of "." ++ method such that the byte after method is not an ident byte (get_or_init ≠ get_or_init_with) and, after optional whitespace and an optional turbofish ::<..>, the next byte is '('; the pair spans the method name and the call's open paren. None if no occurrence qualifies (including a mention with no '(').	lazy_init.rs::a_longer_method_name_is_not_matched_as_a_shorter_one	&str match_indices loop, byte indexing, Option<(usize,usize)>	1,3,5,7
census-lazy-bodyless-decl	contract	lib::is_bodyless_declaration	∀ lines, at < |lines|: is_bodyless_declaration(lines, at) ⇔ the fn signature starting at lines[at] reaches ';' before any body-opening '{', balancing [..] and (..), so `-> [u8; 4] {` opens a body and a ';' inside an array type does not end the signature.	lazy_init.rs::every_reentry_shape_is_judged, lib.rs::a_bodyless_declaration_does_not_swallow_the_fns_after_it	&[&str], depth-counter loop	1,3,5,7
census-held-guard-scan	contract	held_guard::{scan_source,find_acquire,has_blocked_work,touches_cold_codec,receives_work,bind_guard,pattern_idents,strip_type,drops,shadows,split_let,zero_arg_hop,bare_receiver_line,method_call_at,statement_end,paren_balance}	∀ text, over code = mask_unshipped_items(blank_non_code(text)): scan_source(text) = (H, a). a = the number of nonblank lines with a term acquire (term_lock(, crate::term_lock(, term_lock_ui(, or a with_terminal_mut(.., |t| ..) closure region). H = the sorted 1-based numbers of the lines on which a BLOCKED_METHODS call (resize, reflow, hydrate, take_scrollback, rebuild_index, decompress, …) or a COLD_CODEC touch is made on a live guard. A live guard is a value chained directly on an acquire on that line, the closure parameter of with_terminal_mut, or a name bound by `let` from an acquire (patterns, &mut, type ascription, crate:: paths). It reaches the call through zero-arg hops, &mut *g, (*g), deref_mut() or a wrapped chain; the reported line is the line of the work. Liveness ends at drop(g), the guard's block close, or a shadowing re-let (typed or not). A chain that reads a value out (`let c = term_lock(t).cols();`) binds no guard. Comments, strings, char literals, cfg(test) items and raw strings holding `}` or `#[cfg(test)]` never open, close or extend a region. The SAFE offload calls (resize_offloading_scrollback, finish_resize_offload) are not work.	held_guard.rs::every_must_fire_shape_draws_a_finding, held_guard.rs::every_must_silent_shape_draws_none, held_guard.rs::a_finding_is_the_line_of_the_work	struct Walk with &mut self step over lines, Vec<String> live-guard stack with depths, i64 paren balance, string scans	1,2,3,5,7
census-scope-verdict	contract	scope_census::{run_scope_census_over,check_liveness,check_link,check_closure,check_closure_fields,check_closure_ctors,check_aggregation,struct_body,all_struct_bodies,fn_body,enclosing_fn,derive_model_sources}	∀ T, ∀ claim c ∈ SCOPE_CLAIMS: run_scope_census_over(T, [c]).ok ⇔ [OB-13] every chain link's pinned field declaration occurs exactly once in its owner struct (else "occurs n time(s)") with a by-value type ∧ [OB-14] over the sweep files no struct field outside the claimed chain holds a closed token (engine or pipeline type) by value (enum variants, Self:: variants, impl blocks and &/&mut parameters are not fields), and no fn outside the claimed owners constructs one (a by-value ctor from closed_token_ctors, or Type::default()/new()/for_window() inside a loop or Vec); the finding names the owner struct or the constructing fn ∧ [OB-15] the aggregator fn reads every claimed shard expression (e.g. self.parked) ∧ [OB-16] the claimed owner struct and its machine resolve in the derived model catalog (else STALE). Each red log names "[OB-n] claim `id`" and carries the claim's safety argument.	scope_census.rs::a_per_pane_word_decorations_map_fails_the_flash_limiter_chain, scope_census.rs::a_new_per_pane_owner_fails_the_closure, scope_census.rs::a_per_pane_constructor_fails_the_closure, scope_census.rs::a_per_pane_named_constructor_fails_the_closure, scope_census.rs::a_renamed_field_or_owner_fails_stale_rather_than_silently_passing, scope_census.rs::enum_variants_and_fn_parameters_are_not_owners, scope_census.rs::the_supernova_aggregator_regression_goes_red	const claim tables, fs reads, brace-matched &str slicing, BTreeMap<&str, Vec<String>>, String log	1,3,4,5,7,8
census-scope-ctor-discovery	contract	scope_census::{closed_token_ctors,impl_bodies,self_returning_fns,skip_generics}	∀ src, token T: impl_bodies(src, T) = the bodies of the `impl[<..>] [Trait for] [path::]T[<..>]` blocks (generics skipped; Ts or U ≠ T). self_returning_fns(body, T) lists, in order, the fns whose return type mentions Self or T by value (-> Self, -> Option<Self>, -> T where …); &Self, &T and other return types are excluded. closed_token_ctors(files, claims)[T] = the sorted, deduplicated union of self_returning_fns over the impl_bodies of T (inherent and trait impls) in the cfg(test)-masked files.	scope_census.rs::named_constructors_are_discovered_from_impl_blocks	&str scanning with brace matching, BTreeMap<String, BTreeSet<String>>, fs read (closed_token_ctors only)	1,3,5,7,8
census-scope-returns-token	contract	scope_census::returns_token	∀ before (the text preceding a token occurrence): returns_token(before) ⇔ before.trim_end() ends with `->`, optionally followed by `&`, `&mut`, `&'a` or `&'a mut`, i.e. the occurrence is a return type, not a construction. Field ascriptions, let bindings and call arguments give false.	scope_census.rs::a_return_type_is_not_a_construction	&str strip_suffix/trim_end chain	1,3,5
census-wasm-term-hops	contract	wasm_census::wasm_term_hops	∀ body: &[String]: wasm_term_hops(body) lists, in order, every code (not literal) call site `term.m(` on the literal-masked line where the ident ending at the dot is exactly `term` (a field self.term or a local; subterm does not count) and m is a maximal ident ∈ UNBOUNDED_TERM_METHODS (resize_surface ≠ resize). Other receivers (self.rgba, t) and other methods (process) give no hop.	wasm_census.rs::wasm_term_hop_flags_the_field_and_rebind_shapes, wasm_census.rs::wasm_term_hop_ignores_other_receivers_and_methods	&[String], mask_literals, ident_ending_at byte scans, Vec of structs	1,3,5,7
census-wasm-verdict	contract	wasm_census::{run_wasm_census,crate_source_files,spawn_sites,unloaded_files}, wasm_cfg::{unloaded_on_wasm32,mod_decl_name,include_arg,path_attr}	∀ T, with W = the derive_linked_scan_set(T, WASM_ROOT_CRATES, wasm_cfg::target_links_on_wasm32) closure (its unlinked edges printed): run_wasm_census(T).ok ⇔ [OB-8] W derives (else "[OB-8] SCAN-SET DERIVATION FAILED") and the engine crates are in W with their marked sinks ∧ [OB-10] no fn reachable from a public wasm export, in the name-merged graph over Mask_{wasm_cfg::attr_never_on_wasm32} sources, has a wasm_term_hops hazard or calls a registered sink directly ("DIRECT call"); the log prints the export -> … -> fn path and file:line ∧ [OB-12] no SPAWN_TOKENS occurrence lies on a W-crate line kept by that mask (so #[test] fns and items or blocks under a one-line #[cfg(p)] with p false on wasm32-unknown-unknown, e.g. cfg(not(target_arch = "wasm32")) or cfg(windows), are excluded; an unknown p keeps them), and none lies in a file U = wasm_cfg::unloaded_on_wasm32 leaves out, which OB-10's sources skip too: a file is left out ⇔ it is no crate root (src/lib.rs, src/main.rs, a manifest `path`) and it has a loader, and every loader (a body-less `mod` resolved as rustc resolves it, `#[path]` and `include!` paths from the loading file's directory, a #[path]-loaded or root file read both ways; an `include!` of a plain path) is masked by that mask or sits in a file left out; a crate with a live loader the reader cannot place (a `mod` inside an inline item, an `include!` of a computed path, a `mod` in an included file, a `cfg_attr` or unreadable `path`) leaves nothing out and its tree is named; the log counts the files left out, the test-only cuts, and names every other cut with its gate. On green the log states the threading posture.	wasm_census.rs::synthetic_wasm_tree_with_offloaded_resize_is_green_with_zero_findings, wasm_census.rs::synthetic_reintroduced_sync_resize_is_red_ob10, wasm_census.rs::synthetic_new_unregistered_reach_is_red_ob10_with_path, wasm_census.rs::synthetic_direct_sink_call_is_red_ob10, wasm_census.rs::synthetic_unmasked_spawn_in_closure_is_red_ob12_and_gated_spawn_is_not, wasm_census.rs::synthetic_spawn_behind_a_native_target_table_is_not_in_the_closure, wasm_census.rs::synthetic_spawn_in_a_file_only_a_false_mod_loads_is_not_swept, wasm_census.rs::synthetic_missing_root_crate_fails_closed_ob8, wasm_cfg.rs::a_module_only_a_false_gate_loads_is_no_wasm32_file, wasm_cfg.rs::a_crate_root_is_never_left_out, wasm_cfg.rs::a_module_tree_this_reader_cannot_place_leaves_every_file_in, wasm_cfg.rs::an_include_is_a_loader_like_a_mod	fs walk, BTreeMap call-graph BFS, String log	1,3,4,5,7,8
census-cli-parse	contract	main::parse	∀ argument lists A, scanned left to right: the first -h/--help before any error gives Ok(None). An error is a `--x` with x ∉ {all, verbose} ∪ CENSUSES names, any other `-x` except -v, or a second positional; the first error gives Err. Otherwise Ok(Some(inv)) with inv.verbose ⇔ some a ∈ {-v, --verbose}, inv.root = the unique positional if any, and, with N = the names given as `--name` plus every name if --all occurs, inv.selected = [c ∈ CENSUSES | N = ∅ ∨ c.name ∈ N] in CENSUSES order, each census once.	-	impl IntoIterator<Item=String>, Vec<&'static Census>, sort_by_key/dedup_by_key, Option<PathBuf>, Result<Option<_>, String>	1,3,5,7,11
census-shell-word	round-trip	main::shell_word	∀ path p, s = p.display().to_string(): shell_word(p) = s if s is nonempty and every char is ASCII alphanumeric or one of /._-+:@; otherwise "'" ++ s.replace("'", "'\\''") ++ "'". Either way, POSIX sh word splitting and quote removal of shell_word(p) yields exactly the one word [s].	-	String, chars().all, str::replace, format!	1,3,5
census-guard-calls	contract	guards::{calls,is_word}	∀ code: calls(code) = in order of '(' occurrence, the identifiers w where w is the maximal run of word bytes ending right before the '(' (spaces and tabs skipped), leading ASCII digits removed, w nonempty. A '(' after a non-word byte (`(1, 2)`, `f[0](`) contributes nothing.	guards/mod.rs::calls_are_the_identifiers_before_each_paren	&str match_indices loop, byte backward scans, Vec<&str>	1,3,5,7
census-guard-hits	contract	guards::{hits,has}	∀ hay, needle: hits(hay, needle, Sub) = the offsets of str::match_indices(needle), in order; hits(hay, needle, Word) = the subsequence of offsets i that are word-boundary hits (§0). has(…) ⇔ hits(…) ≠ [].	guards/mod.rs::word_hits_respect_identifier_edges	iterator adaptors with closures capturing slices, impl Iterator return	1,3,5,7,13
census-guard-run-rules	contract	guards::{run_rules,scope_findings,run_guards,run_guard_census,Corpus::under}	∀ C, Rs: run_rules(tree, Rs) = concat over R ∈ Rs in order of R.check's findings sorted by (path, line) if R.check returns, or the single finding {R.id, "", 0, "the rule could not run: …"} if it panics. scope_findings(C, Rs) has exactly one finding {R.id, prefix, 0} for each R and each prefix ∈ R.scope under which no file of C lies. run_guard_census is ok ⇔ run_guards yields no finding ∧ the floors hold (≥ 1500 Rust files and ≥ 17 scripts read).	guards/mod.rs::a_panicking_rule_is_one_finding_not_a_crash, guards/mod.rs::a_scope_no_file_lies_under_is_a_finding	std::thread::scope + spawn_scoped + join panic capture, fn pointers, Vec<Finding> sort_by	1,3,4,5,8,13
census-rule-engine-platform	contract	guards::rules::{engine_platform,engine_shipped,links_by_name,grouped_std_modules}	∀ C: spots(Run(ENGINE_PLATFORM, C)) = sorted {f.rel:l | f under an ENGINE_CRATES prefix ∧ engine_shipped(f.rel) (not test_path, not a build.rs, an /examples/ path or aterm-effects' file_feed.rs) ∧ l a Shipped line ∧ (Code(l) has a word or substring hit of an ENGINE_BANNED needle (std::fs, thread::spawn, thread::Builder, the word-bounded modules std::thread and std::net, …) ∨ l holds the first segment fs, net or thread of a grouped `use std::{…}` import, however it wraps ∨ Text(l) names an ENGINE_FRAMEWORKS name (AppKit, Metal, …) word-bounded ∨ l links a native library: link(name = …) in #[link] or #[cfg_attr(.., link(..))], possibly over several lines)}. One line carries one detail per distinct cause, ordered banned, framework, link. Prose, an identifier containing a framework name (MetalLayer) and a fn named link( are not hits.	guards/rules.rs::the_engine_names_no_platform_in_shipped_code, guards/rules.rs::an_engine_framework_named_in_a_literal_or_a_cfg_attr_link_is_found	iteration over corpus files and lines, const tables of (&str, Bound, &str), Vec<Finding>	1,3,5,7
census-test-path	contract	guards::rules::test_path	∀ rel: test_path(rel) ⇔ some '/'-component of rel, with a trailing .rs removed and split on '_', has a piece starting with "test". So tests/, page_tests, style_test_helpers and test_support count; latest.rs and attestation.rs do not.	guards/rules.rs::a_test_path_is_named_by_a_word_not_a_substring	nested str::split iterators with any	1,3,5
census-test-file	contract	lib::is_test_file	∀ path, with s = path.to_string_lossy() and n = the lossy file name (absent: ""): is_test_file(path) ⇔ s contains "/tests/" ∨ s ends with "_tests.rs" ∨ n starts with "tests_" ∨ s contains "/benches/" ∨ s ends with "/proofs.rs" ∨ s contains "proofs_". So src/tests_pwsh_keys.rs, src/unix_tests.rs and tests/smoke.rs are test files; src/lib.rs, src/latests_seen.rs and src/tests_dir/lib.rs (a directory named tests_…, not the file) are not.	lib.rs::a_tests_part_is_a_test_file_and_a_name_that_only_contains_it_is_not	Path::file_name, OsStr/Path lossy conversion, str contains/ends_with/starts_with	1,3,5
census-rule-renderer-terminal	contract	guards::rules::{renderer_terminal,names_terminal}	∀ C: spots(Run(RENDERER_TERMINAL, C)) = sorted {f.rel:l | f under RENDERERS ∧ ¬test_path(f.rel) ∧ l Shipped ∧ names_terminal(Code(l)) ≠ None}, where names_terminal(code) ≠ None ⇔ code has a word-bounded `Terminal` that is the type aterm_core::Terminal (a use, a parameter or a return type). Terminal-prefixed identifiers (TerminalFrame) and prose do not count.	guards/rules.rs::the_renderers_never_name_terminal	corpus iteration, Bound::Word hits	1,3,5,7
census-rule-dev-read	contract	guards::rules::{dev_read,reads_a_device}	∀ C: spots(Run(DEV_READ, C)) = sorted {f.rel:l | f ∈ PRODUCT (tests included) ∧ reads_a_device(l)}, where reads_a_device(l) ⇔ Code(l) has a call to fs::read, read_to_string or read_dir (any path qualification) whose first argument, in Text(l) after optional whitespace, is a string literal starting with "/dev/". A /dev/ literal elsewhere, a comment, or a call inside a string literal does not count.	guards/rules.rs::a_device_read_to_eof_is_found_in_code_only	aligned two-view line scan, Line<'_> struct	1,3,5,7
census-rule-input-probe	contract	guards::rules::input_probe	∀ C: spots(Run(INPUT_PROBE, C)) = sorted {f.rel:l | f ∈ PRODUCT outside the census crate ∧ Text(l) contains an INPUT_PROBES name (CGEventSourceFlagsState, CGEventSourceSecondsSince, SLSInputModifierKeyState, +[NSEvent modifierFlags], …)}. Since Text keeps literals and drops comments, a declaration or string fires and a comment does not.	guards/rules.rs::an_input_probe_is_found_in_code_and_strings_not_prose	corpus iteration, substring search	1,3,5,7
census-rule-protected-folder	contract	guards::rules::{protected_folder,protected_path}	∀ C: spots(Run(PROTECTED_FOLDER, C)) = sorted {f.rel:l | f under PROTECTED_SCOPE ∧ ¬test_path(f.rel) ∧ l Shipped ∧ protected_path(Text(l)) ≠ None}, where protected_path(t) ≠ None ⇔ some string literal in t is, starts with, or has as a whole path component a TCC-protected location: a PROTECTED_HOME_DIRS name (Documents, Downloads, Desktop, …), ~/<dir>, /Volumes/… (not macOS's own /System/Volumes/<volume>, though /System/Volumes/Data/Volumes/… counts), or Library/CloudStorage. Identifiers (DocumentsView) and comments do not count.	guards/rules.rs::a_protected_folder_literal_is_found_in_gui_and_cli_shipped_code	literal extraction from the Text view, path-component matching	1,3,5,7
census-rule-runloop-timer	contract	guards::rules::{runloop_timer,runloop_scope}	∀ C: spots(Run(RUNLOOP_TIMER, C)) = sorted {f.rel:l | runloop_scope(f.rel) ∧ Text(l) contains a RUNLOOP_TIMERS needle (CFRunLoopTimerCreate, scheduledTimerWithTimeInterval:, …)}, where runloop_scope(rel) ⇔ rel ∈ PRODUCT ∧ rel is not aterm-objc's wake_timer.rs ∧ (¬test_path(rel) ∨ rel under WINIT_SOURCE = crates/aterm-window/src/).	guards/rules.rs::a_runloop_timer_lives_only_in_wake_timer	corpus iteration, prefix predicates	1,3,5,7
census-rule-keeper-gui	contract	guards::rules::{keeper_gui,links_gui}	∀ C: spots(Run(KEEPER_GUI, C)) = sorted {f.rel:l | f under KEEPER_SOURCE ∧ links_gui(Text(l)) ≠ None}, where links_gui(t) ≠ None ⇔ t (comments dropped) names a GUI_FRAMEWORKS framework (AppKit, …) in a link attribute, or a word-bounded GUI_NAMES token (objc_msgSend, winit, …). Non-GUI frameworks (Security) do not count.	guards/rules.rs::the_keeper_links_no_gui_framework	corpus iteration, const tables	1,3,5,7
census-rule-keeper-gui-deps	contract	guards::rules::{keeper_gui_deps,is_external_gui}, scan_set::{external_dependency_names,member_patch_targets,read_lock}	∀ T: keeper_gui_deps yields one finding per crate manifest M in the keeper's normal-dependency closure that either (a) declares an external GUI dependency d with is_external_gui(d) (objc2, winit, …), at M's Cargo.toml with detail naming `d`, or (b) is a workspace GUI crate in GUI_CRATES (aterm-render, …), at that crate's Cargo.toml with detail "in the keeper's process closure"; plus, walking Cargo.lock breadth-first from M's external normal dependencies (a [patch] member reached through the lock is read by its manifest's closure, never its lock entry, which lists dev-dependencies), one finding per GUI package reached, at M's Cargo.toml, "reaches `p` through a -> … -> p"; an external dependency the lock lacks is a finding; an unreadable lock is a finding.	guards/rules.rs::the_keepers_closure_holds_no_gui_crate, guards/rules.rs::the_keepers_closure_is_walked_through_cargo_lock	fs manifest reads, BTreeSet worklist	1,3,4,5,7,8
census-rule-std-command	contract	guards::rules::{std_command,std_command_door}	∀ C: spots(Run(STD_COMMAND, C)) = sorted {f.rel:l | f ∈ PRODUCT outside crates/aterm-uds/ ∧ Code(l) has a word-bounded StdCommand followed by `::new` (spaces allowed, `new` a whole word), by ` as ` (a rename), or preceded by `=`, past any path (`aterm_uds::spawnfd::`), on a line naming `type` (a re-alias)}; test items included. Prose, strings and signatures that only take `&mut StdCommand` never fire.	guards/rules.rs::the_std_command_alias_is_built_renamed_or_re_aliased_only_in_aterm_uds	substring and word-bound scans	1,3
census-rule-flock-door	contract	guards::rules::{flock_door,flock_line,syscalls_by_number}	∀ C: spots(Run(FLOCK_DOOR, C)) = sorted {f.rel:l | f ∈ PRODUCT not under LIBC_CRATE ∧ l is a door line}. A door line, read in Code (Text for link_name), is: (1) a line of an extern block declaring `fn flock(`; (2) exactly #[link_name = "flock"] (not flockfile); (3) a word hit of O_EXLOCK or O_SHLOCK (not O_EXLOCKED); (4) F_OFD_SETLK or F_OFD_SETLKW (not F_OFD_GETLK); (5) the line of a SYS_flock word in a syscall argument list; (6) the line where libc::syscall( opens with first argument the literal 131 or 73, possibly wrapped onto the next line. libc::flock calls, file.try_lock, fns named flock_*, my_syscall, other SYS_ numbers, comments and strings are never door lines.	guards/rules.rs::every_flock_door_the_lint_cannot_name_is_found, guards/rules.rs::doors_the_lint_sees_and_near_misses_are_silent	per-file multi-line state (extern block, open syscall), Vec<usize>, Option<&'static str>	1,3,5,7
census-rule-supervise-fx	contract	guards::rules::{supervise_fx,supervise_fx_scope,supervise_clock,reader_homes,read_homes,wall_read,use_statements,use_leaves,use_tree,type_alias,imported_read}	∀ C: the clock and phase-2 findings of Run(SUPERVISE_FX, C) (phase 3's looks are not this row's) are {f.rel:l | f under SUPERVISE = crates/aterm-agent/src/supervise/ ∧ ¬test_path(f.rel) ∧ f not under a SUPERVISE_CLOCK_HOMES prefix (fx.rs, tape/, transport.rs, mail.rs) ∧ l Shipped ∧ (A ∨ B ∨ T) ∧ the hit is not at home}. A: l lies in no use statement (use_statements, joined across its lines) and Code(l) has a word hit of a SUPERVISE_CLOCKS or SUPERVISE_READS name in its shape — Named (Instant::now, SystemTime::now, thread::sleep, thread::sleep_until, thread::sleep_ms, thread::park_timeout, process::id, the approvals:: path readers and bound) anywhere; Reader (unix_now, unix_now_ms, unix_ms, unix_secs, unix_millis, since_epoch) preceded by :: or followed by ( on no fn line; Method (elapsed, try_recv, recv_timeout) preceded by :: or by . and followed by ( — or calls a ledger_by_path reader through approvals::. B: l is a use statement's first line and one of the leaves use_leaves expands it to (whole paths through every group; {self} the module) binds such a name where no line can see it: a multi-segment name or a one-word Reader imported whole (bare or renamed), a one-word Named renamed, a head segment renamed (thread as t, Instant as Mono, approvals as x), its parent module globbed (thread::*), or approvals::<a ledger_by_path reader>; as _ binds nothing, and a Method is never imported. T: l is a type alias of a plain path, read as use path as name. At home: a wall_read (SystemTime::now or a Reader name) in a SUPERVISE_READER_HOMES fn in its own file (journal.rs unix_ms; limit.rs unix_now, unix_now_ms), or a SUPERVISE_READ_HOMES needle in its own fn in its own file (claim.rs default_holder: process::id); a fn of a home's name elsewhere is none, and a home excuses nothing else in its body. The door's own self.fx.sleep(, self.fx.try_recv_mail(, an approvals::*_in reader, a local or field of a reader's name (d.unix_ms), a field named elapsed, an import under the name's own name (use std::thread;, use std::time::Instant;), prose and strings are not hits. Out of a lexical guard's sight, said at SUPERVISE_FX: a macro, a glob of a reader's module then the reader passed bare, <Instant>::now(), a clock read in a helper outside SUPERVISE.	guards/rules.rs::every_clock_read_past_the_supervisors_door_is_found, guards/rules.rs::the_door_and_the_clocks_homes_are_silent, guards/rules.rs::a_wall_readers_home_is_its_own_fn_in_its_own_file_for_the_wall_alone, guards/rules.rs::an_import_that_keeps_a_clocks_name_is_silent	const tables of (&str, enum, &str), fn attribution by brace depth, BTreeSet<usize>, prefix predicates, a use tree expanded to Vec<&str> paths	1,3,5,7
census-rule-store-current	contract	guards::rules::{store_current,store_current_scope,joins_current,resolves}	∀ C: spots(Run(STORE_CURRENT, C)) = sorted {f.rel:l | store_current_scope(f.rel) (a store-handling crate, not test_path) ∧ l Shipped ∧ joins_current(l) ∧ no line in [l, l + RESOLVE_REACH) of f, RESOLVE_REACH = 6, satisfies resolves (canonicalize or physical)}, where joins_current(l) ⇔ Text(l) has a string-literal argument to join(..) or PathBuf::from(..) with a '/'-component equal to "current" (not currently, current_x).	guards/rules.rs::a_store_current_path_is_resolved_within_six_lines	windowed look-ahead over lines, literal parsing	1,3,5,7
census-rule-licence-header	contract	guards::rules::{licence_header,header_gap}	∀ C: spots(Run(LICENCE_HEADER, C)) = sorted {f.rel:1 | f ∈ PRODUCT (not vendor/) ∧ header_gap(f) ≠ None}, where header_gap(f) = None ⇔ the first HEADER_LINES = 8 raw lines of f include a line starting `// SPDX-License-Identifier:` and a line starting `// Copyright`, in either order.	guards/rules.rs::a_header_needs_both_lines_in_either_order	take(8) over lines, two booleans	1,3,5,7
census-rule-devtools-door	contract	guards::doors::{devtools_door,builds_a_process,names_the_tool,path_const_allowed,path_argv_flag,bare_path,product}	∀ C, a line l of a shipped non-test product file f is a finding ⇔ (a) Code(l) names run_devtools_change as a word, on a line that is not its `fn run_devtools_change` definition, anywhere (DEVTOOLS_HOME included) but in the body of spawn_devtools_worker in DEVTOOLS_FLOW; ∨ (b) Text(l) holds the DevToolsSecurity path literal outside DEVTOOLS_HOME, or inside it anywhere but the DEVTOOLSSECURITY_PATH const definition or a quiet-flag (-status/-disable) literal; ∨ (c) DEVTOOLSSECURITY_PATH is in an argv with a non-quiet flag outside the sanctioned devtools_*_command fns; ∨ (d) an AUTHZ_NAMES call or AUTHZ_LITERALS string appears; ∨ (e) a SPAWN_CALLS process-building call names the tool. Prose naming the tool in a print string is not a finding. spawn_devtools_worker is the live arm of ConsentGestures::run_devtools, which aterm-gui reaches only through App::run_debugger_change for the owner's yes in aterm's alert: their Settings press, or the once-per-Mac launch ask (owner ruling O29), which added no caller of the runner, so the rule is unchanged.	guards/doors.rs::only_the_gesture_worker_runs_a_devtools_change	fn attribution (graph::attribute), multi-view line scans, const tables	1,3,5,7
census-rule-ffi-door	contract	guards::doors::{ffi_door,declared}	∀ C: spots(Run(FFI_DOOR, C)) = sorted {f.rel:l | f a shipped product file (¬test_path, cfg(test) masked) ∧ l has `fn N(` inside an extern block ∧ (N, home?, _) ∈ FFI_DOORS with no home, or a home that is neither f.rel nor a prefix of it}. A use outside a declaration (let f = WinExec) or a same-named Rust fn is not a declaration.	guards/doors.rs::a_door_ffi_name_is_declared_in_its_one_file	extern-block tracking across lines, const table lookup	1,3,5,7
census-rule-boot-id-door	contract	guards::doors::boot_id_door	∀ C: spots(Run(BOOT_ID_DOOR, C)) = sorted {f.rel:l | f ∈ PRODUCT outside the census crate (tests included) ∧ some line of f has a SYSCTL_BY_NAME call (sysctlbyname, sysctlnametomib) on Code ∧ Text(l) holds kern.bootsessionuuid}. The name only in comments, or in a file that makes no such call (a spawned `sysctl -n` oracle), is no finding.	guards/doors.rs::the_boot_id_is_read_by_astream_alone	file-level co-occurrence of a call and a literal, substring search	1,3,5,7
census-rule-handoff-name	contract	guards::doors::handoff_name	∀ C: spots(Run(HANDOFF_NAME, C)) = sorted {f.rel:l | f under HANDOFF_SCOPE ∧ f.rel ≠ HANDOFF_HOME ∧ ¬test_path ∧ l Shipped ∧ Text(l) has a literal starting with a HANDOFF_PREFIXES prefix ("ATERM_SEAMLESS_, "ATERM_HANDOFF_) that is not the argument of a HANDOFF_DOORS call (HandoffName::new(, dev_seam!() opened on this line or the line before}.	guards/doors.rs::a_handoff_name_is_spelled_only_as_a_handoff_name	one-line look-back, substring search	1,3,5,7
census-rule-core-signal	contract	guards::doors::{core_signal,core_shape,core_at,aborts,spaced,signal_call,arguments,shell_kill,argv_signal}	∀ C, outside the census crate, a line is a finding ⇔ it ends a process by a core-dump signal (CORE_SIGNALS: ABRT/6, SEGV/11, QUIT/3, ILL/4, TRAP/5, FPE/8, BUS/7), read per language. Rust (Code): a SENDERS call (kill, killpg, raise, pthread_kill) with a core name or number, or a kill/timeout argv or shell string with -SIG, -s SIG or --signal=SIG naming one. Shell (comment lines skipped): kill, pkill, killall or [g]timeout naming one (skill does not count). Python: os.abort, abort() after `from os import`, ctypes abort, faulthandler._sig*, ctypes.string_at(0), or os.kill/signal.raise_signal/pthread_kill with a core signal or number. JS: process.abort() or process.kill(.., 'SIG<core>'). A signal stored but not sent (let quit = SIGQUIT), SIGKILL/SIGTERM/9, os._exit, and numbers passed to unrelated programs (head -n 5) never count.	guards/doors.rs::a_kill_never_names_a_core_signal	per-language line classifiers over Script and SourceFile, const tables, Option<String>	1,3,5,7
census-rule-test-appkit-door	contract	guards::graph::{test_appkit_door,appkit_door_findings,attribute,appkit_door,sends,sel_at,builds_class,reach,walk,fn_name,fn_name_at}	∀ aterm-gui source f: attribute(f) assigns each Code line to its innermost fn by brace depth over Code (string braces do not count) and marks fns inside cfg(test) modules, nested modules included. A door line sends separatorItem or setToolTip:, or builds an NSToolbarItem; selectors match exactly (toolbar:itemForItemIdentifier:… does not). With G = f's name-merged call graph (free calls, method calls, super:: paths), appkit_door_findings(f) has exactly one finding per #[test] fn t (ignored or not) such that t, or a test-only helper it calls, reaches a door line in G; its detail ends with the shortest chain t' -> … -> door, or "<door>, directly". Tests reaching no door, door names in prose or strings, and production fns are silent.	guards/graph.rs::every_test_that_reaches_a_door_is_found_with_its_chain, guards/graph.rs::production_doors_and_tests_that_touch_none_are_silent	BTreeMap<&str, Option<&str>> BFS parent links, per-line fn attribution with a depth stack, lifetimes on borrowed views	1,3,5,7
census-rule-shared-application	contract	guards::graph::{shared_application,shared_application_sends,answers_without_proof,takes_proof,param_list,top_level,is_self_receiver,is_proof_type,impl_of_proof}	∀ C over GUI_SOURCE, ∀ sites: shared_application(C, sites) has one finding per Code-line send of sel!(sharedApplication) whose innermost fn neither holds the `Windowed` proof nor is a sites pair (file, fn), detail "in `<innermost fn>`". A fn holds the proof for its whole body when a parameter's type is `Windowed` or a reference to it (any path; `Option<Windowed>` and a return type do not count), or when it is a `self` method of an impl whose self type is `Windowed`; and from a body statement on, to the end of that statement's block, when the statement is `…Windowed::get()?`, `let Some(..) = …Windowed::get() else`, or `if …Windowed::get().is_none() { return`. Naming the proof otherwise (`let _ = Windowed::get();`, an assertion, `if a && …is_none()`, a block that goes on) holds nothing. A send in an inner fn is judged by the inner fn; a nested listed fn does not cover a later send in the outer fn; a listed name in another file is not listed. Prose and strings never fire. SHARED_APPLICATION runs it over APP_SITES.	guards/graph.rs::a_send_outside_the_windowed_functions_is_found, guards/graph.rs::sends_in_the_listed_functions_prose_and_strings_are_silent, guards/graph.rs::a_proof_taken_or_answered_for_before_the_send_holds_it, guards/graph.rs::a_proof_named_but_not_answered_for_holds_nothing	fn attribution, Vec<AppSend>, header slicing	1,3,5,7
census-rule-installer-network	contract	guards::installer::{installer_network,fences,command_of,gh_call}	∀ C: Run(INSTALLER_NETWORK, C) depends only on tools/install.sh. If it is absent or defines no web_curl transport, the result is the single finding tools/install.sh:0. Otherwise one finding per non-comment line l outside the web_curl body with command_of(l) (the first command after an optional if and !) = curl without --proto, or l containing api.github.com, or gh_call(l) finding `gh api|auth|release` anywhere on the line (after $( or | included). Other scripts are never read.	guards/installer.rs::the_installer_fetches_only_through_its_transport	Script line iteration, &str prefix stripping	1,3,5,7
census-key-phrases	contract	guards::keys::{phrases,symbol}	∀ line, with R = its maximal ASCII-alphanumeric runs in order and sym(c) = the index of lower(c) in PHRASE_ALPHABET: phrases(line) = [map sym r | r ∈ R, |r| = 52, all chars in the alphabet] ++ [concat(map sym g) | g a window of 13 consecutive runs of R, each of length 4 and in the alphabet, consecutive runs separated by exactly one byte, ' ' or '-']. So a run of 51 or 53, an ill-placed i/l/o/u, an underscore or double-space separator, or a prefix character gives nothing.	guards/keys.rs::a_phrase_is_52_bounded_symbols_bare_or_grouped, guards/keys.rs::an_alphabet_in_order_reversed_or_repeated_is_a_fixture	byte loop collecting Vec<(usize,usize)>, slice windows, Option<Vec<u8>> collect, concat	1,3,5,7
census-key-synthetic	contract	guards::keys::synthetic	∀ p: &[u8] with every p[i] < 32, b = p[..|p|−1], step(x, y) = (y + 32 − x) mod 32: synthetic(p) ⇔ |b| < 2 ∨ ∃ s ∈ {0, 1, 31}, ∀ i < |b|−1: step(b[i], b[i+1]) = s. The arithmetic never overflows u8.	guards/keys.rs::an_alphabet_in_order_reversed_or_repeated_is_a_fixture, guards/keys.rs::a_phrase_is_52_bounded_symbols_bare_or_grouped	slice windows, u8 modular arithmetic, closures	1,3,5,7
census-key-file-name	contract	guards::keys::key_file_name	∀ name: key_file_name(name) ⇔ name ∈ {machine.key, master.key, release.key, signing.key, root.key} ∨ name ends with ".pkcs8".	guards/keys.rs::key_file_names	match on &str literals, ends_with	1,3
census-rule-key-material	contract	guards::keys::{key_material,gone,unreadable}, guards::{walk,skipped_dir,Corpus::read}	∀ T, the findings of key_material over Corpus::read(T) are exactly: (a) path:0 "gone" for each KEY_SURFACE file absent from T; (b) path:0 for each file of T outside skipped dirs (.git, target, target-*, target.*, node_modules, and each directory the root .gitignore keeps out by a plain directory row: `name/` anywhere, `/path/` from the root; other dot dirs are walked) whose file name satisfies key_file_name; (c) path:l for each line l of a KEY_SURFACE file, a file under crates/atpkg-keys, or a docs file, that carries some p ∈ phrases(l) with ¬synthetic(p).	guards/keys.rs::the_rule_reads_the_key_surface_and_the_whole_checkout, guards/mod.rs::the_walk_lists_dot_directories_but_reads_no_script_there	recursive fs read_dir walk, Result<_, String>, Vec<String>	1,3,5,7,8
census-rule-headless-lifeline	contract	guards::lifeline::{headless_lifeline,lifeline_findings,script_findings,launcher_script,python_launch,logical_line,armed_shell,waiver,arms_a_lifeline,arg_names_launch,names_broker,broken_launch,opens_args,closes_args,help_probe,shell_launch,program_words,flags_then_headless}	∀ C: Run(HEADLESS_LIFELINE, C) is the union of: (1) one line-0 finding "read no launch in <ANCHOR_LAUNCH_FILE>" ⇔ no launcher file read is ANCHOR_LAUNCH_FILE = crates/aterm/tests/support/headless_boot.rs naming --headless (an anchor, not a count); (2) for each launcher_source Rust file f naming --headless or "broker" (Code, comments ignored): each Rust launch (a --headless argument, or the broker verb's consecutive "link", "broker", in .arg/.args(.., a broken array included, where a broken one's broker launch is a "broker" line right below a "link", line) beyond the count of lifeline arm calls in f (launch_isolation::lifeline, arm_lifeline, Lifeline::arm) is a finding at its line, "unarmed rust launch (k --headless or link broker launch(es), m lifeline arm call(s)"; each shell launch inside a Rust string literal is a finding at its line, "unarmed shell launch", where a shell launch is a program word ($VAR, a path ending in aterm or aterm-gui), then flags, then --headless, unless the logical line carries $LIFELINE_ARGS or --lifeline-fd, is a --help probe, or has a `# NO LIFELINE <reason>` waiver with nonempty reason within WAIVER_REACH = 8 lines above; (3) for each launcher_script (tools/ or a crate's tests/, outside docs), shell launches judged the same way with comment lines skipped, and Python launches armed by lifeline.args within PY_REACH = 3 lines. Mentions in prints, asserts, string comparisons, usage text or non-launch flag arrays never fire.	guards/lifeline.rs::a_scope_that_misses_the_one_boot_is_a_finding, guards/lifeline.rs::every_unarmed_launch_is_found, guards/lifeline.rs::armed_waived_and_non_launch_shapes_are_silent, guards/lifeline.rs::a_scripts_launches_hold_a_lifeline	multi-line windows over lines, counters, &str word parsing	1,3,5,7
census-launcher-source	contract	guards::lifeline::launcher_source	∀ rel: launcher_source(rel) ⇔ (rel = "crates/" ++ k ++ "/" ++ inner with k free of '/' and inner starting tests/, examples/ or benches/) ∨ rel starts with crates/aterm-verify/src/, crates/aterm-release/src/ or crates/aterm-lab/src/. Paths under apps/ and other crate src/ trees give false.	guards/lifeline.rs::only_launcher_sources_are_read	strip_prefix, split_once, starts_with	1,3
census-rule-checked-mktemp	contract	guards::mktemp::{checked_mktemp,fused_mktemp}	∀ C: spots(Run(CHECKED_MKTEMP, C)) = sorted {s.rel:l | s a script or Rust file outside the census crate ∧ raw line l satisfies fused_mktemp ∧ l is not a Rust comment line}. Lines are raw, so a shell script inside a Rust raw string is read too. fused_mktemp(l) ⇔ in l before its first '#', some "$(cd" is followed by ≥ 1 space or tab, an optional '"', then "$(mktemp".	guards/mktemp.rs::a_scratch_directory_comes_from_a_checked_mktemp	match_indices, trim_start_matches, strip_prefix	1,3,5,7
```

## 2. Test → spec map (TSV; all 188)

`primary` is the spec whose proof retires the test. `also` lists the other specs the same test samples.
`-` in `primary` marks a test with no spec (§4).

```tsv
file	test	primary	also
guards/doors.rs	a_door_ffi_name_is_declared_in_its_one_file	census-rule-ffi-door	
guards/doors.rs	the_boot_id_is_read_by_astream_alone	census-rule-boot-id-door	
guards/doors.rs	a_handoff_name_is_spelled_only_as_a_handoff_name	census-rule-handoff-name	
guards/doors.rs	a_kill_never_names_a_core_signal	census-rule-core-signal	
guards/doors.rs	only_the_gesture_worker_runs_a_devtools_change	census-rule-devtools-door	
guards/graph.rs	a_proof_named_but_not_answered_for_holds_nothing	census-rule-shared-application	
guards/graph.rs	a_proof_taken_or_answered_for_before_the_send_holds_it	census-rule-shared-application	
guards/graph.rs	a_send_outside_the_windowed_functions_is_found	census-rule-shared-application	
guards/graph.rs	every_test_that_reaches_a_door_is_found_with_its_chain	census-rule-test-appkit-door	
guards/graph.rs	production_doors_and_tests_that_touch_none_are_silent	census-rule-test-appkit-door	
guards/graph.rs	sends_in_the_listed_functions_prose_and_strings_are_silent	census-rule-shared-application	
guards/installer.rs	the_installer_fetches_only_through_its_transport	census-rule-installer-network	
guards/keys.rs	a_phrase_is_52_bounded_symbols_bare_or_grouped	census-key-phrases	census-key-synthetic
guards/keys.rs	an_alphabet_in_order_reversed_or_repeated_is_a_fixture	census-key-synthetic	census-key-phrases
guards/keys.rs	key_file_names	census-key-file-name	
guards/keys.rs	the_rule_reads_the_key_surface_and_the_whole_checkout	census-rule-key-material	
guards/lifeline.rs	a_scope_that_misses_the_one_boot_is_a_finding	census-rule-headless-lifeline	
guards/lifeline.rs	a_scripts_launches_hold_a_lifeline	census-rule-headless-lifeline	
guards/lifeline.rs	armed_waived_and_non_launch_shapes_are_silent	census-rule-headless-lifeline	
guards/lifeline.rs	every_unarmed_launch_is_found	census-rule-headless-lifeline	
guards/lifeline.rs	only_launcher_sources_are_read	census-launcher-source	
guards/mktemp.rs	a_scratch_directory_comes_from_a_checked_mktemp	census-rule-checked-mktemp	
guards/mod.rs	a_panicking_rule_is_one_finding_not_a_crash	census-guard-run-rules	
guards/mod.rs	a_scope_no_file_lies_under_is_a_finding	census-guard-run-rules	
guards/mod.rs	the_walk_lists_dot_directories_but_reads_no_script_there	census-rule-key-material	
guards/mod.rs	calls_are_the_identifiers_before_each_paren	census-guard-calls	
guards/mod.rs	shipped_lines_drop_test_items_in_both_views	census-mask-gated-items	
guards/mod.rs	word_hits_respect_identifier_edges	census-guard-hits	
guards/rules.rs	a_device_read_to_eof_is_found_in_code_only	census-rule-dev-read	
guards/rules.rs	a_header_needs_both_lines_in_either_order	census-rule-licence-header	
guards/rules.rs	a_protected_folder_literal_is_found_in_gui_and_cli_shipped_code	census-rule-protected-folder	
guards/rules.rs	a_runloop_timer_lives_only_in_wake_timer	census-rule-runloop-timer	
guards/rules.rs	a_store_current_path_is_resolved_within_six_lines	census-rule-store-current	
guards/rules.rs	a_test_path_is_named_by_a_word_not_a_substring	census-test-path	
guards/rules.rs	a_thread_builder_and_a_grouped_std_import_are_found	census-rule-engine-platform	
guards/rules.rs	a_wall_readers_home_is_its_own_fn_in_its_own_file_for_the_wall_alone	census-rule-supervise-fx	
guards/rules.rs	an_engine_framework_named_in_a_literal_or_a_cfg_attr_link_is_found	census-rule-engine-platform	
guards/rules.rs	an_import_that_keeps_a_clocks_name_is_silent	census-rule-supervise-fx	
guards/rules.rs	an_input_probe_is_found_in_code_and_strings_not_prose	census-rule-input-probe	
guards/rules.rs	doors_the_lint_sees_and_near_misses_are_silent	census-rule-flock-door	
guards/rules.rs	every_clock_read_past_the_supervisors_door_is_found	census-rule-supervise-fx	
guards/rules.rs	every_flock_door_the_lint_cannot_name_is_found	census-rule-flock-door	
guards/rules.rs	the_door_and_the_clocks_homes_are_silent	census-rule-supervise-fx	
guards/rules.rs	the_engine_names_no_platform_in_shipped_code	census-rule-engine-platform	
guards/rules.rs	the_keeper_links_no_gui_framework	census-rule-keeper-gui	
guards/rules.rs	the_keepers_closure_holds_no_gui_crate	census-rule-keeper-gui-deps	
guards/rules.rs	the_keepers_closure_is_walked_through_cargo_lock	census-rule-keeper-gui-deps	
guards/rules.rs	the_renderers_never_name_terminal	census-rule-renderer-terminal	
guards/rules.rs	the_std_command_alias_is_built_renamed_or_re_aliased_only_in_aterm_uds	census-rule-std-command	
held_guard.rs	a_finding_is_the_line_of_the_work	census-held-guard-scan	
held_guard.rs	every_must_fire_shape_draws_a_finding	census-held-guard-scan	
held_guard.rs	every_must_silent_shape_draws_none	census-held-guard-scan	
lazy_init.rs	a_borrowed_cell_parameter_is_not_a_declaration	census-lazy-field-cell-name	
lazy_init.rs	a_longer_method_name_is_not_matched_as_a_shorter_one	census-lazy-find-method-call	
lazy_init.rs	every_reentry_shape_is_judged	census-lazy-init-verdict	census-lazy-bodyless-decl
lex.rs	lifetimes_labels_and_raw_identifiers_are_code	census-lex-spans-refine	
lex.rs	the_code_view_blanks_every_literal_and_comment_in_place	census-lex-views	census-blank-non-code
lex.rs	the_text_view_keeps_literals_and_drops_comments_byte_aligned	census-lex-views	
lib.rs	a_bodyless_declaration_does_not_swallow_the_fns_after_it	census-lazy-bodyless-decl	census-mainloop-verdict
lib.rs	a_tests_part_is_a_test_file_and_a_name_that_only_contains_it_is_not	census-test-file	
lib.rs	a_disjunctive_cfg_test_gate_is_not_masked_and_stays_red	census-mainloop-verdict	
lib.rs	cfg_predicate_entails_unshipped_only_when_it_really_does	census-cfg-entails	
lib.rs	conjunctive_cfg_test_helper_is_masked_not_graphed	census-mainloop-verdict	
lib.rs	guard_vars_captures_the_term_lock_binding	census-term-hop	
lib.rs	guard_vars_captures_the_term_lock_ui_binding	census-term-hop	
lib.rs	out_of_closure_marker_cannot_satisfy_the_registry	census-mainloop-verdict	
lib.rs	out_of_closure_marker_is_not_swept_and_is_reported	census-mainloop-verdict	
lib.rs	parse_fn_def_recognises_qualified_definitions_not_fn_bounds	census-parse-fn-def	
lib.rs	run_censuses_keeps_the_given_order_and_turns_a_panic_red	census-run-censuses	
lib.rs	run_censuses_turns_a_missed_floor_red	census-hold-floors	census-run-censuses
lib.rs	synthetic_direct_sink_call_is_red_ob6	census-mainloop-verdict	
lib.rs	synthetic_missing_boundary_def_is_red_ob3_and_missing_offload_is_red_ob4	census-mainloop-verdict	
lib.rs	synthetic_missing_root_is_red_ob2	census-mainloop-verdict	
lib.rs	synthetic_offloaded_tree_is_green	census-mainloop-verdict	
lib.rs	synthetic_prefix_resize_shape_is_red_with_path	census-mainloop-verdict	
lib.rs	term_hop_flags_synchronous_guarded_resize_both_idioms	census-term-hop	
lib.rs	term_hop_ignores_vec_and_local_terminal_resize	census-term-hop	
lib.rs	term_hop_passes_the_offloaded_shape_and_bounded_neighbours	census-term-hop	
lib.rs	top_level_comma_split_ignores_nesting_and_string_literals	census-split-commas	
lib.rs	underivable_workspace_fails_the_mainloop_census_closed	census-mainloop-verdict	
lock_order.rs	a_field_hop_is_followed_when_the_field_type_and_the_callee_both_resolve	census-lock-hold-edges	
lock_order.rs	a_field_hop_is_not_followed_when_the_callee_name_is_ambiguous	census-lock-hold-edges	
lock_order.rs	a_field_hop_is_not_followed_when_the_field_type_is_foreign	census-lock-hold-edges	
lock_order.rs	a_question_mark_receiver_still_names_its_lock	census-lock-receiver-identity	
lock_order.rs	a_return_type_must_name_a_guard_not_merely_contain_the_letters	census-return-type-guard	
lock_order.rs	a_vendored_fn_name_cannot_capture_an_aterm_one_hop_call	census-lock-hold-edges	
lock_order.rs	gated_items_end_at_their_own_shape_not_at_a_brace_count	census-mask-gated-items	
lock_order.rs	guard_final_value_distinguishes_bound_guard_from_consumed_value	census-guard-final-value	
lock_order.rs	impl_headers_resolve_to_a_nameable_self_type_or_none	census-impl-self-type	
lock_order.rs	literals_are_masked_before_token_scans	census-lock-receiver-identity	
lock_order.rs	receiver_resolution_handles_fields_accessors_and_unknowns	census-lock-receiver-identity	
lock_order.rs	self_receivers_take_the_enclosing_impl_type	census-lock-receiver-identity	
lock_order.rs	single_letter_receivers_take_the_path_they_alias	census-lock-receiver-identity	
lock_order.rs	synthetic_abba_cycle_is_red_naming_both_sites	census-lock-order-verdict	
lock_order.rs	synthetic_cfg_test_items_are_masked	census-lock-site-classification	
lock_order.rs	synthetic_cross_boundary_abba_is_red_across_the_namespace	census-lock-order-verdict	
lock_order.rs	synthetic_cross_boundary_one_hop_edge_is_graphed	census-lock-hold-edges	
lock_order.rs	synthetic_diamond_without_cycle_is_green	census-lock-hold-edges	
lock_order.rs	synthetic_drop_and_statement_scope_end_the_hold	census-lock-hold-edges	
lock_order.rs	synthetic_empty_tree_fails_closed	census-lock-order-verdict	
lock_order.rs	synthetic_file_advisory_lock_is_categorized_not_unknown	census-lock-site-classification	
lock_order.rs	synthetic_file_advisory_lock_joins_no_mutex_edges	census-lock-hold-edges	
lock_order.rs	synthetic_file_ascription_is_evidence_and_generics_are_not	census-lock-site-classification	
lock_order.rs	synthetic_file_evidence_is_fail_closed_on_rebind_and_scope	census-lock-site-classification	
lock_order.rs	synthetic_free_fn_with_standard_name_is_not_exempt	census-lock-order-verdict	
lock_order.rs	synthetic_multiline_chain_resolves_receiver_and_nests	census-lock-hold-edges	
lock_order.rs	synthetic_new_gui_dependency_is_scanned_automatically	census-lock-order-verdict	
lock_order.rs	synthetic_one_hop_interprocedural_cycle_is_red	census-lock-hold-edges	
lock_order.rs	synthetic_platform_slice_sites_are_counted_never_graphed	census-lock-site-classification	
lock_order.rs	synthetic_propagated_raw_ptr_binding_is_categorized	census-lock-site-classification	
lock_order.rs	synthetic_raw_ptr_read_joins_no_mutex_edges	census-lock-hold-edges	
lock_order.rs	synthetic_raw_ptr_reads_are_categorized_not_unknown	census-lock-site-classification	
lock_order.rs	synthetic_same_field_on_different_types_does_not_merge	census-lock-hold-edges	
lock_order.rs	synthetic_self_edge_is_red_reentrancy_suspect	census-lock-order-verdict	
lock_order.rs	synthetic_stale_helper_registration_is_red	census-lock-order-verdict	
lock_order.rs	synthetic_stale_vocabulary_registration_is_red	census-lock-order-verdict	
lock_order.rs	synthetic_tree_without_manifests_fails_closed	census-lock-order-verdict	
lock_order.rs	synthetic_try_lock_is_a_source_but_never_a_target	census-lock-hold-edges	
lock_order.rs	synthetic_tuple_field_abba_across_two_types_is_red	census-lock-hold-edges	
lock_order.rs	synthetic_unknown_identity_is_counted_never_dropped	census-lock-site-classification	
lock_order.rs	synthetic_unqualifiable_impl_types_keep_the_per_site_node	census-lock-site-classification	
lock_order.rs	synthetic_unregistered_guard_returning_helper_is_red	census-lock-order-verdict	
lock_order.rs	synthetic_vendored_identities_are_namespaced_and_never_merge	census-lock-hold-edges	
lock_order.rs	synthetic_vendored_unknown_is_summarized_per_crate	census-lock-site-classification	
lock_order.rs	synthetic_vocabulary_interior_is_audited_not_unknown	census-lock-site-classification	
scan_set.rs	a_first_party_patch_target_is_recorded_and_never_review_registered	census-scan-set-patch-registry	
scan_set.rs	a_target_table_the_target_cannot_link_is_left_out_of_its_closure	census-scan-set-closure	
scan_set.rs	a_genuine_first_party_crate_is_not_mistaken_for_a_redistribution	census-classify-patch-target	
scan_set.rs	a_patch_target_outside_vendor_and_crates_fails_closed	census-scan-set-patch-registry	
scan_set.rs	a_redistribution_under_crates_is_refused_not_called_first_party	census-classify-patch-target	
scan_set.rs	a_renamed_patch_row_is_recorded_under_the_package_it_replaces	census-scan-set-patch-registry	census-scan-set-closure
scan_set.rs	a_review_row_for_a_first_party_patch_target_is_refused	census-scan-set-patch-registry	
scan_set.rs	a_renamed_absorbed_member_is_scanned_namespaced_not_as_a_member	census-scan-set-patch-registry	
scan_set.rs	an_absorbed_patch_target_is_scanned_namespaced_not_as_a_member	census-scan-set-patch-registry	
scan_set.rs	cfg_target_path_deps_are_included_platform_independently	census-scan-set-closure	
scan_set.rs	default_features_false_suppresses_and_union_reenables	census-scan-set-closure	
scan_set.rs	dev_and_build_deps_are_excluded	census-scan-set-closure	
scan_set.rs	direct_workspace_and_dotted_path_deps_are_followed	census-scan-set-closure	
scan_set.rs	exclusion_list_is_fail_closed_both_ways	census-scan-set-patch-registry	
scan_set.rs	missing_dep_manifest_and_missing_src_fail_closed	census-scan-set-closure	
scan_set.rs	optional_deps_follow_default_feature_activation	census-scan-set-closure	
scan_set.rs	proc_macro_crates_are_classified_out_and_reported	census-scan-set-closure	
scan_set.rs	scanned_vendored_crate_missing_src_or_slice_path_fails_closed	census-scan-set-patch-registry	
scan_set.rs	the_absorbed_registry_is_fail_closed_both_ways	census-scan-set-patch-registry	
scan_set.rs	the_one_table_dependency_form_is_followed_like_the_inline_one	census-scan-set-closure	
scan_set.rs	the_review_set_is_the_fork_ledger	census-scan-set-patch-registry	
scan_set.rs	unclassifiable_dep_specs_fail_closed	census-scan-set-closure	
scope_census.rs	a_new_per_pane_owner_fails_the_closure	census-scope-verdict	
scope_census.rs	a_per_pane_constructor_fails_the_closure	census-scope-verdict	
scope_census.rs	a_per_pane_named_constructor_fails_the_closure	census-scope-verdict	
scope_census.rs	a_per_pane_word_decorations_map_fails_the_flash_limiter_chain	census-scope-verdict	
scope_census.rs	a_renamed_field_or_owner_fails_stale_rather_than_silently_passing	census-scope-verdict	
scope_census.rs	a_return_type_is_not_a_construction	census-scope-returns-token	
scope_census.rs	enum_variants_and_fn_parameters_are_not_owners	census-scope-verdict	
scope_census.rs	named_constructors_are_discovered_from_impl_blocks	census-scope-ctor-discovery	
scope_census.rs	the_supernova_aggregator_regression_goes_red	census-scope-verdict	
wasm_census.rs	mask_gates_blank_native_only_blocks_and_test_fns	census-mask-gated-items	
wasm_census.rs	synthetic_direct_sink_call_is_red_ob10	census-wasm-verdict	
wasm_census.rs	synthetic_missing_root_crate_fails_closed_ob8	census-wasm-verdict	
wasm_census.rs	synthetic_new_unregistered_reach_is_red_ob10_with_path	census-wasm-verdict	
wasm_census.rs	synthetic_reintroduced_sync_resize_is_red_ob10	census-wasm-verdict	
wasm_census.rs	synthetic_spawn_behind_a_native_target_table_is_not_in_the_closure	census-wasm-verdict	
wasm_census.rs	synthetic_spawn_in_a_file_only_a_false_mod_loads_is_not_swept	census-wasm-verdict	
wasm_census.rs	synthetic_unmasked_spawn_in_closure_is_red_ob12_and_gated_spawn_is_not	census-wasm-verdict	
wasm_census.rs	synthetic_wasm_tree_with_offloaded_resize_is_green_with_zero_findings	census-wasm-verdict	
wasm_census.rs	wasm_term_hop_flags_the_field_and_rebind_shapes	census-wasm-term-hops	
wasm_census.rs	wasm_term_hop_ignores_other_receivers_and_methods	census-wasm-term-hops	
wasm_cfg.rs	a_crate_root_is_never_left_out	census-wasm-verdict	
wasm_cfg.rs	a_module_only_a_false_gate_loads_is_no_wasm32_file	census-wasm-verdict	
wasm_cfg.rs	a_module_tree_this_reader_cannot_place_leaves_every_file_in	census-wasm-verdict	
wasm_cfg.rs	a_target_table_links_unless_it_is_false_on_wasm32	census-wasm-verdict	
wasm_cfg.rs	an_include_is_a_loader_like_a_mod	census-wasm-verdict	
wasm_cfg.rs	only_a_test_or_a_false_gate_drops_an_item	census-wasm-verdict	
wasm_cfg.rs	the_platforms_wasm32_is_not_are_false_and_its_own_are_true	census-wasm-verdict	
wasm_cfg.rs	what_the_target_does_not_settle_is_unknown	census-wasm-verdict	
```

## 3. Coverage

- 188 tests read. 181 map to one of the 66 specs; 7 sample no spec (§4).
- 40 specs are the primary spec of exactly one test, and two (`census-blank-non-code`,
  `census-run-censuses`) are primary for none: they only share tests (`also` in §2). Several of these
  are lemmas the larger specs build on (`census-test-path`, `census-guard-hits`, `census-cfg-entails`,
  `census-split-commas`), so their value is above their test count.
- The biggest collapses (primary tests): `census-lazy-init-verdict` (18), `census-lock-hold-edges` (15), `census-lock-site-classification` (11), `census-lock-order-verdict` (10), `census-mainloop-verdict` (10), `census-scan-set-closure` (9), `census-scan-set-patch-registry` (8).
- Five tests sample two specs each (`also` in §2): the lex code-view test, the missed-floor test,
  the renamed-patch-row test and the two keys phrase tests.
- Gap histogram (specs blocked by each gap): gap 1: 65, gap 2: 6, gap 3: 65, gap 4: 11, gap 5: 61, gap 7: 54, gap 8: 14, gap 11: 1, gap 13: 6.

## 4. Tests that are not spec samples

Status 2026-10-16: the six `live_tree_*` tests are deleted (with the now-unused `test_fixtures::live`).

Status 2026-10-20: `every_registry_entry_is_mirrored_in_the_synthetic_fixture` is deleted. The
fixture it policed (`synth_helper_files`, with `synth_helper_crates`) is now written out from
`GUARD_HELPERS`/`VOCABULARY_INTERIORS` themselves, so there is one list and nothing to keep in
step: a new registry entry no longer turns the synthetic lock tests red.

No test is listed here any more. The six `live_tree_*` tests this section listed are deleted,
since `verify` already runs the same census on the checkout.

## 5. Implied specs that look wrong, or disagree with the code

1. **The scope tests sample today's tree, not the census.** Seven tests in `scope_census.rs`
   (`a_per_pane_word_decorations_map_fails_the_flash_limiter_chain`, `a_new_per_pane_owner_fails_the_closure`,
   `a_per_pane_constructor_fails_the_closure`, `a_per_pane_named_constructor_fails_the_closure`,
   `a_renamed_field_or_owner_fails_stale_rather_than_silently_passing`,
   `enum_variants_and_fn_parameters_are_not_owners`, `the_supernova_aggregator_regression_goes_red`)
   copy files from the live checkout (`copy_root` from `repo_root`) and mutate pinned source text such
   as `    fx: aterm_effects::pipeline::EffectsPipeline,`, `struct WindowState {`,
   `for p in self.parked.values() {` and `pub fn for_window() -> Self {`. They fail on an unrelated
   refactor of aterm-gui or aterm-effects even when `run_scope_census_over` is correct.
   `census-scope-verdict` has to be stated and proved over synthetic claim/tree pairs before they can go.
2. **End-to-end census tests assert prose.** Many assert repair or explanation text in the log:
   "HOW TO REPAIR", "NO waiver channel", "WCAG 2.3.1", "COOPERATIVE OFFLOAD", "VACUOUS",
   "EXCLUDED from the mutex order graph", and "the DERIVED GUI-process closure (4 workspace crates"
   in `lib.rs::synthetic_offloaded_tree_is_green`. `CensusOutcome` exposes only `(ok, verdict, log)`,
   so a spec has nothing structured to quantify over except parsed log text. Recommendation: return
   typed findings (obligation, path, line, edge) beside the log, so the `census-*-verdict` specs speak
   about data. None of the prose asserts is carried into a spec.
3. **A tautology.** The first assert of
   `lock_order.rs::a_return_type_must_name_a_guard_not_merely_contain_the_letters`,
   `"GuardedInput".contains("Guard")`, is about `str::contains` and involves no aterm code. The rest of
   the test samples `census-return-type-guard`.
4. **A fixture chosen by position.** `held_guard.rs::a_finding_is_the_line_of_the_work` takes
   `MUST_FIRE[14]` (today the rustfmt-wrapped chain). Reordering `MUST_FIRE` silently changes what it
   checks. `census-held-guard-scan` states that the hit is the work's line for every shape.
5. **A stale or unintended exclusion (code, not a test).** Settled: `crates/xtask` is gone and
   `lib::marker_sweep_file` excludes only test files and this crate.
6. **The migration manifest over-counts this crate.** The chunk manifest counted 216 tests; there are
   187 (see the header). Its counter should ignore literal contents.
7. **`census-launcher-source` named a tree the code does not read.** Its first statement listed
   `crates/xtask/` as a launcher source. `launcher_source` has no such arm: `crates/xtask/src/x.rs`
   gives `in_crate = "src/x.rs"` and so false. The row now states what the code does. Whether
   xtask belongs in the scope is a question about the code, not about the spec.

## 6. Proof order (easiest high-value first)

No spec is blocked only by gap 0, so the order starts with the specs that need the least beyond
string carriers (gap 1) and widened bodies (gap 3), and then the lemmas that the rule and census
specs are built on.

1. **`census-key-file-name`** and **`census-launcher-source`** (gaps 1, 3). Equality against string
   literals, `ends_with`, `starts_with` and `split_once`, with no loops. They are the first `&str`
   specs Trust can certify once gap 1 lands, and each retires one test.
   **Re-measured on seal `150efe4a` (2026-10-16): both still blocked.** Each fn was wrapped in
   `trust_contract_fn!` with `ensures result == (<spec>) by <thm>`. Each `<thm>` was a
   `verify_island! { clean { … } }` theorem `∀ s : String, trust_import_aterm_census__<fn> s = <spec>`.
   The seal reported three refusals for each fn, the same for both:
   - **Gap 1, then gap 3 (no import).** `Clean island declaration rejected: AutoBoundHeadOverApplied
     { name: "trust_import_aterm_census__key_file_name", func_type: "fvar#9223372036854775808",
     remaining_args: 1 }`, and the same for `…__launcher_source`. E6 minted no kernel definition
     for either `&str` body: the name is an unbound free variable. Because nothing was imported, no
     negative control could be run.
   - **Gap 1 (ensures).** `compiler contract predicate was not lowered into a typed verifier formula:
     unsupported contract predicate expression` (`==` on `&str`, `ends_with`, `starts_with`,
     `split_once`), so an `unsupported_mir` row and a `SpecEnsuresUnparseable` row were added.
   - **Gap 1 (citation).** ``citation `key_file_name_is_key_names` cannot be validated because this
     clause is outside the exact statement fragment: missing supported type for clause variable
     `name` `` (`rel` for `launcher_source`).
   The fn rows in `report.json` were both Inconclusive: `key_file_name` 0 of 4 proved, `launcher_source`
   2 of 13. **A crate-wide blocker that is not gap 1 or 3** also showed up. 707 of the crate's 902 fn
   rows carry `unsupported MIR UnmodeledThreadSpawnEffect` unknowns. Two of them are on
   `key_file_name` and nine on `launcher_source`. These are "may spawn (indirect)" chains that end in
   `guards::run_rules`'s `std::thread::scope` spawn (for example `guards::keys::key_file_name →
   guards::run_guard_census → guards::run_guards → … → spawn_scoped`, and
   `core::str::<impl str>::split_once → … → spawn_scoped`). That is a Trust precision gap: the
   effect is charged to a pure callee from its callers' callable key. Even with gap 1 closed, no fn
   in this crate reaches Certified until this gap is closed. The PM-1 proc-macro boundary did not
   refuse the crate.
2. **`census-test-path`** (gaps 1, 3, 5). One quantified clause over the `'/'` and `'_'` pieces. It
   retires one test, and it is the scope lemma of six rule specs (engine-platform, renderer-terminal,
   protected-folder, runloop-timer, store-current, handoff-name).
3. **`census-key-synthetic`** (gaps 1, 3, 5, 7). `u8` modular arithmetic over a slice window, the
   closest thing here to the scalar code Trust already admits. With `census-key-phrases` it retires
   both keys phrase tests.
4. **`census-shell-word`**, **`census-scope-returns-token`** and **`census-lazy-field-cell-name`**
   (gaps 1, 3, 5). Short straight-line string predicates, one test each.
5. **`census-guard-hits`** and **`census-guard-calls`**. The word-bound and call-site lemmas behind
   every rule spec's `Match`.
   **`census-guard-calls` measured on seal `150efe4a` (2026-10-19): blocked, code unchanged, no
   test deleted.** `calls` is `&str -> Vec<&str>` (gap 1). `trust_contract_fn!`'s `-> $ret:ident`
   arm cannot even spell its result type, and `&str` has no clause type (item 1). So the spec's main
   statement was not contracted. Its scalar half, `is_word(b: u8) -> bool`, was contracted with
   `ensures result == ((b >= 48 && b <= 57) || ((b >= 65 && b <= 90) || ((b >= 97 && b <= 122) ||
   b == 95))) by is_word_is_identifier_byte`, citing a theorem over
   `trust_import_aterm_census__guards__is_word` (the import name carries the module path). The seal
   refused it: `Clean island declaration rejected: AutoBoundHeadOverApplied { name:
   "trust_import_aterm_census__guards__is_word", func_type: "fvar#9223372036854775808",
   remaining_args: 1 }`. E6 mints no import for the body `b.is_ascii_alphanumeric() || b == b'_'`.
   That is gap 3 (a call): a single-file probe of the same body shows `Alloca without an internally
   bound extent` for the `&self` receiver. Without an import, the fn row was 2 of 2 proved by
   `trust-full-verifier` (PDR, smt-backed), not by `clean-kernel-citation`. The PM-1 boundary did
   not refuse the crate. The probe (`$HOME/trust-land/seal-aterm-probe/isword/g`) gives a route for
   the scalar half: write the body as `matches!(b, b'0'..=b'9' | b'A'..=b'Z' | b'a'..=b'z' | b'_')`,
   with no behaviour change. E6 imports that body as
   `decide (48 <= b) && decide (b <= 57) || (… || decide (b = 95))`. The theorem stated in exactly
   that `<=` form closes with `simp only [import]`, and the row is Verified with every postcondition
   `clean-kernel-citation`. Mutating `b'z'` to `b'y'` makes the island fail (`h/`). The `<` form does
   not unify with the import, and rustc's lexer refuses `≤` inside `verify_island!`. The spec stays
   blocked on `calls` (gaps 1, 3, 5, 7) until `&str`/`Vec` carriers and loops land.
6. **`census-lex-spans-refine`**, then **`census-lex-views`** and **`census-blank-non-code`**. These
   are harder (a loop-heavy tokenizer, gap 7), but `Code`/`Text` are the views that every rule, lock
   and lazy-init spec is stated over, so they come before those specs.
7. **`census-cfg-entails`**, **`census-split-commas`**, then **`census-mask-gated-items`**: the
   `Shipped` lemma.
8. The pure line-scan specs: the lock-order helpers (`census-impl-self-type`,
   `census-guard-final-value`, `census-return-type-guard`, `census-lock-receiver-identity`), the
   lazy-init helpers, `census-wasm-term-hops`, `census-term-hop`, `census-cli-parse` (gap 11), then
   the `census-rule-*` specs over `Corpus::of`.
9. Last, the effectful whole-census verdicts (gap 8, plus 4 and often 13): `census-scan-set-*`,
   `census-lock-hold-edges`, `census-lock-site-classification`, `census-lock-order-verdict`,
   `census-lazy-init-verdict` (pure over `&[(path, text)]`, so it goes first of these),
   `census-held-guard-scan`, `census-wasm-verdict`, `census-mainloop-verdict`, `census-scope-verdict`
   (after §5.1), `census-run-censuses` and `census-guard-run-rules`. Factoring each census into a
   pure core over an in-memory tree, with the fs walk as a thin shell, moves most of this group off
   gap 8. Returning typed findings (§5.2) makes the verdict specs statable without parsing prose.
