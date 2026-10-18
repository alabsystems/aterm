<!-- Copyright 2026 Andrew Yates -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# aterm-types: spec recovery from its 398 unit tests

> Status 2026-10-16, after the merge with the tests-and-gates audit: the rows naming 173 tests
> it deleted and 2 specs whose functions it deleted are gone. Counts elsewhere in this file were
> taken before.

> Status 2026-10-15: this is the crate's migration worklist. The 125 specs below are each to be
> proved through Trust/Clean about the compiled Rust function, and then the tests each one subsumes are
> deleted. The 18 tests in §4 sample no spec and can be deleted now. Nothing has been proved yet.

Source: `crates/aterm-types` (read only). Every `#[test]` fn was read, with the code it reaches:
src/app_inspection.rs 5, src/bitflags.rs 5, src/color_palette.rs 32, src/control_socket.rs 22, src/control_verbs.rs 15, src/dev_seam.rs 1, src/dirs.rs 10, src/env_sanitize.rs 8, src/fabric_grammar.rs 3, src/fs_restricted.rs 5, src/identity.rs 1, src/input.rs 11, src/keyboard/dom_map.rs 11, src/keyboard/encode_tests.rs 35, src/keyboard/kitty_progressive_tests.rs 9, src/keyboard/layout_text_tests.rs 7, src/keyboard/tests.rs 27, src/keyboard/tests_legacy.rs 25, src/lib.rs 5, src/mouse/encode.rs 19, src/mouse/types.rs 6, src/mutex_ext.rs 2, src/paragraph_direction.rs 1, src/rfc3339.rs 4, src/rust_lane_tests.rs 17, src/scheme.rs 12, src/selection.rs 12, src/shell_blocks_tests.rs 5, src/shell_types_tests.rs 3, src/terminal_core.rs 2, src/text_shaping.rs 9, src/time.rs 1, src/trust_fmt.rs 2, src/version.rs 1, src/vt_level.rs 1, src/x11_colors.rs 6, src/xterm_keyboard.rs 7, tests/color_space.rs 14, tests/input_key_bridge_tests.rs 3, tests/kitty_keyboard_tests.rs 31, tests/xterm_keyboard_tests.rs 3 = 398.
`tests/types_suite.rs` only holds `mod` lines and has no tests (a text grep counts one extra
because a doc comment there says "#[test]s"). Nothing was compiled or run; the claims come from
reading the code. The recovery ran in two chunks (`src/` and `tests/`); this file merges them,
folding the specs that were the same property seen from two files (the `KeyCode`↔`Key` bridge, the
xterm keyboard state machine and its query responses).

Result: **125 specs**, covering **380 tests**. **18 tests are not spec samples** (§4).

## 0. Notation used in the statements

Paths are relative to `crates/aterm-types`. `file::test` names a `#[test]` fn. `++` is concatenation, `x'` the
post-state, `sat(e)` saturating arithmetic in the operand type.

Text and numbers
- `Tok(s)`: the maximal runs of non-Unicode-whitespace chars in s, in order (what `app_inspection`'s
  `take_token` loop produces). `Rem(s)`: s trimmed of Unicode whitespace.
- `ValidTok(t,n)`: 1 ≤ |t| ≤ n bytes and no char of t is whitespace or control. `ValidPayload(t,n)`:
  1 ≤ |t| ≤ n and t has no CR, LF or NUL.
- `Dec(n)`: the shortest ASCII decimal spelling of an unsigned n (no sign, no leading zero, "0" for 0).
  `PadN(v)`: Dec(v) left-padded with '0' to at least N digits. `Hex4(v)`: 4 lowercase hex digits of a
  u16. `Digits(s)`: s nonempty and all ASCII digits.
- `Lex`: lexicographic order on (row, col).

Colour
- `PReach`: the `ColorPalette` values reachable from `ColorPalette::new()` by any sequence of `set`,
  `reset_color` and `reset`. `PModel(p)(i)` = c if (i,c) ∈ p.overrides, else `default_color(i)`.
