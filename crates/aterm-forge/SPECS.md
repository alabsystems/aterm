<!-- Copyright 2026 Andrew Yates -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# aterm-forge: spec recovery from its 184 unit tests

> Status 2026-10-16, after the merge with the tests-and-gates audit: the rows naming 125 tests
> it deleted and 1 spec whose functions it deleted are gone, and the spec rows no longer cite
> those tests. Counts elsewhere in this file were taken before.

> Status 2026-10-16: this file is the crate's migration worklist. Each of the 63 specs below
> is proved through Trust/Clean about the compiled Rust function, and the proof's commit deletes the
> tests it subsumes. The 19 tests marked **delete** in §4 sample no spec and can go now; the
> 6 marked **becomes-spec-after-fix** wait on a production fix. Nothing has been proved or deleted yet.

Source: `crates/aterm-forge` (read only). Paths below are relative to that directory. I read every
`#[test]` (`src/attest.rs` 28, `src/blame.rs` 12, `src/budget.rs` 22, `src/check.rs` 20, `src/cli.rs` 6,
`src/deny.rs` 5, `src/direct_vendor.rs` 11, `src/dominator.rs` 7, `src/loc.rs` 18, `src/measured.rs` 1 (since deleted, OF-42),
`src/policy.rs` 14, `src/provenance.rs` 7, `src/resolve.rs` 19, `src/survey.rs` 9,
`tests/red_fixtures.rs` 5 = 184) and the code each one reaches. Nothing was compiled or run; the
claims come from reading the code. The plan is `docs/PLAN-tests-to-specs-2026-10-15.md`; gap numbers
are the rows of `docs/AUDIT-trust-for-test-migration-2026-10-15.md` §7.

Result: **63 specs**, covering **159 tests**. **25 tests are not spec samples** (§4):
19 to delete, 6 that become specs once a production gap is fixed.

## 0. Notation used in the statements

- `chars(x)`: the number of Unicode scalar values in `x`. `lines(x)`: `x.split('\n')` with a trailing
  empty piece dropped. `W_LINE` = 96, the report width constant.
- Graph `g = (root, nodes, edges: PkgId → set of PkgId)`. `Path(g,u,v)`: a finite edge walk `u → … → v`.
- `Reach(g,B)` = `{v | v = root, or some walk root → v visits no vertex (root included) equal to B}`;
  empty when `B = Some(root)`. This is `model::Graph::reach`.
- `WF(s)` for a `CellSurvey s`: `s.graph.nodes = Reach(s.graph, None)`, every edge endpoint is in `nodes`,
  `dom(s.facts) = nodes`, and `facts[root].is_third_party = false`. `measure` + `parse_tree` establish it.
- `TP(s)` = `s.third_party()`: the nodes `p` with `facts[p].is_third_party`, in `BTreeSet` order.
  `FP(s,id)`: `facts[id]` exists and `¬is_third_party`.
- `Fall(s,t)` = `Reach(g,None) \ Reach(g,Some(t))`: what drops out of the graph if `t` goes.
- `Class(v)`: the semver line of `v` (`m` for major `m > 0`; `0.m` for `0.m`, `m > 0`; else `0.0.p`).
  `caret(r)`: cargo's caret range for `r`; `=r` means every stated component is equal.
- `spec(id)` = `name ++ "@" ++ version`. `Lines(log) ∋ X`: some line of the log contains `X`.
- `OB-n`: the obligation of that number in `src/attest.rs` (1–10) and `src/check.rs` (11–18).
- A **fixture tree** is any directory the statement quantifies over. Statements that read fs, git or
  cargo are about the pure core over those observations; proving them needs the core factored out of
  the effect (gap 8), which is the first refactor for every row whose gaps include 8.
- **Gap 0** (installed seal publishes no Lean-discharged row) blocks every spec; it is listed in each
  row so the histogram is complete. No spec is blocked by gap 0 alone: gap 1 (non-scalar carriers) is
  in all but one row, because every forge function takes or returns `String`, `Vec`, `Path` or a struct.

## 1. Specs (TSV)

Columns: `id`, `kind`, `functions`, `statement`, `subsumes` (tests; primary and secondary, see §2),
`rust features the proof must handle`, `blocking gaps` (audit §7 numbers).

