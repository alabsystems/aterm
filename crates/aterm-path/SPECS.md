<!-- Copyright 2026 Andrew Yates -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# aterm-path: lexical path rules

These operations use only their arguments and the target platform's lexical `Path` rules.
They do not inspect the filesystem, resolve mount aliases or expand Windows short names.
The Windows system-uppercase and NTFS live oracles remain in
`aterm-sysprobe/src/paths_tests.rs`. None of these specs is proved.

## 1. Specs (TSV)

```tsv
id	kind	functions	statement	subsumes	difficulty	blocking gaps
PATHS-KEY	refinement	windows_key, windows_under, key_under, fold_case	Windows drive and UNC spellings normalize to comparison keys with simple single-unit uppercase; verbatim paths retain their literal separator and component rules. Relative, device, dot-component and short-name spellings that cannot be placed return None. Containment compares whole components; different roots return None rather than a claim about mount aliases. Non-UTF-8 and drive-relative spellings are refused. Ordinary tail names ending in a dot or space are refused; verbatim names are read literally. Some(false) means non-containment on the same root; unreadable keys return None. fold_case leaves non-BMP scalars unchanged. For a BMP scalar it uses one-scalar uppercase only when that fits one UTF-16 unit, retaining full-uppercase expansions unchanged except for the static Greek simple-uppercase mappings; it reads no live system case table.	tests::every_spelling_of_one_directory_keys_alike, tests::a_unc_path_keys_by_its_server_and_share, tests::two_roots_are_not_related_without_a_table, tests::a_verbatim_path_is_read_literally, tests::what_cannot_be_placed_answers_none, tests::case_folds_as_ntfs_upcases_a_name, tests::case_folds_by_the_simple_uppercase, tests::under_is_component_by_component, tests::a_short_name_is_not_placed	Unicode scalar iteration, strings, Option	0,1
PATHS-SPELLING	refinement	short_spelled, plain	short_spelled splits a path only when short-name components are the only reason windows_key refuses it, preserving the lead, verbatim spelling through the last short name, and remaining components. plain removes only a recognized verbatim drive or UNC prefix for display. Malformed prefixes and other spellings are returned unchanged by plain; neither operation expands or resolves a short name.	tests::only_a_path_refused_for_its_short_names_is_short_spelled, tests::plain_drops_only_a_verbatim_drive_or_unc_prefix	Path, Cow, borrowed slices, strings	0,1
PATHS-PLATFORM	refinement	is_under	On Windows is_under uses windows_under; on other targets it returns the lexical Path::starts_with result.	tests::is_under_takes_the_platforms_rule	Platform cfg, Path	0,1
```
