<!-- Copyright 2026 Andrew Yates -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# aterm-lab: spec recovery from its 76 unit tests

> Status 2026-10-16: this is the crate's migration worklist. Each of the 69 specs below is
> proved through Trust/Clean about the compiled Rust function, and then the tests it subsumes
> are deleted. A test is deleted only when its primary spec and every spec in its `also` column
> (§2) are Certified. No test has been deleted yet. §7 adds the 22 tests of the Gemini CLI and
> agy lab and its selftests (21 specs); 29 older tests are still unread (§7, last list).
>
> 2026-10-16: `codex-captures` and its one spec (`codex-slash-login-verdict`) went with aterm's Codex
> screen reader, which the captures fed; the counts below are the recovery's.

Source: `crates/aterm-lab` (read only). I read every `#[test]` (codex.rs 5, codex_proof.rs 1,
door.rs 2, footers.rs 4, jail.rs 4, lab.rs 3, nudge.rs 6, paths.rs 3, probe.rs 3, procs.rs 3,
protected.rs 1, pyjson.rs 3, record.rs 1, recorder.rs 1, relay.rs 1, restore.rs 1, run.rs 2,
sampler.rs 1, scn.rs 3, screen.rs 5, stub/tests.rs 9, sys.rs 3, tripwire.rs 2, util.rs 6,
verbs.rs 3 = 76) and the code each one reaches. The recovery manifest said 75; a grep of the 25
files finds 76 `#[test]` functions, and all 76 are mapped in §2. Nothing was compiled or run. The
claims below come from reading the code.

Result: **69 specs**, covering **75 tests**. **1 test is not a spec sample** (§4). 11 specs are
about effects (fs, processes, sandbox profiles, sockets, local time) and wait on gap 8.

## 0. Notation used in the statements

- Every test id is `crates/aterm-lab/src/<file>::<test>`; functions are named `module::fn`.
- `lines(s)` = `str::lines(s)`; `words(s)` = `split_whitespace(s)`; `trim`/`trim_end`/`trim_start`
  as in std; `chars(s)` = the char sequence; `|x|` = length (chars unless marked `_bytes`);
  `take(n, s)` = the first n chars; `++` = concatenation.
- `kv(t)` = `[(k,v) | w ∈ words(t), w = k ++ "=" ++ v at the FIRST '=']` in order (`screen::kv`).
  `field(t,k)` = the first `v` with `(k,v) ∈ kv(t)`.