```tsv
id	kind	functions	statement	subsumes	rust features	blocking gaps
dom-cost-is-counterfactual-reach	contract	model::Graph::reach, dominator::dom, dominator::dom_against	∀ s, t: let F = {id ∈ Fall(s,t) | ¬FP(s,id)}, ascending. Then dom(s,t) = DomCost{pkgs = |F|, loc = Σ_{id∈F} facts.get(id).map_or(0, loc), also = F \ {t}}, and `also` is strictly ascending. Corollaries: t ∉ Reach(g,None) ⇒ dom(s,t) = DomCost::default(); a third-party t ∈ Reach gives pkgs ≥ 1; WF(s) ⇒ dom(s,root) = (|TP(s)|, third_party_loc(s)); a node that stays reachable without t (a shared child) is never in F.	dominator::a_shared_dependency_is_not_billed_to_either_parent, dominator::blocking_the_root_costs_the_whole_third_party_surface, dominator::a_first_party_node_under_a_third_party_parent_is_not_billed_to_it	BTreeSet/BTreeMap of String-carrying structs, while-loop DFS over a Vec stack, set-difference iterator, Option combinators and closures	0,1,3,5,6,7,13
ranked-is-sorted-permutation	contract	dominator::ranked	∀ s: ranked(s) is a permutation of [(id, dom(s,id)) | id ∈ TP(s)], sorted by (cost.loc descending, id ascending), hence the unique such ordering. No root or first-party node appears; every element of TP(s) appears exactly once.	-	Vec collect, sort_by with a closure comparator, BTreeSet iteration	0,1,3,5,6,7,13
dominator-shape	invariant	dominator::ranked, dominator::dom_against, model::CellSurvey::third_party_loc	∀ s with WF(s), ∀ (id,c) ∈ ranked(s): 1 ≤ c.pkgs; |c.also| = c.pkgs − 1; id ∉ c.also; c.also strictly ascending; c.loc ≤ third_party_loc(s). Also |ranked(s)| = |TP(s)|.	-	composition of dom-cost-is-counterfactual-reach over a Vec; sums over BTreeMap	0,1,3,4,5,6,7,13
dominator-laminar-partition	invariant	dominator::ranked, survey::partition_check, survey::ranked_table	∀ s with WF(s), let R = ranked(s) and Roots = {(id,c) ∈ R | id ∉ ∪_{(j,d)∈R, j≠id} d.also}. The sets {id} ∪ c.also for (id,c) ∈ Roots are pairwise disjoint with union TP(s); hence Σ_Roots c.loc = third_party_loc(s), Σ_Roots c.pkgs = |TP(s)|, and partition_check always takes the OK branch. Lemma: for third-party a ≠ b the two sets are nested or disjoint (laminarity).	-	graph-theoretic lemma over BTreeSet reachability, sums, filter closures	0,1,3,4,5,6,7,13
blame-count-paths	contract	blame::count_paths, blame::walk, blame::parents_of	∀ acyclic g, t: count_paths(g,t) = (min(N, u64::MAX), N > u64::MAX), N = the number of distinct edge walks root → t. So count_paths(g,root) = (1,false) and an unreachable t gives (0,false). On a cyclic g the call terminates and the count is ≤ the number of simple paths.	blame::every_path_is_counted_and_the_shortest_come_first	recursion with &mut BTreeMap memo and &mut BTreeSet guard, overflowing_add	0,1,3,5,6,7
blame-shortest-paths	contract	blame::shortest_paths, blame::depths	∀ g, t, k: k = 0 ∨ t ∉ nodes ∨ t unreachable ⇒ (vec![], false). Otherwise, within SEARCH_BUDGET pops, the result is (P, false): min(k, #simple root→t paths) distinct simple paths, each from root to t, in nondecreasing length, and every simple path not in P is at least as long as P's last. When the budget trips the flag is true and P is a prefix of that order.	blame::every_path_is_counted_and_the_shortest_come_first	BinaryHeap<Reverse<(usize, Vec<PkgId>)>>, VecDeque BFS, nested loops, Vec::contains	0,1,3,5,6,7
patch-state-classification	contract	blame::patch_state, blame::read_forks, policy::in_row_scope	∀ root, facts, forks, id: patch_state returns the first that applies: (1) FirstPartyVendored{dir_line(root,dir)} if facts.root_dir = Some(dir) ∧ is_first_party_vendored(root,dir); (2) Registry if forks has no rows for id.name; (3) Fork{path} for the first row r with r.version = Some(id.version); (4) for the first row r with in_row_scope(r.renamed, r.version, id.version): UnpatchedBesideFork{r.path, v} if r.version = Some(v), else ForkVersionUnreadable{r.path}; (5) Registry. read_forks(root) maps each path row of [patch.crates-io] to patched_package(key,row) ↦ VendorFork{path, version = the [package] version of <root>/<path>/Cargo.toml, renamed = (name ≠ key)}.	-	enum result, Option chains, BTreeMap<String, Vec<_>>, fs read plus aterm_toml parse	0,1,3,6,8,12
blame-spec-parse	contract	blame::run (request prefix)	∀ pkg: if pkg contains '@' and splits as (n,v) with n = "" ∨ v = "", run(root,pkg,cells) = Err(e) with e ∋ "name@version", before any fs or process effect, for every root and cells. Otherwise the request is (n, Some(v)) when pkg = n@v and (pkg, None) when pkg has no '@'. The proof needs this prefix factored into a pure fn.	-	&str split_once, format!, early return	0,1,3,6
blame-not-found-refusal	contract	blame::run, blame::not_found, blame::lock_versions, blame::nearby	∀ surveys S, request (n, v?): let found = {id ∈ ∪ nodes | id.name = n ∧ (v = None ∨ id.version = v)}. found = ∅ ⇒ run = Ok(Outcome{ok: false, log}) where Lines(log) ∋ "NOT RESOLVED" and the name of every surveyed cell, and every branch prints "Fix:". If moreover v = Some(w) and some surveyed node is named n, log ∋ "IS resolved, but not at w" followed by exactly those versions.	-	writeln! String building, BTreeSet, fs read of Cargo.lock, cargo-tree process behind survey_cells	0,1,3,4,5,6,7,8,13
blame-report-contents	contract	blame::run, blame::versions_block, blame::liveness_block, blame::package_block, blame::entry_points	∀ surveys S and requests with found ≠ ∅: ok = true. Each found id gets a versions row whose tag is fixed by patch_state(id): FORKED <path> | UNPATCHED | fork? | registry | first-party (vendored). Each surveyed cell holding id gets a "cell C   dom " line with dom(s,id), a path count from count_paths, and a "first-party edges responsible:" line listing entry_points. "PATCH LIVENESS DEFECT" is printed ⇔ some found id has patch_state(root,None,forks,id) = UnpatchedBesideFork. Under report-line-width's preconditions every line has ≤ W_LINE chars.	-	composition of the graph specs, String formatting, BTreeMap iteration, cargo-tree effect	0,1,3,4,5,6,7,8,13
versions-row-layout	contract	blame::versions_row, survey::fit	∀ name, version, tag, cells with chars(version) ≤ 26: versions_row(...) = ("    " ++ pad28(fit(fit(name, 26 − chars(version)) ++ " " ++ version, 27)) ++ pad32(fit(tag,31)) ++ cells).trim_end(). Hence version occurs in the row, the char before fit(tag,31) is ' ' when tag ≠ "", and chars(row) ≤ 64 + chars(cells).	-	format! width padding, saturating_sub, chars counting	0,1,3,6
report-line-width	invariant	survey::wrap, blame::write_path	∀ out, indent, text, width: wrap appends exactly the lines of a greedy word fill of each '\n'-paragraph of text; each appended line L (trim_end'd) has chars(L) ≤ max(width, chars(indent) + w), w = the longest word on L; words keep their order, so split_whitespace(text) = the concatenated words of the lines. write_path(out, indent, path) emits the segments "name version" joined by " -> ", in order, each line with chars ≤ max(W_LINE, chars(indent) + 2 + chars(seg) + 3) for its longest seg.	-	String push, nested loops over split_whitespace, chars().count()	0,1,3,5,6,7
fit-char-truncation	contract	survey::fit	∀ s, w: chars(s) ≤ w ⇒ fit(s,w) = s; chars(s) > w = 0 ⇒ fit = ""; chars(s) > w > 0 ⇒ fit = first (w−1) chars of s ++ "…". Hence chars(fit(s,w)) = min(chars(s), w), the result is valid UTF-8, and it never panics.	survey::fit_truncates_by_chars_and_never_splits_one	chars iterator take/collect into String	0,1,3,6,7
fit-path-suffix	contract	survey::fit_path, blame::dir_line	∀ p, w ≥ 2, parts = p.split('/'): chars(p) ≤ w ⇒ fit_path(p,w) = p. Otherwise, for the least start ∈ 1..|parts| with chars(join(parts[start..],'/')) + 2 ≤ w, fit_path = "…/" ++ join(parts[start..],'/') (the longest '/'-suffix that fits, a true suffix of p); if none exists, fit_path = fit(p,w). Always chars(fit_path(p,w)) ≤ w. dir_line(root,dir) = fit_path(display(dir.strip_prefix(root) or dir), W_LINE − 16).	-	Vec<&str> from split, slice join in a loop, Path strip_prefix/display	0,1,3,5,6,7
commas-grouping	contract	survey::commas	∀ n: u64, d = n.to_string(): commas(n) is d with ',' inserted before each index i ∈ 1..|d| with (|d| − i) mod 3 = 0. Equivalently: deleting ',' gives d, the first group has 1–3 digits, every later group exactly 3.	-	u64 to_string, char loop with is_multiple_of, String push	0,1,3,5,6,7
survey-reports-live-totals	contract	survey::cell_section, survey::json_report, survey::json_cell, survey::run	∀ s with WF(s), ∀ top: cell_section(out,s,top) emits a line starting "  third-party   " ++ commas(third_party_loc(s)) ++ " LOC   ", the line "  packages      " ++ commas(|nodes|) ++ " resolved   " ++ commas(|nodes| − |TP|) ++ " workspace   " ++ commas(|TP|) ++ " THIRD-PARTY", and the "PARTITION CHECK OK", "ZERO-COST LEAVES" and "DUPLICATE VERSIONS" sections. json_report(root,[s],skipped) is well-formed JSON with "third_party": |TP| and "third_party_loc": third_party_loc(s), and exactly |TP| "dom_pkgs" members whatever top is. run(root,cells,top,json) writes json_report to the json path, and its log is the concatenation of these sections. Under report-line-width's preconditions every line has ≤ W_LINE chars.	-	large String builders, composition with ranked/commas/jstr, fs write, cargo-tree effect for run	0,1,3,4,5,6,7,8,13
cell-select	contract	resolve::select, check::run, survey::run, blame::run	∀ cells, names: names = [] ⇒ select(cells,names) = Ok(cells). If some name matches no cell, select = Err(e) for the first such w, with e ∋ w, every c.name, and "--cell". Otherwise Ok of the named cells, deduplicated in first-occurrence order. check::run, survey::run and blame::run each call select(default_cells(), cells) before any other effect and propagate its Err, so an unknown cell is could-not-run, never a verdict.	-	Vec<Cell> with String fields, find/any closures, format!	0,1,3,5,6,7,13
tree-line-parse	contract	resolve::parse_line, resolve::parenthesised, model::PkgId::spec	∀ line l: l.trim() = "" ⇒ Ok(None). Otherwise Ok(Some(TreeLine{depth, id: PkgId(name, ver), path, is_proc_macro, deduped})) ⇔ l.trim_end() = D ++ ws* ++ name ++ " v" ++ ver ++ (" " ++ A)*, where D is a nonempty digit string parsing as usize = depth, name and ver are nonempty and space-free, and A is a sequence of groups "(X)" each closing at the first ')' followed by end of text or " (". X = "*" sets deduped, X = "proc-macro" sets is_proc_macro, any other X sets path = X (last wins). Every other nonblank line is Err; a missing depth prefix gives an Err ∋ "--prefix depth". ver is kept verbatim (build metadata included), and spec(id) = name ++ "@" ++ ver.	-	byte scanning, splitn, strip_prefix, byte-indexed &str slicing, while loop over annotations	0,1,3,5,6,7
tree-depth-stack	contract	resolve::parse_tree	∀ text, L = its parsed nonblank lines: parse_tree(text) = Ok ⇔ every line parses ∧ L ≠ [] ∧ L[0].depth = 0 ∧ ∀ i: L[i].depth ≤ L[i−1].depth + 1 ∧ every depth-0 line has the same id. Then root = L[0].id, nodes = {l.id}, edges = {(P(i), L[i].id) | L[i].depth > 0} with P(i) the id of the latest j < i with L[j].depth = L[i].depth − 1 (a " (*)" line is a real edge), and paths maps each id to the first path printed for it. A depth jump gives Err ∋ "depth jumped"; no depth-0 line gives Err.	resolve::the_depth_stack_reconstructs_parents	Vec stack truncate/push, BTreeMap entry, loop over lines with early Err	0,1,3,5,6,7
strip-verbatim-prefix	contract	resolve::strip_verbatim_prefix	∀ PathBuf p: if p's first component is Prefix(VerbatimDisk(_)), the result is p's text without its leading `\\?\`; otherwise it is p, byte-identical. Off Windows no Prefix component exists, so it is the identity.	-	std::path Component/Prefix match, to_string_lossy, cfg(windows)	0,1,3,6
abs-root-resolution	contract	resolve::abs_root, resolve::graph_and_paths	∀ root: graph_and_paths(root,cell,log) depends on root only through strip_verbatim_prefix(canonicalize(root)), so two spellings of one directory (relative, or containing "..") give the same graph. canonicalize fails ⇒ Err(e) with e ∋ root.display() and "FIX:". On success abs_root's result has no `\\?\` prefix and contains Cargo.toml when root does.	-	fs canonicalize, targo tree process spawn	0,1,8
offline-first-resolution	contract	resolve::graph_and_paths, resolve::run_tree	∀ cargo, root, cell: graph_and_paths runs `tree -p c.package --target c.triple` with `--offline` first. Offline success ⇒ that graph, log unchanged. Offline failure ⇒ exactly one run without `--offline`: its success is that graph with a note appended to log; its failure is Err naming both errors.	resolve::an_offline_miss_retries_online_once_and_notes_it	process spawn, String log	0,1,8
cli-parse-grammar	contract	cli::parse, cli::next, cli::ParseError::message	parse is total over finite argument sequences and fixed by this grammar. A prefix may hold only `--root V`. Help (-h/--help/help in the prefix; -h/--help after the verb) gives Ok(Help) with the root seen so far. No verb gives NoVerb; a '-' token before the verb gives UnexpectedArg; the first other token is the verb. After it, --root V, --cell V (appended in order), --top N (N: usize, else BadNumber(N)), --json P, --update and --allow-regress V are accepted in any order; a flag missing its value gives MissingValue(flag), any other '-' token UnexpectedArg, and the last bare token is the operand. Verbs: survey ⇒ Survey{cells, top (default 40), json}; blame ⇒ Blame{operand, else MissingOperand("a package name"), cells}; budget ⇒ Budget{update, allow_regress}; attest; check{cells}; anything else ⇒ UnknownVerb, whose message names survey, blame, budget, attest and check. root is the last --root from either position.	-	generic IntoIterator<Item = String>, Peekable, match on &str in loops, enums with String payloads	0,1,3,6,7,11
deny-satisfies	contract	deny::satisfies, attest::parse_version, attest::Ver::triple	∀ req, v: satisfies(req,v) = None if parse_version(v) = None, or if some comma-separated trimmed part of req fails to parse as op ++ ws ++ ver (op = the first of <=, >=, <, >, = that matches; parse_version(ver) ≠ None). Otherwise Some(⋀_parts cmp_op(triple(v), triple(ver))) under lexicographic (major, minor, patch) order. Pre-release and build suffixes are ignored; ^ and ~ forms are None.	deny::comparators_compose_with_commas	&str split/trim/strip_prefix, find_map closure, tuple comparison, ? on Option	0,1,3,5,6,7
deny-findings	contract	deny::findings	∀ policy, graph, annotations: the result is two lists concatenated. (1) For each ban in order, scan nodes ascending, only those named ban.name. If ban.version = Some(r) ∧ satisfies(r, pkg.version) = None, push BadRequirement{ban} and stop that ban. If the node matches (no requirement, or satisfies = Some(true)), let via = [parent | (parent, children) ∈ edges, pkg ∈ children, parent.name ∉ ban.wrappers] in key order, and push Banned{pkg, via, ban} ⇔ pkg = root ∨ via ≠ []. (2) For each (pkg, note) in annotation order, push Source{pkg, note} ⇔ ¬(note.is_absolute() ∧ note.is_dir()) ∧ no g ∈ allow_git is a prefix of note.	deny::a_ban_names_every_parent_that_is_not_a_wrapper, deny::a_version_requirement_bans_only_the_versions_it_matches, deny::a_source_is_a_directory_or_an_allowed_git_prefix	nested loops with break, BTreeMap/BTreeSet iteration, closures capturing ban, Path::is_dir effect	0,1,3,5,6,7,8,13
deny-parse-policy	contract	deny::parse_policy	∀ text: parse_policy(text) = Ok(Policy{bans, allow_git}) ⇔ text is TOML and every element of bans.deny is an inline table with a string name. bans holds, in order, Ban{name, version = string 'version' if any, wrappers = the string elements of 'wrappers' or [], reason = string 'reason' or ""}; allow_git = the string elements of sources.allow-git, or []. Otherwise Err.	deny::the_policy_reads_bans_wrappers_and_git_sources	aterm_toml DocumentMut navigation, filter_map closures, Vec<String>	0,1,3,6,7,12,13
spdx-license-arms	contract	attest::license_arms	∀ expr, T = the whitespace tokens of expr with '(' and ')' replaced by ' ': license_arms(expr) = T in order, minus every OR, AND and WITH, and minus the one token after each WITH.	attest::license_arms_unfold_or_and_and_with	replace, split_whitespace driven by hand in a while-let loop, Vec<String>	0,1,3,5,6,7
apache-mandatory	contract	attest::apache_is_mandatory	As implemented: apache_is_mandatory(expr) ⇔ every piece of expr.split(" OR ") has "Apache-2.0" ∈ license_arms(piece). Intended: ⇔ every license choice satisfying the SPDX expression (OR/AND/WITH precedence, parentheses) includes Apache-2.0. They agree on parenthesis-free expressions; prove the intended one (§5.2).	attest::apache_is_only_mandatory_when_no_other_arm_exists	split into Vec<&str>, all/any closures, composition with license_arms	0,1,3,4,5,6,7,13
req-satisfied-caret-exact	contract	attest::req_satisfied, attest::parse_version	∀ req, v, r = req.trim(): r contains ',' or '*', or starts with '>' or '<' ⇒ None; parse_version(r without a leading '^'/'=') or parse_version(v) fails ⇒ None. r starts with '=' ⇒ Some(major equal ∧ (minor equal ∨ unstated) ∧ (patch equal ∨ unstated)). Otherwise Some(floor(r) ≤ triple(v) < upper(r)) with upper(r) = (M+1,0,0) if M > 0; (1,0,0) if minor unstated; (0,m+1,0) if m > 0; (0,0,p+1) if patch stated; else (0,1,0): cargo's caret rule.	attest::caret_requirements_follow_cargos_zero_major_rules	&str predicates, Option ?, u64 +1 (overflow at u64::MAX), tuple order	0,1,3,6
manifest-requirement-scanners	contract	attest::workspace_requirements, check::dev_dependency_oracles	Over M = [root/Cargo.toml] ++ sorted root/crates/*/Cargo.toml that parse as TOML, both return sorted, deduplicated (repo-relative manifest, requirement) pairs. workspace_requirements(root,n) reads [workspace.dependencies], [dependencies], [dev-dependencies] and [build-dependencies] and takes the entry keyed n. dev_dependency_oracles(root,n) reads [dev-dependencies] and every [target.*.dev-dependencies] and takes every entry whose package field (else key) is n. An entry contributes its string value, or the string version of a table entry with no path, git or workspace key.	check::the_oracle_scanner_finds_plain_and_renamed_holders	fs read_dir/read_to_string, aterm_toml TableLike trait objects (&dyn), nested loops, sort/dedup	0,1,3,6,7,8,12
notice-line-parse	contract	attest::parse_notice	∀ text: parse_notice yields, in order, for each 1-based line i whose trim is "- " ++ H ++ "(`" ++ D ++ "/`)" with D.trim() starting vendor/ or crates/, H.trim() splitting at its first ", " into (NV, license), and NV splitting at its last ' ' into (name, version): NoticeEntry{name, version, license, dir: D.trim(), line: i}, all fields trimmed. Every other line contributes nothing.	attest::notice_lines_parse_into_name_version_license_and_dir	lines().enumerate(), split_once/rsplit_once/strip_suffix chains, Vec of String structs	0,1,3,5,6,7
header-notice-predicate	contract	attest::has_modification_notice	∀ text: has_modification_notice(text) ⇔ some line among the first HEADER_LINES (20), lowercased, contains "aterm" and at least one of "modif", "local patch", "change", "added by".	attest::a_header_notice_is_recognized_and_a_buried_one_is_not	lines().take(20).any(closure), to_ascii_lowercase, contains	0,1,3,5,6,7,13
pristine-diff	contract	attest::diff_against_pristine	∀ trees P, F, Files(F) = every regular file under F relative to F, skipping entries named .cargo-ok, .git or target at every level: if every listing and every read of an F file succeeds, the result is Some(Divergence{modified, added}) with modified = sorted {r ∈ Files(F) | P/r readable ∧ bytes(P/r) ≠ bytes(F/r)} and added = sorted {r ∈ Files(F) | P/r unreadable}; otherwise None.	-	fs read_dir/read, explicit Vec stack walk, byte-vector equality	0,1,3,6,7,8
attest-verdict-is-conjunction	contract	attest::report, attest::run, attest::partition_patches, attest::survey_forks	∀ fixture trees root: report(root) = (fails = 0, log), where fails = Σ OB-1..OB-10 over the patch table partitioned by classify_patch_target into vendored forks, first-party targets and absorbed crates. Every OB-1..OB-10 tag occurs in every log. fails = 0 ∧ unverified = [] ⇒ the log ends "PASS — 10 obligations held"; fails = 0 ∧ unverified ≠ [] ⇒ it ends "PASS, [OB-7] UNVERIFIED" naming those forks, and ok still holds unless could_not_run; fails > 0 ⇒ it ends "FAILED" ++ PRECISION_NOTE. An unreadable patch table is a single OB-1 failure. run(root) = Err ⇔ root/Cargo.toml is not a file.	attest::a_member_without_a_pristine_copy_is_unverified_never_ticked	fs and git check-ignore effects, String log building, composition of the OB functions	0,1,3,4,6,7,8,12,13
attest-structure-obligations	contract	attest::ob1_patch_vendor_agreement, attest::ob2_version_equality, attest::ob3_workspace_stub, attest::ob8_marker_census, attest::ob10_gitignore, attest::vendor_gitlinks	∀ fixture trees, each adds ≥ 1 failure with the quoted text, and 0 otherwise. OB-1: a vendor/<d> that no patch row, applicable roster row (path_dependants ≠ []) or validated astream submodule claims ("no reviewed source classification"), and a vendor/astream that direct_vendor::review refuses (its message, e.g. "`.gitmodules` declares no submodule"). OB-2: one failure per in-scope workspace requirement (in_row_scope(renamed, version, req)) with req_satisfied = Some(false), one per such requirement with None ("requirement form attest does not model"), and a failure when no source-less Cargo.lock entry exists ("DID NOT TAKE") or the source-less versions exclude the fork version. OB-3: a readable vendored manifest without an empty [workspace] table. OB-8: a vendored fork with zero TRUST_MARKER + LOCAL_PATCH_MARKER ("ZERO"). OB-10: a git-ignored path under a vendored fork; paths inside a vendor/<d> recorded as a gitlink are never swept, and d is reported as "`vendor/d` is a git submodule".	attest::renamed_forks_still_refuse_unmodelled_workspace_requirements, attest::an_unreviewed_vendor_directory_still_fails_the_astream_submodule_check, attest::the_ignore_sweep_does_not_descend_into_a_submodule_working_tree	fs walks, git subprocess, aterm_toml, composition with req_satisfied/in_row_scope	0,1,3,4,6,7,8,12,13
attest-license-obligations	contract	attest::ob5_license_files, attest::ob6_notice_agreement, attest::ob7_apache_modification_notices, attest::ob9_spdx_allowlist, attest::pristine_dir, attest::deny_allow	∀ fixture trees, Red = vendored forks ∪ absorbed crates, the absorbed being the crates/ patch targets classify_patch_target calls Absorbed and the renamed members absorbed_members finds (each NOTICE list entry under crates/ that no patch row targets, named by its NOTICE name; OB-1 fails one that is not a crate with sources, and OB-2 does not apply to it). OB-6 fails ⇔ NOTICE cannot be read, or its parsed entries omit some f ∈ Red, list f more than once, disagree with f on version, SPDX or directory, or name an entry matching no f ∈ Red (vendored forks and absorbed crates, including renamed members); a crates/ patch target with no NOTICE line is not absorbed, so OB-1 refuses it as "not under vendor/". OB-9 fails ⇔ some license_arms(f.license) is outside deny.toml [licenses] allow (naming it). OB-7 applies only when apache_is_mandatory(f.license): with a pristine copy (vendor/.forge/<name>/pristine, else the registry) it fails per modified or added file r lacking has_modification_notice and reports "k modified file(s), j added"; without one it reports UNVERIFIED ("cannot be diffed"), never a silent pass: no ✓ [OB-7] line, `Attestation::unverified` names the fork and the verdict says "PASS, [OB-7] UNVERIFIED" (`check` counts it as a note and its GREEN line says "provenance attested except [OB-7] for <names>"). OB-5 requires a LICENSE*/LICENCE* file in each f ∈ Red.	attest::an_apache_only_fork_needs_header_notices, attest::an_absorbed_crate_owes_the_license_obligations, attest::a_renamed_absorbed_member_owes_the_license_obligations, attest::a_member_without_a_pristine_copy_is_unverified_never_ticked, check::a_green_verdict_names_the_forks_ob7_could_not_diff	fs reads, CARGO_HOME/HOME env vars, composition with parse_notice/license_arms/apache_is_mandatory/diff_against_pristine/has_modification_notice	0,1,3,4,6,7,8,12,13
classify-patch-target-locations	contract	aterm_census::scan_set::classify_patch_target	∀ name, rel, root: classify_patch_target(name,rel,root) = Ok(Vendored) ⇒ rel starts "vendor/"; Ok(FirstParty | Absorbed) ⇒ rel starts "crates/". A spec of aterm-census that forge relies on: prove it there, cite it here.	-	cross-crate &str/Path predicates, fs probes	0,1,8,12
budget-parse-grammar	contract	budget::parse, budget::bad	∀ text: parse(text) = Ok(rows) ⇔ every non-whitespace-only line, in order, does not start with '#', has 3 or 4 '\t'-fields, has nonempty scope = f0.trim() and metric = f1.trim(), has f2.trim() parsing as u64, has f3 (if present) not blank, and a (scope, metric) unique so far; rows are those Rows in file order. Otherwise Err(e) for the first offending line n, e starting "tools/forge-budget.tsv:n:" and containing the shape "<scope>", with the reason of the first failed check in this order: comment ("comments are not supported"); a single field of ≥ 3 whitespace words ("TABS, not spaces"); field count ("found k"); empty scope or metric; non-u64 ("`x` is not a whole number"); blank 4th column; duplicate.	-	lines().enumerate(), split into Vec<&str>, u64 parse, linear duplicate search	0,1,3,5,6,7,13
budget-render-parse-roundtrip	round-trip	budget::render, budget::Row::line, budget::parse	∀ rows whose scopes and metrics are nonempty, trimmed and tab/newline-free, whose reasons are tab/newline-free and not blank, and whose (scope, metric) pairs are distinct: parse(render(rows)) = Ok(rows). Conversely ∀ canonical text (no blank lines, trimmed fields, every line '\n'-terminated) with parse(text) = Ok(rows): render(rows) = text.	-	format!/push_str over Vec<Row>, composition with budget-parse-grammar	0,1,3,5,6,7
budget-validate-reason	contract	budget::validate_reason	∀ update, reason: the first that applies: reason = None ⇒ Ok(None); ¬update ⇒ Err naming "--update"; r.trim() empty ⇒ Err (the reason is empty); r contains '\t' or '\n' ⇒ Err mentioning tab; else Ok(Some(r)).	budget::a_tab_in_the_reason_is_refused_because_the_file_is_tsv	Option<&str>, trim, contains, format!	0,1,3,6
budget-ratchet-plan	contract	budget::plan, budget::verdict, budget::over_message	∀ rows, live, update, reason, p = plan(rows, live, update, reason), and per row i with c its ceiling and v = live.get(scope, metric): v = None ⇒ unchanged, p.ok = false, a problem ∋ live.unavailable[scope] (or "not measured"), live cell "?". v < c ⇒ under update the ceiling becomes v (note "LOWERED c -> v") and regress_reason is kept; without update unchanged (note "slack c−v"). v = c ⇒ unchanged. v > c ⇒ under update ∧ reason = Some(r) the ceiling becomes v, regress_reason r, and the row counts in p.raised; otherwise unchanged, p.ok = false, and the problem is over_message ∋ "(v−c) over the ceiling of c", the metric's scope detail list, "git diff -- Cargo.lock" and "--allow-regress". p.changed ⇔ some ceiling changed. Invariant: a ceiling rises only under update ∧ reason ≠ None, and only to the live value. Missing rows: for each armed (scope, metric) ∈ seed_shape(live) with v = live.get(scope, metric) ≠ None and no row, p.ok = false, a problem "MISSING" ∋ the line "scope\tmetric\tv", a table row "RED (no row)", and nothing is added under update either. p.ok ⇔ no unmeasured row ∧ no unaccepted over row ∧ no missing armed row.	budget::a_missing_armed_row_is_red_and_an_unarmed_one_advisory, budget::a_raise_without_a_reason_is_red_and_writes_nothing, budget::an_unmeasured_scope_is_never_a_pass	iter_mut over Vec<Row> with &mut field updates, BTreeMap keyed by (String, String), String formatting	0,1,2,3,5,6,7
budget-validate-metric	contract	budget::validate_metric	∀ row, live: validate_metric(row, live) = Ok(()) ⇔ (scope = "lock" ∧ metric ∈ LOCK_METRICS) ∨ (scope = "patch" ∧ metric ∈ PATCH_METRICS) ∨ (scope = "shipped." ++ t ∧ scope ∈ live.scopes ∧ metric ∈ SHIPPED_METRICS). Otherwise Err: an unknown shipped scope names default_cells; an unknown metric names itself and lists the known ones.	-	&str strip_prefix, slice contains, join	0,1,3,6,7
budget-detail-lists	contract	budget::detail	∀ root, s, T = TP(s): vendored_measured = [spec(p) ++ " (" ++ display(dir.strip_prefix(root)) ++ ")" | p ∈ T, facts[p].root_dir = Some(dir), dir under root/vendor], in T order; biggest = the first 5 of [(spec(p), loc)] sorted by loc descending, then spec ascending; build_scripts and proc_macros = the specs in T with has_build_rs / is_proc_macro; duplicates = duplicate_names(s) rendered "n (v1, v2…)". Since measure only sets root_dir to an existing directory, each shown dir is real.	-	iterator filter/map chains with closures, sort_by, truncate, Path strip_prefix	0,1,3,5,6,7,13
absent-ledger-is-empty	contract	budget::load, policy::load	∀ root: reading root/tools/forge-budget.tsv fails NotFound ⇒ budget::load(root) = Ok([]); reading root/vendor/forge.toml fails NotFound ⇒ policy::load(root) = Ok(Policy::default()) (no forks, no cells). Any other read error ⇒ Err; a successful read ⇒ parse(text).	-	fs read_to_string, io::ErrorKind match	0,8
check-parse-patch-table	contract	check::parse_patch_table, policy::patched_package	∀ text, whence: non-TOML ⇒ Err; no [patch.crates-io] ⇒ Ok(([], [])). Otherwise forks = [PatchEntry{name: patched_package(key,item), key, path} | entries with a string path] sorted by (name, key), and other = the sorted keys of the entries without one.	check::a_git_or_version_patch_is_not_counted_as_a_path_fork, check::a_renamed_row_is_read_as_the_package_it_names	aterm_toml TableLike iteration, sort_by with then_with	0,1,3,6,7,12
check-parse-carve-ledger	contract	check::parse_carve_ledger	∀ text: non-TOML ⇒ Err; no 'carved' key ⇒ Ok([]); 'carved' neither an array of tables nor an array ⇒ Err ∋ "[[carved]]"; an array element that is not an inline table ⇒ Err. Otherwise Ok of the rows in order as Carved{path, reason or ""}, provided every row has a string path p ≠ "" that is not absolute and has no '/'-segment "..". The first row i (1-based) with no path ⇒ Err ∋ "no `path` key" and "path = "; the first row that escapes the tree ⇒ Err ∋ "REPO-RELATIVE".	check::the_carve_ledger_reads_both_the_table_and_the_inline_form	aterm_toml array/array_of_tables, Vec<(Option<String>, String)>, Path::is_absolute, split	0,1,3,6,7,12
check-classify-row-scope	contract	check::classify, check::canon, policy::in_row_scope	∀ graph, paths, name, scope, with canon(d) = d for a nonexistent d: classify returns (live, sibs), live ⇔ ∃ id ∈ nodes, id.name = name ∧ canon(paths[id]) = scope.want; sibs = [(id, paths.get(id)) | id ∈ nodes in order, id.name = name, paths[id] absent or (canon(paths[id]) ≠ want ∧ canon(paths[id]) ∉ scope.others), in_row_scope(scope.renamed, scope.version, id.version)].	check::classify_separates_the_fork_from_its_unpatched_siblings, check::a_renamed_row_answers_for_its_own_version_line_only	BTreeMap<PkgId, PathBuf>, fs canonicalize, filter closures, Vec contains	0,1,3,5,6,7,8
check-oracle-verdict	contract	check::oracle_verdict	∀ lock: oracle_verdict(lock) = Escapes(v) for the first (v, true) in lock; Captured if no entry has has_source = true (including lock = []).	check::a_registry_sibling_is_what_separates_an_escaped_oracle_from_a_captured_one	slice find with closure, String clone	0,1,3,6,7,13
check-verdict-obligations	contract	check::run, check::report_over, check::ledger_findings, check::read_carve_ledger	∀ fixture workspaces root, selected cells C: report_over(root,C).ok ⇔ all of: OB-1..OB-10 — attest::report(root) green, the transcript carrying "[OB-1..OB-10]" and any attest failure (e.g. "NOTICE does not list fork `f`"); OB-11 — every vendored path fork has a `[[fork]]` block in vendor/forge.toml (read by aterm_census::scan_set::reviewed_forks) with the same path and no first-party target has one (a failure names the fork, "NO `[[fork]]` block" and vendor/forge.toml); OB-12 — every selected cell resolves (else could-not-run) its own c.package for c.triple, and in every cell classify finds no in-scope sibling (a failure names "UNPATCHED `n`", n@v and "targo tree --target … -i n@v"; every cell is scored as "cell `c`"); a patch row live in no cell's normal graph is green only when it is live, with no in-scope sibling, in the first cell's `normal,build` closure (else DEAD PATCH, or could-not-run when that closure does not resolve); OB-13 — every carved path is absent (a failure says "<path> … EXISTS again" and quotes its reason); OB-14 — the ratchet holds; OB-15 — no oracle is Captured; OB-17 — ledger_findings is empty (a shipped browser root with no wasm cell of its own, or a [[fork]] whose path, version, license or apache_notice differs from the tree, names the root, row or field; the census class is the census's own input from the same block, so nothing compares it); OB-18 — deny findings are empty. Every OB tag is reported, a RED report carries PRECISION_NOTE, and the verdict is a function of the tree alone.	red_fixtures::a_reinstated_carved_module_reds_the_forge_verb, red_fixtures::an_unreviewed_patch_entry_reds_the_forge_verb, red_fixtures::a_notice_that_omits_a_registered_fork_reds_the_forge_verb, red_fixtures::a_ledger_that_disagrees_with_the_tree_reds_the_forge_verb, red_fixtures::an_unpatched_sibling_version_reds_the_forge_verb, check::patch_liveness_is_judged_in_every_cell_at_its_own_root	cargo tree subprocess, fs, git, composition of every other spec in the crate, large String transcript	0,1,3,4,6,7,8,12,13
astream-validate-ordered-conjunction	contract	direct_vendor::validate, declared_url, gitlink, checked_out_at, pin_did_not_move_back, excluded, resolved_from_submodule, consumer_reaches_submodule	∀ root, facts: validate(root,facts) = Ok(Pin{commit, url, crates}) ⇔ all of the following; otherwise Err of the first failing check in this order. (1) Exactly one submodule.<n>.path with trim_end('/') = vendor/astream, whose last url ∈ {URL, URL.git} (0 matches ⇒ "declares no submodule"; k > 1 ⇒ "k times"; a wrong url names it and "git submodule sync"). (2) The index is exactly [e] with e.path = vendor/astream, mode 160000, stage 0, e.object 40 or 64 hex digits = commit (entries beneath ⇒ "tracks k path(s) beneath" and "git rm -r --cached"; empty ⇒ "records nothing"; wrong mode ⇒ "mode m … not as a gitlink"). (3) checkout = At{head = commit, modified = []} (Uninitialised ⇒ "not an initialised"; another head ⇒ "git submodule update --init vendor/astream" and "commit the bump"; modifications ⇒ "k tracked modification"). (4) motion ∉ {Backward{declared: None}, BaseNotFetched}; Backward names the base, ANCESTOR, "forward only", submodule.recurse and the trailer; BaseNotFetched names "never fetched", "fetch origin" and the trailer; every other motion passes. (5) Some exclude entry, with "./" and trailing '/' trimmed, is nonempty and a component-prefix of vendor/astream. (6) metadata is Ok (an Err propagates verbatim). (7) Each package of the metadata whose name starts "astream-" or whose manifest_path is under root/vendor/astream resolves exactly once, at root/vendor/astream/crates/<n>/Cargo.toml, with null source, license Apache-2.0, not a workspace member; Pin.crates is those names, sorted and deduplicated. (8) aterm-link is at root/CONSUMER and its astream-broker and astream-cap dependencies have path root/vendor/astream/crates/<n>.	direct_vendor::an_astream_crate_no_list_names_is_held_to_the_submodule_too	enum matching with guards, slice patterns, aterm_json Value navigation, PathBuf equality, ? chaining	0,1,3,6,12
astream-pin-motion-decision	contract	direct_vendor::pin_motion, rollback_reason, is_ancestor	pin_motion is this decision over git observations (ancestor and merge-base as in the repositories): pin = gitlink(index), else Unchanged. refs/remotes/origin/main (the full ref, never a branch spelled origin/main) unresolved ⇒ Unchanged. merge-base(HEAD, base) fails ⇒ Undecided. base = the mode-160000 vendor/astream entry of ls-tree(merge-base), absent ⇒ Unchanged. base = pin ⇒ Unchanged. pin not in the submodule store ⇒ Undecided; base not held ⇒ BaseNotFetched{base}. Else base ⪯ pin ⇒ Forward; pin ≺ base ⇒ Backward{base, declared}; incomparable ⇒ Diverged; git error ⇒ Undecided. declared = the first line, over the full messages of merge-base..HEAD, starting "astream-pin-rollback:" with a nonempty trimmed remainder (the reason), at any depth of the branch; indented, quoted or empty mentions declare nothing. Needs a pure decision core over the observations.	direct_vendor::the_pin_is_judged_against_origin_main_at_the_merge_base_with_real_git, direct_vendor::a_stale_checkout_that_never_fetched_the_base_is_caught_not_skipped, direct_vendor::no_gitlink_at_the_merge_base_means_no_earlier_pin_to_fall_behind, direct_vendor::a_local_branch_named_origin_main_is_not_the_base	git subprocesses and exit codes, NUL-separated parsing, String find_map	0,1,3,6,8
physical-lines	contract	loc::physical_lines	∀ b: &[u8]: physical_lines([]) = 0; otherwise #{i | b[i] = '\n'} + (b.last() ≠ '\n' ? 1 : 0) = |lines(b)|.	-	&[u8] iter filter count, last()	0,1,3,5,6,7
count-unsafe-tokens	contract	loc::count_unsafe, loc::is_ident_byte	∀ b: &[u8]: count_unsafe(b) = #{i | b[i..i+6] = "unsafe" ∧ (i = 0 ∨ ¬ident(b[i−1])) ∧ (i+6 = |b| ∨ ¬ident(b[i+6]))}, ident(x) ⇔ x is ASCII alphanumeric or '_'.	-	while loop with windows().position, usize index arithmetic, slice indexing	0,1,3,5,6,7
manifest-field-readers	contract	loc::manifest_facts, loc::package_name, loc::package_version	∀ text: non-TOML ⇒ manifest_facts = ("", false), package_name = "", package_version = None. Otherwise manifest_facts = (string package.license or "", bool lib.proc-macro or false); package_name = string package.name or ""; package_version = Some(string package.version), and an inherited `version.workspace = true` (a table) gives None.	loc::manifest_facts_read_licence_and_the_proc_macro_flag	aterm_toml parse and Item accessors, Option chains	0,1,3,6,12
package-dir-precedence	contract	loc::package_dir, loc::workspace_members, loc::patch_targets, loc::registry_srcs	∀ root, id, printed: package_dir returns the first that applies: (1) Members[(name, version)], Members mapping (manifest [package] name, literal version else [workspace.package] version) to its dir for each crates/* member; (2) Patches[(name, version)] if a directory, Patches mapping (patched_package(key,row), manifest_version(root/path)) to root/path; (3) <src>/<name>-<version> for the first registry src where that is a directory; (4) root/crates/<name> if a directory; (5) printed if a directory; (6) None. Branches (1)–(3) are version-exact (but see §5.4 on branch 4). The memo caches are functions of root.	-	OnceLock<Mutex<BTreeMap>> memo with Box::leak, fs probes, env vars, aterm_toml	0,1,3,6,8,12
third-party-iff-not-first-party	contract	loc::measure, loc::facts_with_paths, provenance::is_first_party	∀ root, id, printed, d = package_dir(root,id,printed): d = None ⇒ measure = PkgFacts{is_third_party: true, root_dir: None, ..0}; else is_third_party = ¬is_first_party(root,d) and root_dir = Some(d). This depends only on d, not on dependants: a registry package reached only from a first-party vendored crate is third-party. facts_with_paths(root,g,printed)[id] = measure(root, id, printed.get(id)) ∀ id ∈ g.nodes.	-	fs walk, memo cache, composition with package-dir-precedence and provenance-classifier	0,1,3,4,6,8
provenance-classifier	contract	provenance::is_first_party_vendored, is_first_party, roster_row, applies_to	∀ root, dir, rel: is_first_party_vendored(root,dir) ⇔ ∃ r ∈ FIRST_PARTY_VENDORED, dir component-prefixed by root/r.dir. is_first_party(root,dir) ⇔ dir component-prefixed by root/crates ∨ is_first_party_vendored(root,dir). roster_row(rel) = the first r with rel = r.dir or rel starting r.dir ++ "/", so a sibling like vendor/astream-broker never matches. applies_to(root) ⇔ root/crates/aterm-forge/src/provenance.rs is a file.	-	Path::starts_with (component-wise), static slice iteration, format!, fs is_file	0,1,3,5,6,8
roster-well-formed	table-property	provenance::FIRST_PARTY_VENDORED	∀ r ∈ FIRST_PARTY_VENDORED: r.dir starts "vendor/", does not end '/', contains exactly one '/'; r.upstream ≠ ""; |r.why| > 80. Decidable by evaluating the const.	-	const &'static [struct of &'static str], str predicates	0,1,5,6
path-deps-and-normalize	contract	provenance::path_deps, provenance::normalize	∀ manifest: path_deps(manifest) = in order, for each occurrence index i of "path", the text between the next two '"' when the rest matches ws* '=' ws* '"' value '"'. normalize(p) folds components left to right: CurDir dropped, ParentDir pops, others pushed (lexical '.'/'..' resolution, no fs).	provenance::path_values_are_read_out_of_a_manifest_and_relative_parts_resolve	match_indices, strip_prefix chains, PathBuf push/pop over components	0,1,3,5,6,7
path-dependants	contract	provenance::path_dependants	∀ root, rel: path_dependants(root,rel) = the sorted, deduplicated file names m of entries under root/crates whose readable Cargo.toml has some p ∈ path_deps(text) with normalize(root/crates/m/p) component-prefixed by root/rel; [] when root/crates is unreadable or none qualifies.	-	fs read_dir/read_to_string, composition with path_deps/normalize, sort/dedup	0,1,3,6,7,8
ledger-parse-fail-closed	contract	policy::parse, header, fork_block, check_keys, CensusMode::parse	∀ text: policy::parse(text) = Ok(Policy{forge, forks}) ⇔ text is TOML; top-level keys ⊆ {forge, fork, carved} (an unknown k ⇒ Err naming k and pointing to a '#' comment); [forge] exists (else Err ∋ "[forge]" and "loc_method") with keys ⊆ {loc_method, graph_method, cells}, loc_method = LOC_METHOD and graph_method = GRAPH_METHOD (else Err quoting both), and cells a nonempty array of inline tables with exactly the string keys name, triple, package; every [[fork]] has keys ⊆ {name, version, path, license, apache_notice, census} (else Err naming the key and "allowed keys are"), string name, version, license, and path ∈ vendor/*, apache_notice a bool defaulting to false, census a table with mode = "scanned" (else "\"m\" is not a mode") and a string namespace (else Err naming census.namespace); fork names pairwise distinct (a duplicate is named). forks keep file order with the fields as written.	-	aterm_toml DocumentMut/TableLike trait objects, loops with early Err, enum parse	0,1,3,6,7,12
semver-row-scope	contract	policy::semver_class, known_semver_class, in_row_scope	semver_class(v) strips leading chars from ^=~<> and space, cuts at the first of - + , or space, then gives major if major ≠ "0"; "0." ++ minor if minor ≠ "0"; "0.0." ++ patch if a patch exists; else "0.0". known_semver_class(r) = Some(semver_class(n)) ⇔ r.trim() with an optional leading ^ or = is n and n's core (before - or +) is 1–3 dot-separated canonical decimal u64s; else None. in_row_scope(renamed, row, v) = ¬renamed ∨ row = None ∨ known_semver_class(row) = None ∨ known_semver_class(v) = None ∨ the classes are equal. Required soundness, which the code violates today (§5.1): known_semver_class(r) = Some(c) ⇒ every version satisfying caret(r) has Class = c.	policy::a_semver_class_is_the_version_line_cargo_resolves_within, policy::only_a_renamed_row_with_a_known_version_is_scoped_to_its_line	&str trim_start_matches with closure, split on char arrays, Vec<&str>, byte predicates	0,1,3,5,6,7,13
patch-entries-measure	contract	policy::patch_entries, patched_manifest, PatchEntry::is_live, lock_entries	∀ fixture roots: patch_entries(root) returns, in manifest order, one PatchEntry per [patch.crates-io] row that is a table with keys ⊆ {path, package} and a string path (any other row shape ⇒ Err), with kind = classify_patch_target and (name, version, license) from the patched manifest: a Vendored target states them literally; a FirstParty or Absorbed target may inherit key k (`k.workspace = true`) from root [workspace.package] k; the manifest name must equal patched_package(key,row). lock_version = the source-less lock version equal to manifest_version, else the first source-less one; shadowed_by = the registry lock versions v with in_row_scope(renamed, manifest_version, v); is_live ⇔ lock_version = Some(manifest_version).	-	fs reads, aterm_toml, closures capturing &doc, nested loops over Vec<LockEntry>	0,1,3,6,8,12
```

