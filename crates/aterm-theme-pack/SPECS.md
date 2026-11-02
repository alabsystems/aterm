<!-- Copyright 2026 Andrew Yates -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# aterm-theme-pack: specs behind its unit tests and its build script

Specs recovered from `src/tests.rs` and `build.rs`, over the data in `contrib/themes/pack/`. Each
is held by what its `subsumes` column names (TP-FOLDER and TP-PORTS by `build.rs`, which refuses
the build); none is proved yet.

## 0. Notation used in the statements

- `T` := `themes()`, the embedded rows. `t.text` is the shipped file, `up(t)` the vendored
  `upstream/<t.upstream>` bytes, `L(f)` := `licence_text(f)`, `C` := the collection's licence.
- `parse` := `aterm_types::scheme::parse_theme_str`. `derive(t)` := `derive(t, up(t),
  [L(f) | f ∈ t.licence_files], C)`.
- An admitted licence is one in `ADMITTED_LICENCES` (`src/policy.rs`).

## 1. Specs (TSV)

`difficulty` is the Rust features a proof must handle; `blocking gaps` is a row number of
`docs/AUDIT-trust-for-test-migration-2026-10-15.md` §7 or, in words, what else a proof lacks.

```tsv
id	kind	functions	statement	subsumes	difficulty	blocking gaps
TP-DERIVED	data-law	derive, notices	∀ t ∈ T: derive(t) = Ok(t.text); and ATTRIBUTION.md = notices(). Negative control: derive over up(t) with `palette = 1=` changed ≠ derive(t).	src/tests.rs::pack_files_are_derived_from_their_upstream	MEDIUM: string building over the native converter; the law is over a closed table.	file reads at test time
TP-FOLDER	data-law	(manifest, build.rs::unlisted)	Every entry of contrib/themes/pack/ is README.md, manifest.tsv, ATTRIBUTION.md, a `<t.id>.conf`, an `upstream/<t.upstream>`, or a LICENSES/ file named by a row, the collection or a built-in port's source. A stray file refuses the build, naming it.	build.rs (build-time refusal)	TRIVIAL: set difference.	directory listing
TP-NOTICE	data-law	carries_notice	∀ t ∈ T, f ∈ t.licence_files: carries_notice(t.text, t.licence, L(f)) ∧ carries_notice(t.text, "MIT", C). Negative control: deleting one notice line falsifies it.	src/tests.rs::every_pack_file_carries_its_notices	LOW: ordered subsequence over lines.	0
TP-PAIRS	data-law	find, scheme::builtin_theme	∀ t ∈ T with t.pair = Some(p): p is a pack id q with q.pair = Some(t.id), or a built-in; and the two appearances differ.	src/tests.rs::pairs_are_symmetric_and_cross_appearance	TRIVIAL: table lookups.	0
TP-PORTS	data-law	port_licence_file, build.rs::port_licence_file, scheme::builtin_theme_list	Every built-in with a source has an admitted licence and the licence file `LICENSES/<spdx>-<repo>.txt`, which the pack embeds; a built-in without a source is a signature theme or Default/Default Light. Derived from the built-ins, never listed.	build.rs (build-time refusal)	TRIVIAL: table lookups.	0
TP-IDS	data-law	parse	∀ t ∈ T: no built-in is named t.id or t.name; parse(t.text) is Ok with appearance declared = t.appearance, author = t.author and name = t.name.	src/tests.rs::pack_ids_are_free_and_files_declare_their_row	LOW: the native parser over constants.	0
TP-FIND	contract	find, withdrawn	find(s) is the t ∈ T with t.id = s ignoring ASCII case, else the one with t.name = s ignoring ASCII case; no id or name answers for two themes (build.rs refuses that, ignoring ASCII case). withdrawn(s) is the pack id or built-in that replaces s (matched ignoring ASCII case), an id a release shipped that is no row's id or name now. Negative control: a built-in's name finds nothing.	src/tests.rs::find_answers_the_id_then_the_name_shown	TRIVIAL: table lookups.	0
TP-INSTALL	io-contract	install, state_in	install(t, None, _) fails; Absent → Wrote with bytes = t.text; Installed → Unchanged; Modified → Exists without replace, Wrote with; a case variant → Failed(case_collision_message); a built-in's name → Refused and nothing written. An earlier copy (t.text but for its `name =` line) is Installed: Unchanged without replace, Wrote with; one other byte changed (a color) is Modified.	src/tests.rs::install_round_trips_in_a_scratch_folder, src/tests.rs::an_earlier_copy_reads_installed_and_an_edit_does_not	HIGH for a proof (filesystem); a test.	filesystem model
```

## 2. Test → spec map (TSV)

```tsv
file	test	primary	also
src/tests.rs	pack_files_are_derived_from_their_upstream	TP-DERIVED	
src/tests.rs	every_pack_file_carries_its_notices	TP-NOTICE	
src/tests.rs	pairs_are_symmetric_and_cross_appearance	TP-PAIRS	
src/tests.rs	pack_ids_are_free_and_files_declare_their_row	TP-IDS	
src/tests.rs	install_round_trips_in_a_scratch_folder	TP-INSTALL	
src/tests.rs	install_refuses_an_id_that_leaves_the_requested_folder	TP-INSTALL	
src/tests.rs	an_earlier_copy_reads_installed_and_an_edit_does_not	TP-INSTALL	
src/tests.rs	find_answers_the_id_then_the_name_shown	TP-FIND	
```

## 3. Coverage

- The strict-check law over the pack lives in `aterm-cli`
  (`theme_author.rs::pack_themes_pass_the_strict_check`), where the checker is; the catalog round
  trip lives in `aterm-gui` (`settings_themes_host.rs::pack_themes_load_through_the_catalog`).
- `build.rs` also refuses (panics on) a malformed manifest: an inadmissible licence, an id
  `validate_user_theme_name` rejects, a duplicate id or name, a name that is its row's id, a
  withdrawn id still in use or standing for no pack id or built-in, a bad appearance, an empty
  field, or a missing upstream or licence file. These are build-time checks, not tests.

## 5. Open finding

TP-INSTALL leaves out that `install` refuses an id `validate_user_theme_name` rejects (`Refused`,
nothing written), which `install_refuses_an_id_that_leaves_the_requested_folder` holds; §2 maps that
test to TP-INSTALL, but the row's `subsumes` does not name it.

## 6. Proof order (easiest, high value first)

1. TP-PAIRS, TP-PORTS, TP-FIND: closed-table lookups; a `const` evaluation in the build script could
   discharge them.
2. TP-NOTICE, TP-IDS: need the native parser's spec (aterm-types) first.
3. TP-DERIVED and TP-FOLDER read files, and TP-INSTALL writes them; they stay tests.
