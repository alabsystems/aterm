<!-- Copyright 2026 Andrew Yates -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# aterm-control: spec recovery from its 77 unit tests

> Status 2026-10-16, after the merge with the tests-and-gates audit: the rows naming 30 tests it
> deleted are gone, and the spec rows no longer cite those tests. Counts elsewhere in this file
> were taken before.

> Status 2026-10-16: this is the crate's migration worklist. Each of the 32 specs below is proved
> through Trust/Clean about the compiled Rust function, then the tests it subsumes are deleted. The 4
> checks in §4 that sampled no spec are deleted; no spec-sample test has been deleted yet. Added since
> the recovery: `visible-text-collapse` and the 5 tests §2 maps from `src/wire.rs`'s three
> `visible-text-collapse` rows to `src/selection.rs`'s two `blocktext-format` rows, and the 4
> tests the asciicast codec, the replay engine and its fork brought (`write_then_parse_round_trips`,
> `every_replay_sets_archive_posture_and_policy`, `forked_variants_equal_full_folds`,
> `offline_drift_sees_the_handoff`).
> `selector-parse` left this crate with `host::Selector::parse`: the wire's selector grammar is
> aterm-types' (`control_request`, spec `cr-request-grammar`, digits-only, which settles §5.1), and the
> roster check (R1) reads ids through it. 31 specs remain. `SessionHost::write_input`,
> `placements` and `HostCapabilities::input_sink` left the trait (no production caller; the
> control-server carve is not scheduled): clause (W) and `a_host_without_a_sink_never_answers_ok`
> went with them, and the unserved-sid arm of the write check is `check_unserved_sid`, on
> `with_terminal_mut`.

Source: `crates/aterm-control` (read only). I read every `#[test]` (src/cast_drift.rs 25,
src/conformance.rs 35, src/selection.rs 14, src/host.rs 2, src/wire.rs 1 = 77) and the code each one
reaches. Nothing was compiled or run. The claims below come from reading the code.

Result: **32 specs**, covering **82 tests**. **4 tests are not spec samples** (§4). Three specs are
mostly about code in other crates and should be re-homed there, with this crate citing the theorem
(gap 12): `json-escape-wire` (aterm-codec; the functions here are re-exports),
`word-cells-are-drawn-cells` and `paint-equals-copy` (aterm-core).

Blocking gaps are the row numbers of the gap table in
`AUDIT-trust-for-test-migration-2026-10-15.md` (historical internal audit) §7.
Gap 0 (the installed seal publishes no Clean-discharged row) is the prerequisite for every row, so it is
listed on every spec. Seal `150efe4a`, built locally, closes it for local use; admitting it to the
machine's `trust` channel is owner-gated. No spec here is blocked by gap 0 alone: the narrowest rows
also need gap 2 and 3 (`drift-settle-cut`) or gap 1 (everything else), because the crate's surface is
`&str` in and `String` out.

## 0. Notation used in the statements

### Hosts and the conformance suite

- `H` ranges over every `impl SessionHost`; `s` is a sid. `Served(H,c)` ⇔ `H.with_terminal(c,_) = Some(_)`.
  `Term(H,s)` is the `Terminal` that `H` serves for `s`.
- `Quiescent(H)`: nothing outside the call mutates `H`'s state while the call runs (no producer thread,
  no scroll).
- `HostContract(H,s)` is the trait contract that `host.rs` documents and `conformance.rs` samples:
  - **(R0)** `¬caps.roster` ⇒ `sessions() = []` and `resolve(sel) = None` for every `sel`.
  - **(R1)** `caps.roster` ⇒ `sessions()` is non-empty, strictly ascending by sid and contains `s`. Every
    row `e` has `aterm_types::control_request::Sid::parse(e.id) = Some(_)` and `e.parent ≠ Some("")`.
    `resolve(Local(e.sid)) = resolve(Id(e.id)) = Some(e.sid)`. `resolve(sel) = None` for every `sel`
    that names no row.
  - **(S)** `Served(c)` ⇔ `with_terminal_mut(c,_) ≠ None` ⇔ `with_terminal_as_it_stands(c,_) ≠ None`, for
    every `c`.
    `request_redraw(c)` and `subscribe(c)` never touch `s`'s state when `c ≠ s`.
  - **(K)** retention: an effect of `with_terminal_mut(s,f)` is seen by every later `with_terminal(s,_)`,
    `with_terminal_as_it_stands(s,_)` or `with_terminal_mut(s,_)`, because there is one engine per sid.
    `with_terminal` may first wait for a whole frame of the program's (`aterm-gui`'s host does, bounded),
    and `blocktext`, `selection`, `copy` and `select` say ` frame=open` for one still unfinished under
    its hold; `with_terminal_as_it_stands` never does, and is what `blocks`, `blocks --json` and `wait`
    read through.
  - **(C)** `capabilities()` is constant during a call.