### Distribution by kind (63)

| kind | count | ids |
|---|---|---|
| contract | 57 | dom-cost-is-counterfactual-reach, ranked-is-sorted-permutation, blame-count-paths, blame-shortest-paths, patch-state-classification, blame-spec-parse, blame-not-found-refusal, blame-report-contents, versions-row-layout, fit-char-truncation, fit-path-suffix, commas-grouping, survey-reports-live-totals, cell-select, tree-line-parse, tree-depth-stack, strip-verbatim-prefix, abs-root-resolution, cli-parse-grammar, deny-satisfies, deny-findings, deny-parse-policy, spdx-license-arms, apache-mandatory, req-satisfied-caret-exact, manifest-requirement-scanners, notice-line-parse, header-notice-predicate, pristine-diff, attest-verdict-is-conjunction, attest-structure-obligations, attest-license-obligations, classify-patch-target-locations, budget-parse-grammar, budget-validate-reason, budget-ratchet-plan, budget-validate-metric, budget-detail-lists, absent-ledger-is-empty, check-parse-patch-table, check-parse-carve-ledger, check-classify-row-scope, check-oracle-verdict, check-verdict-obligations, astream-validate-ordered-conjunction, astream-pin-motion-decision, physical-lines, count-unsafe-tokens, manifest-field-readers, package-dir-precedence, third-party-iff-not-first-party, provenance-classifier, path-deps-and-normalize, path-dependants, ledger-parse-fail-closed, semver-row-scope, patch-entries-measure |
| invariant | 3 | dominator-shape, dominator-laminar-partition, report-line-width |
| round-trip | 2 | jstr-json-string-roundtrip, budget-render-parse-roundtrip |
| table-property | 1 | roster-well-formed |