- `Sh(s)` = the POSIX shell value of one word `s` (single quotes literal; double quotes with
  `\`-escapes; bare backslash escapes; bash `$'…'` ANSI-C quoting). **Shell model** = a Lean/`ty`
  model of POSIX word expansion with no `$`-expansion except the literal `$PATH`.
- `PyJSON(v, style)` = Python's `json.dumps(v, indent, separators, ensure_ascii)` as a reference
  model; `PyRepr(s)` = Python `repr(str)`; `PyFloatRepr(f)` = Python `repr(float)`.
- `J` = `crate::pyjson::J` (`Null|Bool|Int|Float|Str|Arr|Obj`, `Obj` an ordered association list).
  `J::parse` reaches `aterm_json` (cross-crate).
- `RFC7396(t, p)` = JSON Merge Patch.
- `Prot(home, cc)` = `protected::protected_paths(home, cc)`, a list of
  `(Kind ∈ {Prefix, Subpath, Literal}, path)`. `R(x)` = `fs::canonicalize(x)`.
- **Fs model** = an abstract file-system state (inodes, link kinds, nlink, uid, mode, mtime), as
  gap 8 needs. **Process model** = a kernel process table with exit events and reaping.
- Gap numbers are the rows of `docs/AUDIT-trust-for-test-migration-2026-10-15.md` §7:
  1 non-scalar carriers (`String`/`&str`/`Vec`/`Option`/enums/structs), 2 `&self`/`&mut` views,
  3 E6 body widening (calls, matches, casts), 5 quantified clauses, 6 plain-Rust clause
  publication, 7 loops/iterators, 8 effects (fs, process, FFI, unsafe), 10 model refinement,
  11 generics, 12 cross-crate citation (`aterm_regex`, `aterm_phase`, `aterm_json`, `aterm_http`,
  `aterm_uds`), 13 threads/closures/higher-order, 14 Clean elaborator. **Gap 0 (the seal) blocks
  every row and is not repeated in the column.** Gap 14 applies to every Lean-written reference
  model (Shell, PyJSON, PyRepr, civil calendar) and is likewise not repeated.

## 1. Specs (TSV)

Columns: `id`, `kind`, `functions`, `statement`, `subsumes (primary tests; secondary in §2)`,
`difficulty (Rust features the proof must handle)`, `blocking_gaps (audit §7; 0 implied)`.

```tsv
id	kind	functions	statement	subsumes	difficulty	blocking_gaps
codex-safe-args	contract	codex::safe_args	∀ tab: bool, args: [String]: safe_args(tab,args) = Ok(()) ⇔ ∀ a ∈ args: a ∉ {logout, app, app-server, remote-control, agents, exec-server, cloud, update} ∧ (a = "login" ⇒ ¬tab ∧ join(args," ") ∈ {"login status","login --help","login -h"}). On Err the message names the first offending argument in order (the login message for "login", the quoted word otherwise).	codex.rs::a_login_or_a_server_never_runs_in_the_lab	&[String], join, for loop with early return, match on &str, Result<(),String>, format!	1,3,5,7
codex-config-text	contract	codex::config_text, codex::q	∀ mode, port, proj, proj_spelled, model: let T = config_text(…). (a) T ends in '\n' and its first line is "# aterm-lab codex (crates/aterm-lab), mode " ++ mode ++ ": …". (b) Read as TOML (q is a correct TOML basic-string quoter for every input): model_provider = "lab" ⇔ mode ≠ "signed-out"; model = model ⇔ model ≠ ""; chatgpt_base_url = "http://127.0.0.1:"++port++"/backend-api/" and [features].apps = false ⇔ mode = "chatgpt"; [model_providers.lab] exists ⇔ mode ≠ "signed-out", with base_url = "http://127.0.0.1:"++port++"/v1", wire_api = "responses", and requires_openai_auth = true if mode = "chatgpt" else env_key = "LAB_CODEX_KEY"; analytics/feedback/otel/tui disabled. (c) The [projects."…"] keys = {proj, proj_spelled} (one table when equal), each trust_level = "trusted".	codex.rs::the_config_names_its_login_and_trusts_the_project_both_ways	String building, Vec<String> extend/sort/dedup, str::replace, format!, a TOML reading model	1,3,5,7,10
codex-auth-json-lab-jwt	round-trip	codex::auth_json, stub::lab_claims, stub::auth_label, util::b64url, util::b64url_decode, util::utc_stamp, pyjson::J::dump, pyjson::J::parse	∀ label, account, plan, now: i64 (now + 30·86400 not overflowing): j = J::parse(auth_json(label,account,plan,now)) is Ok; tokens.refresh_token = "lab-refresh-" ++ label; last_refresh = utc_stamp(now); access = tokens.access_token has the shape H.P."bGFiLW5vdC1zaWduZWQ"; lab_claims("Bearer " ++ access) = Some(c) with c.lab = label, c.iat = now, c.exp = now+2592000, c["https://api.openai.com/auth"].chatgpt_plan_type = plan and .chatgpt_account_id = account; auth_label(Some("Bearer " ++ access)) = "jwt:" ++ take(24, filter([A-Za-z0-9_-], label)). The same holds for id_token, which also carries email = "user@example.com". Composes stub-auth-label, util-b64url-roundtrip, util-utc-stamp, pyjson-dumps-refines-python.	codex.rs::the_composed_login_is_a_jwt_the_stub_reads_by_its_label_only	closures, J tree construction, JSON print then parse (cross-crate aterm_json), base64url, split, Option chaining	1,3,5,7,12,13
stub-auth-label	contract	stub::auth_label, stub::lab_claims	∀ a: Option<&str>: let v = a.unwrap_or(""), tok = trim(v[7..]) if v[..7] case-insensitively = "bearer " else trim(v). auth_label(a) = "none" if v = ""; else "key" if tok = LAB_CODEX_KEY; else "jwt:" ++ take(24, filter([A-Za-z0-9_-], c.lab)) if lab_claims(v) = Some(c); else "other(" ++ |chars(tok)| ++ " chars)". The output never contains tok unless tok is the fixed LAB_CODEX_KEY (a credential is never echoed). lab_claims(v) = Some(j) ⇔ tok splits on '.' into exactly 3 parts and the middle b64url-decodes to a JSON object j with a string field lab.	(secondary only; see §2)	Option<&str>, str::get char-boundary slicing, eq_ignore_ascii_case, filter/take/collect, JSON parse	1,3,5,7,12
codex-ratelimit-headers	contract	codex::ratelimit_headers, pyjson::py_float	∀ args: (a) args = [] ∨ args[0] = "off" ⇒ Ok(Obj[]). (b) else |args| < 2 ⇒ Err. (c) else for (slot,used,window,reset) ∈ [(primary,args[0],300,3600),(secondary,args[1],10080,432000)]: used = "-" adds nothing; used parses as f64 v ⇒ adds x-codex-<slot>-used-percent = py_float(v), -window-minutes = window, -reset-at = "@NOW+"++reset++"@" in that order; otherwise Err. Then each args[i≥2] = k=v adds headers by k ∈ {credits (has-credits = v∈{yes,unlimited}, unlimited = v=unlimited), balance, reached, promo, active}; any other k ⇒ Err. Header order = argument order.	codex.rs::ratelimits_name_each_window_and_the_credits	&[String], f64 parse, Vec<(String,J)>, for over an array literal, match on &str, Result	1,3,5,7
launcher-argv	refinement	codex::launcher, lab::launcher, util::shell_quote	∀ run, tab, mode, envs, args (codex) and ∀ run, l: Launch (claude), provided the fields interpolated WITHOUT shell_quote (run.run/shim, run.proxy, run.port) hold no shell metacharacter, quote, '$' or whitespace: the script is "#!/bin/sh\n" ++ ("clear\n" iff tab; always for claude) ++ one exec command whose word expansion under the Shell model is exactly the argv. codex: [env, -u, OPENAI_API_KEY, -u, CODEX_API_KEY, -u, OPENAI_BASE_URL, PATH=<run>/shim:$PATH, HOME=<w>/home, CODEX_HOME=<w>/codex-home, the six proxy vars = proxy, NO_PROXY/no_proxy, LAB_SECURITY_LOG, LAB_OPEN_LOG, BROWSER, the two token-URL overrides, LAB_CODEX_KEY=lab-not-a-key iff mode = "lab"] ++ envs ++ [codex_bin, --no-daemon] ++ args. claude: ([<run>/relay/node, --log, <w>/relay.log, --] iff l.relay) ++ [env, PATH=…, CLAUDE_CONFIG_DIR=<w>/home/.claude, ANTHROPIC_BASE_URL=http://127.0.0.1:<port>, proxies, NO_PROXY…, DISABLE_AUTOUPDATER=1, LAB_SECURITY_LOG=<w>/security.log, LAB_OPEN_LOG=<w>/open.log, BROWSER=<run>/shim/open] ++ l.envs ++ [CLAUDE_CODE_NO_FLICKER=1 iff relay] ++ [CLAUDE_CODE_OAUTH_TOKEN=<fake> iff subscriber] ++ [claude_bin] ++ l.args. So the executed program is the binary by path and every user word survives as one argv element; and for both, LAB_OPEN_LOG and BROWSER are env's assignments (before the program), so every open the agent makes (`$BROWSER <url>`, or a bare-name `open` that the shim leading PATH answers) is the recorder's and goes to <w>/open.log. NEGATIVE CONTROL (claude): a line without LAB_OPEN_LOG or BROWSER, or with LAB_OPEN_LOG as an argument after the program, fails it. Uses util-shell-quote-unquote-roundtrip.	codex.rs::the_launcher_execs_codex_by_path_with_no_daemon, lab.rs::claude_options_parse_and_the_launcher_execs_the_binary_by_path	format! string building, PathBuf join/display/to_string_lossy, for over &[String], a Shell model	1,3,5,7,10
lab-parse-launch	contract	lab::parse_launch	∀ args: parse_launch(args) = Ok(l) ⇔ args matches ("-e" V | "--no-wait" | "--relay" | "--subscriber")* ["--" R*]; then l.envs = the V's in order, l.no_wait/relay/subscriber = whether each flag occurs, l.args = R* (everything after the first "--", verbatim, including further flags). Err when a token outside the grammar appears before "--" or "-e" is last.	(secondary only; see §2)	slice iterator with by_ref, match on &str, Vec push/extend, Result<_, String>	1,3,7
lexical-validators	contract	door::lab_socket_shape, paths::valid_tag	∀ s: lab_socket_shape(s) ⇔ ∃ n: s = "/tmp/at." ++ n ++ "/a.sock" ∧ n ≠ "" ∧ '/' ∉ n. ∀ t: valid_tag(t) ⇔ t ≠ "" ∧ |t|_bytes ≤ 24 ∧ ∀ c ∈ t: c ∈ [A-Za-z0-9_-].	door.rs::only_a_lab_shaped_socket_passes, paths.rs::tags_are_short_plain_words	strip_prefix/strip_suffix, Option::is_some_and, chars().all	1,5,7
door-wire-pattern	contract	door::wire_pattern	∀ p: wire_pattern(p) = concatMap(f, chars(p)) with f(c) = "\\x" ++ hex2(c) if c is ASCII whitespace or '=', else c. So the output holds no ASCII whitespace and no '=', and |out| = |p| + 3·#{escaped chars}. Semantic clause (restricted, §5.9): for every p in which no escaped char follows an odd run of backslashes, Regex(wire_pattern(p)) and Regex(p) denote the same language.	door.rs::a_pattern_crosses_the_wire_as_one_token	chars loop, String push/push_str, format!("{:02x}"), char as u32; regex semantics for the restricted clause	1,5,7,12
footers-mode-word	contract	footers::mode_word, aterm_phase::anchor	∀ screen: let row = the LAST line of screen containing "⏵⏵ " or "⏸ ". mode_word(screen) = None if no such row; else the word w of the FIRST entry (glyph, id, w) of the fixed table [bypass, auto, accept-edits, dont-ask (⏵⏵); plan, manual (⏸)] such that row contains glyph ++ " " ++ anchor(id); None if no entry matches.	footers.rs::the_mode_row_names_its_mode	lines().rev().find, array-literal iterator find, format!, &'static str result, cross-crate anchor table	1,5,7,12
footers-footer-row-pattern	contract	footers::footer_row_pattern, aterm_phase::anchors::guard_regex	∀ screen: footer_row_pattern(screen) = None ⇔ every line of screen is blank after trim; else Some(guard_regex(r)) with r = trim of the last non-blank line, and Regex(guard_regex(r)) matches r (guard_regex is a literal pattern: metacharacters escaped, whitespace as '.').	footers.rs::the_footer_row_is_the_last_row_with_text_as_a_literal_pattern	lines().rev().map(trim).find, Option::map, cross-crate regex builder	1,5,7,12
footers-done-row-pattern	equivalence	footers::DONE_ROW, screen::is_done_row, aterm_regex::Regex::is_match	(restricted, §5.1) ∀ r = "✻ " ++ v ++ " for " ++ s with v a non-empty word with no whitespace: Regex(DONE_ROW).is_match(r) ⇔ is_done_row(r) ⇔ s starts with an ASCII digit. is_done_row itself is specified in screen-idle-status. The unrestricted equivalence is FALSE.	footers.rs::the_done_row_pattern_reads_what_the_done_row_reader_reads	strip_prefix, match_indices, char-boundary slicing, a regex semantics model	1,5,7,12
footers-opts-parse	contract	footers::Opts::parse, footers::Opts::variant	∀ args: Opts::parse(args) = Ok(o) ⇔ args matches ("--out" V | "--no-turn" | "--no-bypass" | "--columns" V | "--lines" V | "--subscriber" | "--settings" V)* with every --settings V parsing as a JSON object; o = the defaults (turn, bypass true; cols 80; lines 24; subscriber false; settings None) overridden by the flags in order (last wins). Every Err has code 2 (usage). variant(o) = "settings " ++ dump(settings, COMPACT) if settings is Some, else "the lab's default settings".	footers.rs::the_variant_is_parsed_and_named	slice iterator with a capturing FnMut closure, match on &str, JSON parse, Result<_, Fail>, PathBuf (from_caller reads the cwd: an effect)	1,3,7,8,12,13
jail-render-rules	contract	jail::render, protected::protected_paths, paths::physical	∀ spec whose run/sockdir/state/user_tmp exist, whose quoted paths are profile_safe, whose mark is valid and whose R(run) is not inside_protected: render(spec) = Ok(p) where (a) the "(deny file-read* file-write* (" lines are exactly one per entry of Prot(home, ccsocks), in order; (b) the "(deny mach-lookup" lines are exactly MACH_DENY ++ MACH_DENY_OPEN ++ [mark]; (c) no '@' template hole survives; (d) file-write* is allowed under R(run)/scratch with a literal rule per SCRATCH_DIRS entry, and xcrun_db and unlink of R(user_tmp) are denied; (e) one trusted-write deny rule lists R(state), every lab_dir subpath, the aterm literal, the repo_target literal and physical(target)/debug and /release subpaths; (f) the unlink-deny parents rule lists every proper ancestor of those trusted paths; (g) for each agent binary b = S/name/build/… under S = home/Library/Application Support/aterm/pkg/store: file-read* is allowed on S/name/build and file-read-metadata on each ancestor from S/name up to home/Library/Application Support/aterm, none above; an agent outside the store gets no read rule; (h) a home under /private/tmp/(at|labprobe).<x>/ gets a file-write* allow on its own subpath.	jail.rs::every_protected_entry_and_mach_service_becomes_one_rule, jail.rs::the_agents_under_test_are_readable_inside_the_store_they_live_in, jail.rs::a_replica_home_in_tmp_is_left_to_its_own_rules	large format! template, Vec<String> joins, closures, Path parent walks, fs::canonicalize (effect)	1,3,5,7,8,13
jail-render-refusals	contract	jail::render, jail::valid_mark, jail::profile_safe, protected::inside_protected	∀ spec: render(spec) = Err(_) if any of run, sockdir, state, user_tmp does not canonicalize; or any quoted path (home, run, claude, sockdir, ccsocks, codex, state, aterm, repo_target and its physical forms, user_tmp, lab_dirs) is not profile_safe (holds '"', '\\' or a control char); or the mark is not valid (message holds "bad mark"); or inside_protected(R(run), home, ccsocks) (message holds "owner's data"). ∀ m: valid_mark(m) ⇔ m = "lab.jail." ++ r ∧ r ≠ "" ∧ ∀ c ∈ m: c ∈ [A-Za-z0-9._-].	jail.rs::a_path_the_profile_cannot_quote_a_bad_mark_or_a_run_in_the_owners_data_is_refused	Result early returns, Vec<&str> any, chars().all, fs::canonicalize (effect), random new_mark when empty (effect)	1,3,5,7,8
lab-managed-bin	contract	lab::managed_claude, lab::managed_bin	∀ name, line: managed_bin(name,line) = Some(last) ⇔ line = name ++ " → " ++ chain ++ " — managed " ++ tail (first occurrence of " — managed "), last = the final " → "-separated segment of chain, and last ≠ ""; else None. managed_claude = managed_bin("claude", ·).	lab.rs::the_managed_claude_is_the_last_path_of_the_arrow_chain	strip_prefix with format!, split_once, rsplit, bool::then	1,3,5
lab-mask-account	contract	lab::mask_account	∀ line: mask_account(line) = the left-to-right rewrite replacing each account="V" (V up to the next '"') with account="<user>"; an occurrence with no closing quote is replaced and the rest of the line dropped. Hence no original V survives, mask_account is idempotent, and a line without account=" is unchanged.	lab.rs::the_tripwire_log_is_kept_with_its_account_masked	while-let over str::find, byte-index slicing, push_str	1,5,7
nudge-differing-spans	contract	nudge::differing, nudge::spans	∀ a, b: [String]: differing(a,b) = [i ∈ 0..max(|a|,|b|) ascending | trim_end(a.get(i) or "") ≠ trim_end(b.get(i) or "")]; so differing(a,a) = [] and differing is symmetric. ∀ strictly increasing rows: spans(rows) = join(",", [a if a = b else a ++ "-" ++ b | (a,b) maximal runs of consecutive integers]), and parsing that output back yields rows.	nudge.rs::screens_differ_by_the_rows_whose_text_differs	range filter collect, map_or on slice get, Vec<(usize,usize)> last_mut, join	1,2,5,7
nudge-parse-resizes	contract	nudge::parse_resizes, nudge::hget, screen::kv	∀ stdout, stderr: parse_resizes(stdout, stderr) = (head, epochs) with head = kv(first line of stderr containing "render=") or [] when none; epochs = [Epoch{id, shift = num("shift"), pushed = num("pushed"), fields = kv(rest)} | line ∈ lines(stdout), line = "epoch " ++ id ++ (" " ++ rest)?, id parses as an integer], num(k) = the first value of k in fields parsed, else 0; order kept. hget(h,k) = the first v with (k,v) ∈ h. Uses screen-kv-field.	nudge.rs::resizes_reads_its_header_and_epochs	lines filter_map, split_once, parse::<i64>, closures, Vec of tuples	1,3,5,7,13
nudge-configured-nudge	contract	nudge::configured_nudge	∀ toml: configured_nudge(toml) = Some(lowercase(trim(v))) for the FIRST line l with trim_start(l) = "repaint_nudge" ++ ws* ++ "=" ++ ws* ++ '"' ++ v ++ '"' ++ … (v up to the next quote); None when no line has that shape. Table headers are not consulted (§5.12).	nudge.rs::the_configured_nudge_is_the_render_tables_word	lines find_map, strip_prefix chain, to_lowercase	1,5,7
nudge-judge-verdict	contract	nudge::judge, nudge::differing	∀ expect ∈ {on, off}, m: Trial: judge(expect, &mut m) sets m.shot_rows = max(|differing(shot,before)|, max{n | (_,h,n) ∈ frame_rows, h = rows}), m.end_rows = differing(after,fresh), m.reference_rows = differing(fresh,before), and returns ("INCONCLUSIVE", [why]) with exactly one reason for the FIRST failing gate in the order: error set; |reference_rows| > SLACK; epoch None; epoch class ≠ agent; shift = 0 ∧ pushed = 0; ¬(verdict = desync-risk ∨ by = nudge); shot_rows ≤ SLACK; status_render = "-". Otherwise ("PASS", []) iff every check of the expect-mode list holds (on: healed by=nudge, render ok on both reads, exactly one nudge/hold/healed line, the told sizes; off: the nudge never acted, …), else ("FAIL", bad) with bad = the failed checks in order, non-empty.	nudge.rs::a_healed_trial_passes_on_and_fails_off, nudge.rs::a_trial_that_never_displaced_is_inconclusive	&mut struct with Vec<String> fields, Option<Epoch> clone, closures capturing &mut Vec, iterator chains, format!	1,2,3,5,7,13
nudge-log-line	contract	nudge::nudge_log_line	∀ l: let a = the suffix after the first "aterm_gui::app_nudge: " (or l), a' = a without a leading "repaint nudge ". nudge_log_line(l) = join(" ", [w ∈ split(a', ' ') | ¬∃ p ∈ {session=, reason=, epoch=}: w = p ++ x ∧ x ∈ [A-Za-z0-9_]*]).	nudge.rs::a_nudge_log_line_is_shown_without_its_prefix_and_ids	find + slicing, split/filter/collect/join, closures	1,5,7
paths-forgotten	contract	paths::forgotten	∀ key: forgotten(key) ⇔ key ∈ {AI_AGENT, CLAUDECODE, CURL_HOME} ∨ (key = h ++ "_" ++ t at the first '_' with h ≠ "" ASCII-alphabetic and t ASCII-case-insensitively = "proxy") ∨ ∃ p ∈ FORGET_PREFIXES: key = p ++ r ∧ r ∈ [A-Za-z0-9_]*.	paths.rs::forgets_agent_and_aterm_variables_and_keeps_the_rest	split_once, chars().all, eq_ignore_ascii_case, iteration over a const slice	1,5,7
paths-physical	contract	paths::physical	∀ path, Fs: let h be the longest prefix of path (by components) such that canonicalize(h) succeeds and is a directory. physical(path) = canonicalize(h) joined with the remaining components of path, unchanged; = path itself when no non-empty, non-'.' prefix qualifies. physical creates nothing (Fs unchanged).	paths.rs::physical_resolves_the_existing_head_and_keeps_the_rest	PathBuf pop/push, OsString, loop, fs::canonicalize and is_dir (effects)	1,7,8
probe-op-codec-roundtrip	round-trip	probe::Op::encode, probe::Op::decode, probe::plan	∀ op: Op whose string fields hold no '\t' (and no '\n' for the line channel) and which is not Serve(Some("")): Op::decode(op.encode()) = Some(op). ∀ w, decoy, dirs: every Check c ∈ plan(w, decoy, dirs) has an op meeting that precondition (so every planned operation crosses into the jail unchanged).	probe.rs::every_operation_crosses_into_the_jail_unchanged	enum with String/bool/enum payloads, match, split('\t') into Vec, Option ?, format!	1,3,5,7
probe-replica-plan-coverage	contract	probe::plan, protected::protected_paths	∀ w with w.replica: let P = plan(w, decoy, dirs). #{c ∈ P : c.op ∈ {Reach, Write}, c.label starts with "<replica>"} ≥ 2·|Prot(w.home, w.cc)|; #{c : c.op = Mach(_)} = |MACH_DENY| + |MACH_DENY_OPEN|; for every n ∈ SCRATCH_DIRS a check labelled "move scratch/<n> (the lab writes and reads through it)"; a Denied list check of <replica>/Library/Application Support/aterm; and a metadata-only stat check of every store directory above the Codex build (…/pkg/store/codex).	probe.rs::the_replica_plan_covers_every_protected_path_and_every_mach_service	Vec<Check> building, matches!, format! labels	1,3,5,7
probe-judge-table	table-property	probe::judge	∀ replica, c, inside, control (i = inside or "none"): want = Allowed ⇒ (Ok iff i = "yes", else Fail). want = Denied: i = "yes" ⇒ Fail; i = "EPERM" ⇒ (control ∈ {None, Some("yes")} ⇒ Ok; else replica ⇒ Fail; else Unproven); i = "ENOENT" ∧ c.absent_ok ∧ ¬replica ⇒ Absent; otherwise Fail. The message is empty exactly when the status is Ok or Absent.	probe.rs::a_denial_counts_only_where_the_control_got_through	enum equality, Option<&str>, match with guards, format!	1,3
procs-descendants-in	contract	procs::descendants_in	∀ table: [(pid, ppid)], root: root ≤ 1 ⇒ descendants_in(table, root) = []. Otherwise, for an acyclic table of depth ≤ 64 below root, the output is the post-order walk of root's child forest (children in table order, each child's descendants before it): a permutation of the strict descendants of root in which every pid precedes its parent, with no pid outside the subtree. For depth > 64 the walk truncates (§5.10); for a cyclic table it terminates.	procs.rs::descendants_come_children_first_and_never_for_init	BTreeMap<i32, Vec<i32>>, recursive nested fn with &mut Vec, depth bound as termination measure	1,2,3,5,7,11
procs-reap-liveness	contract	procs::spawn_kept, procs::alive, procs::wait_gone, sys::kill, sys::alive	Under the Process model: for pid = spawn_kept(cmd): alive(pid) ⇔ the child has not exited; once it has, the first alive/wait_gone call reaps it (removes it from the kept table, so no zombie holds the pid: sys::alive(pid) = false). wait_gone(pid, limit) = true iff the process ended within limit (on the kernel exit event, no polling); false leaves a live child kept for a later call. alive(p) = false for p ≤ 1.	procs.rs::an_ended_child_is_reaped_and_reads_dead, procs.rs::a_wait_that_runs_out_leaves_the_child_to_a_later_one	global Mutex<HashMap<i32, Child>>, process spawn, try_wait, kqueue EVFILT_PROC via aterm_uds, poll FFI, signals	1,8,12,13
protected-inside	contract	protected::inside_protected, protected::protected_paths	∀ path, home, cc: inside_protected(path, home, cc) ⇔ ∃ (k, q) ∈ Prot(home, cc): (k = Prefix ∧ path ++ "/" starts with q) ∨ (k ∈ {Subpath, Literal} ∧ path ++ "/" starts with q ++ "/"), i.e. path = q or lies below q, and for a Prefix entry also any name extending q (q.bak).	protected.rs::inside_means_the_path_or_below_it_and_prefix_means_the_name_too	format!, starts_with, any over Vec<(Kind,String)>, enum match	1,3,5,7
pyjson-dumps-refines-python	refinement	pyjson::J::dump, pyjson::J::py, pyjson::J::write, pyjson::write_str	∀ v: J (finite floats), ∀ style ({indent, compact, ascii}, incl. PY and COMPACT): v.dump(style) = PyJSON(v, indent = style.indent, separators = ("," or ", ", ":" or ": ") per compact/indent, ensure_ascii = style.ascii), object keys in insertion order; v.py() = v.dump(PY) = json.dumps(v). With ascii every non-ASCII char is \uXXXX (surrogate pairs above U+FFFF), and empty containers print as {} / [] at any indent. Uses pyjson-py-float.	pyjson.rs::spells_as_python_json_dumps_does	recursive enum traversal with &mut String, Vec<(String,J)>, char escaping, py_float	1,3,5,7,10
pyjson-py-float	refinement	pyjson::py_float	∀ f: f64: py_float(f) = PyFloatRepr(f) for finite f (shortest round-trip digits; ".0" on an integral value; scientific with a sign and at least two exponent digits when |f| < 1e-4 or |f| ≥ 1e16, f ≠ 0); NaN ↦ "NaN", ±∞ ↦ "Infinity"/"-Infinity" (json.dumps spelling). Hence parse(py_float(f)) = f for finite f.	pyjson.rs::floats_read_as_python_repr	f64 classification, std float Display/LowerExp (needs a model of core::fmt shortest float printing), string surgery	1,3,6,10
pyjson-merge-is-merge-patch	contract	pyjson::merge	∀ into: J::Obj, from: J: merge(&mut into, &from) leaves into = RFC7396(into, from) when from is an object (null removes a key, an object merges recursively into an object, any other value, arrays included, replaces), existing keys keeping their position and new keys appended; when from is not an object, into is unchanged (RFC 7396 would replace).	pyjson.rs::merge_removes_on_null_merges_objects_and_replaces_lists	recursive &mut J, ordered assoc-list mutation (remove/set/entry_obj), clone	1,2,3,5,7
record-stream-framing	contract	record::StreamParser::new, record::StreamParser::line, record::StreamParser::t	Fold StreamParser::line over any line sequence L. The outputs are, in order: a line "DELTA … seq=S … screen N …" (N parsed, default 0; S default 0) with N = 0 yields Frame(S, []) at once, and with N > 0 makes the NEXT N lines, verbatim and whatever their content, the rows of Frame(S, rows), emitted on the N-th; a line "EVENT …" outside a frame yields Event(line); "T a b" outside a frame sets t to b when b parses; other lines yield nothing. t() = the last such b.	record.rs::the_stream_yields_frames_with_their_rows_and_events_whole	&mut self state machine with Option<(i64, usize, Vec<String>)>, take(), split_whitespace, parse	1,2,3,5,7
recorder-logged	contract	recorder::logged	∀ arg: if arg contains "://", with scheme = the part before the first "://": scheme ∈ [A-Za-z][A-Za-z0-9+.-]* ⇒ logged(arg) = Some(scheme ++ "://" ++ host) with host = the longest prefix of the rest free of '/', '?', '#' (no path, query or fragment is ever logged); else Some(arg). Otherwise arg starts with '-' ⇒ None; else Some("<a path or app>").	recorder.rs::a_url_is_logged_by_its_scheme_and_host_never_its_query	split_once, take_while/all, Option<String>	1,5,7
relay-parse-args	contract	relay::parse_args	∀ args: parse_args(args) = Some((log, cmd)) ⇔ args = ["--log", p, "--"] ++ cmd with log = Some(p), or args = ["--"] ++ cmd with log = None (when args[0] ≠ "--log"), and cmd ≠ []; cmd is the exact tail slice of args. Otherwise None.	relay.rs::the_relay_takes_an_optional_log_then_dashes_then_a_command	slice patterns with rest bindings, split_first, returned borrowed sub-slice (lifetime)	1,3
relay-ptyrec-take	contract	relay::parse_ptyrec, relay::take_line	∀ args: parse_ptyrec(args) = Some((p, cmd)) ⇔ args = [p, "--"] ++ cmd with cmd ≠ [] (p the exact first element, cmd the exact tail slice); otherwise None. ∀ t: u64, way, bytes: take_line(t, way, bytes) = decimal(t) ++ " " ++ ("O" if way = Out else "I") ++ " " ++ concat(two lower-case hex digits per byte) ++ "\n" — the line aterm-effects' `recording` parses.	relay.rs::a_take_names_its_log_then_dashes_then_a_command_and_writes_ptyrecs_lines	slice patterns with rest bindings, fmt::Write into a String	1,3
restore-agents-of	contract	restore::agents_of, sys::scratch_read	∀ dir, pids, Fs: agents_of(dir, pids) = [(pid, sid) | f ∈ sorted entries of dir with extension json, scratch_read(f) = Ok(Some(b)), J::parse(b) has integer pid and string sessionId, pid ∈ pids]; when dir is reached through a symlink anywhere on its path the result is [] (no record is read). Uses sys-scratch-read.	restore.rs::the_agent_is_the_sessions_record_whose_pid_is_in_the_tree	fs::read_dir, Vec<PathBuf> sort, filter_map with ?, JSON parse, no-follow open (effects)	1,7,8,12
restore-since	contract	restore::since	∀ a, b: i64 (b - a not overflowing): since(a, b) = format("{:.1} s", (b - a) as f64 / 1000), i.e. the elapsed milliseconds as seconds to one decimal (round-half-even on the binary value, as Rust's formatter does).	(secondary only; see §2)	i64 subtraction, int→f64 cast, float formatting with precision	3,6
run-env-sh-roundtrip	round-trip	run::Run::render, run::Run::parse, run::Run::pairs, run::Run::fixture_prefix, util::shell_quote, util::shell_unquote	∀ r: Run whose string and path fields are valid UTF-8 with no '\n', and whose agent ≠ "" (render writes an empty agent as "claude"; §5.2): Run::parse(r.render()) = r. ∀ r: fixture_prefix(r) = "codex-" ++ (codex_ver or "?") if r.agent = "codex", else "claude-" ++ (claude_ver or "?"). Uses util-shell-quote-unquote-roundtrip.	run.rs::env_sh_round_trips_every_field	struct with String/PathBuf/i32 fields, derive(PartialEq), lines/split_once, match on keys, int parse, the shell quoting lemma	1,3,5,7
util-shell-quote-unquote-roundtrip	round-trip	util::shell_quote, util::shell_unquote	∀ s: shell_unquote(shell_quote(s)) = s, and shell_quote(s) is one POSIX word with Sh(shell_quote(s)) = s (bare iff s ≠ "" and every char ∈ [A-Za-z0-9@%+=:,./_-], else single-quoted with ' spelled '\''). ∀ w that is one shell word built from single quotes, double quotes, backslash escapes and $'…' (with \n, \t, \\, \', \" and \xNN escapes): shell_unquote(w) = Sh(w); in particular it reads what printf %q wrote.	util.rs::shell_words_round_trip_through_quote_and_unquote, run.rs::reads_the_env_sh_printf_q_wrote	Peekable<Chars> state machine, nested while-let loops, String::replace	1,5,7,10
sampler-endpoints	contract	sampler::endpoints	∀ lsof -F text f whose lines start with an ASCII tag char: endpoints(f) = the set of strings cmd[pid] proto kind remote ++ ("" if host ∈ {127.0.0.1, ::1, localhost, *, ""} else "  EXTERNAL"), one per 'n' line, where pid/cmd/proto are the latest p/c/P values before it, kind = conn iff the name contains "->" (remote = the part after it) else listen (remote = the name), remote is cut at " (", and host = remote without its trailing :port (port all digits or '*') and without brackets. The ASCII-tag precondition is load-bearing (§5.4).	sampler.rs::loopback_endpoints_are_plain_others_are_external_and_each_names_its_pid	BTreeSet<String>, lines loop with mutable String state, split_at, rfind, replace with a char array	1,3,5,7
scn-put-journal-order	contract	scn::put_journal, scn::dismissed_in, scn::Ctx::put	∀ ctx, jl: after put_journal(&mut ctx, jl), the keys of ctx.sumd end with […, "journal", "dismissed_journal"] in that order (a key already present keeps its position, otherwise it is appended), sumd.journal = jl, and sumd.dismissed_journal = Bool(∃ e ∈ jl: e is an array whose [2] is a string containing "DISMISSED").	scn.rs::the_journal_goes_before_whether_it_holds_a_dismissal	&mut struct holding an ordered J object, any, Option chaining	1,2,3,5,7
scn-cycle-stats-judge	contract	scn::cycle_stats, scn::judge_cycle, screen::is_idle, screen::composer_of	∀ frames (wall-ordered), journal, t_turn: d = cycle_stats(frames, journal, t_turn) has t_idle = wall of the first idle frame with wall ≥ t_turn, t_card = the first card frame with wall ≥ t_turn; if a card: t_zero = the first later card frame whose composer is a non-empty run of '0', t_gone = the first later frame with no card, composer_states = the composer values of frames from the card on with consecutive duplicates collapsed, composer_after = the last frame's composer; t_journal_dismissed = rec_t of the last journal record whose line contains "DISMISSED survey"; each delta (idle_to_card, card_to_zero, zero_to_gone, card_to_gone, gone_to_journal) = round3(y - x) when both stamps are present and non-zero, else Null. judge_cycle(d) = ["no survey card was drawn"] if t_card is falsy; else the list, in order, of: card never went (t_gone falsy), no DISMISSED journaled, composer_after ≠ Str(""). Uses screen-idle-status and screen-composer.	scn.rs::the_cycle_is_measured_from_frames_and_journal_and_judged	slices of structs, find/filter, Vec<Option<String>> dedup, f64 arithmetic and rounding, J object building	1,3,5,7,13
scn-frame-line	contract	scn::frame_line, scn::frame_key, util::py_repr	∀ f: Frame: frame_line(f) = "FRAME seq=" ++ f.seq ++ " T=" ++ (t or "None") ++ " card=" ++ (1 or 0) ++ " composer=" ++ repr_opt(composer) ++ " status=" ++ py_repr(take(50, status or "")) ++ ("" if no rating rows else " rows=[" ++ join(", ", map(py_repr, rating_rows)) ++ "]"), with (card, composer, status, rating_rows) = frame_key(f).	scn.rs::a_frame_line_says_what_changed_in_python_repr	tuple destructuring, chars().take, map/join, format!	1,3,5,7
screen-survey-rows	contract	screen::is_card_row, screen::is_options_row	∀ r: is_card_row(r) ⇔ r = "●" ++ c ++ rest with c one whitespace char and rest starting with CARD (the survey.question anchor). is_options_row(r) ⇔ r = w1 ++ "1: Bad" ++ w2 ++ "2: Fine" ++ w3 ++ "3: Good" ++ w4 ++ "0: Dismiss" ++ tail with each wi a NON-EMPTY whitespace run (any tail).	screen.rs::the_card_and_its_options_are_read_as_drawn	strip_prefix, Chars::as_str, trim_start length comparison, loop over an array	1,5,7,12
screen-composer	contract	screen::composer_of, screen::composer_row, screen::is_rule	Let rule(r) ⇔ |chars(trim r)| ≥ 40 ∧ all chars ∈ {─, ━}. ∀ rows: composer_of(rows) = None if fewer than two rule rows; else with top, bot the last two rule indices: Some("") if bot = top + 1, else Some(trim_end(strip ' '(strip '❯'(replace(NBSP→' ', rows[top+1]))))). ∀ screen: composer_row("codex", s) = None while a row under the last row opening '›' opens '  › ' (a box's cursor at column 2, the question dialog's under the user's message; the lab's own reading, since aterm reads no Codex screen), else Some(trim(trim_start_matches('›', r))) for r the LAST row (after NBSP→' ') starting with '›', None if none; composer_row(other, s) = the last-two-rules rule of composer_of on the NBSP-normalised rows, the first row stripped of '❯' and fully trimmed.	screen.rs::the_composer_row_is_the_first_row_between_the_last_two_rules, screen.rs::a_turn_stands_in_the_agents_own_composer, screen.rs::codexs_question_dialog_is_no_composer	Vec<usize> collect, slice pattern [.., top, bot], get(range), replace/strip/trim	1,3,5,7
screen-idle-status	contract	screen::is_idle, screen::status_row, screen::is_done_row	∀ r: is_done_row(r) ⇔ r = "✻ " ++ rest ∧ ∃ i: rest[i..] starts with " for " ∧ rest[i+5] is an ASCII digit. ∀ rows: status_row(rows) = the first trim_start(r) that starts with one of ✻✶✳✢·*✽ and contains '…'; is_idle(rows) ⇔ (∃ r ∈ rows: is_done_row(r)) ∧ status_row(rows) = None.	screen.rs::a_turn_is_idle_on_its_done_row_with_no_spinner	match_indices, char-boundary slicing, any/find	1,5,7
screen-kv-field	contract	screen::kv, screen::field	∀ text, key: kv(text) = [(k, v) | w ∈ words(text), w contains '=', (k, v) = split at the first '='] in order; field(text, key) = Some(v) for the first (key, v) ∈ kv(text), else None.	screen.rs::key_value_words_are_found_by_key	split_whitespace, split_once, filter_map/find	1,5,7
stub-analyse-kind	contract	stub::analyse, stub::text_of	∀ body: bytes: analyse(body).kind = "opaque" if body is not a JSON object; else "compact" if text_of(system) contains "tasked with summarizing conversations" or the last user message's text contains "Your task is to create a detailed summary"; else "quota" if max_tokens is an integer ≤ 1; else "aux" if tools is absent or empty (array or object of length 0); else "main". For an object: stream = truthy(stream), msgs = |messages|, tools = |tools|, last_user = text_of(content) of the last role=user message, model = model or "?". analyse never fails.	stub/tests.rs::a_request_is_classed_by_what_it_asks	JSON parse (cross-crate), struct of &'static str/usize/String/J, Option chains	1,3,5,7,12
stub-tool-turn	contract	stub::tool_turn, stub::is_tool_result	∀ body, script: let calls = the object entries of script (or [script]) with a truthy name; first = the text of the first user message; mine = [c ∈ calls | c.when truthy ∧ first contains str(c.when)]. If mine ≠ []: Some(Call(name(mine[d]), input(mine[d]), d)) with d = #tool-result messages, or Some(Done) when d ≥ |mine|. Else with main = [c | no when]: step = None initially; for each user message in order, a tool-result increments a defined step and a message whose text contains "toolstart" resets step to 0; the result is None if step is undefined, else Some(Call(main[step]…, step)) or Some(Done) past the end. Non-JSON body ⇒ None. tool_turn keeps no state.	stub/tests.rs::the_tool_script_is_answered_one_call_per_request_from_toolstart, stub/tests.rs::a_call_with_when_belongs_to_the_subagent_whose_prompt_holds_it	Vec<&J>, closures, Option<usize> fold over messages, enum with String/J/usize payload	1,3,5,7,12,13
stub-split-deltas	contract	stub::split_deltas	∀ text: let d = split_deltas(text). concat(d) = text; d ≠ []; split_deltas("") = [""]; every element but the last has ≥ 12 chars; every element is a concatenation of whole word-plus-trailing-whitespace tokens (a delta never splits a word); and each non-last element is the shortest such prefix reaching 12 chars.	stub/tests.rs::deltas_rebuild_the_text_and_each_but_the_last_holds_twelve_characters	Vec<char> indexing in while loops, String accumulation, mem::take	1,5,7
stub-mode-params	contract	stub::limit_params, stub::bg_params, stub::clean_mode, stub::parse_query, stub::percent_decode	∀ mode: limit_params(mode) = Some((claim, reset, plan')) ⇔ split(mode, ':') = [_, claim, reset] or [_, claim, reset, plan] with claim ∈ [a-z0-9_]+ and reset ∈ [0-9]+ fitting i64; plan' = Some(plan) iff plan ∈ [a-z0-9_-]+ (else None, not a refusal). ∀ mode, slow: bg_params(mode, slow) = (n(1, 20), n(2, slow)) with n(i, d) = max(1, parse(trim(part i))) if it parses as i64, else d. ∀ m: clean_mode(m) = strip "custom:"(k') with k = take(64, filter([A-Za-z0-9_.:-], m)) and k' = k, or "ok" if k = "". ∀ q: parse_query(q) = [(pd(k), pd(v)) | kv ∈ split(q,'&'), kv = k=v at the first '=', pd(v) ≠ ""], pd = form-urlencoded decoding (+ ↦ space, %HH ↦ byte, other % kept).	stub/tests.rs::mode_parameters_parse_as_the_modes_document_them	slice patterns over Vec<&str>, Option filter, i64 parse, filter/take, byte loop with lossy UTF-8	1,3,5,7
stub-rate-headers	contract	stub::rate_headers, stub::seq_value, stub::value_text, stub::RateHeaders::summary	∀ spec, served: rate_headers(spec, served) = {on200, on429} with on_x = [(h, seq_value(value_text(v), served)) | (h, v) ∈ spec[on_x] in order] if spec[on_x] is an object, else []. seq_value(v, n) = format("{:.4}", clamp(base + step·n, 0, 1)) when v = "@SEQ:" ++ base ++ ":" ++ step ++ "@" with base, step ∈ [0-9.]+ parsing as f64; else v unchanged. value_text = Python str() of the JSON value (Int as digits, Float as py_float, True/False/None). summary() prints the RL200[…] and RL429[…] groups with the anthropic-ratelimit-unified- prefix cut.	stub/tests.rs::rate_headers_rise_per_answer_served_and_clamp_at_one	Vec<(String,String)>, f64 arithmetic/clamp/precision format, u32→f64, closures	1,3,5,6,7
stub-analyse-responses	contract	stub::analyse_responses	∀ body: analyse_responses(body).kind = "opaque" if body is not a JSON object; else "codex-json" if text.format.type = "json_schema" (schema = text.format.schema), else "codex". For an object: items = |input|, tools = |tools| (array), instr_chars = chars of instructions (a string) or of its py() (non-null other) else 0, stream = truthy(stream), last_user = the content of the last role=user item (a string, or the input_text/text blocks' texts joined by \n).	stub/tests.rs::a_codex_request_is_read_for_its_items_and_its_structured_side_requests	JSON parse, Option chains, filter/map/join	1,3,5,7,12
stub-schema-answer	contract	stub::schema_answer	∀ schema: schema_answer(schema) = PyJSON(Obj[(k, take(m(v), "lab stub")) | (k, v) ∈ schema.properties in order, v.type = "string"]) with m(v) = v.maxLength if a positive integer else 8; "{}" when there are no properties. The answer validates against any schema whose string properties carry only type/maxLength constraints.	(secondary only; see §2)	J object iteration, i64→usize try_from, chars take	1,3,5,7
stub-usage-limit-and-flat-headers	contract	stub::usage_limit_body, stub::usage_limit_headers, stub::flat_headers	∀ claim, reset, plan: usage_limit_body = {error: {type: usage_limit_reached, message, resets_at: reset, limit_window_minutes: 300 iff claim = five_hour, 10080 iff seven_day, absent otherwise, plan_type: plan iff Some}}; usage_limit_headers(claim, reset) = [x-codex-<slot>-used-percent=100, -window-minutes, -reset-at=reset] for five_hour (primary, 300) and seven_day (secondary, 10080), [] otherwise. ∀ spec, now: flat_headers(spec, now) = [] unless spec is an object; else for each (k, v) in order with v scalar (not null/object/array) and k ∈ [A-Za-z0-9-]{1,80}: the entry (lowercase(k), t) with t = value_text(v), every well-formed @NOW+N@ replaced by now + N left to right and control chars mapped to ' '; a later key equal after lowercasing overwrites the value in place. Obligation: now + N does not overflow (§5.11).	(secondary only; see §2)	J building, while-let string rewriting, char map, Vec find/iter_mut, i64 addition (overflow obligation)	1,3,5,7
stub-served-refines-handlers	refinement	stub::start, stub::Stub::handle, stub::Stub::messages, stub::Stub::responses, stub::Stub::control, stub::Stub::black_hole, stub::control, stub::read_request	For every request sequence sent to start(args)'s listener, each HTTP response (status, headers, body/SSE) equals the pure Answer(mode, headers_file contents at request time, modes_dir, served-200 count, request) built from analyse/analyse_responses, rate_headers, flat_headers, usage_limit_*, auth_label and split_deltas: ok mode streams message_start..message_stop (or response.created..response.completed) whose text deltas concatenate to the canned reply; overloaded ⇒ 529 (Anthropic) / 503 (Codex); limit:C:R with R in the future ⇒ 429 with the representative claim and on429 headers, R past ⇒ ok and "limit lifted" logged; a mode file that changes wins; unknown paths ⇒ 404; /__mode sets and persists the mode; a custom mode with auth answers only the named credential; the proxy port answers every CONNECT with 502; every request appends one log line naming kind and auth label, and no token value is ever logged.	stub/tests.rs::served_on_loopback_it_answers_as_the_modes_say_and_logs_each_request, stub/tests.rs::served_on_loopback_it_answers_codex_as_the_modes_say	TcpListener/TcpStream, threads, Arc<Mutex<State>>, file I/O, HTTP framing, SSE chunking, cross-crate aterm_http client	1,2,7,8,10,12,13
sys-put-no-follow	contract	sys::put, sys::open_dir_at	∀ path, mode, src, Fs: put(path, mode, src) = Err if path is relative, if any directory component on the way is a symlink, or if src = File(p) where p is not a regular single-name file reached through no link (the scratch_file rules); in every Err case no file outside path's own directory chain is created or changed (a link's target is untouched). On Ok: path names a fresh regular file (never a symlink; a link standing at the name is replaced and its target untouched) with contents = the src bytes (or p's contents) and permission bits = mode & 0o777; missing directories on the way are created 0755; the replacement is atomic (by rename).	sys.rs::put_writes_by_a_rename_and_never_through_a_link	unsafe libc openat/mkdirat/renameat/fchmod FFI, RawFd ownership, CString, Path components loop	1,7,8
sys-scratch-read	contract	sys::scratch_read, sys::scratch_file	∀ path, Fs: scratch_read(path) = Ok(None) iff the final name is missing; Ok(Some(contents)) iff every component is reached through no symlink and the file is a regular file owned by the current uid with nlink = 1; Err otherwise (a link at the name or on the way, a second hard link, a directory, a FIFO or other non-regular file). The call never blocks on a FIFO (it opens non-blocking).	sys.rs::scratch_read_takes_only_the_users_regular_file_with_one_name	unsafe libc open with O_NOFOLLOW|O_NONBLOCK, fstat, OwnedFd, read_to_end	1,7,8
tripwire-argv	contract	tripwire::parse_argv, tripwire::answer, tripwire::quoted_after	∀ args: parse_argv(args) = (args[0] or "", acct, svc) where, scanning args[1..] left to right, "-a" X sets acct = X, "-s" X sets svc = X ("" when X is missing), every flag in {-X,-p,-j,-l,-D,-C,-T,-c,-r,-G,-t,-y,-k} consumes its value unread, and acct/svc default to "-". Hence no value following a secret-bearing flag appears in the result unless it is also the value of -a/-s. answer(cmd) = 44 for the listed find-*/delete-* password/certificate/identity sub-commands, 0 for show-keychain-info and add-*-password, 1 otherwise. ∀ line, flag: quoted_after(line, flag) = the text between " " ++ flag ++ " \"" at its LAST occurrence and the next '"'; "-" when there is no occurrence or no closing quote.	tripwire.rs::logs_the_account_and_service_and_never_a_secret_value, tripwire.rs::interactive_lines_name_their_last_account_and_service	while loop with index skipping, match on &str, rfind, slicing	1,3,5,7
util-keep-tree	contract	util::keep_tree, util::keep_file, sys::scratch_file, sys::put	∀ from, to, Fs: after keep_tree(from, to): if from is not a real directory (symlink or missing) nothing happens; else for every entry reached from from through directories only (no symlink followed, at any depth): a real directory is recreated under to and recursed into; a regular file that scratch_file accepts (single name, this user's, no link on its path) is copied to the corresponding path with the same bytes, permission mode and modification time; symlinks and second-name files are not copied. keep_file(src, dst) copies nothing when src's path holds a link anywhere. Nothing outside to is written and no source is changed. Uses sys-put-no-follow and sys-scratch-read.	util.rs::archived_scratch_files_keep_times_but_refuse_other_names	recursive fs walk, read_dir, symlink_metadata, File::set_modified, effects throughout	1,7,8
util-utc-stamp	refinement	util::utc_stamp	∀ secs: i64 with |secs| bounded so the arithmetic does not overflow: utc_stamp(secs) = sprintf("%04d-%02d-%02dT%02d:%02d:%02d.000000Z", civil(secs)), civil = the proleptic Gregorian UTC date-time of the unix second; equivalently days_from_civil(y, m, d)·86400 + h·3600 + mi·60 + s = secs with 1 ≤ m ≤ 12, 1 ≤ d ≤ days_in_month(y, m), 0 ≤ h < 24, 0 ≤ mi, s < 60.	util.rs::utc_stamps_are_the_calendar_day	i64 div_euclid/rem_euclid, Hinnant civil_from_days arithmetic, format! padding	1,3,5
util-b64url-roundtrip	round-trip	util::b64url, util::b64url_decode	∀ bytes: b64url_decode(b64url(bytes)) = Some(bytes); b64url(bytes) uses only [A-Za-z0-9_-] (no '=') and has length ⌈4·|bytes|/3⌉ (RFC 4648 §5 without padding). ∀ text: b64url_decode(text) = None iff trim_end('=', text) contains a char outside the alphabet. One direction only (§5.7).	util.rs::base64url_round_trips_without_padding	chunks(3) with u32 shifts, position lookup in a const table, u8 try_from, Vec<u8>	1,3,5,7
util-one-line	contract	util::one_line	∀ s, n: let t = trim(s with every maximal run of control chars (< 0x20 or 0x7f) replaced by one ' '). one_line(s, n) = t if |chars(t)| ≤ n, else take(n - 1, chars(t)) ++ "~" (saturating at n = 0, giving "~"). The output holds no control char and has at most max(n, 1) chars.	util.rs::one_line_folds_control_runs_and_cuts_long_text	chars loop with a bool state, trim, count/take, saturating_sub	1,5,7
util-py-repr	refinement	util::py_repr	∀ s whose chars are all Python-printable or among \\, \n, \r, \t, other C0 controls and DEL: py_repr(s) = PyRepr(s): the delimiter is '"' iff s contains ' and no ", else '; backslash, \n, \r, \t are escaped; the delimiter is escaped; other C0/DEL as \xNN. Restricted to that domain (§5.5).	util.rs::py_repr_picks_the_quote_python_picks	chars loop with match guards, format!("{:02x}")	1,5,7,10
verbs-harness-set	contract	verbs::harness_set	∀ text, kvs: let T' = harness_set(text, kvs). If text has no line equal (after trim_end) to "[harness]", T' = text's lines ++ ["", "[harness]"] and then the keys are applied; for each (k, v) in order, within the [harness] table (from its header to the next line starting with '['): every line l = k ++ ws* ++ "=" … is replaced by "k = v"; if there is none, "k = v" is inserted after the table's last non-blank line. Every other line is unchanged and in order; the trailing newline is kept iff text ended with one (or was empty). Idempotent: harness_set(harness_set(t, kvs), kvs) = harness_set(t, kvs).	verbs.rs::harness_keys_are_set_in_their_table_and_replaced_when_there	Vec<String> insert/position/slicing, &mut iteration, closures capturing k	1,2,5,7
verbs-journal-line	contract	verbs::journal_line, util::hms	∀ line: J::parse(line) fails ⇒ journal_line(line) = trim_end(line). Else with t = r.t (number, default 0) / 1000: journal_line(line) = hms(floor t) ++ "." ++ pad3((t·1000 truncated) mod 1000) ++ " " ++ padR(10, kind or "?") ++ " " ++ padR(12, phase or "-") ++ " " ++ (line field or ""). hms is the local wall-clock h:m:s, an environment input.	verbs.rs::a_journal_record_prints_as_one_aligned_line	JSON parse, f64 floor and casts, rem_euclid, format! with width, local-time FFI in hms	1,3,6,8,12
verbs-version-order	contract	verbs::version_key, verbs::semver_like	∀ v: version_key(v) = [parse_u64(x) | x ∈ take(3, split(v, '.')), x parses as u64]; for v, w both a.b.c with a, b, c decimal u64: version_key(v) < version_key(w) (lexicographic Vec order) ⇔ (a, b, c)_v < (a, b, c)_w numerically. semver_like(v) ⇔ split(v, '.') has ≥ 3 parts and each of the first three is non-empty and starts with an ASCII digit.	verbs.rs::versions_order_by_their_numbers	split/take/filter_map/collect, Vec<u64> Ord, u64 parse	1,5,7
```

Merge notes: `footers-done-row-pattern` and `screen-idle-status` both read `is_done_row`; the
definition lives once, in `screen-idle-status`, and the footers row states only the restricted
regex equivalence. `kv` is defined once (`screen-kv-field`) and cited by `nudge-parse-resizes`.
`stub-auth-label` is the one statement of the credential-label rule; `codex-auth-json-lab-jwt`
composes it instead of restating it.

### Distribution by kind (69)

| kind | count | ids |
|---|---|---|
| contract | 56 | every id not listed below |
| refinement | 6 | launcher-argv, pyjson-dumps-refines-python, pyjson-py-float, stub-served-refines-handlers, util-utc-stamp, util-py-repr |
| round-trip | 5 | codex-auth-json-lab-jwt, probe-op-codec-roundtrip, run-env-sh-roundtrip, util-shell-quote-unquote-roundtrip, util-b64url-roundtrip |
| equivalence | 1 | footers-done-row-pattern |
| table-property | 1 | probe-judge-table |

### Gap histogram (specs blocked by each gap; gap 0 blocks all 69)

| gap | specs |
|---|---|
| 1 non-scalar carriers | 68 (all but restore-since) |
| 3 E6 body widening | 42 |
| 5 quantified clauses | 55 |
| 7 loops/iterators | 61 |
| 2 `&self`/`&mut` views | 8 |
| 6 plain-Rust publication | 4 |
| 8 effects | 11 |
| 10 model refinement | 7 |
| 11 generics | 1 |
| 12 cross-crate citation | 15 |
| 13 threads/closures/higher-order | 10 |

No spec is blocked by gap 0 alone. Gap 1 is the crate's real floor: every spec but one carries a
`&str`, `String`, `Vec`, `Option` or a crate enum in its clause.

### Proof dependency order

Lexical predicates (`lexical-validators`, `paths-forgotten`, `protected-inside`, `recorder-logged`,
`relay-parse-args`, `tripwire-argv`) → string utilities (`util-shell-quote-unquote-roundtrip`,
`util-b64url-roundtrip`, `util-utc-stamp`, `util-one-line`, `util-py-repr`, `pyjson-py-float`) →
`pyjson-dumps-refines-python`, `pyjson-merge-is-merge-patch` → `screen-*` and `screen-kv-field` →
parsers built on them (`nudge-*`, `record-stream-framing`, `sampler-endpoints`, `stub-*` pure
readers) → composites (`codex-auth-json-lab-jwt` needs b64url, pyjson, utc_stamp and
`stub-auth-label`; `run-env-sh-roundtrip` and `launcher-argv` need the shell round trip;
`scn-cycle-stats-judge` needs `screen-idle-status` and `screen-composer`; `scn-frame-line` needs
`util-py-repr`) → fs/process specs (gap 8: `sys-*` first, then `util-keep-tree`,
`restore-agents-of`, `paths-physical`, `jail-*`, `procs-reap-liveness`) →
`stub-served-refines-handlers` last (gaps 8 + 13).

## 2. Test → spec map (TSV; all 76)

`primary` is the spec that retires the test. `also` lists the other specs the same test samples;
the test is deleted only when all of them are Certified.

```tsv
file	test	primary	also
src/codex.rs	a_login_or_a_server_never_runs_in_the_lab	codex-safe-args	
src/codex.rs	the_config_names_its_login_and_trusts_the_project_both_ways	codex-config-text	
src/codex.rs	the_composed_login_is_a_jwt_the_stub_reads_by_its_label_only	codex-auth-json-lab-jwt	stub-auth-label, util-b64url-roundtrip, util-utc-stamp, pyjson-dumps-refines-python
src/codex.rs	ratelimits_name_each_window_and_the_credits	codex-ratelimit-headers	pyjson-py-float
src/codex.rs	the_launcher_execs_codex_by_path_with_no_daemon	launcher-argv	util-shell-quote-unquote-roundtrip
src/door.rs	only_a_lab_shaped_socket_passes	lexical-validators	
src/door.rs	a_pattern_crosses_the_wire_as_one_token	door-wire-pattern	
src/footers.rs	the_mode_row_names_its_mode	footers-mode-word	
src/footers.rs	the_footer_row_is_the_last_row_with_text_as_a_literal_pattern	footers-footer-row-pattern	
src/footers.rs	the_done_row_pattern_reads_what_the_done_row_reader_reads	footers-done-row-pattern	screen-idle-status
src/footers.rs	the_variant_is_parsed_and_named	footers-opts-parse	pyjson-dumps-refines-python
src/jail.rs	every_protected_entry_and_mach_service_becomes_one_rule	jail-render-rules	protected-inside
src/jail.rs	the_agents_under_test_are_readable_inside_the_store_they_live_in	jail-render-rules	
src/jail.rs	a_replica_home_in_tmp_is_left_to_its_own_rules	jail-render-rules	
src/jail.rs	a_path_the_profile_cannot_quote_a_bad_mark_or_a_run_in_the_owners_data_is_refused	jail-render-refusals	protected-inside
src/lab.rs	the_managed_claude_is_the_last_path_of_the_arrow_chain	lab-managed-bin	
src/lab.rs	claude_options_parse_and_the_launcher_execs_the_binary_by_path	launcher-argv	lab-parse-launch, util-shell-quote-unquote-roundtrip
src/lab.rs	the_tripwire_log_is_kept_with_its_account_masked	lab-mask-account	
src/nudge.rs	screens_differ_by_the_rows_whose_text_differs	nudge-differing-spans	
src/nudge.rs	resizes_reads_its_header_and_epochs	nudge-parse-resizes	screen-kv-field
src/nudge.rs	the_configured_nudge_is_the_render_tables_word	nudge-configured-nudge	
src/nudge.rs	a_healed_trial_passes_on_and_fails_off	nudge-judge-verdict	nudge-differing-spans
src/nudge.rs	a_trial_that_never_displaced_is_inconclusive	nudge-judge-verdict	nudge-differing-spans
src/nudge.rs	a_nudge_log_line_is_shown_without_its_prefix_and_ids	nudge-log-line	
src/paths.rs	forgets_agent_and_aterm_variables_and_keeps_the_rest	paths-forgotten	
src/paths.rs	tags_are_short_plain_words	lexical-validators	
src/paths.rs	physical_resolves_the_existing_head_and_keeps_the_rest	paths-physical	
src/probe.rs	every_operation_crosses_into_the_jail_unchanged	probe-op-codec-roundtrip	
src/probe.rs	the_replica_plan_covers_every_protected_path_and_every_mach_service	probe-replica-plan-coverage	
src/probe.rs	a_denial_counts_only_where_the_control_got_through	probe-judge-table	
src/procs.rs	descendants_come_children_first_and_never_for_init	procs-descendants-in	
src/procs.rs	an_ended_child_is_reaped_and_reads_dead	procs-reap-liveness	
src/procs.rs	a_wait_that_runs_out_leaves_the_child_to_a_later_one	procs-reap-liveness	
src/protected.rs	inside_means_the_path_or_below_it_and_prefix_means_the_name_too	protected-inside	
src/pyjson.rs	spells_as_python_json_dumps_does	pyjson-dumps-refines-python	pyjson-py-float
src/pyjson.rs	floats_read_as_python_repr	pyjson-py-float	
src/pyjson.rs	merge_removes_on_null_merges_objects_and_replaces_lists	pyjson-merge-is-merge-patch	
src/record.rs	the_stream_yields_frames_with_their_rows_and_events_whole	record-stream-framing	
src/recorder.rs	a_url_is_logged_by_its_scheme_and_host_never_its_query	recorder-logged	
src/relay.rs	the_relay_takes_an_optional_log_then_dashes_then_a_command	relay-parse-args	
src/relay.rs	a_take_names_its_log_then_dashes_then_a_command_and_writes_ptyrecs_lines	relay-ptyrec-take	
src/restore.rs	the_agent_is_the_sessions_record_whose_pid_is_in_the_tree	restore-agents-of	restore-since, sys-scratch-read
src/run.rs	env_sh_round_trips_every_field	run-env-sh-roundtrip	util-shell-quote-unquote-roundtrip
src/run.rs	reads_the_env_sh_printf_q_wrote	util-shell-quote-unquote-roundtrip	
src/sampler.rs	loopback_endpoints_are_plain_others_are_external_and_each_names_its_pid	sampler-endpoints	
src/scn.rs	the_journal_goes_before_whether_it_holds_a_dismissal	scn-put-journal-order	
src/scn.rs	the_cycle_is_measured_from_frames_and_journal_and_judged	scn-cycle-stats-judge	screen-idle-status, screen-composer
src/scn.rs	a_frame_line_says_what_changed_in_python_repr	scn-frame-line	util-py-repr
src/screen.rs	the_card_and_its_options_are_read_as_drawn	screen-survey-rows	
src/screen.rs	the_composer_row_is_the_first_row_between_the_last_two_rules	screen-composer	
src/screen.rs	a_turn_stands_in_the_agents_own_composer	screen-composer	
src/screen.rs	codexs_question_dialog_is_no_composer	screen-composer	
src/screen.rs	a_turn_is_idle_on_its_done_row_with_no_spinner	screen-idle-status	
src/screen.rs	key_value_words_are_found_by_key	screen-kv-field	
src/stub/tests.rs	a_request_is_classed_by_what_it_asks	stub-analyse-kind	
src/stub/tests.rs	the_tool_script_is_answered_one_call_per_request_from_toolstart	stub-tool-turn	
src/stub/tests.rs	a_call_with_when_belongs_to_the_subagent_whose_prompt_holds_it	stub-tool-turn	
src/stub/tests.rs	deltas_rebuild_the_text_and_each_but_the_last_holds_twelve_characters	stub-split-deltas	
src/stub/tests.rs	mode_parameters_parse_as_the_modes_document_them	stub-mode-params	
src/stub/tests.rs	rate_headers_rise_per_answer_served_and_clamp_at_one	stub-rate-headers	pyjson-py-float
src/stub/tests.rs	served_on_loopback_it_answers_as_the_modes_say_and_logs_each_request	stub-served-refines-handlers	stub-analyse-kind, stub-rate-headers, stub-split-deltas, stub-auth-label, stub-mode-params
src/stub/tests.rs	a_codex_request_is_read_for_its_items_and_its_structured_side_requests	stub-analyse-responses	stub-schema-answer, stub-usage-limit-and-flat-headers
src/stub/tests.rs	served_on_loopback_it_answers_codex_as_the_modes_say	stub-served-refines-handlers	stub-analyse-responses, stub-usage-limit-and-flat-headers, stub-auth-label, stub-split-deltas
src/sys.rs	put_writes_by_a_rename_and_never_through_a_link	sys-put-no-follow	
src/sys.rs	scratch_read_takes_only_the_users_regular_file_with_one_name	sys-scratch-read	
src/sys.rs	the_user_temp_dir_is_named_and_this_process_is_in_no_jail	(none; §4)	
src/tripwire.rs	logs_the_account_and_service_and_never_a_secret_value	tripwire-argv	
src/tripwire.rs	interactive_lines_name_their_last_account_and_service	tripwire-argv	
src/util.rs	archived_scratch_files_keep_times_but_refuse_other_names	util-keep-tree	sys-put-no-follow, sys-scratch-read
src/util.rs	utc_stamps_are_the_calendar_day	util-utc-stamp	
src/util.rs	base64url_round_trips_without_padding	util-b64url-roundtrip	
src/util.rs	one_line_folds_control_runs_and_cuts_long_text	util-one-line	
src/util.rs	shell_words_round_trip_through_quote_and_unquote	util-shell-quote-unquote-roundtrip	
src/util.rs	py_repr_picks_the_quote_python_picks	util-py-repr	
src/verbs.rs	harness_keys_are_set_in_their_table_and_replaced_when_there	verbs-harness-set	
src/verbs.rs	a_journal_record_prints_as_one_aligned_line	verbs-journal-line	
src/verbs.rs	versions_order_by_their_numbers	verbs-version-order	
```

## 3. Coverage

- 76 tests read. 75 map to one of the 69 specs. 1 is not a spec sample (§4).
- Five specs have no primary test of their own and are reached only through `also`:
  `stub-auth-label`, `lab-parse-launch`, `restore-since`, `stub-schema-answer` and
  `stub-usage-limit-and-flat-headers`. They are still worth proving separately: each is the lemma
  a composite needs.
- The biggest collapses: `jail-render-rules` (3 primary), and two primary tests each for
  `launcher-argv`, `nudge-judge-verdict`, `procs-reap-liveness`, `screen-composer`,
  `stub-tool-turn`, `stub-served-refines-handlers`, `tripwire-argv`, `lexical-validators` and
  `util-shell-quote-unquote-roundtrip`.
- The tests here are already mostly one-per-property, so the gain is generality (∀ inputs) rather
  than a large drop in test count.

## 4. Tests that are not spec samples

| test | why | action |
|---|---|---|
| sys.rs::the_user_temp_dir_is_named_and_this_process_is_in_no_jail | An environment probe, not a property of the code. It passes only when the test process runs on macOS outside every sandbox (`jail_pids` returns Err inside one, e.g. under a sandboxed agent) and when `confstr` reports a temp dir. The one contract it touches (`jail_pids(m) = Ok(pids denied m and not m.none)`) needs a kernel model and is better checked as a lab self-test on a real host. | **move-to-bench** (a lab self-test on a real host) |

No test is a pure regression of a removed bug with no general property.

## 5. Implied specs that look wrong, or disagree with the code

1. **`footers.rs::the_done_row_pattern_reads_what_the_done_row_reader_reads`.** The name claims
   `DONE_ROW` and `screen::is_done_row` agree; in general they do not. The regex
   `✻.\S+.for.[0-9]` is unanchored, its `.` matches any char, and it needs a one-word verb.
   `is_done_row` needs a leading `"✻ "` and accepts any text before `" for <digit>"`. So
   `"✻ Worked hard for 3s"` is a done row the pattern misses, and `"x ✻aWorkedXforY1"` matches
   the pattern but is not a done row. The spec is limited to single-word verbs.
2. **`run.rs::env_sh_round_trips_every_field`.** `Run::parse(render(r)) = r` holds only when
   `r.agent` is not empty (render writes an empty agent as `"claude"`, and `Run::default()` has an
   empty agent) and every field is newline-free (`shell_quote` keeps `'\n'` inside single quotes;
   `parse` splits on `lines()`). The test sets `agent = "codex"` and uses newline-free values.
3. **`probe.rs::every_operation_crosses_into_the_jail_unchanged`.** `Op::encode` does not escape
   `'\t'` or `'\n'` in paths, and `Serve(Some(""))` decodes as `Serve(None)`. The round trip holds
   only for tab-free fields, so the spec must also prove `plan` never makes a path with a tab. A
   home or run path with a tab would decode into a different op.
4. **`sampler.rs::loopback_endpoints_are_plain_others_are_external_and_each_names_its_pid`.**
   `endpoints` calls `l.split_at(l.len().min(1))`, which panics when a line starts with a
   multi-byte char: a no-panic gap on untrusted lsof output. Also, a listener on `0.0.0.0` or
   `[::]` is labelled EXTERNAL, which is conservative but not what "loopback is plain" implies.
5. **`util.rs::py_repr_picks_the_quote_python_picks`.** `py_repr` is documented as Python's
   `repr`, but it leaves non-printable non-ASCII chars unescaped (U+00A0, U+0080..U+009F, U+2028),
   while Python writes them as `\xNN`/`\uNNNN`. `scn::frame_line` runs `py_repr` over screen rows,
   and Claude Code puts NBSP after the `❯` marker, so FRAME lines can differ from the Python
   harness's output. The spec is limited to printable text.
6. **`verbs.rs::versions_order_by_their_numbers`.** `version_key` silently drops a component that
   does not parse, so `"2.1.300-rc1"` gives `[2,1]` and sorts below `"2.1.0"`. `semver_like`
   accepts `"2.1.3x"` because it checks only the first char of each part. The test samples clean
   versions only.
7. **`util.rs::base64url_round_trips_without_padding`.** Only `decode(encode(b)) = b` holds.
   `b64url_decode` also accepts non-canonical text (length ≡ 1 mod 4, nonzero trailing bits), so
   `encode(decode(t)) ≠ t` can happen. Harmless for JWT labels; the spec states one direction.
8. **`codex.rs::the_launcher_execs_codex_by_path_with_no_daemon`** (also
   `lab.rs::claude_options_parse_and_the_launcher_execs_the_binary_by_path`). Both launchers put
   `run.run/shim` (inside `"…"`), `run.proxy` and `run.port` into the exec line without
   `shell_quote`, and lab's launcher quotes the relay path only through `q()`. A run directory
   holding `"`, `$` or a backtick makes the shell read a different exec line. The tests use safe
   paths only, so `launcher-argv` carries that precondition; quoting these fields in the code
   would remove it.
9. **`door.rs::a_pattern_crosses_the_wire_as_one_token`.** `wire_pattern` escapes whitespace and
   `=` without regard to a preceding backslash. A regex-escaped space (`"\\ "`) becomes
   `"\\\\x20"`, an escaped backslash followed by literal `x20`, so the pattern changes meaning. The
   semantic clause is limited to inputs without that case.
10. **`procs.rs::descendants_come_children_first_and_never_for_init`.** The doc says "every
    descendant", but the walk silently stops at depth 64. The test does not sample this. Either
    the doc or the spec should state the bound.
11. **`stub/tests.rs::a_codex_request_is_read_for_its_items_and_its_structured_side_requests`.**
    `flat_headers` computes `now + n` with `n` parsed from `@NOW+N@` in a headers file; near
    `i64::MAX` this overflows (a panic in debug builds). A missing no-panic obligation, not a test
    failure.
12. **`nudge.rs::the_configured_nudge_is_the_render_tables_word`.** The name says the `[render]`
    table's word, but `configured_nudge` ignores table headers and takes the first
    `repaint_nudge` key in any table.

## 6. Proof order: the specs I would prove first (easiest, high value)

No spec is blocked by gap 0 alone; gap 1 (string/slice/Option carriers) is the floor for 68 of
69. So the order picks the fewest extra gaps, then value.

1. **`restore-since`** (gaps 3, 6). The only spec without gap 1: `i64` subtraction, one cast and a
   precision format. It needs a model of `{:.1}` float formatting, which `pyjson-py-float` and
   `stub-rate-headers` reuse.
2. **`probe-judge-table`** (gaps 1, 3). A finite decision table over enums and `Option<&str>`,
   with no loops: the sandbox probe's verdict logic, so a wrong cell would hide a jail leak.
3. **`relay-parse-args`** (gaps 1, 3). Slice patterns returning a borrowed tail; the first test of
   slice carriers and lifetimes on a function every relayed launch runs.
4. **`lab-managed-bin`**, **`util-utc-stamp`** (gaps 1, 3, 5). No loops. `util-utc-stamp` is pure
   integer arithmetic once the string is a model of `format!` padding, and it feeds
   `codex-auth-json-lab-jwt`.
5. **`lexical-validators`**, **`paths-forgotten`**, **`recorder-logged`**, **`util-one-line`**,
   **`screen-kv-field`**, **`screen-idle-status`** (gaps 1, 5, 7). Small character predicates over
   one loop each. Security value: `paths-forgotten` decides which agent credentials the lab's
   children never see, and `recorder-logged` that no URL query is logged.
6. Then `lab-parse-launch` (1, 3, 7), the string utilities (`util-shell-quote-unquote-roundtrip`,
   `util-b64url-roundtrip`, `util-py-repr`), and the rest of §1's dependency order. The gap-8
   specs (`sys-*`, `util-keep-tree`, `restore-agents-of`, `paths-physical`, `jail-*`,
   `procs-reap-liveness`, `footers-opts-parse`, `verbs-journal-line`) and
   `stub-served-refines-handlers` come last.

## 7. Added 2026-10-16: the Gemini CLI and agy tests

Twenty-two tests came with the Gemini CLI and agy lab (`gemini.rs`, `gemini_captures.rs`,
`gemini_proof.rs`, `stub/gemini.rs` through `stub/tests.rs`, and one each in `lab.rs`,
`screen.rs` and `sys.rs`), read the same way as §1: every one maps to a spec below, and none is
outside a spec. `composer_row` now has its own branches for `"gemini"` and `"agy"`
(`screen-composer-gemini-agy`), so `screen-composer`'s last clause ("`composer_row(other, s)`")
holds only for `"claude"` and any agent name but these three.

```tsv
id	kind	functions	statement	subsumes	difficulty	blocking_gaps
gemini-safe-args	contract	gemini::safe_args	∀ agent ∈ {Gemini, Agy}, args: safe_args(agent, args) = Ok(()) ⇔ ∀ a ∈ args: a ∉ {login, logout, auth} ∧ (agent = Gemini ⇒ a ∉ {-s, --sandbox}). On Err the message names the first offending argument, in order.	gemini.rs::a_sign_in_or_a_nested_sandbox_never_runs_in_the_lab	&[String], for loop with early return, match on (enum, &str), Result<(),String>, format!	1,3,5,7
gemini-parse-launch	contract	gemini::parse_launch	∀ agent, args: parse_launch(agent, args) = Ok(l) ⇔ args matches ("-e" V | "--no-wait" | "--untrusted" | "--onboarding" [only when agent = Agy])* ["--" R*]; then l.envs = the V's in order, l.no_wait/l.untrusted = whether each flag occurs, l.onboarding = whether --onboarding occurs, and --onboarding sets l.untrusted too; l.args = R* verbatim. Err when any other token comes before "--", when "-e" is last, or on --onboarding for Gemini.	gemini.rs::options_parse_and_onboarding_is_agys_alone	slice iterator with by_ref, match on (enum, &str), Vec push/extend, Result<_, String>	1,3,7
gemini-seeds	contract	gemini::gemini_settings, gemini::agy_settings, gemini::agy_onboarded, pyjson::J::dump, pyjson::J::parse	∀ untrusted: J::parse(gemini_settings(untrusted)) = Ok(g) with g.security.auth.selectedType = "gemini-api-key", g.security.folderTrust.enabled = untrusted, g.privacy.usageStatisticsEnabled = false, g.telemetry.enabled = false, g.general.enableAutoUpdate = g.general.enableAutoUpdateNotification = false, g.ui.theme = "Default", g.ide.enabled = false, g.ide.hasSeenNudge = true. ∀ trusted: [String]: J::parse(agy_settings(trusted)) = Ok(a) with a.modelProvider = "gemini" (for every input: the sign-in is never left on) and a.trustedWorkspaces = trusted when trusted ≠ [], absent otherwise. agy_onboarded() parses to onboardingComplete = consumerOnboardingComplete = true, enterpriseOnboardingComplete = false.	gemini.rs::the_seeds_keep_the_auth_and_move_only_the_trust	J tree construction, JSON print then parse (cross-crate aterm_json), Option chaining	1,3,5,12
gemini-launcher-argv	refinement	gemini::launcher, util::shell_quote	∀ run, agent, envs, args, provided the fields interpolated WITHOUT shell_quote (run.run/shim, run.proxy, run.port) hold no shell metacharacter, quote, '$' or whitespace: the script is "#!/bin/sh\nclear\n" ++ one exec command whose word expansion under the Shell model is exactly [env, -u, GOOGLE_API_KEY, -u, GOOGLE_GENAI_USE_VERTEXAI, -u, GOOGLE_CLOUD_PROJECT, -u, GOOGLE_APPLICATION_CREDENTIALS] ++ ([-u each of the six proxy variables] iff agent = Gemini) ++ [PATH=<run>/shim:$PATH, HOME=<w>/<agent.home()>, TMPDIR=<w>/tmp] ++ ([the six proxy variables = run.proxy, NO_PROXY and no_proxy = 127.0.0.1,localhost] iff agent = Agy) ++ [GEMINI_API_KEY=lab-gemini-not-a-key, GOOGLE_GEMINI_BASE_URL=http://127.0.0.1:<port>, NO_BROWSER=true (Gemini) or AGY_CLI_DISABLE_AUTO_UPDATE=true (Agy), BROWSER=<run>/shim/open, LAB_OPEN_LOG=<w>/open.log, LAB_SECURITY_LOG=<w>/security.log] ++ envs ++ [agent.bin(run)] ++ args. So the agent runs by path, with no proxy for Gemini CLI and the black hole for agy, and every user word survives as one argv element. Uses util-shell-quote-unquote-roundtrip.	gemini.rs::the_launchers_exec_the_agent_by_path_with_the_fake_key_and_the_stub	format! string building, PathBuf join/display/to_string_lossy, for over &[String], a Shell model	1,3,5,7,10
gemini-capture-scripts	contract	gemini_captures::scripts, pyjson::J::parse	∀ agent: scripts(agent) is four JSON texts, each parsing to an object with a string "name" and an object "input"; the names are [run_shell_command, write_file, replace, ask_user] for Gemini and [run_command, write_to_file, replace_file_content, ask_question] for Agy (no name in both lists); the write's and the edit's target (input.file_path, else input.TargetFile) are both "@PROJ@/lab-write.txt".	gemini_captures.rs::each_box_is_one_call_of_the_agents_own_tool	&'static str arrays, JSON parse (cross-crate aterm_json), Option chaining	1,5,12
gemini-capture-patterns	contract	gemini_captures::GEMINI, gemini_captures::AGY, gemini_captures::SURVEY, aterm_regex::Regex::new, aterm_regex::Regex::is_match	Every field of GEMINI and AGY compiles as a regex. The rows read off the 0.46.0 and 1.4.0 screens match their pattern: GEMINI.slash on " (1/41)", GEMINI.thinking on " ⠋ Lab stub thinking (esc to cancel, 3s)", GEMINI.edit on Gemini CLI's edit box head row, AGY.thinking on "⣽  tick 6...", AGY.answered on "> Red", AGY.idle on agy's idle footer, SURVEY on agy's survey question; and AGY.thinking does NOT match the streamed "  tick 16 tick 17". A statement about constants under a regex semantics model.	gemini_captures.rs::every_screen_a_capture_waits_for_matches_its_measured_row	const structs of &str, a regex semantics model (cross-crate aterm_regex)	1,5,12
lab-vendor-version	contract	lab::vendor_version, lab::managed_bin	∀ text: vendor_version(text) = trim_matches('"', trim(v)) for the first line k=v (split at its first '=') whose trim(k) = "version"; "" when no line has that key. The managed_bin clause of lab-managed-bin holds for the name "agy" (its build is the last path of the arrow chain).	lab.rs::an_atpkg_build_record_names_its_version	lines filter_map, split_once, find, trim/trim_matches	1,3,5,7
screen-composer-gemini-agy	contract	screen::composer_row, screen::is_rule	Let bar(r, c) ⇔ r is a run of c (the ▄/▀ bars), rule(r) as in screen-composer, w(r) = |chars(trim_end r)|. ∀ screen (NBSP→' '), rows = its '\n' lines: composer_row("gemini", s) = None unless top = the LAST row with bar(·,▄) exists, some row in [top−3, top) is a rule and a row stands under top; then Some("") if rows[top+1] is a ▀ bar, else Some(trim(strip '>'(trim_start rows[top+1]))). composer_row("agy", s) = None unless the last two rule rows top < bot both have w = max over all rows of w; then Some(trim(strip '>'(rows[top+1]))) when top+1 < bot, else Some(""). So a frame Gemini CLI drew for a SENT prompt (no rule above it) and agy's sent prompts' 60-column rules (under a box, or under its survey) are never read as the composer.	screen.rs::gemini_cli_s_composer_is_its_last_frame_under_the_rule, screen.rs::agy_s_composer_sits_between_two_full_width_rules	Vec<String> collect, rposition, slice ranges, max over map, strip/trim	1,3,5,7
stub-gemini-classify	contract	stub::gemini::analyse_gemini, stub::gemini::schema_fill, stub::gemini::tool_turn, stub::gemini::route	∀ body: analyse_gemini(body).kind = "opaque" if body is no JSON object; else "gemini-json" if generationConfig has responseJsonSchema or responseSchema (then .schema = it) or responseMimeType = "application/json"; else "gemini-aux" if tools = 0; else "gemini" — with tools = Σ over tools[] of |functionDeclarations| (1 for an entry without), contents = |contents|, sys_chars = |chars(the text of systemInstruction's parts)|, last_user = the last non-empty user content's text with a <USER_REQUEST> block unwrapped. ∀ schema, name: schema_fill(schema, name) = the fixed answer for name ∈ {complexity_score: 10, complexity_reasoning, next_speaker: "user", model_choice: "flash"}; else schema.enum's first; else by its type (any case; an array of types its first): object → its required properties (every one when none is required) in key order, each filled under its own name; array → []; integer or number → 0; boolean → false; otherwise "lab stub" cut to maxLength (8 when absent). ∀ body, script: tool_turn(body, script) = None unless a user content holds "toolstart"; with k = the functionResponse parts of the contents after the last such one (of either role) and c = the script's calls without `when`: Call(name_k, input_k, k) while k < |c|, else Done. route(method, path) = Some((model, verb)) ⇔ method = POST ∧ path = /v1beta/models/<model>:<verb> (or /v1/) with verb ∈ {streamGenerateContent, generateContent, countTokens}.	stub/tests.rs::a_gemini_request_is_classed_and_its_tool_script_stepped	JSON tree walks (cross-crate aterm_json), iterator chains, Option<ToolTurn>, string splitting	1,3,5,7,12
stub-gemini-walls	contract	stub::gemini::mode_error, stub::gemini::quota_error	∀ mode, model: mode_error(mode, model) = Some((status, body)) with body.error.code = status and body.error.status = word for (rate-limit, 429, RESOURCE_EXHAUSTED: details a QuotaFailure whose quotaId names a per-day quota, a Help link, a RetryInfo retryDelay "3600s"), (overloaded, 503, UNAVAILABLE), (500, 500, INTERNAL), (401, 401, UNAUTHENTICATED), (bad-key, 400, INVALID_ARGUMENT with an ErrorInfo reason API_KEY_INVALID), (too-long, 400, INVALID_ARGUMENT); None for every mode that is no wall (ok, slow, tool, limit:…). ∀ secs: quota_error(model, secs) carries retryDelay = secs ++ "s".	stub/tests.rs::a_gemini_wall_is_googles_error_body	J tree construction, match on &str, format!	1,3,5
stub-gemini-served	refinement	stub::start, stub::Stub::gemini, stub::Stub::gemini_answer, stub::control	stub-served-refines-handlers for the Gemini API: each POST to a Gemini route answers as the pure Answer for its kind and mode — ok streams SSE `data:` chunks separated by "\r\n\r\n" whose text parts concatenate to the canned reply, the last with finishReason STOP and usageMetadata; a wall mode answers the turn with mode_error's status and body while a side request (gemini-json, gemini-aux) is answered 200 in every mode, a JSON one by schema_fill; tool mode answers tool_turn's call (a functionCall with a thoughtSignature) and then "Tool call done."; slow streams a thought part before its text; a custom mode with auth answers only the named credential; countTokens answers totalTokens; every request, GET /v1beta/models included, appends one log line naming kind and auth label (stub-gemini-credential-label), and no key's or bearer token's value is ever logged.	stub/tests.rs::served_on_loopback_it_answers_gemini_as_the_modes_say	TcpListener/TcpStream, threads, Arc<Mutex<State>>, file I/O, HTTP framing, SSE chunking, cross-crate aterm_http client	1,2,7,8,10,12,13
stub-shared-wall-modes	contract	stub::Stub::messages, stub::Stub::responses	∀ mode ∈ {401, bad-key, 500}: a Claude Code turn (POST /v1/messages) and a Codex turn (POST /v1/responses) are answered with status 401, 401, 500 and a body whose error type is authentication_error / invalid_api_key, authentication_error / invalid_api_key, api_error / server_error respectively: no built-in mode name silently means ok for one API.	stub/tests.rs::the_shared_wall_modes_answer_each_api_in_its_shape	the served stub (effects), JSON bodies, cross-crate aterm_http client	1,7,8,12
sys-scratch-remove	contract	sys::scratch_remove, sys::cpath, sys::cname	Fs model. ∀ path with a parent p and a name n: scratch_remove(path) opens p with O_NOFOLLOW_ANY (no link anywhere on p) and unlinkat(fd, n, 0); Ok(()) when p or n does not exist; Ok(()) and the entry gone when n is a file or a link (a link is removed, never followed: its target is unchanged); Err and nothing removed when p's path holds a link. A path with no parent or name is Err(InvalidInput).	sys.rs::scratch_remove_unlinks_by_name_through_no_link	unsafe FFI (open, unlinkat), errno, an Fs model	1,3,8
gemini-proof-auths	contract	gemini_proof::gemini_auths	∀ log: gemini_auths(log) = (n, other) with n = |{l ∈ lines(log): l contains " kind=gemini"}| and other = [the value after "auth=" of the first such word after " kind=gemini" in l, or "none" when there is none | such l, that value ≠ "gemini-key"], in order. So a log whose every Gemini API request carried the lab's fake key gives other = [], and one with no Gemini request n = 0.	gemini_proof.rs::every_gemini_request_names_the_key_it_carried	lines, split_once, split_whitespace find_map, Vec push	1,5,7
gemini-proof-proxy-refused	contract	gemini_proof::proxy_refused	∀ log: proxy_refused(log) = the hosts h, each once in order of first appearance, of the lines holding "PROXY refused: " whose first word after it containing ':' or '.' is, less a leading http:// or https://, h followed by '/', ':' or the end; [] when no line holds a refusal.	gemini_proof.rs::the_proxy_names_each_refused_host_once	lines, split_once, split_whitespace find, trim_start_matches, split on a char set, Vec contains	1,5,7
gemini-proof-talked	contract	gemini_proof::talked	∀ log, pids, port: talked(log, pids, port) ⇔ ∃ l ∈ lines(log): l ends with "] TCP conn 127.0.0.1:" ++ port and the text between the last '[' before that suffix and it parses as an i32 ∈ pids. So a listen, another port, or a pid that is a prefix of the logged one is no connection.	gemini_proof.rs::the_witness_names_the_pid_that_talked_to_the_stub	lines any, strip_suffix, rsplit_once, parse::<i32>, slice contains	1,5,7
gemini-proof-own-writes	contract	gemini_proof::own_writes, gemini_proof::seeds	∀ agent, rel: own_writes(agent, rel) = [r ∈ rel | r ∉ seeds(agent)] in order, with seeds(Gemini) = {settings.json, trustedFolders.json} and seeds(Agy) = {antigravity-cli, antigravity-cli/settings.json, antigravity-cli/cache, antigravity-cli/cache/onboarding.json}: the entries the lab's seeds are not.	gemini_proof.rs::the_agents_own_writes_are_what_is_not_a_seed	iterator filter, &'static slices, contains	1,5,7
gemini-proof-key-verdict	contract	gemini_proof::key_verdict, gemini_proof::gemini_auths	∀ log: key_verdict(log) = Ok ⇔ let (n, other) = gemini_auths(log) in n > 0 ∧ other = []; else Fail (never Unproven). So a log naming any Gemini API request whose first auth= word is not exactly "gemini-key" — "gemini-key+…" (a bearer beside the key), "other(…", "jwt:…", "none", or no auth= word — fails, and so does a log with no Gemini API request. Uses gemini-proof-auths.	gemini_proof.rs::the_key_check_fails_on_any_credential_but_the_labs_key_alone	tuple destructure, Vec is_empty, a three-valued enum	1,5,7
gemini-proof-egress-verdict	contract	gemini_proof::egress_verdict, gemini_proof::talked	∀ sockets, egress, pids, port: egress_verdict(sockets, egress, pids, port) = Ok ⇔ talked(sockets, pids, port) ∧ ¬∃ l ∈ lines(egress): l contains "EXTERNAL" ∧ ¬∃ l ∈ lines(sockets): l ends with "  EXTERNAL"; else Fail (never Unproven). With sampler-endpoints: for every lsof text f, the logs the witness writes from f ("<ts> " ++ e per endpoint e, and "<ts> EXTERNAL " ++ e for each e ending "  EXTERNAL") give Fail whenever some endpoint's host is not loopback, whichever pid holds it. Uses gemini-proof-talked.	gemini_proof.rs::the_egress_check_fails_when_any_socket_leaves_loopback	lines any, contains/ends_with, bool composition	1,5,7
gemini-proof-refusals-verdict	contract	gemini_proof::refusals_verdict, gemini_proof::proxy_verdict	∀ denied, control: refusals_verdict(denied, control) = Fail if denied ≠ []; else Ok if control; else Unproven. So a refusal naming ~/.gemini fails the check whatever control is. ∀ refused: proxy_verdict(refused) = Unproven if refused = [], else Ok.	gemini_proof.rs::a_refusal_naming_gemini_fails_whether_or_not_the_control_was_kept	if/else on slice emptiness and a bool, a three-valued enum	1,5
stub-gemini-credential-label	contract	stub::gemini::credential_label, stub::auth_label	∀ key, bearer: Option<&str>: credential_label(None, None) = "none"; credential_label(Some(x), None) = credential_label(None, Some(x)) = auth_label(Some(x)); credential_label(Some(k), Some(b)) = auth_label(Some(k)) ++ "+" ++ auth_label(Some(b)). So a request carrying LAB_GEMINI_KEY and any Authorization header is never labelled exactly "gemini-key", and (by stub-auth-label) no label echoes a credential but the fixed fake keys. The served Gemini routes (POST generate/stream/count and GET /v1beta/models) log auth= this label of x-goog-api-key (or the key query) and Authorization.	stub/tests.rs::a_gemini_requests_credentials_are_labelled_both_when_two_ride	match on a pair of Options, format!	1,3,5
```

```tsv
file	test	primary	also
src/gemini.rs	a_sign_in_or_a_nested_sandbox_never_runs_in_the_lab	gemini-safe-args	
src/gemini.rs	options_parse_and_onboarding_is_agys_alone	gemini-parse-launch	
src/gemini.rs	the_seeds_keep_the_auth_and_move_only_the_trust	gemini-seeds	pyjson-dumps-refines-python
src/gemini.rs	the_launchers_exec_the_agent_by_path_with_the_fake_key_and_the_stub	gemini-launcher-argv	util-shell-quote-unquote-roundtrip
src/gemini_captures.rs	each_box_is_one_call_of_the_agents_own_tool	gemini-capture-scripts	
src/gemini_captures.rs	every_screen_a_capture_waits_for_matches_its_measured_row	gemini-capture-patterns	
src/gemini_proof.rs	every_gemini_request_names_the_key_it_carried	gemini-proof-auths	
src/gemini_proof.rs	the_proxy_names_each_refused_host_once	gemini-proof-proxy-refused	
src/gemini_proof.rs	the_witness_names_the_pid_that_talked_to_the_stub	gemini-proof-talked	
src/gemini_proof.rs	the_agents_own_writes_are_what_is_not_a_seed	gemini-proof-own-writes	
src/gemini_proof.rs	the_key_check_fails_on_any_credential_but_the_labs_key_alone	gemini-proof-key-verdict	gemini-proof-auths, stub-gemini-credential-label
src/gemini_proof.rs	the_egress_check_fails_when_any_socket_leaves_loopback	gemini-proof-egress-verdict	gemini-proof-talked, sampler-endpoints
src/gemini_proof.rs	a_refusal_naming_gemini_fails_whether_or_not_the_control_was_kept	gemini-proof-refusals-verdict	
src/lab.rs	an_atpkg_build_record_names_its_version	lab-vendor-version	lab-managed-bin
src/screen.rs	agy_s_composer_sits_between_two_full_width_rules	screen-composer-gemini-agy	
src/stub/tests.rs	a_gemini_request_is_classed_and_its_tool_script_stepped	stub-gemini-classify	
src/stub/tests.rs	a_gemini_wall_is_googles_error_body	stub-gemini-walls	
src/stub/tests.rs	a_gemini_requests_credentials_are_labelled_both_when_two_ride	stub-gemini-credential-label	stub-auth-label
src/stub/tests.rs	served_on_loopback_it_answers_gemini_as_the_modes_say	stub-gemini-served	stub-gemini-classify, stub-gemini-walls, stub-auth-label, stub-gemini-credential-label, stub-split-deltas
src/stub/tests.rs	the_shared_wall_modes_answer_each_api_in_its_shape	stub-shared-wall-modes	stub-served-refines-handlers
src/sys.rs	scratch_remove_unlinks_by_name_through_no_link	sys-scratch-remove	
```

The selftests themselves (`gemini-selftest`, `agy-selftest`) are not unit tests: like `selftest` and
`codex-selftest` they run the real agents in a lab, and their judges are the seven `gemini-proof-*`
specs.

**Still unread** (older than this branch, absent from §1–§2 when it began, and the codex-alab proof's; 31 tests):
`presence.rs` (6: `a_resize_row_reads_its_id_time_and_rows`,
`a_shrink_pairs_with_the_grow_that_takes_it_back`, `the_floor_judges_the_closest_pair`,
`phases_parse_in_any_spelling_and_refuse_strangers`,
`the_stand_in_frame_fills_the_screen_and_parks_on_the_composer`,
`a_verdict_is_the_first_stderr_line_without_the_client_prefix`), `prices.rs` (4), `stats.rs` (3),
`codex_rig.rs` (3: two moved from the retired `codex_upgrade.rs` with the code they read, and `a_stream_carries_each_item_once_and_completes_with_all_of_them`), `codex_alab_proof.rs` (4), `stub/account.rs` (3), `verbs.rs` (3: the two `headers` tests and
`a_custom_mode_is_named_what_the_stub_serves_from_its_file`), `stub/tests.rs` (3:
`each_headers_verb_keeps_the_others_entries`,
`served_on_loopback_the_account_routes_answer_from_the_account_file`,
`a_mode_name_is_judged_as_the_volume_names_its_file`), `restore.rs`
(`the_lift_is_read_off_the_held_tabs_record`) and `sys.rs`
(`scratch_dir_lists_files_and_dirs_through_no_link`).

## 8. Added 2026-10-21: the open recorder's verdict

The four proofs that run an agent in a lab (`selftest`, `codex-selftest`, `gemini-selftest` and
`agy-selftest`, `muse-selftest`) judge the open recorder through one function. `selftest` (Claude Code)
judges it for the first time: Claude Code's launch line now names `LAB_OPEN_LOG` and `BROWSER`
(`launcher-argv`). One new test, read the same way as §1.

```tsv
id	kind	functions	statement	subsumes	difficulty	blocking_gaps
recorder-nothing-opened	contract	recorder::nothing_opened, recorder::Witness::ok	∀ opened, control: nothing_opened(opened, control) ⇔ trim(opened) = "" ∧ ∃ l ∈ lines(control): l ends with " open " ++ CONTROL_URL (CONTROL_URL = "x-lab-control://selftest"); Witness{opened, control}.ok() = nothing_opened(opened, control). So any line an agent's open logged fails the check, and so does a control that was not logged (the empty log of a recorder that logs nothing) or one that logged another URL.	recorder.rs::nothing_opened_needs_an_empty_log_and_the_control_logged	str trim/lines/ends_with, format!, a struct of two Strings	1,5,7
```

```tsv
file	test	primary	also
src/recorder.rs	nothing_opened_needs_an_empty_log_and_the_control_logged	recorder-nothing-opened	
```

## 9. Added 2026-10-21: the settings witness

The selftest's forced survey card did not show in 3 runs out of 3 on a loaded machine. The flag
was written 3 s before the turn, and the lab assumed Claude Code had read it. Every lab's scratch
settings now carry a `ConfigChange` hook (`settings_witness`), and `scn::Ctx::arm` writes the flag
until Claude Code reports that it re-read the file. Five new tests, read the same way as §1.

```tsv
id	kind	functions	statement	subsumes	difficulty	blocking_gaps
witness-rereads	contract	settings_witness::rereads	∀ log, settings: rereads(log, settings) = |{l ∈ lines(log): J::parse(l) = Ok(j) ∧ j.hook_event_name = "ConfigChange" ∧ j.file_path = settings}|. So a change to another file, another hook event, a line that is not JSON and an object with no file_path count nothing.	settings_witness.rs::a_reread_is_a_config_change_line_naming_the_settings_file	lines filter count, JSON parse (cross-crate aterm_json), Option chaining	1,5,7,12
witness-until-reread	contract	settings_witness::until_reread	∀ write, seen, every, hang, wait: until_reread returns Ok(Reread{writes: k, after}) only when a read of seen made after the k-th call of write returned and before any later call exceeds the read of seen made once, just after the first call returned (the baseline); while no read exceeds the baseline, write is called again once every has passed on wait's clock since the last call returned, and wait pauses between reads otherwise; once hang has passed on that clock since the first call the result is Err ("no re-read"). Every call is the same write, so a line that lands during the first write (an older change's) is never the answer, a line that lands while a rewrite is made is, and a log that never grows ends at the hang detector after more than one write where every < hang.	settings_witness.rs::an_unanswered_write_is_made_again_until_a_line_answers, settings_witness.rs::a_line_from_before_the_write_ends_or_none_at_all_is_no_reread	closures (FnMut, Fn), a trait-generic clock (Wait: WallWait's Instant and interruptible sleep (effects), SteppedWait in tests), Duration arithmetic, a loop with early returns	1,3,7,8,13
witness-hook-script	refinement	settings_witness::script, settings_witness::hooks, util::shell_quote	∀ script path p, log path g: hooks(p).ConfigChange = [{hooks: [{type: "command", command: shell_quote(p)}]}]; run by /bin/sh with input x on stdin, script(g) appends x ++ "\n" to g when g's directory exists and leaves the file system unchanged otherwise, and exits 0 in both cases (exit 2 would block the change and any other failure would draw an error in the tab). Uses util-shell-quote-unquote-roundtrip.	settings_witness.rs::the_hook_runs_the_script_which_appends_its_input_and_never_fails	format!, J tree construction, a Shell model and a process (effects)	1,3,5,8,10
witness-in-every-lab	contract	lab::scratch_claude_config	∀ run, helper, proj, o: J::parse(scratch_claude_config(run, helper, proj, o).0).hooks has exactly one ConfigChange command hook, and it runs shell_quote(run.run/config-change.sh), never another run's; apiKeyHelper = helper.	lab.rs::a_lab_ups_claude_settings_carry_the_settings_witness_hook	J tree construction, JSON print then parse (cross-crate aterm_json)	1,3,5,12
```

```tsv
file	test	primary	also
src/settings_witness.rs	a_reread_is_a_config_change_line_naming_the_settings_file	witness-rereads	
src/settings_witness.rs	an_unanswered_write_is_made_again_until_a_line_answers	witness-until-reread	
src/settings_witness.rs	a_line_from_before_the_write_ends_or_none_at_all_is_no_reread	witness-until-reread	
src/settings_witness.rs	the_hook_runs_the_script_which_appends_its_input_and_never_fails	witness-hook-script	witness-rereads
src/lab.rs	a_lab_ups_claude_settings_carry_the_settings_witness_hook	witness-in-every-lab	witness-hook-script
```

## 10. Added 2026-10-21: the windowed lab and L13's keys

L13 (a person's own exchange) passed live only through the development seam `send|key as=person`.
`forever --window` opens the lab's instance as a window, in the same jail, and types `land-p`'s first
prompt through `hwkey`, the path a key press takes. A windowed aterm also adds Claude Code's defaults to
the lab's scratch settings, so Claude Code labels its top rule and the composer reader takes a labelled
rule. Five new tests, read the same way as §1.

```tsv
id	kind	functions	statement	subsumes	difficulty	blocking_gaps
lab-windowed-argv	contract	lab::instance_argv, lab::windowed	∀ run, aterm, lifeline: windowed(run) ⇔ run.window = "1". If windowed(run): instance_argv(run, aterm, None, lifeline) = (if lifeline then [run.run/bin/aterm-lab, "keeper", "--"] else []) ++ [aterm, "--no-reroute", "--control-sock", run.sock, "-d", run.proj, "--columns", run.cols, "--lines", run.rows], with no "--headless" and no "--lifeline-fd" (a window refuses both). Otherwise the line starts [aterm, "--headless"] and holds the lifeline flag when lifeline.	lab.rs::a_windowed_lab_s_instance_opens_a_window_its_keeper_holding_the_lifeline	Vec<String> building, PathBuf join, a bool branch	1,5
crash-refuses-window	contract	crash::crash	∀ run with windowed(run), a, ops: crash(run, a, ops) = Err(usage, code 2) and ops records no event (nothing killed, nothing relaunched). So a SIGKILL never hits a window's keeper and leaves its aterm running.	crash.rs::a_windowed_lab_is_never_crashed	an early return before an Ops trait's effects (fakes)	1,3,8
l13-keys-account	contract	forever::cover::judge_theirs, forever::cover::keys_unaccounted, forever::cover::keyed_line	∀ seen with seen.cover.keyed = Some(k) and a numeric person_stamp: keys_unaccounted(k) = None ⇔ k.seq_after = Some(a) ∧ a ≥ k.seq_before ∧ a − k.seq_before = k.posted ∧ k.posted > 0; where it is Some(why), judge_theirs(seen, t0) = Unobserved(why …), whatever the harness did in the exchange. Where it is None, judge_theirs judges the exchange exactly as for the seam (an act in it is a FAIL), and a PASS opens with keyed_line(k). With keyed = None a PASS opens with "typed through the `as=person` seam".	forever_tests.rs::land_p_typed_through_hwkey_passes_only_where_the_keys_account_for_the_stamp, forever_tests.rs::land_p_s_prompt_is_typed_key_by_key_through_hwkey	Option/checked_sub arithmetic, format!, a judge over journal, ledger and status records	1,5,7
composer-labelled-rule	contract	screen::is_rule, screen::composer_row, screen::composer_of	∀ row r: is_rule(r) ⇔ count(trim(r)) ≥ 40 ∧ (every char of trim(r) ∈ {─, ━} ∨ aterm_phase::phase::is_rule(trim(r))). So Claude Code's top rule with the top-effort mode's label inside it (`──── <name> ─`, which a windowed aterm's defaults turn on) frames the composer as a bare rule does, while a short labelled run, a rule with a table's joint (┬) and a row of words frame nothing.	screen.rs::claude_code_s_labelled_top_rule_frames_its_composer	char counting, a delegated predicate	1
```

```tsv
file	test	primary	also
src/lab.rs	a_windowed_lab_s_instance_opens_a_window_its_keeper_holding_the_lifeline	lab-windowed-argv	
src/crash.rs	a_windowed_lab_is_never_crashed	crash-refuses-window	
src/forever_tests.rs	land_p_typed_through_hwkey_passes_only_where_the_keys_account_for_the_stamp	l13-keys-account	
src/forever_tests.rs	land_p_s_prompt_is_typed_key_by_key_through_hwkey	l13-keys-account	
src/screen.rs	claude_code_s_labelled_top_rule_frames_its_composer	composer-labelled-rule	
```

## 11. Added 2026-10-21: the review's gaps, closed

A review of `b9/lab-hygiene` found behaviour no test held: the refusal of a windowed lab under
`LAB_RESTORE=1`, the windowed launch's readiness wait, the survey arm's use of the settings witness
(an arm that wrote and slept 3 s passed every test), and the 40-column floor of a labelled rule.
`survey --reread` is new: the arm's write, from the command line. Five new tests, read the same way as
§1; `claude_code_s_labelled_top_rule_frames_its_composer` (§10) now also holds the floor.

```tsv
id	kind	functions	statement	subsumes	difficulty	blocking_gaps
lab-opts-refused	contract	lab::opts_refused, lab::up, forever::lab_opts	∀ o: opts_refused(o) = Some(why) ⇔ o.window ∧ o.restore. up(tag, o) with opts_refused(o) = Some(why) returns Err(Fail{code: 2, msg: why}) before it judges tag or makes anything; lab_opts(base, mode, keep, (surfaces, window)) returns Err(Fail{code: 2, msg: "forever: " ++ why}) where the options it builds are refused, so `forever --window` under LAB_RESTORE=1 is a usage error, never a harness's COULD NOT RUN; otherwise it returns base with trust, lifeline = ¬keep, window, the person's seam iff surfaces, and the cap's rate iff mode = Cap.	lab.rs::a_windowed_lab_under_the_restore_seam_is_refused_before_anything_is_made, forever_tests.rs::forever_window_under_the_restore_seam_is_a_usage_error_not_could_not_run	a bool conjunction, early returns, struct update	1,5
lab-window-ready	contract	lab::until_answered	∀ answers, alive, hang, pause: until_answered returns Ok(()) iff some ask of answers returns true; each round asks answers, then alive, then the clock: Err("… ended before its control socket answered") when alive is false first, Err("… hang detector") once hang has passed, and pause's Err unchanged (an interrupt). So an answer stands even when the child has ended since.	lab.rs::a_window_is_ready_once_its_socket_answers_and_never_on_a_dead_child_or_a_hang	closures (FnMut), Instant arithmetic, a loop with early returns	1,3,7,8
arm-awaits-witness	contract	scn::Ctx::arm, scn::Ctx::arm_with_wait, settings_witness::set_env_reread_with_wait	∀ ctx: ctx.arm() returns Ok only after the witness's log holds a ConfigChange line naming ctx.run.settings() that was appended after a write of CLAUDE_FORCE_DISPLAY_SURVEY = "1"; the flag stands in the settings' env, and sumd.arm_writes counts the writes made (more than one where the first went unanswered). Where no such line comes within ctx.witness_pace.1 the result is Err(Failed("the survey flag was not armed: …no re-read…")) and arm_writes is not set. Uses witness-until-reread and witness-rereads.	scn.rs::the_survey_arm_returns_only_once_claude_code_reported_a_reread	a SteppedWait whose pauses step the clock and stand in for the watcher on the arm's thread, the file system (effects), J summary	1,3,8,13
survey-words	contract	verbs::survey_words	∀ words: survey_words(words) = Ok((Some("1"), r)) ⇔ words = ["on"] ++ (r ? ["--reread"] : []); Ok((None, r)) likewise for "off"; every other list of words is Err("survey: on|off [--reread]").	verbs.rs::survey_takes_on_or_off_and_reread_after_it	slice patterns	1
```

```tsv
file	test	primary	also
src/lab.rs	a_windowed_lab_under_the_restore_seam_is_refused_before_anything_is_made	lab-opts-refused	
src/forever_tests.rs	forever_window_under_the_restore_seam_is_a_usage_error_not_could_not_run	lab-opts-refused	
src/lab.rs	a_window_is_ready_once_its_socket_answers_and_never_on_a_dead_child_or_a_hang	lab-window-ready	
src/scn.rs	the_survey_arm_returns_only_once_claude_code_reported_a_reread	arm-awaits-witness	witness-until-reread
src/verbs.rs	survey_takes_on_or_off_and_reread_after_it	survey-words	
```