- `Pred_X(H,s)`: the exact decidable predicate each `check_X` evaluates (spec `conf-check-predicates`).
- Long-term route: lift `HostContract` onto the trait as per-impl obligations, prove it for the production
  hosts (aterm-gui's `GuiHost`, any daemon host), then delete the suite's meta-tests. `MemoryHost` is
  `#[cfg(test)]`-only. The suite sits behind feature `conformance` and has dev-only callers (aterm-gui
  tests, the `aterm-redraw-conformance` dev bin).

### Verbs

- `B(t)` = `t.all_blocks()` in iteration order (oldest first).
- `last_n(B, r)` = `B[|B|-n..]` if `r.trim().parse::<usize>() = Ok(n)` and `n < |B|`; otherwise `B`.
- `opt_-(x)` = `x.to_string()` or `-`. `opt_null(x)` = `x.to_string()` or `null`.
- `state_word`: PromptOnly→`prompt`, EnteringCommand→`entering`, Executing→`executing`,
  Complete→`complete`, other→`unknown`.
- `visible_char(c)` = `' '` if `c = '\0'` or `c.is_control()`; otherwise `c`.
- `NO_SESSION` = `"ERR no such session\n"`. `USAGE` is the verb's usage line. The constants
  `WAIT_MAX_MS = 600000`, `WAIT_DEFAULT_MS = 30000`, `MAX_RUNS = 16`, `RUN_GAP_SECS = 0.25`,
  `MAX_JSON_DEPTH = 32` and `WORD_WINDOW_MAX_ROWS = 64` are the crate's.
- `TtyActive(c)` ⇔ `c ≤ 0x1F`, or `0x7F ≤ c ≤ 0x9F`, or `c ∈ {U+061C, U+200E, U+200F}`, or
  `U+2028 ≤ c ≤ U+202E`, or `U+2066 ≤ c ≤ U+2069`. This is `aterm_codec::tty::is_active`.
- `pct(s)`: aterm-codec's percent encoder. Its output has no space or newline.
- `<lex`: lexicographic order on `(row, col)`.

### Gestures (aterm-core facts used)

- `LL(row)` = `Terminal::logical_line_span(row)` = `(f,l)`.
- `LRC(f',l')` = `Terminal::line_range_cells(f',l')` = `(text, cells)`.
- `SW` = `smart_word_in_cells(smart_rules(), text, cells, g)`.
- `cell(g)` = `(first_row + g / cols, g mod cols)`. The inclusive last cell is `cell(max(end-1, start))`.
- `Whole(t,row,col)` = `SW` over `LRC(LL(row))` with `g = (row-f)·cols + col`, mapped by `cell`.
- `Copy(t)` = `selection_to_string`. `Paint(t)` = per row, the text of the cells `TextSelection::contains`
  marks, joined with `\n` only before a row where `¬row_continues_previous`.

### Cast drift

- `Fold(h, ev, seed, mask, E)` = the `Replay` that `fold()` returns (visible rows, cursor, geometry, lossy
  journal). Row equality is `diff_rows`: compare `trim_end`, and a missing row reads as `""`.
- `Runs` = `group_runs`. `Marks` = `scan_marks`. `Anchor` = the event index of the last clear when no alt
  toggle comes at a later `(event, offset)`; otherwise `None`. A **clear** is `ESC[2J`, `?1049h` or
  `?1047h`; a **toggle** is `?1049h/l`, `?1047h/l` or `?47h/l`.
- `Lossy'` = `Faithful.lossy` restricted to indices after the last toggle event.
  `Lossy''` = `[i ∈ Lossy' | Anchor = None ∨ i > Anchor]`.
- `Eligible` = the candidate list built in `audit()`. `Faithful` = `Fold(mask = None, journal on)`.
  `Stable` = `Fold(apply_mask(Eligible))`, or `Faithful` when `Eligible` is empty.
  `Only_r` = `Fold(apply_mask([r]))`.
- `LiveRec(L, h, ev, p, a, u, l)`: `L = Terminal::new(max(h.height,1), max(h.width,1))` with console-host
  posture `p` and resize law `l` (`Terminal::set_resize_law`, one of `h.resize_laws`), entered alt iff `a`,
  then fed `ev` in order. Output means `process`. Resize means: drop the resize undo first iff `¬u`, then
  resize under policy `p`.
- **Engine resize law** (aterm-core, external; citing it is gap 12): on the alt screen with a non-blank row
  below the cursor, a one-row shrink then grow with any output between them demotes the top row, shifts
  every row up one and appends a blank row. With nothing output between them, the undo hands the row
  back. On the primary screen with the cursor on row 0 it rotates rows through history. The scenario
  tests sample this law composed with the audit specs below.

Cross-crate: JSON escape and percent encoding live in aterm-codec. Block parsing (OSC 133/633/7),
selection, grid cells and resize live in aterm-core. Specs that cite those functions need gap 12, or need
to be re-homed in that crate's `SPECS.md`.

## 1. Specs (TSV)

Columns: `id`, `kind`, `functions`, `statement`, `subsumes (primary tests; secondary in §2)`,
`difficulty (Rust features the proof must handle)`, `blocking_gaps (audit §7)`.

```tsv
id	kind	functions	statement	subsumes	difficulty	blocking_gaps
json-escape-wire	round-trip (home: aterm-codec)	wire::json_escape (= aterm_codec::json::escape), wire::json_escape_into (= aterm_codec::json::escape_into), wire::json_str_field, wire::json_ok	∀ buf:String, s:&str. (a) json_escape_into(buf,s) leaves buf' = buf ++ json_escape(s). (b) json_escape(s) contains no char c with TtyActive(c), and no '"' or '\\' except as part of an escape. Every such char is emitted as a two-character escape (\" \\ and the codec's C0-whitespace table: \n \r \t \b \f) or as \u followed by 4 lowercase hex digits. Every other char, non-ASCII included, is emitted verbatim. (c) JSON-decode("\"" ++ json_escape(s) ++ "\"") = s. (d) json_str_field(k,v) = "\"" ++ json_escape(k) ++ "\":\"" ++ json_escape(v) ++ "\"", and json_ok(x) = "OK 1\n" ++ x ++ "\n". Corollary: "{" ++ json_str_field(k,s) ++ "}" is a valid JSON object whose member k decodes to s.	wire::json_member_keeps_arbitrary_keys_and_values_in_one_member	MEDIUM: &str/String char iteration loop, push_str, format!, a char-class table, a cross-crate re-export. The decode half (c) needs a JSON-decoder model; cast-json-reader's round-trip clause is the same law from the reader side.	0,1,5,6,7,12
visible-text-collapse	contract	wire::{visible_char, VisibleTrim, push_visible, is_blank_row, trimmed_len, visible_row, visible_row_into}, Terminal::push_line_text	∀ out, s, trim. push_visible(out, s, trim) sets out' = out ++ T_trim(map visible_char s), where T_None(x) = x, T_Spaces(x) = x without its trailing U+0020 run, and T_Whitespace(x) = trim_end(x) (Unicode White_Space); the prefix out is never changed, so a trim never crosses into it. is_blank_row(r) ⇔ ∀ c ∈ r. visible_char(c) is White_Space ⇔ push_visible("", r, Whitespace) = "". trimmed_len(rows) = 1 + the largest index i with ¬is_blank_row(rows_i), or 0 if none; hence trimmed_len(rows) = trimmed_len([push_visible("", r, Whitespace) | r ∈ rows]). ∀ t, r, out. visible_row_into(t, r, out) sets out' = out ++ push_visible("", get_line_text(t, r, None) or "", Whitespace) when r < rows, and leaves out unchanged otherwise (Terminal::push_line_text appends exactly get_line_text(r, None) and answers r < rows); visible_row(t, r) = visible_row_into(t, r, "").	wire::push_visible_collapses_then_trims_each_rule, wire::trimmed_len_is_last_nonblank_plus_one, wire::visible_row_into_appends_the_rows_text_reads	MEDIUM: &mut String append with a tail-only truncate, a char map with a rare rebuild, three trim rules, an iterator fold; the engine row walk is aterm-core's	0,1,2,3,7,12
wait-args	contract	wire::wait_ms, selection::cmd_wait (argument arm)	∀ tok. wait_ms(tok) = tok.parse::<u64>().ok().map(|m| min(m, WAIT_MAX_MS)). It does no trimming: wait_ms(" 5") = None. ∀ H, s, rest, with r = rest.trim(): r = "" ⇒ timeout = WAIT_DEFAULT_MS. r ≠ "" ∧ wait_ms(r) = None ⇒ cmd_wait returns exactly "ERR usage: wait [<ms>]\n" before any host call. Otherwise timeout = wait_ms(r). So "2s", "1.5", "-1", "0x10", "5 6" and "timeout=5" are usage errors, and "99999999999" waits WAIT_MAX_MS.	conformance::a_wait_ms_that_is_not_a_number_is_the_usage_line	EASY: &str trim and parse::<u64>, Option::map, u64::min, an early return before generic trait calls	0,1,3,6
verbs-fail-closed-on-unserved-sid	contract	selection::{cmd_blocks, cmd_blocks_json, cmd_blocktext, cmd_wait, cmd_select, cmd_selection, cmd_copy}	∀ H, s with with_terminal(s,_) = None, with_terminal_mut(s,_) = None and with_terminal_as_it_stands(s,_) = None. cmd_blocks(H,s,r) = cmd_blocks_json(H,s,r) = NO_SESSION for every r. cmd_blocktext(H,s,r) = NO_SESSION when r.trim() parses as u64, and its USAGE otherwise. cmd_wait(H,s,r) = NO_SESSION for every valid tail (wait-args), and subscribe is never called. cmd_selection(H,s) = NO_SESSION. cmd_copy(H,s) = NO_SESSION if caps.clipboard, else "ERR unsupported\n". cmd_select(H,s,r) = NO_SESSION for every r that parses to a form, and such a call never calls request_redraw. No verb calls clipboard_set, and no verb answers from another sid's state. Usage and capability refusals are checked before the sid.	conformance::an_unresolved_sid_is_refused	MEDIUM: generic trait methods with impl FnOnce closures returning Option, let-else, String allocation	0,1,2,4,11
blocks-line-format	contract	selection::{cmd_blocks, last_n, state_word}, wire::pct_encode_into, SessionHost::native_cwd	∀ H, s served, rest, with S = last_n(B(Term(H,s)), rest): cmd_blocks(H,s,rest) = "OK " ++ |S| ++ "\n" ++ concat over b ∈ S of "block {b.id} {state_word(b.state)} exit={opt_-(b.exit_code)} prompt={b.prompt_start_row} cmd={opt_-(b.command_start_row)} out={opt_-(b.output_start_row)} end={opt_-(b.end_row)} cwd=" ++ pct(H.native_cwd(b.working_directory or "")) ++ " cmdline=" ++ pct(b.commandline or "") ++ "\n". Since pct output has no space or newline: block_count(reply) = Some(|S|), the reply has exactly |S| body lines, and each starts "block " and carries exit= prompt= cmd= out= end= cwd= cmdline= in that order, which is Pred_check_blocks. The reply is a pure function of B and native_cwd, so a host that renames paths changes only the cwd token.	conformance::the_block_checks_hold_on_a_populated_session, conformance::a_block_cwd_is_printed_through_the_hosts_native_cwd	MEDIUM: Vec collect of an iterator, a slice range, a for loop with write! into a String, Option<&str>/Cow, a generic trait default method, the cross-crate pct encoder	0,1,2,3,5,7,11,12
blocks-json-format	contract	selection::{cmd_blocks_json, last_n, state_word}, wire::{json_ok, json_str_field}	∀ H, s served, rest, with S = last_n(B(Term), rest): cmd_blocks_json = json_ok("{\"blocks\":[" ++ join(",", map item S) ++ "]}"). item(b) = {"id":id, json_str_field(state, state_word), "exit":opt_null(exit), "prompt":prompt_start_row, "cmd":opt_null(command_start_row), "cmdcol":opt_null(command_start_col), "out":opt_null(output_start_row), "end":opt_null(end_row), json_str_field(cwd, native_cwd(wd or "")), json_str_field(cmdline, cmdline or "")}, in exactly that key order. With json-escape-wire, the body has no raw newline, so it is one line, it is valid JSON, and blocks[i].cwd and .cmdline decode to exactly native_cwd(wd) and cmdline. This is Pred_check_blocks_json. "cmdcol" is null iff command_start_col is None; its value is aterm-core's OSC 133;B column fact (§5.10).	conformance::blocks_json_says_the_column_the_input_starts_at, conformance::blocks_json_escapes_what_a_terminal_acts_on_and_decodes_back	MEDIUM: format! with many args, Vec<String> join, closures, Option map_or_else; the column and state values come from aterm-core's OSC 133/633/7 handling	0,1,2,3,5,7,11,12
blocktext-format	contract	selection::{cmd_blocktext, blocktext, block_text, frame_block_text}, wire::{push_visible, trimmed_len}	∀ H, s, rest. If rest.trim().parse::<u64>() fails: cmd_blocktext(H,s,rest) = "ERR usage: blocktext <id>\n"; else cmd_blocktext(H,s,rest) = blocktext(H,s,id,false). ∀ H, s, id, trim. If not served: NO_SESSION. Else, if block_by_id(id) = None: "ERR no such block\n". Else, by block_output_text: Evicted ⇒ "ERR block output evicted from scrollback\n"; NotAvailable ⇒ "ERR block has no output yet\n"; Text(x) ⇒ "OK {m}" ++ o ++ (trim ? " trimmed={k-m}" : "") ++ "\n" ++ concat over the first m lines L of x.lines() of trim_end(map visible_char L) ++ "\n", where k = |x.lines()|, m = trim ? trimmed_len(x.lines()) : k, and o = " frame=open" iff Term.frame_unfinished_at(now) under the with_terminal hold that read x (after any wait the host made), else "". Only block_text (copying x or naming the refusal) and o's check run inside with_terminal; frame_block_text runs after it returns. Instance: no block has id u64::MAX on a fresh session, which with the usage arm is Pred_check_blocktext. An unknown id never gets an empty OK.	selection::blocktext_frames_each_row_and_trims_the_blank_tail, selection::blocktext_frames_after_the_terminal_lock; sampled by conformance::the_block_checks_hold_on_a_populated_session; o by ../aterm-gui/src/control_whole_frame_tests.rs::the_host_verbs_say_frame_open_on_a_frame_unfinished_at_their_bound	MEDIUM: str::lines iterator, push_visible (callee contract), an enum match returning Result inside a closure, a header written before the rows	0,1,2,3,4,7,11,12
wait-completion	contract	selection::{cmd_wait, scan_blocks}, SessionHost::subscribe, ChangeWait::wait	∀ H, s served, timeout T (wait-args). Every read of Term here is with_terminal_as_it_stands (no wait for a program's frame adds to T). Let (done, inflight) = scan_blocks(Term), where done = (id, exit) of the LAST Complete block in B order and inflight = ∃ Executing block. (1) ¬inflight ∧ done = Some((id,e)) ⇒ the reply is "OK complete {id} exit={opt_-(e)}\n", without calling subscribe. (2) Otherwise baseline = done.map(id). The subscription is registered before the first re-check. The reply is the first of these to hold at a re-check: newest_completed_block = Some(b) with baseline.is_none_or(b.id > baseline) ⇒ "OK complete {b.id} exit=…\n"; session_ended(s) ⇒ "ERR exited\n"; caller_gone() ⇒ "ERR hangup\n"; now ≥ entry + T ⇒ "OK timeout\n". Liveness: if a Complete block with id > baseline is committed and the host's ChangeWait wakes before the deadline, the reply names it or a newer one. Termination: given wait(d) returns within d + ε, the call returns by deadline + one slice. T = 0 with no blocks ⇒ "OK timeout\n". Requires (aterm-core): newest_completed_block = the last Complete in B.	conformance::wait_reports_a_completion_that_lands_while_parked	HARD: a loop over an Instant clock, Duration arithmetic, a Box<dyn ChangeWait> trait object, a cross-thread condvar wake (MemoryHost), closures, Option::is_none_or	0,1,2,4,7,8,11,12,13
select-grammar	contract	selection::{cmd_select, select_word, select_line}	∀ H, s served, rest, with r = rest.trim() and toks = r.split_whitespace(). r = "clear" ⇒ the selection is cleared, then one request_redraw(s), then "OK{o}\n". toks empty ⇒ USAGE. Head "word": the next two tokens parse as (i32,u16), else "ERR usage: select word <r> <c>\n"; then select_word. Head "line": an i32, else "ERR usage: select line <r>\n"; then select_line. Head "block": (i32,u16,i32,u16), else the block usage line; then a Block selection start(r1,c1,Left), update(r2,c2,Right), complete. Head "extend": (i32,u16), else the extend usage line; ¬has_selection ∨ is_empty ⇒ "ERR no selection\n" with no redraw; else extend_selection(r, c, Left iff (r,c) <lex (start.row, start.col), otherwise Right), then complete. Any other head is a range: fewer than 3 further tokens ⇒ USAGE; any parse of (i32,u16,i32,u16) fails ⇒ "ERR bad args\n"; else a Simple selection from the lexmin to the lexmax of (r1,c1),(r2,c2), sides Left/Right, completed. Every OK reply is "OK{o}\n", with o = " frame=open" iff Term.frame_unfinished_at(now) under the with_terminal_mut hold that made the change (after any wait the host made), else "", and is preceded by exactly one request_redraw(s); every ERR reply by none. Tokens past a form's arity are ignored (§5.8). With (K): after "0 0 0 4" the next "extend 0 5" is OK; after "clear" the next "extend …" is "ERR no selection\n".	conformance::select_reads_back_and_copies_live_content; o sampled by ../aterm-gui/src/control_whole_frame_tests.rs::the_host_verbs_say_frame_open_on_a_frame_unfinished_at_their_bound	MEDIUM: split_whitespace iterator, tuple-of-Option<Result> let-else patterns, str parse for i32/u16, lexicographic tuple compare, &mut Terminal through an FnOnce closure	0,1,2,3,4,11,12
selection-and-copy-render	contract	selection::{cmd_selection, cmd_copy}	∀ H, s served, with (x, tr) = Term.selection_to_string_bounded(), inc = " incomplete" if tr else "", and o = " frame=open" iff Term.frame_unfinished_at(now) under the same with_terminal hold (after any wait the host made), else "". cmd_selection = "OK {k}{inc}{o}\n" ++ concat over p ∈ t.split('\n') of (p ++ "\n") when x = Some(t), t ≠ "", with k = |t.split('\n')|; otherwise "OK 0{o}\n". cmd_copy: ¬caps.clipboard ⇒ "ERR unsupported\n", with no with_terminal call. Otherwise, for Some(t) with t ≠ "", clipboard_set(t) is called exactly once; true ⇒ "OK {t.len()}{inc}{o}\n", false ⇒ "ERR pbcopy failed\n". For None or "" the reply is "OK 0{o}\n" and clipboard_set is never called. Neither verb mutates the selection or calls request_redraw.	conformance::a_host_without_a_clipboard_refuses_copy; o sampled by ../aterm-gui/src/control_whole_frame_tests.rs::the_host_verbs_say_frame_open_on_a_frame_unfinished_at_their_bound	MEDIUM: str::split collected into Vec<&str>, String building, byte len, generic host calls with a side effect	0,1,2,4,7,11
select-line-logical	contract	selection::select_line, Terminal::{logical_line_span, selection_to_string}	∀ t, row, with (f,l) = LL(row). select_line(t,row) returns (f,l) and leaves a completed Lines selection from (f, 0, Left) to (l, cols-1 saturating, Right). LL is constant over its own rows: ∀ r ∈ [f,l], LL(r) = (f,l). f = l iff row neither continues nor is continued by a soft wrap. Consequently (aterm-core copy law), Copy(t) = concat over r ∈ f..=l of get_line_text(r) with no newline inserted at soft wraps, and nothing from the rows before f or after l (a hard newline bounds it).	selection::a_line_click_selects_the_whole_soft_wrapped_logical_line, selection::a_line_click_on_the_continuation_row_selects_the_same_logical_line, selection::a_hard_newline_still_bounds_a_line_click, selection::unwrapped_lines_and_words_are_unchanged	MEDIUM: &mut Terminal, a u16 saturating_sub, a selection state machine in aterm-core, String concatenation over rows	0,1,2,7,12
select-word-contract	contract	selection::{select_word, word_span}	∀ t, row, col. select_word(t,row,col) returns (a,b) = word_span(t,row,col).unwrap_or(((row,col),(row,col))) and leaves a completed Semantic selection from (a, Left) to (b, Right), with a ≤lex b, so both cells are inclusive. word_span = None when cols = 0, or when SW finds no word at the clicked cell: whitespace, right of the line's text, or a non-word cell such as a lone regional-indicator half. In that case the selection is the clicked cell alone.	(none primary; sampled by selection::unwrapped_lines_and_words_are_unchanged and the gesture tests)	EASY given word-span-window-refines-line: Option unwrap_or on nested tuples, &mut Terminal, selection API calls	0,1,2,12
word-span-window-refines-line	refinement	selection::word_span, Terminal::line_range_cells, smart_word_in_cells	∀ t, row, col, with (f,l) = LL(row). (1) Whole(t,row,col) = None ⇒ word_span = None. (2) If Whole = Some(((r0,c0),(r1,c1))) and the doubling schedule back, fwd ∈ {2,4,8,…} with back + fwd < WORD_WINDOW_MAX_ROWS reaches a window [max(row-back, f), min(row+fwd, l)] that strictly contains rows r0..r1, or whose edge is the logical line's own edge, then word_span(t,row,col) = Whole(t,row,col). (3) Otherwise the result is the match in the last window tried: a prefix or suffix of the word, inside [f,l] and containing (row,col). (4) The loop runs at most about log2(64) iterations and never panics: all arithmetic is checked or saturating. Lemma needed (aterm-core): SW is local. A match whose byte span does not touch a window edge, or touches only an edge that is the line's own edge, is the same in the window and in the whole line.	selection::a_word_wider_than_the_window_is_still_selected_whole, selection::widening_stops_at_the_end_of_the_logical_line, selection::a_word_click_selects_a_word_straddling_the_wrap_whole	HARD: a loop with a break value, saturating i32/usize arithmetic, try_from, checked_mul/add with ?, regex-based smart rules behind a OnceLock static, cross-crate cell vectors	0,1,3,5,7,12
word-cells-are-drawn-cells	contract (home: aterm-core)	Terminal::line_range_cells, smart_word_in_cells, selection::{word_span, select_word}, Terminal::selection_to_string	∀ terminal states t, over live and history rows and any policy history (ambiguous_width wide or narrow, including a live switch after the row was drawn), any grapheme layout (Indic conjuncts spanning several cells, ZWJ clusters folded into one cell, format joiners ZWNJ/ZWJ/MVS/Egyptian joiners inside a word, a VS16 base kept one cell wide in the last column, regional indicators drawn into separate cells), and ∀ row, col: line_range_cells maps every byte of its text to the grid cell that drew it, from the row's own stored cells and not a re-measurement of the text. Hence if word_span(t,row,col) = Some((a,b)): (i) a ≤lex (row,col) ≤lex b; (ii) the cells a..b in reading order across soft wraps are exactly the cells holding the matched word's graphemes; (iii) after select_word, Copy(t) = the word's text; (iv) constancy: for every cell c in [a,b] that holds part of the word, word_span(t,c) = Some((a,b)).	selection::a_word_click_after_an_indic_conjunct_selects_the_word, selection::a_word_click_after_a_sinhala_zwj_conjunct_selects_the_word, selection::a_word_click_on_a_word_a_joiner_spells_selects_it_whole, selection::a_double_click_measures_ambiguous_characters_as_the_grid_draws_them, selection::a_word_click_after_a_last_column_vs16_base_selects_the_word_drawn_there, selection::a_word_click_after_regional_indicators_drawn_apart_selects_the_word_drawn_there, selection::a_word_copied_from_history_after_a_live_ambiguous_switch_is_the_word_drawn_there	VERY HARD: Unicode grapheme segmentation and width tables, grid/ring storage, scrollback, regex rules. Mostly aterm-core code; this crate contributes only the cell(g) mapping.	0,1,2,5,7,12
paint-equals-copy	equivalence (home: aterm-core)	aterm_core::selection::TextSelection::contains, Terminal::{selection_to_string, row_continues_previous, get_line_text}	∀ t with a completed selection of any type that select_line or select_word produces: Paint(t) = Copy(t). For each row in order, take the text of [lo,hi], where lo and hi are the first and last columns with contains(row,c). Join those texts with a newline only before a row where ¬row_continues_previous(row). The result equals selection_to_string. So the highlight the user sees is byte-for-byte what is copied, soft-wrap joins included.	(none primary; sampled by 8 selection tests, §2)	aterm-core only (the selection predicate and the copy walk); a quantified loop over rows and columns	0,1,2,5,7,12
drift-args-parse	contract	cast_drift::DriftArgs::parse	∀ rest. DriftArgs::parse(rest) = Ok(a) iff every whitespace token has the form key=value with: key = max_runs and value a usize; or key = rows and value a usize; or key = seed and value ∈ {auto, alt, primary}. Then a = the left fold from DriftArgs::default() (max_runs 8, list_rows None, seed Auto, undo Keep), where the last occurrence of a key wins, max_runs := min(v, MAX_RUNS), list_rows := Some(v), and undo is always Keep. Every other input is Err(USAGE), including a token without '=', an unknown key such as undo, frames or count, and a bad value such as rows=-1 or seed=main.	cast_drift::args_parse_the_grammar_and_refuse_the_rest	EASY-MEDIUM: a split_whitespace loop, split_once, usize parse, a str match, ? on Result<_, &'static str>, a struct update	0,1,3,6,7
cast-json-reader	refinement	asciicast::parse_json, asciicast::JsonReader::{value, string, unicode_escape, hex4, word, ws, eat}	parse_json is total: for every &str it returns, with no panic and no unbounded recursion. ∀ text, parse_json(text) = Ok(v) iff text = ws value ws, where value follows RFC 8259 except for two leniencies: a number is the maximal run of [-+.eE0-9] starting with '-' or a digit that f64-parses; and true/false share Json::Bool. Nesting depth must be ≤ MAX_JSON_DEPTH; deeper input is Err. String decoding maps \" \\ \/ \b \f \n \r \t to their chars and \uXXXX to the scalar. A high surrogate \uD8xx–\uDBxx immediately followed by a low \uDCxx–\uDFxx joins to 0x10000 + ((hi-0xD800)<<10) + (lo-0xDC00). A lone surrogate, high or low, decodes to U+FFFD, and the escape after an unpaired high is re-read on its own. Other bytes are copied verbatim, and the result is valid UTF-8. An unterminated string or escape, a bad escape letter and a truncated \u are Err. Round trip with json-escape-wire: ∀ s, parse_json("\"" ++ json_escape(s) ++ "\"") = Ok(Str(s)).	asciicast::parser_reads_events_with_every_escape	MEDIUM-HARD: recursive descent over &[u8] with a &mut self cursor (depth as the decreases measure), a recursive enum with Vec and String, while loops, slice get, from_utf8_lossy, char::from_u32, encode_utf8, f64 parse (no float model)	0,1,2,3,5,7
asciicast-structure	contract	asciicast::{parse, parse_header, parse_event}, asciicast::Json::{as_u64, as_f64, get}, cast_drift::fmt_t	∀ text, with lines numbered 1-based by str::lines and blank (trim-empty) lines skipped. No non-blank line ⇒ Err containing "no header". The first non-blank line n0 must be a JSON object whose "version" as_u64 = 2 and whose "width" and "height" are as_u64 integers. Then width/height = clamp(v, 1, 65535); evicted = aterm_truncated.evicted_events as u64, or 0; resize_undo = aterm_resize_undo as u64 mapped through (≠ 0), or None when the key is absent or not an integer; resize_laws = CastHeader::laws_named(aterm_resize_undo as u64): [Rewrap] for 3 and for a value no aterm wrote yet, [PushedRows, HandBack] for 2, [Undo] for 1, 0, and when the key is absent or not an integer; each replay folds under the first law and under the next only while the live screen is not reproduced; with no live screen it folds under every law named, and when their (verdict, culprits, drift) differ: verdict = Unknown, law_ambiguous = true, drift = [], cursor_other = None and every culprit = false, folds summed either way (analyze_inner, Terminal::set_resize_law); dropped = aterm_dropped.map(|d| CastDrops{bursts or 0, bytes or 0}), or, when that key is absent, Some(CastDrops{0, 0}) iff resize_undo is Some (a producer that writes the resize-law key counts drops and omits a count of none), else None; handoff = aterm_handoff.map(|h| HandoffHeader{from_build = h.from_build as u64, or None}). On duplicate keys the first wins. Otherwise the result is Err("line {n0}: …"), and a missing dimension names that key. Every later non-blank line n must be a JSON array of exactly 3 elements [t, kind, data], with t a finite number ≥ 0 and kind, data strings. "o" yields Output{t, data.into_bytes()}. "r" requires data = "<u16>x<u16>" and yields Resize{t, cols, rows}. Any other kind is skipped. Any failure yields Err("line {n}: …"). The events are the kept ones, in file order, and fmt_t(t) = format!("{t:.6}") reproduces a literal with ≤ 6 fraction digits padded to 6 places.	asciicast::the_resize_law_key_is_read_from_the_header, cast_drift::with_no_screen_two_laws_that_disagree_answer_unknown, asciicast::parser_reads_the_aterm_disclosures, asciicast::parser_names_the_bad_line	MEDIUM-HARD: iterator filter/enumerate over lines, slice patterns on Vec<Json>, closures, format!-built errors, u64 and f64 parse, a u16 clamp cast	0,1,3,5,7
cast-until	contract	asciicast::Asciicast::until	∀ cast, u:f64. until(cast,u).header = cast.header, and until(cast,u).events = [e ∈ cast.events | e.t() ≤ u], in the same relative order. So u below every event's t gives no events, and NaN keeps nothing.	(none primary; sampled by asciicast::parser_reads_the_aterm_disclosures)	EASY: Vec::retain with a closure, an f64 comparison, by-value self	0,1,7
drift-render	contract	cast_drift::DriftReport::{render, culprit_word}, cast_drift::fmt_t, cast_drift::Run::{len, net_zero}	∀ report R, ms, computed. render(R,ms,computed) = head ++ "\n" ++ body. body = one run line per R.candidates, in order; then one row line per entry of R.drift.take(R.list_rows or ∞), as "row {r} live={pct(live)} {K}={pct(other)}"; then one cursor line iff R.drift ≠ [] ∧ R.cursor_other = Some, as "cursor live={cursor_live or -} {K}={row},{col}". K = "replayed" iff R.fidelity = Some(false), otherwise "expected". head = "OK {n} verdict={word} fidelity={pass|fail|-} against={screen iff fidelity.is_some(), else replay} drift_rows={|drift|} runs={runs_total} candidates={eligible} capped={capped} culprit={culprit_word} anchor={fmt_t or -} seed={ambiguous if seed_ambiguous, else alt if seed_alt, else primary} seed_basis={word} geom={c}x{r} evicted={evicted} dropped={bursts or -} cut={word or -} folds={folds} fold_events={fold_events} ms={ms}", followed by " carried=0 from_build={b or -}" iff R.handoff = Some(HandoffHeader{from_build: b}), then " law=ambiguous" iff R.law_ambiguous, then " undo=off" iff R.undo_dropped, then " computed={x}" iff computed = Some(x). n = the number of body lines. drift_rows counts every drift row: rows= caps the listing, not the count. culprit_word = join(",", fmt_t(r.t) for each culprit r), or "-". A run line is "run t={fmt_t} n={len} gap_ms={:.3} geom={chain joined by >} net={zero iff first geom = last geom, else last geom} first_out_ms={:.3 or -} clear_ms={:.3 or -} culprit={0|1}".	cast_drift::offline_drift_sees_the_handoff; otherwise sampled by the 11 cast_drift scenario tests that read the wire form, §2	MEDIUM: String building with format! of f64 ({:.6}, {:.3}), which needs a float-formatting model; Vec iteration with take; Option combinators; the cross-crate pct encoder	0,1,2,5,7,12
drift-settle-cut	contract	cast_drift::DriftReport::settle_cut	∀ R, c. settle_cut sets R.cut := Some(c). R.verdict := Unknown iff c = Racy ∧ R.fidelity.is_some() ∧ R.verdict ∈ {Desync, Unfaithful}; otherwise the verdict is unchanged. Every other field is unchanged. So a racy cut never upgrades a verdict, never touches Clean or Empty, and a replay-against-replay report (fidelity None) keeps its verdict.	cast_drift::a_racy_cut_is_answered_by_the_replays	TRIVIAL: &mut self on a struct holding Vec fields, an enum matches! test. The body is scalar-only, so it is the easiest spec here once certified &mut views land.	0,2,3
drift-run-grouping	contract	cast_drift::{group_runs, follow_up}, cast_drift::Run::{len, net_zero}	∀ h, ev, with R = the subsequence of Resize events (index, t, geom), Outputs ignored. group_runs partitions R into maximal blocks of consecutive elements in which each element's t minus its predecessor's t is ≤ RUN_GAP_SECS. Each run has first and last = the indices of its first and last resize; t = the first resize's t; geoms = [the geometry in force before first, starting at (h.width, h.height)] ++ the (cols, rows) of each resize in the block; gap_ms = (t_last - t_first)·1000. first_out_ms = 1000·((t of the first Output at index > last) - t_last), or None. clear_ms is the same over the Outputs that hold a clear (Marks). runs_total = |runs|. Run::len = |geoms| - 1 and net_zero ⇔ geoms.first = geoms.last. Output between the halves never splits a run, and halves more than RUN_GAP_SECS apart are two runs.	cast_drift::a_draw_between_the_halves_does_not_hide_the_flap	MEDIUM: an enumerate loop with Option<Run> state, Vec push, f64 arithmetic and comparison, a backward pass building Vec<Option<f64>> over &mut [Run]	0,1,2,7
drift-anchor-and-seed	contract	cast_drift::{scan_marks, analyze_seeded}, cast_drift::audit (anchor and fidelity re-seed)	∀ h, ev, live, args. scan_marks records every ESC offset in each Output burst whose tail starts with a MARKS sequence. first_toggle_exit = whether the first toggle is an exit; last_toggle and last_clear = the greatest (event, offset) positions. Anchor = None if last_clear <lex last_toggle; otherwise the event of last_clear, and report.anchor = ev[Anchor].t. Seed: seed=alt|primary gives (that, Arg). Otherwise first_toggle_exit = Some(x) gives (x, Toggle); else live.alt = Some(a) gives (a, Live); else (false, Default). When the basis is Default, a live screen is given and Faithful(primary) ≠ live: if Faithful(alt) = live, the seed becomes (true, Fidelity) and Faithful is replaced; one fold is added either way. When there is no live screen, the basis is Default and runs_total > 0, the audit is repeated on ¬seed. If the two verdicts differ: verdict = Unknown, seed_ambiguous = true, drift = [], cursor_other = None and every culprit = false. Folds are summed either way.	cast_drift::an_alt_exit_after_the_last_clear_drops_the_anchor, cast_drift::drift_seeds_alt_for_an_adopted_cast, cast_drift::drift_is_clean_when_ed2_follows_the_flap	MEDIUM-HARD: nested loops over bytes and a const table of byte slices (starts_with), tuple ordering, an enum match table, generic D: AsRef<[u8]>, folds through Terminal	0,1,3,5,7,11,12
drift-candidates	contract	cast_drift::audit (candidate set), cast_drift::{return_chains, fold (lossy journal)}	∀ h, ev. Lossy, from fold's journal: an alt, non-reflowed resize at index i that took k = demoted + pushed rows pushes (i,k), and each later restored_top + restored_bottom repays the newest entries LIFO; lossy = the indices with a positive balance, deduplicated. owned = [r ∈ Runs | len(r) ≥ 2 ∧ (Anchor = None ∨ r.first > Anchor)]. returns = return_chains over Lossy'', ascending: a chain starts at i if i is not inside an owned run or an earlier chain, and runs to the first j > i whose resize restores before[i]; its geoms are before[i] followed by every resize in i..=j. If no such j exists there is no chain. Eligible = sort_by(first)((owned minus the runs nested inside some return) ∪ returns), and report.eligible = |Eligible|. A quiet flap (undo repaid) is still an owned candidate when len ≥ 2, but its rows never appear in Lossy. A shrink that never comes back is no candidate.	cast_drift::a_flap_time_split_is_a_return_and_is_named, cast_drift::a_return_the_app_repainted_through_is_clean_and_a_kept_size_is_no_candidate	MEDIUM-HARD: Vec filter/clone/retain/sort_by_key, range contains, iterator find over indices, a LIFO while loop on Vec<(usize,u32)>, the resize journal (cross-crate)	0,1,2,5,7,12
drift-verdict-and-attribution	contract	cast_drift::{audit, apply_mask, diff_rows, same_on, analyze, analyze_replay}	∀ h, ev, live, args, policy. (Empty) ev = [] ⇒ verdict Empty, folds 0, anchor None, candidates []. Otherwise apply_mask(cands) is false exactly on [r.first, r.last] for a net-zero r and on [r.first, r.last) for any other r. Let Scr = live.rows if a screen is given, else Faithful.rows. (Unfaithful) Given a screen with diff_rows(live.rows, Faithful.rows) = U ≠ []: verdict Unfaithful, fidelity Some(false), drift = U with (live, faithful) text, cursor_other = Faithful.cursor, no culprit, and no variant folded (the gate runs first). (Clean) Otherwise D = diff_rows(Scr, Stable.rows) = [] ⇒ Clean. (Desync) Otherwise verdict Desync with drift D as (Scr, Stable) text and cursor_other = Stable.cursor. Blame: |Eligible| = 1 ⇒ [true]. Otherwise J(r) = ∃ i ∈ Lossy'' ∩ [r.first, r.last]; any J true ⇒ blame = J. Otherwise, over the newest N = min(max_runs, |Eligible|) candidates, Only_r is folded: sole(r) = same_on(Only_r, Stable, D); if any sole holds, blame = sole; else blame(r) = ¬same_on(Only_r, Faithful, D). The remaining candidates are false. Listing: candidates = [Eligible[i] with its culprit flag | i ≥ |Eligible| - max_runs ∨ blame[i]], in order, and capped = |Eligible| - |candidates|; so a culprit is always listed. Folds = 1 + [fidelity re-seed tried] + [Eligible ≠ ∅ ∧ not Unfaithful] + [Only folds run]·N. FoldEvents = |ev| per faithful fold, plus per variant |ev| - (Anchor + 1) when it folded from the anchor fork (Anchor ≠ None and ReplayEngine::fork of the standing faithful fold after event Anchor was Some, for the stable fold and each Only fold alike), else |ev|; Undo::Match and the other-seed audit sum both, as for Folds. A variant from the fork equals the variant folded from the start (drift-replay-fidelity). fidelity = Some(true) on every non-Unfaithful report with a screen, and None without one. analyze_replay = analyze with no screen and alt hint = alt. With drift-replay-fidelity and the engine resize law this fixes every scenario's rows.	cast_drift::drift_finds_a_net_zero_flap_on_the_alt_screen, cast_drift::a_quiet_flap_is_undone_and_drift_is_clean, cast_drift::drift_is_clean_when_the_tail_is_blank, cast_drift::drift_reports_unfaithful_not_desync_when_a_burst_is_missing, cast_drift::drift_attributes_the_one_culprit_among_many_healed_flaps, cast_drift::drift_attributes_between_two_candidates, cast_drift::every_candidate_is_weighed_and_the_journal_names_the_culprit, cast_drift::a_primary_rotation_is_attributed_by_the_only_folds, cast_drift::drift_without_a_screen_compares_the_replays, cast_drift::drift_of_an_empty_recording_is_empty	HARD: a large fn with closures capturing locals, Vec<bool> masks, Vec<Replay> folds, iterator zip/enumerate/filter, struct update syntax, generic D; the concrete row sets also need the aterm-core resize law	0,1,2,3,5,7,11,12
drift-undo-law	contract	cast_drift::analyze_inner	∀ h, ev, live, args. Let A(u) = the seeded audit with keep_undo = u. Undo::Keep ⇒ A(true). Undo::Drop ⇒ A(false), with undo_dropped = true. Undo::Match ⇒ A(true) if live = None ∨ A(true).fidelity ≠ Some(false) ∨ h.resize_undo = Some(true). Otherwise, if A(false).fidelity = Some(true), the result is A(false) with folds += A(true).folds; else A(true) with folds += A(false).folds. So a recording whose header says its engine kept the undo is never replayed without it: an undo regression stays Unfaithful, with no culprit and no " undo=off". A keyless recording that only the undo-less replay reproduces is answered with " undo=off".	cast_drift::a_recording_from_before_the_undo_replays_without_it, cast_drift::a_keyed_recording_is_never_replayed_without_the_undo	EASY-MEDIUM: an enum match, a closure called twice, struct field mutation on owned reports	0,1,2,3,5
drift-replay-fidelity	refinement	cast_drift::{fold, fold_rest, step, Variants::fold}, replay::ReplayEngine::{new, from_checkpoint, configured, fork, process, resize, drop_resize_undo}, Terminal::{new, replay_from_checkpoint, replay_fork, process, resize_with_policy, set_console_host_posture, set_host_resyncs_alt_screen, set_alt_archive_enabled}, wire::visible_row	∀ h, ev, posture p (Posture: seam policy and host resync), seed buffer a, undo law u, resize law l, with L satisfying LiveRec(L,h,ev,p,a,u,l). fold(h, ev, a, None, Engine{p,u,l}, j, f).0.rows = [visible_row(L,r) | r < L.rows()], .cursor = L.cursor() and .geom = (L.cols(), L.rows()), for both values of j and every f. A variant may fold from a fork: for every mask m that keeps every resize at or before event k, if fold(h, ev, a, None, E, j, Some(k)).1 = Some(e) then fold_rest(e, ev, k+1, Some(m), E) = fold(h, ev, a, Some(m), E, false, None).0. This needs ReplayEngine::fork to continue as the engine it copies (aterm-core replay-fork-continues); in this crate's tests Variants::fold checks it on every forked fold. This needs Terminal to be deterministic, and needs the alt-screen archive (off in every ReplayEngine) not to affect visible rows or the cursor (aterm-core). Every ReplayEngine, new or from a checkpoint, has the archive off and the console-host posture p (seam policy and host resync), and resizes under p's policy. Hence analyze(h, ev, Some(screen(L)), args, p) has fidelity Some(true) whenever args selects seed a and undo law u and l is the law the replay chose. This includes ConPTY posture: ?9001h is honoured under ResizePolicy::ConPty and not under Native, and under Posture{host_resyncs_alt_screen: true} (OpenConsole) a prompt on the alternate screen leaves it only at the recorded, forwarded ?1049l, as the live engine does. Contrapositive: a recording missing a burst that changed a visible row reads Unfaithful.	cast_drift::an_audit_replays_the_console_host_posture_of_its_policy, cast_drift::an_audit_replays_a_host_resync_under_the_sessions_posture, replay::every_replay_sets_archive_posture_and_policy, cast_drift::forked_variants_equal_full_folds	VERY HARD: the whole aterm-core Terminal state machine (parser, grid, resize, alt screen, posture) as a determinism and refinement proof between two call sequences; a loop over events	0,1,2,7,10,12
replay-session-config	contract	replay::ReplayEngine::{with_config, fork, configured}, Terminal::apply_config_keeping_modes	with_config(c) rebases the engine on the session's config c keeping the modes its checkpoint restored (aterm-core config-apply-keeping-modes), drops any report the rebase queued, and records c; fork() of an engine with a recorded config applies it to the fork the same way, so a fork answers a colour query and a soft reset as its parent and the live engine do. with_resize_law and with_host_resyncs_alt_screen keep a recorded config. An engine never given a config replays on the defaults, as before.	replay::a_replay_with_the_sessions_config_answers_as_the_live_engine	EASY: one rebase and a copied Arc on fork	1,2,7
conf-check-predicates	equivalence	conformance::{check_blocks, check_blocks_json, check_blocktext, check_wait, check_sessions, check_resolve, check_unserved_sid, unserved_sid, unrostered_sid, check_select, check_selection, first_text_row, check_copy, check_screen_witness, check_declared_capabilities, run_all, run_all_declared, run_read_only, run_read_only_witnessed, run_selection_state, block_count}	∀ H, s. Each check_X(H,s).passed() ⇔ Pred_X(H,s), the predicate the body evaluates. check_sessions: the R0/R1 sessions clauses. check_resolve: ¬roster ⇒ resolve(Local s) = resolve(Id u) = None; otherwise every row round-trips both ways ∧ resolve(Local(unrostered_sid)) = None ∧ resolve(Id u) = None, where u = "s-conformance-absent" ++ "x"^k for the least k with u ∉ the roster ids, and unrostered_sid = the first of s+1, s+2, … (wrapping) that is not a roster sid. check_unserved_sid: unserved_sid = Some(f) ∧ with_terminal_mut(f, _) = None. unserved_sid = the first roster sid c ≠ s with ¬Served(c); else the first top+n (n ∈ 1..=64, top = max(roster sids ∪ {s}), wrapping) with ¬Served; else None. check_screen_witness: w.trim() ≠ "" ∧ Served ∧ ∃ row ∈ [0, rows) ∪ [-rows, -1] with get_line_text(row) ∋ w. check_declared_capabilities: ∀ cap, declared ⇒ advertised; roster declared ⇒ s ∈ sessions ∧ resolve(Local s) = Some(s). check_select: the seven replies equal clear→OK, ""→USAGE prefix, word→word usage, "a b c d"→bad args, extend→no selection, "0 0 0 4"→OK, extend→OK. check_selection, check_copy, check_blocks, check_blocks_json, check_blocktext and check_wait: as their bodies over the verb replies. Every failure's detail carries the arm-specific fragment: the capability name, "not ascending", "unaddressable id", "empty parent", "omits the session under test", "no roster kept", "does not resolve to itself/sid", "unknown @…", "was served", "serves every sid probed", or "{k} non-blank rows". The witness detail never contains row text. Composition: run_read_only = the 6 read-only checks in order; run_read_only_witnessed = [witness] ++ run_read_only; run_all = run_read_only ++ run_selection_state ++ [check_unserved_sid] (10 checks); run_all_declared = [declared] ++ run_all (11).	-	HARD: generic H: SessionHost (per instantiation today), Vec<SessionEntry> windows/iter, while loops with wrapping_add, a bounded probe range, format!-built details, &'static str check names; quantifies over all trait impls (a higher-order spec)	0,1,2,4,5,7,11
conf-sound	contract	conformance::{run_all, run_all_declared, run_read_only, run_read_only_witnessed}	∀ H, s with HostContract(H,s) ∧ Quiescent(H) ∧ ∃ c in unserved_sid's probe set with ¬Served(c): every outcome of run_all(H,s) passes. If also declared ≤ caps pointwise, every outcome of run_all_declared(H,s,declared) passes. If also some row of Term in [-rows, rows) contains w, with w.trim() ≠ "", run_read_only_witnessed(H,s,w) passes. The read-only predicates need only R0/R1 and a coherent Terminal per accessor, not (K). So every host that serves a fresh engine per call with caps = default passes run_read_only: the documented limit, not a defect. Proof: compose conf-check-predicates with the verb specs (blocks-line-format, blocks-json-format, blocktext-format, wait-completion with T = 0, select-grammar, selection-and-copy-render, select-line-logical).	-	HARD: a higher-order statement over every SessionHost impl, composition of many callee contracts, trait generics, the Terminal model	0,1,2,4,5,7,11,12
conf-retention-complete	contract	conformance::{check_select, check_selection}	∀ H, s served. check_select(H,s).passed() ⇒ the selection that cmd_select(…, "0 0 0 4") established is non-empty in the engine that the next with_terminal_mut(s,_) sees, which is (K) at that point. Contrapositive: a host that hands each accessor a fresh Terminal fails check_select, blank screen or not. If, in addition, first_text_row(H,s) = Some(_), such a host fails check_selection: the read-back is "OK 0". A witness-replaying stateless host can therefore pass check_screen_witness and still fail check_select.	-	HARD: reasoning about a sequence of trait calls on an abstract host state (a two-state or relational spec), the selection model	0,1,2,4,5,11,12
conf-nonintrusive	invariant (frame)	conformance::SelectionRestore::{new, drop}, conformance::{check_select, check_selection, check_copy, run_read_only, run_read_only_witnessed, run_all}	∀ H, s satisfying (K). For check_select, check_selection and check_copy, on every exit path (each early return, a passing return, and unwinding from a panic inside the check), the TextSelection of Term(H,s) afterwards equals the one SelectionRestore::new captured. When s is unserved, nothing is written back. run_read_only and run_read_only_witnessed never call with_terminal_mut, request_redraw or clipboard_set. run_all never calls clipboard_set, because check_copy clears the selection before calling copy, and check_unserved_sid hands with_terminal_mut only a closure that changes nothing. So after run_all the clipboard is unchanged, and cmd_selection answers as before.	-	HARD: a Drop impl on a generic guard struct, Option::take, whole-struct assignment through &mut, unwinding semantics (catch_unwind), an effect footprint (which trait methods are called) over a generic host	0,1,2,4,8,11
```

### Distribution by kind (31)

| kind | count | ids |
|---|---|---|
| contract | 24 | visible-text-collapse, wait-args, verbs-fail-closed-on-unserved-sid, blocks-line-format, blocks-json-format, blocktext-format, wait-completion, select-grammar, selection-and-copy-render, select-line-logical, select-word-contract, word-cells-are-drawn-cells, drift-args-parse, asciicast-structure, cast-until, drift-render, drift-settle-cut, drift-run-grouping, drift-anchor-and-seed, drift-candidates, drift-verdict-and-attribution, drift-undo-law, conf-sound, conf-retention-complete |
| refinement | 3 | word-span-window-refines-line, cast-json-reader, drift-replay-fidelity |
| equivalence | 2 | paint-equals-copy, conf-check-predicates |
| round-trip | 1 | json-escape-wire |
| invariant (frame) | 1 | conf-nonintrusive |

### Gap histogram (specs blocked by each gap)

| gap | specs | note |
|---|---|---|
| 0 | 31 | every row; closed locally by seal `150efe4a`, channel admission owner-gated |
| 1 | 30 | non-scalar carriers: every verb takes `&str` and returns `String` (only `drift-settle-cut` escapes it) |
| 2 | 24 | `&self`/`&mut self` views: the host trait, `&mut Terminal`, `DriftReport` |
| 3 | 14 | E6 body widening: parsing, `min`, casts, enum matches |
| 4 | 9 | callee-ensures composition: verbs over the host, the suite over the verbs |
| 5 | 16 | quantified clauses |
| 6 | 3 | plain-Rust clause publication: the SMT-shaped parsers (`wait-args`, `drift-args-parse`, `json-escape-wire`) |
| 7 | 23 | loops |
| 8 | 2 | effects: `wait-completion` (clock, condvar), `conf-nonintrusive` (unwinding) |
| 10 | 1 | model→code refinement: `drift-replay-fidelity` |
| 11 | 13 | generics: `H: SessionHost`, `D: AsRef<[u8]>` |
| 12 | 19 | cross-crate citation: aterm-core engine, selection and grid facts; aterm-codec encoders |
| 13 | 1 | threads and closures: `wait-completion` |

Gaps 9 and 14-17 block no spec here directly (14 and 17 apply to whatever route each proof takes).

### Proof dependency order

The aterm-codec and aterm-core facts come first, from their own crates: `json-escape-wire`,
`word-cells-are-drawn-cells`, `paint-equals-copy`, the engine resize law, logical lines and copy. Then:

- the parsers: `wait-args`, `drift-args-parse`, `cast-json-reader` → `asciicast-structure`, `cast-until`;
- the row text: `visible-text-collapse` (over aterm-core's `push_line_text`), which `blocktext-format` and
  `drift-replay-fidelity` cite;
- the verbs: `verbs-fail-closed-on-unserved-sid`, then `blocks-line-format`, `blocks-json-format`,
  `blocktext-format`, `wait-completion`, `word-span-window-refines-line` → `select-word-contract`,
  `select-line-logical`, `select-grammar`, `selection-and-copy-render`;
- the checker: `conf-check-predicates`, then `conf-sound`, `conf-retention-complete` and `conf-nonintrusive`;
- the drift audit: `drift-replay-fidelity` and `drift-run-grouping`, then `drift-anchor-and-seed`,
  `drift-candidates`, `drift-verdict-and-attribution`, `drift-undo-law`, `drift-settle-cut` and `drift-render`.

## 2. Test → spec map (TSV)

`primary` is the spec that retires the test. `also` lists the other specs the same test samples.

```tsv
file	test	primary	also
src/wire.rs	push_visible_collapses_then_trims_each_rule	visible-text-collapse	
src/wire.rs	trimmed_len_is_last_nonblank_plus_one	visible-text-collapse	blocktext-format
src/wire.rs	visible_row_into_appends_the_rows_text_reads	visible-text-collapse	drift-replay-fidelity
src/selection.rs	blocktext_frames_each_row_and_trims_the_blank_tail	blocktext-format	visible-text-collapse
src/selection.rs	blocktext_frames_after_the_terminal_lock	blocktext-format	
src/selection.rs	a_line_click_selects_the_whole_soft_wrapped_logical_line	select-line-logical	paint-equals-copy
src/selection.rs	a_line_click_on_the_continuation_row_selects_the_same_logical_line	select-line-logical	paint-equals-copy
src/selection.rs	a_hard_newline_still_bounds_a_line_click	select-line-logical	
src/selection.rs	unwrapped_lines_and_words_are_unchanged	select-line-logical	select-word-contract, word-cells-are-drawn-cells
src/selection.rs	a_word_click_selects_a_word_straddling_the_wrap_whole	word-span-window-refines-line	word-cells-are-drawn-cells, paint-equals-copy
src/selection.rs	a_word_wider_than_the_window_is_still_selected_whole	word-span-window-refines-line	
src/selection.rs	widening_stops_at_the_end_of_the_logical_line	word-span-window-refines-line	
src/selection.rs	a_word_click_after_an_indic_conjunct_selects_the_word	word-cells-are-drawn-cells	paint-equals-copy
src/selection.rs	a_word_click_after_a_sinhala_zwj_conjunct_selects_the_word	word-cells-are-drawn-cells	paint-equals-copy
src/selection.rs	a_word_click_on_a_word_a_joiner_spells_selects_it_whole	word-cells-are-drawn-cells	
src/selection.rs	a_double_click_measures_ambiguous_characters_as_the_grid_draws_them	word-cells-are-drawn-cells	
src/selection.rs	a_word_click_after_a_last_column_vs16_base_selects_the_word_drawn_there	word-cells-are-drawn-cells	paint-equals-copy
src/selection.rs	a_word_click_after_regional_indicators_drawn_apart_selects_the_word_drawn_there	word-cells-are-drawn-cells	select-word-contract, paint-equals-copy
src/selection.rs	a_word_copied_from_history_after_a_live_ambiguous_switch_is_the_word_drawn_there	word-cells-are-drawn-cells	paint-equals-copy
src/conformance.rs	an_unresolved_sid_is_refused	verbs-fail-closed-on-unserved-sid	
src/conformance.rs	the_block_checks_hold_on_a_populated_session	blocks-line-format	blocks-json-format, blocktext-format, conf-sound
src/conformance.rs	a_block_cwd_is_printed_through_the_hosts_native_cwd	blocks-line-format	blocks-json-format
src/conformance.rs	blocks_json_says_the_column_the_input_starts_at	blocks-json-format	(cmdcol value: aterm-core, §5.10)
src/conformance.rs	blocks_json_escapes_what_a_terminal_acts_on_and_decodes_back	blocks-json-format	json-escape-wire
src/conformance.rs	a_wait_ms_that_is_not_a_number_is_the_usage_line	wait-args	wait-completion (mislabelled, §5.9)
src/conformance.rs	wait_reports_a_completion_that_lands_while_parked	wait-completion	
src/conformance.rs	select_reads_back_and_copies_live_content	select-grammar	selection-and-copy-render
src/conformance.rs	a_host_without_a_clipboard_refuses_copy	selection-and-copy-render	conf-sound
src/cast_drift.rs	args_parse_the_grammar_and_refuse_the_rest	drift-args-parse	
src/asciicast.rs	parser_reads_events_with_every_escape	cast-json-reader	asciicast-structure
src/asciicast.rs	parser_names_the_bad_line	asciicast-structure	cast-json-reader
src/asciicast.rs	parser_reads_the_aterm_disclosures	asciicast-structure	cast-until
src/asciicast.rs	the_resize_law_key_is_read_from_the_header	asciicast-structure	
src/asciicast.rs	write_then_parse_round_trips	asciicast-structure	json-escape-wire, cast-json-reader
src/cast_drift.rs	an_audit_replays_the_console_host_posture_of_its_policy	drift-replay-fidelity	
src/cast_drift.rs	an_audit_replays_a_host_resync_under_the_sessions_posture	drift-replay-fidelity	
src/replay.rs	every_replay_sets_archive_posture_and_policy	drift-replay-fidelity	
src/cast_drift.rs	forked_variants_equal_full_folds	drift-replay-fidelity	drift-verdict-and-attribution
src/cast_drift.rs	a_draw_between_the_halves_does_not_hide_the_flap	drift-run-grouping	drift-replay-fidelity, drift-verdict-and-attribution
src/cast_drift.rs	an_alt_exit_after_the_last_clear_drops_the_anchor	drift-anchor-and-seed	
src/cast_drift.rs	drift_seeds_alt_for_an_adopted_cast	drift-anchor-and-seed	drift-replay-fidelity, drift-render
src/cast_drift.rs	drift_is_clean_when_ed2_follows_the_flap	drift-anchor-and-seed	drift-candidates, drift-render
src/cast_drift.rs	a_flap_time_split_is_a_return_and_is_named	drift-candidates	drift-run-grouping, drift-verdict-and-attribution, drift-replay-fidelity, drift-render
src/cast_drift.rs	a_return_the_app_repainted_through_is_clean_and_a_kept_size_is_no_candidate	drift-candidates	drift-verdict-and-attribution
src/cast_drift.rs	drift_finds_a_net_zero_flap_on_the_alt_screen	drift-verdict-and-attribution	drift-run-grouping, drift-anchor-and-seed, drift-replay-fidelity, drift-render
src/cast_drift.rs	a_quiet_flap_is_undone_and_drift_is_clean	drift-verdict-and-attribution	drift-candidates, drift-replay-fidelity
src/cast_drift.rs	drift_is_clean_when_the_tail_is_blank	drift-verdict-and-attribution	
src/cast_drift.rs	drift_reports_unfaithful_not_desync_when_a_burst_is_missing	drift-verdict-and-attribution	drift-replay-fidelity, drift-settle-cut, drift-render
src/cast_drift.rs	drift_attributes_the_one_culprit_among_many_healed_flaps	drift-verdict-and-attribution	drift-run-grouping, drift-anchor-and-seed (loose folds bound, §5.6)
src/cast_drift.rs	drift_attributes_between_two_candidates	drift-verdict-and-attribution	drift-candidates
src/cast_drift.rs	every_candidate_is_weighed_and_the_journal_names_the_culprit	drift-verdict-and-attribution	drift-candidates, drift-render
src/cast_drift.rs	a_primary_rotation_is_attributed_by_the_only_folds	drift-verdict-and-attribution	
src/cast_drift.rs	drift_without_a_screen_compares_the_replays	drift-verdict-and-attribution	drift-settle-cut, drift-render
src/cast_drift.rs	drift_of_an_empty_recording_is_empty	drift-verdict-and-attribution	drift-render
src/cast_drift.rs	a_recording_from_before_the_undo_replays_without_it	drift-undo-law	drift-render
src/cast_drift.rs	a_keyed_recording_is_never_replayed_without_the_undo	drift-undo-law	drift-render
src/cast_drift.rs	a_racy_cut_is_answered_by_the_replays	drift-settle-cut	drift-anchor-and-seed, drift-verdict-and-attribution, drift-render
src/cast_drift.rs	offline_drift_sees_the_handoff	drift-render	asciicast-structure
```

## 3. Coverage

- 77 tests read. 73 map to one of the 31 specs. 4 are not spec samples (§4).
- The biggest collapses: `drift-verdict-and-attribution` (10 primary, plus 4 shared),
  `conf-check-predicates` (10 primary), `word-cells-are-drawn-cells` (7), `conf-sound` (6),
  `select-line-logical` and `conf-nonintrusive` (4 each).
- Four specs have no primary test: `select-word-contract`, `paint-equals-copy`, `cast-until` and
  `drift-render`. Each is sampled only as a side check of other tests; it states a
  contract the code keeps and those tests lean on. `drift-render` is the widest of them: 11 tests read
  the wire form it fixes.
- `json-escape-wire`, `word-cells-are-drawn-cells` and `paint-equals-copy` move to aterm-codec's and
  aterm-core's catalogues. Their 8 primary tests are deleted once those crates prove them, and this crate
  then cites the theorems (gap 12).

## 4. Tests that are not spec samples

> Status 2026-10-16: the 4 tests this section listed are deleted (77 → 73 tests in the crate), and their
> §2 rows are gone. One checked the field values `#[derive(Default)]` gives `SessionPlacement`, which the
> derive already guarantees; three asserted only `MemoryHost`'s own `#[cfg(test)]` methods, while the
> property they named is a clause of `HostContract` that each production host owes. Every test left
> samples a spec.

## 5. Implied specs that look wrong, or disagree with the code

1. **`Selector::parse` accepted a leading `+`.** Settled: the parse moved to aterm-types'
   `control_request`, which reads a local number from ASCII digits only (`@+12` is `Invalid`).
2. **`wait_ms` accepts `+5` for the same reason,** although it is documented as "a whole number of
   milliseconds". `cmd_blocktext` ids and `cmd_select` coordinates accept `+N` too, and select also
   accepts `-0`. None of this is tested. Recommendation for 1-2: require ASCII digits (one `bytes().all`
   guard each) and state the strict grammar; the specs get simpler too.
3. **`JsonReader` is more lenient than the tests imply.** `hex4` uses `u32::from_str_radix`, which accepts
   a sign, so `\u+041` decodes to `A`. The number lexer accepts literals JSON forbids, such as `01` and
   `1.`. The parser tests imply a strict RFC 8259 reader. `cast-json-reader` states the leniency
   explicitly.
4. **`parse_header` silently clamps** a width or height of 0 to 1 and values above 65535 to 65535. Its error
   text promises only "no integer", and no test samples a 0 or oversized dimension.
5. **`scan_marks` only sees mark sequences inside one output burst.** An `ESC[2J` or `?1049h` split across
   two `o` events is never seen, so the anchor and the seed can be wrong on real recordings. Every test
   feeds whole sequences.
6. **A bound weaker than the code.** `drift_attributes_the_one_culprit_among_many_healed_flaps` asserts
   `folds <= 3`, but the code is deterministic and gives exactly 2 by `drift-verdict-and-attribution`. The
   loose bound pins nothing exact.
7. **The fixture's resize is not the audit's resize.** The test fixture `Rec::resize` uses
   `Terminal::resize`, not `resize_with_policy(policy)`. The fidelity tests therefore assume
   `resize = resize_with_policy(Native)`, and ConPTY resize is never sampled:
   `an_audit_replays_the_console_host_posture_of_its_policy` has no resize events.
8. **`cmd_select` ignores tokens past a form's arity** (`select line 3 junk` and `select 0 0 0 4 9` answer
   OK), though the usage grammar lists exact forms. Untested either way.
9. **A mislabelled test.** `a_wait_ms_that_is_not_a_number_is_the_usage_line` also samples `cmd_wait`'s
   immediate-completion arm (`''`, `'0'`, `'99999999999'` → `OK complete`). That is the
   `wait-completion` spec, not the argument grammar its name claims.
10. **`cmdcol` is not this crate's fact.** `blocks_json_says_the_column_the_input_starts_at` asserts a value
    that comes from aterm-core's OSC 133;B column bookkeeping. This crate owns only the null-or-number
    rendering (`blocks-json-format`). The column needs its own aterm-core spec.

## 6. Proof order (easiest high-value first)

No spec here is blocked by gap 0 alone. The order below takes the narrowest rows first, because each forces
a small, reusable Trust widening that the larger specs then build on.

1. **`drift-settle-cut`** (gaps 0, 2, 3). A `&mut self` update of one enum field guarded by a `matches!`
   over two others: no loop, no string, no call. It is the smallest certified `&mut self` view in the
   crate and retires `a_racy_cut_is_answered_by_the_replays` (whose seed half also needs
   `drift-anchor-and-seed`).
2. **`wait-args`** (gaps 0, 1, 3, 6). One `str::parse::<u64>` plus a `min`, on a Clean model of
   `u64::from_str` over `&str`. Fixing §5.2 first (ASCII digits only) shrinks that model to a digit fold.
   It retires one test and is a premise of `wait-completion` and `conf-check-predicates`.
3. **`drift-args-parse`** (gaps 0, 1, 3, 6, 7). A token loop over the same parse model; it retires one test.
4. **`cast-until`** (gaps 0, 1, 7). A `Vec::retain` with an `f64` comparison: the first `Vec` content
   model in the crate, needed later by `asciicast-structure`.
5. **`drift-undo-law`** (gaps 0, 1, 2, 3, 5). A case split over two calls to the seeded audit, stated
   with the audit's result as an opaque record. It retires two tests.
6. **`select-word-contract`** and **`drift-run-grouping`** (gaps 0, 1, 2 plus 12 or 7). Small bodies; the
   first is the bridge every gesture test crosses, the second retires one test and is the base of the
   drift candidate specs.

The large collapses come after: `conf-check-predicates` (10 tests) once gaps 4, 5, 7 and 11 land, and
`drift-verdict-and-attribution` (10 tests) once `drift-replay-fidelity` can be cited.