### Gap histogram (specs blocked by each audit §7 gap)

| gap | specs blocked | meaning |
|---|---|---|
| 0 | 63 | installed seal publishes no Lean-discharged row |
| 1 | 62 | non-scalar carriers and content models |
| 2 | 1 | `&mut self` views and two-state clauses |
| 3 | 59 | E6 body widening (calls, matches, loops) |
| 4 | 11 | callee-ensures → caller composition |
| 5 | 33 | quantified clauses |
| 6 | 60 | plain-Rust clause publication |
| 7 | 46 | loops |
| 8 | 21 | effects (fs, process, env) |
| 11 | 1 | generics |
| 12 | 15 | cross-crate theorem citation (`aterm_toml`, `aterm_census`, `aterm_json`) |
| 13 | 20 | closures, threads, async |

Fewest blockers: `absent-ledger-is-empty` {0,8}; `abs-root-resolution` {0,1,8};
`roster-well-formed` {0,1,5,6}; `classify-patch-target-locations` {0,1,8,12}; and the pure
`{0,1,3,6}` group `blame-spec-parse`, `versions-row-layout`, `strip-verbatim-prefix`,
`req-satisfied-caret-exact`, `budget-validate-reason`.

### Proof dependency order

`fit-char-truncation`, `commas-grouping`, `physical-lines`, `count-unsafe-tokens`, `spdx-license-arms`,
`semver-row-scope`, `req-satisfied-caret-exact`, `deny-satisfies` (leaf string and arithmetic
functions) → `fit-path-suffix`, `versions-row-layout`, `report-line-width`, `jstr-json-string-roundtrip`
(report formatting) → `tree-line-parse` → `tree-depth-stack` (establishes the edge relation `WF` is
stated over) → `dom-cost-is-counterfactual-reach` → `ranked-is-sorted-permutation` → `dominator-shape` →
`dominator-laminar-partition`; `blame-count-paths`, `blame-shortest-paths` beside it. The TOML readers
(`check-parse-*`, `ledger-parse-fail-closed`, `deny-parse-policy`, `manifest-field-readers`) need gap 12
for `aterm_toml`. The effectful compositions (`package-dir-precedence` → `third-party-iff-not-first-party`;
the `attest-*` and `astream-*` specs) → `survey-reports-live-totals`, `blame-*-refusal/contents` →
`check-verdict-obligations` last, since it composes every other spec.

