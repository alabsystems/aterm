<!-- Copyright 2026 Andrew Yates -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# aterm-primer: spec recovery from its 61 unit tests

> Status 2026-10-16, after the merge with the tests-and-gates audit: the rows naming 43 tests it
> deleted are gone, and the spec rows no longer cite those tests. Counts elsewhere in this file
> were taken before.
>
> 2026-10-17: `primer-budget` subsumes no test now, §4's two tests are deleted, and §5 lost the
> five items whose tests are deleted or whose fix landed; the rest are renumbered.
>
> Status 2026-10-17: §2 lacked 15 of the crate's 37 tests (src/lib.rs 33, src/settings_json.rs
> 4). They are added, each spec that retires one cites it in §1, and the test counts in §3 and §6
> are taken from the new map. The 66 in the title and in the Source and Result lines is the count
> when this file was written.
> `find-block`, `upsert-block`, `cli-status-report` and `status-line` now state
> `BlockState::Newer` (a later aterm's block is left as is), which `a_newer_block_is_left_as_is`
> pins.

> 2026-10-25: `retired-skill-sweep` is new (the Gemini CLI command an earlier aterm wrote is deleted by
> the automatic pass, `install` and `remove`, a user's own file there kept), and `fold-outcome`,
> `auto-prime-pass`, `auto-prime-idempotent`, `cli-install-gate`, `cli-status-report` and
> `cli-remove` state it. Its test is mapped in §2; the counts elsewhere were taken before.

> 2026-10-27: codex gets the six skills Claude Code gets, at `.codex/skills/<n>/SKILL.md`, and the
> custom prompt it no longer reads is a second `RETIRED_SKILLS` row. `xdg` became `roots`
> (`Roots { xdg, codex }`): `$CODEX_HOME` moves `.codex` as `$XDG_CONFIG_HOME` moves `.config/`, so
> `xdg-join-and-display`, `skill-registry`, `retired-skill-sweep`, `bundled-docs-valid` and the
> rows naming the `_with_xdg` seams (now `_with`) state it. `summary-log-cap` lost the corollary
> that a fresh claude+codex first pass fits the cap. Three tests are new and mapped in §2, and
> `only_a_dot_config_row_follows_xdg_config_home` is now `each_variable_moves_only_its_own_rows`;
> the counts elsewhere were taken before.

> 2026-10-27, review: one fabric body for every agent with a skills folder (Codex's sandbox note in
> it; `CODEX_FABRIC_BODY` is gone), so a skills folder two agents share settles; `remove <agent>`
> keeps a skill file another detected agent's row lands on; the window's pass follows
> `Roots::window` (`$CODEX_HOME` never), retired rows are looked for at `retired_places` (`~` and
> where `$CODEX_HOME` moves them), and `remove` under `$CODEX_HOME` also clears `~/.codex`.
> `bundled-docs-valid`, `skill-registry`, `retired-skill-sweep`, `auto-prime-pass`,
> `cli-status-report`, `cli-remove` and `xdg-join-and-display` state it, `skill-one-body` is
> new, and two tests are new and mapped in §2; the counts elsewhere were taken before.

> 2026-10-28: `open-in-amacs` (opening a file for the person in amacs, waiting or not, and reading
> back what they wrote) is a seventh skill Claude Code and Codex get, so `skill-registry` names it;
> the tests that counted six skills now count `skills_for`'s rows.

> 2026-10-28: `primer-budget` is now `primer-gate-line`. Its bound (`primer_block(None)` at most
> 20 lines, every agent's block at most 2,400 bytes) was false: `primer_block(None)` is 27 lines and
> 8,337 bytes, Codex's block 35 lines and 8,974 bytes. Nothing enforced it, because the test that
> did was deleted as a word pin (44f2f4b090, c04b399914), so the row now states that nothing bounds
> the block and keeps its one true clause, the first fact line's gate.

> 2026-10-28: Claude Code has its own part too (`CLAUDE_ADDENDUM`: the background Bash a wait runs
> as, and the `rm` box its harness answers, both out of the shared body), and an agent's part now
> sits between the body and the Rust note, so `primer-block-shape` states both. A file two agents
> share carries each one's part (`Readers::block`), since every part is said of the agent it
> names, so Claude Code keeps both lines in a file Codex reads too. One test is new
> (`each_program_is_taught_the_wait_its_shell_can_do`), mapped in §2 and cited by that row; the
> counts elsewhere were taken before.

> Status 2026-10-16: this file is the crate's migration worklist. Each spec below is proved
> through Trust/Clean about the compiled Rust function, and then the tests it subsumes are
> deleted. No test has been deleted yet. The two checks that sample no spec (§4) are marked
> for deletion.
>
> **Re-measured on Trust seal `150efe4a`: `top-effort-key-spelling` is still blocked** (2026-10-16).
> Any fn that reads `CLAUDE_TOP_EFFORT_KEY` must go through `str::as_bytes` and a `&[u8]` local, and
> E6 refuses both probes tried (`CLAUDE_TOP_EFFORT_KEY.as_bytes() == b"ultra\x63ode"` returning
> `bool`, and a `usize -> u8` byte reader) at the FACET gate: "local `b` has type
> `Ref { .. Slice { elem: Int { width: 8 } } }`, outside the v1 scalar fragment; the construct outside
> the E6 fragment is the call to `core::str::<impl str>::as_bytes`". No `trust_import_aterm_primer__*`
> is minted, so a Clean theorem naming it fails as an unknown (auto-bound) name. Gaps 1 (`&str`/`&[u8]`
> carriers) and 3 (a call in the reflected body). A scalar `match` table would be admitted, but it
> would be a restated copy of the key and not the constant, so it was not used. The code is unchanged
> and no test was deleted.

Source: `crates/aterm-primer` (read only). Every live `#[test]` (`src/lib.rs` 57,
`src/settings_json.rs` 4 = 61) and every production fn they reach are mapped below.

Result: **47 specs**, covering **61 tests**. **0 remaining non-spec tests** (§4).

The plan is `PLAN-tests-to-specs-2026-10-15.md` (historical internal plan),
and the gap numbers are §7 of
`AUDIT-trust-for-test-migration-2026-10-15.md` (historical internal audit).
Gap 0 (the installed seal publishes no Lean-discharged row) blocks every spec, so the
blocking-gaps column lists it on every row. No spec is blocked by gap 0 alone.

## 0. Notation used in the statements

Function names are relative to the crate root (`aterm_primer::`); `settings_json::` is the
private JSON module.

Registry and blocks
- `AF` = `AGENT_FILES` in registry order `[claude, codex, agy, opencode, muse]`. `names(AF)` = their `name`s.
  `addendum(n)` = the `addendum` of the `AF` row named `n`, or `None` when no row has that name.
- `B(x)` = `block_with(x)`. `Blk(a)` = `block_for(a)` = `B(a.addendum)`.
- `Lines(c)` = `c.split_inclusive('\n')`. `tr(l)` = `l.trim_end_matches(['\n','\r']).trim()`.
  `isBegin(l)` := `tr(l).starts_with(MARK_PREFIX)`. `isEnd(l)` := `tr(l) == MARK_END`.
- `nl*(s)` = `s.trim_end_matches('\n')`. `canon(c)` = `""` if `c.trim()` is empty, else `nl*(c) + "\n"`.
- `WFB(b)`, a well-formed block: `find_block(b) == Ok(Some((0, b.len())))`, `b` ends in `'\n'`, and `b`
  has exactly one `isBegin` line (its first). Every `Blk(a)` is `WFB`.
- `shq(w)` = `"'" + w.replace('\'', "'\\''") + "'"` (the old hook writer's single-quoting).

Paths and filesystem
- `roots` = `Roots { xdg, codex }`: the non-empty `$XDG_CONFIG_HOME` and the absolute `$CODEX_HOME`
  at the public entry points (`Roots::of_env`; `Roots::window` for `auto_prime`), chosen by a test.
  `W(rel)` = `home_join(home, roots.window(), rel)`: `J(rel)` with `.codex` left at `~/.codex`.
- `J(rel)` = `home_join(home, roots, rel)`. `Det(a)` := `J(a.dir).is_dir()`.
- `σ` is the filesystem state: each path is absent, a file (bytes, mode, dev/ino), a dir, or a symlink.
  `resolve(p)` = `canonicalize(p)` when it resolves.
- `untouched(p)` means `σ'(p) = σ(p)`: same kind, same bytes, same link target, and no new sibling
  backup or temp file.
- `Temps(d)` = entries of `d` whose name contains `.aterm-tmp-`. `Baks(p)` = siblings of `p` named `p.bak-*`.
- An *external save* is a write by another party that lands between a rewrite's read and its rename.
  Tests inject one through the `#[cfg(test)]` `SAVE_AFTER_READ` seam.

JSON
- `WF(v)`: every `Object` in `v` has pairwise distinct keys, every `Number` text matches the RFC 8259
  number grammar, and every value sits inside at most 64 containers.
- Parsed objects follow `JSON.parse` semantics: first position, last value.

Outcomes
- `Fold(...)` = `fold_outcome`. `word(o)` = `outcome_word(o)`.

The vendor's top-level effort key is written in this file as `b"ultra\x63ode"`, never joined,
because the export baseline bans the joined word in every exported file (§5.7).

## 1. Specs (TSV)

Columns: `id`, `kind`, `functions`, `statement`, `subsumes (primary tests; secondary in §2)`,
`blocking_gaps (audit §7)`, `difficulty (Rust features the proof must handle)`. Test names are in
src/lib.rs unless prefixed `settings_json::`.

```tsv
id	kind	functions	statement	subsumes	blocking_gaps	difficulty
top-effort-key-spelling	table-property	CLAUDE_TOP_EFFORT_KEY, CLAUDE_DEFAULTS, CLAUDE_EFFORT_DEFAULTS	CLAUDE_TOP_EFFORT_KEY.as_bytes() == b"ultra\x63ode" (the vendor's key; never written joined). CLAUDE_DEFAULTS[0] == (CLAUDE_TOP_EFFORT_KEY, Bool(true)); CLAUDE_DEFAULTS[1] == ("effortLevel", Str("xhigh")); CLAUDE_EFFORT_DEFAULTS == [CLAUDE_TOP_EFFORT_KEY, "effortLevel"].	the_top_effort_key_is_the_vendors_spelling	0,1	TRIVIAL: const &str from concat!, byte-slice equality; decidable by evaluation
primer-block-shape	contract	block_with, primer_block, block_for, Readers::block	∀ ps: &[&str]: block_with(ps) == MARK_BEGIN+"\n"+PRIMER_BODY+"\n\n"+concat(p+"\n\n" for p in ps)+RUST_NOTE+"\n"+MARK_END+"\n" (an agent's own part is a paragraph of the `## aterm` section, ahead of the Rust note's heading). ∀ n: primer_block(n) == block_with(n.and_then(addendum) as a slice). Table fact: addendum(a.name) == Some(CLAUDE_ADDENDUM) iff a.name == "claude", Some(CODEX_ADDENDUM) iff a.name == "codex", else None. Readers::block(a) == block_with(the addenda, in registry order, of a and every reader whose file is a's), so a file one agent reads carries block_for(a) and one Claude Code and Codex share carries both parts. Corollaries: ∀ n ∉ {Some("claude"), Some("codex")} (registry names or not) primer_block(n) == primer_block(None), which contains neither part; primer_block(Some("codex")) has body, CODEX_ADDENDUM, RUST_NOTE in that order, and primer_block(Some("claude")) body, CLAUDE_ADDENDUM, RUST_NOTE.	each_program_is_taught_the_wait_its_shell_can_do	0,1,3,13	EASY: &str/String, format!, Option combinators with closures, iter().find over a const slice of structs
primer-gate-line	table-property	PRIMER_BODY	PRIMER_BODY's first fact line (the line after its `## aterm` heading) names TERM_PROGRAM=aterm and ATERM_CHILD=1 and says to ignore the section otherwise. Nothing bounds primer_block's line or byte count.	-	0,1,3,5	EASY: one line of a const string, substring search; decidable by evaluation
primer-names-env-deny-prefixes	table-property	PRIMER_BODY, aterm_types::domain::ENV_DENY_PREFIXES	∀ p ∈ aterm_types::domain::ENV_DENY_PREFIXES with p != "_DEVTOOL_": PRIMER_BODY contains p.trim_end_matches('_').trim_end_matches('*').	-	0,1,5,12	EASY once cross-crate consts are citable: const slice of &str, substring search
bundled-docs-valid	table-property	skills_for, skill_state	∀ a ∈ AF, s ∈ skills_for(a.name): (i) some line of s.body, trim_start, starts with SKILL_MARK_PREFIX, so skill_state(s.body, s.body) == Current and any other marked file there reads Stale, never Foreign. (ii) If s.path contains "/skills/": first line trimmed is "---"; the first 12 lines include one starting "name:" and one starting "description:"; no description value contains ": ". (iii) FABRIC_BODY starts with SKILL_MARK_PREFIX and contains neither "'''" nor '\\'. (iv) Per-agent fabric headers: claude's, codex's and muse's start "---\nname: aterm-fabric\n" with a description: line in its first 4 lines; opencode's starts "---\ndescription: " with no name: line in its first 3 lines. Facts about the include_str! assets, provable by evaluation.	every_bundled_doc_is_marked_and_parses_for_its_agent	0,1,3,5,7,14	EASY-MEDIUM: kilobyte &'static str constants from include_str!/concat!, lines()/take/any, substring search; decide/eval must scale to the asset sizes
skill-one-body	table-property	skills_for, FABRIC_SKILL_BODY	∀ a, b ∈ AF, s ∈ skills_for(a.name), t ∈ skills_for(b.name): s.path and t.path both end "/skills/" + n for one n ⇒ s.body == t.body, so one skills folder linked into two homes holds a file every row reads Current (auto-prime-idempotent holds through it). FABRIC_SKILL_BODY contains Codex's sandbox note ("Codex tool shells can have different sandbox permissions"), and every fabric skill at a "/skills/" path is it.	a_skill_two_agents_can_share_is_one_body	0,1,3,5	EASY: const slices, rsplit_once, pairwise string equality
skill-registry	table-property	skills_for	skills_for(n) == [] ∀ n ∉ names(AF). skills_for("claude") and skills_for("codex") list exactly drive-aterm, supervise-agent, rust-in-aterm, aterm-themes, aterm-fabric, system-state, open-in-amacs at .claude/skills/<n>/SKILL.md and .codex/skills/<n>/SKILL.md, in that order, each codex body its claude row's. ∀ a ∈ AF but agy: skills_for(a.name) is non-empty and contains the fabric doc at FabricPath(a) (claude .claude/skills/aterm-fabric/SKILL.md; codex .codex/skills/aterm-fabric/SKILL.md; opencode .config/opencode/command/aterm-fabric.md; muse .config/muse/skills/aterm-fabric/SKILL.md); skills_for("agy") == [] (agy's row is the retired Gemini CLI's context file, which agy loads; no skill place of agy's is measured). ∀ a ∈ AF: a.file and every s.path start with a.dir.	codex_gets_every_aterm_skill_where_codex_reads_skills	0,1,3,5	EASY: match on &str returning &'static [struct]; const slices; starts_with
retired-skill-sweep	contract	RETIRED_SKILLS, retired_skills_for, retired_places, marked, sweep_retired_skill, sweep_retired_rows	Table fact: retired_skills_for("agy") paths == [".gemini/commands/aterm-fabric.toml"] (the Gemini CLI command, what "Gemini CLI's command"), retired_skills_for("codex") paths == [".codex/prompts/aterm-fabric.md"] (Codex's custom prompt, what "Codex's custom prompt, which Codex no longer reads"), every earlier version writing each with a SKILL_MARK_PREFIX line, and retired_skills_for(n) == [] ∀ other n. ∀ path, σ: sweep_retired_skill(path) == Ok(false), untouched(path), when no file opens at path (NotFound) or its bytes (lossy UTF-8) have no line l with l.trim_start().starts_with(SKILL_MARK_PREFIX) (the user's file, never deleted); Ok(true) with path removed when they have one; Err naming path when it cannot be read (a directory included) or removed, untouched(path). retired_places(home, roots, r) == [(J(r.path), shown)] ++ [(W(r.path), shown)] when W(r.path) ≠ J(r.path), each shown its roots' display_path: no earlier aterm followed $CODEX_HOME, so a moved retired row is looked for at ~ too. sweep_retired_rows(home, roots, a, out) applies it to each place ∀ r ∈ retired_skills_for(a), appends format!("{:<9} {shown:<30} {verdict}", "  retired") only for Ok(true) ("removed (" + r.what + "; aterm no longer writes it)") or Err(e) ("ERROR: " + e), and returns true iff some Err. Idempotence: a second sweep of a path it emptied is Ok(false) with no row.	the_gemini_cli_command_an_earlier_aterm_wrote_is_deleted_and_the_users_is_kept, the_codex_prompt_an_earlier_aterm_wrote_is_deleted_and_the_users_is_kept	0,1,3,4,8	MEDIUM: fs read/remove effects, lossy UTF-8, lines() with any, a const table filtered by owner
off-switch-phrase	contract	off_switch, auto_prime_note	∀ xdg: off_switch(xdg) == "set `agents_auto_prime = false` in " + F + " to stop", F = x.join("aterm").join("aterm.toml").display() when xdg == Some(x), else "%APPDATA%\aterm\aterm.toml" on Windows and "~/.config/aterm/aterm.toml" elsewhere. auto_prime_note(xdg) is the fixed two-sentence template whose last line is "(" + off_switch(xdg) + ").", so it contains off_switch(xdg).	-	0,1,3,8	EASY: format!, Path join/display (needs a Path model)
xdg-join-and-display	contract	Roots::moved, Roots::window, home_join, display_path	∀ home, roots, rel: Roots::moved(rel) == Some((x, r)) if rel == ".config/" + r ∧ roots.xdg == Some(x); else Some((c, "")) if rel == ".codex" ∧ roots.codex == Some(c), Some((c, r)) if rel == ".codex/" + r ∧ roots.codex == Some(c); else None. Let (base, rest) = moved(rel) or (home, rel). home_join(home, roots, rel) == base.join(seg1)…join(segn) for the non-empty segs of rest.split('/'). display_path(home, roots, rel) == home_join(...).display().to_string() when moved(rel) is Some, else "~/" + rel. roots.window() == Roots { xdg: roots.xdg, codex: None }.	each_variable_moves_only_its_own_rows	0,1,3,7,8	EASY-MEDIUM: Path/PathBuf push, strip_prefix, a split loop, Display of a path
doc-label	contract	doc_label	∀ p: let f = p after its last '/' (p if none), stem = f before its last '.' (f if none). doc_label(p) == stem unless stem.eq_ignore_ascii_case("skill"), in which case it is the second-to-last '/'-segment of p, or p itself when p has no '/'. Never panics. Table: ∀ registered s, doc_label(s.path) is non-empty, has no '.', and is a substring of s.path (§5.9).	-	0,1,3,5,13	EASY-MEDIUM: rsplit/rsplit_once/nth on &str, map_or closures, a borrowed subslice result
harness-state-of	contract	harness_state_of	∀ c: harness_state_of(c) == Some(PathBuf::from(s)) iff c.trim() == "sh '" + q + "'" + tail where q contains no '\'', q == s + "/plugin/hooks/bridge.sh", and s != "". Otherwise None.	-	0,1,3,13	EASY: strip_prefix/split_once/strip_suffix chain, Option combinators, PathBuf
skill-state	contract	skill_state	∀ c, b: skill_state(c, b) == Current if c == b; else Stale if some l ∈ c.lines() has l.trim_start().starts_with(SKILL_MARK_PREFIX); else Foreign.	skill_state_distinguishes_ours_from_the_users	0,1,3,7,13	EASY: &str equality, lines() with any(closure), enum result
fold-outcome	contract	fold_outcome, outcome_word	∀ primer, skills, retired, hooks, fold_outcome returns the first case that applies: Error(e) if primer == Err(e); Error(e) for the first skill Err(e); Error("retired skill: " + e) for the first retired Err(e); Error("hooks: " + e) if hooks == Some(Err(e)); Installed if primer is Created or Appended; Updated if primer is Replaced, or any skill is Installed/Updated, or any retired is Ok(true), or hooks is Some(Ok(Removed{..})); SkippedForeign if any skill is Foreign; Unchanged otherwise. Words: Unchanged "unchanged", Installed "installed", Updated "updated", SkippedForeign "unchanged (a skill file is yours, left alone)", Error(e) "ERROR: " + e.	(lemma; samples in §2)	0,1,3,7,13	EASY-MEDIUM: enums with String payloads, slice iteration with any/find_map closures, matches!
find-block	contract	find_block, block_state, is_newer, marker_version	∀ c: find_block(c) == Ok(None) iff no l ∈ Lines(c) is isBegin (a stray end marker alone is user content); Err iff some line is isBegin and no later line is isEnd; else Ok(Some((s, e))) with s the byte offset of the first isBegin line and e the end offset (including its '\n' when present) of the first isEnd line after it. ∀ b: block_state(c, b) == Ok(Absent) when find_block(c) == Ok(None); Ok(Current) when nl*(c[s..e]) == nl*(b); else Ok(Newer) when is_newer(c[s..e], b); Ok(Stale) otherwise. is_newer(f, b) iff marker_version(f) == Some(x), marker_version(b) == Some(y) and x > y. marker_version(t) is the u32 spelled by the ASCII digits right after 'v' when t.trim_start() is MARK_PREFIX, then optional whitespace, then 'v'; it is None when t has another shape, no digit follows the 'v', or the digits overflow u32. Fail closed: block_state, upsert_block and remove_block return Err exactly when find_block(c) does, never a partial result.	unterminated_block_fails_closed	0,1,3,5,7	MEDIUM: for-loop over split_inclusive, str slicing/trim, Option<usize> state, Result<Option<(usize,usize)>,String>
upsert-block	contract	upsert_block, block_state, is_newer	∀ c, ∀ b with WFB(b): (i) if find_block(c) == Ok(Some((s, e))): upsert_block(c, b) == Ok(None) when nl*(c[s..e]) == nl*(b) or is_newer(c[s..e], b) (a later aterm's block is left as is), else Ok(Some(c[..s] + b + c[e..])) (in place; c[..s] and c[e..] preserved byte for byte). (ii) if find_block(c) == Ok(None): Ok(Some(nl*(c) + (if nl*(c) != "" then "\n\n" else "") + b)). Corollaries: idempotent (upsert_block(c, b) == Ok(Some(c')) ⇒ block_state(c', b) == Ok(Current) ∧ upsert_block(c', b) == Ok(None)); if c has ≤ 1 isBegin line, c' has exactly one isBegin and one isEnd line; upsert_block(c, b) == Ok(None) iff block_state(c, b) ∈ {Ok(Current), Ok(Newer)}.	a_newer_block_is_left_as_is	0,1,3,4,5,7	MEDIUM: String building, str slicing, trim_end_matches, composition with find-block, quantified over strings
remove-block-round-trip	round-trip	remove_block, upsert_block	∀ c: remove_block(c) == Ok(None) iff find_block(c) == Ok(None). When find_block(c) == Ok(Some((s, e))), let r = c[..s] + c[e..]: remove_block(c) == Ok(Some("")) if r.trim() is empty, else Ok(Some(nl*(r) + "\n")). Round trip: ∀ c with find_block(c) == Ok(None), ∀ b with WFB(b): remove_block(upsert_block(c, b).unwrap().unwrap()) == Ok(Some(canon(c))). So c comes back exactly when c == "" or c ends in exactly one '\n' (§5.1).	upsert_is_idempotent_and_remove_round_trips_to_the_original	0,1,3,4,5	MEDIUM: String concat/truncate/trim, composition of two string transforms
aterm-hook-command	contract	is_aterm_hook_command, bare_path_hook, is_link_hook_form, split_first_word	∀ c, let t = c.trim(); L(r) iff some f ∈ LINK_HOOK_FORMS has r == f or r starts with f + " "; basename(w) = w after its last '/'. is_aterm_hook_command(c) iff (H) t == h + RETIRED_INSTALLER_MARK for some h ending in whitespace; or (P) t starts with '/' and ∃ i with t[i] == '/', t[..i] free of SH_META chars, t[i+1..] == name + " " + r, name ∈ ATERM_EXE_NAMES, L(r.trim_start()) (the loop's early exit equals this ∃ because "prefix has a meta char" is monotone in i); or (W) split_first_word(t) == Some((w, rest)), basename(w) ∈ ATERM_EXE_NAMES, L(rest). Never panics.	only_an_aterm_executable_running_the_hook_verb_or_the_harness_mark_is_aterms	0,1,3,5,7,13	MEDIUM: trim/strip_prefix/strip_suffix/match_indices loop, char predicate, const arrays, closures in any(); needs sh-word-round-trip
sh-word-round-trip	round-trip	split_first_word	∀ w, r: split_first_word(shq(w) + " " + r) == Some((w, r.trim_start())) and split_first_word(shq(w)) == Some((w, "")). ∀ unquoted w with no ' ' and not starting '\'': split_first_word(w + " " + r) == Some((w, r.trim_start())) and split_first_word(w) == Some((w, "")). A command starting '\'' with no closing quote under this decoding (each "'\\''" is an embedded quote) gives None. Never panics on a non-ASCII boundary.	(lemma; samples in §2)	0,1,3,5,7	MEDIUM: char_indices loop with manual advance (nth), String push, slicing at char boundaries
json-grammar	refinement	settings_json::Json::parse, Parser::{value,string,number,hex4,array,object}	∀ t: Json::parse(t).is_ok() iff t == ws + V + ws with V an RFC 8259 value and: every value has ≤ 64 enclosing containers; every \u escape has exactly four hex digits (no sign); every \u in D800..DBFF is immediately followed by a \u in DC00..DFFF; no \u denotes a lone low surrogate; strings contain no raw byte < 0x20. On Ok: every Str holds the unescaped text (\" \\ \/ \b \f \n \r \t \uXXXX, surrogate pairs); every Number holds its exact source text; objects follow json-dupkeys. On Err the message is "byte N: expected ...". Never panics, including at end of input.	settings_json::malformed_documents_are_refused	0,1,2,3,5,7,11	HARD: recursive descent with a &mut self cursor over &[u8], loops, depth recursion, char::from_u32, HashMap<String,usize>, Result over a recursive enum
json-dupkeys	contract	settings_json::Parser::object, settings_json::Json::get	∀ t with Json::parse(t) == Ok(v): every Object in v has pairwise distinct keys at every depth. A source object with members (k1,v1)..(kn,vn) parses to [(k, value of the LAST k) for each distinct k, in order of FIRST occurrence] (JSON.parse semantics), so get(k) returns the last value written.	settings_json::a_key_written_twice_reads_as_json_parse_reads_it	0,1,2,3,5,7,11	MEDIUM-HARD: HashMap index map plus Vec mutation in a loop; recursion
json-render-round-trip	round-trip	settings_json::Json::render, settings_json::string, settings_json::Json::parse	∀ v with WF(v): Json::parse(&v.render()) == Ok(v); ∀ s: Json::parse(&string(s)) == Ok(Json::Str(s)). render is the two-space pretty-printer: Null/Bool/Number print their literal (Number verbatim, so 1.50 and 1e3 survive); Str prints string(s); [] and {} print "[]" and "{}"; a non-empty container at depth d prints its opener, each item on its own line indented 2(d+1) with ",\n" separators, then "\n" + indent(d) + closer; an object member prints string(k) + ": " + value. string(s) = '"' + map(s) + '"', map escapes '"', '\\', \n, \r, \t, writes other chars < 0x20 as lowercase \u%04x, copies the rest raw. Corollary: parse(t) == Ok(v) ⇒ parse(render(v)) == Ok(v), since parsed values are WF.	settings_json::a_settings_document_round_trips_in_order, settings_json::strings_escape_and_unescape	0,1,2,3,4,5,7	HARD: recursive render over a recursive enum with Vec/String, &mut String accumulator, loops, char escapes; composition of printer and parser
efforts-set-by-hand	contract	claude_efforts_set_by_hand, efforts_set_by_hand	∀ t: claude_efforts_set_by_hand(t) == [] when Json::parse(t) fails. Else with doc parsed it is top ++ models: top = [("effortLevel", e.render())] when doc.get("effortLevel") == Some(e) ∧ e != Str("xhigh"), else []; models = [("modelSettings." + m + ".effortLevel", e.render()) | (m, ms) in doc.get("modelSettings") as an object, sorted by m in byte order, ms.get("effortLevel") == Some(e) ∧ e != Str("xhigh")]. A non-object modelSettings contributes nothing; duplicates read by json-dupkeys (last wins).	-	0,1,3,5,7,11,13	MEDIUM: Json traversal, BTreeMap collect (sorted), closure capturing &mut Vec, format!
survey-rate-set-by-hand	contract	claude_survey_rate_set_by_hand, survey_rate_set_by_hand	∀ t: None when parse fails. Else None when doc.get("feedbackSurveyRate") is None; None when that value is Number(n) with n.parse::<f64>() == Ok(0.0) (0 and 0.0, not the string "0"); otherwise Some(value.render()).	-	0,1,3,13	EASY-MEDIUM: Json match, f64 parse of a String (needs a float-parse model), float equality, Option::then
compact-offers-kept	contract	compact_offers_kept, claude_compact_offers_kept_in, read_compact_state	∀ t: compact_offers_kept(t) == Err(e) when Json::parse(t) == Err(e). Else, with doc parsed, Ok(the set of m, in byte order, each once, such that some (a, ms) in doc.get("autoCompactOffer") as an object has (m, o) in ms as an object with o.get("resolved").get("choice") == Some(Str("keep")))); a non-object autoCompactOffer or account contributes nothing; duplicates read by json-dupkeys (last wins). ∀ h, σ: claude_compact_offers_kept_in(h) opens h.join(".claude.json") (normal symlinks followed), nonblocking on supported Unix hosts (macOS/Linux); unsupported Unix hosts return Err. Only open NotFound ⇒ Ok([]). The opened handle must be regular, metadata length ≤ 2 MiB, and the actual read (capped at 2 MiB + 1) ≤ 2 MiB; any other open/metadata/read/UTF-8/size error ⇒ Err, which holds only the compact default. Otherwise compact_offers_kept(text).	claude_defaults_add_the_compact_window_only_where_unset_and_no_offer_was_kept, compact_state_reads_are_bounded_before_defaults_are_chosen, compact_state_accepts_regular_symlinks_and_refuses_open_nonregular_files, an_oversized_compact_state_holds_only_its_default_back	0,1,3,5,7,8,11,13	MEDIUM: nested Json traversal with flat_map/filter closures, BTreeSet collect (sorted, dedup), fs read with an io::ErrorKind match
claude-defaults-edit	contract	claude_defaults_edit, ClaudeDefault::json, ClaudeDefault::json_text	∀ path, text, hold: (a) text None ∧ path.parent() not a dir ⇒ Ok(None). (b) Else doc = parse(text), or Object([]) when text is None; parse error ⇒ Err(path + ": " + e); a non-object top level ⇒ Err("... the top level is not a JSON object"). own_effort := efforts_set_by_hand(doc) != [], own_rate := survey_rate_set_by_hand(doc).is_some(), both BEFORE any addition. For each (k, v) ∈ CLAUDE_DEFAULTS in order: skip k if own_effort ∧ k ∈ CLAUDE_EFFORT_DEFAULTS, or own_rate ∧ k == CLAUDE_SURVEY_SWITCH, or hold ∧ k == CLAUDE_COMPACT_WINDOW_KEY; k without '.': append (k, v.json()) at top iff absent; k == p + "." + c: p absent ⇒ append (p, Object([(c, v.json())])); p an object ⇒ append (c, v.json()) to it iff c absent; p present and not an object ⇒ Err containing "\"p\" is not a JSON object". Result Ok(None) when nothing was added, else Ok(Some((doc', keys))) with keys the added k in CLAUDE_DEFAULTS order and doc' keeping every existing member, value and order, only appending. ClaudeDefault::json_text(d) == d.json().render().	claude_defaults_add_only_what_is_unset_and_not_held_back	0,1,2,3,4,5,7	MEDIUM-HARD: &mut Vec<(String,Json)> mutation in a loop over a const array, split_once, iter_mut().find, nested Result/Option
strip-marked-hooks	contract	strip_marked_hooks	∀ doc: doc.get("hooks") None ⇒ Ok(0), doc unchanged. hooks not an object ⇒ Err("`hooks` is not a JSON object"). An event value not an array ⇒ Err("`hooks.<event>` is not a JSON array") at the first such event. Otherwise, with n = number of entries e inside group["hooks"] arrays whose e["command"] is a Str c with is_aterm_hook_command(c): Ok(n), and doc' is doc with those entries removed, every group whose "hooks" array is now empty removed, every event whose array is now empty removed, and the top-level "hooks" removed if it is now an empty object. Everything else is unchanged and in order (other keys, entries, groups, groups whose "hooks" is not an array). See §5.3 on pre-existing empty groups.	-	0,1,2,3,4,5,7,13	MEDIUM-HARD: &mut traversal of nested Vec<(String,Json)>/Vec<Json>, retain with closures, counting, calls to aterm-hook-command
restore-harness-statusline	contract	restore_harness_statusline, is_harness_statusline	∀ doc, path, σ: if doc.get("statusLine") (live value after duplicate merging) is not harness-shaped (its command, trimmed, == h + "# aterm-harness" with h ending in whitespace), return false, doc unchanged. Otherwise: state = harness_state_of(command), kept only if state/plugin is a dir; copied = the statusLine of the first of path + ".aterm-harness.orig" and resolve(path) + ".aterm-harness.orig" that reads, parses and has a NON-harness-shaped statusLine; original = (state exists ∧ state/statusline.user reads to a non-blank cmd (trailing '\n' trimmed)) ? (copied if copied["command"] == cmd else {"type": "command", "command": cmd}) : (state exists ? None : copied). The top-level statusLine is replaced IN PLACE by original, or removed when original is None; returns true. Reads only; writes nothing. See §5.10.	the_harness_statusline_is_given_back_from_the_installers_byte_copy, the_statusline_install_displaced_wins_over_a_stale_byte_copy	0,1,2,3,4,8,13	HARD: fs reads (read_to_string, canonicalize, is_dir), Json mutation, Option chains with closures, PathBuf formatting
atomic-write	contract	write_atomically, write_atomically_if, create_private_temp	∀ path, bytes, no external writer: R = canonicalize(path) if it resolves; if not, Err when dangling_link(path) is Some, else R = path. On Ok: R holds exactly bytes; a symlink path is still the same symlink and its target changed; mode(R) is R's old mode when R was a regular file, else TEMP_MODE (0600) whatever the umask; no Temps(parent(R)) entry made by this call remains. On Err (including R a directory): R untouched, contents included, no temp remains. create_private_temp(t) is Err(AlreadyExists) whenever anything (a symlink included, never followed) is at t; else it creates a new regular file with mode 0600.	a_failed_atomic_write_leaves_the_original_intact, a_symlinked_context_file_is_written_through_to_its_target, the_temp_file_is_never_wider_than_the_context_file_it_becomes	0,8,13	HARD (effects): canonicalize/create_new/OpenOptionsExt::mode/set_permissions/fsync/rename/remove_file, AtomicU64, unix cfg; needs a PlatformSpec for POSIX fs
dangling-link-one-answer	contract	dangling_link, make_room_for, situation, skill_status, install_skill_file, upsert_primer_file	∀ p, let q be the nearest ancestor-or-self of p whose lstat does not fail with NotFound. dangling_link(p) == Some(m) iff q is a symlink and stat(q) fails; m is "a symlink whose target does not exist — left as it is" when q == p, else "under a symlinked directory whose target does not exist — left as it is". When Some(m): make_room_for(p, _), install_skill_file(p, _, _), upsert_primer_file(p, _, _) return Err(m), the link is unchanged and nothing is created at or under its target; situation reports File(Err(m)) and skill_status reports Err(m), never Absent. Install and status give the same answer.	claude_defaults_never_create_over_a_dangling_settings_link	0,3,4,8,13	MEDIUM (effects): Path::ancestors with a find closure, symlink_metadata/metadata, create_dir_all
settings-backup	contract	write_backup, write_backup_with	∀ path, bytes, S = current unix seconds: write_backup(path, bytes) creates, with create_new (never overwrites, never follows a symlink), the first free name among path.bak-S, path.bak-S-1, …, path.bak-S-63; the backup holds bytes, is synced, and has mode(path) when path is a regular file, else 0600. All 64 taken ⇒ Err("...: no free backup name"). write_backup_with(path, fill): fill Err ⇒ the created file is removed and the result is Err containing fill's message; fill Ok ⇒ Ok(candidate), file kept.	the_settings_backup_keeps_the_settings_files_mode, a_backup_whose_bytes_fail_to_go_down_is_not_left_behind	0,7,8	MEDIUM (effects): SystemTime, bounded loop, create_new, FnOnce(&mut File), permissions
rewrite-compare-and-swap	contract	rewrite, AsRead::read, AsRead::still, write_atomically_if	∀ path, backup flag, edit, and every schedule of external saves (each between a try's read and its still() check), rewrite returns one of: (a) Ok(None) when edit returns Ok(None) on the read text: nothing written, no backup. (b) Ok(Some((done, copy))) at some try k ≤ REWRITE_TRIES (3): at try k's still() the file was the same file (dev/ino) with the bytes of text_k, or still absent when text_k was None; resolve(path) holds edit(text_k)'s text; copy == Some(bak) iff backup ∧ text_k is Some, bak holding text_k; every backup of tries 1..k-1 removed. (c) Err(Changed) after 3 tries that each saw a change: the file is as the last external save left it; no backup or temp remains. (d) Err(Read | Refused | Write): the file is as before that try; that try's backup and temp removed. So a save made before still() is never lost. The window between still() and rename is outside this spec, as documented.	(lemma; samples in §2)	0,1,3,4,7,8,13	VERY HARD: bounded retry loop, FnMut closures, fs open/read/metadata (inode), rename, an interleaved writer (two-state over σ)
settings-rmw-serialized	invariant	edit_settings, SETTINGS_RMW	For any two concurrent edit_settings calls in one process on the same path (e.g. remove_aterm_hooks_in ∥ add_claude_defaults_in), the result equals running them one after the other in some order: both succeed with their sequential results (Removed and Added); the final file reflects both edits (no aterm hook command remains, the unset defaults are present); each call uses its own temp name (the WRITES counter makes every temp unique), so neither call's cleanup removes the other's temp.	-	0,8,13	VERY HARD: std::sync::Mutex (poison-tolerant), threads, AtomicU64, fs effects; linearizability
remove-aterm-hooks-file	contract	remove_aterm_hooks, remove_aterm_hooks_in, edit_settings	∀ path, σ, no external writer: no file opens at path (NotFound, a dangling link included) ⇒ Ok(Nothing), untouched(path). Read or parse failure, or strip_marked_hooks Err ⇒ Err naming path, untouched(path), no backup. n = strip_marked_hooks(doc) + [restore_harness_statusline(doc, path)]; n == 0 ⇒ Ok(Nothing), same bytes, no backup. n > 0 ⇒ Ok(Removed{removed: n, backup: b}), resolve(path) holds doc'.render() + "\n", b a new Baks(path) entry holding the old bytes (settings-backup); a failed write ⇒ Err, untouched(path), no backup left. Idempotence: a second call on the file it wrote returns Ok(Nothing) (via json-render-round-trip, and doc' has no aterm hook command and no harness-shaped statusLine; but see §5.10). remove_aterm_hooks_in(h) == remove_aterm_hooks(h.join(".claude/settings.json")).	the_pass_removes_the_hook_entries_aterm_wrote_and_leaves_the_foreign_ones	0,1,3,4,8,13	HARD: fs read/write/rename through rewrite, Mutex guard, closures returning Result<Option<..>>, composition of the pure JSON specs
add-claude-defaults-file	contract	add_claude_defaults, add_claude_defaults_in, edit_settings	∀ path, hold, σ, no external writer (claude-defaults-edit with that hold): no file ∧ parent(path) not a dir ⇒ Ok(Nothing), nothing created. No file, parent a dir, path not a dangling link ⇒ file created, mode 0600, holding render(doc') + "\n" from claude-defaults-edit on Object([]); result Added{keys: all six CLAUDE_DEFAULTS keys in order, backup: None}. path a dangling link ⇒ Err containing "symlink" and "does not exist"; the link is unchanged, nothing created at its target or beside it. File present: edit Err ⇒ Err, untouched(path), no backup; edit Ok(None) ⇒ Ok(Nothing), same bytes, no backup; edit Ok(Some((doc', keys))) ⇒ resolve(path) holds doc'.render() + "\n", result Added{keys, backup: Some(b)}, b holding the old bytes. A failed write ⇒ Err, untouched(path), no backup left. add_claude_defaults_in(h) == add_claude_defaults(h.join(".claude/settings.json"), claude_compact_offers_kept_in(h) != Ok([])).	a_settings_write_that_fails_leaves_no_backup_behind, claude_defaults_create_only_where_claude_code_is, a_save_between_read_and_write_of_the_settings_stands	0,1,3,4,8	HARD: fs effects through rewrite/write_atomically_if, symlinks, permissions, composition with the pure edit
install-skill-file	contract	install_skill_file, read_skill	∀ path, body: read_skill(path, body) == Absent iff fs::read(path) fails with NotFound; any other read error ⇒ Err("unreadable: ..."); otherwise skill_state(String::from_utf8_lossy(bytes), body). install_skill_file: Current ⇒ Ok(Current), Foreign ⇒ Ok(Foreign), both untouched(path); Err ⇒ Err, untouched(path); Absent or Stale ⇒ make_room_for(path, room), then write_atomically(path, body), result Ok(Installed) or Ok(Updated), resolve(path) holds body. So an unmarked file (a non-UTF-8 one included) and an unreadable one are never written.	auto_prime_never_overwrites_a_foreign_skill	0,1,3,4,8	MEDIUM (effects): fs read, lossy UTF-8, enum match, composition with atomic-write
upsert-primer-file	contract	upsert_primer_file	∀ path, ∀ b with WFB(b), no external writer: no file ⇒ make_room_for(path, room), resolve(path) created holding b, result Created. File text c with find_block(c) Err ⇒ Err, untouched(path). upsert_block(c, b) == Ok(None) ⇒ Current, untouched(path). upsert_block(c, b) == Ok(Some(c')) ⇒ resolve(path) holds c', result Replaced if c had a block, else Appended. Non-UTF-8 text or a read error ⇒ Err("unreadable: ..."). A directory at path ⇒ Err, directory and contents untouched. With external saves, the semantics are rewrite-compare-and-swap's.	(lemma; samples in §2)	0,1,3,4,8,13	MEDIUM-HARD: a closure into rewrite, composition with upsert-block and atomic-write, fs
auto-prime-pass	contract	auto_prime_in, auto_prime_with, auto_prime, AutoPrime::changed, AutoPrime::errors, detected	∀ home, roots, switch, σ: auto_prime_in(home, roots, switch) == AutoPrime{agents, summary}. agents has one row per a ∈ AF with Det(a), in registry order, computed via Primed::upsert over Readers::of(home, roots, &[]) (so a shared context file is upserted once with Readers::block(a), and named once in wrote) plus skills = [install_skill_file(J(s.path), s.body) | s ∈ skills_for(a.name)], each attempted whatever primer returned; retired = [sweep_retired_skill(p) | r ∈ retired_skills_for(a.name), (p, _) ∈ retired_places(home, roots, r)]; hooks = remove_aterm_hooks(J(".claude/settings.json")) iff a.name == "claude". Row = {agent, product, outcome: Fold(primer, skills, retired, hooks), wrote: W}, W = concat of: display_path(home, roots, through.file) if primer actually wrote (Created|Appended|Replaced); display_path(home, roots, s.path) for each skill Ok(Installed|Updated); shown + " (" + r.what + ", removed)" for each retired place swept Ok(true); "~/.claude/settings.json (N aterm hook or statusLine entr{y|ies} removed; backup B)" if hooks is Ok(Removed{N, B}). summary: agents == [] ⇒ "agent primer: no coding agents detected (looked for " + join(", ", display_path(home, roots, a.dir) for a ∈ AF) + ")"; else "agent primer: " + join(", ", agent + " " + word(outcome)), followed by " — " + switch + "; wrote " + join(", ", all W) when that is non-empty. changed() iff some W != []. errors() == [(agent, msg) | outcome == Error(msg)]. Nothing outside J(a.dir) of a detected a is created or modified except through symlinks already there; no undetected agent's directory is created. Every folder a's writes make is strictly below J(a.dir) and is made with its parent already there (make_room_for with Room::Below(J(a.dir)): one create_dir per level, never create_dir_all), so a J(a.dir) taken during the pass is not made again, and a write a starts after the take puts nothing where it went: each fails (a missing file reads as absent and its folder cannot be made), so a's row is Error when any write is attempted after the take, and wrote lists only the writes finished before it. A write the take catches mid-way (its temp file made, not yet renamed) fails too and may leave that temp in the folder's new place; forget's tombstone removal takes it. auto_prime(home) == auto_prime_in(home, R, off_switch(R.xdg)), R = Roots::of_env().window(): the non-empty $XDG_CONFIG_HOME and codex None, because aterm strips CODEX_* from the shells it spawns, so the window's $CODEX_HOME reaches no codex in its tabs.	auto_prime_upgrades_every_older_block_in_place, auto_prime_replaces_completion_rules_in_v24_and_supervise_v3, a_corrupt_block_is_an_error_row_that_stops_nothing_else, relocated_config_drives_detection_install_and_remove_in_one_root, an_agent_folder_taken_mid_pass_is_never_made_again	0,1,3,4,7,8,13	VERY HARD: loop over the registry with fs effects, Vec<String> building, zip, format!, composition of every per-file spec, env read at the boundary
auto-prime-idempotent	invariant	auto_prime_in	∀ home, roots, switch, σ: for two consecutive passes P1, P2 with no other writer between, every row of P2 has wrote == [] and outcome ∈ {Unchanged, SkippedForeign, Error(_)}; P2 modifies no file and makes no backup; P2.changed() is false; P2.summary has no " — " + switch suffix. Follows from upsert-block idempotence, install-skill-file on a body it wrote (Current), retired-skill-sweep on a path it emptied (Ok(false)), and remove-aterm-hooks-file idempotence (§5.10 is the exception).	auto_prime_primes_detected_agents_only_and_is_idempotent	0,1,2,4,8	HARD: two-run fs reasoning (two-state), composition of idempotence lemmas
summary-log-cap	table-property	auto_prime_in, off_switch, aterm_log::MAX_RECORD_BYTES	For every pass with roots.xdg == None and no Error row, the byte offset where off_switch(None) ends in summary is ≤ aterm_log::MAX_RECORD_BYTES (512); every row's word precedes the switch and the file list comes last. The offset is 14 + Σ(len(name) + 1 + len(word)) + 2(k-1) + len(" — ") + len(off_switch(None)), k ≤ 5, each word one of the four non-error outcome words; its max over those choices is ≤ 512. A fresh claude+codex first pass lists sixteen paths, which pass the cap: the record cut at 512 bytes loses only file names.	-	0,1,3,12	EASY-MEDIUM: length arithmetic over const strings, a finite max; cross-crate constant (§5.6)
cli-dispatch	contract	agents_report, agents_report_with, select, usage, RETIRED_AGENTS	∀ home, roots, args, sub = args[0] (default "status"), names = args[1..]: (usage(), 0) for sub ∈ {"-h","--help","help"}; ("aterm agents: unknown command '" + sub + "'\n\n" + usage(), 2) for any other sub ∉ {status, install, remove, primer}; when sub ∈ {install, remove, primer} and some n matches RETIRED_AGENTS ("gemini", "gemini-cli" case-insensitively), exit 2 naming agy ("Gemini CLI is no longer supported: agy replaces it, and `aterm agents install agy` primes the same ~/.gemini/GEMINI.md"); when some other n ∉ names(AF), exit 2 with a message containing "unknown agent '" + n + "'" and usage(); exit 2 for primer with > 1 name; (primer_block(names.first()), 0) for primer with ≤ 1 known name; exit 0 for status always; install/remove exit 1 iff some row printed ERROR, else 0. The primer, help and usage-error paths do not touch the filesystem.	agents_usage_errors_exit_2_and_primer_prints_the_block, the_retired_gemini_cli_is_refused_by_name_and_its_file_is_agys	0,1,3,4,7	MEDIUM: slice patterns on &[String], match on &str, Result<Vec<&AgentFile>> collect, format!
cli-install-gate	contract	agents_report_with	install with names == [] goes over every a ∈ AF: ¬Det(a) ⇒ prints format!("{:<9} {:<30} skipped — no {} (not detected; name it to force)", a.name, display_path(a.file), display_path(a.dir)) and creates nothing under J(a.dir), the dir included; Det(a) ⇒ runs Primed::upsert(a, Readers::block(a)) then install_skill_file for each skill, printing one verdict line each from {"already installed", "installed (new file)", "installed", "updated", "skipped — not an aterm-managed file (yours)", "ERROR: ..."} (with Readers::note(a) on context-file rows), then sweep_retired_rows (retired-skill-sweep). install with names forces every named agent, detected or not: a missing dir and file are created (make_room_for with Room::Anywhere) and the file == Readers::block(a). Corollary: a second install over the same state prints "already installed" exactly Σ(1 + |skills_for(a.name)|) times over the installed agents.	install_touches_only_detected_agents_unless_forced	0,1,3,4,7,8	HARD: loop with fs effects, padded formatting, composition
cli-status-report	contract	agents_report_with, situation, skill_status, auto_prime_note	status (and args == []) prints, ∀ a ∈ AF in order, format!("{:<9} {:<30} {state}{}", a.name, display_path(a.file), readers.note(a)) with state ∈ {"not detected (no " + display_path(a.dir) + ")" for NotDetected, "absent (no context file yet)" for NoFile, absent/installed/stale for File(Ok(Absent|Current|Stale)), "newer (a later aterm's; left as is)" for File(Ok(Newer)), "ERROR: " + e for File(Err(e))}; then ∀ s ∈ skills_for(a.name), detected or not, format!("{:<9} {:<30} {st}", "  skill", display_path(s.path)) with st ∈ {installed, stale, "foreign — yours, left alone", absent, "ERROR: " + e}; then ∀ r ∈ retired_skills_for(a.name) and (p, shown) ∈ retired_places(home, roots, r) a "  retired" row for p only when it is marked (r.what + ", no longer written (`aterm agents install` removes it)") or unreadable ("ERROR: unreadable: " + e). Output ends "\n`aterm agents install` brings every detected agent's primer and skills up to date.\n" + auto_prime_note(roots.xdg) + "\n". Exit 0; read-only (σ' == σ).	-	0,1,3,7,8	MEDIUM-HARD: read-only fs, padded formatting, enum match
cli-remove	contract	agents_report_with, Removal::rows, Removal::also_read_by, rewrite, remove_block, read_skill	remove goes over every selected a (all of AF when no names): context file via rewrite(J(a.file), false, remove_block) once per distinct lands_on target (later rows sharing that file echo the same verdict), verdict "nothing to remove" (no file or no block), "removed" (J(a.file) now holds the remove_block result), "ERROR: ..." for a corrupt block (file untouched), suffixed with readers.note(a). Each skill: Absent ⇒ "nothing to remove"; Foreign ⇒ "kept — not an aterm-managed file (yours)", untouched; Current or Stale ⇒ when some detected o ∉ the selected agents, o ≠ a, has a skill t with lands_on(J(t.path)) == lands_on(J(s.path)), "kept — " + o.name + " reads it too (`aterm agents remove " + a.name + " " + o.name + "` removes it)", untouched; else deleted, "removed"; Err ⇒ ERROR. Then sweep_retired_rows (retired-skill-sweep). When W(a.dir) is a dir and lands_on(W(a.dir)) ≠ lands_on(J(a.dir)) (codex under $CODEX_HOME, with ~/.codex there), the same rows follow for a with roots.window(), so aterm's files in ~/.codex go too. For claude a hooks row appears only when remove_aterm_hooks returns something other than Ok(Nothing). Output ends "\n" + auto_prime_note(roots.xdg) + "\n"; install's output never contains auto_prime_note(roots.xdg). Corollary (no external writer): after install of a over a user file c with no block, remove of a leaves J(a.file) == canon(c) and deletes every skill file the install wrote; under external saves every save stands (rewrite-compare-and-swap).	a_save_between_read_and_write_of_a_context_file_stands	0,1,3,4,8,13	HARD: fs effects (remove_file, rewrite), closures, composition with remove-block-round-trip
status-line	contract	status_line, status_line_with	∀ home, roots, σ: status_line_with(home, roots) == join(", ", one row per a ∈ AF), read-only. Row = a.name + " " + w, w ∈ {"not detected" for NotDetected, "absent" for NoFile or Absent, "installed" for Current, "stale" for Stale, "newer" for Newer, "ERROR: " + e}. When Det(a), the row is followed, ∀ s ∈ skills_for(a.name) with skill_status ≠ Ok(Current), by " (skill " + doc_label(s.path) + " " + w' + ")", w' ∈ {"absent", "stale", "yours, left alone", "ERROR: " + e}. When no error message contains '\n', the line contains no '\n'.	-	0,1,3,7,8	MEDIUM-HARD: read-only fs, map/collect/join, continue in a loop, write! to String
shared-file-readers	contract	lands_on, landing, ignores_case, Readers::{of, sharing, way, block, note}, Primed::upsert, situation	lands_on(p) resolves p across up to LINK_HOPS (32) symlink hops even before the leaf or a linked folder exists, canonicalizing existing ancestors and lowercasing non-existent leaf names on case-insensitive volumes (ignores_case). Readers::of(home, roots, named) collects every primed agent (detected or in named) plus any other agent whose lands_on(J(file)) matches a primed agent's: sharing(a) lists the other readers of a's file, block(a) builds block_with of all readers' addenda in registry order (at marker ≥ v34, so a v33 row-by-row build leaves a shared file's block as Newer), note(a) formats " (shared with ...)", way(a) routes a dangling link through a primed non-dangling reader sharing the target (and situation reports NoFile rather than Err when another primed reader makes the target live), and Primed::upsert writes each shared target at most once per pass while failing closed on symlink loops.	agents_that_share_one_context_file_get_one_block, a_row_by_row_aterm_leaves_a_shared_files_block, a_file_not_made_yet_is_one_file_through_a_linked_folder, a_link_made_before_its_file_is_that_file_from_the_first_pass, an_install_through_a_folder_link_made_before_its_folder_makes_one_file, a_named_install_shares_the_file_a_folder_link_waits_for, status_reads_a_link_another_reader_makes_live_as_the_file_not_made_yet, a_link_spelled_in_another_case_is_its_file_where_case_is_ignored	0,1,2,3,4,7,8,13	HARD: recursive symlink-hop resolution with case-folding probe, Readers/Primed deduplication, fs effects
taught-surface-and-retired-names	table-property	taught_ctl_verbs, PRIMER_BODY, CLAUDE_ADDENDUM, CODEX_ADDENDUM, skills_for	taught_ctl_verbs(text) normalizes whitespace (including line breaks inside backtick spans), extracts every `aterm ctl …` invocation, skips global flags (--sock/--pid/--timeout/--host and their arguments) and session/placeholder tokens (@..., "...", '...', -..., $..., <..., [...), and returns each taught (command, verb). Across PRIMER_BODY, every agent addendum and every bundled skill body: (1) every verb returned by taught_ctl_verbs is served by aterm_types::control_verbs::spec or aterm_ctl::CLIENT_HELP_VERBS; (2) no primer block or skill body contains the retired completion token "ATERM-TASK-DONE"; (3) every session-log op/kind/key/out word in code spans of DRIVE_SKILL_BODY, SUPERVISE_SKILL_BODY and the help lines of log/brief/timeline/exits/subscribe belongs to aterm_slog's vocabulary.	the_primer_and_the_skills_teach_served_verbs, nothing_aterm_installs_names_the_retired_token, a_taught_command_is_read_to_its_verb_across_a_line_break, every_log_word_a_skill_names_is_the_logs, every_log_word_the_log_verbs_help_names_is_the_logs	0,1,3,5,7,12,13	MEDIUM: whitespace/backtick scanning loops, cross-crate verb and slog vocabulary tables; decidable by evaluation on bundled constants
```

## 2. Test → spec map (TSV)

`primary` is the spec that retires the test. `also` lists the other specs the same test samples.
A test is deleted only when its primary and every `also` spec are proved. The two tests of §4
were deleted, so no row is `(none)` now.

```tsv
file	test	primary	also
src/lib.rs	the_top_effort_key_is_the_vendors_spelling	top-effort-key-spelling	
src/lib.rs	every_bundled_doc_is_marked_and_parses_for_its_agent	bundled-docs-valid	
src/lib.rs	each_variable_moves_only_its_own_rows	xdg-join-and-display	
src/lib.rs	skill_state_distinguishes_ours_from_the_users	skill-state	
src/lib.rs	unterminated_block_fails_closed	find-block	
src/lib.rs	upsert_is_idempotent_and_remove_round_trips_to_the_original	remove-block-round-trip	upsert-block
src/lib.rs	a_newer_block_is_left_as_is	upsert-block	find-block
src/lib.rs	only_an_aterm_executable_running_the_hook_verb_or_the_harness_mark_is_aterms	aterm-hook-command	sh-word-round-trip
src/settings_json.rs	malformed_documents_are_refused	json-grammar	
src/settings_json.rs	a_key_written_twice_reads_as_json_parse_reads_it	json-dupkeys	json-render-round-trip
src/settings_json.rs	a_settings_document_round_trips_in_order	json-render-round-trip	
src/settings_json.rs	strings_escape_and_unescape	json-render-round-trip	json-grammar
src/lib.rs	claude_defaults_add_only_what_is_unset_and_not_held_back	claude-defaults-edit	efforts-set-by-hand, survey-rate-set-by-hand, add-claude-defaults-file
src/lib.rs	claude_defaults_create_only_where_claude_code_is	add-claude-defaults-file	claude-defaults-edit
src/lib.rs	claude_defaults_add_the_compact_window_only_where_unset_and_no_offer_was_kept	compact-offers-kept	claude-defaults-edit, add-claude-defaults-file
src/lib.rs	compact_state_reads_are_bounded_before_defaults_are_chosen	compact-offers-kept	
src/lib.rs	compact_state_accepts_regular_symlinks_and_refuses_open_nonregular_files	compact-offers-kept	
src/lib.rs	an_oversized_compact_state_holds_only_its_default_back	compact-offers-kept	claude-defaults-edit, add-claude-defaults-file
src/lib.rs	the_harness_statusline_is_given_back_from_the_installers_byte_copy	restore-harness-statusline	
src/lib.rs	the_statusline_install_displaced_wins_over_a_stale_byte_copy	restore-harness-statusline	remove-aterm-hooks-file
src/lib.rs	strip_marked_hooks_keeps_pre_existing_empty_groups_and_events	strip-marked-hooks	remove-aterm-hooks-file
src/lib.rs	the_pass_removes_the_hook_entries_aterm_wrote_and_leaves_the_foreign_ones	remove-aterm-hooks-file	strip-marked-hooks, restore-harness-statusline, auto-prime-pass, auto-prime-idempotent
src/lib.rs	a_failed_atomic_write_leaves_the_original_intact	atomic-write	install-skill-file, upsert-primer-file
src/lib.rs	a_symlinked_context_file_is_written_through_to_its_target	atomic-write	dangling-link-one-answer, upsert-primer-file, cli-status-report, status-line
src/lib.rs	the_temp_file_is_never_wider_than_the_context_file_it_becomes	atomic-write	
src/lib.rs	claude_defaults_never_create_over_a_dangling_settings_link	dangling-link-one-answer	remove-aterm-hooks-file, add-claude-defaults-file
src/lib.rs	the_settings_backup_keeps_the_settings_files_mode	settings-backup	
src/lib.rs	a_backup_whose_bytes_fail_to_go_down_is_not_left_behind	settings-backup	
src/lib.rs	a_settings_write_that_fails_leaves_no_backup_behind	add-claude-defaults-file	rewrite-compare-and-swap, settings-backup
src/lib.rs	a_save_between_read_and_write_of_the_settings_stands	add-claude-defaults-file	rewrite-compare-and-swap, settings-backup, claude-defaults-edit
src/lib.rs	auto_prime_never_overwrites_a_foreign_skill	install-skill-file	fold-outcome, auto-prime-pass, auto-prime-idempotent, status-line
src/lib.rs	auto_prime_primes_detected_agents_only_and_is_idempotent	auto-prime-idempotent	fold-outcome, auto-prime-pass, summary-log-cap
src/lib.rs	auto_prime_upgrades_every_older_block_in_place	auto-prime-pass	upsert-block, upsert-primer-file, fold-outcome, auto-prime-idempotent
src/lib.rs	a_corrupt_block_is_an_error_row_that_stops_nothing_else	auto-prime-pass	upsert-primer-file, fold-outcome, auto-prime-idempotent
src/lib.rs	an_agent_folder_taken_mid_pass_is_never_made_again	auto-prime-pass	upsert-primer-file, install-skill-file, fold-outcome, cli-install-gate
src/lib.rs	relocated_config_drives_detection_install_and_remove_in_one_root	auto-prime-pass	cli-remove
src/lib.rs	install_touches_only_detected_agents_unless_forced	cli-install-gate	
src/lib.rs	agents_usage_errors_exit_2_and_primer_prints_the_block	cli-dispatch	
src/lib.rs	a_save_between_read_and_write_of_a_context_file_stands	cli-remove	rewrite-compare-and-swap, upsert-primer-file, cli-install-gate
src/lib.rs	the_gemini_cli_command_an_earlier_aterm_wrote_is_deleted_and_the_users_is_kept	retired-skill-sweep	fold-outcome, auto-prime-pass, auto-prime-idempotent, cli-install-gate, cli-status-report, cli-remove
src/lib.rs	the_codex_prompt_an_earlier_aterm_wrote_is_deleted_and_the_users_is_kept	retired-skill-sweep	auto-prime-pass, auto-prime-idempotent, cli-status-report, cli-install-gate, cli-remove
src/lib.rs	codex_gets_every_aterm_skill_where_codex_reads_skills	skill-registry	xdg-join-and-display, install-skill-file, cli-install-gate, cli-remove, auto-prime-pass
src/lib.rs	every_skill_md_passes_codexs_frontmatter_check	bundled-docs-valid	
src/lib.rs	a_skill_two_agents_can_share_is_one_body	skill-one-body	bundled-docs-valid, skill-registry
src/lib.rs	a_skills_folder_two_agents_share_settles_and_outlives_one_agents_remove	cli-remove	skill-one-body, auto-prime-idempotent, cli-install-gate
src/lib.rs	each_program_is_taught_the_wait_its_shell_can_do	primer-block-shape	
src/lib.rs	the_retired_gemini_cli_is_refused_by_name_and_its_file_is_agys	cli-dispatch	skill-registry, cli-install-gate
src/lib.rs	auto_prime_replaces_completion_rules_in_v24_and_supervise_v3	auto-prime-pass	upsert-block, install-skill-file, auto-prime-idempotent
src/lib.rs	agents_that_share_one_context_file_get_one_block	shared-file-readers	primer-block-shape, auto-prime-pass, auto-prime-idempotent, cli-status-report, cli-install-gate, cli-remove, status-line
src/lib.rs	a_row_by_row_aterm_leaves_a_shared_files_block	shared-file-readers	find-block, upsert-block, primer-block-shape, auto-prime-pass
src/lib.rs	a_file_not_made_yet_is_one_file_through_a_linked_folder	shared-file-readers	auto-prime-pass, auto-prime-idempotent
src/lib.rs	a_link_made_before_its_file_is_that_file_from_the_first_pass	shared-file-readers	dangling-link-one-answer, auto-prime-pass, auto-prime-idempotent, status-line
src/lib.rs	an_install_through_a_folder_link_made_before_its_folder_makes_one_file	shared-file-readers	cli-install-gate, primer-block-shape
src/lib.rs	a_named_install_shares_the_file_a_folder_link_waits_for	shared-file-readers	cli-install-gate, cli-status-report
src/lib.rs	status_reads_a_link_another_reader_makes_live_as_the_file_not_made_yet	shared-file-readers	dangling-link-one-answer, cli-status-report, status-line, cli-install-gate
src/lib.rs	a_link_spelled_in_another_case_is_its_file_where_case_is_ignored	shared-file-readers	auto-prime-pass, auto-prime-idempotent, status-line
src/lib.rs	the_primer_and_the_skills_teach_served_verbs	taught-surface-and-retired-names	
src/lib.rs	nothing_aterm_installs_names_the_retired_token	taught-surface-and-retired-names	
src/lib.rs	a_taught_command_is_read_to_its_verb_across_a_line_break	taught-surface-and-retired-names	
src/lib.rs	every_log_word_a_skill_names_is_the_logs	taught-surface-and-retired-names	
src/lib.rs	every_log_word_the_log_verbs_help_names_is_the_logs	taught-surface-and-retired-names	
```

## 3. Coverage

- The crate has 61 live tests (`src/lib.rs` 57, `src/settings_json.rs` 4). Every one maps to one of
  the 47 specs; none is a non-spec sample (§4).
- The biggest collapses: `shared-file-readers` (8 primary), `taught-surface-and-retired-names` (5
  primary), `auto-prime-pass` (5 primary, 9 more shared), `compact-offers-kept` (4 primary),
  `add-claude-defaults-file` (3 primary, 4 shared), `atomic-write` (3).
- Four specs are lemmas with no primary test: `fold-outcome`, `sh-word-round-trip`,
  `rewrite-compare-and-swap` and `upsert-primer-file`. Their tests are retired by the composed specs above them, so they must be
  proved first in their chains, but no test is deleted on their account alone.
- `the_pass_removes_the_hook_entries_aterm_wrote_and_leaves_the_foreign_ones` samples five specs, so
  it goes last. `relocated_config_drives_detection_install_and_remove_in_one_root` sampled six
  before the audit cut its status half; it now samples `auto-prime-pass` and `cli-remove`.

## 4. Tests that are not spec samples

None left: the two tests this section marked **delete** are deleted. No census guard replaced the
banned-word list of the no-hook test.

No test is a pure regression of a removed bug with no general property.

## 5. Implied specs that look wrong, or disagree with the code

1. **Resolved — module contract doc.** The stale byte-for-byte preservation prose in `src/lib.rs`
   was pruned; `remove-block-round-trip` states the canonicalized newline round trip (`canon(c)`).
2. **Resolved — `strip_marked_hooks` empty groups.** `strip_marked_hooks` now prunes only hook
   groups, event arrays, and the top-level `"hooks"` object that it emptied itself, leaving
   pre-existing empty groups, empty event arrays, and an empty `"hooks": {}` intact
   (`strip_marked_hooks_keeps_pre_existing_empty_groups_and_events`).
3. **Resolved — dead guard in `find_block`.** Removed the redundant `trimmed != MARK_END` check
   after `trimmed.starts_with(MARK_PREFIX)`.
4. **`doc_label`'s degenerate case is pinned, not principled.** A top-level SKILL-stem path
   (`SKILL.md`) is labelled with the whole path, extension included. The "no `.` in a label" property
   holds only for registered paths, and the test's degenerate-case asserts pin that fallback.
5. **Resolved — `remove_aterm_hooks` idempotence on `statusline.user`.** `restore_harness_statusline`
   now filters `statusline.user` through `!is_harness_statusline_command`, matching the `.aterm-harness.orig`
   branch so a harness-marked `statusline.user` is removed once and subsequent passes return
   `HookRemoval::Nothing` without rewriting `settings.json` or adding backups.
6. **Resolved — deterministic read-error verdict in `auto_prime_never_overwrites_a_foreign_skill`.**
   The test now stages a directory at the skill path to verify `Outcome::Error(_)` and untouched
   contents unconditionally on every OS and privilege level, and under Unix `0o000` asserts
   `Outcome::Error(_)` strictly whenever `std::fs::read` reports the path unreadable.
7. **The vendor's key must not be written joined.** Any SPECS.md, proof or clause text that states
   `top-effort-key-spelling` must spell the key with the hex escape (`b"ultra\x63ode"`), as the test
   does, because the export baseline bans the joined word in every exported file.

## 6. Proof order (easiest high-value first)

No spec is blocked by gap 0 alone; every one also needs gap 1 (`&str`/`String`/enum carriers) or
gap 8 (effects). The order below takes the closest to gap-0-only first, then the pure string core
that most later specs compose.

1. **`top-effort-key-spelling`** (gaps 0, 1). A byte-slice equality over a `concat!` constant,
   decidable by evaluation. It is the smallest probe of gap 1 on this crate and retires its test outright.
2. **The constant-table properties: `primer-gate-line`, `skill-registry`, `bundled-docs-valid`**
   (gaps 0, 1, 3, 5; `bundled-docs-valid` also 7, 14). All are facts about `include_str!`/`concat!`
   constants and a `match` returning const slices, so they close by evaluation once string carriers
   exist. Together they retire 1 test.
3. **`primer-block-shape`, `off-switch-phrase`, `harness-state-of`, `skill-state`** (gaps 0, 1, 3,
   plus 13 for the closures, 8 for the Path model). Straight-line string building and matching with
   no loops over input, each retiring its own tests.
4. **`find-block` → `upsert-block` → `remove-block-round-trip`** (gaps 0, 1, 3, 4, 5, 7). The pure
   core of the crate: one line loop with an obvious invariant, then two string transforms by
   composition. It retires 3 tests and is the lemma under `upsert-primer-file`, `cli-remove` and
   `auto-prime-pass` (§5.1).
5. **`fold-outcome`** and **`doc-label`** (gaps 0, 1, 3, 7, 13). Small enum and slice folds; the
   lemmas `auto-prime-pass` and `status-line` need.
6. **`sh-word-round-trip` → `aterm-hook-command`** (gaps 0, 1, 3, 5, 7, 13). High value: it is the
   predicate that decides which hook entries in a person's settings are deleted.
7. **JSON: `json-grammar`, `json-dupkeys`, `json-render-round-trip`** (adds gaps 2, 11), then the
   pure settings edits `efforts-set-by-hand`, `survey-rate-set-by-hand`, `claude-defaults-edit`,
   `strip-marked-hooks`, and `compact-offers-kept` (its file read needs gap 8).
8. **Effects (gap 8): `atomic-write`, `settings-backup`, `dangling-link-one-answer`,
   `install-skill-file`, `upsert-primer-file`, `restore-harness-statusline`,
   `remove-aterm-hooks-file`, `add-claude-defaults-file`, `rewrite-compare-and-swap`,
   `shared-file-readers`**, then the compositions `auto-prime-pass`, `auto-prime-idempotent`, the
   `cli-*` specs and `status-line`. `settings-rmw-serialized` (gap 13, threads) and `summary-log-cap` /
   `primer-names-env-deny-prefixes` / `taught-surface-and-retired-names` (gap 12, cross-crate constants)
   go when those gaps close.

Gap histogram (number of the 47 specs each gap blocks): 0 → 47, 1 → 43, 3 → 41, 7 → 24,
8 → 21, 13 → 21, 4 → 19, 5 → 19, 2 → 8, 11 → 4, 12 → 3, 14 → 1.
