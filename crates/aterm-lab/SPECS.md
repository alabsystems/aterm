<!-- Copyright 2026 Andrew Yates -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# aterm-lab: spec recovery from its unit tests

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

- `subsumes` names tests as `<file>::<test>` under `crates/aterm-lab/src`; §13 and §14 give the
  path from the crate root (`src/…`, `tests/…`), and §14 groups one file's tests as
  `file::{a,b}`. Functions are `module::fn`. `Uses X` and `Composes X` cite spec X as a lemma.
- `lines(s)` = `str::lines(s)`; `words(s)` = `split_whitespace(s)`; `trim`, `trim_end` and
  `trim_start` as in std; `chars(s)` = the char sequence; `|x|` = length (chars unless marked
  `_bytes`); `take(n, s)` = the first n chars; `++` = concatenation.
- `kv(t)` = the words `k=v` of t, split at the first `=`, in order (`screen::kv`).
- `Regex(p)` = the regex `aterm_regex` compiles from p.
- `Sh(s)` = the POSIX shell value of one word `s` (single quotes literal; double quotes with
  `\`-escapes; bare backslash escapes; bash `$'…'`). **Shell model** = a Lean/`ty` model of POSIX
  word expansion with no `$`-expansion except the literal `$PATH`.
- Reference models: `PyJSON(v, style)` = Python's `json.dumps(v, indent, separators,
  ensure_ascii)`; `PyRepr(s)` = Python `repr(str)`; `PyFloatRepr(f)` = Python `repr(float)`;
  `RFC7396(t, p)` = JSON Merge Patch.
- `J` = `crate::pyjson::J` (`Null|Bool|Int|Float|Str|Arr|Obj`, `Obj` an ordered association
  list); `J::parse` reaches `aterm_json`.
- `Prot(home, cc)` = `protected::protected_paths(home, cc)`, a list of
  `(Kind ∈ {Prefix, Subpath, Literal}, path)`. `R(x)` = `fs::canonicalize(x)`.
- **Fs model** = an abstract file system (inodes, link kinds, nlink, uid, mode, mtime).
  **Process model** = a kernel process table with exit events and reaping.
- `blocking_gaps` are rows of `docs/TRUST-LANE.md` (Blocking gaps):
  1 non-scalar carriers, 2 `&self`/`&mut` views, 3 E6 body widening, 5 quantified clauses,
  6 plain-Rust clause publication, 7 loops/iterators, 8 effects, 10 model refinement, 11 generics,
  12 cross-crate citation, 13 threads/closures/higher-order. Gap 0 (the seal) blocks every row; the
  tables before §12 leave it out. Gap 14 (the Clean elaborator) applies to every Lean-written
  reference model and is not listed.

## 1. Specs (TSV)

`subsumes` lists the tests the spec retires as their primary spec (secondary ones are in §2);
`difficulty` is the Rust the proof must handle.

```tsv
id	kind	functions	statement	subsumes	difficulty	blocking_gaps
cold-retirement-evidence	contract	cold_retirement::{lapsed,exercise,freeze_after_reaping,expire}	A physical cold-retirement run signals only birth-matched children of its own lab, stops the shell after reaping and foreground recovery, observes a saved native-lapsed record after the exact admitted operation's deadline with no retained binding, prepare, hold or account debt, and requires one new agent to handle a prompt on the original conversation under the same host. Malformed or still-held records cannot satisfy the lapse observation.	1/1	medium	1,2,8
codex-safe-args	contract	codex::safe_args	∀ tab: bool, args: [String]: safe_args(tab,args) = Ok(()) ⇔ ∀ a ∈ args: a ∉ {logout, app, app-server, remote-control, agents, exec-server, cloud, update} ∧ (a = "login" ⇒ ¬tab ∧ join(args," ") ∈ {"login status","login --help","login -h"}). On Err the message names the first offending argument in order (the login message for "login", the quoted word otherwise).	codex.rs::a_login_or_a_server_never_runs_in_the_lab	&[String], join, for loop with early return, match on &str, Result<(),String>, format!	1,3,5,7
codex-config-text	contract	codex::config_text, codex::q	∀ mode, port, proj, proj_spelled, model: let T = config_text(…). (a) T ends in '\n' and its first line is "# aterm-lab codex (crates/aterm-lab), mode " ++ mode ++ ": …". (b) Read as TOML (q is a correct TOML basic-string quoter for every input): model_provider = "lab" ⇔ mode ≠ "signed-out"; model = model ⇔ model ≠ ""; chatgpt_base_url = "http://127.0.0.1:"++port++"/backend-api/" and [features].apps = false ⇔ mode = "chatgpt"; [model_providers.lab] exists ⇔ mode ≠ "signed-out", with base_url = "http://127.0.0.1:"++port++"/v1", wire_api = "responses", and requires_openai_auth = true if mode = "chatgpt" else env_key = "LAB_CODEX_KEY"; analytics/feedback/otel/tui disabled. (c) The [projects."…"] keys = {proj, proj_spelled} (one table when equal), each trust_level = "trusted".	codex.rs::the_config_names_its_login_and_trusts_the_project_both_ways	String building, Vec<String> extend/sort/dedup, str::replace, format!, a TOML reading model	1,3,5,7,10
codex-auth-json-lab-jwt	round-trip	codex::auth_json, stub::lab_claims, stub::auth_label, util::b64url, util::b64url_decode, util::utc_stamp, pyjson::J::dump, pyjson::J::parse	∀ label, account, plan, now: i64 (now + 30·86400 not overflowing): j = J::parse(auth_json(label,account,plan,now)) is Ok; tokens.refresh_token = "lab-refresh-" ++ label; last_refresh = utc_stamp(now); access = tokens.access_token has the shape H.P."bGFiLW5vdC1zaWduZWQ"; lab_claims("Bearer " ++ access) = Some(c) with c.lab = label, c.iat = now, c.exp = now+2592000, c["https://api.openai.com/auth"].chatgpt_plan_type = plan and .chatgpt_account_id = account; auth_label(Some("Bearer " ++ access)) = "jwt:" ++ take(24, filter([A-Za-z0-9_-], label)). The same holds for id_token, which also carries email = "user@example.com". Composes stub-auth-label, util-b64url-roundtrip, util-utc-stamp, pyjson-dumps-refines-python.	codex.rs::the_composed_login_is_a_jwt_the_stub_reads_by_its_label_only	closures, J tree construction, JSON print then parse (cross-crate aterm_json), base64url, split, Option chaining	1,3,5,7,12,13
stub-auth-label	contract	stub::auth_label, stub::lab_claims, stub::oauth_label, stub::labelled_oauth_token	∀ a: Option<&str>: let v = a.unwrap_or(""), tok = trim(v[7..]) if v[..7] case-insensitively = "bearer " else trim(v). auth_label(a) = "none" if v = ""; else "key" if tok = LAB_CODEX_KEY; else "gemini-key" if tok = LAB_GEMINI_KEY; else "oat:" ++ l if oauth_label(tok) = Some(l); else "oat" if tok starts "sk-ant-oat01-LABLABLAB"; else "api-key" if tok starts "sk-ant-api03-LABLABLAB"; else "jwt:" ++ take(24, filter([A-Za-z0-9_-], c.lab)) if lab_claims(v) = Some(c); else "other(" ++ |chars(tok)| ++ " chars)". oauth_label(tok) = Some(l) ⇔ tok = LABELLED_OAUTH ++ l ++ "-LAB" ++ r with l the text before the first "-LAB" after the head, 1 ≤ |l| ≤ 24 and l ∈ [a-z0-9_-]+; labelled_oauth_token(l) = LABELLED_OAUTH ++ l ++ "-LAB" ++ "A"×74, so ∀ l of that shape: auth_label(Some("Bearer " ++ labelled_oauth_token(l))) = "oat:" ++ l. The output never contains tok unless tok is a fixed lab key (a credential is never echoed; a labelled token gives its label alone). lab_claims(v) = Some(j) ⇔ tok splits on '.' into exactly 3 parts and the middle b64url-decodes to a JSON object j with a string field lab.	(secondary only; see §2)	Option<&str>, str::get char-boundary slicing, eq_ignore_ascii_case, filter/take/collect, JSON parse	1,3,5,7,12
codex-ratelimit-headers	contract	codex::ratelimit_headers, pyjson::py_float	∀ args: (a) args = [] ∨ args[0] = "off" ⇒ Ok(Obj[]). (b) else |args| < 2 ⇒ Err. (c) else for (slot,used,window,reset) ∈ [(primary,args[0],300,3600),(secondary,args[1],10080,432000)]: used = "-" adds nothing; used parses as f64 v ⇒ adds x-codex-<slot>-used-percent = py_float(v), -window-minutes = window, -reset-at = "@NOW+"++reset++"@" in that order; otherwise Err. Then each args[i≥2] = k=v adds headers by k ∈ {credits (has-credits = v∈{yes,unlimited}, unlimited = v=unlimited), balance, reached, promo, active}; any other k ⇒ Err. Header order = argument order.	codex.rs::ratelimits_name_each_window_and_the_credits	&[String], f64 parse, Vec<(String,J)>, for over an array literal, match on &str, Result	1,3,5,7
launcher-argv	refinement	codex::launcher, lab::launcher, lab::launch_words, clock::vars, util::shell_quote, util::shell_prepend_path	∀ run, tab, mode, envs, args (codex) and ∀ run, l: Launch (claude): the script is "#!/bin/sh\n" ++ ("clear\n" iff tab; always for claude) ++ one exec command whose word expansion under the Shell model is exactly the argv. codex: [env, -u, OPENAI_API_KEY, -u, CODEX_API_KEY, -u, OPENAI_BASE_URL, PATH=<run>/shim:$PATH, HOME=<w>/home, CODEX_HOME=<w>/codex-home, the six proxy vars = proxy, NO_PROXY/no_proxy, LAB_SECURITY_LOG, LAB_OPEN_LOG, BROWSER, the two token-URL overrides, LAB_CODEX_KEY=lab-not-a-key iff mode = "lab"] ++ envs ++ [codex_bin] ++ ([--no-daemon] iff takes_no_daemon(codex_bin): codex-alab's package, its ALab build mark beside the manifest that names it, or any other binary whose bytes hold the literal no-daemon (a codex-alab dev build or copy, vendor 0.163.0's release package, whatever its version); a binary whose bytes do not hold it (vendor 0.151.0, which refuses the flag) goes without) ++ args. claude: ([<run>/relay/node, --log, <w>/relay.log, --] iff l.relay) ++ [env] ++ ([-u k for k ∈ PROXY_KEYS] iff l.stub_route_alone) ++ [PATH=<run>/shim:$PATH] ++ ([CLAUDE_CONFIG_DIR=<w>/stored-login (stored_login_dir) if l.stored, else CLAUDE_CONFIG_DIR=<w>/home/.claude] unless l.own: an --own launch names none, so its Claude Code reads the scratch HOME's own .claude and .claude.json) ++ [ANTHROPIC_BASE_URL=http://127.0.0.1:<port>] ++ ([HTTP_PROXY, HTTPS_PROXY, http_proxy, https_proxy, ALL_PROXY = proxy] unless l.stub_route_alone) ++ [NO_PROXY=127.0.0.1,localhost, no_proxy=127.0.0.1,localhost, DISABLE_AUTOUPDATER=1] ++ [LAB_SECURITY_LOG=<w>/security.log, LAB_OPEN_LOG=<w>/open.log, BROWSER=<run>/shim/open] ++ clock::vars(run) ++ l.envs ++ [CLAUDE_CODE_NO_FLICKER=1 iff relay] ++ [CLAUDE_CODE_OAUTH_TOKEN=<fake> iff subscriber] ++ ["claude" if l.by_name else claude_bin] ++ ([--settings, tap::settings_value(run.aterm)] iff l.own, which launch_claude refuses where the tap cannot name run.aterm) ++ ([--debug-file, <debug_log>] iff l.debug_log) ++ l.args (revised 2026-11-08: the row said every launch carried CLAUDE_CONFIG_DIR=<w>/home/.claude, wrong for --stored and --own, gave --own's --settings pair no place, and said a stub-route-alone launch kept only DISABLE_AUTOUPDATER=1 of the third line, where it keeps NO_PROXY and no_proxy too). So the executed program is the binary by path (unless l.by_name) and every user word survives as one argv element; and for both, LAB_OPEN_LOG and BROWSER are env's assignments (before the program), so every open the agent makes (`$BROWSER <url>`, or a bare-name `open` that the shim leading PATH answers) is the recorder's and goes to <w>/open.log. NEGATIVE CONTROL (claude): a line without LAB_OPEN_LOG or BROWSER, or with LAB_OPEN_LOG as an argument after the program, fails it. Uses util-shell-quote-unquote-roundtrip.	codex.rs::the_launcher_execs_codex_by_path_with_no_daemon_where_it_is_taken, lab.rs::claude_options_parse_and_the_launcher_execs_the_binary_by_path, lab.rs::an_own_launch_is_the_default_homes_login_with_the_tap_on_its_line	format! string building, PathBuf join/display/to_string_lossy, for over &[String], a Shell model	1,3,5,7,10
lab-parse-launch	contract	lab::parse_launch, clock::is_clock_var	∀ args: parse_launch(args) = Ok(l) ⇔ args matches ("-e" V | "--no-wait" | "--prepare-only" | "--relay" | "--subscriber" | "--stored" | "--own" | "--debug-log" | "--by-name")* ["--" R*] with ¬clock::is_clock_var(key_of(V)) for every V and at most one of the logins "--subscriber", "--stored", "--own"; then l.envs = the V's in order, l.no_wait/prepare_only/relay/subscriber/stored/own/debug_log/by_name = whether each flag occurs, l.stub_route_alone = l.stored ∨ l.own, l.args = R* (everything after the first "--", verbatim, including further flags). Err when a token outside the grammar appears before "--", "-e" is last, "-e" names a sleep-clock variable, or two logins are given (revised 2026-11-08: `--stored` and `--own` were missing).	(secondary only; see §2)	slice iterator with by_ref, match on &str, Vec push/extend, Result<_, String>	1,3,7
lexical-validators	contract	door::lab_socket_shape, paths::valid_tag	∀ s: lab_socket_shape(s) ⇔ ∃ n: s = "/tmp/at." ++ n ++ "/a.sock" ∧ n ≠ "" ∧ '/' ∉ n. ∀ t: valid_tag(t) ⇔ t ≠ "" ∧ |t|_bytes ≤ 24 ∧ ∀ c ∈ t: c ∈ [A-Za-z0-9_-].	door.rs::only_a_lab_shaped_socket_passes, paths.rs::tags_are_short_plain_words	strip_prefix/strip_suffix, Option::is_some_and, chars().all	1,5,7
door-wire-pattern	contract	door::wire_pattern	∀ p: wire_pattern(p) escapes every ASCII whitespace char and '=' as "\\x" ++ hex2(c), folding a preceding unescaped '\\' into the hex escape, so the output holds no ASCII whitespace and no '='. Semantic clause: for every valid Regex(p), Regex(wire_pattern(p)) and Regex(p) denote the same language.	door.rs::a_pattern_crosses_the_wire_as_one_token	chars loop, String push/pop/push_str, format!("{:02x}"), char as u32; regex semantics	1,5,7,12
footers-mode-word	contract	footers::mode_word, aterm_phase::anchor	∀ screen: let row = the LAST line of screen containing "⏵⏵ " or "⏸ ". mode_word(screen) = None if no such row; else the word w of the FIRST entry (glyph, id, w) of the fixed table [bypass, auto, accept-edits, dont-ask (⏵⏵); plan, manual (⏸)] such that row contains glyph ++ " " ++ anchor(id); None if no entry matches.	footers.rs::the_mode_row_names_its_mode	lines().rev().find, array-literal iterator find, format!, &'static str result, cross-crate anchor table	1,5,7,12
footers-mode-row-pattern	contract	footers::mode_row_pattern, aterm_phase::anchors::guard_regex	∀ screen: let row = the LAST line of screen containing "⏵⏵ " or "⏸ " (mode_word's row). mode_row_pattern(screen) = None if no such row; else Some(guard_regex(r)) with r = trim of row, and Regex(guard_regex(r)) matches r (guard_regex is a literal pattern: metacharacters escaped, whitespace as '.'); a row under it (Claude's effort hint wrapped onto a row of its own) is never r, so a press that changes the mode and leaves that row standing is seen to change the mode row.	footers.rs::the_mode_row_is_the_pattern_a_press_waits_on	lines().rev().find, Option::map, cross-crate regex builder	1,5,7,12
footers-done-row-pattern	equivalence	footers::DONE_ROW, screen::is_done_row, aterm_regex::Regex::is_match	∀ r: &str: Regex(DONE_ROW).is_match(r) ⇔ Regex(wire_pattern(DONE_ROW)).is_match(r) ⇔ is_done_row(r). is_done_row itself is specified in screen-idle-status.	footers.rs::the_done_row_pattern_reads_what_the_done_row_reader_reads	strip_prefix, match_indices, char-boundary slicing, a regex semantics model	1,5,7,12
footers-opts-parse	contract	footers::Opts::parse, footers::Opts::variant	∀ args: Opts::parse(args) = Ok(o) ⇔ args matches ("--out" V | "--no-turn" | "--no-bypass" | "--columns" V | "--lines" V | "--subscriber" | "--settings" V)* with every --settings V parsing as a JSON object; o = the defaults (turn, bypass true; cols 80; lines 24; subscriber false; settings None) overridden by the flags in order (last wins). Every Err has code 2 (usage). variant(o) = "settings " ++ dump(settings, COMPACT) if settings is Some, else "the lab's default settings".	footers.rs::the_variant_is_parsed_and_named	slice iterator with a capturing FnMut closure, match on &str, JSON parse, Result<_, Fail>, PathBuf (from_caller reads the cwd: an effect)	1,3,7,8,12,13
jail-render-rules	contract	jail::render, protected::protected_paths, paths::physical	∀ spec whose run/sockdir/state/user_tmp exist, whose quoted paths are profile_safe, whose mark is valid and whose R(run) is not inside_protected: render(spec) = Ok(p) where (a) the "(deny file-read* file-write* (" lines are exactly one per entry of Prot(home, ccsocks), in order; (b) the "(deny mach-lookup" lines are exactly MACH_DENY ++ MACH_DENY_OPEN ++ [mark]; (c) no '@' template hole survives; (d) file-write* is allowed under R(run)/scratch with a literal rule per SCRATCH_DIRS entry, and xcrun_db and unlink of R(user_tmp) are denied; (e) one trusted-write deny rule lists R(state), every lab_dir subpath, the aterm literal, the repo_target literal and physical(target)/debug and /release subpaths; (f) the unlink-deny parents rule lists every proper ancestor of those trusted paths; (g) for each agent binary b = S/name/build/… under S = home/Library/Application Support/aterm/pkg/store: file-read* is allowed on S/name/build and file-read-metadata on each ancestor from S/name up to home/Library/Application Support/aterm, none above; an agent outside the store gets no read rule; (h) a home under /private/tmp/(at|labprobe).<x>/ gets a file-write* allow on its own subpath.	jail.rs::every_protected_entry_and_mach_service_becomes_one_rule, jail.rs::the_agents_under_test_are_readable_inside_the_store_they_live_in, jail.rs::a_replica_home_in_tmp_is_left_to_its_own_rules	large format! template, Vec<String> joins, closures, Path parent walks, fs::canonicalize (effect)	1,3,5,7,8,13
jail-render-refusals	contract	jail::render, jail::valid_mark, jail::profile_safe, protected::inside_protected	∀ spec: render(spec) = Err(_) if any of run, sockdir, state, user_tmp does not canonicalize; or any quoted path (home, run, claude, sockdir, ccsocks, codex, state, aterm, repo_target and its physical forms, user_tmp, lab_dirs) is not profile_safe (holds '"', '\\' or a control char); or the mark is not valid (message holds "bad mark"); or inside_protected(R(run), home, ccsocks) (message holds "owner's data"). ∀ m: valid_mark(m) ⇔ m = "lab.jail." ++ r ∧ r ≠ "" ∧ ∀ c ∈ m: c ∈ [A-Za-z0-9._-].	jail.rs::a_path_the_profile_cannot_quote_a_bad_mark_or_a_run_in_the_owners_data_is_refused	Result early returns, Vec<&str> any, chars().all, fs::canonicalize (effect), random new_mark when empty (effect)	1,3,5,7,8
lab-managed-bin	contract	lab::managed_claude, lab::managed_bin	∀ name, line: managed_bin(name,line) = Some(last) ⇔ line = name ++ " → " ++ chain ++ " — managed " ++ tail (first occurrence of " — managed "), last = the final " → "-separated segment of chain, and last ≠ ""; else None. managed_claude = managed_bin("claude", ·).	lab.rs::the_managed_claude_is_the_last_path_of_the_arrow_chain	strip_prefix with format!, split_once, rsplit, bool::then	1,3,5
lab-mask-account	contract	lab::mask_account	∀ line: mask_account(line) = the left-to-right rewrite replacing each account="V" (V up to the next '"') with account="<user>"; an occurrence with no closing quote is replaced and the rest of the line dropped. Hence no original V survives, mask_account is idempotent, and a line without account=" is unchanged.	lab.rs::the_tripwire_log_is_kept_with_its_account_masked	while-let over str::find, byte-index slicing, push_str	1,5,7
nudge-differing-spans	contract	nudge::differing, nudge::spans	∀ a, b: [String]: differing(a,b) = [i ∈ 0..max(|a|,|b|) ascending | trim_end(a.get(i) or "") ≠ trim_end(b.get(i) or "")]; so differing(a,a) = [] and differing is symmetric. ∀ strictly increasing rows: spans(rows) = join(",", [a if a = b else a ++ "-" ++ b | (a,b) maximal runs of consecutive integers]), and parsing that output back yields rows.	nudge.rs::screens_differ_by_the_rows_whose_text_differs	range filter collect, map_or on slice get, Vec<(usize,usize)> last_mut, join	1,2,5,7
nudge-parse-resizes	contract	nudge::parse_resizes, nudge::hget, screen::kv	∀ stdout, stderr: parse_resizes(stdout, stderr) = (head, epochs) with head = kv(first line of stderr containing "render=") or [] when none; epochs = [Epoch{id, shift = num("shift"), pushed = num("pushed"), fields = kv(rest)} | line ∈ lines(stdout), line = "epoch " ++ id ++ (" " ++ rest)?, id parses as an integer], num(k) = the first value of k in fields parsed, else 0; order kept. hget(h,k) = the first v with (k,v) ∈ h. Uses screen-kv-field.	nudge.rs::resizes_reads_its_header_and_epochs	lines filter_map, split_once, parse::<i64>, closures, Vec of tuples	1,3,5,7,13
nudge-configured-nudge	contract	nudge::configured_nudge	∀ toml: configured_nudge(toml) = Some(lowercase(trim(v))) for the FIRST line l inside the `[render]` table with trim_start(l) = "repaint_nudge" ++ ws* ++ "=" ++ ws* ++ '"' ++ v ++ '"' ++ … (v up to the next quote); None when `[render]` has no line of that shape.	nudge.rs::the_configured_nudge_is_the_render_tables_word	lines loop, table-header tracking, strip_prefix chain, to_lowercase	1,5,7
nudge-judge-verdict	contract	nudge::judge, nudge::differing	∀ expect ∈ {on, off}, m: Trial: judge(expect, &mut m) sets m.shot_rows = max(|differing(shot,before)|, max{n | (_,h,n) ∈ frame_rows, h = rows}), m.end_rows = differing(after,fresh), m.reference_rows = differing(fresh,before), and returns ("INCONCLUSIVE", [why]) with exactly one reason for the FIRST failing gate in the order: error set; |reference_rows| > SLACK; epoch None; epoch class ≠ agent; shift = 0 ∧ pushed = 0; ¬(verdict = desync-risk ∨ by = nudge); shot_rows ≤ SLACK; status_render = "-". Otherwise ("PASS", []) iff every check of the expect-mode list holds (on: healed by=nudge, render ok on both reads, exactly one nudge/hold/healed line, the told sizes; off: the nudge never acted, …), else ("FAIL", bad) with bad = the failed checks in order, non-empty.	nudge.rs::a_healed_trial_passes_on_and_fails_off, nudge.rs::a_trial_that_never_displaced_is_inconclusive	&mut struct with Vec<String> fields, Option<Epoch> clone, closures capturing &mut Vec, iterator chains, format!	1,2,3,5,7,13
nudge-log-line	contract	nudge::nudge_log_line	∀ l: let a = the suffix after the first "aterm_gui::app_nudge: " (or l), a' = a without a leading "repaint nudge ". nudge_log_line(l) = join(" ", [w ∈ split(a', ' ') | ¬∃ p ∈ {session=, reason=, epoch=}: w = p ++ x ∧ x ∈ [A-Za-z0-9_]*]).	nudge.rs::a_nudge_log_line_is_shown_without_its_prefix_and_ids	find + slicing, split/filter/collect/join, closures	1,5,7
paths-forgotten	contract	paths::forgotten	∀ key: forgotten(key) ⇔ key ≠ aterm_toolchain::waits::VAR ∧ (key ∈ {AI_AGENT, CLAUDECODE, CURL_HOME} ∨ (key = h ++ "_" ++ t at the first '_' with h ≠ "" ASCII-alphabetic and t ASCII-case-insensitively = "proxy") ∨ ∃ p ∈ FORGET_PREFIXES: key = p ++ r ∧ r ∈ [A-Za-z0-9_]*).	paths.rs::forgets_agent_and_aterm_variables_and_keeps_the_rest	split_once, chars().all, eq_ignore_ascii_case, iteration over a const slice	1,5,7
paths-physical	contract	paths::physical	∀ path, Fs: let h be the longest prefix of path (by components) such that canonicalize(h) succeeds and is a directory. physical(path) = canonicalize(h) joined with the remaining components of path, unchanged; = path itself when no non-empty, non-'.' prefix qualifies. physical creates nothing (Fs unchanged).	paths.rs::physical_resolves_the_existing_head_and_keeps_the_rest	PathBuf pop/push, OsString, loop, fs::canonicalize and is_dir (effects)	1,7,8
probe-op-codec-roundtrip	round-trip	probe::Op::encode, probe::Op::decode, probe::esc_field, probe::unesc_field, probe::plan	∀ op: Op: Op::decode(op.encode()) = Some(op), and op.encode() holds no '\n' or '\r' (Serve(None) encodes as "serve", Serve(Some(d)) as "serve\t" ++ esc_field(d), and esc_field/unesc_field escape '\\', '\t', '\n', '\r'), so any sequence [op_i] written one per '\n' line decodes line-by-line to [Some(op_i)]. Op::decode rejects raw '\n'/'\r', unknown verbs/modes, wrong field counts, and invalid '\\' escapes.	probe.rs::every_operation_crosses_into_the_jail_unchanged	enum with String/bool/enum payloads, match, split('\t') into Vec, backslash escape state machine, Option ?, format!	1,3,5,7
probe-replica-plan-coverage	contract	probe::plan, protected::protected_paths	∀ w with w.replica: let P = plan(w, decoy, dirs). #{c ∈ P : c.op ∈ {Reach, Write}, c.label starts with "<replica>"} ≥ 2·|Prot(w.home, w.cc)|; #{c : c.op = Mach(_)} = |MACH_DENY| + |MACH_DENY_OPEN|; for every n ∈ SCRATCH_DIRS a check labelled "move scratch/<n> (the lab writes and reads through it)"; a Denied list check of <replica>/Library/Application Support/aterm; and a metadata-only stat check of every store directory above the Codex build (…/pkg/store/codex).	probe.rs::the_replica_plan_covers_every_protected_path_and_every_mach_service	Vec<Check> building, matches!, format! labels	1,3,5,7
probe-judge-table	table-property	probe::judge	∀ replica, c, inside, control (i = inside or "none"): want = Allowed ⇒ (Ok iff i = "yes", else Fail). want = Denied: i = "yes" ⇒ Fail; i = "EPERM" ⇒ (control ∈ {None, Some("yes")} ⇒ Ok; else replica ⇒ Fail; else Unproven); i = "ENOENT" ∧ c.absent_ok ∧ ¬replica ⇒ Absent; otherwise Fail. The message is empty exactly when the status is Ok or Absent.	probe.rs::a_denial_counts_only_where_the_control_got_through	enum equality, Option<&str>, match with guards, format!	1,3
procs-descendants-in	contract	procs::descendants_in	∀ table: [(pid, ppid)], root: root ≤ 1 ⇒ descendants_in(table, root) = []. Otherwise, for an acyclic table, the output is the post-order walk of root's child forest (children in table order, each child's descendants before it): a permutation of all strict descendants of root in which every pid precedes its parent, with no pid outside the subtree. For a cyclic table the visited-pid set ensures each reachable pid is visited once and the walk terminates.	procs.rs::descendants_come_children_first_and_never_for_init	BTreeMap<i32, Vec<i32>>, BTreeSet<i32>, iterative DFS stack	1,3,5,7,11
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
run-env-sh-roundtrip	round-trip	run::Run::render, run::Run::parse, run::Run::pairs, run::Run::fixture_prefix, run::env_records, util::shell_quote, util::shell_unquote	∀ r: Run whose string and path fields are valid UTF-8 and whose agent ≠ "" (render writes an empty agent as "claude"; §5.2): Run::parse(r.render()) = r (env_records splits on unquoted, unescaped newlines so multi-line quoted values round-trip without spoofing keys). ∀ r: fixture_prefix(r) = a ++ "-" ++ (ver(a) or "?"), with a = r.agent if r.agent ∈ {codex, agy, muse, grok, kimi} else "claude", and ver(a) that agent's version field. Uses util-shell-quote-unquote-roundtrip.	run.rs::env_sh_round_trips_every_field	struct with String/PathBuf/i32 fields, derive(PartialEq), quote-aware record splitting, split_once, match on keys, int parse, the shell quoting lemma	1,3,5,7
util-shell-quote-unquote-roundtrip	round-trip	util::shell_quote, util::shell_unquote	∀ s: shell_unquote(shell_quote(s)) = s, and shell_quote(s) is one POSIX word with Sh(shell_quote(s)) = s (bare iff s ≠ "" and every char ∈ [A-Za-z0-9@%+=:,./_-], else single-quoted with ' spelled '\''). ∀ w that is one shell word built from single quotes, double quotes, backslash escapes and $'…' (with \n, \t, \\, \', \" and \xNN escapes): shell_unquote(w) = Sh(w); in particular it reads what printf %q wrote.	util.rs::shell_words_round_trip_through_quote_and_unquote, run.rs::reads_the_env_sh_printf_q_wrote	Peekable<Chars> state machine, nested while-let loops, String::replace	1,5,7,10
sampler-endpoints	contract	sampler::endpoints	∀ lsof -F text f whose lines start with an ASCII tag char: endpoints(f) = the set of strings cmd[pid] proto kind remote ++ ("" if host ∈ {127.0.0.1, ::1, localhost, *, ""} else "  EXTERNAL"), one per 'n' line, where pid/cmd/proto are the latest p/c/P values before it, kind = conn iff the name contains "->" (remote = the part after it) else listen (remote = the name), remote is cut at " (", and host = remote without its trailing :port (port all digits or '*') and without brackets. The ASCII-tag precondition is load-bearing (§5.4).	sampler.rs::loopback_endpoints_are_plain_others_are_external_and_each_names_its_pid	BTreeSet<String>, lines loop with mutable String state, split_at, rfind, replace with a char array	1,3,5,7
scn-put-journal-order	contract	scn::put_journal, scn::dismissed_in, scn::Ctx::put	∀ ctx, jl: after put_journal(&mut ctx, jl), the keys of ctx.sumd end with […, "journal", "dismissed_journal"] in that order (a key already present keeps its position, otherwise it is appended), sumd.journal = jl, and sumd.dismissed_journal = Bool(∃ e ∈ jl: e is an array whose [2] is a string containing "DISMISSED").	scn.rs::the_journal_goes_before_whether_it_holds_a_dismissal	&mut struct holding an ordered J object, any, Option chaining	1,2,3,5,7
scn-cycle-stats-judge	contract	scn::cycle_stats, scn::judge_cycle, screen::is_idle, screen::composer_of	∀ frames (wall-ordered), journal, t_turn: d = cycle_stats(frames, journal, t_turn) has t_idle = wall of the first idle frame with wall ≥ t_turn, t_card = the first card frame with wall ≥ t_turn; if a card: t_zero = the first later card frame whose composer is a non-empty run of '0', t_gone = the first later frame with no card, composer_states = the composer values of frames from the card on with consecutive duplicates collapsed, composer_after = the last frame's composer; t_journal_dismissed = rec_t of the last journal record whose line contains "DISMISSED survey"; each delta (idle_to_card, card_to_zero, zero_to_gone, card_to_gone, gone_to_journal) = round3(y - x) when both stamps are present and non-zero, else Null. judge_cycle(d) = ["no survey card was drawn"] if t_card is falsy; else the list, in order, of: card never went (t_gone falsy), no DISMISSED journaled, composer_after ≠ Str(""). Uses screen-idle-status and screen-composer.	scn.rs::the_cycle_is_measured_from_frames_and_journal_and_judged	slices of structs, find/filter, Vec<Option<String>> dedup, f64 arithmetic and rounding, J object building	1,3,5,7,13
scn-frame-line	contract	scn::frame_line, scn::frame_key, util::py_repr	∀ f: Frame: frame_line(f) = "FRAME seq=" ++ f.seq ++ " T=" ++ (t or "None") ++ " card=" ++ (1 or 0) ++ " composer=" ++ repr_opt(composer) ++ " status=" ++ py_repr(take(50, status or "")) ++ ("" if no rating rows else " rows=[" ++ join(", ", map(py_repr, rating_rows)) ++ "]"), with (card, composer, status, rating_rows) = frame_key(f).	scn.rs::a_frame_line_says_what_changed_in_python_repr	tuple destructuring, chars().take, map/join, format!	1,3,5,7
screen-survey-rows	contract	screen::is_card_row, screen::is_options_row	∀ r: is_card_row(r) ⇔ r = "●" ++ c ++ rest with c one whitespace char and rest starting with CARD (the survey.question anchor). is_options_row(r) ⇔ r = w1 ++ "1: Bad" ++ w2 ++ "2: Fine" ++ w3 ++ "3: Good" ++ w4 ++ "0: Dismiss" ++ tail with each wi a NON-EMPTY whitespace run (any tail).	screen.rs::the_card_and_its_options_are_read_as_drawn	strip_prefix, Chars::as_str, trim_start length comparison, loop over an array	1,5,7,12
screen-composer	contract	screen::composer_of, screen::composer_row, screen::composer_at, screen::is_rule	Let rule(r) ⇔ |chars(trim r)| ≥ 40 ∧ (all chars ∈ {─, ━} ∨ aterm_phase::phase::is_rule(trim r)) (composer-labelled-rule, §10). ∀ rows: composer_of(rows) = None if fewer than two rule rows; else with top, bot the last two rule indices: Some("") if bot = top + 1, else Some(trim_end(strip ' '(strip '❯'(replace(NBSP→' ', rows[top+1]))))). ∀ screen (rows = split('\n', replace(NBSP→' ', s))): composer_row("muse", s), composer_row("agy", s), and composer_row("kimi", s) delegate to aterm_phase::{muse::composer_draft, agy::composer_draft, kimi::composer}(rows) and return the draft's first line; composer_row("codex", s) = None while a row under the last row opening '›' opens '  › ' (a box's cursor at column 2, the question dialog's under the user's message; the lab's own reading, since aterm reads no Codex screen), else Some(trim(trim_start_matches('›', r))) for r the LAST row starting with '›', None if none; composer_row(other, s) = the last-two-rules rule on rows, the first row stripped of '❯' and fully trimmed.	screen.rs::the_composer_row_is_the_first_row_between_the_last_two_rules, screen.rs::a_turn_stands_in_the_agents_own_composer, screen.rs::codexs_question_dialog_is_no_composer, screen.rs::muse_code_s_composer_is_the_caret_row_of_its_frame, screen.rs::kimi_composers_are_read_without_treating_its_trust_prompt_as_input	Vec<usize> collect, slice pattern [.., top, bot], get(range), replace/strip/trim, cross-crate aterm_phase	1,3,5,7,12
screen-idle-status	contract	screen::is_idle, screen::status_row, screen::is_done_row	∀ r: is_done_row(r) ⇔ r = "✻ " ++ rest ∧ ∃ i: rest[i..] starts with " for " ∧ rest[i+5] is an ASCII digit. ∀ rows: status_row(rows) = the first trim_start(r) that starts with one of ✻✶✳✢·*✽ and contains '…'; is_idle(rows) ⇔ (∃ r ∈ rows: is_done_row(r)) ∧ status_row(rows) = None.	screen.rs::a_turn_is_idle_on_its_done_row_with_no_spinner	match_indices, char-boundary slicing, any/find	1,5,7
screen-kv-field	contract	screen::kv, screen::field	∀ text, key: kv(text) = [(k, v) | w ∈ words(text), w contains '=', (k, v) = split at the first '='] in order; field(text, key) = Some(v) for the first (key, v) ∈ kv(text), else None.	screen.rs::key_value_words_are_found_by_key	split_whitespace, split_once, filter_map/find	1,5,7
stub-analyse-kind	contract	stub::analyse, stub::text_of	∀ body: bytes: analyse(body).kind = "opaque" if body is not a JSON object; else "compact" if text_of(system) contains "tasked with summarizing conversations" or the last user message's text contains "Your task is to create a detailed summary"; else "quota" if max_tokens is an integer ≤ 1; else "aux" if tools is absent or empty (array or object of length 0); else "main". For an object: stream = truthy(stream), msgs = |messages|, tools = |tools|, last_user = text_of(content) of the last role=user message, model = model or "?". analyse never fails.	stub/tests.rs::a_request_is_classed_by_what_it_asks	JSON parse (cross-crate), struct of &'static str/usize/String/J, Option chains	1,3,5,7,12
stub-tool-turn	contract	stub::tool_turn, stub::is_tool_result	∀ body, script: let calls = the object entries of script (or [script]) with a truthy name; first = the text of the first user message; mine = [c ∈ calls | c.when truthy ∧ first contains str(c.when)]. If mine ≠ []: Some(Call(name(mine[d]), input(mine[d]), d)) with d = #tool-result messages, or Some(Done) when d ≥ |mine|. Else with main = [c | no when, and c has a truthy name, a string text, or a calls list of at least one named call]: step = None initially; for each user message in order, a tool-result increments a defined step and a message whose text contains "toolstart" resets step to 0; the result is None if step is undefined, else Some(Call(main[step]…, step)) for a named step, Some(Calls([(name, fill_sids(input)) of each named call of main[step].calls, in order], step)) for a calls step with no text (its calls made in ONE message, answered by one request, so one step; served as one assistant message with one tool_use block per call at indexes 0…, each with an id of its own, stop_reason tool_use), Some(Say(text, step)) for a text step, or Some(Done) past the end. Non-JSON body ⇒ None. tool_turn keeps no state.	stub/tests.rs::the_tool_script_is_answered_one_call_per_request_from_toolstart, stub/tests.rs::a_call_with_when_belongs_to_the_subagent_whose_prompt_holds_it, stub/tests.rs::a_calls_step_makes_its_calls_in_one_message_and_counts_as_one_step, stub/tests.rs::served_a_calls_step_streams_every_call_in_one_message, stub/tests.rs::a_calls_steps_inputs_have_their_sids_filled	Vec<&J>, closures, Option<usize> fold over messages, enum with String/J/usize payload	1,3,5,7,12,13
stub-split-deltas	contract	stub::split_deltas	∀ text: let d = split_deltas(text). concat(d) = text; d ≠ []; split_deltas("") = [""]; every element but the last has ≥ 12 chars; every element is a concatenation of whole word-plus-trailing-whitespace tokens (a delta never splits a word); and each non-last element is the shortest such prefix reaching 12 chars.	stub/tests.rs::deltas_rebuild_the_text_and_each_but_the_last_holds_twelve_characters	Vec<char> indexing in while loops, String accumulation, mem::take	1,5,7
stub-mode-params	contract	stub::limit_params, stub::bg_params, stub::clean_mode, stub::parse_query, stub::percent_decode	∀ mode: limit_params(mode) = Some((claim, reset, plan')) ⇔ split(mode, ':') = [_, claim, reset] or [_, claim, reset, plan] with claim ∈ [a-z0-9_]+ and reset ∈ [0-9]+ fitting i64; plan' = Some(plan) iff plan ∈ [a-z0-9_-]+ (else None, not a refusal). ∀ mode, slow: bg_params(mode, slow) = (n(1, 20), n(2, slow)) with n(i, d) = max(1, parse(trim(part i))) if it parses as i64, else d. ∀ m: clean_mode(m) = strip "custom:"(k') with k = take(64, filter([A-Za-z0-9_.:-], m)) and k' = k, or "ok" if k = "". ∀ q: parse_query(q) = [(pd(k), pd(v)) | kv ∈ split(q,'&'), kv = k=v at the first '=', pd(v) ≠ ""], pd = form-urlencoded decoding (+ ↦ space, %HH ↦ byte, other % kept).	stub/tests.rs::mode_parameters_parse_as_the_modes_document_them	slice patterns over Vec<&str>, Option filter, i64 parse, filter/take, byte loop with lossy UTF-8	1,3,5,7
stub-rate-headers	contract	stub::rate_headers, stub::seq_value, stub::value_text, stub::RateHeaders::summary	∀ spec, served: rate_headers(spec, served) = {on200, on429} with on_x = [(h, seq_value(value_text(v), served)) | (h, v) ∈ spec[on_x] in order] if spec[on_x] is an object, else []. seq_value(v, n) = format("{:.4}", clamp(base + step·n, 0, 1)) when v = "@SEQ:" ++ base ++ ":" ++ step ++ "@" with base, step ∈ [0-9.]+ parsing as f64; else v unchanged. value_text = Python str() of the JSON value (Int as digits, Float as py_float, True/False/None). summary() prints the RL200[…] and RL429[…] groups with the anthropic-ratelimit-unified- prefix cut.	stub/tests.rs::rate_headers_rise_per_answer_served_and_clamp_at_one	Vec<(String,String)>, f64 arithmetic/clamp/precision format, u32→f64, closures	1,3,5,6,7
stub-auth-modes	contract	stub::auth_mode, stub::rate_headers_for, stub::headers_file_for, stub::headers_file_with	∀ modes, l: auth_mode(modes, l) = Some(clean_mode(trim(m))) ⇔ modes is an object whose l is a string m with trim(m) ∉ {"", "off"}; else None (the stub's own mode answers). ∀ spec, l, n: rate_headers_for(spec, l, n) = rate_headers(spec, n) with, where spec.by_auth[l] is an object o, each of on200 and on429 merged header by header with rate_headers(o, n)'s (o's value wins over the header of its lowercased name, the shared order kept, o's new headers after, lowercased). ∀ old, l, b: headers_file_for(old, l, b) = J::parse(old) (an object, else {}) with by_auth[l] removed and, where b is a nonempty object, set to b; by_auth removed when nothing is left in it; every other entry and credential kept. headers_file_with(old, new, blocks) keeps by_auth of old whatever blocks says.	stub/tests.rs::each_credential_reads_its_own_mode_and_its_own_headers_over_the_shared_ones	JSON object walk, Vec merge preserving order, Option chaining	1,3,5,7
stub-analyse-responses	contract	stub::analyse_responses	∀ body: analyse_responses(body).kind = "opaque" if body is not a JSON object; else "codex-json" if text.format.type = "json_schema" (schema = text.format.schema), else "codex". For an object: items = |input|, tools = |tools| (array), instr_chars = chars of instructions (a string) or of its py() (non-null other) else 0, stream = truthy(stream), last_user = the content of the last role=user item (a string, or the input_text/text blocks' texts joined by \n).	stub/tests.rs::a_codex_request_is_read_for_its_items_and_its_structured_side_requests	JSON parse, Option chains, filter/map/join	1,3,5,7,12
stub-schema-answer	contract	stub::schema_answer	∀ schema: schema_answer(schema) = PyJSON(Obj[(k, take(m(v), "lab stub")) | (k, v) ∈ schema.properties in order, v.type = "string"]) with m(v) = v.maxLength if a positive integer else 8; "{}" when there are no properties. The answer validates against any schema whose string properties carry only type/maxLength constraints.	(secondary only; see §2)	J object iteration, i64→usize try_from, chars take	1,3,5,7
stub-usage-limit-and-flat-headers	contract	stub::usage_limit_body, stub::usage_limit_headers, stub::flat_headers	∀ claim, reset, plan: usage_limit_body = {error: {type: usage_limit_reached, message, resets_at: reset, limit_window_minutes: 300 iff claim = five_hour, 10080 iff seven_day, absent otherwise, plan_type: plan iff Some}}; usage_limit_headers(claim, reset) = [x-codex-<slot>-used-percent=100, -window-minutes, -reset-at=reset] for five_hour (primary, 300) and seven_day (secondary, 10080), [] otherwise. ∀ spec, now: flat_headers(spec, now) = [] unless spec is an object; else for each (k, v) in order with v scalar (not null/object/array) and k ∈ [A-Za-z0-9-]{1,80}: the entry (lowercase(k), t) with t = value_text(v), every well-formed @NOW+N@ replaced by now + N left to right and control chars mapped to ' '; a later key equal after lowercasing overwrites the value in place. Obligation: now + N does not overflow (§5.11).	(secondary only; see §2)	J building, while-let string rewriting, char map, Vec find/iter_mut, i64 addition (overflow obligation)	1,3,5,7
stub-served-refines-handlers	refinement	stub::start, stub::Stub::handle, stub::Stub::messages, stub::Stub::responses, stub::Stub::control, stub::Stub::black_hole, stub::control, stub::read_request	For every request sequence sent to start(args)'s listener, each HTTP response (status, headers, body/SSE) equals the pure Answer(mode', headers_file contents at request time, modes_dir, served-200 count, request) built from analyse/analyse_responses, rate_headers, flat_headers, usage_limit_*, auth_label and split_deltas: ok mode streams message_start..message_stop (or response.created..response.completed) whose text deltas concatenate to the canned reply; overloaded ⇒ 529 (Anthropic) / 503 (Codex); limit:C:R with R in the future ⇒ 429 with the representative claim and on429 headers, R past ⇒ ok and "limit lifted" logged; a mode file that changes wins; unknown paths ⇒ 404; /__mode sets and persists the mode; for POST /v1/messages alone, mode' = auth_mode(the --auth-modes-file's JSON at request time, auth_label(request)) where that is Some, else the stub's own mode (each credential meets its own mode), and its rate headers are rate_headers_for(headers_file, auth_label(request), served); every other route answers in the stub's own mode; a custom mode with auth answers only the named credential; the proxy port answers every CONNECT with 502; every request appends one log line naming kind and auth label (a `/v1/messages` line carries ` auth=` ++ its label right after its `last_user="…"`, before the rate headers' summary and the reply's tail: `limit lifted (reset passed)`, ` sse=N deltas`, a limit's ` limit:C resets in Ns`), and no token value is ever logged.	stub/tests.rs::served_on_loopback_it_answers_as_the_modes_say_and_logs_each_request, stub/tests.rs::served_on_loopback_it_answers_codex_as_the_modes_say	TcpListener/TcpStream, threads, Arc<Mutex<State>>, file I/O, HTTP framing, SSE chunking, cross-crate aterm_http client	1,2,7,8,10,12,13
sys-put-no-follow	contract	sys::put, sys::open_dir_at	∀ path, mode, src, Fs: put(path, mode, src) = Err if path is relative, if any directory component on the way is a symlink, or if src = File(p) where p is not a regular file reached through no link; in every Err case no file outside path's own directory chain is created or changed (a link's target is untouched). On Ok: path names a fresh regular file (never a symlink; a link standing at the name is replaced and its target untouched) with contents = the src bytes (or p's contents) and permission bits = mode & 0o777; missing directories on the way are created 0755; the replacement is atomic (by rename).	sys.rs::put_writes_by_a_rename_and_never_through_a_link	unsafe libc openat/mkdirat/renameat/fchmod FFI, RawFd ownership, CString, Path components loop	1,7,8
sys-scratch-read	contract	sys::scratch_read, sys::scratch_file	∀ path, Fs: scratch_read(path) = Ok(None) iff the final name is missing; Ok(Some(contents)) iff every component is reached through no symlink and the file is a regular file owned by the current uid with nlink = 1; Err otherwise (a link at the name or on the way, a second hard link, a directory, a FIFO or other non-regular file). The call never blocks on a FIFO (it opens non-blocking).	sys.rs::scratch_read_takes_only_the_users_regular_file_with_one_name	unsafe libc open with O_NOFOLLOW|O_NONBLOCK, fstat, OwnedFd, read_to_end	1,7,8
tripwire-argv	contract	tripwire::parse_argv, tripwire::answer, tripwire::quoted_after	∀ args: parse_argv(args) = (args[0] or "", acct, svc) where, scanning args[1..] left to right, "-a" X sets acct = X, "-s" X sets svc = X ("" when X is missing), every flag in {-X,-p,-j,-l,-D,-C,-T,-c,-r,-G,-t,-y,-k} consumes its value unread, and acct/svc default to "-". Hence no value following a secret-bearing flag appears in the result unless it is also the value of -a/-s. answer(cmd) = 44 for the listed find-*/delete-* password/certificate/identity sub-commands, 0 for show-keychain-info and add-*-password, 1 otherwise. ∀ line, flag: quoted_after(line, flag) = the text between " " ++ flag ++ " \"" at its LAST occurrence and the next '"'; "-" when there is no occurrence or no closing quote.	tripwire.rs::logs_the_account_and_service_and_never_a_secret_value, tripwire.rs::interactive_lines_name_their_last_account_and_service	while loop with index skipping, match on &str, rfind, slicing	1,3,5,7
util-keep-tree	contract	util::keep_tree, util::keep_file, sys::scratch_file, sys::put	∀ from, to, Fs: after keep_tree(from, to): if from is not a real directory (symlink or missing) nothing happens; else for every entry reached from from through directories only (no symlink followed, at any depth): a real directory is recreated under to and recursed into; a regular file that scratch_file accepts (single name, this user's, no link on its path) is copied to the corresponding path with the same bytes, permission mode and modification time; symlinks and second-name files are not copied. keep_file(src, dst) copies nothing when src's path holds a link anywhere. Nothing outside to is written and no source is changed. Uses sys-put-no-follow and sys-scratch-read.	util.rs::archived_scratch_files_keep_times_but_refuse_other_names	recursive fs walk, read_dir, symlink_metadata, File::set_modified, effects throughout	1,7,8
util-utc-stamp	refinement	util::utc_stamp	∀ secs: i64 with |secs| bounded so the arithmetic does not overflow: utc_stamp(secs) = sprintf("%04d-%02d-%02dT%02d:%02d:%02d.000000Z", civil(secs)), civil = the proleptic Gregorian UTC date-time of the unix second; equivalently days_from_civil(y, m, d)·86400 + h·3600 + mi·60 + s = secs with 1 ≤ m ≤ 12, 1 ≤ d ≤ days_in_month(y, m), 0 ≤ h < 24, 0 ≤ mi, s < 60.	util.rs::utc_stamps_are_the_calendar_day	i64 div_euclid/rem_euclid, Hinnant civil_from_days arithmetic, format! padding	1,3,5
util-b64url-roundtrip	round-trip	util::b64url, util::b64url_decode	∀ bytes: b64url_decode(b64url(bytes)) = Some(bytes); b64url(bytes) uses only [A-Za-z0-9_-] (no '=') and has length ⌈4·|bytes|/3⌉ (RFC 4648 §5 without padding). ∀ text: b64url_decode(text) = Some(bytes) ⇔ text is a canonical RFC 4648 §5 base64url string (with optional '=' padding up to a multiple of 4, body length ≢ 1 mod 4, and zero trailing pad bits), and then b64url(bytes) = trim_end('=', text).	util.rs::base64url_round_trips_without_padding	chunks(3) with u32 shifts, position lookup in a const table, u8 try_from, Vec<u8>	1,3,5,7
util-one-line	contract	util::one_line	∀ s, n: let t = trim(s with every maximal run of control chars (< 0x20 or 0x7f) replaced by one ' '). one_line(s, n) = t if |chars(t)| ≤ n, else take(n - 1, chars(t)) ++ "~" (saturating at n = 0, giving "~"). The output holds no control char and has at most max(n, 1) chars.	util.rs::one_line_folds_control_runs_and_cuts_long_text	chars loop with a bool state, trim, count/take, saturating_sub	1,5,7
util-py-repr	refinement	util::py_repr	∀ s whose chars are all Python-printable or among \\, \n, \r, \t, C0/DEL/C1/NBSP (≤ 0x1f or 0x7f..=0xa0) and U+2028/U+2029: py_repr(s) = PyRepr(s): the delimiter is '"' iff s contains ' and no ", else '; backslash, \n, \r, \t are escaped; the delimiter is escaped; other C0/DEL/C1/NBSP as \xNN and U+2028/U+2029 as \uNNNN (§5.5).	util.rs::py_repr_picks_the_quote_python_picks	chars loop with match guards, format!("{:02x}")	1,5,7,10
verbs-harness-set	contract	verbs::harness_set	∀ text, kvs: let T' = harness_set(text, kvs). If text has no line equal (after trim_end) to "[harness]", T' = text's lines ++ ["", "[harness]"] and then the keys are applied; for each (k, v) in order, within the [harness] table (from its header to the next line starting with '['): every line l = k ++ ws* ++ "=" … is replaced by "k = v"; if there is none, "k = v" is inserted after the table's last non-blank line. Every other line is unchanged and in order; the trailing newline is kept iff text ended with one (or was empty). Idempotent: harness_set(harness_set(t, kvs), kvs) = harness_set(t, kvs).	verbs.rs::harness_keys_are_set_in_their_table_and_replaced_when_there	Vec<String> insert/position/slicing, &mut iteration, closures capturing k	1,2,5,7
verbs-journal-line	contract	verbs::journal_line, util::hms	∀ line: J::parse(line) fails ⇒ journal_line(line) = trim_end(line). Else with t = r.t (number, default 0) / 1000: journal_line(line) = hms(floor t) ++ "." ++ pad3((t·1000 truncated) mod 1000) ++ " " ++ padR(10, kind or "?") ++ " " ++ padR(12, phase or "-") ++ " " ++ (line field or ""). hms is the local wall-clock h:m:s, an environment input.	verbs.rs::a_journal_record_prints_as_one_aligned_line	JSON parse, f64 floor and casts, rem_euclid, format! with width, local-time FFI in hms	1,3,6,8,12
verbs-version-order	contract	verbs::version_key, verbs::semver_like	∀ v: version_key(v) takes the longest prefix of take(3, split(v, '.')) whose leading ASCII-digit prefix parses as u64; for v, w both a.b.c([-+]…)? with a, b, c decimal u64: version_key(v) < version_key(w) (lexicographic Vec order) ⇔ (a, b, c)_v < (a, b, c)_w numerically, and a non-numeric leading component yields []. semver_like(v) ⇔ splitn(3, v, '.') = (major, minor, rest) with major and minor non-empty ASCII-digit runs and rest starting with a non-empty ASCII-digit patch followed either by "" or by '-'/'+' and a non-empty [A-Za-z0-9.+-]+ tail.	verbs.rs::versions_order_by_their_numbers	split/take/map_while/collect, Vec<u64> Ord, u64 parse	1,5,7
```

Merge notes: `footers-done-row-pattern` and `screen-idle-status` both read `is_done_row`; the
definition lives once, in `screen-idle-status`, and the footers row states only the restricted
regex equivalence. `kv` is defined once (`screen-kv-field`) and cited by `nudge-parse-resizes`.
`stub-auth-label` is the one statement of the credential-label rule; `codex-auth-json-lab-jwt`
composes it instead of restating it.

## 2. Test → spec map (TSV)

`primary` is the spec that retires the test. `also` lists the other specs the same test samples.

```tsv
file	test	primary	also
src/cold_retirement.rs	only_a_durable_lapse_with_no_retained_authority_is_success	cold-retirement-evidence	-
src/codex.rs	a_login_or_a_server_never_runs_in_the_lab	codex-safe-args	
src/codex.rs	the_config_names_its_login_and_trusts_the_project_both_ways	codex-config-text	
src/codex.rs	the_composed_login_is_a_jwt_the_stub_reads_by_its_label_only	codex-auth-json-lab-jwt	stub-auth-label, util-b64url-roundtrip, util-utc-stamp, pyjson-dumps-refines-python
src/codex.rs	ratelimits_name_each_window_and_the_credits	codex-ratelimit-headers	pyjson-py-float
src/codex.rs	the_launcher_execs_codex_by_path_with_no_daemon_where_it_is_taken	launcher-argv	util-shell-quote-unquote-roundtrip
src/codex.rs	a_binary_names_the_flag_across_a_block_seam	launcher-argv	
src/door.rs	only_a_lab_shaped_socket_passes	lexical-validators	
src/door.rs	a_pattern_crosses_the_wire_as_one_token	door-wire-pattern	
src/footers.rs	the_mode_row_names_its_mode	footers-mode-word	
src/footers.rs	the_mode_row_is_the_pattern_a_press_waits_on	footers-mode-row-pattern	
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
src/screen.rs	muse_code_s_composer_is_the_caret_row_of_its_frame	screen-composer	
src/screen.rs	kimi_composers_are_read_without_treating_its_trust_prompt_as_input	screen-composer	
src/screen.rs	a_turn_is_idle_on_its_done_row_with_no_spinner	screen-idle-status	
src/screen.rs	key_value_words_are_found_by_key	screen-kv-field	
src/stub/tests.rs	a_request_is_classed_by_what_it_asks	stub-analyse-kind	
src/stub/tests.rs	the_tool_script_is_answered_one_call_per_request_from_toolstart	stub-tool-turn	
src/stub/tests.rs	a_calls_step_makes_its_calls_in_one_message_and_counts_as_one_step	stub-tool-turn	
src/stub/tests.rs	served_a_calls_step_streams_every_call_in_one_message	stub-tool-turn	stub-served-refines-handlers
src/stub/tests.rs	a_calls_steps_inputs_have_their_sids_filled	stub-tool-turn	
src/stub/tests.rs	a_call_with_when_belongs_to_the_subagent_whose_prompt_holds_it	stub-tool-turn	
src/stub/tests.rs	deltas_rebuild_the_text_and_each_but_the_last_holds_twelve_characters	stub-split-deltas	
src/stub/tests.rs	mode_parameters_parse_as_the_modes_document_them	stub-mode-params	
src/stub/tests.rs	rate_headers_rise_per_answer_served_and_clamp_at_one	stub-rate-headers	pyjson-py-float
src/stub/tests.rs	served_on_loopback_it_answers_as_the_modes_say_and_logs_each_request	stub-served-refines-handlers	stub-analyse-kind, stub-rate-headers, stub-split-deltas, stub-auth-label, stub-mode-params
src/stub/tests.rs	a_labelled_claude_token_reads_as_its_label_and_any_other_as_before	stub-auth-label	
src/stub/tests.rs	each_credential_reads_its_own_mode_and_its_own_headers_over_the_shared_ones	stub-auth-modes	stub-rate-headers
src/stub/tests.rs	served_on_loopback_each_credential_meets_its_own_mode_and_its_own_headers	stub-served-refines-handlers	stub-auth-modes, stub-auth-label
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

## 4. Tests that are not spec samples

`sys.rs::the_user_temp_dir_is_named_and_this_process_is_in_no_jail` probes the environment: it
passes only on macOS outside every sandbox (`jail_pids` errs inside one) and where `confstr` names
a temp dir. The contract it touches is `jail-members` (§13), which needs a kernel model; it belongs
in a lab self-test on a real host.

## 5. Implied specs that look wrong, or disagree with the code

1. **Resolved — `footers::DONE_ROW` matches `screen::is_done_row` unconditionally.** `DONE_ROW` is now
   `r"^✻ (?s:.*) for [0-9]"`, anchored at start-of-row with literal `"✻ "` and `" for [0-9]"` and
   arbitrary text (including multi-word verbs and newlines) between them, so both `Regex(DONE_ROW)`
   and `Regex(wire_pattern(DONE_ROW))` agree unconditionally with `screen::is_done_row`.
2. **Resolved — `run.rs::env_sh_round_trips_every_field` splits records on unquoted newlines.**
   `Run::parse` now splits `env.sh` via `env_records` on unquoted, unescaped newlines (respecting
   `'...'`, `"..."`, and `$'...'`), so `Run::parse(&r.render()) == r` holds for every UTF-8 field
   value (multi-line strings and embedded `KEY=VALUE` text included) whenever `!r.agent.is_empty()`
   (`render` intentionally writes an empty `agent` as `"claude"`).
3. **Resolved — `probe.rs::every_operation_crosses_into_the_jail_unchanged` round-trips every `Op`.**
   `Op::encode` and `Op::decode` now escape `\\`, `\t`, `\n`, and `\r` via `esc_field`/`unesc_field`
   and encode `Serve(None)` as `"serve"` versus `Serve(Some(d))` as `"serve\t" ++ esc_field(d)`, so
   `Op::decode(&op.encode()) == Some(op)` holds unconditionally for all `Op` values.
4. **Resolved — `endpoints` splits on the first `char` boundary.** `sampler.rs::endpoints` now
   takes `chars.next()` and `chars.as_str()`, so untrusted `lsof` lines starting with multi-byte
   UTF-8 or empty lines never panic. A listener on `0.0.0.0` or `[::]` remains conservatively
   labelled EXTERNAL.
5. **Resolved — `util.rs::py_repr_picks_the_quote_python_picks`.** `py_repr` now escapes C1 controls
   and NBSP (`0x7f..=0xa0`) as `\xNN` and `U+2028`/`U+2029` as `\uNNNN` alongside C0 controls, so
   Claude Code screen rows with NBSP after `❯` match Python's `repr`.
6. **Resolved — `version_key` reads leading digits with `map_while` and `semver_like` validates full numeric components.** `verbs.rs::version_key` now parses the leading ASCII-digit run of up to three components via `map_while` (so `"2.1.300-rc.1"` yields `[2, 1, 300]` and `"x.99.0"` yields `[]`), and `semver_like` rejects `"2.1.3x"`, `"2.1.3-"`, `"2.1.3+"`, and `"2.1a.3"`.
7. **Resolved — `util.rs::base64url_round_trips_without_padding`.** `b64url_decode` now rejects
   non-canonical base64url strings (`body.len() % 4 == 1`, malformed `'='` padding, and non-zero
   trailing pad bits), so `b64url(&b64url_decode(t)?) == t.trim_end_matches('=')` holds in both
   directions.
8. **Resolved — all launchers quote `PATH`, `run.proxy` and `run.port`.** `codex::launcher`,
   `lab::launcher`, `agy::launcher`, `muse::launcher` and `claude_login_proof::launcher` now format
   `<run>/shim:$PATH` via `util::shell_prepend_path` and pass proxy and port URLs through
   `util::shell_quote`.
9. **Resolved — `wire_pattern` folds preceding unescaped backslashes.** `door.rs::wire_pattern` now
   tracks unescaped `'\\'` runs and pops a single escaping backslash before emitting `\xNN`, so
   `r"\ "` and `r"\="` keep their regex meaning (`r"\x20"`, `r"\x3d"`).
10. **Resolved — `descendants_in` walks unbounded depth with cycle protection.** `procs.rs::descendants_in`
    now uses an explicit post-order stack and `BTreeSet<i32>` visited set instead of truncating at
    depth 64, returning every descendant while terminating on cyclic tables.
11. **Resolved — `flat_headers` saturates `@NOW+N@` arithmetic.** `stub.rs::flat_headers` now
    computes `now.saturating_add(n)` (with oversized `N` saturating to `i64::MAX`), tested near
    `i64::MAX` in `stub/tests.rs`.
12. **Resolved — `configured_nudge` scopes `repaint_nudge` to `[render]`.** `nudge.rs::configured_nudge`
    now tracks TOML table headers and reads `repaint_nudge` only inside `[render]`.
13. **Resolved — lagging spec rows reconciled with current code.** `launcher-argv`, `lab-parse-launch`,
    `agy-parse-launch`, `run-env-sh-roundtrip`, `screen-composer` and `screen-composer-agy` now match
    the shipped signatures and agent branches, and the remaining `screen.rs` composer tests are mapped
    in §2 and §7.

## 7. The agy tests (first written with Gemini CLI's)

Twenty-two tests came with the Gemini CLI and agy lab (`gemini.rs`, `gemini_captures.rs`,
`gemini_proof.rs`, `stub/gemini.rs` through `stub/tests.rs`, and one each in `lab.rs`,
`screen.rs` and `sys.rs`), read the same way as §1: every one maps to a spec below, and none is
outside a spec.

`composer_row` has its own branch for `"agy"` (`screen-composer-agy`), as it has for `"muse"` and
`"codex"`, so `screen-composer`'s last clause ("`composer_row(other, s)`") holds only for
`"claude"` and the agent names with no branch of their own.

```tsv
id	kind	functions	statement	subsumes	difficulty	blocking_gaps
agy-safe-args	contract	agy::safe_args	∀ args: safe_args(args) = Ok(()) ⇔ ∀ a ∈ args: a ∉ {login, logout, auth}. On Err the message names the first offending argument, in order.	agy.rs::a_sign_in_never_runs_in_the_lab	&[String], iterator find, match on &str, Result<(),String>, format!	1,3,5,7
agy-parse-launch	contract	agy::parse_launch, agy::setting_of	∀ args: parse_launch(args) = Ok(l) ⇔ args matches ("-e" V | "--no-wait" | "--untrusted" | "--onboarding" | "--setting" KV)* ["--" R*] with setting_of(KV) = Some(_) for every KV; then l.envs = the V's in order, l.no_wait/l.untrusted = whether each flag occurs, l.onboarding = whether --onboarding occurs, and --onboarding sets l.untrusted too; l.settings = the setting_of(KV)'s in order; l.args = R* verbatim. Err when any other token comes before "--", when "-e" or "--setting" is last, or when setting_of(KV) = None.	agy.rs::options_parse	slice iterator with by_ref, match on &str, Vec push/extend, Result<_, String>	1,3,7
agy-seeds	contract	agy::agy_settings, agy::agy_onboarded, pyjson::J::dump, pyjson::J::parse	∀ trusted: [String]: J::parse(agy_settings(trusted)) = Ok(a) with a.modelProvider = "gemini" (for every input: the sign-in is never left on) and a.trustedWorkspaces = trusted when trusted ≠ [], absent otherwise. agy_onboarded() parses to onboardingComplete = consumerOnboardingComplete = true, enterpriseOnboardingComplete = false.	agy.rs::the_seeds_keep_the_auth_and_move_only_the_trust	J tree construction, JSON print then parse (cross-crate aterm_json), Option chaining	1,3,5,12
agy-launcher-argv	refinement	agy::launcher, util::shell_quote, util::shell_prepend_path	∀ run, envs, args: the script is "#!/bin/sh\nclear\n" ++ one exec command whose word expansion under the Shell model is exactly [env, -u, GOOGLE_API_KEY, -u, GOOGLE_GENAI_USE_VERTEXAI, -u, GOOGLE_CLOUD_PROJECT, -u, GOOGLE_APPLICATION_CREDENTIALS, PATH=<run>/shim:$PATH, HOME=<w>/agy-home, TMPDIR=<w>/tmp] ++ [the six proxy variables = run.proxy, NO_PROXY and no_proxy = 127.0.0.1,localhost] ++ [GEMINI_API_KEY=lab-gemini-not-a-key, GOOGLE_GEMINI_BASE_URL=http://127.0.0.1:<port>, AGY_CLI_DISABLE_AUTO_UPDATE=true, BROWSER=<run>/shim/open, LAB_OPEN_LOG=<w>/open.log, LAB_SECURITY_LOG=<w>/security.log] ++ envs ++ [run.agy_bin] ++ args. So agy runs by path behind the black hole, and every user word survives as one argv element. Uses util-shell-quote-unquote-roundtrip.	agy.rs::the_launcher_execs_agy_by_path_with_the_fake_key_and_the_stub	format! string building, PathBuf join/display/to_string_lossy, for over &[String], a Shell model	1,3,5,7,10
agy-capture-scripts	contract	agy_captures::SCRIPTS, pyjson::J::parse	SCRIPTS is four JSON texts, each parsing to an object with a string "name" and an object "input"; the names are [run_command, write_file, edit_file, ask_question]; the write's and the edit's input.Path are both "@PROJ@/lab-write.txt".	agy_captures.rs::each_box_is_one_call_of_agys_own_tool	&'static str arrays, JSON parse (cross-crate aterm_json), Option chaining	1,5,12
agy-capture-patterns	contract	agy_captures::AGY, agy_captures::SURVEY, aterm_regex::Regex::new, aterm_regex::Regex::is_match	Every field of AGY compiles as a regex. The rows read off the 1.4.0 screens match their pattern: AGY.thinking on "⣽  tick 6...", AGY.answered on "> Red", AGY.idle on agy's idle footer, SURVEY on agy's survey question; and AGY.thinking does NOT match the streamed "  tick 16 tick 17". A statement about constants under a regex semantics model.	agy_captures.rs::every_screen_a_capture_waits_for_matches_its_measured_row	const structs of &str, a regex semantics model (cross-crate aterm_regex)	1,5,12
lab-vendor-version	contract	lab::vendor_version, lab::managed_bin	∀ text: vendor_version(text) = trim_matches('"', trim(v)) for the first line k=v (split at its first '=') whose trim(k) = "version"; "" when no line has that key. The managed_bin clause of lab-managed-bin holds for the name "agy" (its build is the last path of the arrow chain).	lab.rs::an_atpkg_build_record_names_its_version	lines filter_map, split_once, find, trim/trim_matches	1,3,5,7
screen-composer-agy	contract	screen::composer_row, aterm_phase::agy::composer_draft	∀ screen (NBSP→' '), rows = its '\n' lines: composer_row("agy", s) = the first line of composer_draft(rows)'s draft when composer_draft(rows) = Some(_), else None. So agy's sent prompts' 60-column rules (under a box, or under its survey) and background-work panel rows under the composer are never read as the composer.	screen.rs::agy_s_composer_sits_between_two_full_width_rules, screen.rs::agy_s_composer_over_its_background_panel_is_its_caret_row	Vec<String> collect, Option chaining, cross-crate aterm_phase	1,3,5,7
stub-gemini-classify	contract	stub::gemini::analyse_gemini, stub::gemini::schema_fill, stub::gemini::tool_turn, stub::gemini::route	∀ body: analyse_gemini(body).kind = "opaque" if body is no JSON object; else "gemini-json" if generationConfig has responseJsonSchema or responseSchema (then .schema = it) or responseMimeType = "application/json"; else "gemini-aux" if tools = 0; else "gemini" — with tools = Σ over tools[] of |functionDeclarations| (1 for an entry without), contents = |contents|, sys_chars = |chars(the text of systemInstruction's parts)|, last_user = the last non-empty user content's text with a <USER_REQUEST> block unwrapped. ∀ schema, name: schema_fill(schema, name) = the fixed answer for name ∈ {complexity_score: 10, complexity_reasoning, next_speaker: "user", model_choice: "flash"}; else schema.enum's first; else by its type (any case; an array of types its first): object → its required properties (every one when none is required) in key order, each filled under its own name; array → []; integer or number → 0; boolean → false; otherwise "lab stub" cut to maxLength (8 when absent). ∀ body, script: tool_turn(body, script) = None unless a user content holds "toolstart"; with k = the functionResponse parts of the contents after the last such one (of either role) and c = the script's calls without `when`: Call(name_k, input_k, k) while k < |c|, else Done. route(method, path) = Some((model, verb)) ⇔ method = POST ∧ path = /v1beta/models/<model>:<verb> (or /v1/) with verb ∈ {streamGenerateContent, generateContent, countTokens}.	stub/tests.rs::a_gemini_request_is_classed_and_its_tool_script_stepped	JSON tree walks (cross-crate aterm_json), iterator chains, Option<ToolTurn>, string splitting	1,3,5,7,12
stub-gemini-walls	contract	stub::gemini::mode_error, stub::gemini::quota_error	∀ mode, model: mode_error(mode, model) = Some((status, body)) with body.error.code = status and body.error.status = word for (rate-limit, 429, RESOURCE_EXHAUSTED: details a QuotaFailure whose quotaId names a per-day quota, a Help link, a RetryInfo retryDelay "3600s"), (overloaded, 503, UNAVAILABLE), (500, 500, INTERNAL), (401, 401, UNAUTHENTICATED), (bad-key, 400, INVALID_ARGUMENT with an ErrorInfo reason API_KEY_INVALID), (too-long, 400, INVALID_ARGUMENT); None for every mode that is no wall (ok, slow, tool, limit:…). ∀ secs: quota_error(model, secs) carries retryDelay = secs ++ "s".	stub/tests.rs::a_gemini_wall_is_googles_error_body	J tree construction, match on &str, format!	1,3,5
stub-gemini-served	refinement	stub::start, stub::Stub::gemini, stub::Stub::gemini_answer, stub::control	stub-served-refines-handlers for the Gemini API: each POST to a Gemini route answers as the pure Answer for its kind and mode — ok streams SSE `data:` chunks separated by "\r\n\r\n" whose text parts concatenate to the canned reply, the last with finishReason STOP and usageMetadata; a wall mode answers the turn with mode_error's status and body while a side request (gemini-json, gemini-aux) is answered 200 in every mode, a JSON one by schema_fill; tool mode answers tool_turn's call (a functionCall with a thoughtSignature) and then "Tool call done."; slow streams a thought part before its text; a custom mode with auth answers only the named credential; countTokens answers totalTokens; every request, GET /v1beta/models included, appends one log line naming kind and auth label (stub-gemini-credential-label), and no key's or bearer token's value is ever logged.	stub/tests.rs::served_on_loopback_it_answers_gemini_as_the_modes_say	TcpListener/TcpStream, threads, Arc<Mutex<State>>, file I/O, HTTP framing, SSE chunking, cross-crate aterm_http client	1,2,7,8,10,12,13
stub-shared-wall-modes	contract	stub::Stub::messages, stub::Stub::responses	∀ mode ∈ {401, bad-key, 500}: a Claude Code turn (POST /v1/messages) and a Codex turn (POST /v1/responses) are answered with status 401, 401, 500 and a body whose error type is authentication_error / invalid_api_key, authentication_error / invalid_api_key, api_error / server_error respectively: no built-in mode name silently means ok for one API.	stub/tests.rs::the_shared_wall_modes_answer_each_api_in_its_shape	the served stub (effects), JSON bodies, cross-crate aterm_http client	1,7,8,12
sys-scratch-remove	contract	sys::scratch_remove, sys::cpath, sys::cname	Fs model. ∀ path with a parent p and a name n: scratch_remove(path) opens p with O_NOFOLLOW_ANY (no link anywhere on p) and unlinkat(fd, n, 0); Ok(()) when p or n does not exist; Ok(()) and the entry gone when n is a file or a link (a link is removed, never followed: its target is unchanged); Err and nothing removed when p's path holds a link. A path with no parent or name is Err(InvalidInput).	sys.rs::scratch_remove_unlinks_by_name_through_no_link	unsafe FFI (open, unlinkat), errno, an Fs model	1,3,8
agy-proof-auths	contract	agy_proof::gemini_auths	∀ log: gemini_auths(log) = (n, other) with n = |{l ∈ lines(log): l contains " kind=gemini"}| and other = [the value after "auth=" of the first such word after " kind=gemini" in l, or "none" when there is none | such l, that value ≠ "gemini-key"], in order. So a log whose every Gemini API request carried the lab's fake key gives other = [], and one with no Gemini request n = 0.	agy_proof.rs::every_gemini_request_names_the_key_it_carried	lines, split_once, split_whitespace find_map, Vec push	1,5,7
agy-proof-proxy-refused	contract	agy_proof::proxy_refused	∀ log: proxy_refused(log) = the hosts h, each once in order of first appearance, of the lines holding "PROXY refused: " whose first word after it containing ':' or '.' is, less a leading http:// or https://, h followed by '/', ':' or the end; [] when no line holds a refusal.	agy_proof.rs::the_proxy_names_each_refused_host_once	lines, split_once, split_whitespace find, trim_start_matches, split on a char set, Vec contains	1,5,7
agy-proof-talked	contract	agy_proof::talked	∀ log, pids, port: talked(log, pids, port) ⇔ ∃ l ∈ lines(log): l ends with "] TCP conn 127.0.0.1:" ++ port and the text between the last '[' before that suffix and it parses as an i32 ∈ pids. So a listen, another port, or a pid that is a prefix of the logged one is no connection.	agy_proof.rs::the_witness_names_the_pid_that_talked_to_the_stub	lines any, strip_suffix, rsplit_once, parse::<i32>, slice contains	1,5,7
agy-proof-own-writes	contract	agy_proof::own_writes	∀ rel: own_writes(rel) = [r ∈ rel | r ∉ SEEDS] in order, with SEEDS = {antigravity-cli, antigravity-cli/settings.json, antigravity-cli/cache, antigravity-cli/cache/onboarding.json}: the entries the lab's seeds are not.	agy_proof.rs::the_agents_own_writes_are_what_is_not_a_seed	iterator filter, &'static slices, contains	1,5,7
agy-proof-key-verdict	contract	agy_proof::key_verdict, agy_proof::gemini_auths	∀ log: key_verdict(log) = Ok ⇔ let (n, other) = gemini_auths(log) in n > 0 ∧ other = []; else Fail (never Unproven). So a log naming any Gemini API request whose first auth= word is not exactly "gemini-key" — "gemini-key+…" (a bearer beside the key), "other(…", "jwt:…", "none", or no auth= word — fails, and so does a log with no Gemini API request. Uses agy-proof-auths.	agy_proof.rs::the_key_check_fails_on_any_credential_but_the_labs_key_alone	tuple destructure, Vec is_empty, a three-valued enum	1,5,7
agy-proof-egress-verdict	contract	agy_proof::egress_verdict, agy_proof::talked	∀ sockets, egress, pids, port: egress_verdict(sockets, egress, pids, port) = Ok ⇔ talked(sockets, pids, port) ∧ ¬∃ l ∈ lines(egress): l contains "EXTERNAL" ∧ ¬∃ l ∈ lines(sockets): l ends with "  EXTERNAL"; else Fail (never Unproven). With sampler-endpoints: for every lsof text f, the logs the witness writes from f ("<ts> " ++ e per endpoint e, and "<ts> EXTERNAL " ++ e for each e ending "  EXTERNAL") give Fail whenever some endpoint's host is not loopback, whichever pid holds it. Uses agy-proof-talked.	agy_proof.rs::the_egress_check_fails_when_any_socket_leaves_loopback	lines any, contains/ends_with, bool composition	1,5,7
agy-proof-refusals-verdict	contract	agy_proof::refusals_verdict, agy_proof::proxy_verdict	∀ denied, control: refusals_verdict(denied, control) = Fail if denied ≠ []; else Ok if control; else Unproven. So a refusal naming ~/.gemini fails the check whatever control is. ∀ refused: proxy_verdict(refused) = Unproven if refused = [], else Ok.	agy_proof.rs::a_refusal_naming_gemini_fails_whether_or_not_the_control_was_kept	if/else on slice emptiness and a bool, a three-valued enum	1,5
stub-gemini-credential-label	contract	stub::gemini::credential_label, stub::auth_label	∀ key, bearer: Option<&str>: credential_label(None, None) = "none"; credential_label(Some(x), None) = credential_label(None, Some(x)) = auth_label(Some(x)); credential_label(Some(k), Some(b)) = auth_label(Some(k)) ++ "+" ++ auth_label(Some(b)). So a request carrying LAB_GEMINI_KEY and any Authorization header is never labelled exactly "gemini-key", and (by stub-auth-label) no label echoes a credential but the fixed fake keys. The served Gemini routes (POST generate/stream/count and GET /v1beta/models) log auth= this label of x-goog-api-key (or the key query) and Authorization.	stub/tests.rs::a_gemini_requests_credentials_are_labelled_both_when_two_ride	match on a pair of Options, format!	1,3,5
```

```tsv
file	test	primary	also
src/agy.rs	a_sign_in_never_runs_in_the_lab	agy-safe-args	
src/agy.rs	options_parse	agy-parse-launch	
src/agy.rs	the_seeds_keep_the_auth_and_move_only_the_trust	agy-seeds	pyjson-dumps-refines-python
src/agy.rs	the_launcher_execs_agy_by_path_with_the_fake_key_and_the_stub	agy-launcher-argv	util-shell-quote-unquote-roundtrip
src/agy_captures.rs	each_box_is_one_call_of_agys_own_tool	agy-capture-scripts	
src/agy_captures.rs	every_screen_a_capture_waits_for_matches_its_measured_row	agy-capture-patterns	
src/agy_proof.rs	every_gemini_request_names_the_key_it_carried	agy-proof-auths	
src/agy_proof.rs	the_proxy_names_each_refused_host_once	agy-proof-proxy-refused	
src/agy_proof.rs	the_witness_names_the_pid_that_talked_to_the_stub	agy-proof-talked	
src/agy_proof.rs	the_agents_own_writes_are_what_is_not_a_seed	agy-proof-own-writes	
src/agy_proof.rs	the_key_check_fails_on_any_credential_but_the_labs_key_alone	agy-proof-key-verdict	agy-proof-auths, stub-gemini-credential-label
src/agy_proof.rs	the_egress_check_fails_when_any_socket_leaves_loopback	agy-proof-egress-verdict	agy-proof-talked, sampler-endpoints
src/agy_proof.rs	a_refusal_naming_gemini_fails_whether_or_not_the_control_was_kept	agy-proof-refusals-verdict	
src/lab.rs	an_atpkg_build_record_names_its_version	lab-vendor-version	lab-managed-bin
src/screen.rs	agy_s_composer_sits_between_two_full_width_rules	screen-composer-agy	
src/screen.rs	agy_s_composer_over_its_background_panel_is_its_caret_row	screen-composer-agy	
src/stub/tests.rs	a_gemini_request_is_classed_and_its_tool_script_stepped	stub-gemini-classify	
src/stub/tests.rs	a_gemini_wall_is_googles_error_body	stub-gemini-walls	
src/stub/tests.rs	a_gemini_requests_credentials_are_labelled_both_when_two_ride	stub-gemini-credential-label	stub-auth-label
src/stub/tests.rs	served_on_loopback_it_answers_gemini_as_the_modes_say	stub-gemini-served	stub-gemini-classify, stub-gemini-walls, stub-auth-label, stub-gemini-credential-label, stub-split-deltas
src/stub/tests.rs	the_shared_wall_modes_answer_each_api_in_its_shape	stub-shared-wall-modes	stub-served-refines-handlers
src/sys.rs	scratch_remove_unlinks_by_name_through_no_link	sys-scratch-remove	
```

The selftest itself (`agy-selftest`) is not a unit test: like `selftest` and
`codex-selftest` it runs the real agent in a lab, and its judges are the seven `agy-proof-*`
specs.

## 8. The open recorder's verdict

The four proofs that run an agent in a lab (`selftest`, `codex-selftest`, `agy-selftest`,
`muse-selftest`) judge the open recorder through one function. `selftest` (Claude Code)
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

## 9. The settings witness

Every lab's scratch settings carry a `ConfigChange` hook (`settings_witness`), and
`scn::Ctx::arm` rewrites the survey flag until Claude Code reports that it re-read the file.

```tsv
id	kind	functions	statement	subsumes	difficulty	blocking_gaps
witness-rereads	contract	settings_witness::rereads	∀ log, settings: rereads(log, settings) = |{l ∈ lines(log): J::parse(l) = Ok(j) ∧ j.hook_event_name = "ConfigChange" ∧ j.file_path = settings}|. So a change to another file, another hook event, a line that is not JSON and an object with no file_path count nothing.	settings_witness.rs::a_reread_is_a_config_change_line_naming_the_settings_file	lines filter count, JSON parse (cross-crate aterm_json), Option chaining	1,5,7,12
witness-until-reread	contract	settings_witness::until_reread	∀ write, seen, every, hang, wait: until_reread returns Ok(Reread{writes: k, after}) only when a read of seen made after the k-th call of write returned and before any later call exceeds the read of seen made once, just after the first call returned (the baseline); while no read exceeds the baseline, write is called again at the first round whose reading of wait's clock is every or more past the reading that decided the last call (for the first call, the one reading made just before it), and wait pauses between reads otherwise; once hang has passed on that clock since the first call the result is Err ("no re-read"). Every call is the same write, so a line that lands during the first write (an older change's) is never the answer, and a line that lands while a rewrite is made is. The clock is read once before the first call and once in each round, after seen, and a round's one reading decides after, the hang check (made first), the rewrite and when the next is due, so on any clock, one that moves between reads or during a call included, no call follows a reading at or past hang, and when the next call is due does not depend on how long a call takes. A log that never grows ends at the first round reading at or past hang: on a clock that moves only at a pause or during a call, by a step that divides every, with every call shorter than every (0 < every, 0 < hang), after ⌈hang / every⌉ calls, made at 0, every, 2·every, … from the first, and on a clock with no round reading at or past every after the first call's reading and short of hang, after one call.	settings_witness.rs::an_unanswered_write_is_made_again_until_a_line_answers, settings_witness.rs::a_line_from_before_the_write_ends_or_none_at_all_is_no_reread, settings_witness.rs::no_write_follows_a_reading_at_or_past_hang	closures (FnMut, Fn), a trait-generic clock (Wait: WallWait's Instant and interruptible sleep (effects), SteppedWait and Ticking in tests), Duration arithmetic, a loop with early returns	1,3,7,8,13
witness-hook-script	refinement	settings_witness::script, settings_witness::hooks, util::shell_quote	∀ script path p, log path g: hooks(p).ConfigChange = [{hooks: [{type: "command", command: shell_quote(p)}]}]; run by /bin/sh with input x on stdin, script(g) appends x ++ "\n" to g when g's directory exists and leaves the file system unchanged otherwise, and exits 0 in both cases (exit 2 would block the change and any other failure would draw an error in the tab). Uses util-shell-quote-unquote-roundtrip.	settings_witness.rs::the_hook_runs_the_script_which_appends_its_input_and_never_fails	format!, J tree construction, a Shell model and a process (effects)	1,3,5,8,10
witness-in-every-lab	contract	lab::scratch_claude_config	∀ run, helper, proj, o: J::parse(scratch_claude_config(run, helper, proj, o).0).hooks has exactly one ConfigChange command hook, and it runs shell_quote(run.run/config-change.sh), never another run's; apiKeyHelper = helper.	lab.rs::a_lab_ups_claude_settings_carry_the_settings_witness_hook	J tree construction, JSON print then parse (cross-crate aterm_json)	1,3,5,12
```

```tsv
file	test	primary	also
src/settings_witness.rs	a_reread_is_a_config_change_line_naming_the_settings_file	witness-rereads	
src/settings_witness.rs	an_unanswered_write_is_made_again_until_a_line_answers	witness-until-reread	
src/settings_witness.rs	a_line_from_before_the_write_ends_or_none_at_all_is_no_reread	witness-until-reread	
src/settings_witness.rs	no_write_follows_a_reading_at_or_past_hang	witness-until-reread	
src/settings_witness.rs	the_hook_runs_the_script_which_appends_its_input_and_never_fails	witness-hook-script	witness-rereads
src/lab.rs	a_lab_ups_claude_settings_carry_the_settings_witness_hook	witness-in-every-lab	witness-hook-script
```

## 10. The windowed lab and L13's keys

`forever --window` opens the lab's instance as a window in the same jail and types `land-p`'s first
prompt through `hwkey`, the path a key press takes, so L13 (a person's own exchange) is judged on
real keys (`l13-keys-account`).

```tsv
id	kind	functions	statement	subsumes	difficulty	blocking_gaps
lab-windowed-argv	contract	lab::instance_argv, lab::windowed	∀ run, aterm, lifeline: windowed(run) ⇔ run.window = "1". If windowed(run): instance_argv(run, aterm, None, lifeline) = (if lifeline then [run.run/bin/aterm-lab, "keeper", "--"] else []) ++ [aterm, "--no-reroute", "--control-sock", run.sock, "-d", run.proj, "--columns", run.cols, "--lines", run.rows], with no "--headless" and no "--lifeline-fd" (a window refuses both). Otherwise the line starts [aterm, "--headless"] and holds the lifeline flag when lifeline.	lab.rs::a_windowed_lab_s_instance_opens_a_window_its_keeper_holding_the_lifeline	Vec<String> building, PathBuf join, a bool branch	1,5
crash-refuses-window	contract	crash::crash, crash::may_be_keeper	∀ run, a, ops: when windowed(run) ∧ ops.keeper(run.instance_pid), crash(run, a, ops) = Err(usage, code 2) and ops records no event (nothing killed, nothing relaunched), so a SIGKILL never hits an armed window's keeper and leaves its aterm running; when ¬ops.keeper(run.instance_pid) (`up --window`, whose recorded pid is its aterm) or ¬windowed(run), crash proceeds through the kill and relaunch. may_be_keeper(line) ⇔ line = "" ∨ line contains "aterm-lab keeper -- ".	crash.rs::a_windowed_lab_is_crashed_only_where_its_pid_is_its_aterm, crash.rs::a_keeper_is_known_by_its_command_line	an early return before an Ops trait's effects (fakes), &str is_empty/contains	1,3,8
l13-keys-account	contract	forever::cover::judge_theirs, forever::cover::keys_unaccounted, forever::cover::keyed_line	∀ seen with seen.cover.person_ms = Some(p) and seen.cover.keyed = Some(k): keys_unaccounted(k) = None ⇔ k.posted > 0 ∧ k.key_window = k.posted ∧ k.draft_stood ∧ k.draft_taken ∧ k.seq_after = Some(a) ∧ a = k.seq_before; judge_theirs(seen, t0) = Unobserved(why …) where it is Some(why) and Unobserved(keyed_line(k) …) where it is None, whatever person_stamp reads and whatever the harness did in the exchange: controller-delivered keys never PASS. With keyed = None and a numeric person_stamp, judge_theirs judges the exchange through the seam (an act in it is a FAIL), and a PASS opens with "typed through the `as=person` seam".	forever_tests.rs::native_controller_delivery_needs_composer_effects_and_unchanged_person_count, forever_tests.rs::land_p_does_not_claim_person_takeover_from_controller_delivery, forever_tests.rs::land_p_s_prompt_is_typed_key_by_key_through_hwkey	Option and equality checks, format!, a judge over journal, ledger and status records	1,5,7
composer-labelled-rule	contract	screen::is_rule, screen::composer_row, screen::composer_of	∀ row r: is_rule(r) ⇔ count(trim(r)) ≥ 40 ∧ (every char of trim(r) ∈ {─, ━} ∨ aterm_phase::phase::is_rule(trim(r))). So Claude Code's top rule with the top-effort mode's label inside it (`──── <name> ─`, which a windowed aterm's defaults turn on) frames the composer as a bare rule does, while a short labelled run, a rule with a table's joint (┬) and a row of words frame nothing.	screen.rs::claude_code_s_labelled_top_rule_frames_its_composer	char counting, a delegated predicate	1
```

```tsv
file	test	primary	also
src/lab.rs	a_windowed_lab_s_instance_opens_a_window_its_keeper_holding_the_lifeline	lab-windowed-argv	
src/crash.rs	a_windowed_lab_is_crashed_only_where_its_pid_is_its_aterm	crash-refuses-window	
src/crash.rs	a_keeper_is_known_by_its_command_line	crash-refuses-window	
src/forever_tests.rs	land_p_typed_through_hwkey_passes_only_where_the_keys_account_for_the_stamp	l13-keys-account	
src/forever_tests.rs	land_p_s_prompt_is_typed_key_by_key_through_hwkey	l13-keys-account	
src/screen.rs	claude_code_s_labelled_top_rule_frames_its_composer	composer-labelled-rule	
```

## 11. The windowed lab's refusal and start, and the survey arm

A review of `b9/lab-hygiene` found behaviour no test held: the refusal of a windowed lab under
`LAB_RESTORE=1`, the windowed launch's readiness wait, the survey arm's use of the settings witness
(an arm that wrote and slept 3 s passed every test), and the 40-column floor of a labelled rule.
`survey --reread` is new: the arm's write, from the command line. Five new tests, read the same way as
§1; `claude_code_s_labelled_top_rule_frames_its_composer` (§10) now also holds the floor.
2026-10-27: `lab-opts-refused` also states the reroute-and-clock refusal it always made and the
refusals of `up --fabric` and `up --tab-strip` beside a LAB_ATERM_TOML naming the same key, held
by a sixth test.

```tsv
id	kind	functions	statement	subsumes	difficulty	blocking_gaps
lab-opts-refused	contract	lab::opts_refused, lab::up, forever::lab_opts	∀ o: opts_refused(o) = Some(why) ⇔ (o.window ∧ o.restore) ∨ (o.reroute ∧ o.clock) ∨ (o.fabric ∧ o.aterm_toml parses with a `fabric` key) ∨ (o.tab_strip ∧ o.aterm_toml parses with a `tab_strip_rows` key): the last two are the keys `up --fabric` and `up --tab-strip` add to the scratch aterm.toml (scratch_aterm_toml), which named twice is no TOML document. up(tag, o) with opts_refused(o) = Some(why) returns Err(Fail{code: 2, msg: why}) before it judges tag or makes anything; lab_opts(base, mode, keep, (surfaces, window)) returns Err(Fail{code: 2, msg: "forever: " ++ why}) where the options it builds are refused, so `forever --window` under LAB_RESTORE=1 is a usage error, never a harness's COULD NOT RUN; otherwise it returns base with trust, lifeline = ¬keep, window, the person's seam iff surfaces, and the cap's rate iff mode = Cap.	lab.rs::a_windowed_lab_under_the_restore_seam_is_refused_before_anything_is_made, forever_tests.rs::forever_window_under_the_restore_seam_is_a_usage_error_not_could_not_run, lab.rs::a_bus_or_tab_strip_lab_refuses_an_aterm_toml_naming_the_same_key	a bool conjunction, early returns, struct update	1,5
lab-window-ready	contract	lab::while_starting	∀ look, active, hang, start, pause: while_starting returns Ok(()) iff some look returns Ok(true) before a look's Err, a pause's Err, or hang passing with no look active; each round asks look, then active, then the clock: look's Err unchanged (the child ended before it was ready), Err("… the hang detector") once no look was active for hang, and pause's Err unchanged (an interrupt). A start that keeps being active is never a hang, however long it takes. Its production active (lab::Starting::active) is the child's process group not idle by aterm_sysprobe::activity, or its output's length moved.	lab.rs::a_starting_child_is_waited_for_while_it_is_active	closures (FnMut), Instant arithmetic, a loop with early returns	1,3,7,8
arm-awaits-witness	contract	scn::Ctx::arm, scn::Ctx::arm_with_wait, settings_witness::set_env_reread_with_wait	∀ ctx: ctx.arm() returns Ok only after the witness's log holds a ConfigChange line naming ctx.run.settings() that was appended after a write of CLAUDE_FORCE_DISPLAY_SURVEY = "1"; the flag stands in the settings' env, and sumd.arm_writes counts the writes made (more than one where the first went unanswered), one every settings_witness::REWRITE_EVERY on the arm's clock while no line answers. Where no such line comes within settings_witness::HANG on that clock the result is Err(Failed("the survey flag was not armed: …no re-read… (n writes)…")) with those writes counted, and arm_writes is not set. Uses witness-until-reread and witness-rereads.	scn.rs::the_survey_arm_returns_only_once_claude_code_reported_a_reread	a SteppedWait whose pauses step the clock and stand in for the watcher on the arm's thread, the file system (effects), J summary	1,3,8,13
survey-words	contract	verbs::survey_words	∀ words: survey_words(words) = Ok((Some("1"), r)) ⇔ words = ["on"] ++ (r ? ["--reread"] : []); Ok((None, r)) likewise for "off"; every other list of words is Err("survey: on|off [--reread]").	verbs.rs::survey_takes_on_or_off_and_reread_after_it	slice patterns	1
```

```tsv
file	test	primary	also
src/lab.rs	a_windowed_lab_under_the_restore_seam_is_refused_before_anything_is_made	lab-opts-refused	
src/forever_tests.rs	forever_window_under_the_restore_seam_is_a_usage_error_not_could_not_run	lab-opts-refused	
src/lab.rs	a_bus_or_tab_strip_lab_refuses_an_aterm_toml_naming_the_same_key	lab-opts-refused	
src/lab.rs	a_starting_child_is_waited_for_while_it_is_active	lab-window-ready	
src/scn.rs	the_survey_arm_returns_only_once_claude_code_reported_a_reread	arm-awaits-witness	witness-until-reread
src/verbs.rs	survey_takes_on_or_off_and_reread_after_it	survey-words	
```

## 12. The `ps` stand-in on the kernel's records

The jailed Codex's `ps` stand-in (`aterm-lab` run as `ps`) prints `aterm_sysprobe::kproc` records.

```tsv
id	kind	functions	statement	subsumes	difficulty	blocking_gaps
ps-line-fields	contract	codex_rig::{ps_line,ps_rows}, sys::tty_name	∀ r: kproc::Record, fields, now: ps_line(r, fields, now) = the fields joined by one space, each pid → r.pid, ppid → r.ppid, pgid → r.pgid, tpgid → r.tpgid, tty or tt → tty_name(r.tty) (`??` for None or 0), lstart → lstart(r.start_s), etime → etime(now − r.start_s), any other → r.comm. ps_rows(args, all, one) chooses all() with -A, -e or no -p, else one(p) for each pid p the -p lists name (split by commas and spaces, every -p adding), never pid 0; an empty list or piece, or a word that is no u32, is Err (exit 1, nothing printed).	codex_rig.rs::a_ps_line_reads_the_kernel_record, codex_rig.rs::the_pid_list_chooses_exactly_its_pids	match on &str, i64 arithmetic, String join; devname is FFI	0,1,3
```

```tsv
file	test	primary	also
src/codex_rig.rs	a_ps_line_reads_the_kernel_record	ps-line-fields	
src/codex_rig.rs	the_pid_list_chooses_exactly_its_pids	ps-line-fields	
```

## 13. A jail listing that was not read

`jail_pids` lists this user's processes through `aterm_uds::holders::uid_pids`, and a listing that
fails is an error, never an empty jail. The four proofs that print the jail's members (`selftest`,
`codex-selftest`, `agy-selftest`, `muse-selftest`) print that error in the table's place
instead of an empty table; their checks still read no member, so the orphan's check fails. Two
new tests, read the same way as §1.

```tsv
id	kind	functions	statement	subsumes	difficulty	blocking_gaps
jail-members	contract	sys::{jail_pids,jail_members}	∀ listed, me, in_jail: jail_members(Err(e), me, in_jail) = Err("jailpids: the process list could not be read: " ++ e); jail_members(Ok(pids), me, in_jail) = Ok(the distinct p ∈ pids that fit i32 with p > 1, p ≠ me and in_jail(p), ascending). jail_pids(mark) is jail_members over holders::uid_pids() with in_jail(p) ⇔ sandbox_check(p) = 1 ∧ p is denied mark's mach-lookup ∧ p is not denied mark.none's, after its own refusal when this process is sandboxed or denied mark.	src/sys.rs::a_listing_that_fails_is_an_error_and_never_an_empty_jail	Result map_err, filter_map, sort_unstable, dedup, a closure seam	0,1,3
jail-members-table	contract	selftest::members_table	∀ why: members_table(Err(why)) = "  not read: " ++ why ++ "\n"; members_table(Ok([])) = ""; for a nonempty list it is ps's rows for those pids, each cut at 120 chars and indented two spaces (an effect, not sampled).	src/selftest.rs::a_member_table_that_was_not_read_says_why	Result match, format!	0,1
```

```tsv
file	test	primary	also
src/sys.rs	a_listing_that_fails_is_an_error_and_never_an_empty_jail	jail-members	
src/selftest.rs	a_member_table_that_was_not_read_says_why	jail-members-table	
```

## 14. The threaded search, and what the benches share

`aos-search --threads N` (`src/aos_search.rs`) judges a search's candidates on threads
(`src/spread.rs`). The tests in `tests/aos_search.rs` spawn the built `aterm-lab`, so their spec is
an effect (gap 8). `src/bench_stats.rs` holds what `aos-gpu-bench`, `outlook_captures` and the swarm
partition share. The distributed search (`aos-search --workers`, the worker verb and the runner
`aos-workers-bench`) was removed on 2026-11-04:
it gains no speed on one Mac that threads do not gain better (measured: 26.25 s on eight workers, 18.52 s on eight threads, 46.80 s serial, `docs/measured/aos-m5b-workers-2026-10-23.md`), and git history is the way back.

```tsv
id	kind	functions	statement	subsumes	difficulty	blocking_gaps
aos-search-threads-process	contract	aos_search::{run_in,search,summary_of}; spread::{say,say_built,Stepped::new}	For F the six-line plans file: `aos-search --plans F --threads 3 --frames` exits 0 and writes the bundle the same command without `--threads` writes, timings aside, in mode search; each run says its evaluator built, each candidate trained, the winner's held-out read and its verify, one step each. A run past the one-minute hang detector on its next new step or its end fails, or is COULD NOT RUN when its process waited for a core at least half the stretch and longer than it ran, from 10 s into the stretch and over its last 10 s.	tests/aos_search.rs::{threads_give_the_local_result,a_step_counts_once_and_only_when_the_run_names_it,a_stretch_is_judged_on_the_runs_wait_for_a_core,a_stretch_past_the_detector_is_could_not_run_only_when_starved_through}	processes, pipes, threads, files	0,1,8,13
aos-spread	contract	spread::{Threads::run,Stepped::run}	Threads(w) runs each of n jobs exactly once at any width; Stepped runs every index once through the spread it wraps and says one step per job finished, numbered across its calls; a call with no jobs says nothing.	src/spread.rs::{threads_run_every_index_once,stepped_spreads_say_one_step_per_job_and_run_every_index_once}	threads, atomics	0,1,3,5,7,13
aos-search-parallel-flags	contract	aos_search::parse_args	--threads takes 1 to 256, or exit 2; --frames parses in debug builds, with or without it.	src/aos_search.rs::threads_are_bounded_and_frames_parses_with_or_without_threads	string matching, Option, Result	0,1,3
bench-stats	contract	bench_stats::{williams,median,span_ratio,required_valid,parse_loadavg}	williams(n) for even n is n orders of 0..n, each value once in every position and after every other value exactly once, None for odd n or 0; median is the middle value or the mean of the two; span_ratio is max / min, None when empty or the least is not positive; required_valid(n) = n − ⌊n/6⌋; parse_loadavg takes the first figure of `{ 1m 5m 15m }`.	src/bench_stats.rs::{the_williams_square_puts_each_arm_once_in_every_position,the_williams_square_puts_each_arm_after_every_other_exactly_once,the_load_average_is_the_first_of_sysctls_three,the_median_the_span_and_the_valid_rounds}	Vec, string parsing, f64	0,1,3,5,7
```

```tsv
file	test	primary	also
tests/aos_search.rs	threads_give_the_local_result	aos-search-threads-process	
tests/aos_search.rs	a_step_counts_once_and_only_when_the_run_names_it	aos-search-threads-process	
tests/aos_search.rs	a_stretch_is_judged_on_the_runs_wait_for_a_core	aos-search-threads-process	
tests/aos_search.rs	a_stretch_past_the_detector_is_could_not_run_only_when_starved_through	aos-search-threads-process	
src/spread.rs	threads_run_every_index_once	aos-spread	
src/spread.rs	stepped_spreads_say_one_step_per_job_and_run_every_index_once	aos-spread	
src/aos_search.rs	threads_are_bounded_and_frames_parses_with_or_without_threads	aos-search-parallel-flags	
src/bench_stats.rs	the_williams_square_puts_each_arm_once_in_every_position	bench-stats	
src/bench_stats.rs	the_williams_square_puts_each_arm_after_every_other_exactly_once	bench-stats	
src/bench_stats.rs	the_load_average_is_the_first_of_sysctls_three	bench-stats	
src/bench_stats.rs	the_median_the_span_and_the_valid_rounds	bench-stats	
```

## 15. The shared-buffer inference measurement's runner (aos M5a)

`aos-gpu-bench` (`src/aos_gpu_bench.rs`) runs the pre-registered session over the release
harness `aterm-bench --example aos_gpu`: one child process per arm, spawned and stamped on
`mach_absolute_time`, waited for by a blocking `waitid` (`sys::await_exit`) beside a deadline thread
(`stop::sleep_unless`, woken by `stop::ring`). The runner's effects (the spawn, `pmset`,
`sysctl`, the GPU readings) are untested here, except `sys::await_exit`, which a child that stops
itself tests; its pure core is tested over synthetic summaries in the harness's format, and every
reading it makes of a run is written to `runs/{id}.runner` and read back. Read the same way as §1.

```tsv
id	kind	functions	statement	subsumes	difficulty	blocking_gaps
aos-gpu-bench-record	contract	aos_gpu_bench::{schedule,parse_args,parse_summary,parse_lowpower,parse_ac,parse_swap_used,qos_class,judge,gate0,gate3_session,evaluate,criteria,no_claim,derived,runner_text,parse_runner,render}	Round r runs row (r−1) mod 4 of the Williams square of F, I, T, G (12 rounds: FIGT, ITFG, TGIF, GFTI three times over); a summary parses only as unique key=value lines of format aos-gpu-v1 with its entry, arm, run id, build stamp and last stamp; a nonzero exit (but a deadline kill, G5, and a G run's exit 3 with its GPU failure written, G3), an unparsable summary, an arm or run-id mismatch, repetitions other than the registered ones, a missing figure (G's canonical pipeline time among them) and a B371 reference off its pin are defects that stop the session; G0 needs the runner's clean commit in every harness stamp, `release opt-level=3`, no debug assertions and the harness unchanged at the end; G1 is the texts' digest, tone-int v1's id and F's argmax at their pins; G2's line counts only the runs that exited 0, so completed every series, and names each partial run with the repetitions its summary holds; G3 is a completed, twice-qualified G run with a cold pipeline of at least 2 ms, a canonical (warm-cache) pipeline under 2 ms and its recovery counter still, and one hw.model, osversion, timebase, registryID, driver and child main-thread QoS across the session; G5 is the 120 s deadline, a lifetime footprint of at most 2 GiB, the declared bytes within the device limits, every T pool thread the harness asked to take the main thread's QoS (`pool_qos_wanted`) having taken it, Low Power Mode off and AC power, the last two read from `pmset -g` and `pmset -g ps` kept verbatim; G4 bounds F's B371 p50 span by 1.3× and G6 needs N − ⌊N/6⌋ valid rounds; C1, C2, C5 and C6 (both figures) and the sanity checks C3 and C4 are shown only when they hold in every valid round in a valid, finished session, and a smoke run or a void, stopped, running or interrupted session claims nothing; a run's `.runner` sidecar (aos-gpu-runner-v1) parses back to its measurements and conditions, so the record made from it is the same; the record discloses §12.10's biggest risk with its sentence; the record exits 0 valid, 1 void, 2 on a defect.	src/aos_gpu_bench_tests.rs::{the_schedule_is_the_registered_williams_order_three_times_over,the_arguments_parse_and_rounds_stay_within_the_registered_twelve,a_summary_parses_by_key_and_refuses_what_is_not_one,pmset_and_swapusage_read_as_this_host_prints_them,a_complete_run_of_each_arm_is_valid,every_defect_named_in_g6_stops_the_session,a_gpu_that_misbehaves_voids_its_run_under_g3,a_budget_missed_voids_its_run_under_g5,a_base_that_does_not_reproduce_fails_g1_and_voids_the_session,a_clean_registered_session_is_valid_and_its_criteria_read_per_round,g0_needs_one_clean_commit_and_one_unchanged_release_harness,g3_holds_the_machine_and_driver_still_across_the_session,g4_and_g6_void_an_unstable_or_thin_session,a_defect_stops_the_session_ahead_of_every_gate,derived_figures_are_rows_over_time_and_over_cpu,the_record_carries_every_section_and_its_exit,no_criterion_is_claimed_unless_the_registered_session_is_valid_and_finished,g2_counts_only_the_runs_that_completed_every_series_and_names_the_partial_ones,the_runner_sidecar_round_trips_every_reading_the_record_derives_from}	Vec, BTreeMap, string parsing, f64, enum match	0,1,3,5,7
stop-sleep-unless	contract	stop::{sleep_unless,ring}	sleep_unless(d, done) returns Ok(true) once done is set and the bell rung, Ok(false) at d, and Err on an interrupt before done.	src/stop.rs::{a_ring_after_done_ends_the_wait_and_the_deadline_ends_it_otherwise}	Mutex, Condvar, atomics, threads	0,13
sys-await-exit	contract	sys::await_exit	For a child of this process, await_exit(pid) returns Ok only once the child has exited (CLD_EXITED, CLD_KILLED or CLD_DUMPED) and leaves it unreaped; a stop or continue report Darwin's waitid gives under WEXITED is consumed without reaping (WSTOPPED | WCONTINUED | WNOHANG) and the wait goes on, backing off while nothing is consumed.	src/sys.rs::{await_exit_waits_through_a_stop_and_a_continue_for_the_exit}	processes, signals, FFI	0,8
```

```tsv
file	test	primary	also
src/aos_gpu_bench_tests.rs	the_schedule_is_the_registered_williams_order_three_times_over	aos-gpu-bench-record	
src/aos_gpu_bench_tests.rs	the_arguments_parse_and_rounds_stay_within_the_registered_twelve	aos-gpu-bench-record	
src/aos_gpu_bench_tests.rs	a_summary_parses_by_key_and_refuses_what_is_not_one	aos-gpu-bench-record	
src/aos_gpu_bench_tests.rs	pmset_and_swapusage_read_as_this_host_prints_them	aos-gpu-bench-record	
src/aos_gpu_bench_tests.rs	a_complete_run_of_each_arm_is_valid	aos-gpu-bench-record	
src/aos_gpu_bench_tests.rs	every_defect_named_in_g6_stops_the_session	aos-gpu-bench-record	
src/aos_gpu_bench_tests.rs	a_gpu_that_misbehaves_voids_its_run_under_g3	aos-gpu-bench-record	
src/aos_gpu_bench_tests.rs	a_budget_missed_voids_its_run_under_g5	aos-gpu-bench-record	
src/aos_gpu_bench_tests.rs	a_base_that_does_not_reproduce_fails_g1_and_voids_the_session	aos-gpu-bench-record	
src/aos_gpu_bench_tests.rs	a_clean_registered_session_is_valid_and_its_criteria_read_per_round	aos-gpu-bench-record	
src/aos_gpu_bench_tests.rs	g0_needs_one_clean_commit_and_one_unchanged_release_harness	aos-gpu-bench-record	
src/aos_gpu_bench_tests.rs	g3_holds_the_machine_and_driver_still_across_the_session	aos-gpu-bench-record	
src/aos_gpu_bench_tests.rs	g4_and_g6_void_an_unstable_or_thin_session	aos-gpu-bench-record	
src/aos_gpu_bench_tests.rs	a_defect_stops_the_session_ahead_of_every_gate	aos-gpu-bench-record	
src/aos_gpu_bench_tests.rs	derived_figures_are_rows_over_time_and_over_cpu	aos-gpu-bench-record	
src/aos_gpu_bench_tests.rs	the_record_carries_every_section_and_its_exit	aos-gpu-bench-record	
src/aos_gpu_bench_tests.rs	no_criterion_is_claimed_unless_the_registered_session_is_valid_and_finished	aos-gpu-bench-record	
src/aos_gpu_bench_tests.rs	g2_counts_only_the_runs_that_completed_every_series_and_names_the_partial_ones	aos-gpu-bench-record	
src/aos_gpu_bench_tests.rs	the_runner_sidecar_round_trips_every_reading_the_record_derives_from	aos-gpu-bench-record	
src/stop.rs	a_ring_after_done_ends_the_wait_and_the_deadline_ends_it_otherwise	stop-sleep-unless	
src/sys.rs	await_exit_waits_through_a_stop_and_a_continue_for_the_exit	sys-await-exit	
```

## 16. The learner's verbs and the session runner (aos M6, stage D)

`aos-learn` (`src/aos_learn.rs`) is M6's learner in the lab: the verbs `build`, `init`, `chain`,
`train`, `rollback`, `verify` and `session` over aterm-effects' `m6`, and the files behind its
`DurableLog` (`LabLog`) and `DurableStore` (`LabStore`). `aos-learn-bench` (`src/aos_learn_bench.rs`)
is the session's runner. The
state machines they drive are m6's, tested over memory in aterm-effects' `m6_tests.rs`. A whole
session needs `M6_CHAIN_V1` (stage E) and the account's own home, so no test runs one, and the
runner's spawn and watch are effects (gap 8); the files, the arguments, the summary and the gates
are tested over real temp directories and synthetic children. Read the same way as §1.

```tsv
id	kind	functions	statement	subsumes	difficulty	blocking_gaps
aos-learn-files	contract	aos_learn::{LabLog::{open,open_within,need_runner,need_runner_of,read_only,pairs,hold_runner,version_root},LabStore::{open,stopping,retained},Held,line_files,lock_file,lock_within,make_dirs,write_new,create_linked}	Every flush is aterm_durable's durable one; every directory a ledger line, a store file or the runner's record lives in is made by make_dirs, each new name flushed in its parent after it is made (the .tmp/ staging directories alone are not: nothing is read through them after a crash); a file is made new, written whole and flushed, so its bytes written are its size; a lock is a Held guard released by LOCK_UN on drop. A line k is created once, by link(2) from a flushed temp under the root's .tmp/, and a second create of it is Exists; a pair's lines read only when its directory holds exactly 0.line..(n-1).line (canonical numbers, each at most 4 KiB), else Io; a read-only log appends nothing and makes no chain file; the chain file is made once and read back as a 64-hex digest; the root lists every canonically named pair, sorted, and refuses a non-canonical one, and pairs lists every pair directory by its name with its own result, a name that names no pair first, so one directory's fault is never another's; one writer holds ledger.lock and one holder each store's .lock, for the value's life. A store names only relative, '/'-separated paths with no empty or dot-led part; create is Exists over a name and leaves its bytes; a version directory is made whole or not at all; replace is one rename; a read past 64 KiB is Io; retained sums the non-dot files under versions/, revisions/ and eval/ and counts versions/; no temp is left. A runner's runner.lock holds its pid alone; a session refuses NoRunner unless runner.lock is held and names the session's parent's pid, and the check takes nothing; the runner waits up to its deadline for ledger.lock, each pause between tries by a callback whose error ends the wait, and a free lock is taken with no pause. The seam reports a stop after the n-th write of a step, once it is durable, and a store over these files round-trips, a stop after the revision is StoreTorn and a stop after the pointer is the new state.	src/aos_learn_tests.rs::{every_new_name_is_flushed_durably_in_its_parent,a_new_name_is_flushed_in_its_parent_after_it_is_made,a_session_runs_only_under_its_own_live_runner_and_the_runner_waits_for_the_ledger,a_ledgers_lines_are_made_once_and_read_whole,the_version_root_lists_every_pair_and_refuses_a_stray_name,the_chain_file_is_made_once_by_a_writer,a_practice_ledger_runs_over_the_files,a_store_directory_holds_only_its_own_names,the_seam_stops_after_the_nth_write_of_a_step_once_it_is_durable,a_store_round_trips_over_the_files_and_a_torn_one_is_refused}	files, flock, link(2), rename, Result	0,1,8,13
aos-learn-verbs	contract	aos_learn::{parse,parse_digest,pair_named,cmd_aos_learn,guarded,home}	Each verb takes its own flags alone, a value once, and nothing else: a flag it does not take, a missing required one, a stray argument, a non-canonical number, threads outside 1..=64, a digest not 64 lowercase hex digits, and --replay with --threads are usage errors (exit 2); --help exits 0; a store that is not new is refused before any home or ledger is read, and a rollback of a store that is missing or holds no store.txt is refused Store (exit 1) before any home is read, making nothing in it; every exit of aos-learn and aos-learn-bench, --help's included, passes guarded, which makes any exit a defect (exit 1) when held_out_judged() is not 0. A ledger directory names its pair canonically (pair-i, practice-j, no leading zeros).	src/aos_learn_tests.rs::{each_verb_takes_its_flags_alone,a_digest_is_sixty_four_lowercase_hex_digits,a_ledger_directory_names_its_pair_canonically,a_verb_exits_by_its_named_codes,every_exit_is_checked_against_held_out_judged}	string parsing, Option, Result	0,1,3
aos-learn-summary	contract	aos_learn::{Summary::{text,parse},cost_report,pin_source}	parse(text(s)) = Some(s) for every summary; parse refuses a missing or other header, a missing, extra or reordered line, bits other than 0 and 1, and a wall time without MEASURED. The cost report names each file written with its bytes, the bytes read and files, the retained bytes and versions, and the time MEASURED. The pin source is M6_CHAIN_V1's constant: its digest, then ten 32-byte arrays, the five ids in order and then the five labels in order.	src/aos_learn_tests.rs::{a_summary_round_trips_and_refuses_anything_else,the_cost_report_names_each_file_and_labels_its_time,the_pin_source_is_the_constant_stage_e_commits}	format!, string parsing	0,1,3
aos-learn-bench-gates	contract	aos_learn_bench::{parse,own_build,decide,ours,store_id,heads,head_text,new_file,save,capture,shown,cmd_to,compare_stores,record_text,sidecar_text,Wrote::text,Child::{ok,cause}}	own_build(lab, d, runner) is Ok iff the runner's file reads and its digest under LAB_DOMAIN is d (the lab is the runner's own bytes, so its children may name the runner's commit), else a G0 refusal (exit 1). decide is G0 (a clean runner commit, the children's builds and profiles the runner's, the lab's digest unchanged), G2 (once both finished: the stores equal, and when the ledger was read both children's bits the ledger's look-verdict bits), G3 (held_out_judged 0 in each child that left a summary, and when the ledger was read each finished child's open line on its own store holding the tests log's digest) and G4 (no deadline kill, each finished child's footprint at most 2 GiB and reported). A session whose lines hold no non-replay open (child A's own), or whose ledger was not read, gets no line; else a child's own end names the void (a deadline kill Deadline; an interrupt, a stray signal or a spawn or wait failure Other; any other unclean end ChildFailed), B never run after A finished is Other, an interrupt the runner sees at its last check, made after every other input is read and just before it decides its line, is Other for an opened session when no child's own end named a cause, any gate's miss is Gate, and only a session with every gate passed is Record. compare_stores refuses a store that does not load and an empty eval set, and compares every versions/ and eval/ file byte for byte and the revisions' (reason, active, head), excluding store ids and ledger heads. The runner writes a line only for its own session's lines: none, or a first line that is child A's non-replay open on the store its store.txt names (and its summary, when it left one), so a child A killed before its summary still gets its void. The record says when G2, and G3's open line and G4's footprint for a child that did not finish (and G3's guard for a child not run or that left no summary), were not decided, when G2's bits and G3's open lines were not decided because the ledger was not read (with no FAILED line for either), and when an interrupt came before the runner's line, saying whether a void was written, prints each child's retained bytes and every pair directory's ledger head under its own name (or why its lines do not chain, each directory's fault its own), is written and flushed with the sidecar and the children's captured output, each made new and never truncated, its stdout copy and the help written past a stdout that is gone (no panic, so the exit still passes guarded), is written even when the ledger cannot be taken (no line, said so), and claims only a real pair's session of record whose record line was written; not C1 to C3 when look 2 admitted the control, and not C4 without child A's retained bytes.	src/aos_learn_bench_tests.rs::{a_finished_session_with_every_gate_passed_is_recorded,a_session_stopped_before_its_open_gets_no_line,each_childs_end_names_its_void_cause,each_gates_miss_after_the_children_exit_is_a_gate_void,the_runner_writes_only_for_its_own_sessions_lines,the_record_reads_every_pairs_head_and_names_each_broken_one,a_ledger_not_read_decides_neither_g2s_bits_nor_g3s_opens,an_interrupt_before_the_line_voids_a_finished_session,the_runners_files_are_made_new_and_flushed,the_store_comparison_excludes_ids_and_ledger_heads_alone,the_record_claims_only_a_real_pairs_session_of_record,only_the_runners_own_bytes_run_as_its_lab,the_runner_takes_its_flags_alone}	enum match, Vec, files, string parsing	0,1,3,8
```

```tsv
file	test	primary	also
src/aos_learn_tests.rs	each_verb_takes_its_flags_alone	aos-learn-verbs	
src/aos_learn_tests.rs	a_digest_is_sixty_four_lowercase_hex_digits	aos-learn-verbs	
src/aos_learn_tests.rs	a_ledger_directory_names_its_pair_canonically	aos-learn-verbs	
src/aos_learn_tests.rs	every_new_name_is_flushed_durably_in_its_parent	aos-learn-files	
src/aos_learn_tests.rs	a_new_name_is_flushed_in_its_parent_after_it_is_made	aos-learn-files	
src/aos_learn_tests.rs	a_session_runs_only_under_its_own_live_runner_and_the_runner_waits_for_the_ledger	aos-learn-files	
src/aos_learn_tests.rs	a_ledgers_lines_are_made_once_and_read_whole	aos-learn-files	
src/aos_learn_tests.rs	the_version_root_lists_every_pair_and_refuses_a_stray_name	aos-learn-files	
src/aos_learn_tests.rs	the_chain_file_is_made_once_by_a_writer	aos-learn-files	
src/aos_learn_tests.rs	a_practice_ledger_runs_over_the_files	aos-learn-files	
src/aos_learn_tests.rs	a_store_directory_holds_only_its_own_names	aos-learn-files	
src/aos_learn_tests.rs	the_seam_stops_after_the_nth_write_of_a_step_once_it_is_durable	aos-learn-files	
src/aos_learn_tests.rs	a_store_round_trips_over_the_files_and_a_torn_one_is_refused	aos-learn-files	
src/aos_learn_tests.rs	a_summary_round_trips_and_refuses_anything_else	aos-learn-summary	
src/aos_learn_tests.rs	the_cost_report_names_each_file_and_labels_its_time	aos-learn-summary	
src/aos_learn_tests.rs	the_pin_source_is_the_constant_stage_e_commits	aos-learn-summary	
src/aos_learn_tests.rs	a_verb_exits_by_its_named_codes	aos-learn-verbs	
src/aos_learn_tests.rs	every_exit_is_checked_against_held_out_judged	aos-learn-verbs	
src/aos_learn_bench_tests.rs	a_finished_session_with_every_gate_passed_is_recorded	aos-learn-bench-gates	
src/aos_learn_bench_tests.rs	a_session_stopped_before_its_open_gets_no_line	aos-learn-bench-gates	
src/aos_learn_bench_tests.rs	each_childs_end_names_its_void_cause	aos-learn-bench-gates	
src/aos_learn_bench_tests.rs	each_gates_miss_after_the_children_exit_is_a_gate_void	aos-learn-bench-gates	
src/aos_learn_bench_tests.rs	the_runner_writes_only_for_its_own_sessions_lines	aos-learn-bench-gates	
src/aos_learn_bench_tests.rs	the_record_reads_every_pairs_head_and_names_each_broken_one	aos-learn-bench-gates	
src/aos_learn_bench_tests.rs	a_ledger_not_read_decides_neither_g2s_bits_nor_g3s_opens	aos-learn-bench-gates	
src/aos_learn_bench_tests.rs	an_interrupt_before_the_line_voids_a_finished_session	aos-learn-bench-gates	
src/aos_learn_bench_tests.rs	the_runners_files_are_made_new_and_flushed	aos-learn-bench-gates	
src/aos_learn_bench_tests.rs	the_store_comparison_excludes_ids_and_ledger_heads_alone	aos-learn-bench-gates	
src/aos_learn_bench_tests.rs	the_record_claims_only_a_real_pairs_session_of_record	aos-learn-bench-gates	
src/aos_learn_bench_tests.rs	the_runner_takes_its_flags_alone	aos-learn-bench-gates	
```

## 17. An older lab's Gemini CLI tab

The review of the Gemini CLI removal found that an `env.sh` an older aterm-lab wrote for a
`lab gemini` tab (`AGENT=gemini`) read as Claude Code's: `Run::agent` falls back to `claude` for a
word it does not know, so the lab's verbs drove a Gemini CLI tab with Claude Code's composer reads
and saved its captures under `claude-<version>`. `Run::load`, which every verb that drives a lab's
agent reads it through, now refuses such a record; `down` and `ps`, which act on the lab's jail and
never on its agent, read it through `Run::read`, so that lab can still be listed and taken down.
One new test, read the same way as §1.

```tsv
id	kind	functions	statement	subsumes	difficulty	blocking_gaps
run-retired-agent-refused	contract	run::Run::admitted, run::Run::load, run::Run::read, lab::down, lab::ps_one	∀ r: r.admitted() = Err(e) ⇔ aterm_phase::retired_agent(r.agent) (r.agent ∈ {gemini, gemini-cli}, any case), with e naming the tag, "AGENT=" ++ r.agent, aterm_phase::RETIRED_AGENT, `down <tag>` and `agy <tag>`; otherwise r.admitted() = Ok(r) (the empty word and an unknown word included, which Run::agent still reads as claude). ∀ tag: Run::load(tag) = Run::read(tag) >>= admitted, so no verb that loads a run sees a retired agent's record; lab::down and lab::ps_one read through Run::read and judge only pids and paths.	run.rs::an_older_labs_gemini_cli_tab_is_refused_never_read_as_claude	struct update syntax, String formatting, a cross-crate predicate	1,3
```

```tsv
file	test	primary	also
src/run.rs	an_older_labs_gemini_cli_tab_is_refused_never_read_as_claude	run-retired-agent-refused	
```

## 18. `seamless`'s privacy leg, the carry

`lab seamless` plants a sentinel (`QZP6SECRET<n>`) in each kind of text one instance holds, and the
outgoing process copies every file its update writes before the successor can read one (the keep
seam, `ATERM_DEBUG_SEAMLESS_KEEP`, aterm-gui's `seamless-keep-seam`). The sweep reads the copies.
The session log's own carry (a control sidecar's `log` member) is held to the Host tier: only a
post's key and hop. The session's and terminal's own state, which the update hands over, is held
field by field; a screen may hold the command line the shell echoed. One row, read the same way as
§1.

```tsv
id	kind	functions	statement	subsumes	difficulty	blocking_gaps
seamless-carry-sweep	contract	seamless_carry::{sentinels,read_kept,Kind::of,kept_at,sweep,verdict,Judged::rendered}	∀ kept, planted, left: verdict(kept, planted, left).wrong = [] ⇔ (a) kept holds a manifest; (b) the control sidecar `.s<planted>.ctl` has ≥ 1 `log[]` line and its `log[]` sentinels ⊇ {8, 9}, a manifest's `sessions[].fabric.posts[].body` holds 6, and a screen `.s<planted>.*` holds 1 (the sweep read the log, the waiting post and the screen the plants reached); (c) every sentinel d found at place p of file f has d ∈ kept_at(Kind::of(f), p), where a control sidecar keeps only {8, 9} in `log[]` and 1 under `archive.`, a screen keeps 1, a manifest keeps the title (2), attention (3), topics (4), holds (5), a waiting post's body, key and hop (6, 8, 9) and the screen meta's titles (2) and OSC 633;E command lines (1) in their own fields, a layout keeps titles, attention and holds, and every other place, and a manifest, layout or sidecar that does not parse as its kind (read whole at `-`), keeps none; (d) the directory is 0700 and every file 0600; (e) every name the seam listed was read (no `skip`); (f) left = []. Each failed clause is one `Wrong`, its places and names as data; `rendered` is Ok exactly when none failed.	-	2	-
```

```tsv
file	test	primary	also
src/seamless_carry.rs	the_sentinels_are_read_in_any_case_by_their_digit	seamless-carry-sweep	
src/seamless_carry.rs	a_carry_with_the_session_state_and_a_host_tier_log_passes	seamless-carry-sweep	
src/seamless_carry.rs	a_content_tier_key_in_the_log_carry_is_found	seamless-carry-sweep	
src/seamless_carry.rs	a_sentinel_outside_its_field_or_a_loose_mode_or_a_leftover_fails	seamless-carry-sweep	
src/seamless_carry.rs	a_name_the_seam_did_not_read_fails	seamless-carry-sweep	
src/seamless_carry.rs	a_sweep_that_read_no_planted_path_is_vacuous	seamless-carry-sweep	
src/seamless_carry.rs	the_index_is_read_with_each_file_beside_it	seamless-carry-sweep	
```

## 19. The login a Claude Code capture's header names

Measuring Claude Code 2.1.310's lost logins needed labs with no `apiKeyHelper` (the key removed
from the scratch settings) and with a dead scratch `.credentials.json`; `fixture_header` named
every capture that was not under `claude --subscriber` as taken under "a fake `apiKeyHelper`
key", so those captures' MEASURED headers said a login the lab did not launch with. The header
now reads the launcher, the scratch settings and the scratch credentials file through
`claude_login`. One new test, read the same way as §1.

```tsv
id	kind	functions	statement	subsumes	difficulty	blocking_gaps
verbs-claude-login	contract	verbs::claude_login, verbs::ClaudeLogin::phrase, verbs::fixture_header	∀ launcher, settings, stored: claude_login(launcher, settings, stored) = ClaudeLogin::OAuthToken if launcher contains `CLAUDE_CODE_OAUTH_TOKEN`; else ApiKeyHelper if J::parse(settings) is an object whose `apiKeyHelper` is a string; else StoredCredentials if stored; else NoLogin. Settings that do not parse, or name `apiKeyHelper` as a non-string, are no helper. The four logins' phrases are distinct, and on the stub's route (§20, `verbs-claude-route`) fixture_header names exactly claude_login(the tab's launch record, the run's scratch settings, whether the credentials file its launch reads exists).phrase().	verbs.rs::a_header_names_the_login_the_lab_launched_with	JSON parsing, substring search, an enum with &'static str phrases	1,3
```

```tsv
file	test	primary	also
src/verbs.rs	a_header_names_the_login_the_lab_launched_with	verbs-claude-login	
```

## 20. `claude-login-proof`, and the tab a lab verb reads

`claude-login-proof [tag]` reproduces the hand-run Claude Code 2.1.310 login E2E on a private
headless instance built from this tree: the installed Claude Code on its default route (no
`CLAUDE_CONFIG_DIR`, no `ANTHROPIC_BASE_URL`, no proxy) in a dead-login world the lab lays in its
scratch HOME, four scenarios on two tabs (S1 a lost login a roster row names; S2 one no row names;
S3 `aterm harness accounts add` naming S2's login while aterm's own picker stands; S3b a person's
own `/login` in the same tab), a process watcher for `auth login` and browser or `open` launches
for the whole run, and the lab down by its PIDs on every path but `--keep`. The judges below are
pure functions of the evidence's text (the tab's journal and decision ledger, the acts its tape
holds, the accounts' ledger, the window's `messages.log`, the instance's `aterm.log`, the egress
and open recorders, the keychain tripwire, the jail's profile); the run prints one row each and
exits 1 on any FAIL, under `--keep` too, judged there off the live scratch (no `left` row, the lab
being up). The verb never presses a key in a `/login` picker: each key it sends is fenced on the
composer's own row and pressed once (S1's and S2's prompts and their Enters, S3b's `/login` and
its Enter), and the picker S3b opens is left standing until `down` closes the tab. S3's add lands
within 15 s of one of the host's own looks at the roster (`add_wait`), so its next look is 45 s or
more away and a back-out the add's wake brought is told from one that look brought. The hidden
`--roster-by-hand` writes S3's row into the roster by hand, so no wake marker tells the host, the
live negative control: S3 must FAIL on its look clause and the rest pass. Alongside, `journal`,
`header` and `capture` take a tab (`@<sid>`), and the header names the route and login of the
launch that ran in that tab. Read the same way as §1.

```tsv
id	kind	functions	statement	subsumes	difficulty	blocking_gaps
login-proof-evidence	contract	claude_login_proof::{journal_of,decisions_of,act_line,acts_of,ledger_of,messages_of,log_times,looks_of,names,is_input,is_login_send,is_login_enter,is_fenced_esc}	journal_of(t) = [Said{t_ms: millis(r.t), line: r.line} | l ∈ lines(t), r = J::parse(l) with `t` (integer ms, or seconds with a fraction) and a string `line`], every other line skipped; decisions_of reads `ts`, `rule_id`, `decision` the same way; ledger_of reads `account`, `kind`, `why` (absent = ""); messages_of(t) = [Message{kind, t_ms, title} | l ∈ lines(t) whose tab-separated fields hold `kind=` and an integer `t=`], title "" when absent. log_times(log, n) = [round(1000 · words(l)[0] as seconds) | l ∈ lines(log) holding n whose first word reads as a number], and looks_of(log) = log_times(log, ROSTER_LOOK). act_line(t, ws) = show(t) ++ " " ++ join(" ", [w ∈ ws | w does not start with '@']), and acts_of(act_line(t, ws)) = [Act{t, line: that tail}]. names(line, label) ⇔ label is one maximal run of [A-Za-z0-9._-] in line. is_input(l) ⇔ words(l)[0] ∈ {key, send, paste, turn}; is_login_send(l) ⇔ l starts "send ", holds " if=" and ends " -- /login"; is_login_enter(l) ⇔ l starts "key ", ends " enter" and a field of split(l, ' ') (ASCII spaces alone, so a no-break space stays inside its field) starting `if=` holds "/login"; is_fenced_esc(l) ⇔ l starts "key ", ends " esc" and a word starts "if-human=".	-	1	-
login-proof-s1	contract	claude_login_proof::judge_s1	judge_s1(journal, acts, ledger, label).verdict = Ok ⇔ some `WAITING ` record names label; no `TYPED `, `EXCEPTION ` or `ESCALATED ` record; no act is_input; and ledger holds (label, auth, why = signed-out), (label, login-ended, _) and (label, sign-in, why ∋ "no-route"); else Fail, each missing clause named in the evidence.	-	1	-
login-proof-s2	contract	claude_login_proof::judge_s2	With I = the inputs of acts at or before `to` (S3's mark): judge_s2(journal, acts, messages, to).verdict = Ok ⇔ exactly one i ∈ I is_login_send, exactly one is_login_enter, and no other i ∈ I; exactly one `TYPED ` record at or before `to` holds " rule=" ++ RULE_LOGIN ++ " "; an `EXCEPTION ` record at or before `to` holds " kind=login " and " notice=" ++ LOGIN_CHOOSE.id; and a message went `posted` at or before `to` whose title starts LOGIN_CHOOSE.title.	-	1	-
login-proof-s3	contract	claude_login_proof::judge_s3	With W = the inputs within [from, next] (the add, and S3b's start), e the first of W that is_fenced_esc, and L the least of looks at or after from: judge_s3((journal, decisions, acts), (ledger, messages, looks), label, (from, next)).verdict = Ok ⇔ e exists and |W| = 1; exactly one decision within [from, next] has rule RULE_LOGIN_BACK_OUT and decision `approved`; L exists and e.t < L (the back-out came before the host's own look at the roster, so the add's wake brought it); within [e.t, next] a `CLEARED ` record holds " exception=login " and ends policy::exception::LOGIN_BACKED_OUT, and a `WAITING ` record names label; no `TYPED ` record of RULE_LOGIN within [from, next]; ledger holds (label, sign-in, why ∋ "no-route"); and a message went `retired` within [e.t, next] whose title starts LOGIN_CHOOSE.title.	-	1	-
login-proof-add	contract	claude_login_proof::add_wait	With T = the greatest of log_times(log, WAKE_ARMED) ++ looks_of(log) and φ = (now − T) mod HOST_LOOK_EVERY (in [0, 60 s)): add_wait(log, now) = None if T does not exist or φ ≤ S3_ADD_AFTER_LOOK (15 s); else Some(HOST_LOOK_EVERY − φ + LOOK_SETTLE), so the add lands LOOK_SETTLE past the host's next look.	-	1	-
login-proof-s3b	contract	claude_login_proof::{judge_s3b,s3b_grace}	judge_s3b((journal, decisions, acts), messages, (from, to), (watched, grace)).verdict = Ok ⇔ no input within [from, to] (the person's Enter, and the watch's end); no decision within it is `approved` and one is `escalated`; no `TYPED ` record within it; no message went `posted` within it titled LOGIN_CHOOSE.title…; and watched ≥ grace. s3b_grace(cfg) = cfg.human_grace_s s + TurnEndTiming::default().take_within, read from the lab's own `aterm.toml`, so the watch the verb keeps (grace + 15 s) exceeds the supervisor's back-off however it is configured.	-	1	-
login-proof-safety	contract	claude_login_proof::{judge_procs,judge_egress,judge_keychain,judge_owner,suspect}	suspect(c) = "auth login" if c holds " auth login"; else "open" if the basename of words(c)[0] ∈ {open, osascript} or c holds ".app/Contents/MacOS/"; else None. judge_procs(s, control, n) = Fail if s ≠ []; else Ok if control (the watcher saw its own jailed `auth login` control); else Unproven. judge_egress(sockets, egress) = Fail if a line of either holds "EXTERNAL"; else Ok if sockets has a line; else Unproven. judge_keychain(log) over S = the quoted `service="…"` of each line of log = Fail if some s ∈ S is in OWNER_ITEMS (`Claude Code`, `Claude Code-credentials`, the owner's un-suffixed items); else Unproven if S = []; else Ok. judge_owner(sb, homes) = Ok ⇔ homes ≠ [] ∧ ∀ h ∈ homes: sb holds `(deny file-write* (subpath "h"))`; else Fail.	-	1	-
verbs-tab	contract	verbs::{take_tab,ls_lists,active_in,named_or_first,cmd_journal,cmd_header,cmd_capture}	take_tab(args) (the words after the tag) removes and returns args[0]'s tail when args[0] starts '@' and that tail is 1–64 chars of [A-Za-z0-9_-]; a tail of another shape is a usage error; an args[0] not starting '@' (or none) is Ok(None) and leaves args as they were, a later word starting '@' included. ls_lists(ls, sid) ⇔ some line has words[2] = sid. active_in(ls) = words[2] of the first line with a word `active=1`, else None. `journal`, `header` and `capture` read the tab take_tab names, refused when `ls` lists no such tab; with none named `journal` reads the first tab (`sid`) and `header` and `capture` the active one.	-	1	-
verbs-claude-route	contract	verbs::{claude_route,launcher_value,stored_login_dead,launched,Launched::sentence,claude_launch,launch_record,record_launch,fixture_header_on}	claude_route(l) = Stub if trim(l) = "" or l holds "ANTHROPIC_BASE_URL="; else Default. launcher_value(l, k) = the value after the first `k=` that starts l or follows whitespace or a quote, up to the next quote when it opens with one, else to the next whitespace. stored_login_dead(t) ⇔ J::parse(t) has a `claudeAiOauth` whose `accessToken` and `refreshToken` are absent or "" and whose `expiresAt` is absent or ≤ 0. launched(l, settings, stored) = Launched{route: claude_route(l), login: claude_login(l, settings, stored ≠ None), stored: on Default with login StoredCredentials Some((SecureStorage if launcher_value(l, LOGIN_ENV) exists else ScratchClaude, stored_login_dead(stored))), else None}, and claude_launch(l, settings, stored) = launched(l, settings, stored).sentence(): on Stub the §19 sentence over the login's phrase(); on Default one naming the default route (no `CLAUDE_CONFIG_DIR`, `ANTHROPIC_BASE_URL` or proxy, the scratch HOME, the jail) and, where stored is Some, the login directory (naming LOGIN_ENV exactly when SecureStorage) and whether its login is dead; distinct Launched values the lab makes have distinct sentences. launch_record(run, sid) = the text record_launch kept for sid, else the lab's `claude.sh`, else ""; fixture_header_on reads the tab's dims and launch record and, on Default, the credentials under launcher_value(l, LOGIN_ENV) (else the scratch `.claude`).	-	1	-
```

```tsv
file	test	primary	also
src/claude_login_proof_tests.rs	the_evidence_reads_the_harness_records_it_is_kept_in	login-proof-evidence	
src/claude_login_proof_tests.rs	s1_holds_on_a_roster_rows_wait_and_fails_on_a_login_typed_or_told	login-proof-s1	login-proof-evidence
src/claude_login_proof_tests.rs	s2_holds_on_one_fenced_login_and_enter_and_fails_on_a_key_in_the_picker	login-proof-s2	login-proof-evidence
src/claude_login_proof_tests.rs	s3_holds_on_the_back_out_before_the_hosts_look_and_fails_without_it	login-proof-s3	login-proof-evidence
src/claude_login_proof_tests.rs	the_hosts_looks_at_the_roster_are_read_off_its_log_and_place_the_add	login-proof-add	login-proof-evidence
src/claude_login_proof_tests.rs	s3b_holds_on_a_picker_left_alone_past_the_grace_and_fails_on_a_key_or_a_short_watch	login-proof-s3b	login-proof-evidence
src/claude_login_proof_tests.rs	s3b_grace_is_the_supervisors_own_and_moves_with_it	login-proof-s3b	
src/claude_login_proof_tests.rs	the_safety_judges_fail_on_a_sighting_an_external_socket_or_an_open_home	login-proof-safety	
src/claude_login_proof_tests.rs	the_watcher_suspects_auth_login_and_opens_and_nothing_else	login-proof-safety	
src/claude_login_proof_tests.rs	the_keychain_judge_fails_on_the_owners_own_items_and_proves_nothing_unasked	login-proof-safety	
src/verbs.rs	a_verb_names_its_tab_by_sid_and_reads_the_active_one_otherwise	verbs-tab	
src/verbs.rs	a_header_names_the_route_the_tab_was_launched_on	verbs-claude-route	verbs-claude-login
```

## 21. `claude-rotation-proof`, aterm moving the real Claude Code between accounts

`claude-rotation-proof [tag]` runs the installed Claude Code on a private headless instance built
from this tree, on the lab's stub, which the instance reads as Claude Code's default route only
through its development seam (`ATERM_DEBUG_LAB_DEFAULT_ROUTE`, admitted for the stub's exact
loopback URL alone; `--no-route-seam` brings the lab up without it). The aterm image must be at
a persistent path: the verb rejects temporary and cache-tagged images before starting a lab,
because account restarts correctly decline to persist their usage-tap command. The world is the owner's
shape, laid in the scratch alone: a fleet file beside the roster lists three dedicated rows of one
machine, opted in by name, and the instance's own sync writes the roster from it (`lab-a` the
default login: the scratch HOME's `.claude` and the profile naming its address; `lab-b` and
`lab-c` on their login directories of the accounts store; `lab-c` holds no login until R2). Each
live fake login's token the stub reads by its label (`oat:lab-a`, never its value), each
credential carries its own usage headers and its own mode (`headers-for`, `mode-for`), and no
mailbox and no `apiKeyHelper` is laid. One tab runs under cruise; the scenarios chain on it: R1 a
wall moves it onto `lab-b`; R3 `lab-b`'s wall with `lab-a`'s standing waits for `lab-a`'s opening
and moves back; R4 `lab-a`'s wall with `lab-b`'s login directory gone tries `lab-b`'s automatic
sign-in and resumes at its own reset; R5a and R5b a sign-in on the default login, written as
`/login` writes it (the login, then the profile; nothing is pressed), makes it `lab-b`'s, then
`lab-a`'s again (the owner's 2026-10-26 incident: the row made the default login's again had been
displaced, signed out, and failed its automatic sign-in); R2 a 95 % five-hour figure moves the tab
onto `lab-c` at a turn end. Each key the verb sends is fenced on the composer's own row and pressed
once; nothing is pressed in a `/login` picker. A process watcher records each Claude Code process
the instance starts and the login its exec environment names; the open recorder, the egress
witness, the keychain tripwire (each item asked for judged by the launch that asked), the jail's
rules and the owner's homes are judged as §20 judges them, and the lab goes down by its PIDs on
every path but `--keep`. The judges are pure functions of the evidence's text; the run prints one
row each and exits 1 on any FAIL. Under `--no-route-seam` (the live negative control) R1 must FAIL
on its move and the later scenarios are not run; a `control` row judges that R1 failed for the
route alone (P1 served on `oat:lab-a`, `lab-a` walled, the host's `stay (not-a-stored-login)` or
`account wall not-stored` logged, no restart, no turn on another row, every safety row holding),
and the run exits 0 only where that row holds, 1 otherwise. Read the same way as §1.

```tsv
id	kind	functions	statement	subsumes	difficulty	blocking_gaps
rotation-proof-evidence	contract	claude_rotation_proof::{rows_of,served_of,stamp_ms,launch_line,launches_of,waiting_until,waiting_why,advice_until,listed,restarted,typed_for,drive_steps_of,notes_of}	rows_of(t) = [Row{t, account, kind, to, why} | l ∈ lines(t), r = J::parse(l)], a missing or null field read as "" (t as 0). drive_steps_of(t) = [DriveStep{t, step, tab, pid} | l ∈ lines(t), r = J::parse(l)] likewise. notes_of(t) = one Note per `m1` line of messages.log with a `kind`, its tab-separated `k=v` fields unescaped (`\\`, `\t`, `\n`, `\r`, and `\u` the unit separator detail's lines are split on): kind, id, t, title, detail, key, sev, later ⇔ `need=later`, how, record ⇔ `rec=1`, rep, and a dropped line's n; another line reads as nothing. waiting_why(l) = the words after a WAITING record's `until=<instant>`. served_of(log, day) = one Served per line `HH:MM:SS.mmm POST /v1/messages -> <status> …` with t_ms = day + stamp_ms(stamp) (+ 24 h for each stamp more than 12 h before the one before it), kind/auth/mode = the words after `kind=`, `auth=`, `mode=`, last_user = the text between `last_user="` and the last `" auth=`; stamp_ms("HH:MM:SS.mmm") = ((H·60+M)·60+S)·1000+ms for H < 24, M < 60, S < 61, ms < 1000, else None. launches_of(launch_line(l)) = [l], and a line of another shape reads as nothing. waiting_until(l) = workers::iso_ms(the word after `until=`), the UTC civil instant of `YYYY-MM-DDTHH:MM:SS[.fff]Z`, else None. advice_until(log, label, from) = the `(until <s>)` of the first `: account advice: wait on <label> (until <s>)` line stamped at or after from. listed(json, label) = the `auth`, `login`, `condition` and `sign_in_attempt.outcome` of the row `label` of `aterm harness accounts --json`'s `accounts`, else None. restarted(j, to, w) = the first `RESTARTED ` record within w holding ` rule=account-rotate@v1 relaunch under account ` ++ to and naming to; typed_for(l, rule) ⇔ l starts `TYPED ` or `CONTINUED ` and a word is `rule=` ++ rule.	-	1	-
rotation-proof-world	contract	claude_rotation_proof::{live_login,fleet_text,roster_lacks}	live_login(label, now) is one JSON line whose claudeAiOauth.accessToken = stub::labelled_oauth_token(label) (so stub::auth_label reads "oat:" ++ label), its refresh token a lab word, expiresAt = now + 365 days. fleet_text() parses as a FleetRoster whose accounts_for_machine(MACHINE) are lab-a, lab-b, lab-c, each dedicated, each with its email, and whose opt_in is `named`. roster_lacks(t, (b, c)) = None ⇔ t parses as a Roster whose default login's row (dir None) is lab-a and whose lab-b and lab-c name exactly b and c; else the first thing it lacks.	-	1	-
rotation-proof-r1	contract	claude_rotation_proof::{judge_r1,judge_wall_move,stepped_wrong}	judge_wall_move(ev, leg, (from, to, auth), P).verdict = Ok ⇔ within leg's window aterm.log holds `account advice: move to <to>`; the ledger books a five_hour `wall` for from, a `rotate` from → to and then a `proven` for to; the journal holds restarted(…, to, window), its line saying `(from the limit its turn ended on; the same conversation)`, and a usage-resume@v1 act after it; stepped_wrong(ev, `restart:account`, window) = None, that is the upgrade ledger holds a row of the tab (ev.sid) whose step is exactly `restart:account` (not `restart:account-ahead`) stamped within the window (its whole second), and the first row of the tab after it that is `adopted` or another `restart:` step is an `adopted` of a pid aterm.log's adopted lines of the window name; adopted_wrong(ev, to, window) = None, that is aterm.log's `restart:account pid=N … step=adopted` (or `restart:account-ahead pid=N …`, a move ahead of a wall) within the window names a pid some launch has, and that launch's login is to's (the restart starts its process before the `RESTARTED` record, so the pid, not the time, ties the two); the first 200 `main` turn served after the restart carried auth; and leg.history holds each prompt of P; else Fail naming each missing clause. judge_r1 = judge_wall_move(…, (lab-a, lab-b, oat:lab-b), [P1, P2]).	-	1	-
rotation-proof-r3	contract	claude_rotation_proof::judge_r3	judge_r3(ev, leg, a_reset).verdict = Ok ⇔ advice_until(log, lab-b, leg.from) = Some(u) with a_reset ≤ u ≤ a_reset + 600; a `WAITING ` record within the window names an instant within 120 s of u; the restart onto lab-a comes no earlier than u − 5 s; and judge_wall_move(ev, leg, (lab-b, lab-a, oat:lab-a), [P1, P2, P3]) is Ok; else Fail.	-	1	-
rotation-proof-r4	contract	claude_rotation_proof::{judge_r4,r4_wait_past_reset}	judge_r4(ev, leg).verdict = Ok ⇔ no account restart within the window; the ledger books a `sign-in` for lab-b at or after leg.from whose why starts `auto `; a `WAITING ` record within the window whose reason (waiting_why) starts `usage-session` names an instant from leg.reset − 60 s to leg.reset + max(auto_resume_grace, reset_grace + reset_jitter) + 60 s (TurnEndTiming's defaults: the host's own resume deadline, never a weekly or other far instant); no usage-resume@v1 act is typed from leg.from to before leg.reset − 5 s; the first 200 `main` turn served at or after leg.reset − 5 s carried oat:lab-a (Claude Code's own continuation at the reset it named, or the host's resume once that is later than `auto_resume_grace`; the verdict's words name which); and leg.history holds P1 and P4; else Fail.	-	1	-
rotation-proof-r5	contract	claude_rotation_proof::{judge_r5,failing}	With B = listed(leg.before, d.to) and A = listed(leg.listing, d.to): judge_r5(ev, leg, d) = Fail unless the ledger books d.from `displaced` to d.to at or after leg.from and through leg.to; where B.auth = signed-out, an `auth` row `ok` for d.to through leg.to; no `sign-in` row for d.to at or after the displacement through leg.to (a later leg may displace that account again); no account restart within the window; A exists with auth = ok, login ≠ expired and ¬failing(A.sign_in) (failing(o) ⇔ o ∈ {no-route, failed}); the first 200 `main` turn served at or after leg.asked (else leg.from) carried d.auth; and leg.history holds each of d.prompts. Where every clause holds it is Ok if B shows auth signed-out or failing(B.sign_in), else Unproven (nothing stale stood against d.to, so a sync with no fix would pass too).	-	1	-
rotation-proof-r2	contract	claude_rotation_proof::{judge_r2,session_pct}	judge_r2(ev, leg).verdict = Ok ⇔ the ledger books a `sample` row of lab-a whose why starts `tap: ` and whose `session N%` has N ≥ 90, at or after leg.from and no later than the `rotate` lab-a → lab-c, and then a `proven` for lab-c (a `warned` row, Claude Code's own warning read off its screen, never stands for the tap's sample; the evidence says whether one was booked); restarted(…, lab-c, window) exists and says `(at a turn end; the same conversation)`; stepped_wrong(ev, `restart:account-ahead`, window) = None; the stub gave oat:lab-a no 429 within the window; adopted_wrong(ev, lab-c, window) = None; the first 200 `main` turn served after it (the lab's P8, typed once the move came at P7's turn end) carried oat:lab-c; and leg.history holds P1, P7 and P8; else Fail.	-	1	-
rotation-proof-stub	contract	claude_rotation_proof::judge_stub	judge_stub(S) = Fail if some s ∈ S has auth ∉ {oat:lab-a, oat:lab-b, oat:lab-c}; else Unproven if one of the three carried no request; else Ok.	-	1	-
rotation-proof-asked	contract	claude_rotation_proof::{judge_asked,names_login}	judge_asked(j, N) reads the instance's whole messages.log (N = notes_of, every line from the instance's start) and the tab's whole journal j, whenever each line was written. For each posted note p, with e the last retired note of p's id: a Fail where p's or e's title or detail names `/login`, or p's or e's sev is `error`; else where p.later (a fact for later), a Fail where p.key starts `harness.accounts.` and is not `harness.accounts.no-sign-in` (the recorded no-route error after every route was tried), else allowed and named in the evidence with e.rep (`open` where no e) and, for the no-route error, the rows its detail names; else where e.record (retired `folded` with `rec=1`) allowed and counted; else a Fail (it stood on the owner's screen). A Fail too where a record of j starts `EXCEPTION `, `ESCALATED `, `NEEDS ` or `LEFT ` or is typed_for RULE_LOGIN. Ok where nothing failed and no `dropped` line stands; Unproven where nothing failed but the log dropped lines.	-	1	-
rotation-proof-keys	contract	claude_rotation_proof::{judge_keys,composer_send,composer_enter}	judge_keys(j, acts) = Ok ⇔ every `TYPED `/`CONTINUED ` record of j is typed for a rule of ALLOWED_RULES (the account move, the limit's resume, and cruise's continuations), and every input of acts, in order, is either composer_send (a `send` whose words before ` -- ` are fences alone, one `if-human=` and one `if=` ++ row_guard("❯"), the empty composer, its text no slash command) or, right after one with text T, composer_enter(·, T) (a `key … enter` whose words between are fences alone, one `if-human=`, and whose `if=` guard, past `^` and any `\x20{N}` indent and before `\s*$`, is a part of row_guard("❯" ++ U+00A0 ++ T) between its `^` and `\s*$` that no prefix of row_guard("❯" ++ U+00A0)'s part is, so no Enter on an empty composer passes), each send entered at most once; any other input (another key, an Enter on another row, a second Enter, a paste, a turn), and any that is_login_send, is_login_enter or ends ` -- /usage`, is a Fail.	-	1	-
rotation-proof-control	contract	claude_rotation_proof::{judge_control,SAFETY_ROWS}	judge_control(ev, start, r1, rows).verdict = Ok ⇔ the first `main` 200 turn served at or after start carried oat:lab-a; the stub gave oat:lab-a a 429 within r1's window; aterm.log holds, from start through r1.to, `: account advice: stay (` ++ StayWhy::NotAStoredLogin.as_str() ++ `)` or `: account wall ` ++ Recorded::NotStored.as_str(); the journal holds no `RESTARTED … rule=account-rotate@v1` record at all; no turn was served on oat:lab-b or oat:lab-c; rows' R1 is a Fail; and each row of rows whose id is one of SAFETY_ROWS is Ok, the keychain's Ok or Unproven; else Fail naming each clause. Under --no-route-seam the verb inserts it after R1 and exits 0 ⇔ it is Ok.	-	1	-
rotation-proof-keychain	contract	claude_rotation_proof::judge_keychain	For each line of the tripwire's log with a quoted service s and `ppid=` p, and L the launch whose pid is p: a Fail where L is on the default login and s ∉ OWNER_ITEMS, where L is on a login directory and s ∈ OWNER_ITEMS, or where no launch is p's and s ∈ OWNER_ITEMS; a Fail where the jail's profile lacks a rule of KEYCHAIN_DENIALS (`security`'s exec, the security daemon's lookup, the owner's Keychains folder). Else Ok where both a default-login launch asked for an OWNER_ITEMS item and a login directory's launch for a suffixed one, Unproven otherwise.	-	1	-
```

```tsv
file	test	primary	also
src/claude_rotation_proof_tests.rs	a_stub_stamp_reads_as_ms_into_its_day_and_nothing_else_does	rotation-proof-evidence	
src/claude_rotation_proof_tests.rs	the_stub_log_reads_as_its_answers_each_with_its_credential	rotation-proof-evidence	
src/claude_rotation_proof_tests.rs	the_ledger_journal_and_log_read_as_the_judges_need_them	rotation-proof-evidence	
src/claude_rotation_proof_tests.rs	a_launch_line_reads_back_as_itself	rotation-proof-evidence	
src/claude_rotation_proof_tests.rs	the_instances_listing_reads_as_each_rows_words	rotation-proof-evidence	
src/claude_rotation_proof_tests.rs	each_live_login_carries_a_token_the_stub_reads_as_its_label_alone	rotation-proof-world	stub-auth-label
src/claude_rotation_proof_tests.rs	the_fleet_names_three_rows_of_one_machine_and_the_synced_roster_is_checked_against_it	rotation-proof-world	
src/claude_rotation_proof_tests.rs	r1_passes_on_the_runs_own_move_and_fails_on_each_fact_taken_away	rotation-proof-r1	rotation-proof-evidence
src/claude_rotation_proof_tests.rs	r3_waits_for_the_first_row_to_open_and_fails_on_an_early_move_or_another_instant	rotation-proof-r3	rotation-proof-r1
src/claude_rotation_proof_tests.rs	r4_passes_on_the_runs_own_wait_and_sign_in_and_fails_on_a_move_or_none	rotation-proof-r4	rotation-proof-evidence
src/claude_rotation_proof_tests.rs	r5_passes_on_the_runs_own_displacements_and_fails_on_a_stale_row_or_a_turn_elsewhere	rotation-proof-r5	rotation-proof-evidence
src/claude_rotation_proof_tests.rs	r5_keeps_later_legs_sign_ins_and_witnesses_out_of_its_verdict	rotation-proof-r5	rotation-proof-evidence
src/claude_rotation_proof_tests.rs	r2_passes_on_the_taps_move_ahead_and_fails_on_a_screen_warning_alone_a_wall_or_another_row	rotation-proof-r2	rotation-proof-evidence, rotation-proof-r1
src/claude_rotation_proof_tests.rs	the_stub_row_holds_only_where_every_request_carried_a_roster_rows_credential	rotation-proof-stub	
src/claude_rotation_proof_tests.rs	messages_log_reads_as_its_posts_retirements_and_folds	rotation-proof-evidence	
src/claude_rotation_proof_tests.rs	nobody_asked_reads_the_whole_log_and_fails_on_an_ask_an_error_or_a_login	rotation-proof-asked	rotation-proof-evidence
src/claude_rotation_proof_tests.rs	the_keys_hold_only_the_moves_resumes_and_cruises_rules_and_no_picker_key	rotation-proof-keys	
src/claude_rotation_proof_tests.rs	the_keychain_judge_reads_each_ask_by_the_launch_that_made_it	rotation-proof-keychain	
src/claude_rotation_proof_tests.rs	the_control_holds_only_where_r1_failed_for_the_route_alone	rotation-proof-control	rotation-proof-r1
src/claude_rotation_proof_tests.rs	the_verbs_exit_is_the_tables_and_under_the_control_the_control_rows	rotation-proof-control	
```

## 22. `swarm partition`, the lab partition lane (fluid swarm F8)

`swarm partition [--one-url] [--writers both|a] [--minter-away] [--hold SECS] [--label WORD]` splits
seven private headless aterms (a4 joined the fleet at a2, b4 at b2, and b3 joins at b1 during the
cut) and one vault-only Mac on this Mac's loopback into two islands with aterm-uds's partition seam
(`ATERM_DEBUG_PARTITION`, a file of `ip:port` lines that aterm-net's and aterm-http's connect
funnels read on every dial of a debug build), lets
each island write and admit, heals, and judges the vault and the fleet. Each Mac's subscriber and
approver is `aterm_secrets::client::watch` in a lab process (`partition-agent`), whose converge
runner is the lab's `init --converge` stand-in, which records the start and runs no pass. The
judge is a pure function of the evidence; the run prints one row per check and exits 1 on any
FAIL, 3 when the lane could not run (a fleet not whole within a minute before the cut is one:
a1's and b1's surveys must hear a1, a2, b1 and b2; every fleet Mac's survey hearing every other
is waited for too, but is evidence, not a gate). Every fleet Mac surveys before, during and after
the cut, every member drives every other at the end, and each member's own `fleet-answered`
stamps, read from the fleet's forming until the first survey before the cut and again after the
heal before any member surveys (a survey stamps who answered it too), say whom its heartbeat
reached and which members its last dial missed; a2's, b1's and b2's heartbeats back at a1 are read
from the same stamps, never from a1's book, whose `seen_ms` for a member a merge of any other
member's book moves. The instances run with `ATERM_DEBUG_FLEET_JOINED_PER_ADDRESS=16`
(aterm-net's debug-only seam that raises, never lowers, the four members a book records from one
address), since every lab Mac registers from 127.0.0.1. `--minter-away` kills a1's instance
before the heal. `--one-url` (no `aterm-swarm-policy` server list) is F8's negative control: `one-head` must
FAIL.
Lab instances run no Commons runtime and no harness, so the Commons plane runs the repair exchange
in the lab process on each Mac's own custody (pairing Macs of one island during the cut, across
after the heal, then round a ring of every enrolled Mac until they read alike, a pass moves
nothing with every exchange run, or one pass per Mac has run), and the accounts plane is a model run of main's account claims on two Macs' table
replicas on a simulated clock (a cut longer than the 20 minutes an unheard Mac's lease lives). Read the same way as §1.

```tsv
id	kind	functions	statement	subsumes	difficulty	blocking_gaps
partition-vault	contract	swarm_partition::judge	Of judge(ev)'s vault rows, with expected = ev.vault.members_before ∪ {every Mac in during the cut}: `cut held` ⇔ ev.vault.end_of_cut has both islands, no island view lists a crossed value, every member of each held its island's server's digest, and the two servers' digests differ where an island set anything; `kept writing` ⇔ each island set at least one entry, every set exited 0, and every Mac that asked was posted and in during the cut; ONE_HEAD ⇔ healed = Some((_, h)) and every server and every expected Mac of the run's end reads `in` and holds h's digest; `nothing lost` ⇔ every entry set during the cut was looked for after the heal and is on every expected Mac, and every expected Mac is still `in`; `asked in` ⇔ every Mac that asked during the cut is `in` at the end.	-	1	-
partition-fleet	contract	swarm_partition::{judge,members_after}	Of judge(ev)'s fleet rows, with M = members_after(ev.fleet) = ev.fleet.before_members less ev.fleet.away, then b3 where some b3 join joined: FLEET_CUT_HELD ⇔ some survey early in the cut came from each island, each heard itself and no Mac of the other island, and no drive across the cut early in it exited 0; `drove its own` ⇔ late drives ran and each exited 0; `b3 joined` ⇔ b3's join early in the cut joined; `spread before` ⇔ before_members ≠ ∅ and each of them has a survey before the cut whose last one heard every one of them; `reached every member before the cut` ⇔ |before_members| ≥ 2 and each of them has a Reach in reach_before whose `reached` holds every other one of them; `whole again` ⇔ M ≠ ∅, each m ∈ M has a survey after the heal whose last one heard every member of M, every heartbeat came back after the heal, every drive after the heal exited 0, and some b3 join joined; `drove every other` ⇔ |M| ≥ 2 and for each ordered pair of distinct members of M the last drive at the end between them exited 0; `reached every member` ⇔ |M| ≥ 2 and each m ∈ M has a Reach whose `reached` holds every other member of M. A run measures the build (unmeasured(ev, judge(ev)) = ∅, which its record says either way, and the console names each shortfall when there is one) only where CUT_HELD and FLEET_CUT_HELD both pass.	-	1	-
partition-commons	contract	swarm_partition::commons::judge	Of commons::judge(ev): skipped = Some(why) ⇒ one FAIL row naming why (a failed enrolment or write before the cut is one); else `kept working` ⇔ each island wrote its `-in-cut` row and let a newcomer in, none with an error; `one history` ⇔ some Mac was read after the heal and every one is `ready` on one head, the same head; `rows` ⇔ some Mac was read and every one reads every row a write succeeded for.	-	1	-
partition-repair	contract	swarm_partition::commons::{alike,settled}	alike(reads) ⇔ every read holds the first's state, heads, rows and unreadable reason (true of no read and of one); settled(before, after, failed) ⇔ alike(after) ∨ (after = before ∧ failed = 0).	-	1	-
partition-accounts	contract	swarm_partition::accounts::judge	Of accounts::judge(ev): `one user` ⇔ the `lease` scenario ran and its last users set has exactly one Mac; `kept through the cut` ⇔ it ran and b2 never stopped, or stopped at or after the heal; `claims before use` ⇔ it ran and no scenario recorded a use while the user's own row held no live claim on the account; `home blocks a cold claim` ⇔ the `home` scenario ran and no instant had two users.	-	1	-
partition-parse	contract	swarm_partition::{parse,record_arm}	∀ args: parse(args) = Ok(o) ⇔ args matches ("--one-url" | "--minter-away" | "--keep" | "--aterm" P | "--writers" ("both" | "a") | "--hold" N | "--label" W)* with each N a whole number of seconds in 10..=600 and each W 1 to 40 of `[a-z0-9-]`, neither first nor last `-`; o starts as {aterm: None, publish: Policy, writers: Both, minter_away: false, hold: DEFAULT_HOLD (80 s), keep: false, label: None}, each flag sets its field, a later one overwriting an earlier one ("--one-url" ⇒ publish = OneUrl); "-h"/"--help", any other word, or a flag missing or with a bad value ⇒ Err(Fail::usage(..)) (code 2). record_arm(o) = ("-one-url" where OneUrl) ++ ("-writers-a" where Writers::A) ++ ("-minter-away" where set) ++ ("-" W where labelled), with the flags that chose each.	-	1	-
```

```tsv
file	test	primary	also
src/swarm_partition_tests.rs	a_run_where_every_part_held_passes_every_check	partition-vault	partition-fleet
src/swarm_partition_tests.rs	the_one_url_publish_never_converges_and_fails_one_head	partition-vault	
src/swarm_partition_tests.rs	one_head_that_splits_again_before_the_runs_end_fails_one_head	partition-vault	
src/swarm_partition_tests.rs	an_entry_a_merge_dropped_is_a_loss	partition-vault	
src/swarm_partition_tests.rs	a_set_never_looked_for_after_the_heal_is_a_loss_not_a_pass	partition-vault	
src/swarm_partition_tests.rs	a_mac_admitted_in_the_cut_and_out_after_the_heal_is_a_loss	partition-vault	
src/swarm_partition_tests.rs	an_island_that_could_not_write_or_admit_did_not_keep_working	partition-vault	
src/swarm_partition_tests.rs	a_member_holding_the_other_islands_head_is_a_leaked_cut	partition-vault	
src/swarm_partition_tests.rs	a_survey_or_drive_across_the_cut_is_a_leaked_cut	partition-fleet	
src/swarm_partition_tests.rs	a_drive_refused_once_the_copy_aged_out_fails_the_island_that_lost_its_issuer	partition-fleet	
src/swarm_partition_tests.rs	a_fleet_that_stays_split_after_the_heal_is_not_whole	partition-fleet	
src/swarm_partition_tests.rs	parse_reads_each_arm_and_refuses_what_it_does_not_know	partition-parse	
src/swarm_partition_tests.rs	a_member_before_the_cut_that_the_heal_left_out_fails_one_head_and_is_lost	partition-vault	
src/swarm_partition_tests.rs	a_value_that_crossed_the_cut_is_a_leak_though_the_servers_differ	partition-vault	
src/swarm_partition_tests.rs	b3_joined_and_unheard_after_the_heal_is_not_whole	partition-fleet	
src/swarm_partition_tests.rs	a_cut_surveyed_from_one_island_alone_is_not_shown_to_hold	partition-fleet	
src/swarm_partition_tests.rs	a_member_that_never_surveyed_after_the_heal_is_not_shown_whole	partition-fleet	
src/swarm_partition_tests.rs	macs_that_joined_at_members_and_never_spread_fail_the_survey_the_drives_and_the_reach	partition-fleet	
src/swarm_partition_tests.rs	a_pair_never_driven_fails_every_mac_drove_every_other	partition-fleet	
src/swarm_partition_tests.rs	a_heartbeat_that_asks_the_same_three_members_never_reaches_the_rest	partition-fleet	
src/swarm_partition_tests.rs	a_mac_unheard_before_the_cut_fails_the_spread_before_it	partition-fleet	
src/swarm_partition_tests.rs	with_the_minter_away_the_rest_must_still_spread	partition-fleet	
src/swarm_partition_tests.rs	the_minter_away_and_label_flags_read_and_name_the_record	partition-parse	
src/swarm_partition_tests.rs	a_heartbeat_that_asks_the_same_three_members_before_the_cut_fails_the_reach_before_it	partition-fleet	
src/swarm_partition_tests.rs	a_member_passed_over_after_a_missed_dial_is_told_from_one_never_asked	partition-fleet	
src/swarm_partition_tests.rs	a_heartbeat_answer_is_read_from_the_heartbeating_macs_own_stamps	partition-fleet	
src/swarm_partition_commons_tests.rs	a_whole_commons_passes_every_check	partition-commons	
src/swarm_partition_commons_tests.rs	two_islands_that_each_let_a_mac_in_and_never_recover_fail_one_history_and_the_rows	partition-commons	
src/swarm_partition_commons_tests.rs	macs_each_ready_on_a_different_head_are_not_one_history	partition-commons	
src/swarm_partition_commons_tests.rs	a_row_missing_on_one_mac_is_lost	partition-commons	
src/swarm_partition_commons_tests.rs	an_island_that_could_not_let_its_newcomer_in_or_write_did_not_keep_working	partition-commons	
src/swarm_partition_commons_tests.rs	a_plane_that_did_not_run_is_one_failed_check_naming_why	partition-commons	
src/swarm_partition_commons_tests.rs	a_failed_write_before_the_cut_is_not_blamed_on_the_cut	partition-commons	
src/swarm_partition_commons_tests.rs	macs_read_alike_only_with_the_same_history_heads_rows_and_reason	partition-repair	
src/swarm_partition_commons_tests.rs	a_pass_that_moved_nothing_settles_the_repair_only_where_every_exchange_ran	partition-repair	
src/swarm_partition_accounts_tests.rs	what_f19_promises_passes_every_check	partition-accounts	
src/swarm_partition_accounts_tests.rs	the_claims_leave_one_user_after_the_heal_and_home_use_blocks_a_cold_claim	partition-accounts	
src/swarm_partition_accounts_tests.rs	two_users_after_the_heal_or_nobody_is_not_one_user	partition-accounts	
src/swarm_partition_accounts_tests.rs	a_use_with_no_claim_of_its_own_fails_claims_before_use	partition-accounts	
src/swarm_partition_accounts_tests.rs	a_scenario_that_did_not_run_fails_its_checks	partition-accounts	
```

## 23. `git-starts`, the git starts of one busy Mac

`git-starts [--minutes N] [--every-ms M]` reads the Mac as it is and changes nothing: every M ms
(100) it lists this uid's processes (`aterm_uds::holders::uid_pids`) and reads each pid new to the
listing, and again each one whose read was none (`read_process_birth`, `read_process_path`; no
argv, no environment), and every 1 s it reads each local aterm's `survey facts` over its control
socket from the lab's own process, so it starts no process. A survey read counts only where every
aterm that accepts on its socket answered it. It prints process starts a minute (the advance of
the newest-born process's pid, from the first process born during the run), git starts seen a
minute (a lower bound), agent sessions' turn ends a minute (a lower bound), and the work record's
projected reads and git starts at its 20 s floor, with that projection's ceiling. The readers are
the `Readers` trait; the tests drive `Tally` over a fake. `git-starts --read [--reads K]` instead
reads each local aterm's `survey content` once, takes each agent row's folder (`folders_of`), and
reads its work record K times (5) with `aterm_agent::harness::repo_work::read_work`, printing the
git starts and the wall time per read (`Reads::summary`) and naming no folder. Read the same way as
§1.

```tsv
id	kind	functions	statement	subsumes	difficulty	blocking_gaps
git-starts-pids	contract	git_starts::forward	forward(f, t) = t − f for t ≥ f; (PID_MAX − f) + (t − PID_WRAP_TO) for t < f with t ≥ PID_WRAP_TO and f < PID_MAX; else 0.	-	1	-
git-starts-tally	contract	git_starts::{Tally::{list,survey,look},in_turn,resting}	list(t, None) counts one failed listing and changes nothing else. list(t, Some(P), read): listings += 1; each p ∈ P (on the first listing) or p ∈ P not in the previous listing's read pids (on a later one) is read once; on a later listing a read Some(s) with s.git puts (p, s.birth) into gits; with (b, p) the read of greatest birth, newest := (b, p) where none was held, and where newest = (b₀, p₀) with b > b₀: the first time, counted := (t, t) and nothing is added; after, starts += forward(p₀, p) and counted's end := t; then newest := (b, p); the previous listing's read pids := the p ∈ P carried over or read Some (on the first listing, those read Some). survey(t, None): unanswered += 1, nothing else. survey(t, Some(R)): surveys += 1; for each row its sid joins sessions; for each agent row shown += 1, busy += [word = `busy`], an agent Session (first_ms = t, new ⇔ an earlier Some read was folded) is kept, a turn end at t is pushed where in_turn(its word) (`busy`, `prompt`) and resting(the row's word) (`idle`, `survey`, `question`, `wall:*`), and its word and last_ms become the row's. look(t, r) = list(t, r.pids(), r.process) then survey of each r.survey() in order until it is None.	-	1	-
git-starts-floor	contract	git_starts::{floor_reads,Tally::summary}	floor_reads(s, F) = the count of reads where, from last = s.first_ms if s.new (one read) else none, each turn end at ≥ last + F (or with no last) reads and becomes last. summary(): minutes = (last − first listing) / 60000; reads = Σ floor_reads(s, FLOOR) over agent sessions / minutes; ceiling = (agent rows shown / survey reads) · 60 / FLOOR_s + new sessions / minutes; process starts a minute = starts / ((counted's end − counted's start) / 60000), 0 where that is 0; git starts a minute = |gits| / minutes; turn ends = the nearest-rank median, 95th percentile and most of each agent session's turn ends / ((last_ms − first_ms + SURVEY_EVERY) / 60000).	-	1	-
git-starts-rows	contract	git_starts::rows_of	rows_of(b) = one Row per line of b starting `session ` with a non-empty `sid=`, its sid and agent word pct-decoded, agent ⇔ `program_kind=agent`; other lines read as nothing.	-	1	-
git-starts-pass	contract	git_starts::survey_pass	survey_pass(Err, read) = None. survey_pass(Ok(I), read) = None where some (_, s) ∈ I has read(s) an error other than ConnectionRefused or NotFound; else Some of the concatenated Ok rows in I's order, a ConnectionRefused or NotFound read adding none.	-	1	-
git-starts-read	contract	git_starts::{folders_of,Reads::{fold,summary}}	folders_of(b, h) = for each `session ` line of b whose program_kind=agent, its pct-decoded work_tree= (not `-`) else cwd=, a leading `~` (alone or before `/`) replaced by h, kept where absolute. fold(r, ms) pushes (r's starts, ms) for a Read, counts a None or an Unread. summary() names the three counts, the fewest, nearest-rank median and most git starts and the nearest-rank p50, p95 and most wall time over the pushed reads, 0 for none.	-	1	-
```

```tsv
file	test	primary	also
src/git_starts_tests.rs	a_git_seen_in_many_samples_counts_once	git-starts-tally	
src/git_starts_tests.rs	a_process_that_read_as_none_is_read_again	git-starts-tally	
src/git_starts_tests.rs	process_starts_count_from_the_first_process_born_during_the_run	git-starts-tally	
src/git_starts_tests.rs	process_starts_count_through_the_pid_wrap	git-starts-pids	git-starts-tally
src/git_starts_tests.rs	a_turn_end_is_a_row_in_a_turn_coming_to_rest	git-starts-tally	
src/git_starts_tests.rs	a_survey_read_an_aterm_did_not_answer_folds_nothing	git-starts-tally	
src/git_starts_tests.rs	every_survey_read_finished_between_two_looks_is_folded	git-starts-tally	
src/git_starts_tests.rs	a_survey_pass_counts_only_where_every_listening_aterm_answered	git-starts-pass	
src/git_starts_tests.rs	the_projection_caps_each_session_at_the_floor	git-starts-floor	
src/git_starts_tests.rs	the_summary_reads_each_count_over_its_minutes	git-starts-floor	git-starts-tally
src/git_starts_tests.rs	a_survey_reply_reads_as_its_session_rows	git-starts-rows	
src/git_starts_tests.rs	a_survey_of_an_aterm_that_accepts_nothing_ends_unanswered	git-starts-pass	
src/git_starts_tests.rs	an_agent_rows_folder_is_its_work_tree_else_its_cwd	git-starts-read	
src/git_starts_tests.rs	the_read_summary_counts_starts_and_wall_time_per_read	git-starts-read	
```

## 24. What aterm checked, on every surface (Autopilot v6 slice 9c)

`forever`'s judge holds 9c's surfaces to the receipt 9b journals (`forever_checked.rs`): `status`'s
four `landed_*` fields, the Done for you row's tip (L16, L17), the worker's `checked=` on `await
task`, `result` and its row's tip (L15), the landed chip's title read in front (L1, L7, L16–L19),
and the leg each chain's folder makes, with two chains
whose folders make the legs no other chain does: land-n (L18, a repository with no remote) and
land-o (L19, a plain folder). Read the same way as §1.

```tsv
id	kind	functions	statement	subsumes	difficulty	blocking_gaps
checked-status-fields	contract	forever::checked::{status_agrees,receipt_at,fields_of,leg_of,receipt_of}	For status_agrees(chain, seen, t0) with R the RECEIPT lines journaled after t0: a status read after t0 lacking any of landed_head=, landed_on=, landed_leg=, landed_repo= ⇒ Fail. Else Fail iff some read's four fields (landed_on and landed_repo percent-decoded) differ from fields_of(receipt_at(t)) where its task= is landed and from `- - - -` elsewhere (receipt_at(t) = the last of R journaled by t, else the first of R), or the last of R has a leg other than leg_of(chain) (no-remote for Repo::NoRemote, no-repo for Repo::Plain, on-main otherwise), a head other than the first 12 digits of seen.head where both are named, or an on-main ref other than origin/main. Otherwise Unobserved iff the chain lands (Chain::lands or a scenario whose receipt the lab judges) and no read said landed or R is empty; else Pass. No read after t0 ⇒ Unobserved.	-	1	-
checked-done-tip	contract	forever::checked::done_tip	done_tip(seen, t0) with r the last receipt after t0: none ⇒ Unobserved; Pass iff some Done for you tip read after t0 (not `-`) equals mode_words::landed_tip(Some(r)) and contains `<r.head> on origin/main`; else Fail.	-	1	-
checked-worker	contract	forever::checked::worker_checked	worker_checked(seen, t0) with r the last receipt after t0: none ⇒ Unobserved; Pass iff the manager's await task output has the token checked=<r.checked_word()>, result's header ends ` checked=<word>`, and some worker row's tip after t0 equals mode_words::checked_words(Some(r)); else Fail naming each that did not.	-	1	-
checked-title	contract	forever::checked::{title_checked,titled}	title_checked(seen, t0): no window read ⇒ Pass, saying SKIPPED. Else with T the title reads after t0 whose chip wears face=landed (window.front's line split by split_title, and each window.title read whose latest window.chip line by then has face=landed), `-` and `?` dropped: T empty ⇒ Unobserved; Fail iff some title read at t lacks mode_words::landed_brief(receipt_at(t)) (landed_brief(None) where the journal holds no receipt); else Pass. titled(s) ⇔ s ∈ {L1, L7, L16, L17, L18, L19}.	-	1	-
checked-wants-join	contract	forever::checked::{with_checked,judged}	with_checked(chain, s, seen, t0, v) = v where ¬judged(chain, s); else the wants W = [status_agrees] ++ [done_tip if s ∈ {L16, L17}] ++ [worker_checked if s = L15] ++ [title_checked if titled(s)], joined to v: Fail if v or some W fails, else Unobserved if v or some W is, else Pass, its words v's followed by `; checked: <WORD> (<why>)` for each W. judged(chain, s) ⇔ receipt::judged(chain, s) is Some ∨ s ∈ {W2, W4, W5, W6, W9, W10, W11, O1, the switch scenarios}.	-	1	-
```

```tsv
file	test	primary	also
src/forever_checked_tests.rs	status_agrees_with_the_receipt_and_says_dash_off_the_landing	checked-status-fields	
src/forever_checked_tests.rs	each_folder_makes_its_own_leg	checked-status-fields	
src/forever_checked_tests.rs	the_done_rows_tip_names_the_head	checked-done-tip	
src/forever_checked_tests.rs	the_workers_await_result_and_row_say_checked	checked-worker	
src/forever_checked_tests.rs	the_landed_title_says_its_own_receipt	checked-title	
```

## 25. The tether's verb

Both of the lab's `main`s call `aterm_uds::tether::entry` before the `argv[0]` dispatch (the
macOS one after the debug build's claim holder, as aterm's and atpkg's do), so a child the lab
spawns tethered can start through the lab's own image. The tether's own contract is aterm-uds'.
Read the same way as §1.

```tsv
id	kind	functions	statement	subsumes	difficulty	blocking_gaps
tether-verb-first	contract	main, aterm_uds::tether::entry	For every argv = [exe, "__tether", p, program, argv0, args…], whatever exe's file name: with p this process's parent's pid (> 1) the process ties itself to p and execs program under argv0 with args (the program's exit status, its output its own); with p = 1, or a pid that is not its parent, it exits UNTETHERED (125) and execs nothing. Every other argv dispatches as before.	tests/tether_entry.rs::the_lab_serves_the_tether_verb	process exec and fork, argv dispatch	0,1,8
```

```tsv
file	test	primary	also
tests/tether_entry.rs	the_lab_serves_the_tether_verb	tether-verb-first	
```

## 26. A letter judged by the ring round its strokes (round 27)

`trail-contrast`'s thresholds (`min_contrast`, `--min`, `--newest-min`) judge a glyph cell by its
`local_contrast`, what a reader sees round each stroke, the one reading the letter guard keeps
(`aterm-effects` SPECS §18): the whole cell's `contrast` only where a glyph has no ground that near
or there is no clean twin. Read the same way as §1.

```tsv
id	kind	functions	statement	subsumes	difficulty	blocking_gaps
trail-contrast-judged	contract	trail_contrast::{judged,measure_cell,solid_ink_ratios,report}	For every measured cell c: judged(c) = c.local_contrast where measure_cell gave one, else c.contrast, else None; report counts a cell under --min, and a newest glyph under --newest-min, by judged alone, so a light laid on the cell's ground more than RING_REACH from every solid ink pixel leaves judged(c) = the clean reading of its strokes whatever the whole cell's median ground reads.	-	1	-
```

```tsv
file	test	primary	also
src/trail_contrast_tests.rs	a_light_away_from_the_strokes_leaves_the_letter_its_reading	trail-contrast-judged	
```

## 27. What the effects did, apart from the page's own dim text

`trail-contrast --lowered-min X` counts a glyph cell only where the effects took it down: its
judged reading (§26) under X, and the same reading on its clean twin, `clean_local_contrast`, X or
more, or the judged reading under `LOWERED_SHARE` (0.97, the letter guard's `OWN_SHARE`) of it too:
the letter guard's law, a letter at the bar keeping the bar and a dimmer one its share (amended
2026-11-05, after-land round 3: a glyph at 4.56 of its own read 4.49 under Comet's tail, which the
share excused). A page's own low-contrast text (a coloured prompt segment) reads the same with and
without the effects, and is not counted. Without a clean twin it is an error, never a pass. Read the
same way as §1.

```tsv
id	kind	functions	statement	subsumes	difficulty	blocking_gaps
trail-contrast-lowered	contract	trail_contrast::{clean_judged,lowered_under,measure_cell,report}	For every measured cell c with a clean twin: report counts c under --lowered-min X iff judged(c) < X and (clean_judged(c) >= X or judged(c) < LOWERED_SHARE * clean_judged(c)), where clean_judged(c) = c.clean_local_contrast where measure_cell gave one, else c.clean_contrast; a cell whose reading the effects left unchanged is never counted, and --lowered-min with no clean twin is a report error.	-	1	-
```

```tsv
file	test	primary	also
src/trail_contrast_tests.rs	lowered_min_counts_only_the_glyphs_the_effects_took_down	trail-contrast-lowered	
src/trail_contrast_tests.rs	lowered_min_holds_a_glyph_at_the_bar_to_the_bar	trail-contrast-lowered	
```

## 28. Bounded regular-file reads in `util`

`util::{open_regular_file, read_regular_file, read_regular_text}` open lab files with
`O_NONBLOCK | O_CLOEXEC`, verify `fstat` is a regular file (`InvalidInput` on directories, FIFOs,
sockets and devices), and bound reads to `MAX_LAB_FILE_BYTES` (256 MiB, `InvalidData` when larger or
grown mid-read, or when UTF-8 decoding fails). Read the same way as §1.

```tsv
id	kind	functions	statement	subsumes	difficulty	blocking_gaps
util-read-regular	contract	util::{open_regular_file,read_regular_file,read_regular_text}	open_regular_file(p) opens p with O_NONBLOCK | O_CLOEXEC and returns Ok(f) iff f.metadata().is_file(), else Err(InvalidInput). read_regular_file(p) reads at most MAX_LAB_FILE_BYTES + 1 bytes from open_regular_file(p) and returns Ok(bytes) iff meta.len() ≤ MAX_LAB_FILE_BYTES and |bytes| ≤ MAX_LAB_FILE_BYTES, else Err(InvalidData). read_regular_text(p) decodes read_regular_file(p) as UTF-8, returning Err(InvalidData) on invalid UTF-8.	util.rs::read_regular_file_and_text_refuse_non_regular_entries	fs open with custom flags, fstat, Read::take, UTF-8 decode	1,7,8
```

```tsv
file	test	primary	also
src/util.rs	read_regular_file_and_text_refuse_non_regular_entries	util-read-regular	
```

## 29. The facts leg's pure readers (gap 23)

`codex-alab-proof --facts-only` judges aterm's no-progress fact riding a real codex-alab's
continuation by what it reads passively: codex-alab's journal of each read and ride, the model's
requests, one `sys jobs` read after the measurement, the session ledger and the Messages log
(`codex_alab_facts_proof.rs`). The model's `facts` arms ask for the leg's two calls and tag a
request that carries the rider (`codex_alab_model.rs`). Read the same way as §1.

```tsv
id	kind	functions	statement	subsumes	difficulty	blocking_gaps
facts-journal-lines	contract	codex_alab_proof::facts_proof::{fact_line,fact_lines,story,is_episode,is_rule}	fact_lines(j) = in order, (at, fact_line(rest)) of each line of j split at its first `codex_alab_autopilot: ` where fact_line(rest) is Some, at = workers::iso_ms of the line's first word (its UTC stamp as unix ms, None where that does not parse). fact_line(rest) = Read{why,rule} iff rest = `fact: <why> (<rule>)` with why non-empty and rule = <name>@v<digits>; Told{e} iff rest = `fact <kind> <e> told` with e 16 lowercase hex digits; Rides{e,rule} iff rest = `<key>: fact <kind> <e> rides <rule>`; Other(rest) for any other rest that starts `fact: ` or `fact ` or holds `: fact: ` or `: fact `; None otherwise. story(ls) puts each Read's (why, at) in before while no Rides came yet and its why in after once one did, each Rides in rides, each Told in told and each Other in other.	-	1	-
facts-check-1-in-time	contract	codex_alab_proof::facts_proof::{reads_before_hold,reads_at}	reads_before_hold(b, t) iff b is non-empty, every (why, at) in b has at = Some(a) with why = `stale`, or why = `none` and a − t < NONE_UNTIL_MS (PHI8_QUIET_MS + FACT_FRESH_MS + 3000), and some (_, Some(a)) in b has a − t < PHI8_QUIET_MS (184208, φ 8 over the 10 s floor μ). The leg's t is written_ms of FACTS_TICKS (the file's modified time as unix ms, the rhythm's last tick), util::now_ms where that cannot be read. reads_at(b, t) = for each (why, at): `<why> at <secs(a − t)>`, or `<why> at -` where at is None.	-	1	-
facts-rider	contract	codex_alab_proof::facts_proof::{rider_of,told_call,expected_rider}	rider_of(u) = u[i..] for the last i at which turn_end::FACT_OPEN starts, None where it never does. told_call(r) = Some((p, n)) iff r holds `(pid <p>)` followed by `has made no progress for <n> min` with p: u32 and n: u64 parsing. expected_rider(p, n) = turn_end::fact_suffix of a Fact whose text is BackgroundCall{pid: p, task: None, wait: None, quiet_ms: 60000·n}.told() = ` (aterm: a background call (pid p) has made no progress for n min)`.	-	1	-
facts-board-ledger-messages	contract	codex_alab_proof::facts_proof::{no_progress_leaders,ledger_facts,message_texts}	no_progress_leaders(r) = in order, the u32 after `id=a` of each line of r that starts `job ` and holds the word `no_progress=1`. ledger_facts(rows) = for each row whose rule_id is `fact-no-progress@v1`: its sid without a leading `@`, decision, command, reason and holder (each `` where absent). message_texts(r) = for each line of r that starts `message `: pct_decode(title) + ` ` + the lines of pct_decode(detail) joined with nothing between them.	-	1	-
facts-model-arms	contract	codex_alab_model::{answer,facts_call,facts_holder_call,facts_waiter_call,facts_step}	For a request of leg `facts`: at Step::Person with 0 tool outputs, tag `facts:holder` and one exec_command (login false, yield_time_ms 1000, no tty) whose perl takes LOCK_EX on FACTS_LOCK, writes its pid to FACTS_HOLDER_PID and stops itself; with 1, tag `facts:waiter` and one exec_command whose perl is the call's leader, writes its pid to FACTS_WAITER_PID, prints FACTS_TICK_COUNT ticks FACTS_TICK_MS apart into FACTS_TICKS (its stdout and stderr) and then takes LOCK_EX on FACTS_LOCK; otherwise slow(FACTS_STARTED, SLOW_TURN) tagged `facts:person`. At Step::Continue, slow(facts_step(c), SLOW_TURN) tagged `facts:continue`, c the request's count of continuation texts, with FACT_TAG appended iff the request's user text holds turn_end::FACT_OPEN. facts_step(n) for n in 1..=10 are ten texts no two of which share codex-alab's reply mark (each word's letters, a word with a digit one `#`), and none says done.	-	1	-
```

```tsv
file	test	primary	also
src/codex_alab_facts_proof.rs	codex_alab_s_fact_lines_tell_the_reads_the_ride_and_its_ack	facts-journal-lines	
src/codex_alab_facts_proof.rs	check_1_holds_l3_s_reads_and_refuses_a_late_ride	facts-check-1-in-time	facts-journal-lines
src/codex_alab_facts_proof.rs	the_expected_rider_is_aterms_fact_suffix_for_the_call	facts-rider	
src/codex_alab_facts_proof.rs	sys_jobs_names_the_no_progress_calls_by_their_leader	facts-board-ledger-messages	
src/codex_alab_facts_proof.rs	the_ledger_s_fact_rows_carry_their_holder	facts-board-ledger-messages	
src/codex_alab_facts_proof.rs	messages_rows_are_read_by_their_decoded_words	facts-board-ledger-messages	
src/codex_alab_model.rs	the_facts_leg_runs_its_two_calls_then_its_steps_and_tags_a_fact	facts-model-arms	
```

## 30. `work-overlap`, the overlap fact live

`work-overlap [tag] [--keep]` brings up a headless lab of its own against the stub alone and lays a
scratch world in the project folder's `.ubd/work-overlap.noindex`: a bare `origin.git` whose main
holds `src/lib.rs`, its clone `repo`, and the clone's worktrees `wa` and `wb` off main. The manager
Claude Code spawns two workers, `spawn agent=claude cwd=<worktree>`; each changes `shared()` in
`src/lib.rs` with one Bash call that then holds on a gate file the lab writes. The lab opens the
first's gate once its first record is a floor old, waits for its next aterm-typed prompt and its
`work=` row naming the file, opens the second's the same way, and wants the second's ledger row
`fact-overlap@v1` typed and its next prompt to carry ` (aterm: 1 file this branch changes is also
changed by <first sid>)` naming no place, and both rows to name the file; then it opens the
first's second gate, whose Bash commits and pushes to origin's main before `Nothing left:` (and
`LANDED` at Land's check), and
wants the first's task `done why=landed` and its `work=` row gone while the second's stays. Each
wait is a one-minute hang detector. Read the same way as §1.

```tsv
id	kind	functions	statement	subsumes	difficulty	blocking_gaps
work-overlap-plan	contract	work_overlap::{Plan::{new,script,spawn_command,change_command,land_command,hold,gate},told,typed_after,overlaps_typed,names_no_place,session_of,work_of}	Plan::new(n, w) keys the manager, the first and the second by tokens holding n, in w/wa and w/wb, their gates under w/gates; script() = the manager's two spawns (`aterm ctl @self spawn agent=claude cwd=<tree> - <<'EOF'`, the task, `EOF`) and its words; each worker's change (printf of shared() returning 1 or 2 into src/lib.rs, then hold(gate 1)) and report; the first's land_command (hold(gate 2), TMPDIR in the world, add, commit, `git push -q origin HEAD:main`, status), its `Nothing left:` defense and its `LANDED`; the second's reply, which is no defense. hold(g) waits for g in 0.2 s steps at most GATE_STEPS times and fails where g never came. told(s) = `1 file this branch changes is also changed by s`. typed_after(R, t) = the first user row after the assistant row whose text is t with words and no tool result. overlaps_typed(L) = the commands of L's fact-overlap@v1 rows decided typed. names_no_place(f) ⇔ no `/`, no `lib.rs`, and no word src, wa, wb, main, origin or repo. session_of(j, s) = the record of sid s among j's hosts' sessions; work_of(r) = (work=, work_paths= names src/lib.rs, work_ms= as a number).	-	1	-
```

```tsv
file	test	primary	also
src/work_overlap_tests.rs	the_script_keys_each_conversation_by_its_token	work-overlap-plan	
src/work_overlap_tests.rs	the_seconds_prompt_and_ledger_rows_are_read_where_aterm_typed_them	work-overlap-plan	
src/work_overlap_tests.rs	a_survey_records_work_fields_are_read_by_sid	work-overlap-plan	
```

## 31. The `claude` stand-in's statusLine, and line E's captures (glass S5c)

`docs/DESIGN-harness-glass-rainbow-2026-11-05.md` §6 S5, §7. The `claude` stand-in
(`fleet_fixture.rs`) runs the statusLine its `--settings` names as Claude Code 2.1.318 does and
draws its row under the composer's bottom rule; `outlook-captures` gains scenes I, J and K (the
design's cases (b), (c) and (d), under Cruise), whose stand-in runs the build's own usage tap with
the own login's seeded week, so the window's line E is the quota row. The scenes A to H run no
statusLine and capture line D's fallback unit. `swarm`'s stand-in reads its tap command through
the same `status_line` (its own `status_command_from_args` is gone, and its test
`cli_and_settings_parsing_cover_flags_and_negative_cases`, now parsing the CLI alone, is
`cli_parsing_covers_flags_and_negative_cases`). Read the same way as §1.

```tsv
id	kind	functions	statement	subsumes	difficulty	blocking_gaps
fixture-status-line	contract	fleet_fixture::{status_line,status_payload,run_status_line,status_row,screen}	status_line(a) = the first `--settings <json>` or `--settings=<json>` before a literal `--` whose JSON has statusLine.command c: StatusLine{command: c, every: statusLine.refreshInterval as whole seconds where > 0, else None}; None where no such value. status_payload(s, cwd, l) = a JSON object with session_id (s or null), cwd, model, version and cost.total_api_duration_ms 0, plus rate_limits = J::parse(l) exactly where that is an object. run_status_line(c, p) runs `/bin/sh -c c` (aterm_uds::spawnfd::command) with p on its stdin and CLAUDE_PID = this pid, and is the first line of its stdout with control characters dropped, None where that line is absent or blank. status_row(t, cols) = "  " ++ t where chars(t) ≤ cols − 4, else "  " ++ the first cols − 5 chars of t ++ "…". screen in Idle or Busy with a status row r puts status_row(r, cols) directly under the composer's bottom rule and the mode row under it; with none, the mode row directly under the rule. Against aterm-phase's claude-2.1.318-tap-row-{week,week-100col,week-70col,week-30col,no-figure,silent-command} fixtures the row under the bottom rule equals the measured one at the measured width.	fleet_fixture.rs::{the_status_row_is_where_claude_2_1_318_draws_it,the_settings_name_the_status_command_and_its_refresh,a_run_hands_the_payload_and_shows_the_first_line}	JSON parsing, a child process with piped stdio, char counting	1,8
outlook-glass-seeds	contract	outlook_captures::{plan,Plan::rate_limits,seed}	plan(c).tapped ⇔ c ∈ {I, J, K}, under autopilot "cruise" (else "off"); the own login is the first roster row (`me` for A–H and M, `main` for I–K), the default home's, active in the store and, outside L and M, the only row naming an address. plan(L).outside: its first row, the default home's and the store's active one, is `alt1`, and no row names the profile's address, so the footer shows the profile's login by its own address and the card is of an account outside the pool. plan(c).home and plan(c).vault are held ⇔ c ∈ {L, M}: there every row names a synthetic address, seed writes <home>/.secrets/ai-pool.toml giving the profile's account its home (`m15` in L, this Mac's `m20` in M) and each other row this Mac, and the store's swarm_label is `m20`. A series reading from before the own week began carries the reset of the week it was made in. seed writes <home>/.claude/fleet-fixture-rate-limits.json exactly where plan.tapped and the own login has a seen window: each window as {used_percentage: pct_x10/10, resets_at: now + reset}; so the tap's reading of status_payload over it holds the store's own week (I 63%, J 75%), and K's payload has no rate_limits (the tap prints NO_WEEK).	outlook_captures.rs::{the_seeds_are_synthetic_and_stay_in_their_world,the_tapped_scenes_hand_the_tap_the_stores_week,scene_ms_series_crosses_a_weeks_reset}	JSON building, the accounts store's writer	1,8
```

```tsv
file	test	primary	also
src/fleet_fixture.rs	the_status_row_is_where_claude_2_1_318_draws_it	fixture-status-line	
src/fleet_fixture.rs	the_settings_name_the_status_command_and_its_refresh	fixture-status-line	
src/fleet_fixture.rs	a_run_hands_the_payload_and_shows_the_first_line	fixture-status-line	
src/outlook_captures.rs	the_seeds_are_synthetic_and_stay_in_their_world	outlook-glass-seeds	
src/outlook_captures.rs	the_tapped_scenes_hand_the_tap_the_stores_week	outlook-glass-seeds	
src/outlook_captures.rs	scene_ms_series_crosses_a_weeks_reset	outlook-glass-seeds	
```

## 32. The Account card among the captures

`docs/DESIGN-harness-glass-rainbow-2026-11-05.md` §1.4, §7. `outlook-captures` shoots the Account
card of scenes A, B, C, E and H (Autopilot Off, whose `Forecast` row says there is none and why;
H's account has no figure and no reading of its week) and of scene J (case (c) under Cruise),
the one card whose tab aterm can switch, so the one whose `Forecast` row holds the outlook's
projection. Scenes L and M are the card's own, the states only a Mac in a lab shows: a pool
file and a scratch vault this lab process serves on loopback with the instance's home seated in
it (`setup_token_lab::seat_scratch_vault`). L's tab runs on a login outside the roster whose
home is another Mac and for which the vault holds no token; M's on a pool account at home on
this Mac for which the vault holds a verified lab token (the stub's labelled one, no credential
anywhere), over eight days of readings that cross a week's reset. The glass scenes' other logins were read on this
Mac, so their readings are series rows too (as aterm-agent's `quota_scenes::Fx::read` writes
them) and J's world is case (c)'s. A card is shot once the account reader's card has arrived and
its pane's recovery observer has read, and its rows are the same after the shot as before it.
Read the same way as §1.

```tsv
id	kind	functions	statement	subsumes	difficulty	blocking_gaps
outlook-card-scene-j	contract	outlook_captures::{CARD_SCENES,plan,seed,card_lines,card_shown,shoot_card}	CARD_SCENES = {A, B, C, E, H, J, L, M} and plan(J).autopilot = "cruise". plan(c).read_here ⇔ c ∈ {I, J, K}; seed then writes, for each seen login other than the own one with a week (pct_x10, reset, read), a series fig row {pct_x10, reset: now + reset, at: now + read, src tap} at now + read, so the outlook read from J's world at quota_scenes::T5 on UTC (roster, store, logins, series, Mode::Early) gives quota("main", None, true) equal to quota_scenes::case_c()'s, with a week projection on `main`'s row; scene A's world gives another view. card_lines(t) is t's `section` rows (their text) and `fact` rows (name, value) in order, a `fact` row with an empty name directly under a `fact` row joined to that fact's value by a blank, any other row ending the run. card_shown(t) ⇔ t's first line has open=true and fact=account, card_lines(t) holds the facts Token, Budget and Machine, Machine's value is not `not read yet`, and no Forecast value holds outlook::Refusal::ObserverBusy's words. shoot_card opens the card, and shoots the frame only once card_shown holds and the frame is steady, keeping the shot where `controls fact-menu` reads the same after it as before; SHOWN bounds it as a hang detector.	-	1	-
```

```tsv
file	test	primary	also
src/outlook_captures.rs	scene_js_world_is_case_cs	outlook-card-scene-j	outlook-glass-seeds
src/outlook_captures.rs	a_card_is_shown_once_its_reader_and_observer_have_answered	outlook-card-scene-j	
```

## 33. Program outcomes across native handoffs

The installer-handoff lab can keep two program workers running through its native
replacements, then require the exact success and failure outcomes through the control API.

```tsv
id	kind	functions	statement	subsumes	difficulty	blocking_gaps
handoff-program-outcome	contract	installer_handoff::program_finished	For expected exit e, a task is accepted iff state is done when e = 0 or failed otherwise, why is exit-status, and the parsed exit equals e. A missing status, wrong state, or different exit is refused.	src/installer_handoff.rs::tests::program_completion_requires_the_requested_exit_status	field parsing, integer comparison	0,1,3
```