## 2. Test → spec map (TSV)

`primary` is the spec that retires the test. `also` lists the other specs the same test samples. The
tests of §4 are deleted and have no row.

```tsv
file	test	primary	also
src/attest.rs	apache_is_only_mandatory_when_no_other_arm_exists	apache-mandatory	
src/attest.rs	license_arms_unfold_or_and_and_with	spdx-license-arms	
src/attest.rs	caret_requirements_follow_cargos_zero_major_rules	req-satisfied-caret-exact	
src/attest.rs	notice_lines_parse_into_name_version_license_and_dir	notice-line-parse	
src/attest.rs	a_header_notice_is_recognized_and_a_buried_one_is_not	header-notice-predicate	
src/attest.rs	renamed_forks_still_refuse_unmodelled_workspace_requirements	attest-structure-obligations	
src/attest.rs	an_unreviewed_vendor_directory_still_fails_the_astream_submodule_check	attest-structure-obligations	
src/attest.rs	the_ignore_sweep_does_not_descend_into_a_submodule_working_tree	attest-structure-obligations	
src/attest.rs	an_apache_only_fork_needs_header_notices	attest-license-obligations	
src/blame.rs	every_path_is_counted_and_the_shortest_come_first	blame-shortest-paths	blame-count-paths
src/budget.rs	a_raise_without_a_reason_is_red_and_writes_nothing	budget-ratchet-plan	
src/budget.rs	a_tab_in_the_reason_is_refused_because_the_file_is_tsv	budget-validate-reason	
src/budget.rs	an_unmeasured_scope_is_never_a_pass	budget-ratchet-plan	
src/budget.rs	a_missing_armed_row_is_red_and_an_unarmed_one_advisory	budget-ratchet-plan	
src/check.rs	a_git_or_version_patch_is_not_counted_as_a_path_fork	check-parse-patch-table	
src/check.rs	the_carve_ledger_reads_both_the_table_and_the_inline_form	check-parse-carve-ledger	
src/check.rs	classify_separates_the_fork_from_its_unpatched_siblings	check-classify-row-scope	
src/check.rs	a_renamed_row_answers_for_its_own_version_line_only	check-classify-row-scope	
src/check.rs	a_renamed_row_is_read_as_the_package_it_names	check-parse-patch-table	
src/check.rs	a_registry_sibling_is_what_separates_an_escaped_oracle_from_a_captured_one	check-oracle-verdict	
src/check.rs	the_oracle_scanner_finds_plain_and_renamed_holders	manifest-requirement-scanners	
src/check.rs	patch_liveness_is_judged_in_every_cell_at_its_own_root	check-verdict-obligations	
src/deny.rs	a_ban_names_every_parent_that_is_not_a_wrapper	deny-findings	
src/deny.rs	a_version_requirement_bans_only_the_versions_it_matches	deny-findings	
src/deny.rs	a_source_is_a_directory_or_an_allowed_git_prefix	deny-findings	
src/deny.rs	the_policy_reads_bans_wrappers_and_git_sources	deny-parse-policy	
src/deny.rs	comparators_compose_with_commas	deny-satisfies	
src/direct_vendor.rs	an_astream_crate_no_list_names_is_held_to_the_submodule_too	astream-validate-ordered-conjunction	
src/direct_vendor.rs	the_pin_is_judged_against_origin_main_at_the_merge_base_with_real_git	astream-pin-motion-decision	
src/direct_vendor.rs	a_stale_checkout_that_never_fetched_the_base_is_caught_not_skipped	astream-pin-motion-decision	
src/direct_vendor.rs	no_gitlink_at_the_merge_base_means_no_earlier_pin_to_fall_behind	astream-pin-motion-decision	
src/direct_vendor.rs	a_local_branch_named_origin_main_is_not_the_base	astream-pin-motion-decision	
src/dominator.rs	a_shared_dependency_is_not_billed_to_either_parent	dom-cost-is-counterfactual-reach	
src/dominator.rs	blocking_the_root_costs_the_whole_third_party_surface	dom-cost-is-counterfactual-reach	
src/dominator.rs	a_first_party_node_under_a_third_party_parent_is_not_billed_to_it	dom-cost-is-counterfactual-reach	
src/loc.rs	manifest_facts_read_licence_and_the_proc_macro_flag	manifest-field-readers	
src/policy.rs	forks_are_modeled_including_census_and_notice	ledger-parse-fail-closed	
src/policy.rs	a_semver_class_is_the_version_line_cargo_resolves_within	semver-row-scope	
src/policy.rs	only_a_renamed_row_with_a_known_version_is_scoped_to_its_line	semver-row-scope	
src/provenance.rs	path_values_are_read_out_of_a_manifest_and_relative_parts_resolve	path-deps-and-normalize	
src/resolve.rs	independent_wasm_roots_each_need_a_wasm_cell	check-verdict-obligations	
src/resolve.rs	an_offline_miss_retries_online_once_and_notes_it	offline-first-resolution	
src/resolve.rs	the_depth_stack_reconstructs_parents	tree-depth-stack	
src/survey.rs	fit_truncates_by_chars_and_never_splits_one	fit-char-truncation	
tests/red_fixtures.rs	a_reinstated_carved_module_reds_the_forge_verb	check-verdict-obligations	
tests/red_fixtures.rs	an_unreviewed_patch_entry_reds_the_forge_verb	check-verdict-obligations	
tests/red_fixtures.rs	a_notice_that_omits_a_registered_fork_reds_the_forge_verb	check-verdict-obligations	
tests/red_fixtures.rs	a_ledger_that_disagrees_with_the_tree_reds_the_forge_verb	check-verdict-obligations	
tests/red_fixtures.rs	an_unpatched_sibling_version_reds_the_forge_verb	check-verdict-obligations	
```