- `lin(c)` = clamp01(c' ≤ 0.04045 ? c'/12.92 : ((c'+0.055)/1.055)^2.4), c' = c/255 (IEC 61966-2-1).
  `L(rgb)` = 0.2126·lin(r) + 0.7152·lin(g) + 0.0722·lin(b) (Rec.709). `L_R`/`lin_R` are these over the
  reals; `L_F`/`lin_F` are the compiled f64 functions (`Rgb::luminance` and its nested `linearize`).
  `fl(e)` is IEEE-754 binary64 evaluation. `powf` is not correctly rounded, so a statement about
  `L_F`'s value is a real-number statement with an error bound `eps` (1e-12 covers every test
  tolerance). Trust refuses `powf` by design (src comment), so a luminance-value spec needs a
  correctly-rounded pow model, or an interval or table route.

Keyboard encoder
- `Enc(k,m,mode,ev,base,text)` = `keyboard::encode_key_with_text`. `encode_key_with_layout` =
  Enc(..,base,None), `encode_key_with_event` = Enc(..,None,None) and `encode_key` = Enc(..,Press,None,None)
  are definitional delegations; `Enc(k,m,mode,ev)` abbreviates the `encode_key_with_event` form.
- `KITTY` = `KeyboardMode::KITTY_PROTOCOL_FLAGS`. `Modes(16)`: every u16 bit pattern (KeyboardMode
  defines 16 bits, 0..=15; `WIN32_INPUT` is bit 15). `Legacy(mode)` = mode minus KITTY, the XTERM_* bits
  and WIN32_INPUT.
- `TextKey(k,m)`: (k = Character(c) or Named(Space)) and m ⊆ {SHIFT, CAPS_LOCK, NUM_LOCK}.
  `RecoveryKey` = Enter, Tab, Backspace. `ModKey` = Shift/Control/Alt/Super/Meta/Hyper Left/Right and
  the lock keys.
- `Win32Rec(vk,sc,uc,kd,cs)` = `CSI vk;sc;uc;kd;cs;1 _`, the microsoft/terminal #4999 record. A record pair
  is the kd=1 record followed by the kd=0 record.
- Key bridge (`src/input.rs`): `fwd` = `<Key as TryFrom<KeyCode>>::try_from`, `bwd` = the reverse.
  `NAMED` = the named-variant table {Backspace, Enter, Tab, Escape, Up→ArrowUp, Down→ArrowDown,
  Left→ArrowLeft, Right→ArrowRight, Home, End, PageUp, PageDown, Insert, Delete}. `FK(n)` = NamedKey::F<n>,
  n ∈ 1..=24.
- "every bitflags! instance": each production type the macro generates (Modifiers, KeyboardMode,
  KeyModifiers, TermMode, …). A = `Self::__all_bits()`.

Kitty keyboard state (`src/kitty_keyboard.rs`)
- `KMASK` = 0x1F. Bit i (0..4) is DISAMBIGUATE, REPORT_EVENTS, REPORT_ALTERNATES, REPORT_ALL_KEYS,
  REPORT_TEXT; `kacc_i` is the matching accessor.
- `KAbs(s)` = (s.flags, s.main_saved_flags, s.alt_saved_flags, MS, AS), MS = [s.main_stack[i].flags() |
  i < s.main_sp], AS likewise, oldest first. `KStk(s,Main)` = MS, `KStk(s,Alternate)` = AS.
- `KInv(s)` = s.main_sp ≤ 7 ∧ s.alt_sp ≤ 7 ∧ s.flags.bits() ≤ KMASK. `KReach`: the states reachable from
  `new()` by any sequence of `set_flags`, `push_flags_for_buffer`, `pop_flags_for_buffer`, `switch_screen`,
  `reset`, `reset_current_screen` and `restore_snapshot`. `takeLast(k,xs)`: the last k elements.

Xterm keyboard state (`src/xterm_keyboard.rs`)
- `mok` = modify_other_keys, `fok` = format_other_keys. `XInv(s)` = mok ∈ {None, Some(0..=2)} ∧ fok ∈ {0,1}.

Shell model (`src/rust_lane.rs`)
- `RunsAt(cmd)`: the set of (word sequence, depth) simple commands the POSIX shell would execute for cmd:
  split on `;` `&` `&&` `||` `|` and newlines; descend into `$( )`, backticks, `( )`, `{ }`,
  if/while/until/for/case arms, eval, `sh/bash/zsh -c` strings, `find -exec` and xargs; strip leading
  assignments, redirections and wrapper words (env, nice, timeout, sudo, command, exec, nohup, time,
  caffeinate, stdbuf, ionice, with their option arities); drop heredoc bodies except substitutions in
  unquoted ones; never read comments, quoted data or case patterns. Bounded by MAX_DEPTH = 8.

## 1. Specs (TSV)

Columns: `id`, `kind`, `functions`, `statement`, `subsumes` (count; the tests are in §2),
`rust_features` (what the proof must handle), `blocking_gaps` (audit §7 numbers, below).

Gap 0 (the installed seal publishes no Clean-discharged row) blocks every spec here and is left out of
the column; a spec whose column would list only 0 would be provable as soon as the seal lands. No spec
in this crate is in that position: the scalar-only ones still need gap 3 (bitwise ops, enum matches,
if/else chains, min/max). Two merge corrections to the chunk results: `fs-dir-safe-predicate` was listed
as gap 0 only, but `mode & 0o022` is a bitwise op (gap 3); `sel-linear` was listed as gap 5, but its
`ensures` needs no quantifier, while its if/else-if chain with an early return is gap 3. `color-spec-rgbi`
and `scheme-builtin-legibility` also get gap 6 (float clauses), as the `tests/` chunk used it for floats.

Audit §7 gaps: 1 non-scalar carriers (`&str`/`String`/`Vec`/`Option`/enums/structs); 2 certified
`&self`/`&mut self` views and two-state `x'`; 3 E6 body widening (calls, casts, shifts, bitwise ops,
loops, min/max, enum matches); 4 callee-ensures composition; 5 quantified clauses; 6 plain-Rust clause
publication (SMT/float); 7 loops; 8 effects; 10 model→code refinement; 11 generics; 12 cross-crate
citation; 13 threads, closures, higher-order; 14 Clean elaborator coverage.

```tsv
id	kind	functions	statement	subsumes	rust_features	blocking_gaps
app-inspect-grammar	contract	app_inspection::parse_inspect, WireViewId::new	∀ rest: &str. CR, LF or NUL in rest ⇒ Err(InvalidPayload). Else with T = Tok(rest): T[0] absent ⇒ Err(Usage); T[0] ≠ app/v1 ⇒ Err(UnsupportedVersion). T = [app/v1, tabs] ⇒ Ok(Tabs). T = [app/v1, view, v, kw] with kw ∈ {text, controls, tree, audit} ⇒ Ok(View{v, proj(kw)}) iff ValidTok(v,64); a v failing ValidTok gives Err(InvalidToken("view id")), checked before kw. Unknown kw ⇒ Err(UnknownProjection). Every other shape, extra tokens included ⇒ Err(Usage). The returned view is a subslice of rest equal to v.	2	&str slicing, char-predicate find, trim_start_matches, &mut &str cursor, Result over a lifetime-carrying enum	1,2,3,5,7
app-envelope-header	contract	InspectionEnvelope::header_line, InspectionProjection::keyword	∀ revision: u64, subject: header_line = "app-inspection version=app/v1 revision="++Dec(revision)++" subject=tabs" for Tabs, and …" subject=view view="++v.as_str()++" projection="++keyword(p) for View{v,p}. keyword is the bijection Text→text, Controls→controls, Tree→tree, Audit→audit.	1	format! with runtime args (must become concatenation for Trust), String	1,3
app-act-grammar	contract	app_inspection::parse_act	∀ rest without CR/LF/NUL: Ok(ActRequest{view,ui_key,action,value}) iff Tok(rest) begins [app/v1, view, v, k, a] with ValidTok(v,64), ValidTok(k,256), ValidTok(a,64). value = None if Rem(remainder after a) is empty, else Some(Rem(tail)) with ValidPayload(Rem(tail),16384) required (inner spaces kept verbatim). Errors otherwise, version first. A line break anywhere ⇒ Err.	2	&str tokenizer, Option<&str>, Result	1,2,3,5,7
app-open-grammar	contract	app_inspection::parse_open_app	∀ rest without CR/LF/NUL whose first token is app: settings with R = Rem(rest after settings), r = (R empty ? "/" : R) ⇒ Ok(Settings{r}) iff |r| ≤ 256, r starts with '/' and ValidTok(r,256), else Err(InvalidRoute). markdown|editor with U = Rem(rest after the word) ⇒ Ok(Markdown|Editor{U}) iff ValidPayload(U,16384). Any other app word ⇒ Err(UnknownApp). A missing word or a first token other than app ⇒ Err(Usage). The URI is returned verbatim, never canonicalized.	2	&str tokenizer, chars().any, Result	1,3,5,7
bitflags-boolean-algebra	contract	bitflags! macro: empty, bits, from_bits, from_bits_truncate, is_empty, is_all, contains, intersects, union, intersection, difference, insert, remove, toggle, set, and the BitOr/BitAnd/BitXor/Not/Sub (+Assign) impls	For every bitflags! instance F over T with A = __all_bits(), ∀ x, o: F, b: T, v: bool: empty().bits = 0; from_bits_truncate(b).bits = b & A; from_bits(b) = Some(F{b}) iff b & !A = 0, else None; (!x).bits = !x.bits & A; |, union, |=, insert give x.bits | o.bits; &, intersection, &= give &; difference, -, -=, remove give x.bits & !o.bits; ^, ^=, toggle give xor; set(o,v) = v ? insert : remove; contains(o) iff x.bits & o.bits = o.bits; intersects iff ≠ 0; is_all iff x.bits & A = A; is_empty iff bits = 0. Invariant corollary: operands within A ⇒ every result within A.	5	macro_rules-generated impls per instance, &mut self, operator traits, bitwise ops on u8/u16	2,3,11
palette-default-table	table-property	ColorPalette::default_color, compute_default_color, DEFAULT_COLOR_TABLE	∀ i: u8, default_color(i) = compute_default_color(i) = ANSI_COLORS[i] for i < 8; BRIGHT_COLORS[i-8] for 8 ≤ i < 16; for 16 ≤ i ≤ 231 with j = i-16, Rgb(Lv(j/36), Lv((j%36)/6), Lv(j%6)) where Lv(0) = 0, Lv(v) = 55+40v; for i ≥ 232, gray(8+10(i-232)). Decidable by evaluating 256 cases.	4	const fn with a match on u8 ranges, a const while-loop building a static [Rgb;256], u8 div/mod	3,7
palette-model-invariant	invariant	ColorPalette::{new,set,reset_color,reset,get,overrides_count}	∀ p ∈ PReach: (a) the indices in p.overrides are pairwise distinct; (b) every (i,c) ∈ overrides has c ≠ default_color(i); (c) ∀ i: u8, p.get(i) = PModel(p)(i), so the dense cache agrees with the sparse record, also after a swap_remove of a middle entry; (d) overrides_count(p) = |{i : get(i) ≠ default_color(i)}|.	1	&mut self, SmallVec<(u8,Rgb),16> position/swap_remove/push/get_mut, [Rgb;256] cache, an invariant quantified over 256 indices	1,2,5,7
palette-mutators	contract	ColorPalette::{new,set,reset_color,reset,get,overrides_count}	new(): ∀ i, get(i) = default_color(i) and overrides_count = 0. set(i,c): get'(i) = c; get'(j) = get(j) for j ≠ i; overrides_count' = overrides_count + [c ≠ default(i) ∧ get(i) = default(i)] − [c = default(i) ∧ get(i) ≠ default(i)]. reset_color(i) = set(i, default_color(i)), a no-op when get(i) = default(i). reset() makes the palette equal to new().	8	&mut self, two-state (old/new) clauses, SmallVec, array store	1,2,5
color-spec-hex	contract	ColorPalette::parse_color_spec, parse_hex_component	∀ s. s = "rgb:"++p ⇒ Some(rgb) iff p splits on '/' into exactly three parts of 1..=4 hex digits h with value v; the component is v·17 for |h| = 1, v for 2, v>>4 for 3, v>>8 for 4. s = '#'++r (not rgbi:/rgb:) ⇒ None unless r is ASCII; |r| = 3 gives each digit ·17; 6 gives r[0..2], r[2..4], r[4..6]; 9 gives r[0..2], r[3..5], r[6..8]; 12 gives r[0..2], r[4..6], r[8..10], each pair base 16. Any other length or a non-hex digit ⇒ None. Never panics on any &str, multibyte input included.	8	&str strip_prefix/splitn/get, u8/u16::from_str_radix (absent std bodies); the trust::skip on parse_hex_component must be lifted	1,3,5,7
color-spec-rgbi	contract	ColorPalette::parse_color_spec, parse_float_component	s = "rgbi:"++p ⇒ Some(Rgb(f(a),f(b),f(c))) iff p splits on '/' into exactly three parts a, b, c, each accepted by <f64 as FromStr>, with f(x) = (clamp(parse(x),0,1)·255 + 0.5) as u8 (saturating float→int). Otherwise None.	6	f64 FromStr, clamp, float arithmetic and an as-u8 cast, splitn	1,3,5,6,7
color-spec-named-fallthrough	contract	ColorPalette::parse_color_spec, x11_colors::lookup	∀ s with none of the prefixes rgbi:, rgb:, #: parse_color_spec(s) = x11_colors::lookup(s) (see x11-lookup).	3	call composition through the callee's ensures	1,4
color-spec-format-roundtrip	round-trip	ColorPalette::format_color_spec, trust_fmt::push_hex4, ColorPalette::parse_color_spec	∀ c: Rgb, format_color_spec(c) = "rgb:"++Hex4(257·c.r)++"/"++Hex4(257·c.g)++"/"++Hex4(257·c.b), and parse_color_spec(format_color_spec(c)) = Some(c), since 4-digit scaling gives (257x)>>8 = x.	3	String push, u16 shifts, composition with push-hex4 and color-spec-hex, ∀ over 2^24 colours (symbolic)	1,3,4,5
cs-socket-key	contract	control_socket::socket_key	∀ path: s = path minus a leading \\?\ prefix; text = s if |s| ≥ 2, s[1] = ':' and s[0] ASCII alphabetic, else text = path (a verbatim UNC or Unix path keeps its spelling). Result = ascii_lowercase(text) on Windows, text elsewhere. Hence key(\\?\C:\x) = key(C:\x).	1	&str strip_prefix, byte indexing, cfg!(windows), String	1,3
cs-socket-directive	contract	control_socket::socket_directive, env_flag_engaged	env_flag_engaged(v) iff v = Some(x) with x ∉ {"", "0"}. socket_directive(cs, no) = Disabled if env_flag_engaged(no); else Disabled if cs = Some("0") or cs is ASCII-case-insensitively "off"; PerInstance if cs ∈ {None, Some("")}; Explicit(x) verbatim for every other Some(x), so a path ending in /off stays Explicit.	3	Option<&str>, eq_ignore_ascii_case, enum with a String payload	1,3
cs-instance-name-pid	round-trip	control_socket::{instance_sock_name,instance_token_name,instance_pid}	∀ p: u32, instance_sock_name(p) = "aterm-"++Dec(p)++".sock", instance_token_name(p) = "aterm-"++Dec(p)++".token", and instance_pid of each = Some(p). ∀ n: instance_pid(n) = Some(q) iff n = "aterm-"++d++(".sock"|".token") with Digits(d) and value(d) = q ≤ u32::MAX; None for aterm.sock, aterm.token, a sign, an empty d or a suffix after .sock.	2	u32::to_string and str::parse (absent std bodies; parse is trust::skip), String concatenation	1,3,5,7
cs-graph-entry-lines	contract	control_socket::{graph_entry_sock,graph_entry_pid}	∀ body: L = the first line of body.lines() starting with "sock "; graph_entry_sock(body) = Some(trim(L[5..])) if nonempty, else None. M = the first line starting with "pid "; graph_entry_pid(body) = Some(v) iff r = trim(M[4..]) has Digits(r) and value v fits u32, else None.	2	str::lines/find_map/trim iterators, str::parse	1,3,5,7
cs-token-name-injective	contract	control_socket::{token_name_for_sock,sock_name_pid}	∀ s: token_name_for_sock(s) = instance_token_name(p) if s ends .sock and instance_pid(s) = Some(p); else (s ends .sock ? "" : "aterm-sock-")++s++".token". (1) Injective up to the pid normal form: two non-instance names, or instance names with different pids, never share a token name; (2) the result is never "aterm.token"; (3) instance_pid(token_name_for_sock(s)) = None unless s is an instance .sock name.	4	String concatenation, ends_with, injectivity quantified over strings	1,3,5
cs-token-candidates	contract	control_socket::token_names_for_sock	∀ s: token_names_for_sock(s) = [token_name_for_sock(s)] if s is an instance .sock name with a u32 pid, else [token_name_for_sock(s), "aterm.token"].	1	Vec<String> construction	1,4
cs-stale-instance-files	contract	control_socket::stale_instance_files	∀ names, pid_alive: result = [n | n ∈ names in order, instance_pid(n) = Some(p) ∧ ¬pid_alive(p)] as owned Strings.	1	&dyn Fn callback (higher-order), iterator filter/map/collect, Vec<String>	1,7,13
cv-host-gone-roundtrip	round-trip	control_verbs::{host_gone_line,host_gone}	∀ pid: u32, sock ≠ "" not ending in CR/LF: host_gone(host_gone_line(pid,sock)) = Some((pid,sock)). ∀ l: host_gone(l) = Some((p,s)) iff l minus trailing CR/LF = "ERR gone pid="++d++" sock="++s split at the first " sock=", with Digits(d), value(d) = p ≤ u32::MAX and s ≠ "".	1	format! with runtime args, split_once, str::parse	1,3,5,7
cv-rows-fence-wire	round-trip	RowsFence::{parse,hash_rows}, <RowsFence as Display>::fmt	∀ f with f.first ≤ f.last: to_string(f) = Dec(first)++"-"++Dec(last)++":"++16 lowercase hex digits of hash, and parse(to_string(f)) = Some(f). parse(v) = Some(f) iff v = a++"-"++b++":"++h with Digits(a), Digits(b), values fitting usize, a ≤ b, |h| = 16 all hex; else None. ∀ rows r1..rn: hash_rows(r) = fnv1a_64(r1++"\n"++…++rn++"\n").	1	Display via Formatter::write_str, a [u8;16] digit loop, generic IntoIterator<Item=&str>, cross-crate aterm_hash	1,3,7,11,12
cv-screen-gen-wire	round-trip	ScreenGen::parse, <ScreenGen as Display>::fmt	∀ g: ScreenGen, to_string(g) = Dec(epoch)++"."++Dec(seq) and parse(to_string(g)) = Some(g). parse(v) = Some(ScreenGen{e,s}) iff v = a++"."++b split at the first '.', with Digits(a), Digits(b) and both values ≤ u64::MAX; else None.	1	split_once, closure, u64 parse, Display	1,3,5,13
cv-framing-of	contract	control_verbs::{framing_of,spec}	∀ verb, request: r = the text after the first space if request starts with '@', else request; sub = the 2nd whitespace token of r; F = some token of r ∈ {--json, json}; B = some token ∈ {--bytes, bytes}. framing_of, rules in this order: image with sub = read or B ⇒ Lines; cast with sub ∈ {frames, drift} ⇒ Lines; inbox get ⇒ Bytes; inbox seen, outbox sent, identities forget, topic add|drop ⇒ Status; video frames ⇒ Lines; temporal status ⇒ Status; verb ∈ JSON_CAPABLE_VERBS with F ⇒ Lines; otherwise spec(verb).framing, or Status for an unknown verb.	4	split_whitespace iterators, linear search over a 200+-row static table, &str matching (trust::skip today)	1,3,5,7
cv-verb-table-wellformed	table-property	control_verbs::VERBS, VerbSpec::help_line, catalog_lines, catalog_lines_full, JSON_CAPABLE_VERBS	Over the const table VERBS: names pairwise distinct; ∀ v ∈ JSON_CAPABLE_VERBS, spec(v) = Some(s) with s.op = Read; every summary nonempty, ≤ 96 chars; no summary or detail has a double space or leading/trailing whitespace; help_line(s) = summary if detail = "", else summary++" "++detail; |catalog_lines| = |catalog_lines_full| = |VERBS|; Σ(bytes of catalog_row(name,summary) + 1) ≤ SHORT_CATALOG_MAX_BYTES (12696), which the const assertion beside VERBS also checks at compile time. Decidable by evaluation.	3	a 200+-row static &[VerbSpec] of &'static str, char counting, format! padding	1,3,7
cv-access-classification	table-property	control_verbs::{VERBS,is_owner_only,is_any_scope_meta,is_bridge_only,spec}	Security policy over VERBS, in table order: OwnerOnly = [appnotice, notice, story, devtools, askpass, topic, rekey, hold, fabric, sessions, who, exits, identities, whoami, grant, revoke, connect, disconnect, flows, raise]; AnyScopeMeta = [version, update, help, verbs, privacy]; BridgeOnly = [deliver, link, hello, outbox, outbox sent, log-take, log-landed]. is_X(v) iff spec(v) = Some(s) with s.access = X, so the sets are pairwise disjoint and unknown verbs are in none. Designed (op, framing, target, access) rows: inbox, inbox get, inbox seen, post, topic, deliver, link, hello, hold, outbox, outbox sent, log-take = (Read, Bytes, Meta, BridgeOnly), log-landed = (Write, Status, Meta, BridgeOnly); copy = ClipboardWrite; settings = ConfigWrite; scroll/select = Read; identities target = Meta; privacy = (Read, Lines, Meta, AnyScopeMeta).	5	static table evaluation, enum equality, find over a slice	1,3,7
cv-artifact-ack	contract	control_verbs::{artifact_reply_requires_ack,valid_artifact_ack_nonce}	With r, sub, B as in cv-framing-of: artifact_reply_requires_ack(verb,request) = (verb = image ∧ sub ≠ read ∧ ¬B) ∨ verb = window ∨ (verb = video ∧ sub ∉ {status, stop}); false for every other verb. valid_artifact_ack_nonce(n) iff |n| = 32 and every byte is ASCII hex.	1	split_whitespace/nth/any, matches! on Option<&str>	1,3,5,7
cv-entry-lines-wrap	contract	VerbSpec::entry_lines, catalog_row	∀ VerbSpec s whose help_line h has no double space, W = 71 (100−29): entry_lines(s) = [catalog_row(name, c0)] ++ [29 spaces ++ ci | i ≥ 1], where c0..ck is the greedy word-wrap of h.split(' ') at width W chars (a chunk closes only when the next word would pass W). Hence join(ci, " ") = h; each ci nonempty and trimmed; |ci| ≤ W unless ci is a single word; deterministic.	1	loop with a String accumulator, mem::take, chars().count, Vec<String>, format! width padding (trust::skip today)	1,3,5,7
dev-seam-reads-env	contract	dev_seam! macro	In a build with cfg(debug_assertions) or the calling crate's dev-seams feature, dev_seam!(n) = std::env::var_os(n) ∀ n; in every other build it is None, whatever the environment holds.	1	cfg-dependent macro expansion, a process-environment read (platform effect)	8
dirs-home-from-env	contract	dirs::home_from_env	∀ v: Option<&OsStr>, home_from_env(v) = Some(PathBuf::from(x)) iff v = Some(x) ∧ PathBuf::from(x).is_absolute(); None otherwise, empty and relative values included.	1	OsStr/PathBuf, Path::is_absolute (needs a path model)	1,8
dirs-config-path	contract	dirs::{resolve_aterm_config_path,aterm_config_path}	∀ xdg, appdata, home: xdg = Some(x), x nonempty ⇒ x/aterm/aterm.toml; else on Windows appdata = Some(a), a nonempty ⇒ a/aterm/aterm.toml; else home.map(h ↦ h/.config/aterm/aterm.toml). aterm_config_path() = resolve_aterm_config_path(var_os XDG_CONFIG_HOME, var_os APPDATA, var_os HOME).	1	PathBuf::join, cfg!(windows), env reads in the live wrapper	1,8
dirs-state-dir	contract	dirs::resolve_state_dir	∀ override_root, platform: override_root = Some(r) ⇒ Some(r) iff r.is_absolute(), else None. Otherwise per target: macOS home/Library/Application Support/aterm; Windows absolute local_app_data/aterm; other Unix absolute xdg_state_home/aterm, else home/.local/state/aterm; other targets None. An all-None platform gives None.	1	PathBuf, cfg(target_os) arms, Option chaining	1,8
dirs-logs-dir	contract	dirs::resolve_logs_dir	∀ override_root, platform: override_root = Some(r) ⇒ r/logs when r is absolute, else None. Otherwise: macOS home/Library/Logs/aterm; Windows (absolute local_app_data, else home/AppData/Local)/aterm/logs; other Unix (absolute xdg_state_home, else home/.local/state)/aterm/logs; other targets None.	1	PathBuf, cfg arms	1,8
dirs-data-dir	contract	dirs::{resolve_data_dir,resolve_aterm_data_dir,data_dir}	resolve_aterm_data_dir(Some(r),_) = Some(r) iff r is absolute (= resolve_state_dir(Some(r),_)), else None, the empty path included. resolve_aterm_data_dir(None,p) = resolve_data_dir(p).map(d ↦ d/aterm). resolve_data_dir: macOS home/Library/Application Support; Linux absolute xdg_data_home, else home/.local/share; Windows local_app_data; other targets None. data_dir() = resolve_data_dir(data_platform()).	1	PathBuf, cfg arms, env reads in the live wrappers	1,4,8
dirs-home-from-passwd	contract	dirs::home_from_passwd	∀ contents: &[u8], uid: u32: split on '\n', strip one trailing '\r' per line, splitn(7) each line on ':'. Result = Some(PathBuf from bytes(f[5])) for the FIRST line with ≥ 6 fields, f[2] = Dec(uid) bytewise and f[5] nonempty; None if no line qualifies. Short or malformed lines are skipped. Never panics.	3	byte-slice split/splitn iterators, Vec<&[u8]> collect, OsStr::from_bytes (trust::skip today)	1,3,5,7
env-deny-classifier	contract	env_sanitize::is_ai_env_var	∀ key: is_ai_env_var(key) iff key ∈ ENV_DENY_VARS ∨ ∃ p ∈ ENV_DENY_PREFIXES, key.starts_with(p).	7	loops over static &[&str], &str equality and starts_with	1,5,7
env-deny-policy	table-property	ENV_DENY_VARS, ENV_DENY_PREFIXES	Over the const tables: every ENV_* provisioning const (SESSION_ID, LAUNCH_NONCE, PARENT_SESSION_ID, MUX_BASE, EDGE_TOKENS, EDGE_READ/WRITE/SIGNAL, OBSERVE_SESSION_ID, TAB_SHELL), every seam read through dev_seam!, ATERM_UPDATE_TOKEN, ATPKG_TOKEN and the ATERM_LINK_*/ATERM_FABRIC_COMMAND names are in ENV_DENY_VARS. No prefix is a prefix of "ATERM_" or "__ATERM_" (so ATERM_SHELL_*, ATERM_ORIGINAL_ZDOTDIR, ATERM_UNSET_ZDOTDIR, ATERM_REROUTE_DIR, __ATERM_REROUTE_PASSTHROUGH survive). No prefix matches PATH, HOME, USER, SHELL, TERM, LANG, EDITOR, SSH_AUTH_SOCK, HOMEBREW_PREFIX or XDG_*.	6	evaluation over const string tables	1,3,7
env-key-lossy	contract	env_sanitize::is_ai_env_key	∀ k: OsStr, is_ai_env_key(k) = is_ai_env_var(to_string_lossy(k)) off Windows, is_ai_env_var(ascii_upper(lossy(k))) on Windows. Lemma: lossy conversion keeps every maximal valid prefix byte for byte, so a deny prefix followed by invalid bytes is still denied; an exact deny name followed by U+FFFD is not that name.	1	OsStr::to_string_lossy (needs a Cow<str> and UTF-8 decoding model), cfg(windows)	1,4,8
fab-principal	contract	fabric_grammar::is_principal	∀ p: is_principal(p) iff p = c++n with c ∈ {s-, n-, h-, a-}, 1 ≤ |n| ≤ 32 and every byte of n ∈ [a-z0-9-].	1	find_map over a const array, strip_prefix, bytes().all	1,5,7
fab-topic	contract	fabric_grammar::is_topic	∀ t: is_topic(t) iff 1 ≤ |t| ≤ 32 bytes, t[0] ∈ [a-z0-9], and every later char ∈ [a-z0-9._-].	1	Chars iterator, all	1,5,7
fab-via	contract	fabric_grammar::via_ok	∀ v: via_ok(v) iff v ≠ "", |v| ≤ 16·35 = 560 bytes, v.split(',') has ≤ 16 elements, each satisfying is_principal. Corollary: the longest legal chain (16 hops of 34 bytes, 559 bytes) is accepted.	1	split iterator, count/all, composition with fab-principal	1,4,5,7
fs-dir-safe-predicate	contract	fs_restricted::dir_safe_for_private_write	∀ uid, owner, mode: u32, the result holds iff owner = uid ∧ mode & 0o022 = 0.	3	scalar u32 == and bitwise & (bitwise ops are gap 3; the chunk listed gap 0 only)	3
fs-ensure-private-dir	contract	fs_restricted::{ensure_private_dir,create_dir_restricted}	Unix, ∀ dir: ensure_private_dir(dir) = Ok ⇒ afterwards dir and every directory it created exist with mode & 0o7777 = 0o700, and stat(dir) is owned by getuid() and not group/other-writable. A dir already at 0o700 gets no chmod(2) (its ctime is unchanged). Err(PermissionDenied) iff the post-stat fails dir_safe_for_private_write. Idempotent.	2	filesystem syscalls (mkdir, chmod, stat), extern getuid, io::Error	8
identity-origin-line	table-property	identity::{ORIGIN_LINE,AUTHOR,COMPANY,SITE}	ORIGIN_LINE = "by "++AUTHOR++" \u{b7} "++COMPANY++" \u{b7} "++SITE, and SITE does not contain "://". Const facts; a const assertion suffices.	1	const &str equality (decidable at compile time)	1
input-modifier-bridge	round-trip	<keyboard::Modifiers as From<KeyModifiers>>::from, <KeyModifiers as From<keyboard::Modifiers>>::from	KeyModifiers::{SHIFT,ALT,CTRL,SUPER}.bits() = Modifiers::{SHIFT,ALT,CTRL,SUPER}.bits() = 1, 2, 4, 8. ∀ m: KeyModifiers, From(From(m)) = m and Modifiers::from(m).bits() = m.bits(). ∀ n: Modifiers, KeyModifiers::from(n).bits() = n.bits() & 0xF.	2	bitflags instances, From traits, u8 &	3,4
input-key-bridge	round-trip	<keyboard::Key as TryFrom<KeyCode>>::try_from (fwd), <KeyCode as TryFrom<keyboard::Key>>::try_from (bwd)	∀ kc: KeyCode. fwd(Char(c)) = Ok(Character(c)) ∀ char c (Char(' ') stays Character(' ')); fwd(k) = Ok(Named(NAMED(k))) for the named variants; fwd(F(n)) = Ok(Named(FK(n))) for n ∈ 1..=24; fwd(F(n)) = Err(FunctionKeyOutOfRange(n)) for n = 0 or n > 24. Round trip: fwd(kc) = Ok(k) ⇒ bwd(k) = Ok(kc). Partial converse: bwd(k) = Ok(kc) ⇒ fwd(kc) = Ok(k), or k = Named(Space) ∧ kc = Char(' '). bwd(Named(x)) = Err(ProtocolOnlyKey(x)) exactly for x outside NAMED, F1..F24 and Space (numpad, media, locks, modifiers, F25+).	12	#[non_exhaustive] enums with payloads (char, u8, NamedKey), ~100-arm matches with a range pattern and catch-all, a const [NamedKey;24] with get, Result	1,3,5
cursor-style-from-param	contract	CursorStyle::from_param	∀ p: u16, from_param(p) = Some(BlinkingBlock) for 0 and 1; SteadyBlock 2; BlinkingUnderline 3; SteadyUnderline 4; BlinkingBar 5; SteadyBar 6 (DECSCUSR); None for p ≥ 7.	2	match on u16 literals returning Option<enum>	1,3
mouse-x10	contract	mouse::encode_x10	∀ cb: u8, col, row: u16, encode_x10(cb,col,row) = [ESC, '[', 'M', sat_u8(cb+32), min(sat(col+1),223)+32, min(sat(row+1),223)+32]: always 6 bytes, no coordinate byte wraps.	3	saturating u16/u8 arithmetic, min, as-cast, vec! literal	1,3
mouse-utf8	contract	mouse::{encode_utf8,encode_utf8_coord}	∀ cb, col, row: encode_utf8 = [ESC, '[', 'M'] ++ U(cb) ++ U(sat(col+1)) ++ U(sat(row+1)), where U(x): c = sat(x+32); c < 128 ⇒ [c]; else with c' = min(c,2047), [0xC0|(c'>>6), 0x80|(c'&0x3F)], the 2-byte UTF-8 encoding of c'.	3	&mut Vec<u8> push, shifts/masks, saturating u16	1,2,3
mouse-decimal-forms	contract	mouse::{encode_sgr,encode_urxvt,push_dec_u16}	∀ cb, col, row, release: encode_sgr = "\x1b[<"++Dec(cb)++";"++Dec(sat(col+1))++";"++Dec(sat(row+1))++(release ? "m" : "M"). ∀ cb: u16, encode_urxvt = "\x1b["++Dec(cb)++";"++Dec(sat(col+1))++";"++Dec(sat(row+1))++"M". push_dec_u16(buf,v) appends Dec(v). No overflow at u16::MAX.	5	two while loops over a [u8;8] scratch, Vec push/extend (trust::skip today)	1,2,3,7
mouse-dispatch	contract	mouse::encode_mouse	∀ cb, col, row, enc, release: Lc = release ? (cb & 0b0011_1100)|3 : cb. encode_mouse = encode_x10(Lc,col,row) for X10 (clamped, never an SGR fallback); encode_utf8(Lc,…) for Utf8; encode_sgr(cb,col,row,release) for Sgr and SgrPixel (button identity kept); encode_urxvt(sat_u8(Lc+32),col,row) for Urxvt.	8	enum match, composition with the per-format specs	1,3,4
mouse-button-codes	table-property	MouseButton::code, WheelDir::code	code is xterm's Cb base for button n: n−1 for n ∈ 1..=3 (Left 0, Middle 1, Right 2) and 128+(n−8) for n ∈ {8,9} (Back 128, Forward 129). No button code is 3 (the legacy release code). Wheel n ∈ 4..=7 gives 64+(n−4): Up 64, Down 65, Left 66, Right 67.	2	enum → u8 match	3
mouse-button-slot-injective	table-property	MouseButton::slot	slot is a bijection from the 5 MouseButton variants onto 0..=4 (Left 0, Middle 1, Right 2, Back 3, Forward 4). Recorded reason: code & 7 is not injective (Back and Left collide).	1	enum match	3
wheel-vertical-sense	contract	WheelDir::{vertical_up,is_horizontal}	vertical_up(Up) = Some(true), vertical_up(Down) = Some(false), vertical_up(Left) = vertical_up(Right) = None. is_horizontal(d) iff vertical_up(d) = None.	1	enum match, Option	1,3
mutex-lock-or-recover	contract	<Mutex<T> as MutexExt<T>>::lock_or_recover	∀ T, m: Mutex<T>: lock_or_recover(m) returns a guard to m's inner value whether or not m is poisoned and never panics; *guard is the last value written under any guard, including one held by a thread that panicked. On poison it emits one Error-level log record with the caller's file and line.	2	generic T, std::sync::Mutex poison semantics, threads/panics, #[track_caller], logging side effect	8,11,13
civil-days-roundtrip	round-trip	rfc3339::{civil_from_days,days_from_civil}	∀ days: i64: (y,m,d) = civil_from_days(days) has 1 ≤ m ≤ 12, 1 ≤ d ≤ days_in_month(y,m) (proleptic Gregorian), and days_from_civil(y,m,d) = days. Conversely civil_from_days(days_from_civil(y,m,d)) = (y,m,d) for every valid civil date whose epoch-day value fits i64. Reverse results outside i64 saturate at its bounds.	1	i128 intermediates, division with a floor adjustment, bounded conversion, nonlinear arithmetic (a Clean proof, not SMT)	3,5,14
rfc3339-format-denotation	contract	rfc3339::{format_rfc3339,push_padded}	∀ secs: u64, (y,m,d) = civil_from_days(secs/86400), r = secs%86400: format_rfc3339(secs) = Pad4(y)++"-"++Pad2(m)++"-"++Pad2(d)++"T"++Pad2(r/3600)++":"++Pad2(r%3600/60)++":"++Pad2(r%60)++"Z". The output has 20 bytes iff y ≤ 9999.	1	u64/i64 arithmetic and casts, String push, u64::to_string	1,3,4,7
rfc3339-parse-strict	round-trip	rfc3339::{parse_utc,parse_fields}	∀ s: parse_utc(s) = Some(t) iff |s| = 20, s[19] = 'Z', s matches DDDD-DD-DDTDD:DD:DD at fixed offsets with ASCII digits, 1 ≤ mo ≤ 12, 1 ≤ d ≤ 31, h ≤ 23, mi ≤ 59, se ≤ 60, and t = days_from_civil(y,mo,d)·86400 + h·3600 + mi·60 + se. Round trip: ∀ secs ≤ 253402300799 (end of year 9999), parse_utc(format_rfc3339(secs)) = Some(secs). Offsets, a missing Z, fractions, a space for T and trailing bytes are refused.	1	byte indexing, a closure with a digit loop, i64 arithmetic, composition with rfc3339-format-denotation	1,3,4,5,7,13
rfc3339-parse-fractional	contract	rfc3339::parse_utc_fractional	∀ s: parse_utc_fractional(s) = parse_utc(s[..19]++"Z") iff s ends with 'Z' and the bytes between offset 19 and the final Z are empty or '.' followed by 1..=9 ASCII digits; None otherwise. The fraction is truncated.	2	split_last/split_first on slices, iter().all	1,3,4,5,7
rl-command-position-verdict	refinement	rust_lane::{verdict_in,verdict,check,check_at,split_commands,strip_heredocs,words,offending_words,tool_verdict,find_exec,shell_command_string}	∀ cmd, beside: verdict_in(cmd,beside) = Stock(h) iff some simple command w ∈ RunsAt(cmd) has, at command position, a word whose basename is a STOCK_TOOLS name, and w carries no non-empty ATERM_STOCK_REASON escape. Exempt: (a) dir/rustc with beside(dir, trustc), dir/rustdoc with beside(dir, trustdoc); (b) rustc/rustdoc chosen by a Trust channel (rustup run trust…, +trust…). dir/cargo with beside(dir, targo) is still a hit, with toolchain_dir = dir. Asking where a tool is (which, command -v, type), naming it as data (echo, grep, sed patterns, comments, quoted strings, quoted heredoc bodies, case patterns, ${..} expansions not in command position) and Trust names never hit. check(cmd) = verdict_in(cmd, always-false) projected to Stock.	7	a hand-written shell lexer over Vec<char> with nested loops, depth-bounded recursion, String/Vec heavy, &dyn Fn probe; needs a Clean reference model of POSIX command positions	1,3,4,5,7,10,13
rl-escape-hatch	contract	rust_lane::{verdict,check}	∀ cmd: a stock hit in simple command w is Escaped{hit,reason} iff w's leading assignment words include ATERM_STOCK_REASON=v with v nonempty after shell unquoting (the last assignment wins). An export in an earlier command, another name, an empty value or the word in a non-prefix position escapes nothing. verdict(cmd) = Stock if any unescaped hit exists anywhere in cmd, even when other hits are escaped. check returns None only when every hit is escaped or there are none.	2	Verdict enum folded over segments, String	1,3,5,7
rl-depth-cap-total	no-panic	rust_lane::{verdict_in,check_at,unread}	∀ cmd: verdict_in terminates in time polynomial in |cmd| and never panics, including on unbalanced quotes, parentheses and heredocs. Text nested past MAX_DEPTH = 8 is not parsed: if it contains a stock tool name it is Stock(hit) with hit.unread = true (cleared by an escape on the enclosing command); if not, Clean.	1	recursion with a depth variant, loop variants over char indices, matching_close scans	1,3,5,7
rl-refusal-shape	contract	rust_lane::{refusal,one_command_line}	∀ hit, pin: refusal(hit,pin) begins with a line naming hit.used (for a toolchain_dir hit: that toolchain's compatibility cargo, never "is stock Rust", and no pin); then one indented line per hit.instead spelling (render_spellings); then the pin line iff pin is Some and there is no toolchain_dir; it ends with the ATERM_STOCK_REASON escape sentence. Every echoed command is one_command_line(x): no line break, ≤ REFUSAL_COMMAND_CAP chars, cut on a char boundary. Structure only; exact wording is not spec (§5).	3	String building, char-boundary truncation, enum PinSource	1,3,5,7
rl-hit-fields	contract	rust_lane::check, Hit	For a stock hit on words w = [tool, args…]: hit.used = the tool name plus the +toolchain directive and the verb as typed (e.g. "cargo +1.97.1 test"); hit.instead = trust_spelling(basename(tool), args) with the caller's own arguments, prefixed with toolchain_dir/ when the hit is a compat cargo by path.	1	struct with Vec<Spelling>, composition with rl-trust-spelling	1,4
rl-trust-spelling	contract	rust_lane::{trust_spelling,cargo_spelling,cargo_verb,cargo_verb_index,explicit_toolchain}	∀ stock, args: None unless stock ∈ STOCK_TOOLS. A leading +toolchain arg is dropped. cargo: i = cargo_verb_index(args) (skipping global flags; --color/--config/-Z/-C/--explain take a value), W = [args[i]] ++ args[..i] ++ args[i+1..]. BothLanes ⇒ ["targo trust "++W (VERIFIED), "targo --unverified "++W (UNVERIFIED)]; UnverifiedOnly ⇒ ["targo --unverified "++W]; NoLane, NamedLane, FlagsOnly ⇒ ["targo "++args]; Clippy ⇒ ["targo tippy "++rest]; Unknown ⇒ ["targo "++args] with UNKNOWN_VERB_NOTE. rustc ⇒ [trustc args (VERIFIED), trustc -Ztrust-verify=off args (UNVERIFIED)]; the others give their single STOCK_TOOLS replacement. BOTH_LANES, UNVERIFIED_ONLY and NO_LANE are pairwise disjoint.	1	Vec<String> slicing and extend, String joins, const table membership	1,3,5,7
rl-render-spellings	contract	rust_lane::render_spellings	∀ spellings, indent: one line per spelling: indent++command padded to the widest command plus 3 spaces, then label padded to the widest label and " \u{2014} "++note when there is a label; just indent++command when no spelling has a label. Each line ends in \n.	1	String formatting with computed widths, char counting	1,3,7
rl-channel-in	contract	rust_lane::channel_in	∀ text: channel_in(text) = Some(v) iff some line outside comments is channel = <quoted v> (single or double quotes, any spacing), as rustup reads rust-toolchain(.toml), or the file is a legacy bare channel name; v is the first such value. None otherwise.	1	lines/trim/strip_prefix parsing	1,3,5,7
rl-file-beside	contract	rust_lane::file_beside	∀ dir, name, cwd, home: holds iff resolve(dir)/name is a regular file. resolve: absolute dir as is; relative dir under cwd (None if cwd is None); ~/, $HOME/, ${HOME}/ under home (None if home is None). Any other $, `, *, ? or ~user makes it false. A ~ inside a path is literal.	1	filesystem metadata (is_file), Path join	1,8
scheme-builtin-registry	table-property	scheme::{builtin,builtin_names,builtin_themes,builtin_theme_list,BUILTINS}, Builtin::to_scheme	builtin_names() = ["Default"] ++ [b.name | b ∈ BUILTINS]. builtin_themes() = [(t.name, t.description) | t ∈ builtin_theme_list()], and the names of builtin_theme_list() are builtin_names() in order. Every description is nonempty. No BUILTINS name equals "default" ignoring case. builtin(n) = Some(default()) for n equal to "default" ignoring case; builtin(b.name) = Some(b.to_scheme()) ∀ b ∈ BUILTINS, and to_scheme keeps the name; builtin(x) = None otherwise.	3	static table, eq_ignore_ascii_case, Vec collect, String	1,3,7
scheme-theme-name-grammar	contract	scheme::validate_user_theme_name	∀ name: Ok(()) iff name is nonempty, name = trim(name), name ∉ {., ..}, no control char, no / or \\, no comma, and the trimmed lowercase text before the first ':' (if any) ∉ {dark, light}. The Err reason names the first failing clause in that order.	1	chars().any, split_once, to_ascii_lowercase (String)	1,3,5,7
selector-draw	contract	ThemeSelector::draw_builtin, SelectorDraw::unset, scheme::UNSET_THEME	∀ selector s, appearance a: draw_builtin(s,a) = Unset(builtin(UNSET_THEME)) iff name_for(s,a) = None; Builtin(builtin(n)) iff name_for(s,a) = Some(n) and n names a built-in (case-insensitive; the scheme is canonically named); Other(n) otherwise. UNSET_THEME = Default on every appearance: following the OS is a written pair, never the unset value. aterm-gui's theme resolution and aterm-wasm's theme_api both draw through it.	1	enum with owned payloads, Option combinators	3,12
scheme-projections	contract	ColorScheme::{default,to_color_palette,to_theme_parts}	∀ s: to_color_palette(s).get(i) = s.ansi[i] for i < 16; for i ≥ 16 it is default_color(i) when !s.palette_generate, else generated_extended_palette(s.ansi, s.foreground, s.background)[i-16]. to_theme_parts(s) = {fg: pack(fg), bg: pack(bg), cursor: pack(cursor or else fg), selection: pack(selection_band(s))} with pack(c) = r<<16 | g<<8 | b, selection_band(s) = selection or else derived_selection(fg, bg), and derived_selection(f, b) = per channel round((78·b + 22·f) / 100). default().ansi[i] = default_color(i), so default().to_color_palette() equals ColorPalette::new() pointwise.	2	loop over [Rgb;16], composition with palette-mutators, u32 shifts	1,3,4,7
scheme-builtin-legibility	table-property	BUILTINS, Rgb::contrast	∀ built-in scheme s: s.appearance = Light ⇒ luma(s.background) > luma(s.foreground), luma = 0.299r + 0.587g + 0.114b; contrast(s.foreground, selection_band(s)) ≥ 3.0. ∀ built-in s, Default included: contrast(fg, bg) ≥ 4.5, contrast(selection_band(s), bg) ≥ 1.5, contrast(cursor or else fg, bg) ≥ 3.0, and no ansi[i] = bg. At least 4 built-ins are Light.	1	f64 arithmetic (WCAG luminance with powf), evaluation over a const table; builds on luminance-denotation	1,3,6,7
scheme-parse	contract	scheme::{parse_theme_str,parse_theme_located,parse_scheme_str,parse_hex,lookup_theme_key,strip_inline_comment,THEME_KEYS}	∀ text: a leading U+FEFF is dropped (line numbers unchanged); then, over 1-based lines: skip blank lines and lines starting # or ;. Every other line must be key = value: split at the first '=', value = trim(strip_inline_comment(rest)), nonempty. The key is read by lookup_theme_key over THEME_KEYS (trimmed, ASCII-lowercased; a row's name or any alias, and for the indexed row a stem color|palette|ansi followed by decimal digits N ∈ 0..16): name, description, author (text: at most MAX_THEME_TEXT_CHARS chars, no control char); appearance ∈ dark|light (case-insensitive); palette_generate ∈ true|false (case-insensitive; omitted ⇒ false), any other value an error at its line; foreground|fg, background|bg, cursor|cursor_color, selection|selection_color, selection_foreground and colorN (ansi[N]) take parse_hex: #RRGGBB, bare RRGGBB, or #RGB (the 3-digit form needs the #; d ↦ 17·d). Any other key is an error. A key set twice under any of its spellings (fg then foreground, color1 then palette1) is an error at the second line. Every failure is Err(Parse{line, …}) at the first offending line. After the loop a missing foreground or background is Err(Parse{line: 0}). Unset ansi slots = default_color(i); unset cursor/selection/selection_foreground = None; appearance_declared iff appearance was set. strip_inline_comment cuts at the first whitespace-preceded # or ; that is not the leading token. parse_scheme_str(t) = parse_theme_str(t).map(scheme).	4	lines().enumerate loop, String, closures, usize parse, [bool;16] seen-set (trust::skip today)	1,3,5,7,13
sel-linear	contract	selection::selection_contains_linear	requires (r1,c1) ≤Lex (r2,c2). ∀ row, col: selection_contains_linear(row,col,r1,c1,r2,c2) iff (r1,c1) ≤Lex (row,col) ≤Lex (r2,c2). On one row (r1 = r2 = row) this is c1 ≤ col ≤ c2.	3	scalar i32/usize comparisons, an if/else-if chain with an early return (the chunk listed gap 5; the ensures needs no quantifier)	3
sel-block	contract	selection::selection_contains_block	∀ inputs: selection_contains_block(row,col,r1,c1,r2,c2) iff min(r1,r2) ≤ row ≤ max(r1,r2) ∧ min(c1,c2) ≤ col ≤ max(c1,c2). Symmetric in the two anchors.	4	i32/usize min/max (std calls)	3
sel-glyph-span	contract	selection::glyph_cell_span	∀ col, lead, cont: glyph_cell_span = (sat_sub(col,1), col) if cont; (col, sat_add(col,1)) if lead ∧ ¬cont; (col,col) otherwise. Hence for a wide pair at (c, c+1), 0 ≤ c < u16::MAX: span(c,true,false) = span(c+1,false,true) = (c, c+1). The span never wraps.	3	const fn, saturating u16, tuple return	1,3
sel-side-total-order	contract	<SelectionSide as Ord>::cmp, <SelectionSide as PartialOrd>::partial_cmp	cmp is a total order with Left < Right and each side Equal to itself; partial_cmp = Some(cmp), consistent with the derived Eq.	1	trait impls, match on a tuple of enums	1,3
shell-block-predicates	contract	OutputBlock::{is_complete,succeeded,failed}, CommandMark::{is_complete,succeeded}	∀ b: is_complete iff state = Complete; succeeded iff exit_code = Some(0); failed iff exit_code = Some(c), c ≠ 0; ¬(succeeded ∧ failed). ∀ m: is_complete iff exit_code is Some; succeeded iff exit_code = Some(0).	5	&self on structs with String/Option fields, Option<i32> patterns	1,2
shell-block-row-ranges	contract	OutputBlock::{command_rows,output_rows}	∀ b: output_rows = None if output_start_row is None, else Some((s, end_row or else sat(s+1))). command_rows = None if command_start_row is None, else Some((s, output_start_row or else end_row or else sat(s+1))). The synthesized end is ≥ s and never wraps at u64::MAX.	2	&self, Option<u64>, saturating_add	1,2
caps-feature-list	contract	TerminalCapabilities::{feature_list_string,aterm_capabilities}	∀ caps: feature_list_string = join(";", [name_i | flag_i, in field order]) over the fixed name table (truecolor, 256color, hyperlinks, sixel, Terminal-images, kitty-graphics, clipboard, shell-integration, sync-output, kitty-keyboard, soft-fonts, unicode, bracketed-paste, focus-events, mouse, altscreen). No name is a substring of another's token, so contains(name_i) iff flag_i. aterm_capabilities() has kitty_graphics = true and soft_fonts = false.	2	Vec<&str> push, slice join (String)	1,3,7
font-feature-token	contract	FontFeature::parse_token	∀ t: (tag, v) = (t[1..], 1) if t starts +; (t[1..], 0) if t starts -; (a, parse_u32(trim(b))) if t = a=b split at the first '=' (None when the parse fails); (t, 1) otherwise. tag' = trim(tag). Result = Some(FontFeature{tag: tag' right-padded with spaces to 4 bytes, value: v}) iff 1 ≤ |tag'| ≤ 4, tag' ASCII with no ASCII whitespace; otherwise None. Every Some is well-formed: 4 ASCII bytes <1..=4 non-space><spaces>.	7	strip_prefix, split_once, u32 parse, [u8;4] copy_from_slice (trust::skip today)	1,3,5,7
font-features-list	contract	text_shaping::parse_font_features	∀ spec: parse_font_features(spec) = take(256, filter_map(parse_token, split_whitespace(spec))). Hence length ≤ token count, a blank spec gives [], and it never panics.	3	iterator adapters and collect into Vec	1,4,7
duration-to-nanos	contract	time::duration_to_nanos	∀ d: Duration, duration_to_nanos(d) = min(d.as_secs()·10^9 + d.subsec_nanos(), u64::MAX) = min(d.as_nanos(), u64::MAX).	1	a by-value core::time::Duration struct (alloca refused by trust-mc today), saturating mul/add	1,3
push-hex4	equivalence	trust_fmt::push_hex4	∀ out: String, v: u16, push_hex4(out,v) appends exactly format!("{v:04x}") = Hex4(v), most significant digit first.	1	&mut String push of ASCII chars, shifts, nested fn	1,2,3
debug-string	equivalence	trust_fmt::debug_string, DebugAsDisplay::fmt	∀ T: Debug, v: T, debug_string(v) = format!("{:?}", v).	1	generic T: Debug, the fmt::Formatter machinery (absent std bodies), trust::skip today	1,11,12
build-stamp-first-install-wins	contract	version::{install,installed,app_version}	Over a process history: before any install, installed() = None and app_version() = "0.0.0". After installs s1, s2, …, installed() = Some(s1) and app_version() = s1.app_version; later installs have no effect.	1	a global OnceLock static (process state), &'static references	8,13
vt-decscl-roundtrip	table-property	VtLevel::{decscl_param,from_decscl_param}	∀ p: u8, from_decscl_param(p) = Some(l) ⇒ decscl_param(l) = p, and it is Some iff 61 ≤ p ≤ 65. ∀ l: from_decscl_param(decscl_param(l)) = Some(canon(l)), canon mapping each level to its decscl class representative VT100, VT220, VT320, VT420 or VT520 (the identity on those five).	1	const fn enum matches	1,3
x11-table-wellformed	table-property	x11_colors::X11_COLORS	The X11_COLORS names are strictly increasing in byte order, all lowercase ASCII, each 1..=24 bytes. Every "gray" name has a "grey" twin with the same RGB.	2	evaluation over a 140-entry const table	1,3,7
x11-lookup	contract	x11_colors::lookup	∀ name: lookup(name) = Some(Rgb(r,g,b)) iff name is nonempty ASCII, |name| ≤ 24, and (ascii_lowercase(name), [r,g,b]) ∈ X11_COLORS; None otherwise (the binary search agrees with linear membership, given x11-table-wellformed).	5	[u8;24] stack buffer zip loop, binary_search_by with a closure, slice get (trust::skip today)	1,3,4,7,13
xtk-state-machine	contract	XtermKeyboardState::{new,default,reset,set_modify_other_keys,reset_modify_other_keys,disable_modify_other_keys,set_format_other_keys,reset_format_other_keys,modify_other_keys,format_other_keys}	new() = reset(any) = (mok: Some(0), fok: 0); the derived default() = (None, 0) and deliberately differs (§5). ∀ s, v: u8: set_modify_other_keys(v) ⇒ mok' = Some(min(v,2)); reset_modify ⇒ mok' = Some(0); disable ⇒ mok' = None; set_format_other_keys(v) ⇒ fok' = min(v,1); reset_format ⇒ fok' = 0. Each mutator leaves the other field unchanged. Every constructor and mutator preserves XInv.	8	&mut self on a Copy struct with an Option<u8> field, u8::min, derived Default, two-state postconditions	1,2,3
xtk-query-responses	contract	XtermKeyboardState::{query_modify_other_keys_response,query_format_other_keys_response}	∀ s: query_modify_other_keys_response(s) = "\x1b[>4m" if mok = None, else "\x1b[>4;"++Dec(v)++"m" for Some(v). query_format_other_keys_response(s) = "\x1b[>4;"++Dec(fok)++"f". With xtk-state-machine: default() and disable answer "\x1b[>4m", new() answers "\x1b[>4;0m".	3	String push_str/push, u8::to_string (decimal rendering needs a model), match on Option<u8>	1,3,4
kb-mode-bit-layout	table-property	KeyboardMode consts, KITTY_PROTOCOL_FLAGS	The 16 KeyboardMode flag consts are pairwise distinct single bits 1<<0 … 1<<15, WIN32_INPUT = 1<<15. KITTY = DISAMBIGUATE | REPORT_EVENT_TYPES | REPORT_ALTERNATE_KEYS | REPORT_ALL_KEYS_AS_ESC | REPORT_ASSOCIATED_TEXT and does not intersect WIN32_INPUT. empty() contains none of them. With bitflags-boolean-algebra every subset is representable and contains-exact.	4	const evaluation of bitflags consts	3
kb-mode-accessors	contract	KeyboardMode::{xterm_modify_other_keys_level,xterm_format_other_keys}	∀ mode: level = 2 if XTERM_MODIFY_OTHER_KEYS_LEVEL2 ∈ mode, else 1 if LEVEL1 ∈ mode, else 0. xterm_format_other_keys iff XTERM_FORMAT_OTHER_KEYS ∈ mode.	1	bitflags contains	3
kb-termmode-projection	contract	TermMode::{from_keyboard_state,to_keyboard_mode}	∀ app_cursor, app_keypad, vt52, kitty: KittyKeyboardFlags, x: XtermKeyboardState: from_keyboard_state sets exactly APP_CURSOR, APP_KEYPAD, VT52 per the bools, the 5 kitty bits per kitty, LEVEL1 iff x.mok = Some(1), LEVEL2 iff x.mok = Some(2), FORMAT iff x.fok = 1; no other TermMode bit. ∀ tm: to_keyboard_mode(tm) = tm projected onto the same-named KeyboardMode flags, every non-keyboard flag dropped. Hence to_keyboard_mode(from_keyboard_state(…)) has exactly those flags.	6	bitflags across two types, struct params	1,3
kb-wire-values	contract	Modifiers::{kitty_encoded,xterm_encoded}, KeyEventType::kitty_value	∀ m: Modifiers, kitty_encoded(m) = 1 + m.bits(), lock bits included (CAPS_LOCK = 64, NUM_LOCK = 128). xterm_encoded(m) = 1 + (m.bits() & chord bits), locks excluded. kitty_value: Press = 1, Repeat = 2, Release = 3.	2	u8 arithmetic on bitflags	3
kb-kitty-code-table	table-property	NamedKey::kitty_code	∀ NamedKey k, kitty_code(k) = the kitty protocol functional-key code: Escape 27, Enter 13, Tab 9, Backspace 127, Space 32; Insert 57348, Delete 57349, Left 57350, Right 57351, Up 57352, Down 57353, PageUp 57354, PageDown 57355, Home 57356, End 57357, CapsLock 57358, ScrollLock 57359, …; F1..F35 = 57364..57398; KP_0..KP_9 = 57399..57408, …; media keys 57428 and up; ShiftLeft 57441 … MetaRight 57452. kitty_code is injective on NamedKey.	6	a ~120-arm enum → u32 match	1,3
kb-dom-map	contract	keyboard::{map_dom_key,map_dom_named_key}	∀ key: &str: exactly one char c ⇒ Some(Named(Space)) for c = ' ', else Some(Character(c)). Multi-char ⇒ Some(Named(n)) iff key is in the UI Events name table (navigation, editing, Escape, ContextMenu/Pause/PrintScreen, the locks, F1..F35, media/audio keys, and the modifiers mapped Left-canonical: Shift→ShiftLeft, Control→ControlLeft, Alt→AltLeft, Meta→SuperLeft); else None: empty, multi-codepoint text, Dead, Process, Compose, Unidentified, AltGraph, Fn, F0, F36+.	8	chars iterator, a large &str match	1,3
kb-dom-encode	contract	keyboard::encode_dom_key	∀ key, mods, ev, base, mode: None if map_dom_key(key) = None or ev > 2. Otherwise b = encode_key_with_layout(map(key), Modifiers::from_bits_truncate(mods), mode, [Press,Repeat,Release][ev], base); result = None if b is empty, else Some(b). mods are the chord's modifiers: a host clears CTRL and ALT on a press AltGraph chose (Windows reports AltGr as both, the glyph already in key), so a Ctrl+Alt that reaches the encoder is a chord (AltGr+Q's '@' with them left on is ESC NUL, AltGr+E's '€' ESC ^E, by kb-layout-text's Ctrl twin on base).	3	composition with the encoder specs, Option<Vec<u8>>	1,4
kb-shift-map	refinement	keyboard::{shifted_character,encode_key}	∀ ASCII c with an ANSI US-QWERTY shifted glyph S(c) (a-z→A-Z, the digit row → !@#$%^&*(), and the 11 punctuation pairs): shifted_character(c, SHIFT) = Some(S(c)). Corollary: encode_key(Character(c), SHIFT, ∅) ≠ encode_key(Character(c), ∅, ∅) for every such c.	2	char matches, the encoder (trust::skip today), Vec<u8>	1,3,4,5
kb-write-u32	contract	keyboard::encode::{write_u32,write_u8}	∀ buf, v: u32, write_u32(buf,v) appends Dec(v) and terminates, u32::MAX included. write_u8 likewise appends Dec(v).	1	a digit loop into a fixed scratch array, &mut Vec push	1,2,3,7
kb-shift-enter-distinct	contract	keyboard::encode_key	∀ mode ∈ Modes(16): encode_key(Named(Enter), SHIFT, mode) ≠ encode_key(Named(Enter), ∅, mode), both nonempty. Witnesses: legacy CR vs LF; DISAMBIGUATE CR vs CSI 13;2u; LEVEL2 CR vs CSI 27;2;13~; WIN32 CR vs the VK_RETURN record pair.	2	∀ over 2^16 modes (exhaustive or symbolic), the whole encoder	1,3,4,5,7
kb-release-gating	contract	keyboard::{encode_key_with_event,should_encode_kitty_event,encode_kitty}	∀ k, m, mode: (1) REPORT_EVENT_TYPES ∉ mode ⇒ Enc(k,m,mode,Release) = []. (2) REPORT_ALL_KEYS_AS_ESC ∉ mode ∧ (TextKey(k,m) ∨ k ∈ RecoveryKey) ⇒ Enc(k,m,mode,Release) = []. (3) Enc(k,m,mode,Release) ≠ [] ⇒ it differs from Enc(k,m,mode,Press). (4) k ∈ ModKey ⇒ Enc(k,m,mode,ev) = [] ∀ ev unless REPORT_ALL_KEYS_AS_ESC ∈ mode, and then the report carries the modifier's own bit only as present in m. (5) Lemma (why the gate exists): encode_kitty(k,m,mode,ev) with REPORT_EVENT_TYPES ∉ mode does not depend on ev. Win32 Release is [] (kb-win32-records).	8	∀ over mode space × key grid, Vec<u8>, the encoder (trust::skip)	1,3,4,5,7
kb-kitty-text-preserving	equivalence	keyboard::encode_key_with_event	∀ mode with mode ∩ KITTY ≠ ∅, REPORT_ALL_KEYS_AS_ESC ∉ mode, WIN32_INPUT ∉ mode, ∀ ev ∈ {Press, Repeat}: for TextKey(k,m), or k ∈ {Enter, Tab, Backspace} with m = ∅, Enc(k,m,mode,ev) = encode_key(k, m, Legacy(mode)). Text stays text (A, not CSI 97;2u), including in the Codex mode 1|2|4.	3	equivalence of two encoder paths	1,3,4,5,7
kb-kitty-csi-u-denotation	contract	encode_kitty, encode_kitty_legacy_functional, kitty_key_codes, kitty_modifiers_for_event, append_mod_event, letter_final, tilde_final	∀ k, m, ev, mode with DISAMBIGUATE ∈ mode, WIN32 ∉ mode, the event not covered by kb-kitty-text-preserving or kb-release-gating. Keys with a legacy functional form (arrows, Home/End, F1-F4 as letter finals; Insert/Delete/PgUp/PgDn/F5-F12 as tilde numbers): CSI [1;]mod[:ev] X or CSI n[;mod[:ev]] ~. Every other key: CSI code[:alts] [;mod[:ev]] [;text] u, code = the lowercase/unshifted codepoint or kitty_code, mod = kitty_encoded(m) (omitted when 1 and no :ev), :ev = :kitty_value(ev) present iff REPORT_EVENT_TYPES ∈ mode ∧ ev ≠ Press. Escape and Alt/Ctrl/Ctrl+Alt/Shift+Alt chords are always CSI u.	3	Vec<u8> building, decimal writes, nested matches over Key/NamedKey	1,2,3,4,7
kb-kitty-progressive	contract	encode_kitty, associated_text_codepoints, named_key_text, should_encode_kitty_event	Extends kb-kitty-csi-u-denotation. REPORT_ALL_KEYS_AS_ESC: every key, text and modifier keys included, is a CSI u report, except the legacy functional keys keep their CSI letter/tilde forms. REPORT_ALTERNATE_KEYS: code gets :shifted when Shift changes the glyph and ::base / :shifted:base from the base-layout key. REPORT_ASSOCIATED_TEXT under report-all: a third field of the codepoints the press typed (the glyph, a keypad key's main-block glyph, Space's U+0020); omitted for named keys whose text is a control code (Enter, Tab, Backspace, Escape) and for keys with no text.	7	char → codepoint, Vec<u32>, encoder branches	1,3,4,7
kb-keypad-fold	contract	Key::main_block_twin, encode_key_with_event	main_block_twin maps each keypad key to its main-block twin: Numpad0-9 → digits; Decimal, Separator, Divide, Multiply, Subtract, Add, Equal → . , / * - + =; NumpadEnter → Enter; numpad arrows, Home, End, PgUp/Dn, Insert, Delete → the named keys; None for NumpadBegin and every main-block key. Encoding: under a kitty mode without DISAMBIGUATE and APP_KEYPAD ∉ mode, a keypad key encodes as its twin's text press/repeat. APP_KEYPAD outranks the fold (SS3 form); DISAMBIGUATE keeps the dedicated keypad code; associated text carries the twin's glyph. A modified keypad key never composes the main row's shifted or control glyph (Shift+KP_5 is 5, never %).	5	enum matches, &self	1,2,3,4
kb-xterm-mok	contract	encode_xterm_other_keys, xterm_modify_other_keys_level1_applies, xterm_level1_character_applies, xterm_modify_other_keys_level2_applies	∀ k, m, mode with mode ∩ KITTY = ∅, WIN32 ∉ mode, Lv = xterm_modify_other_keys_level(mode) > 0. If Lv = 1 and k is a Character whose Ctrl/Ctrl+Shift chord has no traditional C0 byte (Ctrl+Shift+c, Ctrl+1), or Lv = 2 and k is a modified Character whose result is not plain shifted text (Shift alone on a char whose shifted glyph is outside @..~ and [ \\ ] ^ _ stays text), or k = Backspace with Shift at Lv = 2: encode_key = CSI 27;xterm_encoded(m);code ~, or CSI code;mod u under FORMAT_OTHER_KEYS, code = the shifted keysym, as xterm reports it: the layout's when known, else the US Shift table (Shift+2 = 64, Ctrl+Shift+c = 67). Otherwise the legacy encoding (Alt+Backspace = ESC DEL, Ctrl+Backspace = BS, keypad untouched, arrows legacy). Any kitty flag disables modifyOtherKeys.	8	char predicates, Vec<u8>	1,3,4,7
kb-legacy-character	contract	encode_legacy, typed_character, shifted_character	∀ Character(c), m, with mode ∩ (KITTY ∪ XTERM ∪ WIN32) = ∅. Ctrl: the C0 byte of the chord's glyph (a-z and @[\\]^_ → 0x00-0x1F, ? → 0x7F, Ctrl+Shift+- → US 0x1F, Ctrl+~ → RS 0x1E, Ctrl+Space → NUL). Shift: UTF-8 of shifted_character. Alt: ESC prefix of the unmodified result unless ALT_NO_ESC; with Ctrl, ESC prefix of the Ctrl byte (C-M-SPC = ESC NUL, M-C-h = ESC BS). Otherwise UTF-8 of typed_character(c,m). The emacs/readline bindings (Alt+b/f/d, Ctrl+a/e/k…) are instances.	8	char matches, UTF-8 encoding of char, Vec<u8>	1,3,4,7
kb-legacy-named	contract	encode_legacy, encode_{control,navigation,function,numpad}_named_legacy, encode_arrow, encode_home_end, encode_tilde_key, encode_f1_f4, encode_numpad	∀ named k, m, legacy mode (no kitty, xterm or win32 bits), chord = m minus the lock bits (CAPS/NUM never change a navigation or function form). Arrows: CSI A-D, SS3 under APP_CURSOR, CSI 1;xterm(chord) X when chord ≠ ∅. Home/End: CSI H/F likewise. Ins/Del/PgUp/PgDn/F5-F12: CSI n~ / CSI n;mod~, Shift+F10 = CSI 21;2~ (kf22). F1-F4: SS3 P-S, or CSI 1;mod P-S. Enter: CR, Shift+Enter LF, Ctrl CR, Alt ESC CR. Tab: HT, Shift+Tab CSI Z. Backspace: DEL, BS under BACKARROW_SENDS_BS, Ctrl inverts it. Escape: ESC. Numpad without APP_KEYPAD: the main-block twin's bytes (KP_Enter = main Enter); with APP_KEYPAD: SS3 p-y/M/k/m/j/o/X…, not cancelled by NumLock. KP_Begin: CSI E or SS3 E, never a digit.	10	large NamedKey matches, Vec<u8> building, bitflags	1,3,4,7
kb-legacy-mode-switches	contract	encode_legacy, alt_prefixed	∀ k, m, legacy mode: ALT_NO_ESC ∈ mode ⇒ Alt+x encodes without the ESC prefix (the Alt-free encoding). META_SENDS_ESC ⇒ a META-modified key is the ESC-prefixed Meta-free encoding. NO_SPECIAL_MODIFIERS ⇒ the encoding equals that of m minus NUM_LOCK. Without the flags: Alt prefixes ESC, Meta is ignored, NumLock is a special modifier.	3	bitflags, Vec prefixing	1,3,4
kb-vt52	contract	encode_legacy, encode_arrow, encode_f1_f4, encode_home_end, encode_numpad	∀ m, under VT52_MODE (no kitty or win32 bits): arrows ESC A-D whatever APP_CURSOR and m; F1-F4 ESC P-S (PF1-PF4); Home/End ESC H / ESC F, modifiers ignored; F5+ keep the ANSI form. Numpad with APP_KEYPAD: ESC ? p..y and the VT52 application forms; without it, the plain digits and glyphs.	6	encoder branches	1,3,4
kb-layout-text	contract	keyboard::encode_key_with_text, layout_glyph, chord_ctrl_byte, ctrl_layout_twin, encode_character_legacy, typed_character	∀ c, m, mode, ev, base, text. typed_character(c,m) = shifted_character(c,m) when SHIFT ∈ m, else (CAPS_LOCK ∈ m ? uppercase(c) : c); Shift and Caps both held gives the shifted glyph. t = layout_glyph(k, text): text, the layout's character at the press's Shift and lock level with Ctrl and Alt aside, when k is a Character and text is not a control character; a named key takes none. On a text press (no Ctrl/Alt/Meta/Hyper/Super) t, when Some, replaces the US-derived glyph in every mode where text stays text. Kitty reports keep code = the unshifted layout key; the shifted key under REPORT_ALTERNATE_KEYS is t while Shift is held, chord or not, and the associated text is t. In the legacy, xterm and win32 paths a Ctrl chord's byte is chord_ctrl_byte(c,m,t) = ctrl(c), else ctrl(t when Some, else shifted_character(c,m)) (German Ctrl+Shift+ß types '?': DEL; Ctrl+Shift+/ stays 0x1F). When that is None on a non-ASCII c and the physical key's US character base has a control byte, the press is encoded as Character(base) and without t (ctrl_layout_twin): Russian Ctrl+С = 0x03, Russian Ctrl+х on the [ key = ESC, AZERTY Ctrl+é on the 2 key = NUL, and modifyOtherKeys level 2 reports base's keysym (Russian Ctrl+Shift+х = CSI 27;6;123~). An Alt chord (Meta under META_SENDS_ESC) with no control byte is ESC + (t when Some, else Shift ? shifted_character(c,m) : lowercase(c)): German Alt+Shift+2 = ESC ", US Alt+Shift+2 = ESC @. A chord with neither a control nor an ESC form (no control byte for c, t, shifted_character or base; or Alt under ALT_NO_ESC) writes the character the layout types at that level, t, else the US Shift table's: German Ctrl+ö on the ; key = ö.	8	Option<char> params, char case mapping (Unicode for non-ASCII), encoder composition	1,3,4,7
kb-win32-records	contract	win32_key_record, win32_enter_chord, win32_escape_record, win32_ctrl_character_record, conhost_holds_once_switched, us_physical_key, encode_win32_record_pair	∀ k, m, mode with WIN32_INPUT ∈ mode. (1) conhost_holds_once_switched(b) iff b is a lone ESC or an ESC-led sequence conhost leaves dangling (ESC, ESC [, ESC O, ESC ESC…; the measured set). (2) For a key whose legacy bytes satisfy (1), or Enter with a nonempty chord (Shift, Ctrl, Alt, combinations; NumpadEnter as an enhanced key), or a Ctrl letter chord whose legacy bytes are a lone BS (Ctrl+H, Ctrl+Shift+H, Ctrl+H under Caps Lock, a non-Latin layout's Ctrl+H key through its US twin; conhost reads a bare BS as Ctrl+Backspace; a BS character, no letter, keeps its byte): Press and Repeat give the record pair Win32Rec(vk,sc,uc,1,cs) ++ Win32Rec(vk,sc,uc,0,cs) from us_physical_key and the Windows control-key state; Release gives []. Escape, Ctrl+[, Ctrl+3, Alt+[ and Alt+O are records of their own physical key. Ctrl+H is VK_H with the physical key's scan (0x23 on US), uc BS, cs LEFT_CTRL alone whether Shift is held or the char is H (BS carries no Shift; PSReadLine 2.4.5/2.0.0 insert a literal BS on a SHIFT_PRESSED Ctrl+H record, measured 2026-10-16). (3) Every other key keeps exactly its non-win32 encoding (plain Enter CR, plain text text, Ctrl+Backspace BS, Ctrl+C ETX, Alt+Ctrl+H ESC BS); the one change outside the held forms and the Enter chords is the Ctrl+H record of (2) (win32_ctrl_h_is_a_record_of_the_h_key, win32_ctrl_h_record_outranks_kitty_and_modify_other_keys). (4) On the keys of (2) WIN32 outranks kitty flags and modifyOtherKeys; elsewhere kitty flags still apply. Across the printable-ASCII keyboard WIN32 changes exactly the held forms and Ctrl(+Shift)+h/H (win32_changes_only_the_held_forms_across_the_whole_ascii_keyboard).	12	struct record building, decimal writes, &[u8] predicate, encoder composition	1,3,4,5,7
contrast-symmetric-bounded	contract	Rgb::contrast, Rgb::luminance (opaque)	la = clamp(L_F(a),0,1), lb = clamp(L_F(b),0,1). ∀ a, b: Rgb, contrast(a,b) = fl((max(la,lb)+0.05)/(min(la,lb)+0.05)). Corollaries: (i) contrast(a,b) = contrast(b,a) bit for bit; (ii) contrast(a,a) = 1.0 exactly (x/x = 1 for finite x ≥ 0.05); (iii) 1.0 ≤ contrast(a,b) ≤ fl(1.05/0.05). Holds for ANY luminance with range [0,1], so luminance is treated as opaque: only the clamp postcondition is needed, not powf.	3	f64 add/div/compare, f64::clamp, a call used only through its range postcondition, Copy struct of three u8; contrast is #[trust::skip] today (float-interval lane)	3,4,6
luminance-denotation	contract	Rgb::luminance, Rgb::luminance::linearize, Rgb::contrast	∀ rgb: |L_F(rgb) − L_R(rgb)| ≤ eps. Exact endpoints: lin_F(0) = 0.0, lin_F(255) = 1.0 (powf(1.0,2.4) = 1); hence L_F(0,0,0) = 0 and L_F(255,255,255) = fl(0.2126+0.7152+0.0722), within 1 ulp of 1. L_F is additive over channels within eps. With contrast-symmetric-bounded, contrast(white,black) = 21 within eps. The pinned point values (lin(128) ≈ 0.2158605; the pure primaries 0.2126, 0.7152, 0.0722) are instances.	8	f64 arithmetic, f64::from(u8), a piecewise branch, f64::powf (inexact, refused by Trust by design; needs a pow error model or a 256-entry real-valued table of lin), nested private fn, real-vs-float error reasoning	3,6,14
luminance-monotone	contract	Rgb::luminance::linearize, Rgb::luminance	lin_F is strictly increasing on u8: ∀ c < d, lin_F(c) < lin_F(d), including 10 → 11 across the 0.04045 seam. Hence L_F is strictly increasing in each channel with the others fixed, and along the gray ramp v ↦ (v,v,v). Decidable by evaluating the 256 lin_F values once a pow model exists (lin_F(10) < 0.005 is one).	2	the float and powf obligations of luminance-denotation, plus a 256-case check or per-branch monotonicity with the seam	3,5,6
gray-aa-threshold	table-property	Rgb::contrast	∀ v: u8, contrast(white, (v,v,v)) ≥ 4.5 iff v ≤ 0x76. Follows from luminance-monotone (contrast with white strictly decreasing in v) plus the two points v = 0x76 (≈4.54) and v = 0x77 (≈4.48).	1	f64 comparisons, a 256-gray sweep or monotonicity plus two points; depends on the pow model	3,5,6
kitty-flags-mask-accessors	contract	KittyKeyboardFlags::{none,from_bits,bits,contains,disambiguate,report_events,report_alternates,report_all_keys,report_text}	∀ b: u8, from_bits(b).bits() = b & KMASK and none().bits() = 0. ∀ f, i ∈ 0..5, kacc_i(f) = (f.bits() & (1<<i) ≠ 0), so from_bits(1<<j) turns on kacc_i exactly when i = j. Type invariant: every KittyKeyboardFlags built by none, from_bits, Default or apply has bits() ≤ KMASK (serde Deserialize excepted; §5).	4	u8 newtype, const fns, bitwise & and ≠ 0, calls to admitted fns; 256 × 5 cases decidable by evaluation	1,3,4
kitty-flags-apply	contract	KittyKeyboardFlags::apply, KittyKeyboardState::set_flags	∀ f, bits, mode: u8, mk = bits & KMASK: apply(f,bits,mode) gives f'.bits() = mk for mode ∈ {0,1}; f.bits() | mk for 2; f.bits() & !mk for 3; f.bits() for mode ≥ 4. apply preserves bits() ≤ KMASK. set_flags(s,bits,mode) changes only s.flags, as apply does; every other KAbs field and every stack slot is unchanged.	7	&mut self on a u8 newtype, match on u8 with or-pattern and wildcard, |=, &=, !, a delegating &mut self call composed through the callee's ensures	2,3,4,5
kitty-state-new-reset	contract	KittyKeyboardState::{new,default,reset,flags,query_flags}	KAbs(new()) = KAbs(default()) = (none, None, None, [], []) and KInv(new()). ∀ s, KAbs(reset(s)) = KAbs(new()) and every raw stack slot is the default entry. query_flags(s) = s.flags().bits(). Corollary (with kitty-push-pop-stack-model, kitty-switch-screen): after reset every pop and switch_screen yields flags 0 until a new set_flags or push.	2	struct with Option fields and [Entry;8] arrays, &mut self field assignments, array repeat expressions	1,2
kitty-push-pop-stack-model	refinement	KittyKeyboardState::{push_flags_for_buffer,pop_flags_for_buffer}, KittyKeyboardStackEntry::{new,flags}	∀ s with KInv(s), x: u8, n: u16, buf, S = KStk(s,buf). push: KStk(s',buf) = takeLast(7, S ++ [s.flags]), s'.flags = from_bits(x), the other buffer's stack and both saved slots unchanged. pop with n = 0: s' = s. pop with n > |S|: s'.flags = none, KStk(s',buf) = []. pop with 1 ≤ n ≤ |S|: s'.flags = S[|S|−n], KStk(s',buf) = take(|S|−n, S); the other buffer and saved slots unchanged. Both preserve KInv (sp ≤ 7, every index in bounds). Entry round trip: ∀ f with bits ≤ KMASK, StackEntry::new(f).flags() = f. buf picks the stack independently of the active screen; the live flags are shared.	11	&mut self, a tuple of two &mut borrows into self chosen by matches!, [Entry;8] with masked indices, a for 0..6 eviction-shift loop, saturating_add/wrapping_sub on usize, u16 → usize cast, early returns, a list abstraction of an array prefix; push is #[trust::skip] today	1,2,3,4,5,7
kitty-switch-screen	contract	KittyKeyboardState::switch_screen	∀ s: switch_screen(s,true) gives main_saved' = Some(s.flags), flags' = s.alt_saved.unwrap_or(none), alt_saved' = None. switch_screen(s,false) gives alt_saved' = Some(s.flags), flags' = s.main_saved.unwrap_or(none), main_saved' = None. Both stacks unchanged; KInv preserved. Corollary: switch(true); switch(false) restores flags, alternation keeps each screen's flags separately, and a repeated switch(true) overwrites main_saved with the current flags.	5	&mut self, Option::take/unwrap_or on Option<newtype>, a bool branch, a two-state postcondition over several fields	1,2,4
screen-buffer-from-bool	contract	<ScreenBuffer as From<bool>>::from	∀ b: bool, ScreenBuffer::from(b) = (b ? Alternate : Main).	1	a fieldless #[non_exhaustive] enum result, if on bool, a trait impl method	1,11
kitty-snapshot-roundtrip	round-trip	KittyKeyboardState::{snapshot,restore_snapshot}	Normal(snap): main_sp, alt_sp ≤ 7, every flags value has bits ≤ KMASK, and main_stack[i] = none for i ≥ main_sp (alt likewise). (a) ∀ s, ∀ snap with Normal(snap): snapshot(restore_snapshot(s,snap)) = snap. (b) ∀ snap: restore clamps both sp to min(sp,7), keeps the first sp entries and zeroes the rest, so snapshot(restore(s,snap)) = normalize(snap) whatever s was. (c) ∀ s ∈ KReach, ∀ t: KAbs(restore_snapshot(t, snapshot(s))) = KAbs(s), so later pushes and pops behave as on s.	1	iter_mut().zip() / take(n).zip() loops over [_;8], usize::min, usize → u8 cast, usize::from(u8), derived PartialEq, &mut self	1,2,4,7
scheme-generated-palette	table-property	scheme_palette256::{generated_extended_palette,palette_anchors,oklab_to_linear}	∀ ansi, fg, bg, with (D, W) = palette_anchors(ansi, fg, bg) — D = ansi[15] if L(ansi[15]) < L(ansi[0]) else ansi[0]; W = the first of ansi[0], ansi[7], ansi[15], fg, bg of greatest OkLab L — g = generated_extended_palette(ansi, fg, bg): cube slot 16+36r+6g+b (r,g,b ∈ 0..6) is, when every level is 0 or 5, its corner byte for byte (16 = D, 196 = ansi[9], 46 = ansi[10], 226 = ansi[11], 21 = ansi[12], 201 = ansi[13], 51 = ansi[14], 231 = W), else the OkLab trilinear blend of the eight corners at t = XTERM_CUBE_LEVELS[v]/255 per axis; grey slot 232+i is the OkLab blend of D and W at t = (8+10i)/255. A blend outside sRGB keeps L and hue and takes the largest chroma a 20-step bisection finds in gamut; encoding rounds half up. Pure and deterministic; the ramp's OkLab L rises from 232 to 255 whenever L(D) < L(W), whatever the order of ansi[0] and ansi[15].	3	f64 OkLab transforms (trust::skip), fixed [Rgb;8] corners, [Rgb;240] output, bisection loop	1,3
scheme-palette-generate-key	contract	scheme::{THEME_KEYS,parse_theme_str,ThemeKey::PaletteGenerate}, scheme_import::to_native_conf_package, scheme_import (Ghostty palette-generate)	∀ text: the theme key palette_generate (row 11 of THEME_KEYS, type Flag) sets ColorScheme::palette_generate; omitted ⇒ false; a value other than true|false (any case) or a second setting is Err(Parse{line}) at that line. The native writer emits `palette_generate = true` iff the flag is set, so write∘parse is the identity on it. A Ghostty file's `palette-generate = true|false` carries to the flag; any other value is listed in the import's dropped keys.	2	THEME_KEYS row order const assert, Flag reader, Draft field	1,3,5,7
```

## 2. Test → spec map (TSV)

`primary` is the spec that retires the test. `also` lists the other specs the same test samples. The
tests of §4 have no row.

Deleted since (rows kept for the record): `summaries_fit_the_short_catalog_budget` (the summary and
short-catalog caps are a `const` assertion over `VERBS`, a build error) and
`access_exceptions_are_exactly_the_declared_sets` (a `VERBS` row with no dispatch arm answers
`ERR <verb> has no handler`, so a missing arm fails closed).

```tsv
file	test	primary	also
src/app_inspection.rs	inspect_version_and_stable_view_are_explicit	app-inspect-grammar	
src/app_inspection.rs	semantic_response_envelope_pins_version_subject_and_revision	app-envelope-header	
src/app_inspection.rs	action_targets_view_and_semantic_key_not_focus	app-act-grammar	
src/app_inspection.rs	open_request_is_parsed_but_not_canonicalized_or_authorized	app-open-grammar	
src/app_inspection.rs	opaque_tokens_and_payloads_are_bounded_and_line_safe	app-inspect-grammar	app-act-grammar, app-open-grammar
src/bitflags.rs	operations_produce_expected_bits	bitflags-boolean-algebra	
src/color_palette.rs	palette_cache_agrees_with_overrides_after_mutations	palette-model-invariant	
src/control_socket.rs	socket_key_collapses_the_verbatim_drive_spelling_and_nothing_else	cs-socket-key	
src/control_socket.rs	instance_names_roundtrip_through_pid_parse	cs-instance-name-pid	
src/control_socket.rs	instance_pid_rejects_fixed_and_malformed_names	cs-instance-name-pid	
src/control_socket.rs	graph_entry_sock_parses_the_shared_on_disk_format	cs-graph-entry-lines	
src/control_socket.rs	graph_entry_pid_parses_hosting_pid_and_fails_closed	cs-graph-entry-lines	
src/control_socket.rs	token_choice_follows_symlink_target	cs-token-name-injective	
src/control_socket.rs	explicit_sockets_get_their_own_token_each	cs-token-name-injective	
src/control_socket.rs	odd_socket_names_never_land_on_a_reserved_token_name	cs-token-name-injective	
src/control_socket.rs	the_token_name_rule_is_injective_over_socket_names	cs-token-name-injective	
src/control_socket.rs	read_candidates_put_the_per_socket_token_first	cs-token-candidates	
src/control_socket.rs	stale_sweep_removes_only_dead_instances	cs-stale-instance-files	
src/control_verbs.rs	the_host_gone_line_round_trips_and_nothing_else_reads_as_it	cv-host-gone-roundtrip	
src/control_verbs.rs	a_rows_fence_round_trips_refuses_bad_shapes_and_hashes_the_rows_joined	cv-rows-fence-wire	
src/control_verbs.rs	screen_gen_parses_exactly_the_wire_spelling	cv-screen-gen-wire	
src/control_verbs.rs	json_flag_switches_every_json_capable_verb_to_lines	cv-framing-of	
src/control_verbs.rs	framing_base_and_sub_forms	cv-framing-of	cv-access-classification
src/control_verbs.rs	artifact_ack_classification_covers_every_guarded_subform	cv-artifact-ack	
src/control_verbs.rs	summaries_fit_the_short_catalog_budget	cv-verb-table-wellformed	
src/control_verbs.rs	entry_lines_wrap_every_verb_deterministically_and_losslessly	cv-entry-lines-wrap	
src/control_verbs.rs	access_exceptions_are_exactly_the_declared_sets	cv-access-classification	
src/control_verbs.rs	fabric_verbs_carry_their_designed_classes	cv-framing-of	cv-access-classification
src/control_verbs.rs	copy_and_settings_have_their_own_op_class	cv-access-classification	
src/control_verbs.rs	privacy_is_a_headless_answerable_meta_read	cv-framing-of	cv-access-classification
src/dirs.rs	an_empty_or_relative_home_is_no_home	dirs-home-from-env	
src/dirs.rs	aterm_config_path_follows_the_one_rule_the_window_loads_by	dirs-config-path	
src/dirs.rs	state_dir_honours_an_absolute_override_and_refuses_a_relative_one	dirs-state-dir	
src/dirs.rs	logs_dir_follows_each_platforms_convention	dirs-logs-dir	
src/dirs.rs	aterm_data_dir_follows_the_state_home_seam_and_else_the_os_data_dir	dirs-data-dir	
src/env_sanitize.rs	a_non_utf8_key_is_classified_by_its_lossy_spelling	env-key-lossy	
src/fabric_grammar.rs	a_principal_is_bounded_and_class_prefixed	fab-principal	
src/fabric_grammar.rs	a_topic_is_one_bounded_lowercase_segment	fab-topic	
src/fabric_grammar.rs	a_via_chain_is_bounded_in_hops_and_bytes	fab-via	
src/fs_restricted.rs	ensure_private_dir_creates_0700_tightens_and_is_idempotent	fs-ensure-private-dir	
src/fs_restricted.rs	an_already_private_dir_keeps_its_ctime_and_a_drifted_one_is_hardened	fs-ensure-private-dir	
src/input.rs	bit_layout_matches_keyboard_modifiers	input-modifier-bridge	
src/mutex_ext.rs	test_lock_or_recover_after_poison	mutex-lock-or-recover	
src/rfc3339.rs	format_rfc3339_matches_known_instants	rfc3339-format-denotation	
src/rfc3339.rs	parse_utc_inverts_format_and_refuses_looser_shapes	rfc3339-parse-strict	rfc3339-parse-fractional
src/rfc3339.rs	parse_utc_fractional_truncates_a_fraction_and_nothing_else	rfc3339-parse-fractional	
src/rfc3339.rs	days_from_civil_inverts_civil_from_days	civil-days-roundtrip	
src/rust_lane_tests.rs	the_interim_guards_thirty_seven_cases_hold	rl-command-position-verdict	
src/rust_lane_tests.rs	a_trust_toolchains_own_rustc_by_path_is_not_stock_and_its_cargo_names_its_targo	rl-command-position-verdict	rl-refusal-shape
src/rust_lane_tests.rs	file_beside_resolves_a_directory_the_way_the_shell_wrote_it	rl-file-beside	
src/rust_lane_tests.rs	the_other_places_a_shell_runs_a_command_are_read	rl-command-position-verdict	
src/rust_lane_tests.rs	asking_about_a_tool_and_naming_it_as_data_are_not_running_it	rl-command-position-verdict	
src/rust_lane_tests.rs	the_escape_hatch_clears_exactly_one_simple_command	rl-escape-hatch	
src/rust_lane_tests.rs	an_escape_is_reported_with_its_reason_and_never_hides_a_second_command	rl-escape-hatch	
src/rust_lane_tests.rs	the_refusal_is_bounded_and_one_line_per_command	rl-refusal-shape	
src/rust_lane_tests.rs	a_hit_names_what_was_used_and_the_trust_command_with_its_arguments	rl-hit-fields	
src/rust_lane_tests.rs	every_cargo_verb_gets_the_lane_targo_accepts_for_it	rl-trust-spelling	
src/rust_lane_tests.rs	a_pin_is_read_the_way_rustup_reads_it	rl-channel-in	
src/rust_lane_tests.rs	nesting_past_the_cap_is_refused_and_malformed_input_is_bounded	rl-depth-cap-total	
src/rust_lane_tests.rs	a_trust_channels_own_rustc_and_rustdoc_are_not_stock	rl-command-position-verdict	
src/rust_lane_tests.rs	the_reviewers_thirteen_missed_shapes_are_read_and_the_allow_probes_still_pass	rl-command-position-verdict	
src/rust_lane_tests.rs	a_parameter_expansion_a_case_pattern_and_a_dashed_heredoc_delimiter_are_read_as_the_shell_reads_them	rl-command-position-verdict	
src/scheme.rs	user_theme_name_grammar_rejects_paths_splits_and_normalization_traps	scheme-theme-name-grammar	
src/scheme.rs	draw_builtin_follows_name_for	selector-draw	
src/scheme.rs	default_scheme_matches_palette_defaults	scheme-projections	
src/scheme.rs	builtin_lookup	scheme-builtin-registry	
src/scheme.rs	light_builtins_are_light_and_selection_legible	scheme-builtin-legibility	
src/scheme.rs	parse_scheme_str_full_theme	scheme-parse	
src/scheme.rs	parse_scheme_str_minimal_defaults_unset_slots	scheme-parse	
src/scheme.rs	parse_scheme_str_rejects_malformed	scheme-parse	
src/scheme.rs	palette_generate_key_parses	scheme-palette-generate-key	scheme-parse
src/scheme_import.rs	ghostty_import_carries_palette_generate	scheme-palette-generate-key	
src/scheme_palette256.rs	generated_corners_are_the_theme_anchors	scheme-generated-palette	scheme-projections
src/scheme_palette256.rs	grey_ramp_follows_black_to_white	scheme-generated-palette	
src/scheme_palette256.rs	generated_palette_is_deterministic	scheme-generated-palette	scheme-projections
src/selection.rs	both_halves_of_a_wide_glyph_name_the_same_pair_of_columns	sel-glyph-span	
src/selection.rs	containment_matches_the_normalized_shapes	sel-linear	sel-block
src/shell_blocks_tests.rs	block_is_complete_requires_complete_state	shell-block-predicates	
src/terminal_core.rs	kitty_graphics_advertised_after_kitty_core	caps-feature-list	
src/terminal_core.rs	soft_fonts_not_advertised	caps-feature-list	
src/text_shaping.rs	parse_font_features_short_tag_padded	font-feature-token	
src/text_shaping.rs	parse_font_features_skips_malformed	font-feature-token	font-features-list
src/text_shaping.rs	parse_never_panics_and_is_well_formed	font-feature-token	font-features-list
src/text_shaping.rs	affixes_and_explicit_values	font-feature-token	
src/time.rs	duration_to_nanos_cases	duration-to-nanos	
src/trust_fmt.rs	push_hex4_matches_format	push-hex4	
src/version.rs	install_first_wins	build-stamp-first-install-wins	
src/vt_level.rs	decscl_param_roundtrip	vt-decscl-roundtrip	
src/x11_colors.rs	table_is_sorted	x11-table-wellformed	
src/x11_colors.rs	lookup_case_insensitive	x11-lookup	
src/x11_colors.rs	lookup_grey_variants	x11-table-wellformed	x11-lookup
src/x11_colors.rs	lookup_unknown_returns_none	x11-lookup	
src/xterm_keyboard.rs	test_set_modify_other_keys	xtk-state-machine	
src/xterm_keyboard.rs	test_reset_modify_other_keys	xtk-state-machine	
src/xterm_keyboard.rs	test_disable_modify_other_keys	xtk-state-machine	
src/xterm_keyboard.rs	test_format_other_keys	xtk-state-machine	
src/xterm_keyboard.rs	test_query_responses	xtk-query-responses	
src/xterm_keyboard.rs	test_reset_all	xtk-state-machine	
src/mouse/encode.rs	a_thumb_buttons_legacy_release_is_button_three	mouse-dispatch	
src/mouse/encode.rs	utf8_large_button_code	mouse-utf8	
src/mouse/encode.rs	encode_mouse_dispatches_sgr_pixel	mouse-dispatch	
src/mouse/types.rs	button_slots_are_dense_and_distinct	mouse-button-slot-injective	
src/mouse/types.rs	only_vertical_wheel_directions_carry_a_viewport_sense	wheel-vertical-sense	
src/keyboard/dom_map.rs	modifier_keys_encode_only_under_report_all	kb-dom-encode	
src/keyboard/dom_map.rs	encode_dom_key_honours_the_explicit_mode	kb-dom-encode	
src/keyboard/dom_map.rs	encode_dom_key_text_release_requires_report_all	kb-dom-encode	kb-release-gating
src/keyboard/dom_map.rs	a_dom_ctrl_chord_without_a_control_byte_takes_the_physical_keys	kb-dom-encode	kb-layout-text
src/keyboard/encode_tests.rs	shifted_character_refines_independent_spec	kb-shift-map	
src/keyboard/encode_tests.rs	legacy_shift_changes_every_shiftable_key	kb-shift-map	kb-legacy-character
src/keyboard/encode_tests.rs	write_u32_renders_every_magnitude	kb-write-u32	
src/keyboard/encode_tests.rs	shift_enter_distinct_from_enter_in_every_mode	kb-shift-enter-distinct	
src/keyboard/encode_tests.rs	release_encodes_to_nothing_without_report_event_types	kb-release-gating	
src/keyboard/encode_tests.rs	text_and_recovery_key_releases_require_report_all_keys	kb-release-gating	
src/keyboard/encode_tests.rs	reported_release_is_never_byte_identical_to_press	kb-release-gating	
src/keyboard/encode_tests.rs	ungated_release_would_collide_with_a_press_report	kb-release-gating	
src/keyboard/encode_tests.rs	kitty_text_events_encode_exactly_like_legacy	kb-kitty-text-preserving	
src/keyboard/encode_tests.rs	a_modifier_release_carries_its_bit_only_as_the_caller_states_it	kb-release-gating	
src/keyboard/encode_tests.rs	modifier_keys_silent_in_every_mode_without_report_all	kb-release-gating	
src/keyboard/encode_tests.rs	functional_form_witnesses	kb-kitty-csi-u-denotation	
src/keyboard/encode_tests.rs	kitty_flags_suppress_modify_other_keys	kb-xterm-mok	
src/keyboard/encode_tests.rs	codex_progressive_keyboard_mode_witnesses	kb-kitty-text-preserving	
src/keyboard/encode_tests.rs	shift_enter_concrete_witnesses	kb-shift-enter-distinct	
src/keyboard/encode_tests.rs	main_block_twin_table	kb-keypad-fold	
src/keyboard/encode_tests.rs	kitty_legacy_folds_keypad_digits_to_their_glyph	kb-keypad-fold	
src/keyboard/encode_tests.rs	kitty_legacy_fold_yields_to_application_keypad_mode	kb-keypad-fold	
src/keyboard/encode_tests.rs	kitty_associated_text_carries_the_keypad_glyph	kb-keypad-fold	
src/keyboard/encode_tests.rs	kitty_associated_text_carries_the_spacebars_space	kb-kitty-progressive	
src/keyboard/encode_tests.rs	a_modified_keypad_key_never_composes_the_main_rows_glyph	kb-keypad-fold	
src/keyboard/encode_tests.rs	win32_input_is_not_a_kitty_flag	kb-mode-bit-layout	
src/keyboard/encode_tests.rs	win32_enter_chords_are_record_pairs_and_plain_enter_stays_legacy	kb-win32-records	
src/keyboard/encode_tests.rs	win32_release_emits_nothing_and_repeat_emits_the_pair	kb-release-gating	kb-win32-records
src/keyboard/encode_tests.rs	win32_outranks_a_negotiated_kitty_mode	kb-win32-records	
src/keyboard/encode_tests.rs	win32_outranks_xterm_modify_other_keys	kb-win32-records	
src/keyboard/encode_tests.rs	win32_keys_conhost_still_reads_keep_their_bytes	kb-win32-records	
src/keyboard/encode_tests.rs	conhost_holds_exactly_the_measured_forms	kb-win32-records	
src/keyboard/encode_tests.rs	win32_escape_is_a_record_pair	kb-win32-records	
src/keyboard/encode_tests.rs	win32_ctrl_bracket_and_ctrl_3_are_records_of_their_own_key	kb-win32-records	
src/keyboard/encode_tests.rs	win32_alt_chords_that_open_a_sequence_are_records	kb-win32-records	
src/keyboard/encode_tests.rs	win32_escape_records_outrank_kitty_and_modify_other_keys	kb-win32-records	
src/keyboard/encode_tests.rs	win32_changes_only_the_held_forms_across_the_whole_ascii_keyboard	kb-win32-records	
src/keyboard/encode_tests.rs	win32_numpad_enter_chord_is_an_enhanced_key_record	kb-win32-records	
src/keyboard/encode_tests.rs	win32_ctrl_h_is_a_record_of_the_h_key	kb-win32-records	
src/keyboard/encode_tests.rs	win32_ctrl_h_record_outranks_kitty_and_modify_other_keys	kb-win32-records	
src/keyboard/kitty_progressive_tests.rs	report_all_keys_encodings	kb-kitty-progressive	
src/keyboard/kitty_progressive_tests.rs	kitty_report_all_keys_keeps_legacy_functional_forms	kb-kitty-progressive	
src/keyboard/kitty_progressive_tests.rs	report_alternate_keys_encodings	kb-kitty-progressive	
src/keyboard/kitty_progressive_tests.rs	report_associated_text_encodings	kb-kitty-progressive	
src/keyboard/kitty_progressive_tests.rs	report_associated_text_omits_payload_for_named_key	kb-kitty-progressive	
src/keyboard/kitty_progressive_tests.rs	modifier_key_encodings	kb-kitty-progressive	
src/keyboard/layout_text_tests.rs	caps_lock_types_the_capital_when_no_layout_says	kb-layout-text	
src/keyboard/layout_text_tests.rs	typed_character_is_the_us_derivation	kb-layout-text	
src/keyboard/layout_text_tests.rs	a_text_press_writes_the_layouts_character	kb-layout-text	
src/keyboard/layout_text_tests.rs	kitty_reports_the_layouts_character_beside_the_unshifted_key_code	kb-layout-text	
src/keyboard/layout_text_tests.rs	xterm_modify_other_keys_reads_the_layouts_shifted_keysym	kb-xterm-mok	kb-layout-text
src/keyboard/layout_text_tests.rs	a_chord_writes_its_control_or_escape_form	kb-layout-text	
src/keyboard/layout_text_tests.rs	ctrl_on_a_non_latin_layout_sends_the_us_letters_control_byte	kb-layout-text	
src/keyboard/layout_text_tests.rs	a_chord_on_a_non_us_layout_takes_the_layouts_character	kb-layout-text	kb-xterm-mok
src/keyboard/tests.rs	keyboard_mode_xterm_accessors	kb-mode-accessors	
src/keyboard/tests.rs	term_mode_from_keyboard_state_all_kitty	kb-termmode-projection	
src/keyboard/tests.rs	term_mode_from_keyboard_state_xterm_levels	kb-termmode-projection	
src/keyboard/tests.rs	term_mode_to_keyboard_mode_roundtrip_mixed	kb-termmode-projection	
src/keyboard/tests.rs	term_mode_to_keyboard_mode_ignores_non_keyboard_flags	kb-termmode-projection	
src/keyboard/tests.rs	modifier_wire_values	kb-wire-values	
src/keyboard/tests.rs	kitty_disambiguate_encodings	kb-kitty-csi-u-denotation	
src/keyboard/tests.rs	kitty_event_type_encodings	kb-kitty-csi-u-denotation	
src/keyboard/tests.rs	xterm_modify_other_keys_encodings	kb-xterm-mok	
src/keyboard/tests.rs	xterm_modify_other_keys_level1_preserves_backspace_legacy_forms	kb-xterm-mok	
src/keyboard/tests.rs	xterm_modify_other_keys_level1_reports_only_ambiguous_ctrl_shift_chars	kb-xterm-mok	
src/keyboard/tests.rs	xterm_modify_other_keys_level2_preserves_unambiguous_shifted_text	kb-xterm-mok	
src/keyboard/tests.rs	xterm_modify_other_keys_level2_ctrl_backspace_uses_legacy_toggle	kb-xterm-mok	
src/keyboard/tests.rs	xterm_modify_other_keys_does_not_capture_keypad_keys	kb-xterm-mok	
src/keyboard/tests.rs	disambiguate_plain_keys_are_not_over_escaped	kb-kitty-text-preserving	
src/keyboard/tests.rs	legacy_lock_bits_do_not_corrupt_navigation_keys	kb-legacy-named	
src/keyboard/tests_legacy.rs	legacy_character_encodings	kb-legacy-character	
src/keyboard/tests_legacy.rs	legacy_encode_ctrl_underscore_is_us_for_emacs_undo	kb-legacy-character	
src/keyboard/tests_legacy.rs	legacy_encode_shift_symbol	kb-legacy-character	
src/keyboard/tests_legacy.rs	legacy_control_and_editing_key_encodings	kb-legacy-named	
src/keyboard/tests_legacy.rs	legacy_cursor_and_function_key_encodings	kb-legacy-named	
src/keyboard/tests_legacy.rs	legacy_encode_shift_f10_is_terminfo_kf22	kb-legacy-named	
src/keyboard/tests_legacy.rs	legacy_numpad_encodings	kb-legacy-named	
src/keyboard/tests_legacy.rs	legacy_numpad_enter_outside_app_keypad_is_the_main_enter	kb-legacy-named	
src/keyboard/tests_legacy.rs	legacy_emacs_bindings	kb-legacy-character	
src/keyboard/tests_legacy.rs	legacy_meta_word_movement	kb-legacy-character	
src/keyboard/tests_legacy.rs	test_vt52_cursor_keys	kb-vt52	
src/keyboard/tests_legacy.rs	test_vt52_mode_overrides_decckm	kb-vt52	
src/keyboard/tests_legacy.rs	test_vt52_cursor_keys_ignore_modifiers	kb-vt52	
src/keyboard/tests_legacy.rs	vt52_function_keys_and_home_end	kb-vt52	
src/keyboard/tests_legacy.rs	ctrl_alt_space_and_backspace_keep_the_alt	kb-legacy-character	
src/keyboard/tests_legacy.rs	ctrl_tilde_is_record_separator	kb-legacy-character	
src/keyboard/tests_legacy.rs	test_vt52_numpad_application_mode	kb-vt52	
src/keyboard/tests_legacy.rs	test_vt52_numpad_without_app_keypad_sends_digits	kb-vt52	
src/keyboard/tests_legacy.rs	legacy_backspace_default_sends_del	kb-legacy-named	
src/keyboard/tests_legacy.rs	legacy_backspace_decbkm_sends_bs	kb-legacy-named	
src/keyboard/tests_legacy.rs	legacy_alt_no_esc_suppresses_esc_prefix	kb-legacy-mode-switches	
src/keyboard/tests_legacy.rs	legacy_meta_sends_escape_on_prefixes_esc	kb-legacy-mode-switches	
src/keyboard/tests_legacy.rs	legacy_special_modifiers_off_strips_numlock	kb-legacy-mode-switches	
src/keyboard/tests_legacy.rs	legacy_numpad_begin_is_the_xterm_letter_form_never_a_digit	kb-legacy-named	
src/keyboard/tests_legacy.rs	legacy_numpad_digit_under_app_keypad_ignores_num_lock	kb-legacy-named	
tests/color_space.rs	contrast_wcag_aa_boundary	gray-aa-threshold	
tests/color_space.rs	luminance_monotonic_for_grays	luminance-monotone	
tests/kitty_keyboard_tests.rs	test_flags_from_bits_masks_invalid	kitty-flags-mask-accessors	
tests/kitty_keyboard_tests.rs	test_flags_individual_bits_no_cross_contamination	kitty-flags-mask-accessors	
tests/kitty_keyboard_tests.rs	test_push_set_flags_pop_restores_pre_push_state	kitty-push-pop-stack-model	
tests/kitty_keyboard_tests.rs	test_push_pop_three_levels	kitty-push-pop-stack-model	
tests/kitty_keyboard_tests.rs	test_pop_multi_count	kitty-push-pop-stack-model	
tests/kitty_keyboard_tests.rs	test_pop_explicit_count_zero_pops_nothing	kitty-push-pop-stack-model	
tests/kitty_keyboard_tests.rs	test_pop_count_exceeds_stack_resets_to_zero	kitty-push-pop-stack-model	
tests/kitty_keyboard_tests.rs	test_pop_from_empty_stack_resets_to_zero	kitty-push-pop-stack-model	
tests/kitty_keyboard_tests.rs	test_stack_overflow_evicts_oldest	kitty-push-pop-stack-model	
tests/kitty_keyboard_tests.rs	test_screen_buffer_stacks_independent	kitty-push-pop-stack-model	
tests/kitty_keyboard_tests.rs	test_set_flags_direct	kitty-flags-apply	
tests/kitty_keyboard_tests.rs	test_reset_clears_everything	kitty-state-new-reset	
tests/kitty_keyboard_tests.rs	test_switch_screen_basic_round_trip	kitty-switch-screen	
tests/kitty_keyboard_tests.rs	test_switch_screen_preserves_alt_flags_across_round_trips	kitty-switch-screen	
tests/kitty_keyboard_tests.rs	test_switch_screen_double_enter_overwrites_saved_main	kitty-switch-screen	
tests/kitty_keyboard_tests.rs	test_switch_screen_after_reset_no_stale_flags	kitty-switch-screen	
tests/kitty_keyboard_tests.rs	test_switch_screen_interleaved_with_push_pop	kitty-switch-screen	
tests/kitty_keyboard_tests.rs	test_snapshot_roundtrip_restores_full_state	kitty-snapshot-roundtrip	
src/xterm_keyboard.rs	test_default_vs_new_divergence	xtk-state-machine	xtk-query-responses
```

## 3. Coverage

- 398 tests read. 380 map to one of the 125 specs. 18 are not spec samples (§4).
- The biggest collapses (primary tests): `input-key-bridge` (12), `kb-win32-records` (11), `kitty-push-pop-stack-model` (11), `kb-legacy-named` (10), `palette-mutators` (8), `color-spec-hex` (8), `mouse-dispatch` (8), `xtk-state-machine` (8), `kb-dom-map` (8), `kb-xterm-mok` (8).
- 51 specs retire one test or none as primary; they are kept because each states a property no
  other spec states. Small specs that the larger ones compose as lemmas include `kb-mode-accessors`,
  `push-hex4`, `kb-write-u32`, `fab-principal` and `x11-table-wellformed`.
- Gap histogram (specs blocked by each gap; a spec counts once per gap it lists): gap 1: 104, gap 2: 19, gap 3: 93, gap 4: 38, gap 5: 44, gap 6: 6, gap 7: 58, gap 8: 12, gap 10: 1, gap 11: 5, gap 12: 2, gap 13: 9, gap 14: 2. Gap 0 blocks all 125.
- Effects (gap 8) block 12 specs: `cs-symlink-targets-pid`, `dev-seam-reads-env`, `dirs-home-from-env`, `dirs-config-path`, `dirs-state-dir`, `dirs-logs-dir`, `dirs-data-dir`, `env-key-lossy`, `fs-ensure-private-dir`, `mutex-lock-or-recover`, `rl-file-beside`, `build-stamp-first-install-wins`. Per the plan, their pure cores (`fs-dir-safe-predicate`, the `resolve_*` dirs functions) go first.

## 4. Tests that are not spec samples

Status 2026-10-15: all 18 rows below except `src/scheme.rs::builtin_palette_values_are_canonical` are deleted (with the now-empty test module in `src/paragraph_direction.rs`); that one remains.

Status 2026-10-17: `builtin_palette_values_are_canonical` went on 2026-10-16, so all 18 are deleted,
and their rows here and in §2 are gone. They were tautologies over `derive(Default)` or `repr(u8)`
discriminants, restatements of a one-line body or a const's own value, prose-pinning, a wall-clock
sanity check, and a vacuity check on a test-local oracle. None was a regression of a removed bug with no general property: every regression
test found is an instance of a §1 spec.

## 5. Implied specs that look wrong, or disagree with the code

1. **The "exhaustive over the mode space" keyboard tests are not exhaustive.**
   `src/keyboard/encode_tests.rs` sets `KEYBOARD_MODE_DEFINED_BITS = 15` and its comment says bits
   0..=14. But `KeyboardMode` defines 16 bits: `WIN32_INPUT = 1<<15`, which
   `win32_input_is_not_a_kitty_flag` asserts. Every mode with WIN32_INPUT set is skipped by
   `shift_enter_distinct_from_enter_in_every_mode`, `release_encodes_to_nothing_without_report_event_types`,
   `text_and_recovery_key_releases_require_report_all_keys`, `reported_release_is_never_byte_identical_to_press`
   and `modifier_keys_silent_in_every_mode_without_report_all`, though they are labelled an "exhaustive
   PROOF over the whole 2^15 mode space". The §1 specs quantify over `Modes(16)`.
2. **`XtermKeyboardState` has two different defaults.** The derived `Default` gives `(mok: None, fok: 0)`
   (disabled; the query answers `CSI > 4 m`), while `new()` (doc: "Create a new default state") and
   `reset()` give `(Some(0), 0)`. No xterm reset produces `Default::default()`, so any caller using it gets a
   state the protocol never reaches. `src/xterm_keyboard.rs::test_default_vs_new_divergence` pins the split
   as intended. Probably a latent bug; `xtk-state-machine` states the code
   as it is.
3. **serde breaks the type invariants the tests sample.** With the `serde` feature, the derived
   `Deserialize` on `KittyKeyboardFlags` and `KittyKeyboardStateSnapshot` accepts any u8 and skips the
   `KMASK` mask (`tests/kitty_keyboard_tests.rs::test_flags_from_bits_masks_invalid`), and on
   `XtermKeyboardState` it skips the `min(2)`/`min(1)` clamps (`test_set_modify_other_keys`, `test_format_other_keys`). A proved type
   invariant needs a validating `Deserialize` (`try_from`), or the specs must be weakened to values built
   through the constructors.
4. **The UTC parser has a four-digit-year boundary.** `format_rfc3339` is total on u64 and
   emits a 5+ digit year (21+ bytes) for secs ≥ 253402300800 (year 10000), which `parse_utc`
   refuses. The formatter/parser documentation and boundary regression now match the
   round trip through 9999-12-31T23:59:59Z stated in `rfc3339-parse-strict`.
5. **`selection_contains_linear` has an unstated precondition** (start ≤ end in reading order). For a
   reversed anchor pair it returns nonsense (a row in range never holds); callers must normalize.
   `sel-linear` states it, and the Rust fn should carry it as a `requires`.
6. **`OutputBlock::command_rows`/`output_rows` can return an inverted range** when `output_start_row` or
   `end_row` is earlier than the start, and nothing guards that, though the #5715 comment says inverted
   ranges were the bug. `command_and_output_rows_saturate_at_u64_max` pins only saturation. A
   `start ≤ end` ensures needs a struct invariant on `OutputBlock` that is stated nowhere.
7. **A test of an internal helper.** `src/keyboard/encode_tests.rs::ungated_release_would_collide_with_a_press_report`
   calls the private `encode_kitty` and asserts a *collision*, the reason the release gate exists. It is
   kept as lemma (5) of `kb-release-gating`, but it pins internal behaviour, not the public contract.
8. **The luminance tests measure through `contrast`.** Every luminance test in `tests/color_space.rs` reads
   luminance back as `contrast(c, black)·0.05 − 0.05`, which adds its own rounding and leans on contrast's
   formula. The specs are stated on the private `luminance`/`linearize` directly, with an error bound
   against the real formula. Continuity at the 0.04045 seam (the two branches agree at c' = 0.04045
   within eps) is untested.
9. **`scheme-builtin-legibility` may be cheaper as a build check.** It relies on `Rgb::contrast` (f64, WCAG
   luminance with `powf`); a const-evaluated check over `BUILTINS` may beat a float proof.
10. **Dead state.** `KittyKeyboardStackEntry::VALID_BIT` is "set for consistency but never checked" (its
    doc). Dropping it would make the stack abstraction exact (entry = flags).

## 6. Proof order (easiest high-value first)

No spec is blocked by gap 0 alone (§1), so the order starts with the specs blocked only by gap 3, the E6
body widening the parser pilot also needs, and moves outward one gap at a time.

1. **`fs-dir-safe-predicate`** (gap 3). `owner_uid == uid && mode & 0o022 == 0`: two scalar compares and one
   bitwise and. It is the security decision under `ensure_private_dir`, retires 3 tests, and is the
   pure core the plan wants factored out before the effectful `fs-ensure-private-dir` (gap 8).
2. **`sel-linear` and `sel-block`** (gap 3). Scalar `i32`/`usize` comparisons, an if/else-if chain and
   `min`/`max`. They retire 6 tests between them (plus the shared `containment_matches_the_normalized_shapes`)
   and force the missing `requires` on `selection_contains_linear` (§5.5).
3. **`kb-wire-values`, `kb-mode-accessors`, `kb-mode-bit-layout`** (gap 3). u8 arithmetic and `contains`
   on bitflags. They retire 7 tests and are the lemmas every keyboard encoder spec composes (modifier
   parameter, modifyOtherKeys level, the 16-bit mode space of §5.1).
4. **`mouse-button-codes`, `mouse-button-slot-injective`** (gap 3). Enum→u8 matches, decidable by
   evaluation; 3 tests.
5. Then the scalar specs that add only gap 1 for an enum/`Option` result: `cursor-style-from-param`,
   `wheel-vertical-sense`, `sel-glyph-span`, `sel-side-total-order`, `vt-decscl-roundtrip`, `mouse-x10`,
   `duration-to-nanos`, `kb-kitty-code-table`, `kb-termmode-projection`; then `palette-default-table`
   (gap 3, 7: a const loop) and the kitty flag specs (`kitty-flags-mask-accessors`, `kitty-flags-apply`),
   then `&mut self` state machines once gap 2 lands (`xtk-state-machine`, `kitty-state-new-reset`,
   `kitty-switch-screen`, `palette-mutators`, `shell-block-*`).
6. String and table specs after gap 1 (the `cs-*`, `cv-*`, `fab-*`, `app-*`, `x11-*`, `color-spec-*`,
   `font-*`, `rfc3339-*`, `scheme-*` families), then the keyboard encoder (`kb-legacy-*`, `kb-kitty-*`,
   `kb-xterm-mok`, `kb-win32-records`, needing gaps 1, 3, 4, 5, 7), then `rl-command-position-verdict`
   (gap 10, a Clean model of POSIX command positions).
7. Last: the float specs (`luminance-*`, `contrast-*`, `gray-aa-threshold`; gap 6 plus a pow model) and the
   effectful ones (gap 8: `dev-seam-reads-env`, `fs-ensure-private-dir`, `mutex-lock-or-recover`,
   `build-stamp-first-install-wins`, and the shells of the `dirs-*` resolvers).