## 3. Coverage

- 184 tests read: 159 map to one of the 63 specs, 25 are not spec samples (§4).
  The chunk manifest said 183; a fresh count of `#[test]` attributes gives 184
  (`src/attest.rs` has 28).
- Six tests sample two specs each (the `also` column): the two `blame` path tests, the verbatim-prefix
  test, the 80-character reason, and two `loc` member tests.
- The biggest collapses (primary tests): `tree-line-parse` (9), `ledger-parse-fail-closed` (8), `attest-license-obligations` (7), `budget-parse-grammar` (7), `budget-ratchet-plan` (7), `astream-validate-ordered-conjunction` (7), `cli-parse-grammar` (6), `attest-structure-obligations` (6).
- `dom-cost-is-counterfactual-reach` has 4 primary tests, but `dominator-shape`,
  `dominator-laminar-partition`, `ranked-is-sorted-permutation` and the totals of
  `survey-reports-live-totals` and `blame-report-contents` are corollaries of it.

## 4. Tests that are not spec samples

**Status (2026-10-16): deleted** — 16 of the 19 **delete** rows are gone: every one except
`loc::mac_arm_stays_within_the_measured_baseline`, `loc::linux_stays_within_the_measured_baseline`
and `loc::licences_are_read_for_every_third_party_package`, which were not in the confirmed deletion list and are kept.
Status 2026-10-17: those three went too, with forge's other live-repo tests (8c5dd17c3f).
The **becomes-spec-after-fix** rows remain. Wasm cell coverage now belongs to
`check-verdict-obligations`: production OB-17 requires a cell for each browser root,
without requiring either browser module's graph to contain its sibling's.

**Status (2026-10-17):** every test this section listed is gone, the three kept `loc` tests and
the **becomes-spec-after-fix** tests included, and so is its table. Most of them read the live
repository and pinned today's facts about it, not a property of forge's code. The silent 0-LOC
case one of them guarded is §5.13.

No test is a regression of a removed bug with no general property: each regression-shaped test is an
instance of one of the §1 specs.

## 5. Implied specs that look wrong, or disagree with the code

1. **`semver-row-scope` is unsound for `0` and `0.0`.** `semver_class("0")` returns `"0."` (the minor is
   `unwrap_or_default`'ed to `""`), and `known_semver_class` gives `0` and `0.0` one class although `^0`
   and `^0.0` span many lines. So `in_row_scope(true, Some("0.52.0"), "0") = false`: a requirement `0`
   or `0.0` against a renamed row is silently dropped by attest OB-2, `policy::patch_entries`
   `shadowed_by` and `check::classify`. `renamed_forks_still_refuse_unmodelled_workspace_requirements`
   tries `*`, `>=0`, `<3`, `>2`, `0.*` and `~2`, never `0` or `0.0`. Fix the code, then prove the
   soundness clause.
2. **`attest::apache_is_mandatory` ignores parentheses.** It splits on the text `" OR "`, so
   `"Apache-2.0 AND (MIT OR BSD-3-Clause)"` returns false although every choice includes Apache-2.0,
   and OB-7 skips its §4(b) check. The test samples only parenthesis-free expressions. Prove the
   intended statement in `apache-mandatory`.
3. **`attest::workspace_requirements` misses renamed dependencies.** It looks up the table key
   (`table.get(name)`), so `x = { package = "forked", version = "…" }` is never checked by OB-2.
   `check::dev_dependency_oracles` already handles `package =`, and its test pins that; unify the two
   scanners on the oracle rule.
4. **`loc::package_dir` has a version-blind fallback.** Branch 4 (`root/crates/<name>`) contradicts the
   "no version-blind fallback" comment. The other-version assertion in
   `package_dir_prefers_the_member_then_the_patch_and_bills_by_directory` passes only because tracing
   lives in `crates/aterm-tracing`; for a member whose directory equals its package name (e.g.
   `aterm-core`), every version resolves to the member.
5. **Two definitions of "the surface".** `survey::partition_check`'s DISAGREES branch is reachable only
   when a node has no facts entry: `dominator::dom_against` counts a fact-less node as third-party, while
   `model::CellSurvey::third_party()` excludes it. They agree only under `WF`; pick one.
6. **`deny::findings` depends on node order for one ban.** On the first node whose requirement cannot be
   read it pushes `BadRequirement` and breaks, but `Banned` findings from earlier nodes of the same ban
   are already out. `deny::satisfies` also ignores pre-release tags (`0.17.14-rc1` does not satisfy
   `<0.17.14`), unlike cargo semver.
7. **Substring key matching.** `provenance::path_deps` finds `"path"` anywhere, including inside other
   keys (`xpath = "…"`), so a non-path key can add a spurious dependency. `loc::manifest_version`
   likewise reads any `[package]` key beginning with `version`.
8. **`attest::parse_version` drops a fourth component** (`1.2.3.4` parses as `1.2.3`) silently.
9. **The width tests check the wrong bound.** Every line-width test asserts ≤ 100 while `W_LINE` = 96,
   and `survey::wrap` and `blame::write_path` never break an over-long token, so "every line fits"
   holds only when tokens are short. The specs here state the `W_LINE` bound under that precondition.
10. **Broken message text.** `resolve::abs_root`'s error contains runs of about 13 spaces (missing `\`
    continuations: "directory              holding").
11. **Stale fixture.** The policy tests' `SAMPLE` still records winit as a vendored fork at
    `vendor/winit`; winit was absorbed into `crates/aterm-window`. The parse tests are unaffected.
12. **OB-15 is weaker than its docs.** `check::dev_dependency_oracles` reads only the
    `dev-dependencies` tables, so an oracle that a feature activates as an optional dependency is
    invisible to it.
13. **An unresolvable third-party package counts as 0 LOC.** When `package_dir` finds no source (a
    registry package that was never unpacked), `loc::measure` returns a third-party `PkgFacts` with
    `loc = 0`, no build script, no proc macro and no `root_dir`. Survey adds that 0 to its LOC
    totals and budget counts no build script or proc macro for it, instead of reporting
    could-not-run. `third-party-iff-not-first-party` states the code as it is. Once survey and budget
    treat a third-party package with no `root_dir` as could-not-run, that becomes a clause of
    `third-party-iff-not-first-party` and `survey-reports-live-totals`.

## 6. Proof order (easiest high-value first)

No spec is blocked by gap 0 alone, so the order prefers the smallest gap sets over pure code, weighted by
the tests each proof retires.

1. **`budget-validate-reason`** {0,1,3,6}. Straight-line `Option<&str>` checks; retires 3 tests outright
   and the boundary half of a fourth, and is the guard `budget-ratchet-plan` relies on.
2. **`req-satisfied-caret-exact`** {0,1,3,6} with **`semver-row-scope`** {0,1,3,5,6,7,13}. Tuple
   arithmetic on parsed versions. The second one forces the fix in §5.1, a real soundness bug in three
   gates.
3. **`tree-line-parse`** → **`tree-depth-stack`** {0,1,3,5,6,7}. Retires 12 tests and establishes the
   graph every dominator and blame spec is stated over.
4. **`budget-parse-grammar`** + **`budget-render-parse-roundtrip`**. Retires 8 tests; a line-wise
   parser whose round trip is the ledger's integrity.
5. **`cell-select`** {0,1,3,5,6,7,13}. Retires 5 tests across 3 files, and its "before any effect"
   clause carries into every verb.
6. Leaf formatters with one test each, cheap once gap 1 lands: `fit-char-truncation`, `commas-grouping`,
   `physical-lines`, `count-unsafe-tokens`, `spdx-license-arms`, `strip-verbatim-prefix`,
   `blame-spec-parse`, `versions-row-layout`, `roster-well-formed` (decidable by const evaluation).
7. **`dom-cost-is-counterfactual-reach`** and its corollaries (`ranked-*`, `dominator-shape`,
   `dominator-laminar-partition`), then `blame-count-paths` / `blame-shortest-paths`.
8. The parsers behind gap 12 (`aterm_toml`): `ledger-parse-fail-closed` (8 tests), `check-parse-carve-ledger`
   (5), `check-parse-patch-table` (3), `deny-parse-policy`, `manifest-field-readers`.
9. **`budget-ratchet-plan`** (7 tests; the only spec needing gap 2 for its `&mut` row updates).
10. The effectful compositions once their pure cores are factored out (gap 8), ending with
    `check-verdict-obligations`, which composes all of the above.
