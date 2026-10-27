<!-- Copyright 2026 Andrew Yates -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# aterm-agent: spec recovery from its 2840 unit tests

> Current scope, 2026-10-21: `harness::spend` holds the price table, per-request pricing, the Codex
> conversation usage fold, and the account-wide Claude Code `Spend` (the home's transcript fold and
> the capacity figure) with their tests. Main removed the account-wide half on 2026-10-19 for want of
> a production caller; the account stats A1 view (`harness::account_stats`, `accounts stats`) calls
> that fold and figure, so its merge restored them, and only them: the spend file (`SpendStore`,
> persistence), the Codex home fold and the calibration, which nothing calls yet, stay removed, as
> does the `spend_fold` example. The recovery counts below describe the older survey.

> Status 2026-10-16, after the merge with the tests-and-gates audit: the rows naming 590 tests
> it deleted and 17 specs whose functions it deleted are gone (among them `acc-merge-join`, whose
> `merge_observations` had no caller, and `drive-dial-guard` and `relay-turn-transparency`, which left
> with `--dial` and `aterm-net`'s drive). The §3 gap histogram and the §6 lists count the 623 specs left. Counts elsewhere
> in this file were taken before.

> Status 2026-10-16: this is the crate's migration worklist. Nothing has been proved or deleted yet.
> Each of the 635 specs in §1 is proved through Trust/Clean about the compiled Rust function, then the
> tests it subsumes are deleted. The 80 chunk-6 groups in §1b still need their statements recovered,
> and the 82 tests in the six files listed in §3 were never read. The checks in §4 sample no spec.
>
> 2026-10-16, after the sessions-as-subagents merge (`docs/DESIGN-sessions-as-subagents-2026-10-15.md`
> §7): the code that merge removed left with its tests, so the rows below that name it are void, not
> work. That is `operator::` (the durable queue, its WAL and `tests/conformance_operator.rs`);
> `fleet_cli`'s operator clients and its `events`/`exec` bridge; `drive_cli`'s commands beyond
> `classify`, `phase`, `answer` and `ledger` (the prompt turn and its relay included); `SelfGovernor`
> with `Turn`, `ControlClient` and `TurnError`; `RelayClient`'s `dial` relay
> (`tests/remote_turn_loopback_tls.rs`; its row, `relay-turn-transparency`, is dropped); `mail::task`; and
> `watch_telling`, `story_of` and `set_manager`. The counts above are as recovered, before it.
> `drive-supervise-opts-only-limit` is void with them: `drive_cli::{supervise_opts,max_budget,
> mail_needs_sid}` left with `drive supervise`. A follow-up removed the loop's mail lane, which
> only its own tests still reached: `supervise::mail` whole (`Lane`, `MailRow`, `MailOpts`), the
> loop's `MailIn`, `Fold`, `Held`, `hold_for_report`, `NoLane`, `SuperviseOpts::mail`,
> `watch_mail`, `supervise_with` and `render_result_mail`, and `Review::review`'s fold. So
> `mail-lane-exactly-once` and `mail-report-fold` are void too, and `mail-row-and-principal`
> keeps only `is_from_session`, which moved unchanged to `ledger` with `field` and `bare_sid`.
> A later cut removed what only the engine's own tests still reached: `SuperviseOpts::notes`
> (with `append_note`) and `SuperviseOpts::report` (with `brief_report`, `ReportBrief`,
> `reported_event_line`, `event_line_as` and `Review::review`'s report argument), the policy's
> `session_questions` switch, and the ledger's `reports` row. So `watch-report` is void, and
> `render-phase-grammar` no longer has `event_line_as`. `Session::supervise`, `supervise_to`,
> `await_turn`, `await_turn_to` and `render_result` are now `#[cfg(test)]`, the engine tests'
> seams over the hosted loop's engine, as `watch` already was; the rows that name them name
> those seams.

> Status 2026-10-19, the credits opt-in (`[harness] usage_credits`; the owner, 2026-10-19: *"for
> continuing on credits, that's an optional additional feature that's off by default. use
> subscriptions."*): three specs are added (`credits-hold`, `turn-end-refines-SupervisorCreditsHold`,
> `credits-billed-model-target`) with §2 rows for their tests, and the §6.3 lists name them;
> `config-power-lattice`, `config-reader-never-grants`, `approval-never-spends`,
> `approval-dialog-rules`, `usage-wall-wait` and `model-availability-decision` say the key, and their
> `subsumes` counts are their §2 rows as they now stand. The §3 histogram and the counts above
> were taken before. After that branch's review (2026-10-19): `credits-hold` reads a model's credits,
> keeps an episode through reads that draw no notice, holds the usage and bucket acts, and ends at a
> bound; `credits-billed-model-target` fails closed and reads the stored consents;
> `approval-dialog-rules` cancels a credits consent with Esc (`credits-decline@v1`) and
> `approval-one-shot-only` carves `credits-consent@v1` out; their rows and counts follow.

> Status 2026-10-21, the usage tap times the wait at a usage wall (accounts v2 §10, slice S10a,
> `supervise::tap_clock`): four specs are added (`tap-clock-window-of-wall`, `tap-clock-step-says`,
> `tap-clock-timed`, `tap-wall-wait`) with §2 rows for their tests, and
> `turn-end-refines-SupervisorUsageWall` now names the model's `Tapped` dial and the real `TapClock`,
> its `subsumes` count taking its two new tests. The §3 histogram and the counts above were taken before.
> After that slice's review (2026-10-22): `tap-wall-wait` says the wall row keeps the notice's and
> the tap's resets apart, and takes the four successor tests.

> Status 2026-10-23, the hung-call design's slice 4 (`docs/DESIGN-hung-background-call-2026-10-20.md`
> R2, R3): three specs are added (`facts-no-progress-episode`, `fact-rides-turn-end`,
> `stuck-break-line`) with §2 rows for their tests; `config-power-lattice` and
> `config-reader-never-grants` say `stuck_break_note`, and the latter takes its test. The model
> half (`OccupancyDoor`'s `Key`, `OpenStuck`, `StuckWithoutKey` and `TheStuckLineNeedsItsKey`) lives
> in `aterm-models-harness`, which keeps no SPECS.md; its Tier-1 bind is
> `tests/conformance_occupancy.rs`'s stuck runs. The §3 histogram and the counts above were taken before.
> After that slice's reviews (2026-10-23): `fact-rides-turn-end` takes
> `a_prompt_left_in_the_composer_says_its_fact_not_submitted`, and `stuck-break-line` says its pace
> counts the lines that went (its own and the door's typed ones, a predecessor's too) and takes
> `the_stuck_lines_pace_counts_lines_that_went_across_a_restart`,
> `the_stuck_line_waits_out_the_pace_of_a_nudge_the_door_typed` and
> `a_successor_paces_by_the_lines_typed_at_a_break_alone`; their counts follow.

> Status 2026-10-23, classifier audit 3 (`docs/measured/classifier-audit-3-2026-10-23.md`): §2
> gains rows for the 40 tests of `classify_tests.rs` and `approval_tests.rs` it did not list (32
> landed from other branches: trust dialogs, Gemini CLI, agy and Muse boxes, the worker verbs, the
> trace; 8 are this audit's) and for `drive_cli.rs`'s new
> `classify_refuses_a_read_of_a_secret_under_the_default_list`. One spec is added
> (`approval-trace-names-decider`, named in §6.3). `drive-classify-verb`, `classify-sound`,
> `classify-flags-on-argv`, `approval-safe-bash-sound`, `approval-breaker-read-only-sound`,
> `approval-no-rule-read-only-sound` (its `no_rule_clear` is now `proof_gaps_clear`, run on every
> box the classifier alone proves), `approval-network-free`, `approval-trust-dialog`,
> `approval-limit-dialog`, `approval-dialog-rules`, `approval-one-shot-only`,
> `approval-owner-review-held`, `approval-whole-read`, `approval-roots` and `approval-breaker-naming`
> say what those tests pin, and the `subsumes` counts of those specs and of
> `approval-level-monotone`, `approval-not-a-box` and `box-command-pending-call` are their §2 rows as
> they now stand. `drive_cli.rs`'s §2 rows, which still name 17 tests that file no longer holds and
> miss 13 it does, are left for that file's own pass. The §3 histogram and the counts above were
> taken before.
> Landed onto main's narrow port of its option exactness (18777c2128, which took
> `read-only@classify-v8`, `no-rule-read-only@v3`, `rm-breaker@v7` and `breaker-read-only@v8` for
> whole long-option names alone), the audit's ids are `read-only@classify-v9`, `no-rule-read-only@v4`,
> `rm-breaker@v8` and `breaker-read-only@v9`, and §2 gains the port's two tests under
> `approval-safe-bash-sound`.

> Status 2026-10-26, v6's slice 12a (`docs/DESIGN-harness-autopilot-2026-10-15.md` §14.17.5.1):
> one spec is added (`turn-end-refines-SupervisorConditionResume`: a resume past a usage limit is
> the one cleared line, never typed into the standing wall or over the person) with §2 rows for the
> three tests of `tests/supervise_conformance_condition_resume.rs`. The specs that named a limit's
> resume as `keep going` were not restated; their tests now hold it to the line.

> Status 2026-10-26, v6's slice 10a (`docs/DESIGN-harness-autopilot-2026-10-15.md` §14.10 and its
> as-built §14.10.1): one spec is added (`autopilot-word-edge`: the loop's waits carry the
> session's stored Autopilot word, at rest and inside the agent's turn, and a switch of it is decided
> after a one-second settle, never typed into a running turn) with §2 rows for its eight tests in
> `src/supervise/autopilot_loop_tests.rs`, a file §3 lists as read by no chunk (its other tests
> still are not), and §6.3 names it. `never-type-over-a-person` says the word edge types only at
> rest and takes the mid-turn test as a sample. The policy half (`te-land-policy`,
> `turn-end-refines-SupervisorLand`) was amended with its code. The §3 histogram and the counts
> above were taken before. Amended 2026-10-26, after the real lab's first `--mode switch` run found
> a host that keeps a session log waited on with no word through a busy turn: `autopilot-word-edge`
> names the log wait among the turn's waits, with a §2 row for its test.

Source: `crates/aterm-agent` (read only). The tests were read in nine chunks, together with the code each
one reaches; nothing was compiled or run, and every claim comes from reading the code. The chunk manifest
said 2712 tests. A `#[test]` count over the crate today finds 2840: the nine chunks read 2758 of them
(several manifest counts were short by the stand-in bodies and late tests they found), and six files that
landed after the chunks were cut were not read (§3).

Result: **635 specs**, retiring **2359 tests** as their primary spec. **80 chunk-6 groups**
(statements pending) hold **363 more**. **36 tests are not spec samples, or must be fixed first** (§4).

## 0. Notation used in the statements

- Paths in §2 are relative to `crates/aterm-agent`; function names are module-qualified under its `src`
  (`harness::…`, `supervise::…`, `drive_cli::…`, `fleet_cli::…`, or the crate root).
- `⊓`, `⊕`, `⊖`: the meet of two policies, saturating `+`, saturating `−`. `sat(x)`: saturating `i64`.
- **Ctl world W**: any implementation of the `Ctl` / `ControlClient` trait, a reply function from the request
  history to `Result<CtlReply,String>`. The scripted `Mock`, `Still`, `ResetWorld`, `PanelWorld` and stand-in
  instances are sample points of W. A loop-level spec quantifies over every W, every `SuperviseOpts`, the clock
  and the zone table.
- **Trace / Req(run)**: the ordered request lines a run sends. **Writes** are `key`, `send`, `paste`, `turn`;
  **act requests** add `signal`. **Fenced(w, J)**: w carries `if=row_guard(row)` for its row as drawn in the
  judged read J, plus `if-gen=g(J)` where the host's help names it and `if-human=n(J)` where it publishes a
  person count. **The lease bracket**: `lease acquire … hard` … `lease release`.
- **Screen / read**: `text --json[ tail=40]` parsed by `screen::parse_text_json` (supervise) or
  `upgrade_drive::parse_screen` (harness); **live zone** = the rows from `busy_zone_start` / `transcript_end` on.
  **Reading(rows)**: aterm-phase's prompt reading, an opaque specified callee (cross-crate, gap 12).
- **Status(L)**: one `status` reply line with `hold=`, `hand=`, `input=`, `human_ms=` tokens.
- **Transcript rows**: JSONL; a **main-chain** row has `isSidechain != true`; an **answer** is a main-chain
  assistant row with a real model id; `Stamp(r)` = its parsed RFC 3339 timestamp.
- **St / Row / Facts / Phase / Step**: `upgrade_drive::St` (the per-conversation upgrade record), its owner-facing
  projection `upgrade_status::Row`, and the reducer's `upgrade::{Facts,Phase,Step}`. A step word is an act unless it
  is `current` or starts `wait:`, `skip:`, `busy:` or `would-`. `dry(x)`: no state write, no ledger row, no act,
  the step in its `would-` form.
- **Dir(agent)**: the agent's Claude directory as Claude Code resolves it (`HOME/.claude` when
  `CLAUDE_CONFIG_DIR` is unset; absolute as given; relative or empty against the directory the agent started in).
- **accounts**: `Inv_bounds(s)` (every map within its MAX, every key admissible, counts saturating);
  `cond(r,s,e,t)` = `accounts::condition`; `Reach_acc` = states reachable from `State::default()` by the
  transactions; `Advise(x)` = `accounts::advise(…)`; `Rec(s,me,point)` = s with my own wall recorded.
- **Disk**: `Survey`, `Report = report(survey,cfg)`, rows with class, removable flag and witness;
  `Applied = apply(…)`; the journal callback is write-ahead (`Removing` before the remover runs).
- **Power order ≤ on `SupervisorConfig`**: `a ≤ b` iff `a.limits(b) ∨ a = b`, per key; text keys are
  incomparable "own" values; opt-in keys (`stall_term`, `limit_reset`, `usage_notes`,
  `usage_credits`, `stuck_break_note`) default false = bottom (`usage_tap` ships `true` since 2026-10-22, and its off is
  a limit); `limit_reset_kinds` moves only with `limit_reset` off.
- **ShellSem**: a reference model, to be written in Clean, of zsh 5.9 (`-f`) and bash for the fragment the
  classifier judges; **ReadSet** is the (program, argv-form) pairs the classifier's tables call reads.
- **Refinement** rows bind the real functions to the named `ty_model!` machine in `aterm_spec::derive` under the
  projection its conformance test already computes: every real decision is an action the model enables at the
  projected state, every model invariant holds there, and every `Buggy`/knob variant is caught (gap 10).
- **Effects lane**: a spec whose functions touch fs, processes, sockets, threads or the clock is stated over an
  abstract oracle where the code already injects one (closure, trait object, host trait); otherwise it carries
  gap 8.

## 1. Specs (TSV)

Columns: `id`, `kind`, `functions` (paths under `crates/aterm-agent/src`, module-qualified), `statement`, `blocking_gaps` (rows of the audit §7 table; `0`, the seal upgrade, blocks every row and is not repeated), `subsumes` (number of tests this spec retires as their primary spec / number that sample it; the tests are in §2).

```tsv
id	kind	functions	statement	blocking_gaps	subsumes
drive-parse-sub-positionals	contract	drive_cli::{parse_sub,no_positionals}	parse_sub(verb,args)=Ok(sub) ⇒ sub.sid = the first `@` word before `--` (classify: None, and every word from the first non-flag word on goes to rest verbatim); `--` ends flag parsing and appends the rest verbatim; rest = positionals in order. no_positionals(v,sub)=Ok ⇔ rest=[], else Err naming the unexpected words.	1,3,7	2/2
drive-flag-ownership	table-property	drive_cli::parse_sub	A word is read as a flag iff it comes before the first `--`, is not the value a valued flag took, and, for classify, comes before its first positional. ∀ verb, recognised flag f read as a flag: (verb,f)∉Own ⇒ Err starting "{verb}: {f} is " naming the owner (`ledger's (…)`, `answer's (question box token)`; --source=… is named --source), before f's value is read; (verb,f)∈Own ⇒ no ownership error; Own = {(v,--allow-python) | every v} ∪ {(ledger,f) | f ∈ {--since, --source (or --source=…), --journal, --format, --out}} ∪ {(answer,--box)}. Any other word w starting `--` read as a flag ⇒ Err("{verb}: unknown option '{w}'; run `aterm drive --help`"). Decidable over the finite verbs × flags table.	1,3,5,7	1/1
drive-flag-values	contract	drive_cli::parse_sub, parse_since, LedgerFormat::parse	Each valued flag reads only its own field, its value the next word verbatim (for --source=v the rest of its word); a missing value Errs "{verb}: {f} needs {what}"; --allow-python repeats in order; --since = parse_since(v, tz_offset_s()), Err "{verb}: --since needs a time: …" where that is None; --source v (or --source=v) accepts only `log`, which sets no field, refuses `anchor` naming ANCHOR_REMOVED and Errs "{verb}: --source needs log, the one source" for any other v; --journal and --out take v as a path; --format = LedgerFormat::parse(v), so v ∈ {text,md,markdown,html}, else Err "{verb}: --format needs text, md or html"; --box takes v as the box token.	1,3,5,7,13	1/2
drive-top-parse-help-routing	contract	drive_cli::{parse,run}	Global flags are consumed until the first other word: --socket/--sock is the one, taking the next word verbatim ("" included) and Erring "--socket needs a PATH" when there is none; any other `--` word there but --help Errs starting "unknown option '{w}'" (--dial/--remote naming `aterm ctl --host`, the rest "(only --socket PATH)"), so a removed global flag such as --ready, --timeout or --idle is refused. cmd=[help] iff the first other word is -h|--help|help or cmd[1] is exactly -h/--help; run with cmd [] or [help] = Ok(Reply{DRIVE_HELP ++ "\n", 0}) with no I/O.	1,3,7	1/1
drive-classify-verb	contract	drive_cli::{classify_verb,classify_verb_at,python_allow,run}, policy::approval::command_reads_no_secret	classify_verb Errs "classify needs a shell command line" iff the joined rest is blank; else classify_verb_at(args, HOME) replies {"read-only",0} iff classify_command_with(cmd, python_allow(sub)).read_only and command_reads_no_secret(cmd, home) (secrets_free over default_secrets(home), each ~ resolved to home; with no HOME the rules rooted at the home drop out), else {"not-read-only <reason>",1}, the classifier's refusal named before the secret check's; touches no host. Since classifier audit 3 (2026-10-23) the verb is what an agent asks before it runs a line, so it refuses a read of a secret as every box rule does; it does not run the box-only checks (proof gaps, blanks, zsh, git configuration).	1,3,4	1/1
drive-phase-reply	contract	drive_cli::phase_reply, supervise::render_phase_and_survey, Session::{phase_now,await_phase,program_reader}	phase_reply(turn, reader, allow) = Reply{render_phase_and_survey(turn, reader, allow), 124 if timed_out else 0}: the phase line, then `survey 0` iff reader.survey_block is open (`survey withheld (<why>)` for a card the loop would not press), then `context {n}%` iff reader.context is Some. The turn and every line are read by the worker's own program's reader: `phase` reads through phase_now (program_reader: `status program=`, else the screen; then turn_of), `await-turn` through await_phase (the program named before the wait, so the first wait is for that program's busy guard).	1,3,7	1/1
drive-journal-sid	contract	drive_cli::{journal_sid,ledger_verb}	journal_sid(p)=Some(x) iff the journal reads and its records name exactly one sid x; ledger_verb with no sid and no journal sid Errs "ledger needs the worker:" before any ctl call.	1,7,8	1/1
drive-own-ctl	contract	own_exe, CtlClient::{own_at,new,run}	own_exe() = canonicalize(current_exe()) (current_exe() itself where it cannot be canonicalized); CtlClient::own_at(exe, sock).run(args) execs exe with argv ["ctl", "--sock", sock] ++ args; CtlClient::new(exe, sock) puts nothing before --sock.	8	1/1
align-capture-bounded	contract	harness::align::{capture_bounded,capture_bounded_impl,kill_capture_child,read_capture_pipe}	A child that exits and closes stdout before the deadline gives Ok{first min(cap,|out|) bytes, code}; otherwise Err naming the deadline within a bounded grace, its whole process group killed; an escaped descendant holding stdout cannot delay the return.	8,13	4/4
align-capture-lifecycle-refines-model	refinement	harness::align::capture_bounded_impl, aterm_models_harness::harness_capture_worker_lifecycle_model	Every return is a Return step of the model: child reaped and reader thread joined (never detached), also when an escaped descendant holds stdout.	8,10,13	1/1
upgrade-summary-complete	contract	harness::upgrade_drive::bounded_output	Some(text) requires complete UTF-8 stdout, a successful exit within the shared capture deadline, and at most cap bytes. An oversized valid prefix, read failure, invalid UTF-8 or nonzero exit returns None; process/reader cleanup follows align-capture-bounded.	1,3,8,13	1/1
acc-label-grammar	table-property	accounts::{admissible_label,label_refusal}, Store::login_dir	admissible_label(l) ⇔ label_refusal(l)=None ⇔ 1 ≤ |l| ≤ LABEL_MAX ∧ l[0] ∈ [a-z0-9] ∧ l ⊆ [a-z0-9._-] ∧ l ≠ "help"; for a Store::at store, login_dir(l)=Some(dir/logins/l) iff admissible (a Store::at_for(state, cli ≠ claude, home) store's is home/.aterm/logins/<cli>/l, its own statement not recovered here).	1,7	2/2
acc-roster-admission	contract	accounts::{Roster::parse,row_of,row_refusal,Roster::row}	Roster::parse(text)=Ok(R) ⇔ TOML whose only key is `account`, ≤ MAX_ACCOUNTS rows with only known keys, admissible distinct labels, class ∈ {dedicated,shared}, dir absent or an absolute ≤ DIR_MAX path without control/U+2028, distinct dirs, at most one dirless row; R.rows in file order, dir verbatim. Otherwise Err starting "accounts.toml" naming the first offending row and reason. Empty text ⇒ default.	1,3,5,7,12	1/1
acc-roster-file-read	contract	accounts::{Roster::load,roster_text}	load: missing ⇒ default; a readable UTF-8 file ≤ MAX_ROSTER_BYTES ⇒ parse; else Err naming the path. roster_text never admits. Launch policy uses the recovered, fenced read (AccountLaunchRead), never a raw cardinality fallback.	1,8	2/2
acc-alias-rediscovery	refinement	accounts_sync::{build_machine_roster,default_login_unlisted}	AccountAliasRelabel: a roster just built for the observed default provider identity does not request another default-login sync, including declared aliases. A later different provider identity requests sync once and respects the existing retry hold. Twelve native roster/identity scenarios exercise both observations using private profiles and configured sources without fetching them.	8,10	1/1
acc-sync-report	refinement	accounts_sync::{read_sync_report,write_sync_report,SyncReport::into_init}, accounts::Store::recover_account_records	AccountSyncReport: a failed record read returns no report; an admitted pending journal completed by the report read sets changed. CLI and setup retain the same bound roster, state and once-observed enrollment after later edits; fleet metrics describe the admitted source, independently of the later account snapshot. Staging and earlier sync recovery also contribute to changed. Credential files remain outside the publication protocol.	8,10	5/5
acc-dashboard-report	refinement	cli::{write_accounts_report,write_accounts_report_snapshot}, accounts_sync::{render_dashboard_html_with,dashboard_rows_html}	AccountDashboard: the table retains the text report's admitted roster, filtered state and once-observed enrollment across later edits; an unavailable report yields no table rows even if records subsequently recover. A healthy empty or own-only text report stays distinct from a refusal. Provider files and fleet metadata remain separate observations; mutations revalidate independently.	8,10	3/3
acc-renewal-read	refinement	accounts_sync::read_renewal_candidates	AccountRenewalRead: all four providers derive ranked renewal candidates, raw worker policy and login generation from one recovered account publication. Broken or held pending records yield no candidates. A later policy/binding change leaves the captured ranking intact and invalidates the old worker policy. Credential presence and sign-in history remain independent observations.	8,10	5/5
acc-signin-epoch	refinement	cli::{run_accounts_login,run_accounts_browser_with,signin_policy_ready}	AccountSignInEpoch: validate captured raw policy, effective row and generation before opening a plain login or browser; record no stale result; always tidy an opened browser before checking completion. Result times follow command start and retain later observations. Positive generations tolerate unrelated budget/binding changes, while unbound rows retain the conservative global counter. Native host fixtures exercise 60 positive-generation and 10 unbound cases without provider or window IO.	8,10	7/7
acc-append-row-roundtrip	round-trip	accounts::{append_row,append_rows,row_toml,row_inline}	append_row(existing,…)=Ok(t) ⇔ the appended text admits; then t extends existing, ends with row_toml, and parse(t).rows = parse(existing).rows ++ [row] (dir verbatim). Otherwise Err naming the refusal; an inline `account` gives Err "defines `account` inline" ending with row_inline.	1,3,4,5,7	2/2
acc-row-for-login-exact	contract	accounts::{Roster::row_for_login,login_env}	row_for_login(None|Some("")) = the dirless row; Some(d) = the row whose dir = d byte for byte (no normalisation). login_env(row) = (LOGIN_ENV, dir or "").	1,7	1/1
acc-enrolled	contract	accounts::{enrolled,login_made,is_enrolled}	enrolled = {label | dir=None ∨ login_made(dir)}; login_made(d) ⇔ read_dir(d) yields ≥ 1 entry; is_enrolled(r,E) ⇔ dir=None ∨ label ∈ E.	1,8	1/1
acc-state-json-roundtrip	round-trip	accounts::State::{to_json,from_json}	∀ reachable bounded s with stamps ≤ now: from_json(to_json(s),now)=Ok(s); to_json is one line, keys sorted, absent = null; keys added later read as their weakest value; an undo list survives only beside an untaken canary.	1,3,5,7,12	1/1
acc-state-unread-classification	contract	accounts::State::from_json, Unread::why	Err(Torn) ⇔ not one JSON document (no panic at any depth); Err(NotOurs) ⇔ JSON but not an object or schema ≠ 1; else Ok, unknown keys ignored.	1,3,12	1/1
acc-stamp-cap	contract	accounts::{State::from_json,stamp,stamp_cap}	from_json(text,now) caps observation stamps at now+STAMP_SKEW_S and machine-local stamps at now; instants meant to be ahead (until, resets, use_by) are unchanged.	1,3,7	1/1
acc-state-bounds-invariant	invariant	accounts::{State::from_json,note_*,claim_probe,strike,begin_rotation}	Inv_bounds holds after from_json and is preserved by every transaction; when a bound bites the newest is kept (for walls the oldest scoped clear is evicted first, otherwise the earliest standing end); counts saturate; inadmissible labels/sids/words are dropped; a transaction with inadmissible input is a no-op.	1,2,3,5,7	2/2
acc-condition-table	table-property	accounts::condition, Condition::{as_str,fresh,runs}, Account::limited_until	condition = first of NotEnrolled, Unseen (no record), SignedOut, Limited{max stands_until} if a wall stands, Low, Usable (observed), Unseen; (as_str,fresh,runs) per the fixed table.	1,3	1/1
acc-signed-out-list	contract	accounts::signed_out	signed_out = [label | row in roster order, email=None, condition=SignedOut]; unlisted labels never named.	1,3,7	1/1
acc-wall-stands-until	contract	accounts::{Wall::stands_until,rung}	stands_until = None for a scoped cleared record; otherwise Some(sat(until+RESET_GRACE_S)) when at ≤ until, Some(sat(at+LATE_WALL_S[min(max(attempts,1),2)-1])) when at > until, or Some(sat(at+NO_RESET_WALL_S[min(max(attempts,1),3)-1])) when until=None. A standing wall limits at E-1, not at E.	3	1/1
acc-sample-windows	contract	accounts::Sample::{session,week,low_until,low,leave_early}, figure	figure = pct with end = reset or at+W while t < end; low_until = max end of windows at threshold (90/97); low ⇔ low_until ≠ None; leave_early ⇔ end - t > RESET_SOON_S.	3	1/1
acc-note-wall-episode	contract	accounts::{note_wall,believed_reset,end_pin,most_lapsed}	until beyond RESET_BELIEVED_S is believed None; at capped at now. No-op for inadmissible input, older stamps, rows older than the owner pin or that raw window's scoped clearing, or the same standing wall. A clearing-second row is new. Otherwise records a live wall with attempts 0/held+1/1 per the episode rule, emits one Wall event, and ends a pin on that account with Unpin "wall".	1,2,3	3/3
acc-words-table	table-property	accounts word enums (Class, Mode, StayWhy, Advice, WhyNot, Auth, Kind, ResetDone)	For each word enum: parse(as_str(x)) = Some(x); every other string is None (exact, case-sensitive); as_str injective and equal to the design table. Decidable by enumeration.	1	2/2
acc-advise-rule1-guards	contract	accounts::{advise,advise_with}	Off ⇒ Stay(Off); empty roster ⇒ Stay(NoRoster); unlisted ⇒ Stay(NotListed); not default home ⇒ Stay(OtherHome); not a stored login ⇒ Stay(NotAStoredLogin), in that order, before any state is read.	1,3	3/3
acc-advise-rule2-follow-and-canary	contract	accounts::{advise_with,State::canary_holds}	Past rule 1, an active account A ≠ mine that runs and is unproven is followed with takes_canary only when no canary holds and (at a wall or A fresh), else Stay(Canary); a fresh, or a running pinned, A is followed; a low proven unpinned A is not followed here.	1,3,4	3/3
acc-candidates-order	contract	accounts::{candidates,Rank::key,usable_at,next_candidate}	candidates is a permutation of the roster with verdicts by condition (Me, NotEnrolled, SignedOut, Limited, LowAheadOfAWall, LowWhileOneIsNear, Ok); Oks first sorted by (low, class, week_reset none, week_reset, label), Errs after in roster order; next_candidate = the first Ok.	1,3,5,7,13	2/2
acc-advise-rule3-wall	contract	accounts::{advise_with,wait_instant,breaker_until}	At a wall: follow a low proven active when the next candidate is low or the breaker stands; else Stay(Breaker); else Move{rotates:true} to the next candidate; with none, Wait{wait_instant} if in the future else Stay(NoCandidate). The answer is the same whether or not my wall is recorded. A lost login signed back in moves to me.	1,3,4,5,7	3/3
acc-next-usable-opening-hold	contract	accounts::{next_usable,next_opening,hold_until,State::canary_until}	next_usable = the earliest usable instant over rows, a dedicated row within DEDICATED_WAIT_S of it preferred; next_opening the same over instants > t; hold_until(Canary|Breaker) = min of the hold and the next opening, else None.	1,3,7	2/2
acc-advise-rule4-early	contract	accounts::{advise_with,probe_due,offer_expiring}	At a turn end with mode Early: pinned ⇒ Stay; due probe ⇒ Probe (not for an unanswered or limited or held account); not leaving ⇒ NothingDue; else OfferExpiring / NoFresh / Canary / Breaker stays, else Move{ahead:true,rotates:true}.	1,3,4	5/5
acc-advise-past-probe	equivalence	accounts::{advise,advise_past_probe}	advise_past_probe ≠ Probe; equals advise where advise ≠ Probe; where advise = Probe it equals advise with a fresh own probe claim.	1,3,4,5	1/1
acc-probe-claim-spacing	contract	accounts::{claim_probe,probe_held}	probe_held ⇔ a claim at ≤ t within PROBE_MIN_S (same account) or PROBE_APART_S (other); claim_probe succeeds iff admissible, sid non-empty and not held; an account is never probed twice within PROBE_MIN_S.	1,2	1/1
acc-probe-interval	contract	accounts::{probe_interval,probe_due}	probe_interval = clamp(min over comparable windows of (threshold-pct)/rate / 2, PROBE_MIN_S, PROBE_MAX_S), PROBE_DEFAULT_S without two comparable samples; probe_due ⇔ no sample ∨ stamped ahead ∨ age ≥ interval.	3	1/1
acc-advise-resume	contract	accounts::{advise_with,Account::cleared_since}	At a dated raw-window wall row older than mine's owner-pin or matching scoped clearing, with mine running, advise = Resume{latest matching clearing capped at now} under every mode but Off; never at an undated row, a clearing-second/newer row, a turn end, another account's clearing or a different raw-window scoped clearing.	1,3	1/1
acc-advice-commits	invariant	accounts::{advise,note_wall,begin_rotation,strike}	∀ reachable s: advice on s equals advice with my wall recorded; a rotating Move always commits through begin_rotation (never Err(Pinned)); a NotMoved strike then restores the state; a Resume writes nothing.	1,2,3,4,5,10	2/2
acc-begin-rotation-cas	contract	accounts::begin_rotation	Err(NoSession|Raced|Pinned|Held|Spent|NotACandidate) in that order, each leaving s unchanged; Ok sets active'=to, previous, proven=false, pinned=false, a fresh canary, pushes undo (≤ MAX_UNDO) and counts the rotation.	1,2,3,4	2/2
acc-strike-undo-exact	contract	accounts::{strike,prove,forget_session}	strike NotMoved after begin_rotation restores the prior state exactly (pin off, rotation uncounted); a Wall/SignedOut/Failed strike restores but keeps the rotation and records the word; strikes from a non-canary sid or on a non-active account do nothing (or only record); prove once; forget_session drops the canary and its undo.	1,2,3,4	4/4
acc-canary-hold-take	contract	accounts::{take_canary,State::canary_holds}	canary_holds ⇔ an unproven canary with at ≤ t < at+CANARY_HOLD_S; take_canary succeeds iff active unproven, the canary equals the expected record, no hold stands and sid non-blank, changing only the canary and undo.	1,2	1/1
acc-rotation-breaker	contract	accounts::{rotations_spent,breaker_until,recent}	recent ⇔ 0 ≤ t-x < W; rotations_spent ⇔ empty roster ∨ recent rotations ≥ 2·|roster|; breaker_until = the instant the count drops below the bound.	1,3,7	2/2
acc-pin-unpin	contract	accounts::{pin,unpin,note_auth}	pin refuses an absent/unenrolled/signed-out row by name (state unchanged), else clears its walls and makes it active, proven and pinned; unpin emits Unpin iff pinned; a SignedOut on the pinned account ends the pin.	1,2,3	0/1
acc-note-observations	contract	accounts::{note_sample,note_offer,note_auth,note_session,State::sessions_on}	Each note_* records only newer admissible observations (a stamp ahead does not shadow), with its own event rule; note_offer keeps the kind its count is of, through the state file, and a row with no kind or an unknown one reads as none; note_session emits Move on an account change; ran_at survives forget_session; sessions_on is sorted.	1,2,3	1/1
acc-note-reading-max	contract	accounts::{note_reading,merge_fig,sample_window}	Per window: take the read when the held is void or the read's reset is later; same reset ⇒ the newest epoch cuts every reading served before it, then max pct (a tie keeps the newer); earlier ⇒ keep. Written iff a figure, a reset, an epoch or the key changed or the heartbeat elapsed.	1,2,3	1/1
acc-reset-claim	contract	accounts::{claim_reset,reset_in_flight}	claim_reset: Attempted for inadmissible/blank input; a claim in flight answers Mine only for the same sid,pid,start and raw window, otherwise Held{until}; a reset timestamp already attempted cannot be retargeted through another window. Otherwise Taken when the wall is newer or the old reset came, retaining the raw window; writes no ledger row.	1,2,3	1/1
acc-reset-outcome	contract	accounts::{reset_outcome,reset_reason,ResetDone::word}	reset_outcome only on an open matching account/until/sid claim: records done and one event. Used/Lifted mark only its raw-window Wall cleared when until matches and wall.at ≤ claim.at; another/newer wall or an unscoped legacy claim clears nothing. An exact generic session/week key refills only its own warning when that wall stood at claim and neither warning nor sample is newer than claim. Cleared records share the bounded wall map; no automatic global cleared_at write. Whatever the kind spent, only the claimed raw window is cleared. Used{K,Some(n)}'s reason is ResetKind::resets(K,n) + ' left', Walled's that + ', still walled'. Walled (spent, the line saying the wall still stands), Kept and Failed change no wall; reset_reason is the head before ` · `, cut to WORD_MAX on a char boundary, control chars folded.	1,2,3,7	1/1
acc-reset-allowed	contract	accounts::{reset_allowed,Roster::own}	reset_allowed: not-listed, not-default, active-other, other-login checked in order, else Ok; Roster::own() = [main, dedicated, no dir].	1,3,7	1/1
acc-event-line-roundtrip	round-trip	accounts::Event::{new,with_to,with_sid,because,to_json_line,of_json_line}	of_json_line(to_json_line(e)) = Some(e); the line is one bounded line with sorted keys; of_json_line is None for anything not one bounded object with integer t, a known kind and an account.	1,3,5,7,12	2/2
acc-wakes-table	table-property	accounts::Event::wakes	wakes ⇔ kind ∈ {Use,Unpin,Proven,Struck,ResetUsed,ResetKept,ResetFailed,Renewed} ∨ (Auth ∧ why=ok).	1	1/1
acc-store-load-save	contract	accounts::Store::{at,load,save,ensure,touch_wake,transact}, read_bounded, record_wall	load: missing ⇒ default with a fresh id, nothing created; Torn ⇒ empty state and a note; NotOurs/unreadable/oversize ⇒ Err, file untouched. save is atomic (0700/0600) and reads back equal; touch_wake replaces by rename; every write path refuses a NotOurs file byte-for-byte and an unmakeable directory.	1,8	5/5
acc-store-lock	contract	accounts::Store::{acquire,try_acquire,acquire_within,transact_within}, record_wall, record_gone	The lock excludes across handles and processes and frees on drop; acquire_within tries a bounded ladder; a transaction that cannot lock returns LOCK_HELD without reading or writing.	8,13	2/2
acc-ledger-file	contract	accounts::Store::{append,tail}	append([]) creates nothing; rows append as lines (0600) and the file is cut to its newest LEDGER_KEEP_ROWS past LEDGER_MAX_BYTES; tail(n) = the last n parsing rows in order.	1,7,8	1/1
acc-process-login-of-env	contract	accounts::ProcessLogin::{of_env,of_exec_env,stored}, names_another_login	of_env: login = the first non-empty LOGIN_ENV; default_home ⇔ no non-empty HOME_ENV; stored_login ⇔ no credential/profile/route name set non-empty and LOGIN_ENV not given twice differently; names matched exactly. of_exec_env(empty)=None, else of_env split at '='; of_exec_env(None).stored() = false.	1,3,7,11	3/3
acc-process-login-only-weakens	invariant	accounts::ProcessLogin::{with_profile_store,with_settings,me}	Each builder changes only stored_login and never grants it; with_settings clears it for any unreadable/unparsable source, apiKeyHelper, or an env block setting a credential/profile/route/login/home name.	1,3,12	1/1
acc-profile-configs-dir	contract	accounts::{profile_configs_dir,profile_configured}	ANTHROPIC_CONFIG_DIR/configs, else XDG_CONFIG_HOME/anthropic/configs, else HOME/.config/anthropic/configs, else None; profile_configured ⇔ unlistable-but-present or a *.json entry.	1,8	0/0
acc-judged-and-footer-label	contract	accounts::{judged_label,footer_label}	judged_label = me(R).account iff default home ∧ stored login; footer_label = judged_label iff ≥ 2 rows.	1,3	2/2
acc-roster-beside	contract	accounts::{roster_beside,default_roster_path}	roster_beside(c) = parent(c)/accounts.toml when c has a file name, None for / and "".	1,8	1/1
acc-record-wall-decision	contract	accounts::record_wall	NotListed, NotStored, NotThisProcess (row older than the process) touch no store; otherwise one transaction notes the session and the wall for the process login's account (whatever is active, no strike), returning Wall/Unchanged/Cleared; wake touched iff a waking event was written.	1,2,3,4,8	6/6
acc-record-gone	contract	accounts::record_gone	Ok(false) creating nothing when no state, no directory or unknown sid; else forget_session and Ok(true), touching wake iff the sid was the canary.	1,2,8	1/1
acc-drop-stale-sessions	contract	accounts::drop_stale_sessions, Store::transact	Removes exactly sessions older than SESSION_KEEP_S (ahead stamps kept), applied only when the transaction writes.	1,2,8	1/1
acc-pool-launch-row	contract	harness::accounts_pool::{launch_env,record_usage}, accounts::launch_row	launch_env(seat,cli,read) = None for an unmeasured or keyring-held CLI; else over the bound roster (logins.json's renewed homes): under accounts = "off" only a pinned row of this machine; otherwise record_usage reads each enrolled row's usage in its own home, notes samples and walls in one transaction, and launch_row picks the pin, else a fresh active row, else the next usable row, else an active row that still runs; a row whose overlay home drifts (shared entries of its own) is never started on.	1,3,8	1/1
acc-codex-usage-of	contract	harness::codex_app_login::usage_of	usage_of(account/rateLimits/read result, now) buckets each window by its duration (Window::bucket), each share taken up to the next whole percent; a wall is set iff a window is ≥ 100%, ordinaryUsageAllowed = false, or rateLimitReachedType ≠ null, ending at that window's resetsAt.	1,3	1/1
acc-session-move-proof	contract	harness::accounts_pool::session_move	At TurnEnd the session's row is proved and the session noted in the record transaction; a keyring-held CLI stays (keyring-store); a line that cannot be written stays (no-line) and commits no rotation.	1,2,3,8	1/1
oauth-mail-filter	contract	harness::headless_oauth::{MailFilter::admits,domain_names,mail_from_and_date,parse_mail_date}	With no senders and no since, every mail is admitted; else a mail is admitted iff its From address's domain names a sender word as whole labels (a dotted word: the domain or its tail) and its Date ≥ since − MAIL_CLOCK_SLACK_S; a mail with no readable sender or date is refused. A refused mail gives no credential and is never marked consumed.	1,3,7	1/1
oauth-flow-bound	contract	harness::headless_oauth::{FlowBound::of,FlowBound::outside,vendor_step}	A vendor sign-in is bound to its first page's client_id, redirect_uri, state and loopback port: a page or mailed link naming another client_id or redirect_uri, or handing its login off this machine and off the pinned hosts, is refused; a loopback page ends the drive only on the started port.	1,3	1/1
oauth-exact-identity	contract	harness::headless_oauth::{vendor_profile_dir,another_account,shows_email}, MailboxConfig::{alias_label_to_email,contested_labels}	A vendor sign-in's browser profile is keyed by the row's exact lowercased email (a digest added where the key is lossy), so plus-addressed rows never share one; a page showing any email but the row's exact one (the vendor's own apart) as signed in is another account; Proof::Page needs the exact email beside signed-in words; a label that two pools give different emails aliases its secret onto Claude Code's row alone.	1,3,8	1/1
cli-grammar	contract	harness::cli::{parse,main_entry}	parse is the denotation of the USAGE grammar: every advertised form ↦ its Cmd, help anywhere ↦ the verb's page, every other word sequence Err (named "{verb} takes no {flag}"), main_entry exiting 2 before reading the environment.	1,3,7	4/4
cli-parse-defer	contract	harness::cli::parse_defer	Ok(n·unit) iff digits n>0 with optional s|m|h|d and the span ≤ 30 days; Err otherwise.	1,3	1/1
cli-retired-verbs	contract	harness::cli::{run,answer_retired}	install/uninstall exit 2 with RETIRED, writing nothing; hook/statusline exit 0, draining a pipe silently or saying one `retired` line at a terminal.	1,8,11	2/2
cli-usage-transcript-pick	contract	harness::cli::{pick_transcript,run_usage}	The transcript is picked by CLAUDE_CODE_SESSION_ID, else CLAUDE_PID's registry, else the newest (ambiguity printed), else none (never zeros); a relative CLAUDE_CONFIG_DIR folds nothing; a named session folds its subagents and never a neighbour's.	1,7,8	3/3
cli-usage-account-attribution	contract	harness::cli::{run_usage,ClaudeProcess}	The HUD names "acct L" iff the registry row names this session with a matching kernel start, the exec env reads, the roster lists ≥ 2 and judged_label = L; the process is read at most once; a tap reading shows only for this tab's pid.	1,4,8	2/2
cli-limits-view	contract	harness::cli::{limits_view,limits_text}, aterm_phase::classify_wall	The memory banner is its own no-limit wall; a bare reset clock is the next instant after the row's print at that zone's offset then (DST-correct; dated resets use that date's offset); limits_text starts `wall=… resets_at=…`.	1,3,7,12,13	3/3
cli-limits-read-once	contract	harness::cli::run_limits_with	With a sid: exactly one `text --json` ctl call, exit 0 or 1 naming the sid on failure; with none: exit 2 and no call.	1,4,11	2/2
cli-disk-write-ahead-journal	invariant	harness::cli::{disk_tick,run (Disk),bound_ledger}, disk::apply	Every run appends a report row; a `removing` row is durably appended before each removal and a failed append is a denial with nothing removed; outcome rows follow; bound_ledger cuts to the newest rows past the bound and is otherwise byte-identical.	1,8,13	5/5
cli-disk-tick	contract	harness::cli::{disk_tick,on_volume}	Plenty and no calls at or above the floor or with auto off; below it apply_auto over the targets on the measured volume only; a target named twice is one row and links survive.	1,3,8,13	5/5
cli-disk-verb-report	contract	harness::cli::run (Disk), disk::Report::headline	`disk` without --apply removes nothing and names the flag (and the config switch only when the file turns it off); --apply c removes exactly c's removable rows in report order; --json carries per-row outcomes.	1,4,8	4/4
cli-disk-config-fail-closed	contract	harness::cli::{disk_config_from,disk_config,DiskReading::at_or_above_floor}, disk::Config::below_auto_floor	Readable [disk] knobs are read whatever the TOML spelling; any unreadable or misplaced removal knob, a non-TOML file or a non-table `disk` turns every removal off with exactly one note (floor kept only if read cleanly); an unreadable file is off-by-fault; at_or_above_floor is false with no floor.	1,3,7,12	6/6
cli-harness-switch-misplaced-off	contract	supervise::config::SupervisorConfig::{from_aterm_toml,from_path}	enabled=false whenever the kill switch is written under [harness] or under any other table (noted with its dotted path).	1,7,12	1/1
cli-upgrade-sweep-gates	contract	harness::cli::run (Upgrade, UpgradeOwner)	With the master switch off a hand-run upgrade prints step=refused:bypassed, exits 1 (dry 0), takes no lock and names restarts in flight; a held lock is busy:another-sweep exit 75; unopenable lock or unwritable state exit 1; an owner's word kept out by a sweep exits 75 writing nothing.	1,8	4/4
cli-upgrade-owner-status	contract	harness::cli::run (UpgradeOwner), harness::upgrade_drive	--status lists recorded upgrades with a live holder, takes no lock; unwatched/claimed/outside-aterm rules as stated; --now/--defer/--skip write the request and name the switch that keeps it off, and an unrecorded tab exits 1.	1,8,13	5/5
cli-upgrade-lines	contract	harness::cli::{upgrade_line_with,upgrade_ledger_line}, upgrade_drive::gave_up_words, harness::one_line	The dry-run line carries pending_for and wait_for in compact h/m ('-' when caught up); gave_up_words names up to HELD_NAMED holders then "; and k more" within GAVE_UP_DETAIL_BYTES, fixed under one_line.	1,3,7	3/3
cli-ledger-verb	contract	harness::cli::{run (Ledger),tail_lines,accounts_ledger_line}	tail_lines = the last min(n,READ_MAX_ROWS) non-empty lines; `ledger` prints the newest rows of the chosen file (approvals filtered by sid), JSON verbatim, accounts rows rendered; non-rows have control chars folded.	1,7,8	3/3
cli-accounts-report	contract	harness::cli::run_accounts (Report), Env::{accounts_roster,accounts_store}	The report never writes; no roster names the file and the add line; a refused roster exits 1 and every write verb refuses; otherwise the fixed lines/JSON (schema 1, 22 row keys, null for unobserved).	1,4,7,8	7/7
cli-accounts-add	contract	harness::cli::run_accounts (Add, AdoptDefault), harness::cli::wake_for_roster	add refuses (exit 1, nothing written) an inadmissible/listed label, main first, a relative state dir, a dangling symlink, an inline array (printing the row); otherwise appends via append_row (first add lists main), makes the login dir 0700, writes the roster (through a link keeping its mode), and what it wrote admits. After its roster write, add (and adopt-default after its row) replaces the accounts store's wake marker, so a running aterm's host hears of the row at once rather than at its minute-long look at the roster's bytes; a refusal replaces no marker.	1,4,8	6/6
cli-accounts-login-table	contract	harness::cli::{run_accounts (Login),login_check}, LoginCheck::auth	Over an abstract AccountsHost: prints the login line; not a terminal ⇒ exit 0 nothing run; unknown label or unaskable ⇒ exit 1; the flow runs once; login_check maps the auth status to SignedIn (record, wake, exit 0) / SignedOut (record, exit 1) / CouldNotRun (nothing recorded); a NotOurs store says NOT recorded and exits 1.	1,4,8,11	9/9
cli-login-secrecy	other	harness::cli::{run_accounts (Login, Report),login_check}	Noninterference: every output and file depends on the vendor's auth-status bytes only through login_check's verdict and a fixed reason vocabulary.	1,4,8	1/1
cli-login-runners	contract	harness::cli::{login_flow_of,login_command,auth_status_within}	login runs `p auth login` with HOME_ENV=dir (removed for None) and LOGIN_ENV removed, returns the exit code (128+signal) and restores signal dispositions; auth_status_within refuses without USER/LOGNAME and otherwise runs `p auth status --json` once, bounded, with only the auth_check_env names.	8,13	2/2
cli-auth-check-env	contract	harness::cli::auth_check_env	Err(AUTH_CHECK_NO_USER) iff USER and LOGNAME are both unset/empty; else the set non-empty names among USER, LOGNAME, ATERM_CHILD in order, nothing else.	1,3,13	1/1
cli-accounts-use	contract	harness::cli::run_accounts (Use, Auto)	use L = pin in one transaction (refusal plus remedy, nothing changed; success prints and ledgers Use, wakes); use --auto = unpin ("no pin holds" writes nothing).	1,4,8	2/2
cli-accounts-schema-refusal	contract	harness::cli::run_accounts	A state file of another schema is refused by every write form (exit 1, file untouched); the report shows an empty state with a note; a torn file is replaced by the next write.	1,4,8	1/1
disk-report-classification	contract	harness::disk::{report,scan,scan_target}	CargoTargets rows only for marked targets with an idle unlocked profile (bytes = its incremental/, witness StaleBuildDir), else a note; version rows removable only when the live version resolves; AtpkgGc rows never removable; one VendorReports purge row; scan resolves the live link.	1,3,7,8	7/7
disk-free-warn	contract	harness::disk::{report,Report::{headline,to_json}}	warn ⇔ free known ∧ free < warn_free_gib·GiB; unknown free ⇒ no warn, free=unknown, null in JSON.	1	1/1
disk-apply-guards	contract	harness::disk::{apply,plan,guard,auto_plan,apply_auto}	remover runs only for a planned, removable row of the class that guard accepts (anchored, outside the transcripts root, reported) and whose Removing row was journalled; each refusal is one Denial; remover errors are remove-failed and the rest continue; auto_plan only below the floor and only CargoTargets.	1,3,7,11,13	4/4
disk-tree-nofollow	contract	harness::disk::{remove_tree,dir_bytes}	remove_tree refuses a symlink untouched; dir_bytes sums regular files without following links.	7,8	2/2
disk-class-table	table-property	harness::disk::Class::{parse,as_str,delegate,ALL}, Delegate::command	parse(as_str(c)) = c and nothing else parses; delegate names the fixed commands.	1	1/1
disk-age-days	contract	harness::disk::age_days	age_days(now,t) = max(0, now-t) div 86400; a future mtime is age 0.	0	1/1
disk-human-bytes	contract	harness::disk::human_bytes	"{n} B" below 1024, else one decimal in the largest of KiB..TiB that fits.	1,3	1/1
disk-report-json-headline	contract	harness::disk::Report::{to_json,headline,…}, report_row, removal_row, denial_row, step_row	to_json is one line with the fixed keys, reclaimable sums and every row with its witness; the headline keeps pressure rows apart as an upper bound; the journal rows are one-line JSON of their kind.	1,3,7	2/2
disk-target-dedupe-nested	invariant	harness::disk::{scan,report}	Build directories are keyed by canonical path (one row per directory); a nested profile is judged and counted by its innermost build directory only.	1,5,7,8	4/4
disk-pressure-pass	contract	harness::disk::{report,apply_auto}, Config::pressure_target_bytes	Below the floor, LRU rows for probed unlocked same-volume profiles older than PRESSURE_MIN_IDLE_S follow the idle rows in last-use order; apply_auto takes idle rows, then LRU rows each after a measure, stopping at the target, at the counted shortfall, or on a failed measure; a locked profile is a denial with its cache intact.	1,3,7,8,13	12/12
disk-target-assess-witness	contract	harness::disk::target::{assess,target_witness,Select::refuses,TargetDir::reclaimable}	assess finds profiles at any depth within the walk budget; a witness needs a complete walk, no source files, and ≥1 profile idle by its cache directories' clock with a non-empty incremental/; an unread clock is said, never dated zero.	1,3,7,8	6/6
disk-target-reclaim-only-incremental	contract	harness::disk::target::{reclaim,reclaim_with,dev_of}	reclaim removes only incremental/ of the selected profiles; everything else byte-identical; nothing through a symlink; never another device; undeletable entries reported while siblings go; bytes = released blocks.	7,8	7/7
disk-target-reclaim-locking	contract	harness::disk::target::{reclaim_with,locks_held_with,HeldLock}	Every cargo lock of a profile is taken (created only when proceeding, with cargo's mode) and held through the delete, idleness re-checked under them; a held lock skips the profile; locks are close-on-exec.	8,13	7/7
disk-target-reclaim-lru-whole	contract	harness::disk::target::{reclaim_lru,reclaim_lru_with,reclaim_whole_with}	reclaim_lru takes only the named profile's incremental/ under its locks, refusing one written since the survey or just built; reclaim_whole_with removes the directory only after locking every profile and the owner check, deleting through its handle.	8,13	3/3
disk-discover-roots	contract	harness::disk::discover::{discover,Origin}	discover returns each marked build directory once by canonical path from exactly the listed root kinds, never hidden, Library, another app's data or a file-provider domain (`APP_DATA_ROOTS`, `FILE_PROVIDER_ROOTS`), or too-deep directories; ownership by checkout over link; Linked when only a scratchpad link names it.	1,7,8	5/5
disk-discover-xpy-rule	table-property	harness::disk::discover (x.py rule)	Under build/<host>/ only stage<N≥1>-<kind> and bootstrap-tools with a signed tag are build directories; stage0, sysroots, llvm and links never; the marker walk never enters an x.py build/.	1,7,8	2/2
disk-discover-budget-and-confinement	contract	harness::disk::discover::discover	A spent budget keeps what was found and says so (suppressing owner judgement); the search stays on the measured device, out of mount points and privacy folders, and skips unsafe temp roots.	1,7,8	5/5
disk-session-gone	contract	harness::disk::discover::{gone,claude_census,unused}	A session is gone only when the whole registry names it for no running pid, nothing of it was written in the window, and no possibly-holding Claude process exists; an unreadable process keeps it.	1,8,13	1/1
disk-whole-pass	contract	harness::disk::{report,apply_auto,step_row}	Below the floor with dead_build_dirs on, gone owners' directories are whole rows after the idle caches in last-write order, each after a measure; otherwise they are ordinary; the journal names the session.	1,3,7,8,13	2/2
disk-whole-guards-at-removal	contract	harness::disk::remover, discover::unused, target::reclaim_whole_with	At removal a whole row is refused, nothing deleted, for a live owner, a new lock, a resumed session, a process using the directory, an unreadable process, a failed listing, or a Linked row.	8,13	3/3
disk-hogs	contract	harness::disk hogs	The MAX_HOGS largest build directories no row frees, biggest first, nested counted outermost, x.py grouped; after a pass the hogs name what was left and why, clipping the path not the reason.	1,3,7,8	2/2
footer-model-display	contract	footer::model_display	With base = id up to '[': `claude-<f>-<v…>` with f alphabetic ↦ Capitalised f ++ " " ++ join('.', the all-digit parts shorter than 8); any other id ↦ itself.	1,3,7	1/1
footer-model-name	contract	footer::{footer_model_name,is_word}	Strip the (default)/(1M context)/(recommended) suffixes to a fixpoint; a `claude-` word ↦ model_display; else Some(n) iff 1..=64 chars of [A-Za-z0-9 .-_()[]] with no double space; else None.	1,3,7	1/1
footer-tail-floor-attribution	equivalence	footer::{tail_facts,scan}	tail_facts(body, floor) = Fold over the attributable main-chain rows newest first up to the first row stamped before the floor (unstamped rows dropped when a floor is given); a body whose newest stamped row precedes the floor gives default facts.	1,3,5,7,12	3/3
footer-tail-fold-refines-model	refinement	footer::{scan,fold_result,effort_result,TailFacts::*}, upgrade_models::model_result_of	scan equals ClaudeFooterModel.Read folded over the rows: answers meet the model; model results meet footer_model_name and decide effort by their clause; an unanswered /model or /effort decides Unread; /fast decides only with `model set to`; Kept model decides nothing; quoted tags are text; an older effort stands only for the same model; the fold stops when settled.	1,2,3,5,7,10,12	8/8
footer-chose-model-since	contract	footer::{chose_model_since,rows_back}	chose_model_since ⇔ a main-chain row stamped ≥ floor is a /model command (whatever its result) or a result that moved the model; Kept model, answers, other commands and quoted tags never count.	1,3,5,7,8,12	1/1
footer-rows-back-window-invariance	equivalence	footer::{rows_back,read_range,read_range_then}	rows_back yields every row once newest first, an oversized row once as an Oversized marker in place, independent of where the windows fall.	1,7,8	1/1
footer-tail-cache-equals-fresh-read	equivalence	footer::{TailCache::*,TailKey::of,read_tail_facts}	Each TailCache read returns what a fresh backward scan of the file's current content gives for that floor (plus the carried decision for the same process and file); it re-serves only when pid, start, floor, inode, size, mtime and guard hash are unchanged; failed opens are never cached; a new floor or replaced/shrunk file is read afresh.	1,2,5,7,8	4/4
footer-floor-of	contract	footer::floor_of	floor_of = max(started.or(proc_start), started_at) when both Some, else either; facts read no transcript model with no floor.	3	1/1
footer-process-session-switch-model	refinement	footer::{facts_for_entry_at,facts_for_entry_from,TailCache::{for_process,session_floor,process_of},resumed_model,…}	FooterModel extended with session switches: the model and effort shown after each read equal the model's, by the precedence own rows since the floor > a /resume's newest plain answer unless the process is pinned > a /clear's prior decision > launch --model > open.	1,2,5,7,8,10	8/8
footer-path-branch-follow	contract	footer::{facts_for_entry_at,last_cwd_since,git_head}	path = home_path(d), branch = git_head(d), d = the newest cwd since the floor else entry.cwd; the branch is re-read every call, even with a cached tail.	1,2,8	2/2
footer-last-cwd	contract	footer::{last_cwd,last_cwd_since}	last_cwd = the cwd of the newest main-chain row whose cwd is an absolute path string.	1,3,7,12	1/1
footer-session-entry-parse	contract	footer::{parse_session_entry,is_session_id,squash}	Some(e) ⇔ a JSON object with that pid, a matching squashed procStart when both known, a [A-Za-z0-9-]+ sessionId and a string cwd; fields projected as stated.	1,3,5,12	2/2
footer-lstart-utc	contract	footer::lstart_utc	lstart_utc(s) = DAY MON d hh:mm:ss yyyy from civil_from_days, equal to `ps -o lstart=` in UTC/C, whitespace squashed.	1,3,12	0/0
footer-project-slug	contract	footer::project_slug	Maps every non-ASCII-alphanumeric char of the lossy path to '-', keeping the char count.	1,3,7	1/1
footer-launch-facts	contract	footer::{launch_facts,launch_facts_of}, upgrade::{launch_model,launch_resume}	pinned ⇔ a non-empty --model or an ANTHROPIC_MODEL / ANTHROPIC_DEFAULT_<X>_MODEL env; model = model_display of a real id (aliases None); resume = Session(id) / Unnamed / None by the resume flags.	1,3,5,7	2/2
footer-card-fill	contract	footer::{FooterFacts::filled_from,launch_card}	filled_from fills only open fields (effort only for the same model, nothing from another build's card); launch_card only under a live REPL with no box over the composer.	1,3,12	2/2
footer-branch-of-head	contract	footer::{branch_of_head,printable}	`ref: refs/heads/x` ↦ printable(x) (non-empty, ≤128 chars, no control); a ≥7-hex head ↦ its first 7; else None.	1,3,7	1/1
footer-git-head-resolution	contract	footer::{read_git_head,read_git_head_in,resolve_git_dir_of,read_head,is_eperm,under_file_provider_domain}	Climb to the first .git (dir, or `gitdir:` file); Found(branch) when readable; Absent under a file-provider domain or another app's data (`APP_DATA_ROOTS`), without .git, or on non-EPERM failures; Denied iff EPERM.	1,7,8	4/4
open-regular-nonblocking	contract	footer::{open_regular,read_small,open_nonblocking,regular}, session_usage cursor open	open_regular opens O_NONBLOCK and keeps the handle only if fstat says regular file; a FIFO is never read or folded.	8	2/2
footer-home-and-elide-path	contract	footer::{home_path,elide_path}	home_path renders ~ / ~/rest by component prefix (none for a parentless home), else the lossy path, refusing control chars; elide_path = …/last when shorter.	1,3,7	2/2
footer-segments-order	contract	footer::{segments,fact_segments,text_width}	segments = [UPGRADE?] ++ present of [WALL, MODEL(+effort), ACCOUNT, PATH, BRANCH, USAGE] in that order, absent ones omitted, every mark width 1.	1,3,7	3/3
footer-rule-run	contract	footer::{rule_run,rule_room}	rule_run(c) = the first longest maximal run of trues, None iff none; rule_room = len ⊖ (RULE_LEAD+1).	1,5,7	1/1
footer-fit-rule-ladder	contract	footer::{fit_rule,facts_ladder,plain_ladder,rule_width,chips_room,top_of}	fit_rule returns the first fitting candidate of the fixed ladder (titled, reason rungs, untitled rungs, model alone, chips alone); later rungs drop tokens, cut path, branch, path, effort, model in order; the account whole or not; the wall and model are never dropped by a note; monotone in room.	1,3,5,7,12,13	5/5
footer-set-facts-typesetting	contract	footer::{set_facts,segments_width}	set_facts places every glyph in order at FACT_PITCH, GAP pitches between facts, loosened evenly; whole-cell width ≥ glyph cells; ends half a pitch in; wide chars two cells; ≤ one cell per char.	1,3,7	1/1
footer-mode-row-recognizer	contract	footer::{is_mode_row,mode_row,mode_row_under,pill_row_under,pill_indicator,opens_with_pill_glyph,is_cut_pill,after_pill,cycle_hint_len,PILLS}	pill_indicator recognises `  glyph text` for a PILLS pair; is_mode_row accepts only empty, gap, ` · …` or a word-cut prefix of the cycle hint after it; mode_row finds the first such row below the composer frame and never above it.	1,3,5,7,12	7/7
footer-shell-cwds	contract	footer::shell_cwds	The newest cwds (≠ launch) of the live sessions launched in that directory; dead pids and other launches add nothing.	1,7,8	1/1
claude-dir-of	contract	footer::claude_dir_of, upgrade_drive::claude_home_dir	absolute config ↦ itself; relative (incl. "") ↦ cwd()/c (cwd called only then); none ↦ home/.claude for an absolute home, else None.	1,3,13	1/1
footer-fold-is-one-session	invariant	footer::{read_pid,FooterCache::{usage,shown,forget},session_of_pid,transcript_path}, session_usage::wall_at	Every footer read shows usage and wall folded from the session the same read's registry names (and its subagents) only; one parse and one lookup per read; a torn read shows nothing and keeps the fold only for the same process and session; a fold behind shows no tokens or unreached wall; a later served response takes the wall down.	1,2,5,7,8,13	10/10
lights-read-screen	contract	lights::read_screen, Mode::of_indicator	None iff no composer; else Screen{mode of the pill row under the composer, pill present, busy = a hard busy signal}; a cut hint still names its mode.	1,3,12	6/6
lights-state-and-fold	contract	lights::{Reading::{state,fold},deviates,read}, Mode::is_expected	state = Unknown for no mode, On for Bypass/Auto, else Off; deviates ⇔ Off; fold updates the mode iff the newer read has a mode or a pill.	2,3	3/3
lights-mode-pill-table	table-property	lights::Mode::{of_indicator,glyph,word,hue,is_expected}, footer::PILLS	Each Mode has exactly one PILLS entry, glyph non-empty, of_indicator injective; hue ⇔ is_expected ⇔ Bypass/Auto.	3,7	1/1
truncate-bytes	contract	harness::truncate_bytes	Returns s[..e] with e the largest char boundary ≤ max (s itself when short); never panics or grows.	1,3,7	1/1
one-line-shortcut-equivalence	equivalence	harness::{one_line,is_one_line_already}, supervise::limit::one_line	one_line(t,max) = limit::one_line(truncate_bytes(t,max)).trim(): the shortcut returns t only where the fold would; every break/control incl. U+2028/9 becomes a space.	1,3,7	1/1
lanes-table	table-property	harness::lanes::lanes, Program::supervisable	relaunch = upgrade = supervisable(p); restart_in_place ⇔ p = Claude.	3,12	2/2
netstate-leases-bounded	invariant	netwatch::NetState::{ask,waiting}	ask keeps only waiters younger than NET_LEASE plus the asker and begins no probe; waiters ≤ distinct recent askers.	1,2,5	1/1
netstate-schedule	contract	netwatch::NetState::{next_probe,begin,finish}	next_probe = None with nobody waiting or a probe in flight, now with no fresh measure, else m.at + (GAP_CADENCE after a Down or Unknown that no fresh measure preceded, DOWN_CADENCE[downs-1] after any other Down, STEADY_CADENCE otherwise); begin refuses a second flight; finish counts downs, not the one that ended a gap.	2,3,5	2/2
netstate-reach-freshness	contract	netwatch::NetState::{reach,fresh,finish}	reach = Unknown for an overrun flight, else the last measure while fresh on both clocks (small wall steps back tolerated), else Unknown; since kept across the same fresh outcome.	2,3,5	2/2
netwatch-route-of	contract	netwatch::{route_of,names_a_route,settings_route}	Default ⇔ the env is known, names no route (ROUTE_KEYS or prefixes with non-blank value) and every settings source is missing or quiet; else Custom(first reason).	1,3,7	1/1
netwatch-settings-of	contract	netwatch::settings_of, claude_settings::parse	None iff not an object or a non-object env; else env pairs in order (strings, null as "", others as JSON), api_key_helper, status_line, hooks_off as stated.	1,3,12	2/2
relaunch-held-wait-table	table-property	relaunch::{held_wait,ends_at_once}, Timings	t = Timings::PRODUCTION: held_wait(t, CAUSE_HOST) = t.restored_held = 7 s > t.ended_at_once; held_wait(t, c) = t.held = 90 s for c ∈ {exit, "", fresh}; ends_at_once("") ∧ ends_at_once(fresh).	3	1/1
harness-timings-orders	table-property	harness::timings::{Timings::PRODUCTION,Timings::FAST,FAST_SEAM}	∀ t ∈ {PRODUCTION, FAST}: t.restored_held > t.ended_at_once, t.shell_look < t.ended_at_once, t.restored_held < t.held. Every FAST bound ≤ its PRODUCTION bound except claim_ttl, where FAST.claim_ttl > MUST_HAPPEN (a lease never lapses inside the wait for its release). FAST_SEAM is a name the child-environment deny list holds.	3	1/1
relaunch-program-identity	contract	relaunch::launched	Some(program) iff the process runs Claude itself: a claude image, or an interpreter whose absolute argv[1] script is Claude; wrappers, relative scripts and unknown images give None.	1,3,7	1/1
relaunch-on-exit-refines-model	refinement	relaunch::Relaunches::{exited,exited_as,attempted,decide,pending,running,ask,conversation_runs,asks,failing,crash,counts,forgive,restarted}, on_exit, outcome	Driving the real Relaunches with the HarnessRelaunchOnExit actions keeps project(state) = model state and every invariant: owned exits never relaunched, limits said, back-off 1/10/60/600 s forgiven after a healthy run and climbed only by a crash's attempts (NoCrashNoStreak), misses said once, a word asking for a conversation standing until it runs (AskStandsForItsConversation), a failing relaunch's word ending with its trying (TriesOnlyWhileDue; running() while it keeps failing = PersonReturns then PersonRuns).	2,3,5,10	5/5
relaunch-ended-at-once-shell-look	contract	relaunch::{ShellBack::{after,due,saw},line_over,first_idle}	ShellBack::after(block, t) looks every t.shell_look; it reads the relaunched agent as ended iff its line was seen to run (the shell not holding the terminal, or line_over of its block) and every later look over t.ended_at_once found the shell holding the tab.	2,3,5,7	1/1
relaunch-one-shot	contract	upgrade::one_shot	⇔ some -p/--print stands at a flag position (before `--`, not a preceding flag's value).	1,3,7	1/1
relaunch-promise-iff-plan	equivalence	relaunch::{resumes_on_exit,carries_restart,exit_line,plan,line_for,required_env}, upgrade::rewrite_argv, upgrade_drive::home_carried	resumes_on_exit(h,s) ⇔ plan(s) yields a line for a user whose home is h (registered conversation, argv rewritable, not one-shot, known dialect, the Claude home its line starts it under is the one its records are in, line_for Ok with the home and login); carries_restart(h,s) = resumes_on_exit(h,s) for Claude Code; for a Codex, the snapshot carries a copy-validated line (self_claimed_line).	1,3,4,7	2/2
relaunch-snapshot-follow	contract	relaunch::{follow,foreground}	follow moves session/version/cwd to Claude's record iff it names the same pid, start and a session id, returning whether it moved; foreground reads the published foreground group.	1,2,8,12	1/1
relaunch-target-build	contract	relaunch::relaunch_target, upgrade_build_start::proved	The newest installed build newer than the running version the move may take, while upgrade is on, the version reads and held(t) is false (the owner's word, a failed start, or a build no probe has proved to start); else the program it ran.	1,3,7	1/1
relaunch-resumed-prompt-one-line	contract	relaunch::resumed_prompt	One line (no break/control), starting with the continuation head, ending with keep going, naming the cause class.	1,3	1/1
relaunch-exit-record-look	contract	relaunch::{exit_record,look_at_exit}, ExitRecord::{crashed,word}	Claude: absent at the first look ⇒ Removed; gone before the settle ⇒ Removed; present through the settle ⇒ Crashed (not earlier); nothing decided before EXIT_GONE while running. No Codex end is read here (a codex-alab's is its copy's).	3,5,7,13	1/1
relaunch-permission-mode	contract	relaunch::{permission_mode_of,with_permission_mode,mode_flag,line_for}	permission_mode_of = the newest known mode row; with_permission_mode replaces every --permission-mode with the named mode (bypass kept only for a bypass launch); mode_flag(Manual)=default; the line carries the mode past many-valued flags and `--`.	1,3,7	4/4
relaunch-quiet-outcome	contract	relaunch::{quiet_outcome,outcome_line}	quiet_outcome names the build and the models and never `unconfirmed`; the ask flow's outcome_line with no model says `unconfirmed`.	1,3	1/1
relaunch-line-env-quoting	round-trip	relaunch::{login_line,line_for,line_tapped,with_tap,login_env,required_env}, upgrade::relaunch_line_env	For zsh/bash/fish, the shell's parse of login_line yields exactly the required assignments in order (values verbatim, empty kept as '') ahead of the switch and argv (tap before --resume); Err(line) iff a value has a control char or fish has a cd with env; R=[] is the golden line; with_tap never adds a second --resume.	1,3,5,7	4/4
relaunch-line-drop-order	contract	relaunch::{login_line,line_tapped,line_for}	The line typed is the first of [R+switch+tap, R+switch, R] within the kernel's bound, else Err(line); the login is never dropped.	1,3,7	2/2
relaunch-account-login	contract	relaunch::{account_login,runs_under}	account_login = the row's dir verbatim ("" for the dirless row) for a listed label; runs_under ⇔ the process's LOGIN_ENV equals it byte for byte (unset = empty).	1,3	1/1
relaunch-cause-classes	table-property	relaunch::{relaunch_cause,ends_its_agent,carried_quietly,starts_afresh,ends_at_once,model_restart}	`account:<label>` is a relaunch cause that ended its agent, carried quietly, ends at once, not afresh; look-alikes and "" are the upgrade's; CAUSE_ASKED is a relaunch cause that did not end its agent; model_restart carries what rides.	1,3	2/2
relaunch-carried-word	contract	relaunch::carried_word	wait:restart-in-flight when an account move meets another restart in flight and the step is no failure; else the step.	1,3	1/1
relaunch-cold-login	contract	relaunch::cold_login	Some(account_login(active)) iff ≥ 2 listed and enrolled, active listed/enrolled/proven/not signed out, default home; else None.	1,3,5	1/1
restart-stamp-file	round-trip	restart_stamp::{stamp,read,path}, Stamp::{to_json,from_json}	For a plain-word tab key, stamp then read = identity, one file per tab, no temp left, no `phase` key; a non-word key writes nothing and Errs.	1,8	1/1
restart-stamp-take-if	contract	restart_stamp::take_if	Takes and removes the stamp iff the one present when judged satisfies ours; a refused stamp is never moved; a stamp written in between is put back.	8,13	1/1
restart-stamp-lookup-prune	contract	restart_stamp::{lookup,standing,prune}, Stamp::{stale,stands_for}	lookup names the cause for its process while fresh (and sweeps gone/stale stamps); standing reads only; prune removes exactly stale stamps; stale ⇔ now - at ≥ STALE_S.	1,8	3/3
kernel-start-spelling	equivalence	upgrade_drive::kernel_start, restart_stamp::kernel_start	lstart_utc(kernel_start(pid)) = `ps -o lstart=` for a live pid, read by syscall. On Windows, where no `ps` answers `lstart`, restart_stamp::kernel_start is lstart_utc of read_process_birth (GetProcessTimes) alone, so the restart stamp and a proven self-claim read a start there.	8	1/1
restart-stamp-consume	contract	relaunch::consume_stamp	Removes the tab's stamp iff it names this process and it is no dry run; the filed cause is CAUSE_ASKED.	1,8	0/0
resume-line-of-pid	contract	resume::{line_of,command}, footer::read_pid	line_of = Some(command(argv, that pid's own session id)) iff its record parses; never a sibling's, never --continue.	1,4,8,12	1/1
resume-command-flags	contract	resume::{command,bare_of}, upgrade::rewrite_argv	command = `claude <kept flags, shell-quoted> --resume <id>` iff rewrite_argv Ok and resumable; resume family, --effort and positionals dropped; else the bare line or None.	1,3,4,7	1/1
resume-never-cut	contract	resume::never_cut	Renders the first of [whole line, mode alone, bare, no clause] the renderer leaves uncut; never a strict prefix of the session id.	1,3,13	1/1
usage-incremental-fold-equals-one-pass	equivalence	session_usage::TranscriptUsage::{advance,fold_line,restarts}	Once at EOF, the cursor's fold equals the one-pass fold of the final content for any append history, cut points and budgets; torn lines wait; ids counted once; oversize or non-UTF-8 lines counted unparsed; shrink, rotation or a changed boundary byte restarts the fold.	1,2,5,7,8	9/9
usage-read-cost	other	session_usage::TranscriptUsage::advance	On growth an advance reads exactly the appended bytes plus the boundary byte; unchanged files are not opened; only a torn tail is retained.	1,2,8	3/3
usage-session-aggregate	invariant	session_usage::SessionUsage::{refresh,total_fold,complete,subagent_files}	total_fold = main + Σ followed subagent files (agent-*.jsonl only); followed files are never dropped; quiet files release ids; unreadable or over-bound files count once as unread; no new file opened once the budget is spent; complete ⇔ caught up with nothing unread and the walk whole; otherwise `+N unread` is said.	1,2,5,7,8	12/12
usage-moved	contract	session_usage::SessionUsage::moved	moved ⇔ behind with progress possible, or a stat of the transcript, a followed file, or a walked directory changed (a deleted followed file is no change).	1,2,7,8	3/3
usage-wall-of	contract	session_usage::{wall_of,notices}, limit::{parse_reset,reset_in_zone}	wall_of = the newest notice's reset when placeable, in the future, with no response served after it (weekly clock next after the notice, session today's, dated by date, spans from the notice, DST honoured); the zone is omitted iff it matches the local offset then.	1,3,5,13	8/8
usage-tokens-short	contract	session_usage::tokens_short	The shortest unit form never rounding up, total at u64::MAX.	1,3	1/1
usage-text-and-model-naming	contract	session_usage::{usage_text,usage_text_at,model_tokens,spend,runtime_short,wall_text}, UsageFacts::{with_runtime,wall_only}	model_tokens names models uniquely (provider ids as their model, one model by two routes once), drops zero, orders by tokens, counts the rest; sums saturate; Total shows runtime and merged tokens; unread is always said.	1,3,5,7	5/5
tap-value-ours	round-trip	tap::{settings_value,is_ours,carried_by}	is_ours(v) ⇔ v = settings_value(p) for an admissible absolute p; other values carried verbatim; carried_by finds either spelling before `--`.	1,3,7	1/1
tap-verb-total	contract	tap::{tap,run,line_of}	Always prints one non-empty line and exits 0; writes a v1 0600 file in a 0700 dir iff JSON with figures, tab and pid named and the dir made; otherwise the dash line.	1,8	1/1
tap-noninterference	other	tap::tap, cli::main_entry	Inputs agreeing on the kept fields and the four read variables give identical file and line; no other field or variable (secrets) reaches either.	1,8,16	1/1
tap-read-own	contract	tap::read	Some iff the (tab,pid) file is regular (no link), names the same tab/pid with v1, and was written at or after started.	1,8	1/1
tap-freshness	contract	tap::Freshness::admit	admit ⇔ figures ∧ (new process ∨ figures changed ∨ api_ms rose); an idle repeat never.	2,3	1/1
tap-silence-machine	contract	tap::Silence::{step,silent,untapped}	Silent at most once, at the second of two points ≥ 30 s apart on a served process up ≥ 30 s with a missing/stale file; unmarked on news, relaunch or no tap; a new process starts over.	2,3,5	2/2
tap-sweep	contract	tap::sweep	Removes exactly dead processes' files, over-age files and stale temps; keeps live files and foreign names.	1,7,8	1/1
tap-gate	contract	tap::gate	Some(tap) ⇔ the conjunction of every guard (switch, nameable path, stored login, no own home/profile, no foreign --settings, no --bare, argv rewritable, a readable settings chain that sets no statusLine or only the owner's own plain command in the user settings.json, which the value then carries as `--carry`, of one length whatever the command (the tap reads the command when it runs; a legacy `--then <hex>` value is still ours): R-12); each guard alone negated gives None.	1,3,4	1/1
tap-launch-gate	contract	tap::{launch_value,launch_admits}	launch_value = gate's value iff armed, an aterm tab id, no --no-harness, HOME absolute and the account's own, the line not already tapped, and its first operand (if any) no subcommand-like word; each guard alone negated gives None; the managed Claude Code twin puts a Some ahead of the launch's own arguments and runs every None as typed.	1,3,4	3/3
tap-line-kept	contract	relaunch::{line_tap_kept,note_tap_dropped}	The relaunch line carries the tap whenever the tapped line fits the dialect's line limit; it goes without it only when it does not, and then the reason is returned and written as one tap-dropped ledger row (none on a dry run).	1,3	2/2
tap-room	contract	relaunch::tap_room	tap_room ⇔ the relaunch line of the published snapshot, with room kept for a model and a mode, keeps the tap; the footer says `until relaunch` only then, else `untapped` (Refusal::Room).	1,3	2/2
tap-row-safe	contract	tap::line_of, aterm_phase::read, footer::{mode_row,live_repl_row}	With the tap's line in its row, the phase reads idle with no wall and the footer readers find the same rows; the line has no vendor guarded string.	1,3,5,12	1/1
tap-clock-window-of-wall	contract	supervise::tap_clock::UsageWindow::{of_wall,len_s,word}	of_wall(kind, message) = [five_hour] for a session wall whose notice names a session window, [seven_day] for a weekly wall, both for a session wall that names no window, [] for every other kind (a spend wall included: R-9's credits rule decides it by whether a reset is named); len_s is 5 h and 7 d; word is the payload's name.	1,3,12	1/1
tap-clock-step-says	contract	supervise::tap_clock::TapClock::{new,step,says}	A reading counts iff read_at ≥ first_read (an idle repeat written since counts); a window is full iff its figure's x10 ≥ 1000 (a 99.6 is not); a full window times the wall iff 0 < resets_at − now ≤ len_s, the later of the windows timed, the newest timing reading winning; says = ResetsAt(timed) if any, else RolledAt(the latest) iff every window of the wall shown full has since been shown dropped or not full (a later full reading undoes the rollover), else Nothing; folding the same reading again changes nothing.	1,2,3,5,7	5/5
tap-clock-timed	contract	supervise::tap_clock::timed	timed(notice, ResetsAt(t)) = t within CLOCK_SLACK_S of the notice (inclusive) or with no notice, else max(notice, t) with disagree = (notice, t); timed(None, RolledAt(r)) = r; otherwise the notice's.	1,3	1/1
tap-wall-wait	contract	Session::{tap_reset,tap_timing,tap_edge,tap_left_wall,wall_reset_unix}, IdleHost::tap_window	At a Claude Code wall a window times, a decision's read folds the host's readings into the wall's TapClock (kept per wall key from its first read, and started afresh where a turn the loop saw run ends on the wall again or the wall answers an act of the loop's) and the wait is timed(notice, says) + grace + the session's jitter of that reset, the notice's reset of a limit only the transcript shows being its row's quotaLimits.resetsAt; a disagreement past the slack writes one tap-reset@v1 ledger row per pair, never an alert; a reading during the wait that moves the reset decides the point again; a wall whose notice left the screen is retimed the same way, the policy's limit taking the tap's reset from then on; the wall-wait@v1 row keeps the notice's placement (reset_unix=) apart from the tap's reset (tap_unix=), so a successor loop waits to the tap's until its own tap reads, sets that reading against the notice's reset, and drops the carried reset at a new answer of the wall; no host, no reading, a reading written before the first read, a window the wall is not timed by, or another program leaves the notice's reset or the back-off; nothing is typed.	1,2,3,5,7,8,13	12/12
transcript-projection-refines-full-parse	equivalence	transcript::{usage,metadata,command,conversation}	Each projection P(text) = project(full parse, SHAPE_P) and Errs exactly when the full parse does (discarded fields fully validated).	1,3,5,7,12	3/3
transcript-conversation-quota	contract	transcript::conversation	conversation keeps quotaLimits {status, resetsAt, rateLimitType}, each wrong shape Null alone; other projections keep none.	1,3,12	1/1
catalog-parse-window	contract	upgrade_catalog::parse_window, Baked::{latest,model}	None iff no `models:[` marker; else the parseable models with derived efforts and native_1m; model ignores `[…]`; latest is a model id or None.	1,3,7	2/2
catalog-build-facts-roundtrip	round-trip	upgrade_catalog::{render_facts,parse_facts}	parse_facts(render_facts(b)) = b; parse_facts("{}") = default.	1,3,12	1/1
catalog-scan-chunk-invariance	equivalence	upgrade_catalog::scan_build	scan_build = (parse of the first marker window, the most frequent VERSION literal), independent of chunk boundaries.	1,5,7,8	1/1
catalog-twin-binary	contract	upgrade_catalog::twin_binary	Some(P) iff the last `exec '<P>' "$@"` line has P absolute.	1,3,7	1/1
codex-package-version	contract	codex_alab::{parse_package,package_version,package_entrypoint,alab_bytes}	parse_package ⇔ layoutVersion 1, a release version and a relative entrypoint without ..; package_version reads the nearest manifest naming exe, never executing it.	1,7,8,12	2/2
harness-contract-v3-read	contract	harness::contract::{read,holder,row_for,row_named,ClaimVersion::parse,SERVED,served_word}	SERVED = [2,3] and served_word() = "2,3". ROSTER = [grok-alab: Program::Grok, package grok-alab, vendor grok, allows relaunch,upgrade]; holder(h) = Some((row,pid)) iff h = <row id>@<canonical pid > 1>. read(h, words): no contract= ⇒ V2(words) for an unrostered h, Refused for a rostered one; with contract= each of contract/scope/version/vendor at most once and nothing else, contract = 3, scope present, every word known, unrepeated and within the row; version = MAJOR.MINOR.0[+meta] canonical (0.1.1 and pre-releases refused), vendor ≤ 64 bytes of [A-Za-z0-9.+_-] ⇒ Scoped(Claim) for a rostered h, Foreign for an unrostered one; anything else ⇒ Refused(why).	1,7,8	5/5
codex-rewrite-argv	contract	codex_alab::rewrite_argv	Ok([resume] ++ kept flags verbatim ++ [t]), the kept flags alone with no thread; Err(UnknownFlag) for the first unknown flag; Err(NotResumable) for exec/app-server/--remote/--worktree.	1,3,7	2/2
codex-is-tui-argv	contract	codex_alab::is_tui_argv	⇔ the first non-flag word is absent, resume, fork or a prompt, not app-server/exec/agents.	1,3,7	1/1
codex-thread-ids	contract	codex_rollout::{is_thread_id,is_rollout_of,thread_created_ms}, codex_alab::{lock_thread,resumed_thread}	is_thread_id ⇔ the hyphenated UUID shape; lock_thread, is_rollout_of, resumed_thread and thread_created_ms (UUIDv7 48-bit ms) as their grammars say.	1,3,7	2/2
codex-restart-record-closure	contract	relaunch::{landed,restarted,ended_by_restart,stale_exit_line}, St::resumed_pid	The record names the relaunched TUI; no Codex exit is read as the harness's own restart; outlived exits are never waited on; a stale exit's line names the wait in a person's words.	1,2,8,10	5/5
agy-exit-witness-refines-model	refinement	relaunch::{look_at_agy_exit,exit_record,after_exit}, upgrade_drive::agy::{look_at_exit,after_exit_onto}	The real agy exit read over a stand-in tab, its crash file on disk, keeps what HarnessAgyExitWitness keeps (crash, clean end, refused start, no verdict) and acts as the model: a crash of a witnessed conversation whose crash file stood is relaunched on it (at the look, else at the attempt), every graceful way out, Ctrl-Z (alive), a refused start and an unwitnessed end are left, and nothing is relaunched without the conversation its process held.	1,2,8,10	1/1
muse-quiet-upgrade-refines-model	refinement	upgrade_muse_live::{order,moves_onto,build_version,part_start,open_work,quiet_log,gate}, upgrade_drive::muse::live::{behind,log_look,screen_idle,look_of,look_at,claim_record,upgrade_visit,upgrade_due_with}, relaunch::{ended_by_restart,muse_landed}	Muse Code's live upgrade restarts only where HarnessMuseQuietUpgrade's Restart is enabled: a newer build by atpkg's revision-aware order of the store records, this process's part of session.jsonl (every record after its route_facts in the file, whatever their stamps) with every run terminal, every accepted/started task ended (completed, cancelled, failed, rejected) but a session_name.allocate reservation, and every queued inbox item drained, the same of each subagent log from the part's start, a log unreadable or unproved failing closed, the reader Idle authoritative with an empty composer, nobody holding the tab, no descendant process at all; the claim (in flight, the upgrade's cause) is on disk before the SIGTERM, so ended_by_restart reads its 143 as the harness's own restart, relaunched through the twin and adopted by the session's lock, nothing typed after.	1,2,8,10	1/1
muse-exit-witness-refines-model	refinement	relaunch::{look_at_muse_exit,exit_record,after_exit}, upgrade_drive::muse::{look_at_exit,after_exit_onto}	The real Muse Code exit read over a stand-in tab keeps what HarnessMuseExitWitness keeps (crash, clean end, refused start, no word) and acts as the model: a crash of a witnessed session is relaunched on it (its block's word, else its session log's), its own exit, someone's SIGINT/SIGTERM and a told refusal are left, and nothing is relaunched without the session its process held.	1,2,8,10	1/1
codex-prune-refines-model	refinement	upgrade_drive::codex_alab_drive::prune, upgrade_drive::{registry_life,prune_states}	prune removes exactly HarnessUpgradeStateCollection's collected records (closed tabs' settled records ≥ 1 week old), never a live tab's or an in-flight restart's; the registry vouches only for a root of its own kind.	1,7,8,10	2/2
screen-reply-parse	contract	upgrade_drive::{parse_screen,is_generation}	None iff not JSON or no `rows` array; rows as strings; first/seq default 0; cursor relative to first (None above it); generation only when ScreenGen parses; human Unknown/Never/Ago(n).	1,3,5,12	1/1
busy-footer-live-zone	contract	upgrade_drive::busy_footer, aterm_phase::busy_zone_start	busy_footer ⇔ `esc to interrupt` occurs in rows[busy_zone_start..], below the newest composer frame; a dead or quoted footer above never counts.	1,3,7,12	2/2
held-by-status	contract	upgrade_drive::{held_by_status,our_hand}, relaunch::{hand_on_tab,driver_hand,held_by_someone,person_present}	held ⇔ not OK ∨ hold=1 ∨ a hand other than ours ∨ unread input ∨ human_ms < grace·1000; grace 0 holds nobody by keystroke; our lease holds nothing.	1,3,5,7	2/2
host-roster-parse	contract	upgrade_drive::{parse_host_roster,roster_rows,unique_tab_for_group}	None unless OK with every line `<u64> s-<id>` and distinct ids; fgpgid positive or None; unique_tab_for_group = the one tab with that group, else None.	1,3,5,7	1/1
roster-aside-union	contract	upgrade_drive::{roster_aside,aside_of,is_host_holder}	roster_aside = {claimed by another supervisor, autopilot off} from the real columns (decoded), None on ERR or no answer; aside_of = union of Some answers.	1,5,7,8,13	2/2
codex-due-short-circuit	contract	upgrade_drive::{due_after_lanes,lane_due}	The first lane past Claude Code's, in upgrade::Agent::others() order, answering Some(d) ⇒ d with no Claude scan (Muse Code's for a tab its TUI leads: due while its build is behind the twin's); None from every one (agy, with no upgrade lane, always None) ⇒ one full Claude roster scan.	4,8	1/1
due-never-drops-unread	refinement	upgrade_drive::{due_among,no_record,due,Targets::read_with,cached_version_with}	Each due_among verdict is an action HarnessUpgradeLook enables (NeverDropped): No only when every read answered; timeouts and foreign-group records are Unread; a missing record waits RECORD_WINDOW_S; an own restart in flight is Yes.	1,4,5,8,10,12,13	6/6
version-cache-backoff	contract	upgrade_drive::cached_version_with	Clean answers are cached per file key with no probe; a timeout reads Unread and is retried at t0+VERSION_RETRY_FIRST·2^k, never before; a changed key is probed at once.	1,2,5,8,13	1/1
bounded-child-probe	contract	upgrade_drive::version_within, upgrade_build_start::{probe_within,probe}	Returns within the bound plus a constant; a hung child or held pipe is TimedOut with its group killed; probe runs `--help` and starts ⇔ exit 0.	8,13,17	2/2
state-record-roundtrip	round-trip	upgrade_drive::St::{to_json,from_json}	from_json(to_json(st)) = st for Claude/Codex records in every phase; an unknown phase or agent word is None (muse's and agy's records are their relaunch lanes', their `muse_*` and `agy_*` keys written only when set; a record naming the retired Gemini CLI, `gemini`, is no record); omitted fields read their defaults (relaunched_at from the phase's at). Files from older writers, or with fields this build ignores, parse with defaults.	1,3,5,6,7	4/4
background-walk	contract	upgrade_drive::{background,background_procs,background_or_unreadable}, upgrade::gate_restart	background_procs = BACKGROUND-named proper descendants (bounded walk), except the agent's own keep-awake child (its descendants still walked); an empty table reads UNREADABLE_TABLE; any background holds the gate.	1,3,4,5,7	3/3
etime-and-command-head	contract	upgrade_drive::{parse_etime,command_head}	parse_etime = [d-][h:]m:s in seconds, None for 1 or >3 fields or non-numbers; command_head unwraps `&& eval '…' < /dev/null` and the quote escapes.	1,3,5,7	1/1
ancestry-guard	contract	upgrade_drive::is_our_ancestor	⇔ pid is in the first 64 links of our ppid chain; no sweep signals itself or an ancestor.	1,3,7,8	1/1
terminal-owner	contract	upgrade_drive::{terminal_owner,owned_by_aterm,owner_word}	terminal_owner = the first ancestor (≤64 steps) on another tty; owned_by_aterm ⇔ pid 1 or an aterm name; owner_word sanitised to ≤32 [A-Za-z0-9._-] or ?.	1,3,5,7	1/1
job-classification	contract	upgrade_drive::{job,foreground_shell,job_of}	job = NoJobControl / Background / Foreground by the group ids; foreground_shell maps them to Ok(shell) or the named Err.	3,8	2/2
relaunch-by-parent	contract	relaunch::relaunch_by_parent, Live::parent	shell 0 ⇒ NotOurs; parent = shell ⇒ Ours; another ⇒ NotOurs; unread ⇒ Unread.	3,8	1/1
session-roster-integrity	contract	upgrade_drive::{session_files,require_unique_owner,unique_live_owner}, Homes::roster	None iff a live pid's record is unreadable, torn or misfiled; dead and non-numeric files ignored; require_unique_owner refuses a None roster or a duplicate holder; any home None makes the roster None.	1,7,8	3/3
session-roster-cache	contract	upgrade_drive::SessionRosterCache::files_at	Rescans iff nothing cached, no stamp, a changed stamp or the rescan interval passed; failed scans never cached.	1,2,5	2/2
host-candidates-read-order	contract	upgrade_drive::{host_candidates,needs_upgrade_inspection}	Reads in a fixed cheapest-first order (no argv/ps for records not needing inspection, argv only for a group naming a tab, ids after); Ok claims only for the env's tab in its foreground group; one roster scan per sweep.	1,3,4,5,7,13	3/3
composer-empty-caret-dim	contract	upgrade_drive::{cell_is_dim,composer_empty,continuation_composer_empty}, upgrade::{composer_is_empty,composer_dim_row}	cell_is_dim ⇔ a well-formed cell reply (aterm_codec::cell::parse_style, which refuses a trailing frame=open) whose attrs contain dim; composer_empty asks one cell on the caret row only when dim can decide; the continuation poll trusts a tail draft, confirms a blank tail with the full screen, and falls back on ERR.	1,2,3,5,8	6/6
tail-window-bounded	contract	upgrade_drive::{tail_window,tail_to_end,since,named_standing_wall,named_reset_line}, upgrade::transcript_has_ready	tail_window reads at most the budget ending at len (an append after len is never read); lossy decoding never invents a READY; the wall/reset readers read only the named transcript's bounded tail.	1,3,5,7,8,11	4/4
step-word-classification	table-property	upgrade_drive::{Report::is_act,Report::line,after,unknown_step,moved,typed}	is_act ⇔ not current/wait:/skip:/busy:/would-; after maps each taught family to the host's next move; untaught words are looked at again; moved/typed per the step.	1,3,5	4/4
turn-end-ownership	contract	upgrade_drive::{owns_turn_ends,owns_turn_ends_as,bounded_wait_as,release_settles,log_settles,after_attended}	Owns turn ends for announced:*, a READY settling/draining within its bound, an owed release whose gate only settles (release_settles) within the settle's bound, Muse Code's quiet restart waiting on its log to catch up with its idle screen (log_settles: wait:turn-open, wait:queued-work) within the settle's bound, and wait:attended within grace + ATTENDED_OWNED_S; never for holds, releases held by anything else, Muse's other gate waits, or final words; the quiet flow owns nothing for its own work.	1,3,5	5/5
release-is-next	contract	upgrade_drive::{release_is_next,owed_word}	release_is_next for the upgrade's own give-up/void/stop and gate-free waits, false for every gate wait; owed_word turns an owed final word into wait:release:<why>.	1,3,5	1/1
pick-tab-word	contract	upgrade_drive::pick	The tab's own report over another tab's (a current tab defers to the other's word); else the first act, else the first wait.	1,3,7	1/1
announce-preflight	contract	upgrade_drive::announce, relaunch::line_for	A refused plan types no notice, fails the round once (refused:<why>); a wait plan waits; dry runs change nothing; an Ok plan types the notice once and records the marker.	1,2,4,8	1/1
drain-void	contract	upgrade_drive::{drain_expired,answered}, upgrade::{next_step,gate_release}	A readiness answer a box holds past DRAIN_S is voided: notice references forgotten, release owed (Claude only), drain-expired:<why> ledgered once; the next ask waits REASK_S and the box's going.	1,2,4,8	1/1
sweep-lock-exclusive	contract	upgrade_drive::{sweep_lock,SweepLock::drop,sweep}	While held a second sweep is busy:another-sweep (not an act); dropping releases it even with descriptor copies alive.	8,13	2/2
no-act-without-ownership-proofs	invariant	upgrade_drive::{visit,visit_with_claim,announce,process_in_tab}, relaunch::{carry_on_with_tab_probe,type_relaunch_line}	Every act request is preceded, after the last roster read, by: a complete roster in the agent's home naming the holder; Foreground at every job read; a terminal aterm owns; the tab claim re-checked at the turn; an unheld status. Any failure types and signals nothing and says its wait or refusal word.	1,2,4,7,8,13	9/9
last-look-before-signal	invariant	upgrade_drive::{restart,terminate,status_lets_signal,turn_over_shown}	The SIGTERM is sent only after a last look on the same connection re-reads screen, status, job, identity and background unchanged and idle; any change restores the state, gives the hand back and sends nothing.	1,2,4,8,13	5/5
hand-lease-bracket	invariant	upgrade_drive::{Hand::take,keep,give_back,type_fenced}, relaunch::{type_relaunch_line,carry_on_with_tab_probe}	Every typed act sits inside `lease acquire … hard` … `lease release`; a passed-in hand is not re-acquired or released; keep renews only a held hand after HAND_RENEW; a refused acquire is wait:held with nothing typed.	1,2,8,13	5/5
typed-turn-fence	contract	upgrade_drive::{turn_fenced,claude_fence,typing_fence,RowsFence::hash_rows}, upgrade::turn_wait	Turns carry if-rows of the input box where the host takes it, else if-gen of the last read, else nothing; the last request before the turn is that read; changed skips are retried up to NOTICE_FENCE_TRIES; a typed turn returns once its submit is taken.	1,4,8,12	7/7
pressed-text-delivery	contract	upgrade_drive::{turn_fenced,turn_verdict}, upgrade::turn_wait	The notice and release are pressed whole with a guarded submit where the host takes cap= and guarded:, other lines pasted; timeouts sized to keys, with a hang detector for each echo; a survey refuses the notice; skipped typed=0 is a wait, a cut is never announced or retyped.	1,3,4,8	7/7
left-typed-never-erased	invariant	upgrade_drive::{left_standing,St::left_typed}	Nothing is pressed to take aterm's words back; the part left is recorded and read first by every later look (unsent / mixed / gone).	1,2,8	2/2
relaunch-prompt-read	refinement	relaunch::{type_relaunch_line,prepare_prompt}, upgrade_drive::PromptMark	type_relaunch_line types iff relaunch_prompt_read_model's Read is enabled; text after the mark (wrapped, homed, typeahead, right prompts) is typing; columns (the cursor's, cmdcol, a right prompt's start) are grid cells, a row's text read at them by grid column, so a wide prompt glyph is two and a zero-width character in a blank's cell leaves it blank (East Asian Ambiguous glyphs count one cell, the terminal's default policy: `text --json` does not say the session's); a moved prompt waits; a person's keys dated against the agent's end only when the restart ended it; yield timeouts wait.	1,2,4,7,8,10,12	11/11
relaunch-line-dialect-and-rekey	contract	relaunch::{type_relaunch_line,shell_dialect,with_rekey}	Bash gets `send` + `key enter`, others a fenced turn; a known dialect asks `rekey` and types the key before the line (refused or over-bound ⇒ the plain line); an attempt that types nothing withdraws the rekey.	1,4,8	3/3
relaunch-line-construction	contract	relaunch::{line_for,with_model,plan}, upgrade::launch_model, models::model_due, tap::gate	A fresh line is the launch flags without --resume; a resume line adds the pair last; with_model replaces every --model spelling before --resume; a failed model is dropped; the home and login prefix the program; --settings <tap> only where the tap gate holds; the survey export is dropped before refusing for length, a store binary's recorded one too; a store binary's line also exports its shim's recorded env (relaunch-line-shim-env).	1,3,4,5,7,8	7/7
relaunch-line-shim-env	contract	upgrade::{relaunch_line_env,relaunch_line_env_clear,task_line_env_clear,with_shim_env,with_store_env}, atpkg::shim_env::{of_store_binary,turns_updater_off}	Every line typed for an exe inside an atpkg store build (store/<program>/<build>/, or its laid-aside <build>.noindex) exports, after the line's own env, each pair of that build's <build>.shim-env record the line neither sets nor removes, quoted so no shell runs it (DISABLE_AUTOUPDATER=1 for Claude Code, the one program whose typed lines name a store build today: agy's and Muse Code's type the managed twin, whose shim exports theirs); a line too long with the whole record is made with only the record's pairs that turn the program's own updater off (the survey switch drops, DISABLE_AUTOUPDATER=1 never does, so a line that fits only without it is refused); a build with no record, a path outside any store, a relative path or one with a `..` step adds nothing, so its line is the line it was before.	1,3,5,7	2/2
notice-owner-refinement	refinement	upgrade_drive::{St::notice_belongs_to,St::notice_gone,visit_with_claim,heard}	The visit terminates only where harness_upgrade_notice_owner_model enables Terminate (unique live owner, the notice's own process); a gone notice resets the round for the new holder (keeping Skip/Defer) and an old READY never ends it.	1,2,4,8,10,12	4/4
retarget-arms	contract	upgrade_drive::{St::retarget,St::for_target,retarget_how}	InPlace iff pending/announced for this process with the same promised model (phase, marker and READY kept); otherwise Anew; age, word (not a Skip), streak and Codex stamps carried while nothing began; ledgered once.	1,3,5,6	8/8
age-is-one-process	refinement	upgrade_drive::{St::for_target,mint_behind,note_behind,forget_settled_pending,St::note_held,St::idle_looks}, upgrade_status::rows	Refines harness_upgrade_age_model (AgeIsThisProcess): an age is carried only while the same process stays behind; mint_behind mints at attach clamped to now; idle runs and holders are keyed to their process.	1,2,5,10,12	8/8
owner-word-survives-restart	contract	upgrade_drive::St::{kept_by_restart,word_onto,holds_off,request_for}	A restart keeps the owner's Skip/Defer on its tab while it names a movable build and the clock of a record about the kept build; holds_off is the word or a resting failed start; --now waives nothing in another tab.	1,2,3,5	5/5
forget-settled-pending	contract	upgrade_drive::forget_settled_pending	Removes unwanted same-build pending restarts and pending records already reached (unless a word in force); never announced, stopped, owed, newer-build or dry-run records.	1,8	2/2
failed-start-gate	contract	upgrade_build_start::{note_failure,starts}, upgrade_drive::{build_starts,save_at}, St::{keep_stop,failed_start,reopen_landed,roll_back}	A no-resume relaunch onto b notes one failure per stop; starts(b) needs a probe passing since the latest note (rate-limited, blind after a later failure except under --now); no agent moves onto a failing build; the stop rides restarts and is withdrawn when the relaunch came up; a Claude Code moved onto b whose relaunch ends unregistered is relaunched once on the line back to the image it ran, its failure kept.	1,2,4,8	9/9
release-owed-is-paid-once	invariant	upgrade_drive::St::owe_release, upgrade::{release_is_next,build_hold,gate_release,retry_due}	A round whose notice reached an agent and ends without restarting it types exactly one release to that process as soon as nothing else is next; a release owed to another process is dropped; dry runs write nothing.	1,2,4,5,8,13	8/8
stop-rest-ladder	contract	upgrade::{rest_extension,unanswered_rest_extension,retry_due}, St::{fail,rearm,stop,void,work_seen}, Facts::work_ended	rest = RETRY_S·2^min(k-1,MAX_SHIFT) for a repeating stop (GAVE_UP: ×{1,2,4,12}, up to a day); a different reason restarts k; the rest always ends (rearmed); the work's end re-arms and asks within the floor and daily budget, bounding notices a day. Every stop stamps its rest; rearm yields Pending with a fresh salt above every old marker, carrying an owed release; a void restarts the rest on the same ladder; notice-to-notice silence ≤ REASK_S + rest + DRAIN_S + rest.	1,2,3,5,7	9/9
in-flight-orphan-pass	contract	upgrade_drive::{orphans,sweep,St::exited_at,relaunched_over}, upgrade_wake::wait_exit	A record whose conversation is held live is left to visit; otherwise exited_at is stamped once and the stop decided before any dial (resume / stale-exit / shell-gone / no-resume); a hung exit is noted once and nothing forced.	1,2,8,13	12/12
relaunch-adoption	contract	upgrade_drive::{relaunch_of,visit,St::landed_after_stop}, relaunch::{await_new,resume}	relaunch_of = Ours iff the holder is our shell's child of the conversation (Unread if unreadable); another shell fails the round once; a no-resume stop is reopened only for the recorded shell's own relaunch; adoption is said once.	1,2,4,8,11	8/8
relaunch-ended-verdict	contract	relaunch::{await_new,ShellHolds}, Kernel::shell_holds_tab, St::line_block	A relaunch (not CAUSE_EXIT) is stopped failed:no-resume at once iff its line is proven ended (its block over, or the agent seen taking and giving back the terminal); otherwise it waits; shell_holds_tab reads leadership and foreground.	1,8,13	8/8
exit-record-refinement	refinement	relaunch::{look_at_exit,exit_record,after_exit,after_host_ended}	Refines HarnessExitRecord: the record is read at the exit look and kept; only a crash is relaunched; graceful exits, one-shots and conversation-less launches are left; the harness's own restarts are never graceful; a host end relaunches in the reopened tab.	1,8,10,12	5/5
carry-restart-outcome	contract	upgrade_drive::{restarted,carry_restart,outcome}, relaunch::account_restart	restarted finds exactly the record of a restart that ended that process in that tab; carry_restart goes on within STALE_S, then told:/refused: words; a landed move answers landed silently; account_restart names its label.	1,4,8	4/4
restart-carries-login-and-tap	contract	relaunch::{restart_from,restart_as,at_rest_for}, St::login, tap::Armed::lone_login, TurnEndReading::sign_in_menu_drawn	A restart in place uses the same program and conversation, refuses one-shots and carries an in-flight restart; every line re-applies exactly the process's login (set, empty or unset); an account move types the roster dir, and is made over a sign-in menu drawn in the composer's place (nothing typed stands there to lose), where every other restart waits for an empty composer (`wait:changed`); the tap rides under its switch without writing settings.	1,4,8	7/7
model-way-back	contract	relaunch::{model_restart,fallback_origin,exit_fallback,way_back,chose_by_hand}	A restart asks for the hand choice since start, else the launch --model, else none; a fallback records its origin (kept by a second fallback, carried by every record); the way back prefers a newer hand choice, then the origin, then the alias, and spends it.	1,4,5	13/13
model-ask-ledger	contract	upgrade_models::ModelRecord::take_back, relaunch::confirm, upgrade_drive::model_ride	An ask is recorded only with its typed line and taken back otherwise; an applied ask is never failed later; the due clock survives unknown reads; a due model rides every restart; the model before/after are read from the readiness answer and the first answer past the mark.	1,2,4,8	14/14
model-evidence-scope	contract	upgrade_drive::{model_evidence,ModelCtx::at,ModelCtx::read,agent_settings,model_read_for}, models::{availability,recommendations,ingest,offered}	The model rule's evidence is read only where the agent's Claude Code reads it: the settings chain (highest document per key, unparsable ⇒ unknown), the agent's own home and profile, --settings only for the same kernel start, nothing under several logins, the native build's catalog for native sessions; announcements grow only their home's list.	1,4,5,8,12	12/12
claude-home-resolution	contract	upgrade_drive::{Records::*,home_dir_at,Homes::read,home_carried,conversation_under,process_transcript_tail,instance_tabs}, footer::claude_dir_of	Every reader of an agent reads under Dir(agent) (relative homes resolved where the agent started; the tail where it writes now); St::home records the directory; a line that would move a relative home is refused; homes are learned once, never the user's own by mistake; split homes wait.	1,4,8,12	26/26
task-detection-and-fresh-restart	contract	upgrade_drive::{Tasked,Tasked::of_snapshot,visit}, relaunch::restart	Tasked is Unknown/No/Yes by the transcript (the harness's and its supervisor's own rows are no task), read incrementally; No ⇒ a fresh restart that types nothing; Yes ⇒ announce or quiet resume; a vanished transcript restarts afresh.	1,2,7,8	8/8
quiet-flow	contract	upgrade_drive::visit, upgrade::Facts::quiet, relaunch::carried_quietly, upgrade_drive::status_lets_signal	Under upgrade_ask=false a tasked idle conversation is restarted with nothing typed (after the waiting record has stood a look) and carried on with nothing; a lagging status holds it except under --now with status_lets_signal.	1,4,8	3/3
continuation-words-and-mode	contract	relaunch::{resumed_prompt,carry_on_from}, upgrade_drive::plan	The continuation's words follow the cause; the line takes the pill's mode, else the transcript's, else the launch's; the notice is typed only once a line is planned.	1,4	3/3
presence-settle-ladder-gates	contract	upgrade_drive::{visit,look,ladder_look}, upgrade::{attended_by,next_step}	An attended tab (human_ms < 10 min, no stamp fails closed) is said once and never typed into or signalled without --now; the settle is the server's idle verdict age; the ladder's rungs admit as stated; a lagging status waits one look; a break types the notice once.	1,4,5,8	7/7
ready-consent	contract	upgrade_drive::{answered,heard,visit}	An ordinary readiness answer counts only after the matching current notice in the current round; legacy marker replies remain readable; a hold ends the notice; an unrecorded answer holds the point; a person's later message supersedes it and the point asks again.	1,8	2/2
wait-log-ledger-and-prune-bounds	contract	upgrade_drive::{St::note_wait,ledger_append,prune_states,Report}, upgrade::Agent::parse	The wait log gains a row only on a change or an act, bounded to WAITS_KEPT; the ledger is cut past 1 MiB with the newest row last; prune_states removes only week-old settled records of dead conversations; an unknown agent's record is left byte-identical.	1,7,8	6/6
ladder-refinement	refinement	upgrade::{rung,gate}, upgrade_drive::{ladder_look,look_facts,ladder_projection,St::still}	Refines HarnessUpgradeLadder: Move exactly where Claude Code's quiet flow restarts, over every state the model reaches, each Buggy member caught alone; the still run counts looks at the same words QUIET_S apart.	1,7,10,12	2/2
login-wall-reader	contract	upgrade::{login_wall,transcript_login_state}, upgrade_drive::notice_answered_by_wall	Walled from an authentication_failed row until `Login successful` (either form) or an answered turn; other synthetic rows and subagent rows never set or lift it; a notice reached the model iff its answer is not the wall's.	1,3,5,7	3/3
login-wall-spends-nothing	contract	upgrade::{next_step,rearmed}, St::notices, upgrade_drive::visit	At the login wall every gate waits `login` and no clock runs; a notice the wall answered spent no ask and is retyped as the same ask; rearmed recovers 0.93.0 records; St::notices counts each typed ask once.	1,2,5,8	8/8
model-family-version	contract	upgrade_models::{family_version,base_id}, upgrade::is_model_id	Some((f,(a,b))) iff base_id(id) = claude-<f>-<a>[-<b>] plus date parts of ≥ 6 chars, f a lowercase word, 1 or 2 short numeric parts (b=0 when absent); else None.	1,3,7	1/1
priority-list-roundtrip	round-trip	upgrade_models::Priority::{parse,render,seed,rank}	parse(render(p)) = p for well-formed lists (1..=32 distinct family_version ids, history ≤ 64); parse refuses any other text; rank ignores [1m].	1,3,5,7	1/1
priority-admit-monotone	contract	upgrade_models::{Priority::admit,ingest,recommendations}	admit inserts a known, unlisted id above its family's best only when newer than every listed member of a listed family; never older, never a new family; ingest is idempotent.	1,2,3,5,7,13	2/2
model-availability-decision	contract	upgrade_models::{availability,target,parse_served}, upgrade_catalog::version_triple	availability is the first matching clause (a family billed to usage credits under `usage_credits` off (credits-billed-model-target), denied, disabled, unknown to the build, served row missing/disabled/deprecated/too new, else Yes by the served or baked catalog, else No); target = the first Yes in list order.	1,3,7	1/2
credits-billed-model-target	contract	upgrade_models::{credits_billed,fable_billed,billed_to_credits,availability,CREDITS_BILLED_UNREAD}, upgrade_drive::{credits_billed_for,model_evidence}, profile::{parse,Selection,Context,effective_for_billing,Profile::consented,Profile::credits_only}, relaunch::{moves_onto_credits,restart_as}	fable_billed(f, now) iff f = CreditsOnly, LimitsEnd(None), Unknown, or LimitsEnd(Some(at)) with now ≥ at (Included: never); credits_billed(profile, now) = ["fable" if fable_billed] ++ profile.consented (fableOverageConsentV2 any id true → fable; usageCreditsOnlyModelConsent families whose any id is true) ++ profile.credits_only (tengu_calm_seal), each a lowercased letters-only family word, deduplicated, no id ever kept; billed_to_credits(m, b) iff the family of m's base id (an alias is its own family) is in b. Under `[harness] usage_credits = false`, credits_billed_for = Some(credits_billed of the profile selected by the process's coherent captured plain-profile path, record directory and filename context) only where that selection and profile read are known; unknown context, selection metadata, legacy-vs-diskless ambiguity, several logins or unread profile = None. The final per-process offer is rebuilt even for a cached default-home view and is empty on None; Restart::Model and Restart::ModelBack also refuse unknown billing or a billed target with refused:model-on-credits (the cost classification of other restart reasons is unchanged). A preliminary home report/pre-filter retains CREDITS_BILLED_UNREAD = ["fable"], which grants no final move. Known billed families still make availability(m) = No("billed to usage credits (usage_credits off)"), so offered and family_successor exclude them. The live Snapshot retains its coherent captured profile Context; restored snapshots explicitly carry None/unknown, with no layout wire change. Under the key on credits_billed_for = Some([]). Under key-off, a new model launch also requires a named target and the same resolved Standard context after required home/login overrides; its quoted line explicitly sets/removes HOME/config/login and sets an empty custom OAuth selector, never inferring the shell's current exports from initial ProcArgs. Custom OAuth or unknown context refuses; explicit usage_credits opt-in preserves the old launch environment behavior.	1,3,7,8	3/3
claude-json-model-facts-only	contract	upgrade_models::parse_claude_json	Only denied apiNames, disabled option ids and bounded model announcements reach the result; non-JSON gives empty lists.	1,3,7	1/1
live-model-reader	contract	upgrade_models::{live_model,model_result_of,model_command_display,quoted_display,strip_sgr,model_of_display}	A later Claude-own model result wins (by_command; None when the build does not know its display), else the last answer's model; quoted phrases are never a result; strip_sgr removes exactly SGR sequences.	1,3,7	2/2
family-successor-newest-strictly-newer	contract	upgrade_models::family_successor	The newest offered same-family base strictly newer than current (keeping current's [1m]); None across families or when nothing is newer.	1,3,7	1/1
offered-set-and-list-target	contract	upgrade_models::{offered,list_target,allowed_by}	offered = dedup of baked latest ++ list ids that parse, are available and allowed; list_target = the first list id offered; an empty allowlist offers nothing.	1,3,7	3/3
model-due-rule	contract	upgrade_models::{model_due,spent,chosen_by_hand}	The documented decision list; SAFETY: Due{from,to} is a newer same-family move, or a better-ranked other-family move nobody chose by hand with no same-family successor, and never to an applied or failed id.	1,3,5	3/3
move-why-from-ids	contract	upgrade_models::{move_why,MoveWhy::words}	Family when both ids parse with equal families, List when they differ, Unknown otherwise.	1,3	1/1
model-record-roundtrip	round-trip	upgrade_models::ModelRecord::{parse,render}	parse(render(r)) = r for model-id fields; non-ids are dropped and absent fields default (no due clock = nothing due).	1,3,7	2/2
settle-ask-takeback	contract	upgrade_models::ModelRecord::{settle,asked,take_back,due_clock}	settle marks an ask applied when it runs and failed only after MODEL_SETTLE_S if never applied (applied never becomes failed); asked;take_back is the identity on an untouched record; due_clock restarts only on a new target.	1,2,3	2/2
ride-bar	contract	upgrade_models::{ride_bar,spent,chosen_by_hand}	A carried model is barred when spent, under an alias launch, unreadable, not newer in-family, or (other family) unranked, ranked no better, or chosen by hand.	1,3	1/1
model-read-step-reads-tail-iff-pending	contract	upgrade_models::{model_read_step,live_model_at,last_answer_at}	With nothing offered, nothing set and nothing carried the tail is never read and the verdict is no-model-available; otherwise the tail is read once and the result is the composition of live_model_at, settle, model_due, ride_bar and due_clock.	1,2,3,4,11	1/1
model-ladder-monotone-bounded	contract	upgrade_models::{model_moves_now,model_to}	model_moves_now = cache-cold | build-restart | warm-wait-over (d ≥ MODEL_WARM_MAX_S) | None: monotone in d and Some at the bound; model_to = announced, else Due's target when the ladder moves.	0	1/1
utc-and-lstart-parse	contract	upgrade_models::{parse_utc,parse_lstart,last_answer_at}, rfc3339::parse_utc_fractional	parse_utc accepts only Z-zoned RFC 3339 at or after the epoch; parse_lstart rebuilds the stamp from the five ps tokens; last_answer_at = the last stamped main-chain answer.	1,3,7,12	1/1
hand-chosen-model	contract	upgrade_models::{hand_chosen,launch_names,hand_model,newest_row_at}	A hand choice is a /model result not naming the launch's model (aliases resolved), or a recorded human id for the running model; none for a process whose rows all predate it.	1,3,7	3/3
live-model-at-launch-until-answer	contract	upgrade_models::live_model_at	A real launch id is live until the process writes its first row; else live_model.	1,3	1/1
user-model-settings-parse	contract	upgrade_models::{parse_user_settings,model_settings_of}	default = a model-id `model`; allowed = None if absent, else the model ids of availableModels (Some([]) when not an array); unparsable text is default.	1,3,7	1/1
version-order	contract	upgrade::Version::{parse,cmp,eq,fmt}	parse accepts dotted digit components (≤ 12 digits, optional leading v); order and equality pad with zeros; Display joins the components.	1,3,7	1/1
session-file-parse	contract	upgrade::{parse_session_file,is_session_id}	Ok iff a JSON object with every required key of the right type; Err naming the first missing key.	1,3	1/1
choose-target-newest-managed-tie	contract	upgrade::choose_target	The newest candidate strictly newer than running, managed on a tie; None otherwise.	1,3,7	1/1
rewrite-argv-refines-commander	equivalence	upgrade::{rewrite_argv,parsed,carried,flag,tap_tokens}	Against a Clean model of Commander's parseOptions: unknown options and Refuse-fate flags are refused; an Ok rewrite parses cleanly with the same Keep events and exactly one dropped --resume; only aterm's own tap --settings is shed.	1,3,5,7,14	4/4
launch-model-and-resume-by-the-rewrites-parse	contract	upgrade::{launch_model,launch_resume,parsed}	launch_model = the last --model event; launch_resume = Some(Some(id)) for a named resume without --fork-session, Some(None) for --continue/bare resume/fork, None otherwise.	1,3,7	2/2
relaunch-line-shape-quote-bound	contract	upgrade::{relaunch_line,relaunch_line_env,relaunch_line_on,quote,with_rekey,with_rekey_on}, LineDiscipline	The line is heal ++ `( cd -- dir && exec words )` with each word quoted so the dialect reads it back, Err past tty.max_line(); with_rekey puts the quoted key path first when it fits, never its value.	1,3,5,7,12	3/3
running-clause-held-list	contract	upgrade::{running_clause,held_list,with_running,one_line,command_words,path_middle}	The clause lists processes oldest first, at most HELD_NAMED named, one line, commands ≤ HELD_COMMAND_CHARS with long paths elided before cutting; placed before NOTICE_CLOSE.	1,3,7	3/3
ready-answer-reader	contract	upgrade::{ready_marker,transcript_has_ready,is_ready_line,says_ready,ready_superseded,directed_since_ready,turn_of,is_direction,is_announcement,typed_words}	Readiness is an unconditional ordinary-language answer at the own model's turn end after the matching notice, or a legacy marker reply; no later announcement, direction or agent row may supersede it; references differ by salt; directions are someone else's messages, not meta rows or the harness's lines.	1,3,5,7	8/8
composer-empty	contract	upgrade::{composer_is_empty,composer_dim_row}	Empty ⇔ the composer row is blank after the glyph or exactly a dim placeholder run; anything else is a draft.	1,3,7	1/1
reducer-refines-drain-bound-model	refinement	upgrade::{next_step,gate_restart,gate_announce,person_void,void_of}	At every reachable state of HarnessUpgradeDrainBound the enabled action set is exactly {kind(next_step)}; Buggy/LagEnds/LagHolds disagree.	1,3,5,10	1/1
reducer-refines-work-end-model	refinement	upgrade::{next_step,requested_step}	At every idle point of HarnessUpgradeWorkEnd the reducer re-asks or re-arms at the work's end once the gate is open but for READY, and waits while it settles; defect knobs disagree.	1,3,5,10	2/2
quiet-step-contract	contract	upgrade::{quiet_step,gate_restart,status_lags}	quiet_step never announces; Terminate only at a genuinely idle point (no work, box, draft, person, limit; untasked only after a look); old announced rounds give up after REASK_S; a held READY voids past DRAIN_S.	1,3	4/4
next-step-safety	invariant	upgrade::{next_step,requested_step,break_step,gate_*,person_hold,build_hold,retry_due,clock_held}, upgrade_drive::rearm_held	Terminate only with READY and nothing running, held, limited, attended (bar --now) or unsettled; a break takes every step but the end; a limit waits with its clock held; only a box or draft standing HOLD_S is a person's hold; announce/give-up/void only through their gates; a Failed round re-arms after its rest unless the owner holds it; --now waives only the settle and attendance.	1,3,5	24/24
upgrade-clocks	contract	upgrade::{hold_since,ready_since,attended_by,rung,rung_in_force,person_grace,still_print,clock_held,clock_held_until}	hold_since continues a hold only while sampled within HOLD_GAP_S; attended_by uses the per-session stamp (true without one); rung is monotone in time behind; still_print depends only on the words above the composer; a limit episode moves the round's window forward.	1,3,7	5/5
model-outcome-reader	contract	upgrade::{transcript_model,transcript_first_model,assistant_models,restart_outcome,restart_outcome_listed,is_model_id}	transcript_model = the last real-id answer; transcript_first_model = the first of the new version; the outcome names the model after, says a change by base, and blames a kept --model flag.	1,3,7	6/6
harness-prompts-shape	contract	upgrade::{prepare_prompt*,continue_prompt*,release_prompt,is_announcement}	Every harness prompt is one line; the notice opens with ANNOUNCE_HEAD, carries the marker, ends with NOTICE_CLOSE and names its model iff given; continuations and releases are never announcements and carry no marker.	1,3	3/3
owner-words-roundtrip	round-trip	upgrade::{Request::{parse,shown},Agent::{parse,word,program},wait_word,span}	Request and Agent words round-trip and unknown words are nothing; deferrals are shown as span; span uses the two largest units.	1,3	5/5
transcript-tasked	contract	upgrade::{transcript_tasked,TaskScan,prompt_of,same_words,command_line}	tasked ⇔ a main-chain user prompt that is someone else's (not the harness's marked lines, not the supervisor's ledgered words within TYPED_WITHIN_MS, not a turnless command); monotone under appends.	1,3,7	2/2
screen-limit-recogniser	contract	upgrade::limited	limited ⇔ the supervisor's usage-limit recogniser matches the live banner (never history or a tool's copy).	1,3,12	1/1
notice-fate-and-queue	contract	upgrade::{notice_fate_of,notice_scan,Scan::rests_until,queued_copies,queued_until,transcript_limit_until,transcript_limit_reset,queue_rest}	A notice's fate is Answered / Wall / Queued→Taken(t) / Open by what answered it; the standing limit lasts to its reset (or REASK_S); queued copies count notices since the model's last row; queue_rest doubles from RETRY_S to a day without overflow.	1,3,7	4/4
standing-quota-wall	contract	upgrade::{transcript_standing_wall,standing_limit,is_limit_type}, usage::notice_window	Some iff the standing limit row is a quota rejection; limit_type from rateLimitType or the text's window; wrong-shaped keys cost only themselves.	1,3,7,11	2/2
reset-dialog-line	contract	upgrade::{transcript_reset_line,local_command_text}	The last /rate-limit-options result text since the claim, cut to RESET_LINE_MAX.	1,3,7	1/1
upgrade-refines-never-strands-model	refinement	upgrade::next_step, upgrade_drive::St::{signalled,signal_failed,stop,dropped}, upgrade_drive::after	The real reducer and record transitions take HarnessUpgradeNeverStrands's steps; the incident schedules stay within NeverStalls ∧ NeverStranded and are refused under Buggy/Terminal.	1,2,3,5,10	3/3
release-ownership	contract	upgrade_drive::St::{release,notice_pid,stop,forget}, upgrade::{release_is_next,round_markers}	A release is owed only to the notice's own process; a restart past its signal owes none; the owner's hold releases the asked agent and forgets the markers; round_markers are exactly the round's notices' markers.	1,2,3	3/3
limit-queue-refines-model	refinement	upgrade::{notice_scan,transcript_limit_until,requested_step}, upgrade_drive::queue_facts	Every rendering of harness_upgrade_limit_queue_model is read and stepped as the model does (Retype, Announce, GiveUp, Elapse, NowAsks); a week-long no-reset limit gets copies at 0,0,2,4,8,16 h then daily.	1,3,5,7,10	2/2
visit-shell-refines-pure-reducer	refinement	upgrade_drive::{visit_with_claim,watch::watch_with,release_visit,St::{save,load}}	The effectful visit's step, typed lines and saved St equal the pure composition of readers, reducer and record transitions; at most one notice or release per visit; no signal unless Terminate; watches type nothing.	1,2,3,4,8,13	28/28
upgrade-dry-run-no-hands	contract	harness::upgrade_drive::{connect,connect_at,connect_reads,roster_connection}	connect_at(opts, sock) is reads_only (relay-read-only-connection) iff opts.dry_run; connect_reads and roster_connection are reads_only whatever opts says. Every control connection the harness pass makes is one of these, so a dry run — the CLI's --dry-run and the window's watch — sends READ_VERBS requests alone.	8	1/1
progress-stamp-single-writer	invariant	upgrade_drive::{St::progress_at,progress_key,note_step,refused,refused_at}, restore	progress_at moves iff progress_key moves on every transition; restore keeps the prior stamp; a self-refused notice counts refused without moving the key.	1,2,3,5	2/2
deadline-totality	contract	upgrade_drive::watch::{due_by,watch_at}, St::from_json	Every record owed a step has a due_by and a watch_at (legacy shapes included); only finished moves and pre-restart rounds a --skip holds have neither.	1,3,5	2/2
watch-refines-progress-look	refinement	upgrade_drive::watch::{watch_with,look_word}, upgrade::held_point, upgrade_status::progress_projection	watch_with moves the record exactly as HarnessUpgradeProgress's Watch, with zero input requests.	1,3,8,10	1/1
retarget-in-place	contract	upgrade_drive::St::{for_target,promised_model}	A pending round stays pending retargeted; an announced one stays announced with its marker iff the model handed equals the promised one, else is abandoned owing its release.	1,2,3	2/2
reducer-refines-progress-machine	refinement	upgrade::{requested_step,break_step,quiet_step,held_point}, watch::look_word	StepAtIdle/StepAtBreak are enabled exactly where requested_step acts and land the same phase; the watch's recorded word is the machine's class; negative controls caught.	1,3,5,10	2/2
classifier-refines-progress-machine	refinement	upgrade_status::{Row::attention,progress_projection,Row::{interrupt,badge,limit_passed}}	progress_projection(row(σ)) = σ; no wait marks a tab; attention is Quiet except a passed limit (Fault) and own work held a day (Digest); every time-only turn falls at an instant next_change names.	1,3,5,10	2/2
next-change-complete	contract	upgrade_status::{next_change,Row::next_attention_at,Row::attention,behind_words_change_at,Row::age_said}	next_attention_at(now) is the first later instant where the level changes (None iff it never does); next_change = the min over rows and every time-only change; ageless words never wake the host.	1,3,5,7	9/9
attention-horizons	invariant	upgrade_status::{Row::attention,Attention::level,wait_class,special}	Young records are Quiet; at 100 days never Quiet nor Interrupt; each unclaimed wait turns a Fault at its own horizon; fresh notices keep records quiet; a person's hold or peer refusal never blames aterm.	1,3,5	6/6
interrupt-only-for-an-ended-agent	invariant	upgrade_status::{Row::attention,Row::interrupt,Row::stall}	Interrupt only for an agent the upgrade ended that is not back, or a goal it paused and could not resume; every interrupt is a stall with its one action.	1,3,5	3/3
own-work-news	contract	upgrade_status::{Row::attention,Row::behind_since,Row::rests_on_own_work}	Own-work rows are Quiet until a day behind and a whole round asked, then one Digest(own-work) with counts; the class reads own_work_at (remembered across a round's acts, restamped at most every OWN_WORK_RESTAMP_S) so no act flips it.	1,2,3,5	12/12
aterms-own-fault	contract	upgrade_status::{Row::attention,refused_notice,unwatched,Need::upgrade_cannot_run,fence_fails_under_now}	Unclassified waits, self-refused notices, unwatched records and failing builds are one Fault line each (build-fails keyed by build), never a Digest or mark; upgrade_cannot_run is one bounded sentence with one action.	1,3,5	5/5
stall-mark-and-remedy	contract	upgrade_status::{Row::stall,remedy,moving,word_in_force,will_ask_again,repeating_stop,person_seen}, now_moves_past	stall = Some only for an upgrade that will not move on its own (held-back, repeating stops, stuck restarts, ended agents); overdue waits mark nothing; --now quiets a stall for NOW_QUIETS_S; remedy is Now exactly where --now moves the wait; a person is named only while seen.	1,3,5	14/14
wait-class-total-and-stable	table-property	upgrade_status::{wait_class,Row::cause,Row::overdue_cause,claude_wait_words}	Every wait word either lane records has a class (only unknown words are Unclassified) and Claude words fit 64 chars; one stall keeps its class through the notice path's passing words.	1,3	5/5
row-of-record	contract	upgrade_status::{Row::of,column,move_words,next_round_in,failed_after_exit}, St::{notice_gone,request_for}	Row::of projects the record: the one-token column, move words, restarting/done around confirm_by, next_round, failed-after-exit rows standing with no holder, gone-notice words holding nothing, moved_at, Codex records' rows like Claude's.	1,3	11/11
status-line-and-json-fields	contract	upgrade_status::{Row::line,to_json,waiting_line,word}	Row::line is fixed-order key=word fields (watch fields last, refused= only when non-zero); word(s) is s when a safe ≤64-byte token else its filtered prefix or -; to_json carries the same facts, holders bounded to five.	1,3,7	7/7
need-and-mark-text-bounds	contract	upgrade_status::{Need,cut,Row::more_lines,stall_reason,stall_words,refusal_sentence,refused_stopped}	Needs fit NEED_WHY_MAX/NEED_THEN_MAX with one action; an interrupt mark ≤ 200 bytes never cuts the resume id; digest fingerprints are (kind,tab); refusal and stopped words are the line's own cause.	1,3,7	7/7
resume-command-inverts-relaunch-line	round-trip	upgrade_status::{resume_command,launch_of,export_of,program_at,resume_login}, upgrade::relaunch_line_env	resume_command(relaunch_line_env(dialect,…,F ++ --resume sid), sid) = `claude F --resume sid` with no assignments, for every dialect and option; resume_login names the chosen login.	1,3,5,7	3/3
unwatched-predicate	contract	upgrade_status::{unwatched_since,unwatched_turns_at,Row::unwatched,Row::reread,View::note_look}	unwatched_since holds past WATCH_GAP, UNWATCHED_AFTER_S and the wake grace; unwatched_turns_at is exactly its first second; rows re-read the awake clock; a just-resumed view flags nothing.	2,3,13	4/4
awake-clock-probe	other	upgrade_status::{system_awake_since,awake_since,sysctl_secs}	system_awake_since ∈ [2001, now] = max(waketime, boottime) on macOS, /proc btime on Linux; the test seam pins awake_since.	8	1/1
aside-precedence	contract	upgrade_status::{Row::set_aside,keep_aside,View::{set_claimed,set_off},aside_tabs,not_in_front}	A row on a claimed, autopilot-off or backgrounded tab reads held by that aside and never unwatched; a stood-down view forgets claims.	1,2,3,8	5/5
view-host-protocol	contract	upgrade_status::{View::refresh,watch_due,follows,stand_down}, read_rows	refresh hands rows only when changed; marks are raised and lowered once per stall (including foreign carried marks at the first look); unreadable records hand nothing; watch_due and follows list as stated.	1,2,3,8,13	8/8
row-vetting	contract	upgrade_status::{status_rows,status_rows_with,holders,sessions_to_vet,read_rows}	A pending/announced/stopped record yields a row iff a live process holds its conversation in its (or its holder's proven) tab on an older build; finished and in-flight records always stand; relaunch records never are upgrade rows; unreached rows read the live status.	1,3,8	6/6
owner-word-ask	contract	upgrade_status::{ask,ask_for,ask_within,same_target,word_marker}, St::rearm	Under the sweep lock a word is written with one ledger line, wakes the window and arms a new round (forgetting a gave-up round's READYs); refused with nothing written for a restart under way, an unrecorded tab, another target, or a held lock.	1,2,3,8	6/6
person-hold-footer-note	contract	upgrade_status::{Row::pause_note,held_words,person_seen}	pause_note is Some exactly while a ready move is held only by the person, the same text at every look, naming no time.	1,3	1/1
proc-stat-exited	contract	upgrade_wake::stat_exited	⇔ the first non-space char after the last ')' is Z or X.	1,3	1/1
moved-rule	contract	upgrade_wake::moved	moved(&mut seen, next) returns next ≠ seen and stores next: each change reported exactly once.	2,11	1/1
activation-wake	other	upgrade_wake::{ActivationWake::*,watch_dir,identity,Mark::ensure_watch,SidePoll::look,side_stamp,WakeTrigger,configured_marker}	wait returns true iff a wake mark's identity changed before the fallback; unrelated writes and side marks do not wake; the trigger ends the wait; the watch climbs to the nearest existing ancestor with a recheck.	1,2,8,13	9/9
wait-exit	other	upgrade_wake::{wait_exit,zombie}	Returns true as soon as the process is gone or a zombie (at once if already gone), false only after the limit.	8,13	2/2
json-number-readers	contract	usage::{num_f64,num_u64}	num_f64 reads only JSON numbers; num_u64 floors and saturates, None for negatives and non-numbers.	1,3	1/1
transcript-fold-max-merge	contract	usage::TranscriptUsage::{fold_line,per_model,total,absorb}, spend_from_transcript	Per model, each remembered id counts the max of its rows' counts once, id-less rows sum; repeats are Duplicate or Conflict (growth only); missing model files under UNKNOWN; sidechains summed; total = Σ models.	1,2,3,5,7	8/8
transcript-fold-classification	contract	usage::{TranscriptUsage::fold_line,cannot_be_assistant}	fold_line classifies TooLong, Blank, NotAssistant, Unparsed, NoUsage or a summed outcome; the byte prefilter never skips a row the parser would sum.	1,2,3,5,7	5/5
transcript-fold-bounds	invariant	usage::TranscriptUsage::fold_line	|seen| ≤ SEEN_IDS_MAX with FIFO eviction; |per_model| ≤ MAX_MODELS+1 with overflow into OTHER_MODEL.	1,2,3,7	2/2
working-time-union	contract	usage::TranscriptUsage (turn_duration fold), row_time_ms	Working time = the measure of the union of main-chain turn intervals.	1,3,7	1/1
limit-notice-rows	contract	usage::{LimitNoticeRow,notice_window}	notice_window names five_hour/seven_day for session/weekly notices and None otherwise; the fold keeps the newest synthetic notice per window; the projected parser agrees with a full parse.	1,3,7	3/3
served-at	contract	usage::TranscriptUsage::served_at	The newest timestamp of a real served response (new or grown id, not synthetic).	1,2,3	1/1
last-model	contract	usage::{TranscriptUsage::{last_model,absorb},AccountView::add_transcript}	last_model = the newest main-chain real model; absorb keeps the latest naming one; add_transcript copies it.	1,2,3	3/3
usage-view-shown	contract	usage::UsageView::shown	The first active account, else the first, else None.	1,3	1/1
hud-line	contract	usage::{hud_line,sanitize,truncate_to,model_short,AccountView::add_reading}	The HUD line's fixed shape, windows only from a tap reading, line-breaking chars spaced, ≤ HUD_MAX_BYTES cut on a char boundary; model_short names the family.	1,3,7	5/5
usage-json-schema-1	contract	usage::{usage_json,account_json,spend_json}	One line of sorted-key schema-1 JSON with the fixed fields that parses back to its values.	1,3,7	2/2
rfc3339-utc-civil	equivalence	usage::rfc3339_utc	Total over i64 and equal to the proleptic Gregorian rendering; parse_utc inverts it for e ≥ 0.	3,5,12	1/1
usage-panel-windows	contract	usage::{usage_panel_windows,panel_title,panel_key_slug,pct_used,reset_after,is_bar_glyph}, harness::cli::limits_view	One window per titled meter block with its N% used and reset text, in every measured layout; unmeasured titles get no key; non-panels give nothing.	1,3,7,13	5/5
usage-panel-read-phase	contract	usage::usage_panel_read, aterm_observe::row_matcher	Classifies Inline, NotUsage, Fold, Unsettled or Settled (whole tab, both windows, hint last, nothing unsettled); the header guard matches only the header row.	1,3,7,12	9/9
usage-panel-sample	contract	usage::panel_sample	Some iff both windows were read; figures are their pcts; resets placed in the panel's zone, None past their window's bound (figure kept).	1,3,7,12	3/3
ready-fd-signal	contract	ready::{Ready::open,Ready::hand,Ready::wait}	open leaves no name in its directory and both FIFO ends close-on-exec; hand clears close-on-exec on exactly the write end's number in the child. wait(bound) = Ready iff one byte is read, Closed iff end-of-file with nothing written (every write end closed), TimedOut iff neither within bound; a wait that timed out may be made again, and once one answered Ready or Closed every later wait answers Closed.	8,13	1/1
relay-read-only-connection	contract	RelayClient::{reads_only,may_send,request_line,request_frame}, is_read_request, READ_VERBS	A connection made reads_only() writes request line l iff is_read_request(l): the first word of l after its @-selectors ∈ READ_VERBS, and for `meta` no `set`/`unset` follows. Every other request line, and every length-framed write, returns Err(PermissionDenied) with nothing written. A connection never made reads_only writes every request.	1,8	1/1
relay-framing-one-line	contract	RelayClient::{write_line,send,request_frame}	A line or head with CR/LF is InvalidInput with nothing written; otherwise exactly line+LF, or head SP len LF payload.	1,2,8,11	3/3
relay-read-bounds	contract	RelayClient::{read_line,read_body_lines,request_bytes}	read_line enforces MAX_LINE_BYTES however reads chunk; read_body_lines checks the count and one aggregate allowance, consuming nothing past a failure; request_bytes reads exactly its count.	1,2,5,7,8	2/2
reply-count-grammar	contract	reply_count, RelayClient::{request_counted,read_text}	Ok(Some(n)) for `OK n…` with n fitting usize, InvalidData for an overflowing digit token, Ok(None) otherwise; read_text refuses a header without a count.	1,3,5,7	2/2
relay-reply-classification	contract	RelayClient::{read_reply,request_status,help_counted}, is_ok_reply, control_verbs::host_gone	A first line that is host_gone is UnexpectedEof naming the pid; otherwise verbatim. Success is exactly OK or OK SP …, including memoized help; body lines are never classified.	1,3,12	1/1
ctl-failure-words	contract	failure_words, unsettled	failure_words is the trimmed stderr, else `aterm-ctl exited {code}[: stdout]`, never empty; unsettled names min(ms, cap) and which bound applied.	1,3	1/1
answer-plan-contract	contract	answer::{plan,plan_by_role,option_named,names_no_option,policy_refusal}, QuestionPriority::parse, recommended_set, answer_question	A dialog read whole (Claude Code's): human ⇒ Human; recommended ⇒ the decider's keys; a single-select answer names one option by number or label, else Err; a multi-select toggles what differs then presses the button (refusing a dirty free-text row); review takes `submit`. A question read by its answers' roles (no dialog: Codex's) ⇒ plan_by_role: human ⇒ Human; no Answer-role option ⇒ Err; recommended ⇒ the decider's Digit(n, guard) or Err; `<n>` or an option's exact label (case folded) naming an Answer-role option ⇒ Digit(n, row_guard(the title row)); the last row (`None of the above`, Role::Other) ⇒ Err (a person's own words); a head off the rows read ⇒ Err; anything else ⇒ names_no_option's Err (a text priority's refusal names `meta set questions <word>`).	1,3,5,7,12	6/6
answer-box-token	contract	question_box_token, role_box_token, prompt_box_token, token_of	question_box_token = 8 lowercase hex chars of a folded FNV-1a of (form, headers, question); role_box_token = the same over (title, description, option labels); prompt_box_token(p) = the dialog's question_box_token, else role_box_token for a Question with an Answer-role option, else None; deterministic in exactly those inputs (distinctness is not claimed).	1,3,7,12	2/2
ledger-row-json	round-trip	approvals::{Row::to_json,json_str,cut,sha256_hex}	to_json is the fixed-key object; cmd_sha256 hashes the whole command; command cut on a char boundary to COMMAND_BYTES; fields parse back.	1,3,5,7,12	1/1
ledger-readers-are-row-projections	contract	approvals::{typed_at,typed_texts,probe_owed,reset_owed}	Each reader is the stated projection of the session's rows; json_str escaping makes forged fields impossible for every row content. reset_owed's Enter is an approved row of the rule whose command is exactly some kind's `Yes, use my <kind>` (2.1.304) or an older build's `Yes, use my reset` (RESET_YES_2_1_287), never a bare `Yes, use my …`.	1,3,5,7,8	4/4
ledger-paths-and-bound	contract	approvals::{ledger_under,path_under,journal_beside,bound}, Ledger::write	ledger_under = st/drive/<sanitised sid or self>.jsonl; journal_beside appends .journal.jsonl; bound keeps the last KEEP_ROWS past MAX_BYTES; write appends one line.	1,3,7,8	2/2
blocks-partition	contract	blocks::{blocks,head_kind,is_tool_head,is_group_row,is_prose_row,GUTTER}, phase::is_tool_call	blocks(rows) partitions rows into contiguous blocks at heads read by Claude Code's grammar, for every program's report (aterm reads no Codex screen since its screen lane's retirement, 2026-10-16); prose continuations never start a block; every phase tool call is a Tool head.	1,3,5,7,12	3/4
blocks-views	contract	blocks::{view_rows,without_gutter,trimmed}, report::Report::render_view	view_rows(rows, view): All = rows; Messages = user, message and done blocks with their gutter output dropped (`⎿`, rows indented ≥5 under it) (a subsequence); a done block's rows are its done row; Final = the last message block (head possibly scrolled off) and the first done block after it. render_view adds view= and kept= to the header.	1,3,5,7	8/9
claim-verb-classes	contract	claim::{is_input,is_write,busy_holder}	is_input ⇔ the verb is key/send/turn/paste/feed-bin; is_write adds post and meta set/unset attention; busy_holder decodes supervisor=.	1,3,5	2/2
classify-sound	refinement	classify_command, classify_command_with, and its scanners	read_only ⇒ under a Clean model of zsh/bash every program run is a read in ReadSet (or an allowed python3 script), no redirect writes, no assignment binds an evaluated parameter or a variable a program reads to run something (HAZARD_VARS and the HAZARD_VAR_PREFIXES LD_·DYLD_·GIT_·BASH_FUNC_·LESS, the last since classifier audit 3: LESSKEYIN=f less ran f's #env LESSOPEN command, MEASURED on less 668), no expansion evaluates unspelled text, nesting bounded; a bare INPUT redirection with no command (bare_redirection) runs $READNULLCMD (default more) and is refused, except where no null command runs (MEASURED zsh 5.9, bash 3.2): a substitution whose whole body is one input redirect from fd 0 ($(< f), `<f`, $(0<f), which reads f) and a redirect trailing a ( … ) group ((cat) < f, which feeds the group); a bare output redirect runs $NULLCMD (cat, a read).	1,3,5,7,10,14	54/61
classify-precise-on-read-grammar	contract	classify_command, classify_command_with	Every line of the read grammar G (lists and compounds of ReadSet heads with literal words, operand-position expansions, read-only redirects and substitutions in G) is read_only: the classifier is not trivially false.	1,3,5,7,10,14	1/1
classify-except-rm	contract	classify_except_rm, is_mktemp_guard, assignment_hazard, redirect_hazard	read_only ⇒ classify-sound holds with the rm/mktemp/assignment-only segments removed, none of those has a hazard or a write redirect; equal to classify_command_with on lines without such segments.	1,3,5,7,10	1/2
plain-zsh-line-grammar	contract	classify::{plain_zsh_line,plain_zsh,plain_dollar,substitution_close}	Ok ⇔ the line is in the language L_plain: no special redirect, reserved first word or control char outside single quotes, and every `$` one of `$NAME` or `${NAME}` (ASCII name, letter or `_` first) or a `$(…)` whose text is itself in L_plain one level deeper (no newline, comment, backtick or backslash in it; at most PLAIN_NEST deep), none followed by `[`, `:`, `(`, `{` or a non-ASCII char; `$((` refused. Under ShellSem's zsh (Bash tool setopt, inert startup) such a line expands no further than its bash reading: a parameter read unsplit and unglobbed, a substitution split and unglobbed.	1,3,5,7,13	2/2
glob-match-fnmatch	equivalence	glob_match	glob_match(p,t) ⇔ t ∈ L(p) with * any string (including /) and ? one char.	1,3,5,7	1/1
python-allowlist	contract	segment_head (python3 arm), DEFAULT_PYTHON_ALLOW, glob_match	The default allowlist is empty; `python3 p` reads iff p has no .. and no expansion and matches an allowed glob; every other python3 form refuses.	1,3,5,7	2/2
strip-quotes-shape	contract	strip_quotes	Quoted spans become "" (substitutions inside double quotes kept after a mark), \( \) unescaped, everything else kept; idempotent on its output.	1,3,7	1/1
split-segments	contract	split_segments	Flattens the command list on its real separators (not \; or redirect &), groups as their own segments, substitutions replaced and their bodies emitted before the enclosing segment; split_segments_at also tags each segment's position: a command (Command), the whole body of a substitution that is one input redirect from fd 0 with no separator (FileRead), or the words after a closing ) of a group (AfterGroup).	1,3,5,7	1/2
program-words	contract	program_words	Same segmentation as split_segments ∘ strip_quotes with quote-resolved words and expansions rendered `$…`; program_words_nested also marks each segment that is a substitution's body.	1,3,5,7	1/1
classify-flags-on-argv	contract	supervise::classify::{flag_hazard_at,seg_flag_hazard,argv_flag_hazard,runs_or_writes_by_flag,git_read_writes,program_words,classify_command_with,classify_except_rm}	read_only ⇒ no argv word of the line (program_words: single and double quotes and backslashes resolved) is a flag-and-option write-or-run form: find -delete/-fprint*/-fls/-exec·-execdir·-ok·-okdir with a program off FIND_EXEC_READ or with that program's own write flag (find -exec file -C), sed -i/-I, sort/tree -o, uniq out, python -c, perl -e, bash -c, a git write flag or subcommand, a listed git read's write form (git_read_writes: branch/tag create·move·delete, a remote verb, the first operand, that writes, config set·edit·unset), the git cat-file textconv/filters driver by parse-options abbreviation (git cat-file --t), git remote show naming a remote without -n/--no-query (git_remote_show_contacts: the verb is the first operand and an operand follows it; it contacts the remote), and the per-head run-or-write flags (less/more +·-o·-k, file -C, tree -R, sysctl =·-w·-f·-p·--file·--system (a file of name=value pairs that sets the kernel), date -s, rg --pre·--hostname-bin (classifier audit 3, REASONED from rg 14's --help: it runs a program for a hyperlink's host name), printf -v, sort --compress-program). So a quoted or escaped flag the quote-stripped scans blank to "" still refuses (each read-only under classify-v6, refused under classify-v7: find '-delete', sort '-o' out, git remote 'add' x u, git branch -a '-m' a b, printf '-vx[1]' hi). A flag token is refused whatever program it follows, quoted or not; redirects, assignments and compound closings read a quoted glyph as data and stay on the quote-stripped words.	1,3,5,7	8/9
classify-expansion-into-a-flag	contract	supervise::classify::{glob_in_an_option_slot,may_expand_into_a_flag,option_slots_end,expansion_in_an_option_slot,head_from}	read_only ⇒ for a SELECTED_BY_AN_ARGUMENT head or a listed git read, no quote-stripped word in its option slots (before `--`; every word for find, whose primaries `--` does not end, and sysctl) may brace-expand or glob into a word starting with `-` or `+` (may_expand_into_a_flag: expands_apart and no literal first character); for uniq, sysctl, git config and git remote no such word expands at all (an operand's count or verb); git's global options hold none; an expansion past `--` is refused for find and sysctl, an unquoted one for uniq and git config (a bash split). Words that expand only into their own literal start (HEAD~1, ./*, src/*.rs), globs past `--` and `{}` keep their verdict.	1,3,5,7	1/1
prelex	contract	prelex	Removes word-opening comments (a `#` at the text's start, or right after a blank, `;`, `&`, `|`, `(` or `)` that no backslash escapes) and quoted-heredoc bodies, normalises $'…'/$"…"/${…}; Errs on substituting unquoted heredocs, unterminated heredocs/quotes and newline-decoding $'…'.	1,3,5,7	0/0
sed-script-and-alarm-idiom	contract	sed_script_writes, sed_args_write, is_alarm_body, strip_alarm_idiom	sed_script_writes = None ⇔ the script parses fully with no w/W/e command or flag, the regex sections (address and s pattern) read a [...] bracket expression's delimiter literally and a label to a command end; sed_args_write reads the script from -e/-f spelled whole, attached, clustered or abbreviated, and -l takes no value; is_alarm_body ⇔ `alarm N; exec @ARGV`; strip_alarm_idiom removes exactly that wrapper.	1,3,5,7	1/4
numeric-format	contract	numeric_format	⇔ the \u/\U-decoded format has a numeric conversion or * width/precision; malformed escapes are conservatively numeric.	1,3,5,7	0/0
zsh-hazard-tables	table-property	assignment_hazard, ZSH_RUNS_A_VALUE, ZSH_TIED, ZSH_ARITHMETIC_VARS, HAZARD_VARS	Every listed name assigned a path is a hazard; tied arrays are judged as their scalar (table membership against real zsh is a bench measurement).	1,3,5	0/0
config-power-lattice	invariant	SupervisorConfig::{default,limit,limits,set,default_shown}, KEYS	≤ is a partial order with default on top (except the opt-in keys, `stall_term`, `limit_reset`, `usage_notes`, `usage_credits` and `stuck_break_note`, whose default false is the bottom, and `occupancy`, whose default `observe` sits under `prompt`); a changed `limit_reset_kinds` is ≤ only with limit_reset off (neither the lone-offer default nor a list is less power), and limit leaves it as it is; limit(default,k) ≤ default and changes only k; enabling is visible; default_shown is None only for rules_file and non-keys.	1,2,3,5	3/3
config-reader-never-grants	contract	SupervisorConfig::{from_aterm_toml,set}	from_aterm_toml(text) ≤ default except the root-table opt-ins (each ships false, and `occupancy` ships `observe`; only the root [harness] table's true — `occupancy`'s `"prompt"`, never an on spelling — turns it on, a misplaced table's grants nothing and its false or off still limits; `stuck_break_note`'s grant also needs `facts`, whose false limits it); an unreadable value keeps the default with one note; any off spelling is the limit; unknown keys change nothing; answer_questions=false wins in either order.	1,2,3,5,7,12	14/14
config-misplaced-only-limits	contract	SupervisorConfig::from_aterm_toml (misplaced pass)	Result = root reading ⊓ the limits of every misplaced harness.<k> (grants contribute ⊤), independent of order, each noted with its path.	1,2,3,5,7,12	2/2
config-line-reader-agrees	refinement	SupervisorConfig::from_aterm_toml (fallback line reader)	For text TOML refuses, the meet of the one-line [harness] settings outside multi-line values and comments, BOM-tolerant, first note `not valid TOML`.	1,3,5,7	3/3
config-texts-round-trip	round-trip	SupervisorConfig::{texts,set,against_default}	texts lists KEYS in order and set(default,k,text(c,k)) = c; against_default names limits and own values by key.	1,2,3,5,7	2/2
config-retired-and-agent-keys	table-property	RETIRED_KEYS, agent table reader, is_unknown_key_note, is_retired_key_note, cli::disk_faults	A retired key written false is the limit it named with one note; agent tables read only `enabled`; agent names are disjoint from keys; [harness] disk.* notes are the disk reader's.	1,3,5	5/5
config-from-path-and-hosted	contract	SupervisorConfig::from_path, SuperviseOpts::{hosted_with,hosted}	from_path reads lossily (missing = default, unreadable = a note); hosted_with(c).policy = c.	1,8	2/2
mode-word-parsers	round-trip	Autopilot::{parse,word,ALL}, autopilot_for, MemoryCeiling::{parse,text}	parse(word(m)) = m with the stated synonyms; MemoryCeiling parses auto/off/0/n GB and refuses fractions, negatives and bare units.	1,3,5	1/1
escalate-attention-text	contract	escalate::{attention_text,point_label,fit_reason,kind_and_subject}	attention_text ≤ ATTENTION_BYTES (200 keyed), char-boundary cut, `<agent> <kind>: <subject>`, ending `…)` when cut, and any resume line kept whole or dropped.	1,3,5,7,12	4/4
escalate-kind-and-subject	contract	escalate::{kind_and_subject,status_field}	A question's subject is its own question text; every rm breaker kind is rm-breaker; status_field reads k=v off the OK line.	1,3,5,12	4/4
journal-line-grammar-and-json	round-trip	journal::JournalRecord::{of_line,to_json,from_json}	of_line splits kind, phase and the leading seq/complete/rows/report tokens; from_json(to_json(r)) = r, older records accepted, truncated or kindless refused.	1,3,5,7	3/3
ladder-clamp	contract	ladder::Ladder::{at,step}	at(n) = s[min(n,|s|-1)] (None for empty); step = at or 0.	1,3	1/1
doubling-backoff	contract	ladder::doubling, approval_loop::reask_pause	doubling(base,n,max) = min(base·2^n, max) without panic; reask_pause(a) = doubling(250 ms, a⊖2, 60 s), monotone and ≤ 60 s.	1,3	2/2
ledger-html-assets	table-property	ledger_html::{legend,swatch,STYLE,SCRIPT}	The legend has one row per MarkKind (the reply's working bar after the reply's own), each in a swatch class STYLE has a rule for, and each item's data names its swatch; SCRIPT colours every mark by that swatch, in the colour its STYLE rule shows; neither asset fetches anything.	1,5	2/2
ledger-mark-of-word	contract	ledger::{MarkKind::of_record,assert_marked}, journal::{Word,Noted,Journal::record}	Each journal Word but EVENT, and each Noted word (a line kept as kind other), marks its lines with a kind no other word, EVENT phase, turn, reply or mail shares; each EVENT phase the loops print has its mark; an older build's other line of a word this build reads takes that word's mark; a kind, a word or an EVENT phase the reader does not know is unread, and a test build's journal refuses to write such a line.	1,3	2/2
ledger-gather	contract	ledger::{gather,Ledger::summary,items}, parse_history_records	gather issues each verb of its source once; failing sources are named (never zero counts); turns are timed only by the log's own records joined by ledger=, never by a clock anchor, and no time is guessed: an unread log is named where the latency would be; missing rows are listed as swallowed; summary sums as stated; items sorted by t.	1,2,4,7,8,13	10/10
ledger-since	contract	parse_since, ledger::gather (since filter)	parse_since is total and accepts ms or a valid date[-time][zone]; with since every item has t ≥ since.	1,3,5,7	3/3
ledger-clock-words	contract	span, stamp, zone	span in s / m ss / h mm; stamp in local time with or without the date; zone UTC±HH:MM.	1,3	1/1
ledger-render-escapes	contract	render_md, render (Html)	Markdown escapes | in cells; HTML is one self-contained, well-formed page with worker text escaped and `</script>` only where it belongs.	1,3,5,7	2/2
limit-zone-and-reset-grammar	contract	limit::{parse_reset,parse_clock,resumes_by_itself,one_line,zone_offset_at}	parse_reset accepts the span, clock and date shapes with zones and refuses out-of-range parts; resumes_by_itself by its phrases; one_line joins words; zone_offset_at is None for a name no zone has.	1,3,5,7	8/8
limit-reset-placement	refinement	limit::{reset_at,civil,days_from_civil}	reset_at equals the reference placement (spans from now, dated in the nearest year, bare clocks today unless passed beyond SAME_DAY); civil and days_from_civil are inverse bijections.	1,3,5,7,13	3/3
limit-wall-placement	contract	limit::{reset_in_zone,reset_at_with,place_reset,place_reset_with,within_a_session_window,offset_at,zone_offset_at}, Session::{reset_unix_for,reset_unix}	reset_in_zone finds the wall-clock instant after now in that zone's offset at that instant (DST-correct); place_reset puts a weekly/spend wall's bare clock after its print, a session wall's within 5 h of the print, else the session rule; an undated cancelled limit is clamped to a session window; zone lookups refuse non-zone names.	1,3,5,7,8,13	7/7
mail-row-and-principal	contract	ledger::is_from_session	is_from_session(from, worker) ⇔ from splits at its first '@' into (s, n) with n starting `n-`, and, for w = worker less one leading '@', either w holds no '@' and s = w, or w splits at its first '@' into exactly (s, n): the whole principal, never the sid alone, so a from with no '@' or a node not `n-…` never matches and a worker that names a node matches that node only.	1,3,5,7	0/0
memory-ceiling-formula	contract	memory::MemoryCeiling::ceiling_mib	Off ⇒ None; Gib(n) ⇒ 1024n; Auto ⇒ clamp(p/BASE + min(a,p)/AVAILABLE, floor, max(p/2, floor)) (UNKNOWN_MACHINE_MIB without p); monotone in available.	1,3	1/1
memory-judgement	contract	memory::{AgentMemory::strain,dominant,judge,MemoryOver::acts,episode_over,MemoryOver::verdict}	strain at Critical or Warn-with-swap; dominant = largest and ≥ 1/5 of use; judge Some iff past the ceiling; acts ⇔ strain ∧ dominant; the episode ends an eighth under; verdict depends only on (strain, dominant, restarts).	1,2,3,4	5/5
memory-ceiling-order	contract	memory::MemoryCeiling::no_more_than	A partial order with Off at the bottom, higher Gib limiting, Auto incomparable to Gib.	1,3	1/1
memory-fact-banner	contract	memory::{MemoryOver::fact,warning,gb}, aterm_phase::memory_banner_head	fact survives memory_banner_head unchanged; gb has one decimal; the warning's last clause depends only on the verdict.	1,3,12	1/1
approval-never-spends	invariant	policy::approval::{decide,never_spends,kept_or_bought,buys,spends,aterm_phase::uses_a_reset,confirms_reset,keeps_reset,allow_once,other_dialog,credits_consent}	No decision presses an option that buys or spends, whatever `usage_credits` says, except the limit-reset grant row under its own key; structured question options and checked options submitted by a multi-select button also pass the reset-spend guard; spends reads 2.1.304's kinds (`Use a <kind> (…)`, `Yes, use my <kind>`, the Fable kind's too) and 2.1.286–2.1.303's words alike; a reset confirmation is never answered yes; the chooser `Which reset do you want to use?` is pressed by no rule at any level; a Codex `Continue with` is no consent; Claude Code 2.1.304's spend-limit nudge (`Adjust monthly spend limit: $N`) and promo buy screen (`Pay $X · Get $Y`) are purchases; a consent onto usage credits (credits_consent) is pressed only under `[harness] usage_credits` (approval-dialog-rules). A credits consent's Esc (`credits-decline@v1`, Choice::FocusKey key esc) chooses no option: the press gate and never_spends read none of it, and it saves nothing (2.1.304 bundle). The spend-limit nudge whole gets its `Wait for limit to reset` row (no-spend@v1), key on or off.	1,3,4,5,7,12	7/9
approval-one-shot-only	invariant	policy::approval::{decide,once_choice,Role,decide_box,kept_or_bought,kept_for_good,allow_once}	Every press is the one-shot allow, the specific option a named rule picks, or a decline's No; never a standing grant (an option kept for good by its role, Persist, its label, settles_later or a whole-label kept_answer, or its rows, Opt::kept: a run of its unnumbered block's label rows that takes in one of its own spells a kept_answer whole, or, ending the block's rows as a frame half drawn may, the first two words or more of one (kept_answer_head), however a narrow pane's wrap left its label joined, a term kept as defence in depth since the reader reads such an option Persist whenever it reads roles - every option of an MCP server dialog, and under any title, its roles read or withheld, the one-server dialog's three answers, the several-server dialog's `Enable selected` and the Chrome setup's `Continue with browser tools` - is refused by the press gate whatever rule picked it); a grant-only box is declined; a role-less box escalates, and so does a box read with no role a press is chosen by, only standing ones (Role::standing: an unnumbered block a rest that opens with no capital blanks), no yes of it taken by its label. The standing answers given are the folder-trust dialog's and the autocompact offer's (its own rule, by the owner's direction). Carved out: a consent onto usage credits (credits_consent) under `[harness] usage_credits` is its own rule's, `credits-consent@v1` — a model's `Continue with <M>`, which Claude Code keeps for good per model and account, the owner's opt-in — and never logged as allow-once@v1. Full power answers agy's and Muse's boxes by role (the one-shot allow) and leaves Muse's tool-action and unix-socket boxes to a person; each captured Claude Code lab box (2.1.307 to 2.1.309) is answered as measured.	1,3,5,7,12	16/19
approval-level-monotone	contract	policy::approval::{decide,decide_safe,decide_box,rm_breaker_warn}	None escalates every permission box; whatever Safe approves All presses the same way (unproven presses marked); Safe approves an unproven rm breaker with a warning unless rm_breaker_ask; non-default shells and remote boxes are never proven.	1,2,3,4,5,12	5/7
approval-safe-bash-sound	contract	policy::approval::{bash,readings_proven,readings_read_only,proof_gaps_clear,read_only_every_reading,network_free,brace_free,blanks_shown,plain_under_zsh,shell_models_hold,git_reads_clear,secrets_free,secret_hit,path_readings,operand_comps,default_secrets}, classify::path_operands, classify_command_with, policy::box_command::blanks_read_as_drawn	Safe approves a Bash box only out of bypass, with no vendor note (or the no-rule note alone, under approval-no-rule-read-only-sound), local, default shell, every reading read-only, network-free (approval-network-free), brace-free (approval-brace-free), read as drawn and clear of proof gaps (readings_proven = read_only_every_reading, which ends with network_free then brace_free, then blanks_shown, then proof_gaps_clear, in that order, the first refusal returned: no here-document, and no backslash before a line's end, a tab, two blanks or a blank that ends a line; no assignment, env, export, xargs, loop variable or glob beside a program off GLOB_SAFE, as approval-no-rule-read-only-sound states; readings_read_only, the advisory word aterm-gui's prompt_detail and the supervisor's EVENT and phase lines call, is readings_proven then secrets_free over default_secrets of this process's HOME), plain under zsh, every git read clear, and secrets-free: the Read box's matcher (secret_hit over default_secrets, shared) finds no secret among classify::path_operands of any reading, each ~ resolved to the home directory and a word led by an expansion ($HOME/…) also matched as under it. path_operands is a superset of the names the line's programs open, by head: every redirect target (heredoc and herestring words are text); for grep, rg, jq, awk, pgrep, wc, du, git grep and git log the operands and the path-valued options of a grammar (pattern, filter, program and text-valued options excluded, grep's, rg's and git grep's counts, contexts, filters, types and separators among them (classifier audit 3: grep -A·-B·-C·-D·-d·-m and their long names, MEASURED on macOS grep 2.6.0-FreeBSD, where --context and --color take a value only glued; git grep -A·-B·-C·-m·--context·--max-depth·--threads, MEASURED on git 2.54.0, a prefix spelling a long one; rg -g·-t·-T·-M·-m·-j·-d·-r·-E and the rest of rg 14's value options, REASONED from rg --help), a long option matched by its full name, else, for a tool that abbreviates (grep, git grep, gawk, GNU pgrep, wc and du; not jq or git log, MEASURED, nor rg, REASONED), by a prefix unless the prefix is a grammar's listed flag (grep --binary), so jq --slurp and rg --ignore are never --slurpfile or --ignore-file; awk string literals and jq import paths included); nothing for a head that opens no named file (echo, printf, which, type, test, ls, stat, df, find without -exec·-execdir·-ok·-okdir·-files0-from, …); every word for any other head and for git's other subcommands (a REV:PATH word also as PATH, a short-option cluster as each tail after one of its letters); and every word of every segment inside a substitution or on a line that names xargs or files0-from. A secret a program discovers (recursion, a glob) or a path built at run time is outside it; each operand (operand_comps: the word, a ~ lead resolved to the home, an expansion lead read as the home at either separator, a root-relative \… also on a drive home's drive) and each home rule are read by path_readings, a hit on any reading a hit, so each reading past the plain / split only adds refusals (stricter only, so no id moves): split at / alone and at \ too (Windows' separator, which Git Bash's MSYS layer also takes), a drive path (C:\a, C:/a) as Git Bash's /c/a spells it, and a rooted path's .. kept as a component (a symlink before it leads elsewhere) and also taken lexically; so a Unix home's rules and a Windows home's (C:\Users\name) match those spellings of a path under it. Windows' own aliases of a name (8.3 short names, trailing dots and spaces, :stream suffixes) are outside it. git_reads_clear refuses a git remote show by classify::git_remote_show_contacts. RULE_READ_ONLY = read-only@classify-v9 (classifier audit 3 runs proof_gaps_clear on the note-free box, MEASURED on 2.1.309 to draw a folder grant and no note before the vendor's own assignment, env, loop and glob checks; adds the LESS… variables to the hazard prefixes (LESSKEYIN ran a command, MEASURED on less 668), refuses rg --hostname-bin, and reads the grep, rg and git grep value options; the id moved from v8 both ways. v8 read jq, rg and git log long options by whole names, keeping --slurp and --ignore flags from consuming a file operand, while the other grammars kept prefix matching (main's narrow port of this audit's option exactness, 18777c2128); the id moved from v7 both ways. Classifier audit 2 added the bare-input-redirection, network, flag/path-reaching-brace and sed/sysctl/git refusals and secrets_free, and approves reads v6 mis-parsed: sed -l SCRIPT, an attached -eSCRIPT, a bracketed delimiter, git remote show naming no remote; the id moved from v6 both ways).	1,3,4,5,7,12	19/25
approval-breaker-read-only-sound	contract	policy::approval::{breaker_read_only,command_readings,names_a_removal,readings_proven,proof_gaps_clear,read_only_every_reading,network_free,brace_free,blanks_shown,plain_under_zsh,shell_models_hold,git_reads_clear,secrets_free}	RULE_BREAKER_READ_ONLY (breaker-read-only@v9; v6 judged by classify-v6 and read no transcript, v7 judged by classify-v7 and runs secrets_free, v8 shares the whole-name option grammar in secrets_free, v9 judged by classify-v9 and runs proof_gaps_clear, since the box draws no note of its parse and the classifier is its whole proof) approves only the TooManySubstitutions breaker with its own two-option Yes/No, under Safe in any mode, when no reading names rm or rmdir as a word in any case and every reading of the screen's rows (every join command_readings keeps) is read-only, network-free, brace-free, secrets-free, plain under zsh, startup-modelled and git-clear, and read as drawn (blanks_shown) and clear of proof gaps (proof_gaps_clear); no transcript is read and no wait is spent for one (v4's transcript lane decided nothing on 2.1.308 and cost every too-many box lag retries plus a 1 s settle); the press is option 1 under row_guard(the first command row).	1,3,4,5,7,12	7/9
approval-no-rule-read-only-sound	contract	policy::approval::{bash,readings_proven,proof_gaps_clear,GLOB_SAFE}, classify::{unquoted_glob,scan_unquoted,scan_double,runs,command_words,program}, aterm_phase::anchors box.note_no_rule	RULE_NO_RULE_READ_ONLY (no-rule-read-only@v4; v1 judged by classify-v6, v2 by classify-v7, which lists CDPATH/PYTHONHOME/FPATH among its hazard variables so the no-rule note no longer lifts a CDPATH assignment, v3 shares the whole-name option grammar in secrets_free, v4 by classify-v9) approves a Claude Code Bash box whose notes are exactly [anchor(box.note_no_rule)] = ["This command requires approval"] (2.1.308's bashMissKind no-rule-match, drawn where its allow check `!Me && !xe && Oi.isReadOnly(e,w) && !tzt(e,s) && !y.some(Lhe)` fails, bundle @17777748: a Read deny rule beside xargs, an unquoted glob for a program off its qGt set, its read-only check, or an assignment outside its safe variables), under every condition of approval-safe-bash-sound and proof_gaps_clear (named no_rule_clear before classifier audit 3, which runs it through readings_proven on every box the classifier alone proves: this note, the note-free box under read-only@classify-v9 and the too-many breaker under breaker-read-only@v9; MEASURED 2026-10-23 on 2.1.309 in the real-Claude lab, a read outside the working directory draws a folder grant and no note, so FOO=1 cat /etc/hosts, env cat /etc/hosts, a for loop and sort /etc/host* reached aterm with no note; the rm breaker's arm does not run it, being bypass-only, with the rm resolver judging assignments): for every reading r and segment s of command_words(r), with a = runs(s)'s command index (len(s) when it has none), no word of s[..a] is an assignment (NAME=value; NAME+= and NAME[…]= the classifier refuses as programs), no word of s[..=a] is env, export or xargs by program(), s[a] is not for or select, and when unquoted_glob(r) finds an unquoted, unescaped *, ? or [ (quotes and backslashes honoured; $(…) and backquoted commands scanned as shell text, inside double quotes too), program(s[a]) ∈ GLOB_SAFE (the vendor's qGt less cd) for every segment; the press is the one-shot Yes (Digit(1)). Any other note, a second note, or the note's text changed escalates with "the box carries a vendor note: {first note}"; a refusal under the note ends " (the box's vendor note: This command requires approval)". MEASURED 2026-10-22 (docs/measured/box-notes-2026-10-22.md): the too-complex notes stop the vendor's walk at the first form it cannot check, and the flag and command notes carry the vendor's own concern, so both stay with a person.	1,3,4,5,7,12	4/5
approval-network-free	contract	policy::approval::{network_free,read_only_every_reading,rm_breaker arm}, classify::command_words	network_free(R)=Ok ⇔ for every reading r ∈ R and word w of command_words(r): w contains none of /inet/, /inet4/, /inet6/ (gawk's network files) ∧ when w's operator (leading digits dropped) starts with '<' and not '<<', its target (the rest of w after the operator and any '&', else the next word) neither starts with /dev/tcp/ or /dev/udp/ (after quote removal, as bash matches its special files) nor contains '$' or '`' (an expansion may compute it). Held by read_only_every_reading (so by read-only@classify-v9, no-rule-read-only@v4 and breaker-read-only@v9) and by the rm-breaker arm before blanks_shown. Classifier audit 2 also gave the classifier its own network refusal (classify::{network_read,network_reach}, called from classify_command_with, the CLI/presence path), so the CLI and the presence word agree; approval::network_free is kept to name the reading in the box message and for the rm-breaker arm (classify_except_rm leaves the network reach to that arm).	1,3,4,5,7,12	1/2
approval-brace-free	contract	policy::approval::{brace_free,read_only_every_reading,rm_breaker arm}, classify::{brace_expansion,scan_unquoted,scan_double,past_single,brace_close}	brace_free(R)=Ok ⇔ no reading r ∈ R holds an unquoted, unescaped '{' not right after '$' whose depth-matched '}' closes it in the same word (before an unquoted blank, tab, newline, ';', '|', '&', '<', '>' or an unopened ')', with '(…)' nesting part of the word) with an unquoted ',' or '..' at depth 1; quoted text ('…', "…", $'…' with escapes, opened only by an unescaped quote or $) and a backslash's character are characters, but a $(…) or backquoted command is scanned as a command of its own wherever it stands, inside double quotes too, and a '{' inside ${…} is scanned all the same. bash brace-expands before any grammar (MEASURED: `echo sort x {-o,y}` prints `sort x -o y` under bash 3.2.57 and zsh 5.9), so the classifier judges words bash never runs; breaker-read-only@v4 approved the too-many box over `sort x {-o,y}`. Held by read_only_every_reading and the rm-breaker arm. Classifier audit 2: the classifier refuses a brace that can reach a flag or path through its head-aware option-slot scan (glob_in_an_option_slot/may_expand_into_a_flag and the git global/sub-arg scan), so `sort x {-o,y}` and `git -C {a,b} status` are refused there (and at every consumer) while an operand-only brace (`echo {a,b}`) stays a read; approval::brace_free is kept as a CONSERVATIVE box-level check that refuses EVERY brace expansion and names the reading (it adds box-level strictness over the classifier, and the rm-breaker arm calls it). brace_expansion/brace_close live in classify and are used only by brace_free.	1,3,4,5,7,12	1/1
box-command-blanks-as-drawn	contract	policy::box_command::blanks_read_as_drawn	blanks_read_as_drawn(t)=Ok ⇔ no run of '<' in t is 2 long or longer than 3 (a here-string `<<<` is one) ∧ no backslash in t is followed by ⏎, t's end, ⇥, or ' ' then ' ', ⇥, ⏎ or t's end.	1,3,12	1/1
box-command-pending-call	contract	policy::box_call::{pending_calls,ancestor}	pending_calls(t) = the main-chain tool_use calls of t, in file order, with no result (sidechain and non-user/assistant rows are ignored; a tool_result in any main-chain user row of the tail answers its call wherever it is written; a user row with no result ends every call written before it in the file unless it is on that call row's parentUuid chain within the tail, its own turn's prompt; a call row with no uuid has no chain, so any later such row ends it), each with its id, tool, file_path or notebook_path, and command (rows read through transcript::calls). Since breaker-read-only@v7 only the tool lane (box_call::drew) reads them, keying on tool and file; no Bash rule reads a call's command. PendingCall.command is kept (classifier audit 2, not moved or dropped): it completes the parsed record and its parser test pins the extraction; aterm-lab does not read it (its .command reads are on forever_cover's ledger rows, an unrelated struct).	1,3,7	2/3
approval-rm-breaker-sound	contract	rm_breaker::resolve, resolvable_breaker, classify_except_rm, blanks_shown, is_mktemp_guard, secrets_free, decide (rm arm)	RULE_RM_BREAKER approves only in bypass with cwd known and scratch roots, every operand resolving (through links) strictly inside a scratch root or a guarded mktemp dir, the rest of the line read-only, every reading read as drawn (blanks_shown), no rebinding hazard, and (classifier audit 2) no secret among the rest of the line's path operands (secrets_free, after git_reads_clear); anything else escalates under Safe. (subsumes recomputed 2026-10-22: 5 §2 rows, was mislabelled 8/8.)	1,3,4,5,7,8,10	5/5
approval-read-rule	contract	policy::approval::{read_box,read_readings,default_secrets,vendor_distrusts,forced_read}, probe_live::{never_listed,hidden_twin_within}	A Read box is approved iff it offers its folder grant and every reading (as written and canonical) is under a read root and no secret rule; the hidden-twin directory walk never lists inside another app's data or a file-provider domain (`APP_DATA_ROOTS`, `FILE_PROVIDER_ROOTS`).	1,3,5,7,8	4/4
approval-trust-dialog	contract	decide (trust arm), RULE_TRUST_DIALOG, RULE_TRUST_ANY, ApprovalCtx::trusted_cwd	Safe trusts a folder-trust dialog only for the session's own folder (its cwd known), its options exactly an exit and a trust, under a trust root (none by default) or as the folder its starter trusted (trusted_cwd equal to the cwd, adding no root): Claude Code's dialog either way, agy's and Muse's only in their worker's trusted folder (no trusted folder, one beside it, a moved session or a root alone escalates), the press the trust option, guarded on the folder's row; full power trusts a person's own dialog in any folder, unproven; aterm answers no Codex box.	1,3,4,12	6/7
approval-roots	contract	roots_from_config, own_claude_tmp, ScratchRoot::{holds,label}, ApprovalCtx::new, abs_components	roots_from_config expands ~ and a final-component glob, maps claude-* to this uid's own, drops unsafe specs; holds = component prefix; ApprovalCtx::new builds the default roots. ApprovalCtx::new takes no TMPDIR that holds too much (/tmp, /Users, a parent of the cwd's scratch folder) as a scratch root.	1,3,5,7	3/3
approval-whole-read	invariant	head_unread, tall_box, quoted_invocation, decide	A box not read whole is never pressed, except the tall-box decline for a single quoted removal in bypass; quoted_invocation needs exactly one backtick pair. The bypass tall-box decline under an rm breaker's `not ruled out — <why>` note gives a split into shorter commands, and for a why aterm has no reading of (2.1.309's `the shell may read this command differently than it was parsed`, an unknown rmdir why) a plainer split, never the substitution advice; a box whose head is off the screen and whose tail names no box is held.	1,3,4,5,12	5/5
approval-not-a-box	invariant	decide_screen, draws_claude_boxes, aterm_phase::read	Only a box the program's reader vouches for as a live prompt is judged; quoted, goal-text, pager-shown or footerless setup boxes press nothing; Safe judges only Claude's boxes; no Codex box is judged.	1,3,4,12	2/3
approval-owner-review-held	invariant	settings_ask, unvoiced_tools, forced_read, decide	Every level escalates a box the owner's ask rule or hook reaches (by any tool that can draw it), unreadable settings, or a Read box with no folder grant. No reading of the screen above the box (a call's message reaching off it, queued edit rows, an edit after a cut output) and no transcript candidate (box_call) narrows the tools that may have drawn a box below every possible one, so each possible tool's owner hold stands.	1,3,5,7,8,12	11/13
approval-guarded-press	invariant	decide, row_of, file_path_row, focus_steps, decline_step	Every press carries a guard matching exactly one row of its screen; focus presses move then Enter after a confirming read; each decline state gets one guarded keystroke.	1,3,5,7,12	2/2
approval-amend-input	contract	amend_input_open, offers_amend, refusal_of, decline	No digit into an open amend input (escalate); fixed refusal labels are no input; decline only with `Tab to amend` and sound options and an empty input.	1,3,5,12	3/3
approval-decline-text	contract	decline_text, squashed	DECLINE_PREFIX + what + ` was not run: ` + cut160(squashed(why)) + `. ` + fix + `.`: no control char, fix never cut.	1,3,7	1/1
approval-command-readings	contract	command_readings, box_width, wrap_margin, cells_are_chars, no_option_above_the_question, PromptV2::readings	Every possible joining of the command rows is judged; a row with room left cannot have wrapped; non-ASCII rows are never measured; barred descriptions and wrapped titles/footers are read whole.	1,3,5,7,12	8/8
approval-limit-dialog	contract	decide (limit arm), RULE_LIMIT_WAIT, RULE_LIMIT_RESET, aterm_phase::reset_row	The usage-limit dialog takes the grant row (limit_reset on: with limit_reset_kinds empty, the ONE option that spends a reset, of a kind the step takes — 5-hour, weekly, full — read whole, never a Fable row, none with two or more rows that spend a reset; with it listed, the row of the first listed kind on offer, read whole, never an unlisted kind or a Fable row, and none where that row does not read whole; under either none with a kind drawn twice, nor with a row that spends a reset and opens as no kind's row — a kind this build does not name, one whose name only begins with a known one's — wherever it stands) or else the wait row by its label at every level; with neither it escalates; stop/spend/upgrade rows never chosen.	1,3,5,12	6/6
approval-autocompact-offer	contract	decide (autocompact arm), autocompact_offer, RULE_AUTOCOMPACT_OFFER	Claude Code's autocompact offer, read whole by an authoritative Claude reader, is decided first at every level: with autocompact_offer = Some(k) on the ladder, Focus onto the accept row (Enter) iff its size = k, else FocusKey one stepper key toward k; never the keep row, Esc or a printable key; off, unread or off-ladder escalates.	1,3,5,12	3/3
approval-auto-mode-setup	contract	decide (auto-mode-setup arm), auto_mode_setup, RULE_AUTO_MODE_SETUP	Claude Code's auto-mode setup offer, read whole by an authoritative Claude reader, is decided first at every level: Digit(n) of its `Not now` (n = 2) under the title row's guard, subject `<title> => Not now`; never `Yes` (a minute-long scan nobody asked for) or `Don't show again` (a standing answer on the person's behalf); unread, another program's or unvouched escalates; a look-alike in other words is never this rule's, and at safe and none nothing presses it.	1,3,5,12	2/2
approval-dialog-rules	contract	decide (plan, question, consent, setup, session-paused, model-confirm, model-picker, goal-resume, mcp-server arms), decide_box, consents, credits_consent, credits_decline, credits_held, other_dialog, mcp_server_held	Each dialog is decided only by its own rule (least-power plan yes, questions by answer_questions, fallback switch, model confirms with the standard subtitle, goal resume only when owed), and every box of a program the supervisor does not host (Codex's, whose screen aterm reads no more, and the generic reader's) is escalated by decide_box before any rule; a consent is `Continue with` as a whole word and never an answer kept for good (by its role, its KEPT words, its whole label or its rows; its role Persist by `always` too where a rest under it that opens with no capital blanks its block's other roles) nor a label Claude Code cut short (`…`); a Claude Code consent onto USAGE CREDITS (credits_consent: its label, the box's title or the box's own rows, wrapped rows joined and frame glyphs dropped, name `usage credit` or `extra usage`) is taken (credits-consent@v1) only under `[harness] usage_credits` (ApprovalCtx::usage_credits, from the policy) and otherwise cancelled with Esc (credits_decline, credits-decline@v1: Choice::FocusKey { steps: 0, the focused label, esc }, guarded on the title row) where its footer reads `Esc to cancel` and its title row is on the screen, else escalated with the reason naming the key and the consent's kind (a model's kept for good per model and account; `Continue with usage credits` saving nothing and carrying the session onto credits already on), none of its rows pressed, while a consent naming no credits is taken (allow-once@v1) whatever the key (owner, 2026-10-19, superseding 2026-09-24's consent → accept); and `Continue without …` is a refusal (a setup dialog's decline@v1), never a consent: the Chrome setup panel (its options' descriptions drawn beside their labels) is declined with `Continue without browser tools` in every phase, or escalated where a narrow pane cuts its labels short, its connected phase's `Continue with browser tools` (kept for good) never taken; the Chrome install offer that opens that setup (`Claude wants to use your browser`, its descriptions beside its labels) is declined with `Not now` wherever its cursor is, never `Install extension` or `Don't ask again` (Persist), and escalated with no `Not now` (2.1.302 bundle: `not_now` saves nothing, only `dont_ask_again` writes `chromeInstallUpsellDismissed`, Esc saves nothing, and the offer is skipped while the session auto-allows tool calls and with no Chromium-family browser); Claude Code's MCP server dialog is escalated at every level (none, safe, all, bypass), and so is each frame of it a narrow pane wraps or cuts, read as one with the reason that every answer, Esc included, is saved for good in the project's .claude/settings.local.json and that `claude mcp reset-project-choices` undoes it, both kept in the loop's badge at none, safe and all, and nothing is pressed; Safe and None hand them over. A dialog Ink wraps (its head off the screen, a label wrapped at a name or inside a word) is answered by its own rule with the whole label; agy's first-run screens are pressed at no level and its survey is no box; a Muse question with a note open under the recommended answer, and Muse's several-question, multi-select and review boxes, are a person's.	1,3,4,5,12	16/16
approval-footer-mode	table-property	footer_mode	The footer's pill names its mode (manual by default); None when the box replaced the footer.	1,3,5	1/1
approval-breaker-naming	contract	rm_breaker_label, name_the_breaker, aterm_phase RmBreakerKind::NotRuledOut	name_the_breaker puts the breaker's distinct label once at the head (idempotent). rm_breaker_label of a `not ruled out — <why>` breaker aterm has no reading of (RmBreakerKind::NotRuledOut, 2.1.309) names the why in the vendor's words and that the command need not name rm; a refusal at safe power (warn and bypass too) starts with that label, names the breaker once and never the nested cause.	1,3	1/2
approval-trace-names-decider	contract	policy::approval::decide_traced, policy::trace::{Trace,NoTrace,TraceStep,Outcome,deciding}	decide_traced(reading, rows, ctx, t) returns the decision decide gives for every trace t (NoTrace, which keeps nothing, included); the steps it keeps name each check consulted, in order (a read-only git box: supervisable, owner-review, settings-ask, forced-read, head-unread, then the rule), and deciding(steps), the last step Decided or Overruled, is the check and rule that made the decision: the read-only rule under safe-rules; full-power, with safe-rules' outcome Unproven, for a write at full power; approve-none with no rule under approve = "none".	1,3,12	1/1
approval-git-read	contract	git_reads_clear, worker_writes, git_config::*, claude_settings env reader	Safe approves a git read only where the config git would load in the worker's environment, for every Bash-tool directory, runs no program, with no -c or remote, a readable environment and a git not in a worker-written directory.	1,3,5,7,8	5/5
report-mark-roundtrip	round-trip	report::{Mark::parse,Mark as Display,decimal}	decimal = an all-ASCII-digit u64; Mark::parse = ORIGIN:INDEX or INDEX of decimals, anything else None; parse(m.to_string()) = m.	1,3,5,7	1/1
report-parse-history	contract	report::{parse_history,parse_turn_line,newest_turn}, pct::decode	parse_history keeps `turn <id> …` lines with id, pct-decoded text, the last submitted=/arch= words; newest_turn = the last submitted turn, else the last.	1,3,5,7,12	1/1
report-parse-offscreen	contract	report::{parse_offscreen,header}	The header comes from stderr's OK line (aterm ctl framing) else stdout's first line; the payload count must match; screen_rows splits archived from screen; fields default as stated; both framings parse equal.	1,3,5,7,13	3/3
report-join	contract	report::join, ScreenReader::transcript_end	join(off, reader) = archived rows ++ the screen's rows to reader.transcript_end (Claude Code's: phase::transcript_end; the ledger joins by Claude Code's) not re-shown from the archive (a re-shown row the read missed is kept), with archived/first/upto.	1,3,5,7,12,13	3/3
transcript-end-positional	contract	aterm_phase::phase::transcript_end	The index of a busy status row, else just past the last transcript row above an idle composer (skipping hints, tips, surveys and blanks; code and done rows are transcript), else past the last non-blank row; never inside the composer.	1,3,5,7,12	5/5
report-find-start	contract	report::{find_start,is_user_row,user_text,paste_like,paste_fits}	is_user_row ⇔ `❯` + whitespace; find_start(rows, marker, text, caret) reads user rows as `caret` + whitespace (`❯`); Since ⇒ 0; UserRow ⇒ the last user row; Ledger ⇒ the last user row matching the first MARKER_CHARS of the text, else for a paste the last `[Pasted text` row only if paste_fits.	1,3,5,7	5/5
report-finish-assess	contract	report::{finish,assess,screen_only}	finish(…) trims blank ends and splits archived/screen; assess yields the reason set (NoArchive, MainScreen, ArchiveReset, ArchiveGap, MaxRows, MarkerNotFound) and a recheck index exactly as stated; complete ⇔ no reasons.	1,3,5,7	4/6
report-render	contract	report::Report::{header,render,render_view}, Marker::name, Reason::name	header and render have the fixed shapes (reasons in Reason order); render_view(All) = render(); other views add view= and kept= and render view_rows(rows, view).	1,3,5,7	0/3
report-gather-protocol	refinement	report::Session::{report,gather_report,ledger_turn,offscreen_snapshot,gap_after,full_screen}, without_verb, ScreenReader::transcript_end	Requests history 8 (unless since), then offscreen since=/tail=. An archive read is joined by Claude Code's grammar for every program (its caret and its transcript_end; aterm reads no Codex screen since its screen lane's retirement, 2026-10-16: the Codex main-screen report, the pinned-prompt drop and the missing-reply-head reason went with it); a worker off the alternate screen is MainScreen. Otherwise an old host falls back to one text read; a one-row gap recheck only when assess asks.	1,2,4,8,11	10/11
reset-step-refines-model	refinement	reset_loop::Session::{reset_take,reset_run,reset_confirmation,reset_end,reset_take_up,reset_passed,reset_gate}	For every composed reset world and key setting the run refines supervisor_limit_reset_model (reset_violations = []); typing the reset command is never enabled; every named mutant is refused.	1,2,3,4,7,8,10,11,13	3/3
reset-keys-safety	invariant	reset_loop::Session::{reset_take,reset_run,reset_confirmation,reset_shows}, policy::reset::confirm_verdict, policy::approval::kept_for_good, row_guard	ResetAsk/Go carry the row's kind, and a confirmation of another kind (or a Fable one, read Inexact('kind')) is kept. No send or turn; only arrows/Enter on the dialog, Enter on the confirmation, Esc on the confirmation or Checking, never on a box that keeps nothing of the confirmation and offers an answer kept for good by its role (Persist), its label (settles_later, kept_answer) or its rows (Opt::kept: the focused label read joined onto the unmarked option under it in a narrow pane is still kept; read again as defence in depth, the reader's role already Persist where it reads roles), nor on one whose head is off the screen with no Esc read (an MCP server dialog: read as one, every answer Persist; retitled, cut or read mid-move with its roles withheld, by its kept answers' labels, the one-server dialog's three and the several-server dialog's `Enable selected`, each read whole where a narrow pane wraps it; drawn only down to its server rows, by its unread head and Esc); every key fenced on the judged read; the grant row's Enter only where the read after the arrows still reads it as the row the rule takes (aterm_phase::reset_row under the limit_reset_kinds reset_take chose it by, the same label; press::press_focused); the confirmation's Enter only on the go, the settle and no person, at most RESET_ENTERS; no Enter after an Esc; skipped keys re-judged.	1,2,5,7,8,10,11	9/9
reset-host-dialogue	contract	reset_loop::Session::{reset_take,reset_end}, IdleHost::{reset_take,reset_told,inform}	The host's reset_take is asked once per mount and again before the Enter (not while held); Again repeats up to RESET_AGAIN_MAX; each taken step is told its end exactly once; inform at most once; no badge.	1,2,4,7,8,10,11,13	11/11
reset-books	contract	reset_loop::Session::{reset_take,reset_confirmation,reset_end}, reset_loop::outcome_words, approvals::Row::to_json	A step writes approved (taken), at most one yes or declined row, then exactly one ended row; journal RESET lines per stage, the end's said (a line, not only a note) for a spend — Spent, Walled — and worded by outcome_words (`used (<kindResets> left)`, `used (<kind>)` with no count, `used (<kindResets> left), the wall still stands`); a told end with no turn running owes exactly one continuation.	1,2,8,10,11	8/8
reset-successor-escape-only	invariant	reset_loop::Session::{reset_take_up,reset_run,reset_end}, approvals::reset_owed	With an open reset row and no go, every box the step takes (reset_shows: never one with nothing of the confirmation and an answer kept for good, or its head off the screen and no Esc read; a `Use a <kind>?` title with no box read, as 2.1.304's screen-reader layout draws, only while the step's Enter is unwritten, the Escape guarded on the title row) gets only Escapes after one person's grace, never an Enter or a reset_take, and any other box is the approval policy's; key off or stale ⇒ nothing asked or continued; an attended loop never takes a step up.	1,2,5,8,10,11	9/9
reset-gates-equivalence	equivalence	reset_loop::Session::{reset_gate,reset_passed}, policy::approval::decide	With any gate shut the requests equal those of a loop without the reset rule (one SKIPPED line); every gate open gives the same three keys at every level; the grant row's approval is ledgered under its own label.	1,2,5,8,10,11	3/3
reset-place	contract	reset_loop::Session::{reset_place,reset_day_start}, limit::{parse_reset,reset_at_with,days_from_civil}	reset_place is defined only for a dated clock and places it in the year nearest the named year (or now), in the held notice's zone or by the local offset at that instant.	1,2,3,5,7,13	1/1
box-key-normal-form	contract	run::{point_key,box_key,before_the_echo,file_request_key}, approval_loop::box_rows	box_key is kind, form, option numbers, normalised box text and the letters above the echo; redraws, rewraps, focus moves and countdowns do not change it; another command, diff, title or target does.	1,3,5,7,11,12	2/2
box-ask-refines-model	refinement	Session::{supervise_to,run_hosted,escalate,wait_for_next}, run::{point_key,box_anchor}	Every walk refines supervisor_box_ask_model: equal point keys ⇔ the same request; asks, EVENT lines and badges equal the model's one per request; CLEARED when an asked request ends; dial mutants refused. A box with no row under its header (a frame half drawn read as one row) has no anchor: its wait is on the content moving, never a panic.	1,2,4,7,8,10,11,12	4/4
box-settle	invariant	Session::{supervise_to,box_settle,agent_wake}	Every press or badge is decided on a read at least box_settle after the mount's first read, with an await seq between; redraws do not restart it.	2,7,8,10,11	3/3
question-text-refines-model	refinement	Session::{supervise_to,run_hosted,press_text}, policy::question::answer_question, approval_loop::question_key	Every run under a text priority refines supervisor_question_text_model: keys and sends enabled where they go, the send fenced and exactly the policy text, a person's key between read and send blocking it.	1,2,4,7,8,10,11,12	3/3
question-text-safety	invariant	Session::press_text, question memory, approval_loop::question_key	One fenced send per question key into a pristine field (re-sent only when provably unwritten); Enter/move-off only on a settled read-back of exactly the text; hand-over with no key on short, gone, untaken, unticked, changed or foreign text or an unexpected check; a person's skip yields.	1,2,5,7,8,10,11	18/18
question-priority-resolution	contract	policy::question (effective priority, falls_back), Session::{question_word,press_text}	The session word overrides the global priority; unknown words hand over; a word changed after the first key hands over; under a text priority unfenceable hosts, previews and tabs too short fall back to the recommended option (latched).	1,2,3,5,8,10,11	10/10
told-choice	contract	Session::{tell_choice,supervise_to,run_hosted}	CHOSE and its journal and story lines go together exactly once iff the loop's policy keyed every answer with no person after its first key and no hold, at the first busy read after the last key (or the settled composer), before a stop.	1,2,5,8,10,11	5/5
question-answer-refines-model	refinement	Session::{supervise_to,press_question}, policy::question::{answer_question,target_focus}, approval_loop::reask_pause	Under recommended every key refines supervisor_question_answer_model: fenced, guarded arrows seen landing, then Enter; never a digit or send; persons waited out without escalation; unfenced hosts hand over; untaken keys retried once (back-off unattended); third returns wait reask_pause; drafts untouched.	1,2,4,7,8,10,11,12	22/22
controller-answer	contract	Session::{answer,answer_digit,answer_focus,program_reader,press_one_guarded}, policy::question::target_focus, run::Fence	The question is read by the worker's own program's reader (program_reader). On a dialog read whole (Claude Code's), `answer t` moves the focus by guarded, fenced arrows to t's option and presses Enter; never a digit or over a person's text; recommended on a multi-select ticks then Next. On a question read by its answers' roles (Codex's), one digit under the title row's guard, fenced on the screen generation and never without it (press_one_guarded with Fence::Required: no `key if=`, no `if-gen=` in `help key`, no generation on the read, or the fenced key turned away by a usage line or a bare ERR ⇒ NOT-SERVED with nothing typed), never the last row's digit; a person within the grace ⇒ LEFT. Muse Code’s role-read single-select question uses guarded cursor moves, each seen landing, then Enter on the selected answer’s own row; every key requires the screen-generation fence and the initial read’s person-count fence, retained across generation retries. A missing initial person count or any read’s Unknown quiet stamp ⇒ NOT-SERVED before that attempt writes a key; Never or Ago at least the grace permits the attempt. A skipped generation replans the same question token at most three attempts (each may send several arrows before Enter). Admission refusals precede input; a later refusal may follow focus moves; human types nothing; another box token ⇒ REFUSED.	1,2,4,7,8,11	8/8
press-fence-and-guard	invariant	Session::press_one_guarded, run::Fence, policy::row_guard, aterm_observe::row_matcher	A guarded digit uses `key [if-gen] if=row_guard(row) k` with the gen fence learned once per session. Fence::WhereKnown (the window's own box press) retains the legacy downgrade after unknown syntax (the row guard alone, then the confirmed fallback, never unguarded while the survey is open); Fence::Required presses nothing without the fence: no `key if=`, no `if-gen=`, no generation on the read, or the fenced key turned away (a usage line, a bare ERR) ⇒ Unconfirmed, no retry, the fence never dropped. Changed skips re-decide (handing over after 5); a person's skip yields.	1,2,5,7,8,11,12	3/3
hold-and-refusal-parking	contract	Session::{press,park_on_hold,box_back_off}	ERR halted parks on status/inbox holds until hold=0 then re-decides from a fresh read; busy and rate refusals back off; none ends the loop or escalates.	1,2,7,8,11	3/3
loop-applies-decision	refinement	Session::{supervise_to,approval_ctx}, policy::approval::decide, config::from_aterm_toml, footer_mode, ApprovalCtx::set_trust_roots	For each settled box the loop builds ApprovalCtx from the config, cwd (OSC7 else agent_cwd) and footer mode, and writes exactly decide's keys (APPROVED) or escalates with its reason.	1,2,4,8,9,10,11	12/12
ledger-one-row-per-decision	invariant	Session::{ledger_decision,approval_ledger}, approvals::Row::to_json, SuperviseOpts::hosted	Each decision appends exactly one row with its rule, decision and subject; the hosted ledger is opts.ledger and nothing else.	1,8,11,12	3/3
session-group	contract	approval_loop::{session_group,roster_rows,unique_tab_for_group}	Ok((sid,g)) iff the roster parses, sid is the stable id (or the local id's row), its fgpgid g is unique and fits u32; a missing stable id is never replaced by the local one.	1,3,5,7	2/2
worker-env-binding	contract	Session::worker_env, WorkerSource::Session, caller_shell::process_args, policy::WorkerEnv	Boxes are judged in the environment of the session's foreground group (refusing one whose parent session id differs): its git config overrides, Claude config dir transcripts and shell snapshot options.	1,4,8,12	3/3
wake-protocol	contract	Session::{await_turn_from,agent_wake,wait_for_next}, footer_mode	A busy footer read waits on `await log close=turn,need,kind=agent since=<log_seq> ep=<log_ep>` (plus `base=<log_base>` when that `status` prints `log_base=`) from the head one `status` prints when that head still owes the wake (a turn open, no need open, the reader's word `busy` or `idle`; 6H-5), else on `await gone` (else seq + idle), and the screen read after either wait decides; the frame after gets the short settle first; an idle point waits on the agent verdict except where a content wait is needed; footer_mode names a non-default mode.	1,2,7,8,10,11	5/5
autopilot-word-edge	contract	Session::{park,word_latch,wait_on,word_moved,word_edge,wait_for_next,moved_past,park_while_off,turn_end_execute,log_wake}, AutopilotWord::{latch,reported}, Caps::{await_autopilot,await_log_word}, turn_end_loop::WORD_SETTLES	A wait at rest (`wait_for_next`'s anchored `await gone`, `await agent` and `await seq` under an unattended review, and the Off park's `await seq` and `await idle`) carries `autopilot=<w>`, w the stored word last read or last reported (`-` unset, never the table's), wherever `meta` names the word and the host has not refused the token; an act's own waits carry none. The waits inside the agent's turn carry it too (`Session::busy_wait`: `await_turn_from`'s idle and seq waits and `wait_gone`; and the log wait a turn a read showed running sleeps on, `Session::log_wake`'s `await log`, whose `OK autopilot` is `LogWoke::Word`, a usage refusal of the token there sending that wait again bare and no log wait carrying it again, `Caps::await_log_word` = false): a switch there is noted, wakes the wait at most BUSY_WORD_WAKES (4) times, each next try carrying the new word, and types nothing into the turn; the point a noted switch ends on decides on the word read then, so a switch from Land to Off or Cruise mid-turn types no instruction. The first wait that carries it learns the host's answer with no read of its own: `ERR usage` sends the same wait again bare, and if that is no usage error no wait carries the token again (`Caps::await_autopilot` = false; no `help` is asked); an `OK autopilot` naming a word this build cannot parse drops the token too. `OK autopilot <w'>` journals `NOTE seq=<n> autopilot=<w'> the session's Autopilot word moved: decided after a <ms> ms settle`, makes w' the word later waits carry, and sets word_at = now + WORD_SETTLE (1 s), set again by a switch inside it. Once word_at passes, the word read then decides the point through `turn_end_now` (Off is left to the park, an anchored box to the look), and an act a point decides before it is due at word_at (`WORD_SETTLES`); the Off park steps no further than word_at and unparks on any word but Off read once it passes. So writes of Land then Off inside the settle type nothing, a host that never answers is paced as before, and a switch made mid-turn is typed at the next point or one settle after it, never into the running turn.	1,2,3,7,8,11	7/7
stop-semantics	invariant	Session::{call,run_hosted,await_turn_from}	After a stop every input request is refused and no wait begins; the run ends `EXIT stopped` within one wait, its badge cleared and a due choice told first.	2,7,8,11,13	5/5
attention-ownership	invariant	Session::{reconcile,escalate,attention_text,unset_if_ours,finish_supervisor_attention,metadata_requires_proof,metadata_authority,retry_supervisor_attention}	At start a stale own badge is cleared or adopted; other owners and bare attention are untouched. A handed point creates one logical escalation intent, distinct from its last acknowledged entry: refused or uncertain SETs and CLEARs retain custody for bounded, paced metadata retries on an unchanged point. Managed writes verify an existing lease without renewing or acquiring one; missing proof defers, a successor or deliberate handback retires local custody, and handover leaves native badges for the successor. Explicitly unclaimed calls and hosts without lease support from the start retain ordinary legacy behavior, but gain no timer authority; once lease support is confirmed, refusal or release cannot downgrade custody to that fallback. Cleanup uses the current desired kind and text, preserves unrelated or unflagged adopted entries, and reports CLEARED only after an acknowledged take-down or an entry it keeps; pending cleanup reports SKIPPED.	1,2,8,10,11	3/3
survey-press-refines-model	refinement	Session::{dismiss_survey,survey_press,survey_backspace}	Every 0 and backspace is enabled in supervisor_survey_press_model at the server's state (survey_press_violations = []): no press before the settle, under a hand, over a draft or box; backspace only past landing; each mutant forbidden.	1,2,7,8,10,11,13	9/9
focus-choice-refines-model	refinement	Session::{press_focused,supervise_to}	Focus choices refine supervisor_focus_choice_model (NeverEnterOffTheChosenOption): fenced arrows, re-read, Enter only on a confirmed focus; never a digit or Esc on the trust dialog.	1,2,7,8,10,11	4/4
limit-dialog-wait-row	contract	policy::approval::decide (limit-wait@v1), Session::press_focused	Under limit_wait the usage-limit dialog's wait row is chosen by a confirmed focus move and fenced Enter at any level; without one it escalates.	1,2,4,8,10,11	4/4
autocompact-offer-loop	contract	policy::approval::decide (autocompact-offer@v1), Session::{auto_read,box_young,press_focused}	The offer is judged only after it has shown max(box_settle, AUTOCOMPACT_SETTLE); every arrow, step and Enter is Fenced(w, J) with if-human=n(J); Enter only on a fresh read showing the focus on the accept row at the target size; ERR halted parks; approve=none does not withhold it; autocompact_offer off badges it under its kind with nothing pressed.	1,2,4,8,10,11	4/4
auto-mode-setup-loop	contract	policy::approval::decide (auto-mode-setup@v1), Session::{auto_read,box_young,press_one_guarded,retries_presses,left_typed,offer_rose,probe_type}	The offer is judged only after it has shown max(box_settle, AUTOCOMPACT_SETTLE), since the vendor drops a key pressed within 500 ms of its mount; its digit is Fenced(w, J) with if-human=n(J) and the title row's guard; a person's input inside the choice grace holds it; approve=none does not withhold it; an unattended press that does not land is tried again; the answer is a ledger row; a continuation the offer rose over raises no Ask, and once the offer is gone the turn's end is decided again; the accounts probe's `/usage` the offer took (no `/usage` in the composer under it) is no `/usage was typed and not sent` note, and the probe is tried again.	1,2,4,8,10,11	6/6
tail-reread-equivalence	equivalence	Session::screen, run::{tail_misses_the_live_rows,drawn_above_tail}	Decisions on `text --json tail=40` equal those on the whole screen cut to its live zone for every cut and cursor; a box cut by the pane itself is never pressed.	1,2,5,7,8,10,11,12	9/9
press-landing-and-retry	contract	Session::{press,box_back_off}, ladder::doubling	A press has landed iff a later read shows the box gone or the worker busy; unattended full power re-presses on the doubling back-off (badged once); Safe hands over after one; default-No ticking boxes get one fenced Enter; full power has no repeat cap.	1,2,7,8,10,11	5/5
reader-by-program	contract	Session::{program,reader}, aterm_phase::identify	The box is read by the program status names (else identify); a non-agent never gets a press.	1,8,11,12	2/2
claim-refines-model	refinement	Session::{claim,renew_claim,release_claim}, SuperviseOpts::yield_when_held	Unattended loops refine supervisor_claim_model (NoPressUnderAnothersClaim): a renewed lease before every press, watching only behind another's claim, the hosted loop yielding, a claimless host supervised as before.	1,2,7,8,10,11	5/5
decline-keys-refines-model	refinement	Session::press_decline, policy::decline_text, decline_box, PromptV2::foot_note	press_decline writes down, tab, the reason, enter, one fenced guarded key per fresh read, refining supervisor_decline_keys_model; stale, unconfirmed, swapped or person-held boxes stop it as stated; nothing is resent.	1,2,7,8,10,11	12/12
host-turn-end-protocol	refinement	Session::host_step, IdleHost::{at_idle,turn_ran,usage_tap,busy_tap,taskless}, HostStep::{typed,repeat}, TurnEndState	Continuations refine supervisor_host_turn_end_model: a released turn end is continued once; the host's step is taken in place; turn_ran only after a busy read; usage_tap at a decided idle point, busy_tap at a busy read; a host-typed turn is not the worker's; taskless sessions get nothing; limit episodes are told.	1,2,4,8,10,11,13	11/11
idle-point-classification	contract	Session::idle_point, IdleHost::{at_background,withheld}, run::background_settle	An idle point is a composer up with no wall and none of the listed exclusions (login reply, background break before its settle, half-drawn REPLs); withheld points are said once by guard; breaks after own acts follow supervisor_break_offer_model.	1,2,5,8,10,11,12	5/5
limit-notice-reset-placement	contract	IdleHost::limit_until, Session::reset_unix_for, limit::{parse_reset,reset_at_with}	A limit notice is an idle point once reset + grace passes (episode or transcript reset); a no-reset limit is never spent by an earlier notice; bare clocks are placed on the transcript's or done row's day.	1,2,3,5,8,11	4/4
exception-lifecycle	contract	Session::exception, IdleHost::exception	A no-reset spend limit is journaled once as an exception and told once; cleared at the first live turn (background work alone does not); `later` raises nothing.	1,2,8,11	2/2
memory-ceiling-decision	table-property	Session::memory_point, IdleHost::{restart,warn}	The memory action is a total function of (footprint vs ceiling, Mac state, largest, host answer, banner): restart, warning once, escalation or continue; a change of state decides the standing point again; refused restarts keep the person's row until a later adopted one.	1,2,4,8,10,11	11/11
model-bucket-and-fallback-timing	contract	Session::model_bucket, IdleHost::relaunch, TurnEndTiming::{restart_backoff,reset_jitter}	Bucket fallbacks and returns are host relaunches (never /model) with the decision's verdict kept; a deferred fallback is retried on the last backoff or at the reset + grace + jitter, the continuation first; a deferred return never holds the point; a successor waits out its predecessor's back-off.	1,2,4,7,8,11	8/8
usage-probe-refines-model	refinement	turn_end_loop::Session::{usage_probe,probe_gate}	Every /usage, Escape and sample is enabled in supervisor_usage_probe_model (usage_probe_violations = []), each mutant caught; the gate reads the point as decided.	1,2,7,8,10,11	2/2
human-input-stamp	contract	screen::HumanInput::{of_status,within,ago,quiet_since}	of_status reads human_ms= on the first OK line: `-` Never, a u64 Ago, else Unknown; within/ago/quiet_since are the stated comparisons (None for Unknown).	1,3,5,7	1/1
parse-text-json	contract	screen::parse_text_json, ScreenGen::parse	Ok iff (after an optional OK 1) the body is a JSON object with an all-string `rows` array; fields default and saturate as stated; unknown keys ignored; `launch` is read only as a whole `person` witness (id, seq, word), else none; the server's writer round-trips.	1,3,5,7,12	5/5
json-reader-grammar	contract	screen::{Json::parse,parse_value,parse_string,hex4,…,Json::{get,as_str,as_u64,as_array}}	Json::parse accepts exactly RFC 8259 (numbers as lexemes, surrogate pairs combined, invalid code points U+FFFD) and never panics; get returns the first key; the accessors project.	1,3,5,7,14	1/1
screen-cursor-index	contract	screen::Screen::cursor_index	Some(cursor_row - first) iff it lies in rows, else None.	1,2	1/1
stall-status-read	contract	stall::{worker_stall,publishes_input}, escalate::status_field	worker_stall = Some iff input= is stalled or stopped, with bytes, wait and rss parsed; publishes_input ⇔ an input= field.	1,3,7	1/1
wait-text-format	contract	stall::{wait_text,rss_text}	wait_text in s / m ss s / h mm m / d h; rss_text in MB below 1024 else one-decimal GB (needs an overflow bound on mb).	1,3,6	1/1
frozen-text	contract	stall::{frozen_warning_text,stall_facts}, resume::{restart_command,shorter}	The warning names the program, the wait, backlog and size, then the remedy by AfterRestart: it is the two lines "{program} frozen: not reading input" ++ stall_facts(stall) and the remedy, joined by "\n". stall_facts = " for {wait_text(wait_ms)}" iff the wait was measured, then " ({n} B queued, {rss_text(mb)})" of the parts measured, in that order, with no parentheses when neither was. The remedy starts "aterm leaves it running — ": Relaunch ⇒ "`{restart_command(Some(sid or <sid>), pid)}` ends it, and {RELAUNCH_WORDS}"; Act ⇒ "`{that command}` restarts it"; Unknown ⇒ "`aterm ctl @{sid without its leading '@'s, or <sid>} signal term` ends it (`signal kill` if it survives)"; Command(line) ⇒ that, then ", then `{l}` resumes it" for the first l of [line] ++ shorter(line) whose clause keeps the remedy within NOTE_LINE_CHARS characters, or nothing when none fits.	1,3,4,7	1/3
relay-render	contract	transport::{render,is_timeout,lossy,client_failure}	render reproduces aterm-ctl's printing (ERR to stderr code 1; timeouts 124; framing-specific stdout/stderr notes, `search`'s ` incomplete` note by aterm_ctl::search_incomplete_note), so every CtlReply reader returns the same verdict.	1,3,7,11,12	1/1
relay-connection	contract	transport::{RelayCtl::call,call_at_host_of,preferring,exchange,conn_for,endpoint_conn,endpoint_sock,dial,hold,drop_conn,connect,io_words}	A connection carries only requests one instance answers: a request naming a session (aterm_ctl::target_sid) rides the connection of the instance its graph entry names (aterm_ctl::hosting_sock), dialled with the token beside that socket; when no entry names one or its connect finds nothing listening (ENOENT, ECONNREFUSED), the endpoint's connection of that session's own; a request aimed at a session's host (call_at_host_of) rides the host's connection, else that session's relayed one, else the endpoint's; every other request rides the endpoint's own connection, the only one the given token is presented on. A session's route stands while its connection lives. A mid-call I/O error is a lost reply that drops that connection and its routes, and the next call dials again (re-reading the graph entry); connect failures are lost; unsupported framings fail without I/O. A transport built by preferring(endpoint, token) presents that token on the endpoint's socket; a reply `ERR auth` on a connection that presented it drops the connection and the token for good, and the request is sent once more on a connection dialled with the token beside the socket, which every later request uses, so at most one request per transport is sent twice. A refusal on any other connection, or on a transport built by new, is the reply.	2,8,13	6/6
relay-interrupter	contract	transport::RelayCtl::{interrupter,call,exchange}, hangup::hang_up	After the cut every call Errs; a parked read returns early; every connection held is hung up, both halves each, so a half-closed peer sees EOF.	8,13	3/3
reply-wait	contract	transport::reply_wait	The first `timeout N` / `timeout=N` in args plus AWAIT_MARGIN, else DEFAULT_REPLY_WAIT.	1,3,5,7	1/1
ctl-reply-readers	contract	run::CtlReply::{ok,timed_out,err_text,bare_err,usage_error,unknown_form,is_err,seq,skipped,turn_verdict,submitted,warned_input_unread}	Each reader is the stated projection of code, stderr and stdout (seq digits after seq=, skipped on OK skipped, the verdict line carrying submitted=, warn=input-unread off that line).	1,2,3,7	2/2
ctl-reply-lost	contract	run::{CtlReply::lost,unserved,client_lines,turned_away,unsent,cut}, Session::unserved	lost ⇔ not ok and (exit 75, or the first ERR line turned away by a whole phrase, or no ERR and a client unsent/cut line); unserved adds `no such session` during an outage; server ERRs are never lost.	1,3,5,7,12	2/2
line-clip-and-exit-reason	contract	run::{clip,exit_reason,one_line}, Session::ride_out	clip = the first 160 chars with controls spaced; exit_reason wraps exited/no-such-session as `session gone (…)`; RECONNECT lines are clipped.	1,3,7	2/2
approval-line-cells	contract	run::{approval_line,cut_cells}, breaks_a_line, char_width, prompt_box_first_row	cut_cells spaces line breakers, trims, and keeps the longest prefix within cap cells; approval_line applies it to the box's first row at APPROVAL_LINE_CELLS.	1,3,7,12	1/1
typed-draft	contract	run::{typed_draft,typed_draft_in,stray_digit}, ScreenReader::composer	A draft is any later composer line, or a non-empty first line unless it is the homed placeholder on the caret row; stray_digit ⇔ no prompt and the composer holds exactly the digit.	1,3,7,11,12	1/1
busy-up	contract	Session::busy_up, row_matcher, RowMatch::clears	busy_up ⇔ some row is not cleared by the guard's matcher (an exhausted scan counts as up); false for an invalid guard; cached consistently.	1,2,12,13	1/1
context-watch	invariant	Session::{watch_context,await_turn_from,moved_under_verdict}, run::indicator_gone, aterm_phase::context_left, harness::footer::context_gone	A machine, run on every screen the loop reads in a turn and at rest, Armed → Warned at ≤ context_warn (one EVENT context) → Armed on a 30-point rise or Claude Code's indicator gone by the footer's rule (footer::context_gone: the frame standing, Claude Code's own footer row right under its bottom rule with nothing against the rule's right edge, no reading, no box) (one EVENT compacted); a screen with no reading is not enough: a menu, the shortcuts or a notice in the indicator's slot is never a compaction; warn 0 says nothing; the reading is context_left's, the one the window and `aterm drive phase` take, under the ❯ composer's frame or shell mode's `!` one: the indicator ending a footer row that reaches the rule's right edge (every build measured, 2.1.281 to 2.1.310), else right-aligned above the composer's top rule (2.1.267/2.1.268); the read at rest watches a newer screen another read left in `last` before it waits.	1,2,3,7,12,13	8/8
render-phase-grammar	contract	run::{render_phase,render_phase_and_survey,phase_prompt,esc_line,render_prompt,busy_rule,event_line,survey_event_line}	render_phase(turn, reader, allow) prints the phase word then its fields in reader's grammar: busy reason (Claude Code's busy_signal; Codex's background-terminal line, status row or a last message with no end row); limit message/reset; prompt kind/command/description/notes/classify/options (Claude Code's parse_prompt, any other reader's PromptV2; classify, and event_line's classify=, is the read-only rule's verdict over every reading of a Bash box's rows, readings_read_only, never the classifier's word on the joined command; a Bash box parse_prompt reads but whose PromptV2 rows were not read whole has no verdict and prints `classify not-read-only the box's command was not read whole`, never read-only), then the Esc line (reader.esc) — `cancel esc`, `exit esc` where Esc quits the program (a trust dialog's: Claude Code's, Codex's, Muse Code's, and agy's undrawn one), or `interrupt esc` where the box's footer says Esc interrupts (Codex's question), no line where nothing says — then a question's box token; survey and context lines follow (reader.survey_block, reader.context); event lines summarise the worker's words, never a survey's.	1,3,7,12	4/4
await-turn-wait-protocol	contract	Session::{await_turn,await_turn_to,await_turn_from,wait_for_next,screen}, live_rows, caps	The first wait is a capped `await gone`; capabilities are learned once, except that `Caps::log` follows each OK `status` (the first busy wait reads one while it is unknown) and `Caps::await_log` latches false on a refused `await log`, and a reconnect starts both over; reads are tail=40 (whole when the cursor is above); busy waits are scoped to the live zone; frameless screens are never idle until an idle await latches; timeouts are steps; only served non-timeout errors Err.	1,2,3,4,5,7,8,11,12	10/10
budget-bound	invariant	Session::{watch,supervise_to,await_turn_to,drive,ride_out}	No write and no EVENT after start + max; the run ends TIMEOUT (code 124) with only the badge unset after it; confirmed approvals stand.	2,4,7,8,11	3/3
approval-press-safety	invariant	Session::{drive,press_guarded,approve_or_hand}, approval_loop, prompt::parse_prompt, classify	A 1 is written only if the policy approves (status, meta, help key first), and only as `key if-human=<n> if-gen=<g> if=<row>` on a read carrying the person count, a quiet stamp and the generation (no unguarded fallback); the guarded press counts only on an OK not skipped; after a press the box must be seen gone; one command at most twice between review points.	1,2,3,4,5,7,8,11,12	8/8
stray-digit-cleanup	invariant	Session::{clear_stray,survey}, run::{stray_digit,typed_draft_in}	A stray 1 or 0 of ours is backspaced exactly once with one note when no person or hand is on the session, never counted; a refused backspace stays owed until taken.	1,2,3,7,8,11	2/2
survey-press-gate	invariant	Session::{survey,dismiss_survey}, survey_block, SurveyBlock::withheld, run::survey_open	A guarded 0 only with dismissal on, a non-withheld card, a guard-capable host, no box and no person or hand; one per appearance from busy reads, the idle look retrying on its ladder; DISMISSED iff a later read shows the card gone with no 0 left.	1,2,3,5,7,8,11,12	14/14
survey-guard-unique	contract	aterm_phase::survey_block, run::survey_open, row_matcher	Every card kind's guard matches exactly its question row and no copy (quoted, EVENT line, help text).	1,3,5,7,12	2/2
survey-report-once	invariant	Session::survey, run::survey_event_line	An unpressed card's EVENT survey line is printed exactly once per appearance (a busy spell starts a new one), before the point's EVENT, never under a box.	1,2,7,11,12	2/2
survey-host-told	invariant	Session::survey, IdleHost::survey_stuck	The host is told Withheld once per appearance at an idle look, Unclosed once from the second failed 0, None once when the card leaves (or once at the first look with nothing); no badge for a card.	1,2,7,11,13	3/3
survey-zero-timing	invariant	Session::{survey,set_survey_settle,set_survey_land}	The first 0 waits SURVEY_SETTLE from the card's first sight; a lone 0 younger than SURVEY_LAND is waited on, older is backspaced once; a card taking the 0 inside the landing time is never backspaced.	2,7,8,10,11,13	3/3
parked-card-keeps-busy	contract	aterm_phase::worker_phase, parked_span, Session::drive	Any card parked above a busy composer leaves the phase Busy; with dismissal off no EVENT idle or key.	1,3,5,7,12	1/1
review-point-once	invariant	Session::{drive,look,wait_for_next}, Looking, Saw	An EVENT is printed exactly when the point's id differs from the last handed one, a busy read came since, or an unserved outage intervened; at most one per point; the next wait is seq- or gone-based.	1,2,3,5,7,8,11,12	7/7
outage-ride-out	invariant	Session::{ride_out,note_exchange,metadata_authority,unserved,set_reconnect}, Outage, CtlClient::call	An unserved reply or proof-read transport loss begins one outage (one RECONNECT line), probed by reads on a doubling pause until served (RECONNECTED) or the window lapses (EXIT, code 1; badge cleanup still requires custody proof). Relapses share the window until a non-probing served request of the failed kind or a successful await seq ends it. Raw metadata proofs use the same canonical meta kind and bookkeeping without claim renewal, so successful proof reads close their outage even when no await seq succeeds, and a later loss receives a new recovery window. Nontransport refusals follow their caller's handling and do not create an outage.	1,2,4,7,8,11,13	10/10
supervise-result-and-log-split	contract	Session::supervise_to, render_result	supervise returns the rendered point (or TIMEOUT) with no event lines, which go to the log; it never badges, posts, types or reads a report.	1,2,7,11	3/3
journal-mirrors-output	invariant	journal::{Journal::open,append,read_journal}, Session::{watch_to,supervise_to}	The journal (append, 0600) holds exactly the printed lines plus journal-only kinds, in order; supervise journals what watch would have printed; a failing journal warns once and changes nothing else.	1,2,7,8,11	3/3
escalation-badge	invariant	Session::{escalate,write_attention_target,unset_if_ours,retry_supervisor_attention,finish_supervisor_attention,waits_out,wait_for_next}, escalate::refused_cap, run::fit_bytes	Each logical escalation is journaled ESCALATED with its words and published as `meta set attention owner=supervisor`; a tighter cap permits one recut per attempt. Only final OK acknowledges publication or cleanup; refused or lost replies retain intent after box, limit or turn-end flags are consumed. A future metadata deadline retries without policy, model, input or claim actions, while existing policy and look deadlines win; fresh Off converts publication to cleanup. Owner-scoped cleanup is requested when the box goes, the episode closes, Off parks or the watcher exits, subject to custody and kind checks. A limit's badge names its kind after the owner: `kind=wait` exactly when the limit is waited out with no one's act — Claude Code continues by itself (`limit::resumes_by_itself`, whatever `resume_limits` says), or `resume_limits` is on and the notice names a reset the loop read within RESET_HORIZON — and `kind=ask` otherwise, decided (Session::waits_out) before the badge goes up; a box's, a question's and a turn end's badge names none (an ask to every reader).	1,2,3,4,7,8,11,12	7/7
limit-extend	contract	Session::extend_for_limit, limit::{parse_reset,reset_instant}	One EXTEND per episode moves the deadline to reset + 600 s iff the reset is ahead, within a week and later than the deadline.	1,3,4,7,8	2/2
limit-episode	invariant	turn_end_loop::*, Session::look, limit::*, composer_guard, TurnEndTiming	Limited points form episodes closed by work, an answered continuation or (self-resuming) a busy read; each escalated at most once unless handled; exactly one guarded continuation per look when the reset passed or the notice left, with a frame, no draft, status first and the backoff run.	1,2,3,4,7,8,10,11,12	12/12
host-look-wake	invariant	Session::wait_for_next, IdleHost::{wants_at,wants,at_idle}	Idle waits never outlast the host's wants_at; past it the loop steps the host; the run's writes equal those with no host.	2,7,8,11,13	2/2
stall-hold	invariant	stall::Session::{note_status,stall_in_hand,stall_before_wait,withhold_badge,stall_probe,hold_for_stall,thaw,stall_step,stall_look}	While a stall is in hand no write or new badge goes; holding it withdraws our badge, journals FROZEN once and tells a host it holds the stall; thawing journals THAWED, tells the host the stall lifted and decides the point again; input-unread warnings read status before the next act.	1,2,4,7,8,11,13	8/8
stall-probe-cost	invariant	stall::Session::{stall_step,withhold_badge,note_status}, Caps::input	On a host that never measures input the requests are those without stall handling plus at most one status per connection; unknown measuring probes once after a wait runs out.	1,2,5,7,8,11	3/3
stall-remedy-refines-model	refinement	stall::Session::{stall_remedy,warn_stall,stall_signal_term,after_restart}, stall::refusal_lifts, beside::Session::beside_self_claim	Under stall_term the restart request is sent iff supervisor_stall_remedy_model enables Term (once per episode, never under another's hand, beside a proven self-claim on the stall alone, the agent's record never read for its rest); otherwise one STALL-WARNED and inform, under the same gate; refusals decide as stated (restart unavailable: halted|driver-lease|driver-turn uncounted); never signal kill.	2,4,7,8,10,11	4/4
beside-memory-remedy	contract	beside::Session::beside_memory_remedy, run::Session::memory_edge, harness::self_claimed::{idle_point,read_idle}	Beside a proven self-claim the loop decides no point at a memory measure: a measure that MemoryOver::acts, with relaunch on, a host that relaunches, unattended, no stall in hand and IdleHost::beside_idle true at this step and at the step before it on the same screen seq (the idle point stood), sends exactly `restart pid=<leader> quiet=<grace>` once per agent process; anything else sends nothing and asks again at the next step; a refusal for another hand (busy, halted, restart unavailable: halted|driver-lease|driver-turn) is uncounted; a measure no longer acted on ends the count. idle_point = status idle ∧ draftEmpty true ∧ no exit; read_idle is false unless the identity is live before and after the read and the record names that pid.	2,4,7	3/3
beside-remedies-under-hold	contract	beside::Session::beside_memory_remedy, stall::Session::stall_remedy	Beside a proven self-claim under a hold the loop claims and types nothing; the stall's explicit restart (on the stall alone) and the memory ceiling's (at the record's rest) are asked, a refusal the next read may lift (ERR halted, restart unavailable: driver-lease|driver-turn) is uncounted and asked again, one it cannot (restart unavailable: not-relaunchable) is counted once.	2,4,7	1/1
self-claimed-relaunch-route	contract	relaunch::{after_host_ended,after_exit_as,self_claimed_line}, upgrade_drive::self_claimed_relaunch	A Codex snapshot that carries a self-claimed validated line (CodexRun::self_claimed) is relaunched by the crash rule's relaunch on that line, its witness ledger read (a handled witness is ended:handled), on the exit's cause, except an exit of its own (ExitCause::Exit), which is the Codex lane's as is one with no line (an orderly end is never typed back).	2,4	1/1
composer-guard	contract	turn_end_loop::{composer_guard,capped_rules}, policy::row_guard, row_matcher	composer_guard(c,t) matches the last row of every word wrap of `c t` (and an NBSP caret); for long texts every matched row starts with the caret or two spaces, never a shell's row; capped_rules cuts at a space to RULES_CAP plus `…`.	1,3,5,7	2/2
refusal-news-episode	contract	lifecycle::{refusal_key,Session::refusal_news}, Session::{call,park_on_hold}	refusal_key drops measure tokens and keeps holders; refusal_news is false only for the same key within REFUSAL_QUIET; a taken write or a lifted hold ends the episode, so one REFUSED/HALTED line per episode while every retry is sent.	1,2,3,5,8	4/4
resume-jitter	contract	turn_end_loop::resume_jitter, Session::turn_end_reading	resume_jitter is a pure FNV-1a of (sid, reset) mod span, < span (0 for span 0); the reading uses it only for a reset-lifted wall with a session id.	1,3,7	1/1
clock-unix-roundtrip	round-trip	turn_end_loop::nearest_s, Session::{instant_of,instant_of_ms,unix_of,unix_ms_of,stamp_in,carried_model_switch,wall_reset_at}	At any clock phase unix_of(instant_of(u)) = u and stamps read back as their second; ledger times carried by the switch and wind-down rows read back exactly; a wall's reset is cached as unix seconds and follows wall-clock jumps.	1,3,8	3/3
transcript-ends-on	contract	wall_confirm::{transcript_ends_on,row_of,user_entry,local_command,command_notice,opens_the_conversation}	transcript_ends_on = the last conversation entry in file order, skipping sidechain, meta, compaction, local-command and torn rows; notices are Notice entries; a parentless first prompt after a notice does not end it.	1,3,7	3/3
transcript-confirms-class	contract	wall_confirm::{confirms,class,error_class}, classify_wall	confirms(k,e) ⇔ e's last entry is a notice whose class (by the wall table, else the error word) equals k's; tool output confirms nothing.	1,3	3/3
transcript-lagging	contract	wall_confirm::{TranscriptEnd::lagging,transcript_ends_on}	lagging ⇔ no turn_duration closed the last turn and its last entry is the prompt, a tool call, a tool result or nothing.	1,3	1/1
wall-confirm-gate	contract	wall_confirm::Session::{confirm_wall,under_call_on_screen}, Session::turn_end_reading, LAG_TRIES	A Claude wall is acted on iff the transcript confirms its class (re-read while lagging), or with no transcript iff no tool call is over it on the tail and on a whole-grid read of the same wall; a refused wall's act is not taken, journaled once per episode; decision reads re-ask the transcript, books reads reuse it.	1,2,3,5,7,8,11	16/16
turn-end-decision	contract	policy::turn_end::{decide_turn_end,says_done,clean_defense,open_work_suffix,repo_work_suffix,own_repo_work_suffix,reply_mark}, policy::needs, TurnEndState::{observe,acted,repeats,restarted,weighs_tools,reads_turn_whole}, ScreenReader::{turn_ran_tools,reads_tool_rows,turn_opens_above}, Session::turn_end_reading, upgrade::transcript_tasked, QuestionPriority::prose_rule_id	At an idle turn end under Cruise or Land (never under Off, never into a taskless session), no current prompt asks for the token, and a done phrase, an idle report, a legacy ATERM-TASK-DONE line or a `Nothing left:` defense is a done claim (says_done) that ends nothing by its words. CRUISE ends no task: a done claim ⇒ the defend-and-find-more prompt (continue-done-check@v1), suffixed with the open work the claim's own words name (open_work_suffix) and the work the host's probe finds in the session repository that no remote main has (TurnEndReading::repo, own_repo_work_suffix: land what of it the agent made and leave the rest exactly as it is, since the probe reads other sessions' stashes and worktrees too; Land's nudge asks for all of it, repo_work_suffix); otherwise keep going under continue@v1, at most once per point. LAND: the episode's first free point ⇒ the land instruction (land@v1); every free point after it ⇒ the land nudge (land-nudge@v1), with the same suffixes and no count of its own (only the same-reply hold below stops it); a question ⇒ the land answer (land-answer@v1); the task is landed (done, nothing typed until someone else's turn) only at a land act's yield, a wall's retry of one included, whose reply is a clean defense (clean_defense: a `Nothing left:` line, no NEEDS YOU line, no open work in Land's strict reading) over a repository the probe finds nothing in. In both modes the short streak's back-off paces every act (a done claim lengthens it; a managed worker's first MANAGED_SHORT_TURNS go at once; a turn of real work ends the streak whatever act it answered, a wall's taken retry included). THE SAME-REPLY HOLD (TurnEndState::repeats) covers EVERY act at a turn end: a continuation, a Land nudge, a wall's retry, a limit's resumption, a command's owed continuation and a reset's own continuation. At a point whose reply came again nothing is typed and nothing waited for. A reply (reply_mark) is the turn's earlier and last words, or a read wall's kind and message, each word with a digit in it one number (a countdown, a clock with its `AM` or `PM`, an error id); words no reader reads are one reply; words cut at the screen's top are read whole from the history at every point while a reply is kept (reads_turn_whole), so a later point compares them as its answer read them. The streak keeps the newest REPLIES_KEPT replies (TurnEndState::replies) that acts' answers seen taken (a busy spell, a submit, a redraw) settled to, each with its answers and, at a read wall, its tries: the answers to the wall's own acts (its retry or resumption, a command's owed continuation), never the act's whose answer first met it. An answer counts when it ran no tool (TurnEndReading::ran_tools: turn_ran_tools of the readers whose reads_tool_rows is true, none read for the others; where the turn's opening message is above the screen (turn_opens_above) at a point where it weighs (weighs_tools: an act's answer read, or a reply kept), the loop reads the whole turn from the history above it, or the history to its start (all of the turn the terminal holds), and judges that, and where it reads neither the answer may have run one and is work. Claude Code: a call's `⏺ Tool(…)` row, or one drawn with no `(…)` and its `⎿` output under it (TaskStop's `⏺ Stop Task`, an edit of the plan file's `⏺ Updated plan`), or a collapsed group's `⏺ ` or two-space row read by every clause of 2.1.268's group renderer (clauses joined by `, `, the first capitalised), the first the renderer's and any one a tool's, before the expand hint: `Thought for <time>` (no tool), and the tool clauses edited or editing N files (an `+a -r` after), made N edits, committed, amended commit or cherry-picked <shas>, pushed to <refs>, merged or rebased onto <ref>, a PR or MR operation, one of the renderer's verbs (created, edited, merged, commented on, closed, reopened, marked ready, marked draft, enabled or disabled auto-merge on) before `PR #n` or `MR !n` (`Created PR #12`, never `Waiting on PR #12`), published, searched for N patterns, read N files, listed N directories, REPL'd N times, called <servers>[ N times], ran N agents or `ran agent · <description>`, a lone agent's description whose first word 2.1.268's verb rules conjugate (its tables and rules, UWt: `Reviewed the diff`, `Reviewing the diff…`, `Re-ran the suite`) in a row of at most 500 characters and no count after that word (a hooks summary's `Ran 2 stop hooks` is none), called N tools, ran N shell commands, recalled or wrote N memories, searched memories, the counted and bare clauses each in its -ing form too (`Reading`, `Searching`, `Listing`, `Running`, `Editing`, `Making`, `REPL'ing`, `Calling`, `Recalling`, `Writing`, `Publishing`, `Thinking for`), the git and PR clauses past only, as the binary draws them; with no hint every clause the renderer's and each count's noun a file, pattern, directory, shell command, agent, tool or memory, singular or plural (the git clauses as wait_bg17.out drew them, `  Pushed to main, rebased onto origin/main`), a two-space one standing alone under a blank row or the screen's top (under another group it adds nothing), never a reply's next row nor a row a `⏺ ` or two-space line above wraps onto (that line's, read once from its first row, so the read is linear in the turn's rows), and one whose tool clauses are git or PR ones alone only by what a sentence lacks, wherever it stands: a commit's sha of 7 to 40 hex digits, a ref with a `/`, a PR or MR number after the renderer's verb, or a second clause (a reply's closing `  Committed abc1234, pushed to main` is work; a lone `  Pushed to main`, `  Merged cleanly` or `  Waiting on PR #12` is words); a stop hook's summary and `Ran N PreToolUse hooks` are none, and so is ExitPlanMode's `⏺ Exited plan mode`, a plan handed to a person; or a background task's notice of its end, which opens the program's own turn (task_notice, each end 2.1.268's notice builders draw: `⏺ Background command "…"` completed[ (exit code N)], failed[ with exit code N] or was stopped[ because the system is running low on memory]; `⏺ Monitor "…"` ended without producing output[ (exit N)], stream ended, script failed[ (exit N)] or stopped; `⏺ Agent "…"` finished, is waiting on its own background work, failed: <error>, was stopped[ by Claude| by user] or stopped at its N-turn limit (…); `⏺ Dynamic workflow "…"` completed, failed: <error> or was stopped; `⏺ Remote task "…"` completed successfully, failed, is blocked[: …] or was stopped; then the row's ` · <time>` and ` · delivered to Claude as a <Tool> result (N chars)` parts, the description wrapped or not), never a Monitor's event (`⏺ Monitor event: …`, its monitor running on). agy: a `●` or `○` row that is no reply and no `Thought for` row. Muse Code: a `◆ ` tool head (tool_row), or one ran_row reads (drawn with the ` · ctrl+o` expander, a Monitor or Stopped terminal row with its outcome, a Scheduled cron or Unscheduled id row), or a two-space Read or Searched row) and was busy under min_work. One that ran none counts however long it was busy at a read wall, for every reader whose walls are read, Grok's and Kimi's whose tool rows are unread included (the unit test a_long_answer_at_a_read_wall_is_no_work_whether_or_not_its_reader_reads_tool_rows binds it), and, where tool rows are read, when it says a kept reply again: it is no work (the program's own retries, Muse Code's provider attempts) and neither ends the streak nor restarts the wall's ladder; elsewhere a long answer is work, and where tool rows are read, in new words, it is kept too. So, by what the screen shows, a long answer at a wall no reader names is work where the reader reads neither tool rows nor words (Grok, Kimi and the generic program, whose replies all mark the same: holding or counting it would hold a worker's every long turn), and a long tool-less answer in new words is work where tool rows are read: neither is held, counted in the run below or backed off (a done claim's back-off aside), each paced by its own busy spell of min_work or more. A wall read at the answer is settled by the next point's read; an act whose row stands unanswered leaves the replies (no model action reaches it, since the walk draws nothing while an act is pending: the unit test an_act_whose_row_stands_unanswered_past_its_take_leaves_the_replies_though_the_screen_drew binds it); a turn the books say ran that no loop timed is work; the ledger's point and act rows carry the replies across loops, with the reset the newest answer counted showed (`reset at <unix ms>`), so a loop that takes over at a reset that moves forward holds where the one before held while the wall's placed reset is remembered (MEMORY_TTL past it; past that a reset that names a clock alone is placed anew, a new wall to that loop, one resume per such takeover). A reply no reader names as a wall holds once two answers settled to it, any kept reply said again included (two wordings by turns hold), the generic program's too, whose reader reads no words: holding is the safe side. A read wall holds once it came twice and FUTILE_AFTER_TRIES tries were spent at it (its kept tries or its track's attempts, so a loop that takes over holds where the one before held): a retry ladder keeps its tries and no more, a retryable server error's (5xx, overload) included. A read wall whose date or clock reset is still ahead is waited out while its tries last, and whatever its tries where that reset is not the one its last answer showed (a new reset is a new wall); one whose reset is read relative to its notice (agy's `retryDelay:45s`, `resets in 1h`) is waited out while its tries last. The tab's exception says so (Exception::repeats; a usage, spend or model limit's Exception::limit_repeats, futile, where its notice left only while that limit's track stands). A hold lasts until the reply changes (another wall message included), a date or clock reset is named or passes since the last answer, the one the last answer showed (read again within SAME_RESET, the loop placing a reset on its clock at each read) only while the wall's tries last (passing keeps every walled reply kept with no answers and with its tries, so a reset that moves forward with each answer is resumed FUTILE_AFTER_TRIES times, two wordings of the wall by turns (Muse Code's one-line and two-line usage walls) until one of them meets its last, then held under a futile exception until the wall's message changes to one no kept reply has, the agent works or a person's turn comes; a relative reset, restated ahead of every answer, lifts the same-reply hold only while the wall's tries last, never the bound below, and passing clears nothing, so its answers count as the wall's tries), the host measures the route back up since the last answer (Reach::Up at an Unreachable or CutOff API error: one more try), someone else's turn runs (a person's keys since the last point read and the last answer counted (TurnEndReading::person), or, for Claude Code alone, whose screen names each turn's prompt (TurnEndReading::answered, the `❯` row above its last message), a prompt row other than the last one read; a person's turn lifts the hold only where a point saw that turn busy: one the books alone show, a person's keys since the last point read, lifts nothing unless a tool row of it was read (on the screen, or in the history read to its start), one opening above a screen whose history was not read included (TurnEndReading::tools_unread: an act's answer there may have run one and is work, but this turn is the person's), whatever else it drew and whoever's prompt it answered, in a loop or at a takeover, since the person drives and the hold lifts at their next turn a point sees): one more act goes, a wall's spent tries left one short of FUTILE_AFTER_TRIES; the program's own no-work turn while a reply is kept, a Monitor's event or its own retry, counts into the replies and the run as an act's answer does (one no loop saw busy, its turn's words other than the last point's, whatever wall either showed, and its reply other than the newest kept, counts as a turn of no length where it ran no tool and ends the replies where it ran one; a wall's notice leaving changes no words and is none), a long one that ran no tool at a read wall or saying a kept reply again where tool rows are read included, so saying it again holds and new words count toward the bound below, though a long one ends the short streak; the other programs' readers name no prompt, so a lead agent's message typed into one of them through `aterm ctl send|turn`, which moves no person's keys, and its no-work answer count as that program's own turn), the agent works, a restart is made, Autopilot is parked Off (TurnEndState::park_off; the ledger's off-stood row ends the replies for a loop that follows), the program was started again since the books, or a loop's first point shows other than the newest reply the loop before it kept with no act of that loop's awaited (the program moved while no loop watched); time alone ends nothing, so a loop that takes over days later at the screen it was held at holds. A wording-blind bound stands beside it: NO_WORK_ACTS acts in a row answered by no work (Replies::run, the no-work answers seen taken and the program's own no-work turns; work, someone else's turn, a date or clock reset passing or a restart clears it, and the ledger's replies note carries it as `N in a row`) hold the next point the same way while it shows the newest reply kept (TurnEndState::repeats compares the screen's reply_mark, or the wall a track stands for, with it, as for the same-reply hold), under the same stalled exception and lifted as that hold is (a relative reset aside), so a program rotating through more than REPLIES_KEPT wordings is held, except at a read wall whose date or clock reset passes between its answers: each passing starts the run afresh, so there each wording's own tries bound the resumes, up to REPLIES_KEPT wordings by turns, and more than that are resumed at each reset (none measured draws more than two); NO_WORK_ACTS is 2×REPLIES_KEPT, two answers through each reply the streak keeps, so a wording said again holds first and a loop doing work never reaches it. A read wall's own tries still come first: at one whose tries are not spent the bound is NO_WORK_ACTS + FUTILE_AFTER_TRIES, so a wall met at the end of the run keeps its ladder and read walls of other classes by turns, whose tracks start their tries afresh, are held all the same. codex-alab supervises itself beside this loop (claim.rs), so this loop types nothing into it; a NEEDS YOU line never holds an act, which carries NEEDS_ACK once per new set; a person's short exchange ⇒ left; idle compaction after nothing to type ⇒ nothing; a question under Cruise ⇒ the session word's text (one bare meta first) or an escalation.	1,2,3,5,7,8,10,11	9/9
repo-work-probe	contract	harness::repo_work::{probe,Probing::ask,RepoRead,RepoWork::of_git,RepoWork::summary,printable,session_dir}, IdleHost::repo_work, Session::{repo_read,repo_asked}	probe(dir) = Clean where git cannot run, dir is in no repository, dir (or its top level) is under a folder macOS guards, or the repository holds nothing no remote has at a HEAD the probe could not name (no commit yet, or its head read, the clean path's last, cut or refused after every other read finished) or over a read git refused that its counts rest on (the stash list, for-each-ref refs/remotes, the unpushed or unlanded count), so Clean names nothing it did not read; Checked(CleanAt { repo, head: HEAD's full lowercase hex, main_ref, remote: whether for-each-ref found any remote-tracking ref }) where it holds nothing no remote has at a HEAD named, which every decision reads as Clean (is_clean true, found and into_found None, same(Clean, Checked) true; §14.9 slice 9a), taped {"at":{…}} where Clean is null and an older tape's null reads Clean; Unread where a read it needed did not end within PROBE_LIMIT of the end of the read before it (the first, of the call) or did not start for one that did not, could not be made (git could not start for a reason other than there being none, or a signal ended it), or was the status read of a repository it found and failed, and it found nothing before (never clean: Land lands nothing over it, its nudge asks for the git status, and the journal says REPO UNREAD); else Found with the counts of tracked changes staged or not, untracked unignored paths, stash entries and commits on HEAD no remote-tracking ref contains (0 where the repository has no remote-tracking ref), with at most NAMED_PATHS printable paths, however long reads that each end within PROBE_LIMIT take together; the host reads it beside the loop (Probing::ask: on a thread of its own, an ask answering None while it reads on past PROBE_ASK or once the worker is stopped), and the loop asks again until it answers, renewing its claim when due between asks, a stop ending the asking with the repository unread and nothing journaled; the loop asks it only where the words say done or the record says finished, again for other words, a turn run or REPO_REPROBE past.	1,3,8	5/5
typing-budget	contract	TurnEndState budget seeding, approvals::Row::to_json	The hourly budget starts at N minus this session's typed acts in the last hour from the ledger (account moves excluded); 0 escalates; N=0 is no cap.	1,3,7,8	2/2
act-write-protocol	contract	Session::{turn_end_execute,press_fenced,call}, REACTION_WAIT, composer_guard	A typed act is a fenced send then an Enter on a read showing exactly the text (Codex waits for the echo to settle), else one guarded turn after a fresh status; suggestions accepted by right + fenced enter; then await seq REACTION_WAIT; a reply inside the settle is a short yield; a stop refuses further waits and writes; shells are never written.	1,2,3,5,7,8,11	8/8
never-type-over-a-person	invariant	Session::{turn_end_execute,human_grace,status,press_focused,launch_call,word_edge}, approval_loop::LaunchStatus::of, screen::parse_text_json, decide_turn_end draft/person arms, composer reading	Every write is judged on a read with no keystroke within the grace, no other driver's hand, an empty or placeholder composer (or a draft standing the whole grace, submitted by a fenced Enter), and a non-shell program; a missed guard is no act and leftover text escalates. Folder-trust arrows and Enter also carry the judged read's person count; a person's intervening key yields. One exception, THE LAUNCH (`Session::launch_call`): a box of kind Trust that trust-dialog@v1 or trust-any@v1 approves by `Choice::Focus` is pressed within the grace iff no other driver's hand is on the session, the Autopilot word read in that pass is not off, the judged read J carries `human_seq` n and `"launch"` {why=person, seq=n} whose word is the program of J's reader, the decision's status says `launch=person launch_seq=n`, `program=` that agent and `input=clear` (pending|typeahead ⇒ decided again on a fresh read after a bounded wait; missing, `-`, stalled, stopped ⇒ held), and that witness id has answered no other box; `LAUNCH seq= id= word=` is journaled; every key carries `if-human=n(J)` and a person's skip yields. A newer verdict publication or first client sight proves no exemption from the grace. A person's key after an accepted arrow can still land on the moved focus. The key that sent the prompt a lost login answered holds its /login only LOGIN_QUIET (no key since the read that saw the wall answer it, the composer empty, no Esc, at a wall this loop saw arrive); a later key, or a loop's first point, keeps the whole grace, and the exchange the wall swallowed stays the person's through its sign-in. A switch of the Autopilot word (`Session::word_edge`, `autopilot-word-edge`) writes only through `turn_end_now` at rest, after its settle, judged as a point's decision is: a wait in a running turn the switch wakes (`Session::busy_wait`) types nothing, so a switch made mid-turn is typed at the next point or after it, never into the turn.	1,2,3,5,7,8,11	14/16
refused-write-retried	other	Session::{park_on_hold,back_off,press_fenced,probe_left_enter}	Liveness: a refused act is parked or backed off and re-sent exactly once more after the refusal stops; never dropped silently.	1,2,3,7,8,13	2/2
api-wall-acts	contract	decide_turn_end (outage-resume/context-compact), TurnEndTiming, Session::{wait_for_next,reach_edge}, IdleHost::reach	A 529 is an outage: it waits its rung, then types its one blind line once; a full context compacts then continues, the third failure one context exception until lowered; a connection wall asks the reach once per point, types nothing while it is measured down, and its one measured line once Up after the last try, the blind ladder again when Unknown.	1,2,3,5,7,8,10,11	6/6
wait-told	contract	Session::{tell_wait,due_at}, Due::unix_ms, IdleHost::plan	The host is told a wall's next try once as unix seconds and again only when that second moves; cleared when the try is typed; the take-check wait is never a plan.	1,2,3,8,11	3/3
live-zone-read	contract	run::{tail_misses_the_live_rows,drawn_above_tail,gap_above_tail}, Session::read_screen	A tail with missed live rows or a blank gap above the composer is read again whole and cut to the live zone.	1,3,7,8,11	1/1
login-wall-acts	contract	decide_turn_end (auth-login, login-dialog, login-back, sign-in-menu), policy::turn_end::{login_quiet,login_wall_waits,sign_in_menu_waits,TurnEndReading::at_sign_in_menu,TurnEndState::{account_point,account_answer,login_left,login_back_from,login_backed_out_since}}, policy::login::{pick_method,picks,dialog_act,backs_out,back_out_of_picker,cancel_row,picker_gate}, harness::login_latch::{login_key,LoginLatches}, exception_for, approval_loop (the picker's own rule), Session::{escalate_point,confirm_dialog,turn_end_execute}	A confirmed lost login that is no roster row's (`login_label` with `logins` on, read again after the host's discovery, asked once before any /login: `IdleHost::login_discover`, the upkeep's sync) and has no account move to come (over a finished task too, a move the host could not make included) types /login once, never into a loss whose /login was left (aterm's or a person's own), and raises no badge: the owner is told by the login's notice where the sign-in is theirs. A roster row's lost login types none, whatever holds its move (relaunch or resume_limits off, the process not restartable, a restart not made, a draft): it waits on the row's own sign-in (`login_wall_waits`), and the host is told the row's login ended (`AccountPoint::LoginEnded`) over a draft, a finished task or no task too. A lost login drawn as a sign-in menu in place of the composer (no point, nothing typed into it) is escalated with how the agent signs in, except a roster row's in Claude Code: that one asks no person either, it waits on the row's own sign-in, its wait naming what holds the move, a person's keys holding its relaunch, the host is told the row's login ended there too (no other point is read at the menu), and the tab is relaunched onto the renewed row, the relaunch made over the menu; a tab with no task, or a done one, is left alone whatever its wait. A menu drawn after a move is the moved-to login's own, never the wall the session left: it answers the move signed out, and the tab waits on the row's sign-in as before the move, the move cap bounding its relaunches. The picker of the loop's own /login gets the method the session's login already is, from the row the dialog opens on, in the inline renderer's frame alone (the fullscreen renderer's bottom panel, past whose method pick nothing has been read in the real-Claude lab, gets no method: the person picks it, told by the login-choose notice), else nothing; a person's own picker is never answered. A /login the loop typed whose sign-in nobody needs (`login::nobody_signs_in`: the login a roster row's, `login_label` with `logins` on by the dialog; the login back in another tab; or a renewal the tab follows replaced it) is backed out of by itself with the dialog's own Esc (`login-back-out@v1`), the row named before the dialog was decided or after (the accounts' news decides the dialog of aterm's own /login again): at the picker by the approval loop, the focus where it stands and no method picked (alike in the inline dialog and the fullscreen renderer's bottom panel), and at the browser hand-off or the dialog's error by the turn end, fenced on the read, its person count and the dialog's `Esc to cancel` row; no person is told to close the picker, the browser hand-off or the message of a /login aterm typed (Console's menu, which only a person's pick at aterm's picker opens, is the person's: nothing is pressed there, and over a login back in another tab `login-back-dialog` tells them it can be closed); while that dialog closes nothing else is pressed or told, a dialog still drawn once its pause is over gets the Esc again (each pause twice the last, at most `BACK_OUT_PAUSE_MAX`; the approval loop holds a picker still drawn until then, asking no one, and never presses it inside the pause) and no person is told to sign the row in there, a login back in another tab then has its turn delivered again (login-back@v1) and a renewed one is relaunched onto, and once it is read gone (`Login interrupted` or the composer back, the loop's Esc or a person's) the tab waits on the row's own sign-in as at the row's wall, the login exception told before is `CLEARED` naming the back-out, not a sign-in or the agent working (`exception::LOGIN_BACKED_OUT` once the picker's Esc left it; `exception::LOGIN_BACKING_OUT` as the turn end's own Esc goes in at the browser hand-off or the dialog's error, where a method was picked), the host told the row's login ended, the relaunch onto the renewed row made where it reaches the tab, and no /login typed again. A person's own /login dialog is never backed out of, one opened after the dialog aterm backed out of was read gone included, and raises no notice at its picker, Console's menu or browser hand-off (the message it stopped on is told as `login-notices` says); a person's own /login left over the row stays theirs. Login successful gets one fenced Enter, which no typing budget counts. One sign-in per login: a tab whose stored login another tab of the host is signing in types no /login and waits on it (a tab refused that sign-in at its act types nothing either); once the login is back there, its turn is delivered again once (login-back@v1), and met by the wall again it signs in itself, the picker and the hand-off of that /login judged alike (`TurnEndState::login_back_from`: a login the host still reports back elsewhere is the one delivered, so neither is backed out of); a closed tab's sign-in is the next tab's.	1,2,3,5,7,8,10	26/26
login-notices	contract	policy::notice::{NoticeText::fill,ALL,LABEL_ROOM,COMMAND_UNDER_DETAILS}, policy::turn_end::login_wall_waits, policy::exception::{Exception::of_notice,claude_login,exception_for}, turn_end_loop::sync_exception, policy::login::dialog_stands, run (busy read)	Every way an auto login, a renewal or an account move can fail to finish is ONE notice of `ALL` (a unique id, a unique two-letter code) in four one-sentence parts: what happened (no label), what aterm did, the one step (ending in its command where it names one) and what follows (asking nothing); filled at any admissible label, the message engine admits each part, and a label is named whole or not at all in a command (`accounts use <label>`, or the step points under Details). No notice's step asks a person to renew or sign in a roster row (`accounts renew`, `accounts login`): aterm's sign-in ladder signs in every row with an email, and the roster's two facts (a signed-out row with no email, a renewal that signed in another account) say what aterm did and step to the read `aterm harness accounts`. A roster row's lost login types no /login and raises none, whatever holds its move: the tab waits on the row's own sign-in and is moved onto the login it makes, and where nothing can move it (relaunch or resume_limits off, the process not restartable, a restart not made) its wait names what holds the move; a renewed row's draft alone is told (send or clear it, naming no sign-in). A lost login's exception carries its notice (`EXCEPTION … notice=<id>` in the journal) and is raised only where the sign-in stands as the owner's (the browser hand-off of a /login over a login no roster row names, a picker with no method aterm may choose, a /login left, aterm's over a login no row names or a person's own), never beside a badge; a /login about to go, a move and its wait raise none, nor does the dialog of a /login aterm typed whose sign-in nobody needs (typed before the roster named the login's row, over a login back in another tab, or one a renewal the tab follows replaced), which aterm backs out of and goes past; a login back in another tab is told (`login-back-dialog`) only at a dialog a person is in (Console's menu their pick at aterm's picker opened, or the message their own /login stopped on). A busy read ends it as `the agent working` only when no `/login` dialog short of its success stands.	1,2,3,7,8	11/11
usage-wall-wait	contract	Session::{wall_reset_at,reset_unix_for}, decide_turn_end (usage-resume), TurnEndState::untaken_act, limit::place_reset	Nothing is typed before placed_reset + grace, then one usage-resume (consent-accept for a bucket that asks consent to go on on usage credits, only under `[harness] usage_credits`; off, that bucket gets continue = false's answer: an advised account move, else the fallback relaunch, else its reset waited out); off, a spend wall, or a model bucket whose notice names usage credits, that names no reset and is no countdown the vendor runs by itself is never continued: its fallback relaunch where its model is named and one can be made, else Escalate once; the reset is placed on the done row, the arrival read, or the read after an untaken act; reprints and Codex placements cached by words; the budget stretch uses the same placement.	1,2,3,5,7,8,10,13	9/9
credits-hold	contract	policy::turn_end::{decide_turn_end,credits_hold,credits_fallback,held_by_credits,credits_model}, TurnEndState::{fold_credits,credits_held,credits_hold_stands,credits_reset,credits_told,agent_started,restarted}, TurnEndTiming::credits_bound, policy::outlook::credits_outlook, Session::note_credits, run (Guard::Credits), aterm_phase::credits::in_use	Under `[harness] usage_credits = false` (the default; owner, 2026-10-19), while the reading says usage credits carry a Claude Code session (TurnEndReading::credits: the account's notices, or a MODEL's — `<M> is drawing from usage credits`, `<M> is now using usage credits instead of your plan limits`, `Now using usage credits for <M>` — whose family is CreditsCarry::model; an episode on TurnEndState opened by a notice and standing through reads that draw none while its named reset + grace is ahead, or until since + credits_bound when no reset could be placed, or for a model's while the process lives, its `since` kept; one ended — its reset passed, or with no reset since + credits_bound — and still read opening a new episode), decide_turn_end returns no act of the policy's own that types (the continuation, the done check, a land act, an answer, an owed act, and the retry, /compact, usage-limit and model-bucket acts at their walls): an advised account move goes first (Restart Account Ahead, the hold its otherwise); else for a model's credits with model_fallback another family, the session restartable and no fallback made or refused, Restart Model fallback (model-fallback@v1, the hold its otherwise; the made restart clears the episode and owes the return at its reset); else, a reset named, WaitUntil(reset + grace_for(program)); else Escalate(CREDITS_HOLD_TOLD, or CREDITS_HOLD_MODEL_TOLD for a model's) once an episode, then WaitUntil(since + credits_bound) — at which one act goes as the probe — or, for a model's, Nothing until the process changes (agent_started after the episode drops it). Acts that type nothing stand; the login and memory wall acts are not held; a person's own turns are never held and a person's draft is not submitted; the host's foreground and background steps are withheld under Guard::Credits, and the loop keeps a reset still ahead, an unplaced-reset account episode to its original bound, and a model's credits through reads that draw no notice (Session::note_credits). Under the key on, with no notice and no standing episode, or past the hold's end, the decision is the same screen's without its notice; the probe past a named reset or at the bound may be billed to credits where another window still binds.	1,2,3,5,7,8,11	11/12
left-limit-tracking	contract	Session::observe_point, wall_memory, TurnEndState::seed, wall_confirm::served_since_notice, turn_end_reading	A limit last read is waited on to its reset even after its notice leaves (Esc, /model, /resume, cancel) until a turn is served under its own notice; wall-wait rows open and close it in the ledger and seed a successor; the Tier-1 bind holds the wall exactly where supervisor_usage_wall_model (Hidden=1) does.	1,2,3,5,7,8,10,11	25/25
sleep-wake-settle	contract	slept_through, Session::{watch_sleep,woke,settle_wake,due_at,falls_at,wait_for_next,call,host_steps_in_background}, WAKE_SETTLE, WAKE_SPREAD	A sleep (wall ahead of monotonic by > 1 min between any two marks) sets a settle at wake + WAKE_SETTLE + the session's spread; dues are decided at their wall time and acted after the settle; nothing is typed, stepped or probed inside it (the woken-turn Esc, which types nothing, excepted: `woken-turn-esc`); waits step to its end.	1,2,3,8,11	12/12
woken-turn-esc	contract	Session::{watch_sleep,note_retry_row,unstick_woken_turn,woken_at,woken_waiting,await_turn_from}, woken_turn::{WOKEN_MARGIN,WOKEN_SHORTEST_WAIT_S}, aterm_phase::api_retry_row, Fx::full_wake, TurnEndState::{woken_esc,woken_esc_stands,took,box_up,observe}, person_hold, act_past_walls	A sleep the loop measured while a busy read showed the worker's turn running arms one Esc for that wake, pressed on an unattended Claude Code busy read (Autopilot not off, `retry_api_errors` on, nothing written or signed in) whose status row is the vendor's network or no-response API retry (never a usage limit, a queued request, the auto-mode check, a refused connection or a server error), with >= WOKEN_SHORTEST_WAIT_S to its next try where it counts down, in a full wake, the host's reach Up since the wake for >= WOKEN_MARGIN, and for a no-response row (which names the failed try's window, never when the retry in flight went out) only where the same row stood at the last busy read before the sleep and at every busy read since (Session::{note_retry_row,retry_seen}, WokenTurn::carried), one first read since the wake never, no draft in the composer, fenced `if-human=` and `if=` on the row (a no-response row's whole text, so never a later try's row); only a press the server took spends it; the point it stops is no person's Esc (no grace hold) while its mark stands (from the press to a live read, re-set by a retry-row read within WOKEN_MARGIN of the press; a box or a point no Esc stopped ends it), and the vendor's continuation after a usage limit gets the limit's resume there.	1,2,3,8,11	13/13
stall-ladder	contract	stall rungs, Session::stall_rungs, IdleHost::{exception,record,background_jobs}, background::{BackgroundCall::{fact,why},task_token,exe_word,wait_phrase}, Exception::no_progress	An unnamed stuck screen (busy on background alone under a resting title, or an unchanged idle dialog) climbs STUCK, STALLED + record, EXCEPTION once each at T, 3T, 6T; nothing typed; the end withdraws both and re-tells a standing point. A busy-on-background break the host reads as a call making no progress (IdleHost::background_jobs = BackgroundEvidence::NoProgress, the longest-quiet call, asked once the break has stood background_settle) climbs to STALLED at once with a record naming its pid and minutes (StallSeen::NoProgress), at once too after a rung that named no call (STUCK, or a STALLED in the screen's words) even within the title's status pace, which every other rung keeps, then to EXCEPTION one T after that if the same call (its group) still stands, a different worst call being named at STALLED in its turn first; both name the call by template words alone (a [A-Za-z0-9_-]{1,32} task id or none, a plain exe word or `-`, pid, minutes, a wait word from a closed list); BackgroundEvidence::Unknown leaves the ladder and its words as they were.	1,2,3,7,8,11	6/6
facts-no-progress-episode	contract	facts::{EpisodeKey::{no_progress,of_call},FactsReading::{fresh_at,tells_at,open},FactsState::{observe,observe_closed,pending,told,is_told,seed}}, background::BackgroundCall::told, occupancy::door::{seed_rows_in,fact_intent_reason,fact_outcome_reason}, Session::hold_seed_from	A no-progress episode is the FNV-1a fold of (pgid, the leader's birth): another birth in the same group is another episode, and a call whose birth was not read names none. pending(r, now) = the worst (quiet longest) untold open episode of a present reading no older than FACT_FRESH_MS (FactsReading::tells_at) whose sentence (BackgroundCall::told: a [A-Za-z0-9_-]{1,32} task id or "a background call", the pid, minutes, a closed-list wait; never the exe, a path, argv or output) is non-empty and within FACT_CAP, else None. observe forgets a told episode only on a present reading no older than NO_PROGRESS_FRESH_MS (fresh_at) that no longer names it; an absent or stale reading forgets nothing, and other kinds keep what they hold. seed(rows, now) tells exactly the episodes whose last row is typed (an intent alone, or a skip, leaves it untold) and holds a no-progress episode by its own key, never its kind; a successor reads each fact row's kind back from its `kind=… episode=… door=…` reason (seed_rows_in), so a restart does not retell a told call and a call born since is told.	1,2,3,7,12	7/9
fact-rides-turn-end	contract	policy::turn_end::{FACT_RIDES,fact_may_ride,fact_rides,fact_suffix,carries_fact,decide_unridden,decide_turn_end_traced,TurnEndState::{fact_rode,in_limits_place}}, TurnEndReading::fact, Session::{fact_for,fact_offer,fact_intends,fact_rode,type_text_written}	With `[harness] facts` on (the default; false is a limit, and then no fact is asked or typed anywhere: decide_turn_end_traced drops a reading's fact under it, so a shadow's candidate is judged by its own key), the decision's final pass asks IdleHost::facts only for an act in FACT_RIDES (continue, the done check, land, the land nudge) outside the limit's place, and the act carries ` (aterm: {fact})` after its text and its standing rules, whole or not at all within VERBATIM_UNITS: a fact that does not fit leaves the act as it was decided and stays untold. No answer, person's draft or other act carries one. The intent row (fact-intent@v1, `door=turn-end`) is booked before the write and the outcome row (fact-no-progress@v1, typed, or skipped why=not-written, or why=not-submitted where the text stands in the composer unsent) after it; the episode is told only where the act was typed, the person's record made then (IdleHost::inform of the break's own line shape), and an episode told at either door is told at neither again until it closes.	1,2,3,5,7	5/6
stuck-break-line	contract	occupancy::door::{stuck_at_break,break_lines_in,write_held,BreakLine,StuckVerdict,FactDoor}, SupervisorConfig::stuck_break_grant, Session::{stuck_break,stuck_books,stuck_lines_seed,break_lined,stuck_types,type_fact}, run::host_steps_in_background, occupancy::templates::stuck_line	`[harness] stuck_break_note` ships false; only the root table's true makes the StuckBreakGrant, whatever `occupancy` says, and `facts = false` withdraws it. Without it a break asks no facts reading, books no row and journals nothing: the loop is main's under every occupancy mode. With it, at a settled, drawn, unattended break whose fresh reading names an untold episode, stuck_at_break holds the line by the first door guard for its autopilots (Cruise and Land, never Off), then by the break's line gone, the host's withhold and the upgrade's notice due (rank 3 first), then by the session's pace (SESSION_PACE_MS: the lines that went, its own and under `occupancy = "prompt"` the door's, never the door's books, which hold every line observe would have typed, a predecessor's too after a switch to prompt; a successor carries each line typed at a break within the pace, door::break_lines_in); else its intent row (`door=break`), the fenced write of `[aterm harness] aterm: {fact}.` re-judged at the write, break_taken and the outcome row. One line a break: after the host's notice and before the nudges, which then wait for its act and its pace; once an episode; booked alike under occupancy off and observe. Refines OccupancyDoor's OpenStuck/Intend/Type under `Key = 1` (tests/conformance_occupancy.rs).	1,2,3,5,7,8	6/11
stop-and-handover	contract	Session::{stop,call,hand_over}, escalate::adopt_badge	A handover stop leaves the badge for the next loop to adopt with one set; a final stop clears it; a stop mid-probe writes nothing more and leaves the panel owed.	1,2,8,13	2/2
model-switch-carry	contract	approvals::open_model_switch, Session::carried_model_switch, TurnEndState::restarted, decide_turn_end (model bucket)	An open model switch is read back from the ledger unless undone or closed by a later made account move, due only past its reset; the Tier-1 bind continues, falls back and returns exactly where supervisor_model_switch_model does.	1,2,3,7,8,10	4/4
account-move-execution	contract	decide_turn_end (account_point), IdleHost::account, Session::restart_agent, relaunch::carried_quietly	An advised move is only the host's relaunch (typed ledger row), never typed text; wait:changed holds at a wall but defers ahead of one; refused:line is waited out; a move at a wall closes the old episode and resumes; the owner's clearing types one resume.	1,2,3,7,8,10,11	5/5
account-advice-asks	contract	Session::{account_round,account_edge,wait_for_next}, IdleHost::{account,account_stamp}, TurnEndState::seed_account_moves	Advice is asked once per decision after the point is folded; a standing advice wait is decided again once per stamp move (not the stamp at the ask or at start); a short-wait answer cuts the wall's wait; accounts and logins off ⇒ nothing asked.	1,2,3,7,8,11	8/8
login-lifecycle-accounts	contract	Session::account_round, IdleHost::login_superseded, exception_for, accounts renew/login	With a rostered login a lost login waits on aterm's own sign-in of the row (no /login, nothing told, the host told the row's login ended), the /login it typed before the roster named the row backed out of and waited past alike, a lost login whose row the host's discovery names before any /login (the default login's address the upkeep's sync adopts) waited on alike, and is followed onto it where the relaunch reaches the tab; where none does (relaunch or resume_limits off, the process not restartable, a restart not made) it waits all the same, its wait naming what holds the move, never a /login or a renewal ask; a host's wait with an end is waited at; a superseded login is followed by the relaunch, never renewed again; old told moves seed a wait.	1,2,3,7,8,11	5/5
account-move-answer	contract	IdleHost::{account_served,account_answered}, TurnEndState::{account_answer,account_told}, Session::account_tells	After a move the host is told once what answered it (served, a limit or lost login only once a turn ran under the new login), before the point asks advice; the point read again after a relaunch tells nothing.	1,2,3,7,8,11	4/4
account-ledger-readback	round-trip	approvals::{account_moves,account_switch_reason,account_unmade_reason,account_answer_reason}, TurnEndState::{seed_account_moves,seed_account}	A successor loop reads the session's account rows back into the same policy memory (bound, latch, unanswered move, resumes, clearings) that wrote them; other sessions' rows read as nothing.	1,2,3,7,8	7/7
usage-probe-gates	contract	Session::{probe_gates,probe_advised,probe_step,probe_type}, IdleHost::claim	/usage is written iff every gate is open at the point's first look (else one SKIPPED gate line per point); with no host or advice requests are byte-for-byte unchanged; a run is fenced /usage + Enter, two settled equal reads, the sample, one Escape, then the point's own act.	1,2,3,5,7,8,11	10/10
usage-probe-panel-close	invariant	Session::{probe_owes,probe_closed,probe_step}, approvals::probe_owed	An Escape only under the panel's own header with no busy signal, our claim and no person; an unowed panel gets no key; each failure mode closes once with no sample; refused Escapes are retried, never a second /usage; the panel stays owed in the ledger until the composer returns.	1,2,3,7,8,11,13	16/16
usage-probe-memory-neutral	invariant	Session::probe_step, TurnEndState	A probe step leaves TurnEndState unchanged whatever its outcome, and its ledger row is not a typed act.	1,2,8	1/1
usage-probe-left-typed	contract	Session::{probe_left_stands,probe_left_enter,holds_probe_draft}, IdleHost::inform	A refused /usage leaves nothing and the act goes on; text left typed is a record (panel owed); past the grace it is sent by the probe's own Enter and its panel closed, booking nothing; untaken Enters are retried; the probe never types over its own text.	1,2,3,5,7,8,11	6/6
turn-end-refines-SupervisorTurnEnd	refinement	decide_turn_end, TurnEndState::{observe,acted,host_typed,standing_needs}, transcript_tasked	At every reachable state of supervisor_turn_end_model (Cruise) the real verdict is the model's enabled action: a done claim's point gets the defend prompt (continue-done-check@v1) and the task never rests; a standing need's point is acted on, NEEDS_ACK beside the act; the newest unwalled reply the streak keeps (TurnEndState::replies) is the model's same, each turn saying its own words and WorkedShortSame the last's again, and a no-work answer that repeats the last one's gets no act and no wait for one (ANoWorkReplyGetsOneMoreActAtMost); a turn of real work ran a tool, or was busy past min_work at no read wall in words no kept reply has (WorkedLongQuiet: such a long answer that ran no tool is kept in its new words, ends the streak with no back-off and lets the next act go at once; WorkedLongQuietSame: said again it is no work, as WorkedShortSame is, and holds); a wall's retry the worker answers with real work ends the streak, no back-off after it (RetryTaken: the owner's ruling, checked at every state the walk reaches, which keys each state on the real streak and replies with the model's, so a path the real side reaches otherwise is walked too); a wall first met as a continuation's answer (WorkedHitsTheWall) keeps all its Futile tries, that answer none of them (AWallsTriesAreItsOwnRetries: tries ≤ retried); a wall that answered its Futile (FUTILE_AFTER_TRIES) retries the same gets no more of them, nor a draft submitted at it (AWallGetsItsTriesAndNoMore; the real side's tries are the answers to the wall's own acts kept for its reply, Kept::tries); NoWork (NO_WORK_ACTS) no-work answers in a row get no act, nor a draft or an escalation, NoWork + Futile at a wall whose tries are not spent (ANoWorkRunGetsNoMoreActs: the model's run, which WorkedShort, WorkedShortSame, WorkedLongQuietSame, WorkedHitsTheWall, RetryHitsTheWall, HarnessTurn, the program's own no-work turns ProgramTurn (new words), ProgramTurnSame (the last reply again) and ProgramTurnUnseen (new words in a turn no loop saw busy, a turn of no length) and a wall appearing while a reply is kept count and work or a person's turn ends, is the real Replies::run, keyed into the walk's states, so the walk reaches the bound with each turn in its own words); a targeted path takes each hold, then a person's keys that submitted nothing (PersonTypes, GraceEnds), mirrored as their keys with a new screen mark, no new prompt and the session's submits as they were or one up by an Enter over the emptied composer, leave both saying Nothing, as does an Enter that left their draft standing (DraftLeft, GraceEnds), mirrored as their keys with the submits one up, the composer read typed, a new screen mark and no new prompt; the model has no action for a person's turn no point saw busy, but the mirrors with an Enter are one the books read that ran no tool, so the walk checks TurnEndState::observe's books-turn arm there (without it the walk fails), and the replays check the rest of that rule (same_reply_tests: a person at a held wall, a lead agent's prompt at a Claude Code hold with a person's key beside it or none); WorkedShortSame says a report again, never a claim; Buggy (a done claim that rests the task, CruiseNeverRests; a need that holds the point, ANeedNeverBlocks; a box, a person or a draft typed over; Buggy = 3's continuation into the same reply again; Buggy = 4's back-off after a taken retry's work, NeverBackOffAfterRealWork; Buggy = 5's retry past the bound, AWallGetsItsTriesAndNoMore; Buggy = 6's continuation answer counted as a try, AWallsTriesAreItsOwnRetries; Buggy = 7's act past the run, ANoWorkRunGetsNoMoreActs; Buggy = 8's program turn that lifts the replies as someone else's would, caught through quiet, the ghost of the run only it parts from, ANoWorkRunGetsNoMoreActs) and the back-off knobs disagree.	1,2,3,7,10	1/1
turn-end-refines-SupervisorCruise	refinement	decide_turn_end, TurnEndState::{observe,acted,took,seed_task_done}, open_work_suffix, own_repo_work_suffix	Under Cruise the decider continues, types the defend prompt at every done claim (suffixed with the open work or the repository's work exactly where the model's open is 1 or 2; a managed worker's at once within MANAGED_SHORT_TURNS, anyone else's past the back-off), answers, acts at a wall and types nothing at a busy screen exactly where supervisor_cruise_model enables them; it never reaches task_done and refuses an old done seed; its done reports, managed worker, streak (a yield of real work ends it whatever act it answered, a wall's included, real work being a turn that ran a tool, or one past min_work at no read wall in words no kept reply has; this walk has no long answer that ran no tool, which SupervisorTurnEnd walks as WorkedLongQuiet, so the replay a_wall_no_reader_names_answered_after_a_long_spell_that_ran_no_tool_is_held_all_the_same binds it under Cruise and Land: said again it is held, in words of its own each time it is typed into at every answer), open items, repository work and the reply its streak keeps (same: each turn its own words, YieldAskSame the last's again, then no act and no back-off's wait) are the model's; Buggy's rest at the defend prompt's answer, its continuation over a claim, its old done seed and its act and finish over a busy screen, and Buggy = 2's answer into the same reply again, are refuted.	1,2,3,7,10	2/2
turn-end-refines-SupervisorLand	refinement	decide_turn_end, TurnEndState::{observe,acted,took,seed_task_done,face_moved,face_start}, TaskFace::of, clean_defense, open_work_suffix, repo_work_suffix	Under Land the decider types the instruction, the nudge (no count of its own; suffixed where the model's said and dirty say so), the land answer and the wall's act, waits, or types nothing exactly where supervisor_land_model enables them; its landing (only at a land act's yield, a wall's retry included, on a clean defense over a clean repository), person's exchange, episode, needs, open work, repository work, streak (a yield of real work ends it whatever act it answered, a wall's included, real work being a turn that ran a tool, or one past min_work at no read wall in words no kept reply has; this walk has no long answer that ran no tool, which SupervisorTurnEnd walks as WorkedLongQuiet, so the replay a_wall_no_reader_names_answered_after_a_long_spell_that_ran_no_tool_is_held_all_the_same binds it under Cruise and Land: said again it is held, in words of its own each time it is typed into at every answer) and the reply its streak keeps (same: YieldAskSame says the last no-work reply again, and nothing is typed or waited for past it) are the model's; a landed record is seeded where SeedLanded is enabled and never at a wall; the face the loop tells (face_moved at points and at live reads of someone else's turn) is the model's, and three tellers that miss a move fail; the instruction waits no back-off (TheInstructionWaitsNoBackOff, v6 §14.10: typed at the point however long a streak it follows, where the nudge and Land's answer wait theirs); Buggy's landing on the token, over a dirty repository or over a need, its quiet after two nudges, its need that holds the point, its nudge into a landed task and its seed at a wall, Buggy = 3's answer into the same reply again, and Buggy = 4's back-off over the untyped instruction, are refuted.	1,2,3,7,10	2/2
turn-end-refines-SupervisorOutageWait	refinement	decide_turn_end, TurnEndState::{observe,acted}, cleared_text	At each state reached by the finite walk's first mirrored path in supervisor_outage_wait_model, under Cruise at a connection wall the host measures (ENOTFOUND) or a 529 it never measures, with the real outage ladder scaled to one minute a tick (outage_first = First, outage_cap = Cap), the real decider types the measured line (outage-resume@v1, "The connection error has cleared; …") exactly where ResumeMeasured is enabled, the blind line ("… may have cleared; …") exactly where TryBlind is, and otherwise waits with WaitKind::Outage: tries = false while the route is measured down, else tries = true until the rung gap ticks from the wall's start or its last try; the up measured before the last try is read by the real track's since, not told it. Buggy 1 (today's typed retry: a try whatever the route measures) disagrees at a measured-down wall; a checker handed main's retry words wherever the decider types (api-back@v1's where the line is measured, api-retry@v1's where it is blind) disagrees at those states and only there, so the walk holds every typed act to the line; Buggy 2 (no doubling) types before the real rung; Buggy 3 (no cap) waits past it; Buggy 4 (a second resume per clearing) types where the real decider waits; each breaks its law along a named path the base model keeps (TheLineSaysHowItCleared: Buggy 1's blind try at an edge up, Buggy 4's measured line at an up from before the last try). The gap's cap is an invariant (Buggy 3 breaks it), the box a bound. Land's instruction as the resume and the hold's bypass are bound by the policy's unit tests, not this walk.	1,2,3,7,10	5/5
turn-end-refines-SupervisorUsageWall	refinement	aterm_phase reader, decide_turn_end, TurnEndState::untaken_act, limit::{place_reset,reset_at,within_a_session_window}, tap_clock::{TapClock,timed}	The reader and decider continue a usage wall exactly where supervisor_usage_wall_model's Act is enabled (else wait to that tick) with the real placement and, under the Tapped dial, the real TapClock's reset of a cut notice; each historical defect is caught exactly where it fires, and the loop without the tap types on the back-off.	1,2,3,7,10	13/13
turn-end-refines-SupervisorConditionResume	refinement	decide_turn_end, TurnEndState::{observe,acted}, cleared_text	At each state reached by the finite walk's first mirrored path in supervisor_condition_resume_model, under Cruise at a weekly wall whose notice names its reset, the real decider types the one cleared line (usage-resume@v1, "[aterm harness] The usage limit has cleared; continue where you stopped.") exactly where Resume is enabled, Cruise's continuation exactly where ModePrompt is, and waits otherwise: while the wall stands, and while a person's last key is inside their grace, past a clearing as at the agent's own turn end. A loop deaf to the person's keys disagrees only where the person holds the act, typing its resume over them; one blind to the wall types into it; a checker that wants the mode's words for the resume disagrees at every owed resume. Buggy 1-4 each break their law along a named path the base model keeps. Land's owed instruction as the resume, and the blind try's "may have cleared", are bound by the policy's unit tests, not this walk.	1,2,3,7,10	3/3
turn-end-refines-SupervisorCreditsHold	refinement	aterm_phase::credits::in_use, decide_turn_end, TurnEndState::fold_credits	At each state reached by the finite walk's first mirrored path in supervisor_credits_hold_model, under both UsageCredits values, the real reader and decider type the supervisor's own act exactly where OwnAct is enabled (with no known episode, at the named reset, or at the hold's bound with none named), escalate exactly where Tell is, and otherwise type nothing; the model distinguishes known from notice, NoticeGone/NoticeReturns preserve a no-reset episode's clock, and its gone notice before/at Bound and no-known-episode states are reached; the decider under the key on disagrees only in known episodes, including one whose notice vanished, and forgetting a reset has a named-reset boundary witness (its effects may carry into later episodes). Buggy's ActsOnCredits breaks NoOwnActOnCredits, StaleNoticeHolds NoStaleHold and UnboundedHold NoEndlessHold, each law stated over OwnAct's guard; TheCreditsHoldEnds ([]<> no hold standing, weak fairness on Tick) is broken by StaleNoticeHolds and UnboundedHold. These are authored finite correspondence controls, not provider or billing evidence.	1,2,3,7,10	2/2
account-rotation-refines-model	refinement	accounts::{advise,begin_rotation,take_canary,prove,strike,pin,unpin,note_*,forget_session,condition}, decide_turn_end account arms	accounts::State simulates supervisor_account_rotation_model: per-session advice and every transaction match the model at every reachable state; the decider departs only toward today's per-session bounds; histories from empty stay in step; every defect disagrees.	1,2,3,5,7,10,13	9/9
login-wall-refines-HarnessLoginWall	refinement	upgrade::{transcript_login,rearmed,notices_received,clock_held,next_step,announce_asks}, decide_turn_end (login_shared, login_wall_waits), exception_for, cli::run_accounts (renew)	At every reachable state of harness_login_wall_model the real code takes exactly the allowed step (login once where the login is no roster row's, wait on a roster row's own renewal and type no /login over it, back out of the dialog of a /login typed before the roster named the row or over a login back in another tab and tell no one there, tell where the sign-in is the owner's, pick a known method at its own /login, continue, announce, give up, restart, relaunch, follow, renew in staging only), never rests silent at a wall, never types /login over a person's own /login left or while another tab carries the login's sign-in, and never answers a person's own picker; each of the model's eight mutant dials and twenty-two knobs is a defect the real code does not have, the knob that tells a person to sign a roster row in at aterm's own /login breaks NoRosterRowSignedInByAPerson, and the dial and knob that tell a person to close aterm's dialog over a login back elsewhere (Buggy=8, AskToClose) break NoPersonClosesAtermsDialog. A roster row's wall drawn as a sign-in menu, the composer gone, is bound where the composer's wall is but for the typed acts a menu cannot take (the login back in another tab's continuation): no /login, no person told, no badge, and the relaunch exactly where Relaunch is enabled, on a tab that made no move and on one a move made at a wall relaunched onto the row.	1,2,3,7,8,10	4/4
footer-model-refines-ClaudeFooterModel	refinement	footer::{session_of_pid,facts_for_entry_at,TailCache}, FooterFacts::filled_from, launch_card, launch_facts	From every state of claude_footer_model_model (sampled within 5 actions) the real chain names the model `shown` says; each old reader is caught.	1,3,7,8,10	2/2
usage-tap-refines-HarnessUsageTap	refinement	tap::{gate,Silence::step,Silence::untapped,Fresh::admit}, accounts::note_reading	On every reachable state of harness_usage_tap_model the real gate, admit, note_reading and silence step give the model's fields.	1,3,7,8,10	1/1
upgrade-model-ladder-refines-model	refinement	upgrade_models::model_moves_now	model_moves_now moves exactly where harness_model_ladder_model's VisitMoves is enabled; the cold-only rule is caught.	3,10	2/2
upgrade-model-priority-refines-model	refinement	upgrade_models::{Priority::{admit,render,parse},target,list_target,ingest}	The priority writers and reader are harness_model_priority_model's; ingest admits only what the model's writer admits; pre-fix writer and head reader caught.	1,3,7,10	3/3
upgrade-model-switch-refines-model	refinement	upgrade_models::{model_read_step,live_model_at,ModelRecord::*,model_due,ride_bar,model_to,line_model}	From every reachable state of harness_model_switch_model the read step and record operations produce the model's transition; flipped decisions and pre-fix shapes are refused.	1,3,7,10,13	3/3
footer-window-allocation	other	footer::{facts_for_entry_at,TailCache,rows_back}	Walking an n-byte transcript back allocates exactly ceil(n/TAIL_BYTES) fixed-size window buffers.	1,7,8	1/1
approval-full-power-presses-every-bash-box	contract	policy::decide_screen, ApprovalCtx, Approve	Every captured 2.1.280 Bash permission box is approved by its one-shot 1 at full power, with Safe's rule when Safe approves and allow-once otherwise.	1,3,5,7	1/1
```

### 1b. Chunk-6 groups whose statements were not delivered

The chunk covering `src/supervise/policy/*` (366 tests) returned no specs. Only its test grouping survived (80 groups, 363 tests plus 3 non-spec). Each group below is a spec to recover: read its tests, write the statement, functions and gaps, then move the row into §1. Their tests are mapped in §2 with these ids.

```tsv
id	tests
row-guard-exact	5
key-args-gen-fence	1
outlook-table	1
answer-of-outlook	1
exception-for-table	9
exception-text-lint	1
confirm-verdict	2
reset-go-and-host-defaults	1
rule-tool-parse	1
hook-matcher	1
settings-parse	1
settings-chain-order	5
owner-asks-extraction	2
settings-file-read-and-fence	4
managed-preferences	1
git-reads-placement	6
git-reads-flags	1
git-hazard	5
git-view-refines-git	12
git-program-on-path	1
worker-var-and-claude-dir	2
snapshot-parse	3
startup-admits	4
names-word	1
startup-read	2
question-recommended-decision	15
question-target-invariant	1
question-destructive-refusal	4
decide-routes-questions	4
question-decider-refines-model	2
policy-text-rules	1
fit-and-reflow	2
choose-text-table	5
recommended-path-independent	1
rm-resolver-sound	26
rm-resolver-complete-fragment	13
rm-on-disk	3
rm-no-roots-no-proof	1
scratch-root-patterns	1
rm-shell-tables-match-shells	3
te-continue-at-idle	5
te-suggestion-allowed	1
te-short-streak-backoff	3
te-continue-budget	1
te-classify-said	4
te-last-tail-memo	1
te-destructive-word	1
te-answer-policy	5
te-person-grace	3
te-draft-handling	3
te-done-check-finished	10
te-taskless-no-act	2
te-person-exchange	13
te-autopilot-off	1
te-land-policy	11
te-api-wall-ladder	14
te-wall-text-untrusted	2
te-retry-wait-flag	1
te-usage-limit-wait	7
te-left-notice	13
te-sleep-adjust	3
te-wall-breaker	2
te-model-bucket	10
te-screen-parsers	1
te-context-compact	3
te-login-wall	8
te-memory-wall	1
te-upgrade-ownership	4
te-act-take-window	4
te-account-move	14
te-probe-transparent	1
te-probe-gate	1
te-account-point	1
te-login-after-move	10
te-move-answer-and-resume	10
te-limit-reset-continuation	5
te-renewable-login	7
```

## 2. Test → spec map (TSV)

`primary` is the spec that retires the test; `also` lists the other specs the same test samples. Paths are relative to `crates/aterm-agent`. Tests listed in §4 have `primary` = `§4`. Files no chunk read are in §3 and are not listed.

```tsv
file	test	primary	also
src/harness/contract.rs	the_served_contracts_are_two_and_three	harness-contract-v3-read	
src/harness/contract.rs	the_roster_names_grok_alab_for_relaunch_and_upgrade	harness-contract-v3-read	
src/harness/contract.rs	a_rostered_claim_is_read_with_its_scope_and_versions	harness-contract-v3-read	
src/harness/contract.rs	the_scope_parser_refuses_whole_and_admits_only_the_rows_words	harness-contract-v3-read	
src/harness/contract.rs	the_claim_version_is_major_minor_zero	harness-contract-v3-read	
src/drive_cli.rs	a_removed_source_is_a_usage_error_naming_its_date	drive-flag-values	
src/drive_cli.rs	a_verb_followed_by_help_asks_for_help_and_never_acts	drive-top-parse-help-routing	
src/drive_cli.rs	classify_parses_the_command_and_exits_on_the_verdict	drive-parse-sub-positionals	
src/drive_cli.rs	classify_refuses_a_read_of_a_secret_under_the_default_list	drive-classify-verb	
src/drive_cli.rs	phase_ends_with_the_context_left	drive-phase-reply	
src/drive_cli.rs	the_ledger_takes_its_worker_from_the_journal_when_none_is_named	drive-journal-sid	
src/drive_cli.rs	the_ledgers_flags_belong_to_it	drive-flag-ownership	drive-flag-values
src/harness/accounts_tests.rs	a_candidate_struck_past_an_unanswered_account_leaves_that_account_as_it_was	acc-strike-undo-exact	
src/harness/accounts_tests.rs	a_dir_is_used_verbatim_and_two_spellings_of_one_place_are_two_logins	acc-append-row-roundtrip	
src/harness/accounts_tests.rs	a_held_sessions_wait_ends_at_the_rosters_next_opening_when_that_comes_first	acc-next-usable-opening-hold	
src/harness/accounts_tests.rs	a_hostile_state_is_bounded_as_it_is_read	acc-state-bounds-invariant	
src/harness/accounts_tests.rs	a_label_is_judged_only_for_a_stored_login_in_the_default_home	acc-judged-and-footer-label	
src/harness/accounts_tests.rs	a_ledger_row_round_trips_as_one_bounded_line	acc-event-line-roundtrip	
src/harness/accounts_tests.rs	a_line_that_is_not_a_row_of_this_ledger_is_no_event	acc-event-line-roundtrip	
src/harness/accounts_tests.rs	a_lost_login_signed_back_in_is_where_the_session_is_relaunched	acc-advise-rule3-wall	
src/harness/accounts_tests.rs	a_process_login_is_read_from_its_own_environment_and_nothing_else	acc-process-login-of-env	
src/harness/accounts_tests.rs	a_reset_is_allowed_only_on_the_machines_own_login_with_no_other_login_on_record	acc-reset-allowed	
src/harness/accounts_tests.rs	a_reset_more_than_eight_days_past_its_row_is_recorded_as_naming_none	acc-note-wall-episode	
src/harness/accounts_tests.rs	a_reset_used_or_a_limit_lifted_clears_only_its_claimed_window	acc-reset-outcome	Preserves unrelated and newer walls
src/harness/accounts_tests.rs	a_roster_admits_whole_in_file_order_and_a_missing_file_is_the_empty_roster	acc-roster-admission	
src/harness/accounts_tests.rs	a_row_wakes_waiting_tabs_only_when_an_account_is_usable_sooner	acc-wakes-table	
src/harness/accounts_tests.rs	a_session_row_older_than_thirty_days_is_dropped_when_the_state_is_saved	acc-drop-stale-sessions	
src/harness/accounts_tests.rs	a_session_that_ended_is_forgotten_and_a_canarys_end_wakes_the_others	acc-record-gone	
src/harness/accounts_tests.rs	a_settings_file_can_take_a_stored_login_away_and_never_gives_one	acc-advise-rule1-guards	
src/harness/accounts_tests.rs	a_spent_account_about_to_reset_is_low_and_never_a_fresh_candidate	acc-candidates-order	
src/harness/accounts_tests.rs	a_stamp_ahead_of_the_clock_is_not_recent_and_shadows_nothing	acc-rotation-breaker	
src/harness/accounts_tests.rs	a_stamp_ahead_of_the_clock_is_read_as_made_no_later_than_now_plus_the_skew	acc-stamp-cap	
src/harness/accounts_tests.rs	a_state_file_that_is_not_ours_is_a_refusal_and_never_an_empty_state	acc-store-load-save	
src/harness/accounts_tests.rs	a_struck_candidate_puts_active_back_and_nobody_else_was_moved	acc-strike-undo-exact	
src/harness/accounts_tests.rs	a_tap_reading_keeps_the_largest_figure_within_a_window_and_writes_only_news	acc-note-reading-max	
src/harness/accounts_tests.rs	a_torn_state_file_is_an_empty_state_and_a_note_never_a_panic	acc-store-load-save	
src/harness/accounts_tests.rs	a_wall_is_claimed_for_a_reset_once_and_the_claim_stays_as_its_record	acc-reset-claim	
src/harness/accounts_tests.rs	a_wall_is_recorded_for_the_process_login_whatever_is_active_and_strikes_nothing	acc-record-wall-decision	
src/harness/accounts_tests.rs	a_wall_row_older_than_the_process_is_the_previous_accounts_and_is_recorded_for_nobody	acc-record-wall-decision	
src/harness/accounts_tests.rs	a_wall_stands_until_its_instant_and_not_a_second_longer	acc-wall-stands-until	
src/harness/accounts_tests.rs	a_wall_the_owner_cleared_is_not_put_back_by_its_row_and_ends_no_pin	acc-note-wall-episode	
src/harness/accounts_tests.rs	an_exec_environment_that_reads_empty_names_no_login	acc-process-login-of-env	
src/harness/accounts_tests.rs	append_row_writes_only_what_the_next_read_admits	acc-append-row-roundtrip	
src/harness/accounts_tests.rs	at_a_wall_the_session_is_limited_whether_or_not_its_wall_is_recorded_yet	acc-advise-rule3-wall	
src/harness/accounts_tests.rs	candidates_sort_by_one_key_fresh_dedicated_soonest_week_label	acc-candidates-order	
src/harness/accounts_tests.rs	enrolled_is_the_rows_in_whose_login_directory_a_login_was_made	acc-enrolled	
src/harness/accounts_tests.rs	every_malformed_roster_is_refused_whole_and_names_the_row_and_the_reason	acc-label-grammar	
src/harness/accounts_tests.rs	every_non_candidate_has_its_reason_by_name	acc-words-table	
src/harness/accounts_tests.rs	every_rotation_the_advice_gives_commits_once_the_wall_is_recorded	acc-advice-commits	
src/harness/accounts_tests.rs	low_is_a_running_window_at_its_threshold_however_near_its_reset	acc-sample-windows	
src/harness/accounts_tests.rs	no_account_is_probed_within_ninety_seconds_of_any_accounts_claim	acc-probe-claim-spacing	
src/harness/accounts_tests.rs	one_wall_is_one_observation_until_it_is_found_standing_again	acc-note-wall-episode	
src/harness/accounts_tests.rs	rule_1_stays_for_a_session_the_roster_does_not_govern	acc-advise-rule1-guards	
src/harness/accounts_tests.rs	rule_4_leaves_a_limited_account_and_not_one_whose_low_window_is_about_to_reset	acc-advise-rule4-early	
src/harness/accounts_tests.rs	rule_4_probes_when_the_sample_is_due_and_only_then	acc-advise-rule4-early	
src/harness/accounts_tests.rs	samples_offers_auth_and_sessions_are_recorded_newest_first	acc-note-observations	
src/harness/accounts_tests.rs	several_accounts_are_listed_only_by_a_roster_that_admits_two	acc-roster-file-read	
src/harness/accounts_tests.rs	the_canarys_hold_lasts_ten_minutes_then_the_next_session_takes_its_place	acc-canary-hold-take	
src/harness/accounts_tests.rs	the_compare_and_set_refuses_by_name_and_changes_nothing	acc-begin-rotation-cas	
src/harness/accounts_tests.rs	the_condition_table_in_its_order	acc-condition-table	
src/harness/accounts_tests.rs	the_footer_names_an_account_only_where_two_are_listed	acc-judged-and-footer-label	
src/harness/accounts_tests.rs	the_ledger_appends_whole_rows_and_is_cut_to_its_newest	acc-ledger-file	
src/harness/accounts_tests.rs	the_lock_excludes_a_second_holder_and_is_free_the_moment_it_drops	acc-store-lock	
src/harness/accounts_tests.rs	the_machine_rotates_at_most_twice_the_roster_size_an_hour	acc-rotation-breaker	
src/harness/accounts_tests.rs	the_owners_clearing_resumes_a_session_at_the_wall_it_lifted	acc-advise-resume	
src/harness/accounts_tests.rs	the_probe_interval_is_half_the_time_to_the_threshold_clamped	acc-probe-interval	
src/harness/accounts_tests.rs	the_recorder_keeps_what_is_on_record_and_ends_a_pin_at_its_wall	acc-record-wall-decision	
src/harness/accounts_tests.rs	the_recorder_reads_a_row_of_any_shape_into_a_wall_the_state_can_hold	acc-record-wall-decision	
src/harness/accounts_tests.rs	the_recorder_records_nothing_for_a_session_the_roster_does_not_govern	acc-record-wall-decision	
src/harness/accounts_tests.rs	the_recorder_records_this_process_wall_for_its_account_once	acc-record-wall-decision	
src/harness/accounts_tests.rs	the_recorder_refuses_a_state_that_is_not_ours_and_leaves_it_byte_for_byte	acc-store-load-save	
src/harness/accounts_tests.rs	the_recorder_says_refused_where_its_directory_cannot_be_made	acc-store-load-save	
src/harness/accounts_tests.rs	the_recorder_waits_for_the_lock_a_bounded_time_and_says_it_is_held	acc-store-lock	
src/harness/accounts_tests.rs	the_roster_is_beside_the_config_by_one_rule	acc-roster-beside	
src/harness/accounts_tests.rs	the_signed_out_labels_are_the_enrolled_ones_in_roster_order	acc-signed-out-list	
src/harness/accounts_tests.rs	the_state_round_trips_through_json_with_sorted_keys_and_null_for_unknown	acc-state-json-roundtrip	
src/harness/accounts_tests.rs	the_store_keeps_the_state_private_and_reads_back_what_it_wrote	acc-label-grammar	
src/harness/accounts_tests.rs	the_transactions_keep_every_map_bounded	acc-state-bounds-invariant	
src/harness/accounts_tests.rs	the_unproven_active_account_is_not_probed	acc-advise-rule4-early	
src/harness/accounts_tests.rs	the_vendors_profile_source_is_not_a_stored_login	acc-process-login-only-weakens	
src/harness/accounts_tests.rs	the_wait_is_for_the_earliest_instant_and_prefers_a_dedicated_one_minutes_later	acc-next-usable-opening-hold	
src/harness/accounts_tests.rs	the_wake_marker_is_a_new_file_at_every_touch	acc-store-load-save	
src/harness/accounts_tests.rs	under_another_accounts_fresh_claim_the_leave_check_answers	acc-advise-rule4-early	
src/harness/accounts_tests.rs	unknown_keys_are_tolerated_and_a_file_this_build_must_not_guess_at_is_refused	acc-state-unread-classification	
src/harness/align_tests.rs	a_child_that_closes_stdout_then_hangs_cannot_survive_the_deadline	align-capture-bounded	
src/harness/align_tests.rs	a_child_that_never_returns_is_killed_at_the_deadline	align-capture-bounded	
src/harness/align_tests.rs	a_descendant_holding_stdout_is_killed_with_its_process_group	align-capture-bounded	
src/harness/align_tests.rs	escaped_descendant_holding_stdout_cannot_retain_a_reader_thread	align-capture-lifecycle-refines-model	
src/harness/align_tests.rs	escaped_holder_stage	§4	
src/harness/align_tests.rs	stdout_is_capped_and_the_exit_code_reported	align-capture-bounded	
src/harness/upgrade_drive_tests.rs	a_bounded_reader_requires_complete_successful_utf8_evidence	upgrade-summary-complete	
src/harness/cli_tests.rs	a_conversation_resumed_outside_aterm_is_never_told_unwatched	cli-upgrade-owner-status	
src/harness/cli_tests.rs	a_deferral_is_a_span_with_a_unit_within_a_month	cli-parse-defer	
src/harness/cli_tests.rs	a_disk_knob_that_cannot_be_read_turns_every_removal_off	cli-disk-config-fail-closed	
src/harness/cli_tests.rs	a_dry_run_line_says_how_long_behind_and_how_long_waiting	cli-upgrade-lines	
src/harness/cli_tests.rs	a_failed_flow_runs_nothing_and_records_only_a_first_login_signed_out	cli-accounts-login-table	
src/harness/cli_tests.rs	a_floor_the_file_does_not_say_never_reads_as_space_back	cli-disk-config-fail-closed	
src/harness/cli_tests.rs	a_flow_that_ends_signed_out_is_recorded_so_and_exits_1	cli-accounts-login-table	
src/harness/cli_tests.rs	a_hand_run_upgrade_stands_down_while_the_master_switch_is_off	cli-upgrade-sweep-gates	
src/harness/cli_tests.rs	a_hand_run_upgrade_that_could_not_sweep_does_not_exit_zero	cli-upgrade-sweep-gates	
src/harness/cli_tests.rs	a_kill_switch_written_below_another_header_still_reads_off	cli-harness-switch-misplaced-off	
src/harness/cli_tests.rs	a_ledger_tail_keeps_only_the_newest_rows	cli-ledger-verb	
src/harness/cli_tests.rs	a_login_is_recorded_over_a_word_stamped_ahead_of_the_clock	cli-accounts-login-table	
src/harness/cli_tests.rs	a_login_nobody_could_ask_afterwards_is_not_started	cli-accounts-login-table	
src/harness/cli_tests.rs	a_login_that_could_not_be_asked_is_said_and_never_recorded	cli-accounts-login-table	
src/harness/cli_tests.rs	a_refused_roster_is_said_by_name_and_nothing_acts_on_it	cli-accounts-report	
src/harness/cli_tests.rs	a_roster_write_wakes_a_waiting_host_and_a_refused_one_wakes_nobody	cli-accounts-add	
src/harness/cli_tests.rs	a_row_whose_login_directory_is_empty_is_listed_and_not_enrolled	cli-accounts-report	
src/harness/cli_tests.rs	a_state_file_of_another_schema_is_refused_by_every_write_and_left_as_it_is	cli-accounts-schema-refusal	
src/harness/cli_tests.rs	a_tab_another_supervisor_claims_reads_so_unless_a_switch_is_off	cli-upgrade-owner-status	
src/harness/cli_tests.rs	a_tick_cut_short_mid_removal_leaves_the_intent_in_the_journal	cli-disk-write-ahead-journal	
src/harness/cli_tests.rs	accounts_with_no_roster_shows_this_logins_tap_reading	cli-accounts-report	
src/harness/cli_tests.rs	add_refuses_a_listed_or_inadmissible_label_by_name_and_writes_nothing	cli-accounts-add	
src/harness/cli_tests.rs	add_to_an_inline_roster_says_so_and_prints_the_row_to_add_by_hand	cli-accounts-add	
src/harness/cli_tests.rs	add_writes_a_symlinked_roster_through_the_link	cli-accounts-add	
src/harness/cli_tests.rs	an_add_to_a_roster_with_rows_lists_no_login_of_its_own	cli-accounts-add	
src/harness/cli_tests.rs	an_owners_word_kept_out_by_a_sweep_exits_75_and_writes_nothing	cli-upgrade-sweep-gates	
src/harness/cli_tests.rs	an_unreadable_aterm_toml_grants_no_removal_and_the_verb_says_so	cli-disk-config-fail-closed	
src/harness/cli_tests.rs	an_unwatched_upgrade_says_so_and_a_switch_off_says_why	cli-upgrade-owner-status	
src/harness/cli_tests.rs	every_spelling_the_window_parser_reads_reads_the_same_knobs	cli-disk-config-fail-closed	
src/harness/cli_tests.rs	limits_reads_the_sessions_screen_once_and_names_its_wall	cli-limits-read-once	
src/harness/cli_tests.rs	limits_with_no_session_to_read_refuses_and_reads_nothing	cli-limits-read-once	
src/harness/cli_tests.rs	login_at_a_terminal_asks_that_one_login_and_records_that_it_works	cli-accounts-login-table	
src/harness/cli_tests.rs	login_keeps_no_byte_of_the_vendors_output	cli-login-secrecy	
src/harness/cli_tests.rs	login_prints_the_line_and_runs_it_only_at_a_terminal	cli-accounts-login-table	
src/harness/cli_tests.rs	only_the_measured_volumes_targets_are_candidates	cli-disk-tick	
src/harness/cli_tests.rs	the_auth_runner_runs_the_program_it_is_given_under_the_login	cli-login-runners	
src/harness/cli_tests.rs	the_check_after_a_sign_in_needs_user_or_logname	cli-auth-check-env	
src/harness/cli_tests.rs	the_default_window_reclaims_a_profile_idle_two_days_and_keeps_one_built_today	cli-disk-tick	
src/harness/cli_tests.rs	the_disk_journal_is_cut_back_to_its_newest_rows_past_the_bound	cli-disk-write-ahead-journal	
src/harness/cli_tests.rs	the_disk_knobs_are_read_from_aterm_toml	cli-disk-config-fail-closed	
src/harness/cli_tests.rs	the_disk_report_names_nothing_removable_by_default_and_is_journalled	cli-disk-write-ahead-journal	
src/harness/cli_tests.rs	the_disk_tick_leaves_a_target_on_another_volume_alone	cli-disk-tick	
src/harness/cli_tests.rs	the_disk_tick_reclaims_stale_targets_below_the_floor_and_nothing_above	cli-disk-tick	
src/harness/cli_tests.rs	the_first_add_lists_this_machines_own_login_then_the_new_row	cli-accounts-add	
src/harness/cli_tests.rs	the_grammar_parses_what_the_help_text_advertises	cli-grammar	
src/harness/cli_tests.rs	the_grammar_refuses_what_it_does_not_know	cli-grammar	
src/harness/cli_tests.rs	the_json_report_has_every_key_and_null_for_what_nobody_read	cli-accounts-report	
src/harness/cli_tests.rs	the_ledger_verb_reads_the_supervisors_approval_ledger	cli-ledger-verb	
src/harness/cli_tests.rs	the_login_flow_runs_under_the_home_it_is_given	cli-login-runners	
src/harness/cli_tests.rs	the_memory_banner_is_its_own_wall_and_no_limit	cli-limits-view	
src/harness/cli_tests.rs	the_owner_reads_the_recorded_upgrades_and_says_their_word	cli-upgrade-owner-status	
src/harness/cli_tests.rs	the_retired_hook_verbs_drain_a_pipe_and_never_wait_on_a_terminal	cli-retired-verbs	
src/harness/cli_tests.rs	the_tick_removes_nothing_whose_intent_it_cannot_journal	cli-disk-write-ahead-journal	
src/harness/cli_tests.rs	the_verb_applies_cargo_targets_with_the_same_unit	cli-disk-verb-report	
src/harness/cli_tests.rs	the_verb_json_carries_what_each_row_went_kept_and_why	cli-disk-verb-report	
src/harness/cli_tests.rs	the_verb_removes_nothing_whose_intent_it_cannot_journal	cli-disk-write-ahead-journal	
src/harness/cli_tests.rs	the_view_places_a_weekly_walls_bare_clock_after_its_print	cli-limits-view	
src/harness/cli_tests.rs	the_worst_give_up_row_fits_its_cut_whole	cli-upgrade-lines	
src/harness/cli_tests.rs	usage_folds_the_session_claude_code_names_not_the_newest	cli-usage-transcript-pick	
src/harness/cli_tests.rs	usage_names_the_account_its_claude_code_runs_under	cli-usage-account-attribution	
src/harness/cli_tests.rs	usage_shows_the_tap_reading_of_its_own_process	cli-usage-account-attribution	
src/harness/cli_tests.rs	usage_with_a_relative_claude_config_dir_folds_nothing_and_says_so	cli-usage-transcript-pick	
src/harness/cli_tests.rs	use_records_the_owners_pin_and_claims_no_move	cli-accounts-use	
src/harness/cli_tests.rs	use_refuses_an_unlisted_unenrolled_or_signed_out_label_by_name	cli-accounts-use	
src/harness/cli_tests.rs	with_no_roster_the_report_names_the_file_and_the_add_line_and_writes_nothing	cli-accounts-report	
src/harness/disk_discover_tests.rs	a_build_directory_a_checkout_also_names_is_the_checkouts	disk-discover-roots	
src/harness/disk_discover_tests.rs	a_cwd_inside_an_xpy_build_tree_never_reaches_stage0_or_a_sysroot	disk-discover-xpy-rule	
src/harness/disk_discover_tests.rs	a_link_out_of_an_ended_sessions_scratchpad_never_gives_it_what_the_link_names	disk-discover-roots	
src/harness/disk_discover_tests.rs	a_live_owner_a_held_lock_or_a_recent_build_keeps_the_whole_directory	disk-whole-guards-at-removal	
src/harness/disk_discover_tests.rs	a_process_running_from_an_ended_sessions_build_keeps_it	disk-whole-guards-at-removal	
src/harness/disk_discover_tests.rs	a_process_that_cannot_be_read_keeps_a_gone_owners_build_directory	disk-whole-guards-at-removal	
src/harness/disk_discover_tests.rs	a_search_cut_short_in_home_reaches_no_owners_build_directory	disk-discover-budget-and-confinement	
src/harness/disk_discover_tests.rs	a_search_cut_short_keeps_what_it_found_and_says_so	disk-discover-budget-and-confinement	
src/harness/disk_discover_tests.rs	a_session_is_gone_only_when_nothing_names_it_and_nothing_of_it_was_written	disk-session-gone	
src/harness/disk_discover_tests.rs	a_temp_root_another_user_could_write_in_is_not_searched	disk-discover-budget-and-confinement	
src/harness/disk_discover_tests.rs	above_the_floor_or_switched_off_an_ended_sessions_build_is_any_other	disk-whole-pass	
src/harness/disk_discover_tests.rs	an_xpy_checkout_gives_its_bootstrap_build_dirs_and_never_a_sysroot	disk-discover-xpy-rule	
src/harness/disk_discover_tests.rs	every_root_kind_is_found_and_each_build_directory_once	disk-discover-roots	
src/harness/disk_discover_tests.rs	other_apps_data_is_never_entered_from_any_root	disk-discover-roots	
src/harness/disk_discover_tests.rs	the_hogs_a_tick_hands_the_window_name_what_the_pass_left	disk-hogs	
src/harness/disk_discover_tests.rs	the_journal_names_whose_build_directory_went_whole	disk-whole-pass	
src/harness/disk_discover_tests.rs	the_largest_hogs_are_what_no_row_frees_biggest_first	disk-hogs	
src/harness/disk_discover_tests.rs	the_privacy_guarded_folders_are_never_walked_from_home	disk-discover-budget-and-confinement	
src/harness/disk_discover_tests.rs	the_rust_build_cache_gives_each_workspace_build_directory	disk-discover-roots	
src/harness/disk_discover_tests.rs	the_search_stays_on_the_measured_volume_and_out_of_mount_points	disk-discover-budget-and-confinement	
src/harness/disk_pressure_tests.rs	a_build_directory_inside_another_gives_each_profile_one_pressure_row	disk-target-dedupe-nested	
src/harness/disk_pressure_tests.rs	a_build_directory_on_another_volume_gets_no_pressure_row	disk-pressure-pass	
src/harness/disk_pressure_tests.rs	a_failed_re_measure_stops_the_pressure_pass	disk-pressure-pass	
src/harness/disk_pressure_tests.rs	a_locked_profile_is_never_taken_under_pressure_even_when_it_is_the_oldest	disk-pressure-pass	
src/harness/disk_pressure_tests.rs	a_profile_built_within_the_last_minutes_is_kept_under_pressure	disk-pressure-pass	
src/harness/disk_pressure_tests.rs	a_re_measure_that_never_rises_stops_once_the_counted_bytes_cover_the_shortfall	disk-pressure-pass	
src/harness/disk_pressure_tests.rs	a_survey_that_did_not_probe_the_recent_profiles_makes_no_pressure_row	disk-pressure-pass	
src/harness/disk_pressure_tests.rs	above_the_floor_no_recent_profile_is_taken	disk-pressure-pass	
src/harness/disk_pressure_tests.rs	below_the_floor_with_every_profile_recent_the_oldest_goes_first_and_the_pass_stops	disk-pressure-pass	
src/harness/disk_pressure_tests.rs	idle_profiles_go_first_and_whole_before_any_recent_one	disk-pressure-pass	
src/harness/disk_pressure_tests.rs	the_headline_keeps_the_pressure_rows_apart_as_an_upper_bound	disk-report-json-headline	
src/harness/disk_pressure_tests.rs	the_innermost_build_directory_decides_a_nested_profile_between_two_surveys	disk-target-dedupe-nested	
src/harness/disk_pressure_tests.rs	the_pressure_pass_takes_profiles_least_recently_used_first	disk-pressure-pass	
src/harness/disk_pressure_tests.rs	with_a_zero_day_window_a_profile_built_minutes_ago_is_still_never_taken	disk-pressure-pass	
src/harness/disk_target_tests.rs	a_build_holding_any_lock_keeps_its_profile	disk-target-reclaim-locking	
src/harness/disk_target_tests.rs	a_compile_since_the_survey_keeps_the_profile	disk-target-reclaim-locking	
src/harness/disk_target_tests.rs	a_created_lock_takes_the_mode_cargo_gives_it	disk-target-reclaim-locking	
src/harness/disk_target_tests.rs	a_directory_holding_source_is_never_a_candidate	disk-target-assess-witness	
src/harness/disk_target_tests.rs	a_lock_is_released_while_a_child_forked_under_it_has_yet_to_exec	disk-target-reclaim-locking	
src/harness/disk_target_tests.rs	a_missing_lock_is_created_only_when_the_removal_proceeds_and_its_failure_skips	disk-target-reclaim-locking	
src/harness/disk_target_tests.rs	a_probed_lock_is_released_while_a_child_forked_under_the_probe_has_yet_to_exec	disk-target-reclaim-locking	
src/harness/disk_target_tests.rs	a_reclaim_cut_short_leaves_the_profile_idle_for_the_next	disk-target-reclaim-only-incremental	
src/harness/disk_target_tests.rs	a_walk_cut_short_makes_the_whole_directory_not_a_candidate	disk-target-assess-witness	
src/harness/disk_target_tests.rs	a_whole_removal_takes_no_profile_it_did_not_lock_and_deletes_through_its_handle	disk-target-reclaim-lru-whole	
src/harness/disk_target_tests.rs	an_empty_incremental_is_not_a_candidate_and_gets_no_lock	disk-target-assess-witness	
src/harness/disk_target_tests.rs	an_idle_profile_loses_only_its_incremental_cache_and_everything_else_is_byte_identical	disk-target-reclaim-only-incremental	
src/harness/disk_target_tests.rs	an_undeletable_entry_is_reported_and_its_siblings_still_go	disk-target-reclaim-only-incremental	
src/harness/disk_target_tests.rs	an_unread_clock_is_said_never_dated_zero_days	disk-target-assess-witness	
src/harness/disk_target_tests.rs	every_lock_is_held_for_the_whole_delete_and_released_after	disk-target-reclaim-locking	
src/harness/disk_target_tests.rs	nothing_is_reached_through_a_symlink	disk-target-reclaim-only-incremental	
src/harness/disk_target_tests.rs	profiles_are_found_at_any_depth_and_each_is_judged_alone	disk-target-assess-witness	
src/harness/disk_target_tests.rs	reclaim_lru_refuses_a_profile_used_since_the_survey_or_just_built	disk-target-reclaim-lru-whole	
src/harness/disk_target_tests.rs	reclaim_lru_takes_only_the_named_profile_under_every_lock	disk-target-reclaim-lru-whole	
src/harness/disk_target_tests.rs	the_bytes_counted_are_the_blocks_the_unlinks_released	disk-target-reclaim-only-incremental	
src/harness/disk_target_tests.rs	the_delete_never_enters_another_device	disk-target-reclaim-only-incremental	
src/harness/disk_target_tests.rs	the_device_of_a_link_is_the_one_it_resolves_to	disk-target-reclaim-only-incremental	
src/harness/disk_target_tests.rs	the_idle_clock_is_the_cache_directories_only	disk-target-assess-witness	
src/harness/disk_tests.rs	a_build_directory_named_twice_is_one_row	disk-target-dedupe-nested	
src/harness/disk_tests.rs	a_path_outside_the_list_is_refused_with_a_denial_row	disk-apply-guards	
src/harness/disk_tests.rs	a_removal_that_fails_is_a_denial_row_and_does_not_abandon_the_rest	disk-apply-guards	
src/harness/disk_tests.rs	a_superseded_store_build_is_reported_and_delegated_never_removed_here	disk-report-classification	
src/harness/disk_tests.rs	a_synthetic_stale_target_is_reported_and_then_removed_with_its_witness_in_the_row	disk-report-classification	
src/harness/disk_tests.rs	a_target_without_a_marker_or_with_a_fresh_clock_is_never_a_candidate	disk-report-classification	
src/harness/disk_tests.rs	a_version_dir_that_is_not_the_link_target_is_reported_and_the_live_one_is_not	disk-report-classification	
src/harness/disk_tests.rs	age_days_never_reads_a_future_mtime_as_old	disk-age-days	
src/harness/disk_tests.rs	an_unresolvable_live_symlink_blocks_every_version_row_rather_than_guessing	disk-report-classification	
src/harness/disk_tests.rs	dir_bytes_counts_a_tree_and_does_not_follow_a_symlink	disk-tree-nofollow	
src/harness/disk_tests.rs	free_space_warns_only_on_a_figure_it_actually_has	disk-free-warn	
src/harness/disk_tests.rs	nothing_under_the_transcripts_root_is_ever_removed_even_when_it_is_on_the_report	disk-apply-guards	
src/harness/disk_tests.rs	remove_tree_refuses_a_symlink	disk-tree-nofollow	
src/harness/disk_tests.rs	report_only_is_the_default_and_apply_without_a_class_does_nothing	disk-apply-guards	
src/harness/disk_tests.rs	scan_reads_the_versions_dir_and_resolves_the_live_link	disk-report-classification	
src/harness/disk_tests.rs	the_innermost_build_directory_decides_a_nested_profile_in_the_idle_pass	disk-target-dedupe-nested	
src/harness/disk_tests.rs	the_purge_row_is_surfaced_and_never_run_from_here	disk-report-classification	
src/harness/disk_tests.rs	the_report_json_carries_every_row_with_its_witness	disk-report-json-headline	
src/harness/disk_tests.rs	the_safelist_is_closed_and_its_spellings_round_trip	disk-class-table	
src/harness/footer_tests.rs	a_branchs_copied_memory_mode_row_is_not_its_own	footer-process-session-switch-model	
src/harness/footer_tests.rs	a_clear_keeps_the_model_the_process_chose	footer-process-session-switch-model	
src/harness/footer_tests.rs	a_command_pairs_only_with_the_result_directly_under_it	footer-tail-fold-refines-model	
src/harness/footer_tests.rs	a_conversation_born_before_the_switch_is_resumed	footer-process-session-switch-model	
src/harness/footer_tests.rs	a_fifo_is_not_read	open-regular-nonblocking	
src/harness/footer_tests.rs	a_fold_still_behind_shows_no_wall_it_has_not_caught_up_to	footer-fold-is-one-session	
src/harness/footer_tests.rs	a_head_names_its_branch_or_a_short_commit	footer-branch-of-head	
src/harness/footer_tests.rs	a_kept_model_leaves_the_facts_as_they_were	footer-tail-fold-refines-model	
src/harness/footer_tests.rs	a_launch_card_fills_only_open_facts	footer-card-fill	
src/harness/footer_tests.rs	a_launch_pins_the_model_by_flag_or_environment	footer-launch-facts	
src/harness/footer_tests.rs	a_launch_resume_pins_the_model_from_the_start	footer-process-session-switch-model	
src/harness/footer_tests.rs	a_long_path_keeps_its_last_directory	footer-home-and-elide-path	
src/harness/footer_tests.rs	a_model_command_row_pins_whatever_its_result_says	footer-chose-model-since	
src/harness/footer_tests.rs	a_model_result_display_is_the_vendors_name	footer-model-name	
src/harness/footer_tests.rs	a_narrow_pane_cuts_the_hint_between_words_and_the_row_is_still_the_mode_row	footer-mode-row-recognizer	
src/harness/footer_tests.rs	a_narrow_rule_gives_way_title_first_and_the_model_last	footer-fit-rule-ladder	
src/harness/footer_tests.rs	a_new_conversation_or_process_never_shows_the_last_ones_usage	footer-fold-is-one-session	
src/harness/footer_tests.rs	a_reason_and_the_upgrade_note_take_the_accounts_room	footer-fit-rule-ladder	
src/harness/footer_tests.rs	a_registry_entry_must_name_the_pid_it_is_filed_under	footer-session-entry-parse	
src/harness/footer_tests.rs	a_renamed_pill_still_opens_with_a_pill_glyph	footer-mode-row-recognizer	
src/harness/footer_tests.rs	a_response_a_subagent_was_served_since_takes_the_wall_down	footer-fold-is-one-session	
src/harness/footer_tests.rs	a_resume_carries_the_model_only_where_the_process_pinned_it	footer-process-session-switch-model	
src/harness/footer_tests.rs	a_resume_into_a_conversation_begun_since_the_start_pins_it	footer-process-session-switch-model	
src/harness/footer_tests.rs	a_resume_to_another_session_never_pairs_its_model_with_the_old_usage	footer-fold-is-one-session	
src/harness/footer_tests.rs	a_reused_pid_is_refused_by_its_start_time	footer-session-entry-parse	
src/harness/footer_tests.rs	a_switch_from_the_launch_model_names_no_effort_until_one_is_read	footer-process-session-switch-model	
src/harness/footer_tests.rs	a_switch_that_names_no_effort_keeps_it_only_for_the_same_model	footer-tail-fold-refines-model	
src/harness/footer_tests.rs	a_torn_read_of_another_processs_file_shows_nothing	footer-fold-is-one-session	
src/harness/footer_tests.rs	a_torn_read_of_the_same_session_keeps_its_fold	footer-fold-is-one-session	
src/harness/footer_tests.rs	a_worktree_pointing_into_a_provider_domain_is_absent	footer-git-head-resolution	
src/harness/footer_tests.rs	an_answer_past_the_tail_window_is_carried_for_the_same_process	footer-tail-cache-equals-fresh-read	
src/harness/footer_tests.rs	an_effort_result_names_the_effort_at_once	footer-tail-fold-refines-model	
src/harness/footer_tests.rs	an_oversized_newest_answer_is_never_passed_over	footer-tail-cache-equals-fresh-read	
src/harness/footer_tests.rs	an_unreadable_model_choice_voids_model_and_effort	footer-tail-fold-refines-model	
src/harness/footer_tests.rs	an_unstamped_row_is_not_attributed	footer-tail-floor-attribution	
src/harness/footer_tests.rs	every_recorded_screen_has_a_rule_the_footer_can_write	§4	
src/harness/footer_tests.rs	facts_floor_at_the_kernel_else_the_registry_start	footer-floor-of	
src/harness/footer_tests.rs	git_head_follows_a_worktree_file_and_climbs_to_the_top	footer-git-head-resolution	
src/harness/footer_tests.rs	model_ids_read_as_the_vendor_spells_them	footer-model-display	
src/harness/footer_tests.rs	one_footer_read_looks_its_transcript_up_once	footer-fold-is-one-session	
src/harness/footer_tests.rs	only_an_eperm_refusal_reads_as_denied	footer-git-head-resolution	
src/harness/footer_tests.rs	read_git_head_opens_nothing_under_a_file_provider_domain	footer-git-head-resolution	
src/harness/footer_tests.rs	rows_back_meets_an_oversized_row_once_in_its_place	footer-rows-back-window-invariance	
src/harness/footer_tests.rs	shell_cwds_reads_the_live_sessions_launched_in_the_directory	footer-shell-cwds	
src/harness/footer_tests.rs	tail_cache_reuses_only_one_process_and_one_unchanged_file	footer-tail-cache-equals-fresh-read	
src/harness/footer_tests.rs	the_account_costs_only_the_tokens	footer-fit-rule-ladder	
src/harness/footer_tests.rs	the_account_stands_after_the_model_and_before_the_path	footer-segments-order	
src/harness/footer_tests.rs	the_claude_dir_is_the_processs_own	claude-dir-of	
src/harness/footer_tests.rs	the_context_indicator_has_gone_on_recorded_screens	context-watch	
src/harness/footer_tests.rs	the_context_indicator_has_gone_only_from_an_empty_slot	context-watch	
src/harness/footer_tests.rs	the_facts_are_set_as_a_line_of_type	footer-set-facts-typesetting	
src/harness/footer_tests.rs	the_folded_facts_are_this_sessions_transcript	footer-fold-is-one-session	
src/harness/footer_tests.rs	the_last_cwd_is_the_newest_main_thread_rows	footer-last-cwd	
src/harness/footer_tests.rs	the_launch_card_is_read_only_under_a_live_repl	footer-card-fill	
src/harness/footer_tests.rs	the_limit_wall_is_read_from_the_transcripts_row	footer-fold-is-one-session	
src/harness/footer_tests.rs	the_mode_row_is_found_under_the_composer_and_nowhere_else	footer-mode-row-recognizer	
src/harness/footer_tests.rs	the_path_and_branch_follow_the_bash_tools_directory	footer-path-branch-follow	
src/harness/footer_tests.rs	the_path_shows_home_as_a_tilde	footer-home-and-elide-path	
src/harness/footer_tests.rs	the_rule_run_is_the_widest_plain_run	footer-rule-run	
src/harness/footer_tests.rs	the_slug_matches_claude_codes_project_directories	footer-project-slug	
src/harness/footer_tests.rs	the_tail_takes_the_newest_main_thread_model_and_effort	footer-tail-fold-refines-model	
src/harness/lights.rs	a_drawn_pill_this_build_cannot_name_replaces_the_mode_shown	lights-state-and-fold	
src/harness/lights.rs	a_narrow_panes_cut_hint_still_names_the_mode	lights-read-screen	
src/harness/lights.rs	each_toggle_is_claudes_own_input	lights-state-and-fold	
src/harness/lights.rs	every_captured_footer_screen_of_every_build_is_read	§4	
src/harness/lights.rs	every_recorded_screen_reads_definitely_or_not_at_all	§4	
src/harness/lights.rs	no_composer_no_lights_and_no_drive	lights-read-screen	
src/harness/lights.rs	the_2_1_283_screens_read_as_measured	lights-read-screen	
src/harness/lights.rs	the_2_1_301_screens_read_as_measured	lights-read-screen	
src/harness/login_latch_tests.rs	a_closed_tabs_sign_in_is_the_next_tabs	login-wall-acts	
src/harness/login_latch_tests.rs	each_login_has_its_own_sign_in	login-wall-acts	
src/harness/login_latch_tests.rs	the_first_tab_carries_the_sign_in_and_the_others_read_it_back	login-wall-acts	
src/harness/login_latch_tests.rs	the_key_is_the_stored_login_the_process_reads	login-wall-acts	
src/harness/mod.rs	one_line_folds_what_a_hand_rolled_control_sweep_misses	one-line-shortcut-equivalence	
src/harness/mod.rs	truncate_bytes_never_splits_a_character_and_never_grows_a_string	truncate-bytes	
src/harness/netwatch.rs	a_failed_measure_after_a_gap_is_probed_again_within_seconds	netstate-schedule	
src/harness/netwatch.rs	a_stale_measure_reads_unknown_on_either_clock	netstate-reach-freshness	
src/harness/netwatch.rs	asks_expire_churned_sessions_even_while_the_probe_is_stuck	netstate-leases-bounded	
src/harness/netwatch.rs	one_reader_says_whether_a_document_sets_a_status_line_or_turns_hooks_off	netwatch-settings-of	
src/harness/netwatch.rs	one_reader_takes_a_settings_files_env_block_and_its_api_key_helper	netwatch-settings-of	
src/harness/netwatch.rs	since_is_the_runs_start_and_an_overrun_is_unknown	netstate-reach-freshness	
src/harness/netwatch.rs	the_route_is_the_default_only_without_a_base_url_provider_socket_or_proxy	netwatch-route-of	
src/harness/netwatch.rs	the_schedule_probes_at_once_then_3_15_30_60_while_someone_waits_and_parks_when_nobody_does	netstate-schedule	
src/harness/relaunch_tests.rs	a_cold_relaunch_carries_the_proven_active_account_and_nothing_else	relaunch-cold-login	
src/harness/relaunch_tests.rs	a_label_is_a_login_only_through_the_roster	relaunch-account-login	
src/harness/relaunch_tests.rs	a_login_directory_is_typed_literally_or_the_line_is_refused	relaunch-line-env-quoting	
src/harness/relaunch_tests.rs	a_model_switch_onto_a_model_billed_to_usage_credits_is_refused	credits-billed-model-target	
src/harness/relaunch_tests.rs	a_one_shot_run_is_read_wherever_its_flag_stands	relaunch-one-shot	
src/harness/relaunch_tests.rs	a_pill_names_its_mode_flag	relaunch-permission-mode	
src/harness/relaunch_tests.rs	a_relaunch_moves_to_a_newer_build_only_while_the_upgrade_is_on	relaunch-target-build	
src/harness/relaunch_tests.rs	a_relaunch_resumes_the_mode_the_conversation_ran_in	relaunch-permission-mode	
src/harness/relaunch_tests.rs	a_shell_that_has_the_tab_back_reads_as_ended_only_once_its_line_ran	relaunch-ended-at-once-shell-look	
src/harness/relaunch_tests.rs	a_snapshot_follows_its_own_process_and_nothing_else	relaunch-snapshot-follow	
src/harness/relaunch_tests.rs	a_store_binarys_line_exports_its_shims_env_and_a_bare_build_adds_nothing	relaunch-line-shim-env	
src/harness/relaunch_tests.rs	a_store_binarys_long_line_drops_the_survey_switch_and_keeps_the_updater_switch	relaunch-line-shim-env	
src/harness/relaunch_tests.rs	a_step_is_relaunched_left_not_yet_or_never	relaunch-on-exit-refines-model	
src/harness/relaunch_tests.rs	an_account_move_over_another_restart_in_flight_is_not_tried	relaunch-carried-word	
src/harness/relaunch_tests.rs	an_account_moves_record_is_a_relaunch_that_ended_its_own_agent	relaunch-cause-classes	
src/harness/relaunch_tests.rs	an_exit_is_promised_a_relaunch_only_where_the_line_can_hold_its_login	relaunch-promise-iff-plan	
src/harness/relaunch_tests.rs	an_exit_is_relaunched_only_where_the_relaunch_would_plan_it	relaunch-promise-iff-plan	
src/harness/relaunch_tests.rs	an_exit_is_relaunched_unless_someone_owns_it_or_it_is_limited	relaunch-on-exit-refines-model	
src/harness/relaunch_tests.rs	the_back_off_grows_caps_and_forgives_a_healthy_run	relaunch-on-exit-refines-model	
src/harness/relaunch_tests.rs	the_conversations_last_permission_mode_is_read_from_its_transcript	relaunch-permission-mode	
src/harness/relaunch_tests.rs	the_look_at_an_exit_waits_out_its_own_removal_and_no_longer	relaunch-exit-record-look	
src/harness/relaunch_tests.rs	the_program_is_the_script_or_the_image_that_is_claude	relaunch-program-identity	
src/harness/relaunch_tests.rs	the_real_relaunch_decisions_conform_to_the_model	relaunch-on-exit-refines-model	
src/harness/relaunch_tests.rs	the_relaunch_line_carries_the_claude_home_its_agent_ran_under	relaunch-line-env-quoting	
src/harness/relaunch_tests.rs	the_relaunch_line_carries_the_login_in_every_shells_form	relaunch-line-env-quoting	
src/harness/relaunch_tests.rs	the_relaunch_line_carries_the_mode	relaunch-permission-mode	
src/harness/relaunch_tests.rs	the_resumed_prompt_is_one_line_and_keeps_going	relaunch-resumed-prompt-one-line	
src/harness/relaunch_tests.rs	the_tap_is_dropped_before_the_required_login	relaunch-line-drop-order	
src/harness/relaunch_tests.rs	the_tap_rides_before_the_resume_in_every_shells_form	relaunch-line-env-quoting	
src/harness/repo_work_tests.rs	a_clean_probe_cut_at_its_head_read_reads_as_before_the_head_was_read	repo-work-probe	
src/harness/repo_work_tests.rs	a_clean_reading_over_a_count_git_refused_names_nothing	repo-work-probe	
src/harness/repo_work_tests.rs	a_clean_repository_reads_its_head_and_main	repo-work-probe	
src/harness/repo_work_tests.rs	a_guarded_folder_is_never_probed	repo-work-probe	
src/harness/repo_work_tests.rs	a_guarded_repository_with_work_reads_clean_and_names_nothing	repo-work-probe	
src/harness/repo_work_tests.rs	a_long_path_keeps_its_end	repo-work-probe	
src/harness/repo_work_tests.rs	the_probe_finds_uncommitted_unpushed_and_stashed_work_and_nothing_once_pushed	repo-work-probe	
src/harness/repo_work_tests.rs	the_reads_output_is_counted_and_a_clean_repository_is_no_finding	repo-work-probe	
src/harness/repo_work_tests.rs	the_session_works_in_the_transcript_s_last_cwd_else_where_it_started	repo-work-probe	
src/harness/restart_stamp_tests.rs	a_stamp_is_one_file_per_tab_written_whole	restart-stamp-file	
src/harness/restart_stamp_tests.rs	taking_a_stamp_takes_only_the_one_meant	restart-stamp-take-if	
src/harness/restart_stamp_tests.rs	the_act_reads_a_start_in_the_snapshots_own_spelling	kernel-start-spelling	
src/harness/restart_stamp_tests.rs	the_lookup_names_its_exit_and_sweeps_what_is_over	restart-stamp-lookup-prune	
src/harness/restart_stamp_tests.rs	the_prune_removes_only_a_stale_stamp	restart-stamp-lookup-prune	
src/harness/restart_stamp_tests.rs	the_stamp_standing_for_a_process_is_the_one_its_exit_is_read_by	restart-stamp-lookup-prune	
src/harness/resume_tests.rs	a_named_resume_line_is_kept_whole_bare_or_not_at_all	resume-never-cut	
src/harness/resume_tests.rs	the_line_carries_the_launch_flags_as_the_relaunch_does	resume-command-flags	
src/harness/resume_tests.rs	two_records_in_one_directory_each_pid_resumes_its_own_conversation	resume-line-of-pid	
src/harness/session_usage_tests.rs	a_budget_bounds_one_read_and_the_next_carries_on	usage-incremental-fold-equals-one-pass	
src/harness/session_usage_tests.rs	a_fifo_is_not_folded	open-regular-nonblocking	
src/harness/session_usage_tests.rs	a_file_rewritten_in_place_is_caught_by_the_boundary_byte	usage-incremental-fold-equals-one-pass	
src/harness/session_usage_tests.rs	a_finished_subagent_keeps_its_totals_and_grows_on	usage-session-aggregate	
src/harness/session_usage_tests.rs	a_line_that_is_not_utf8_is_counted_unparsed	usage-incremental-fold-equals-one-pass	
src/harness/session_usage_tests.rs	a_missing_transcript_is_not_caught_up	usage-session-aggregate	
src/harness/session_usage_tests.rs	a_quiet_file_behind_is_not_released	usage-session-aggregate	
src/harness/session_usage_tests.rs	a_quiet_subagent_keeps_only_its_totals	usage-session-aggregate	
src/harness/session_usage_tests.rs	a_released_file_changed_but_appended_is_unread	usage-session-aggregate	
src/harness/session_usage_tests.rs	a_reread_reads_only_the_appended_bytes	usage-read-cost	
src/harness/session_usage_tests.rs	a_reset_in_the_next_year_is_placed	usage-wall-of	
src/harness/session_usage_tests.rs	a_response_served_since_takes_the_wall_down	usage-wall-of	
src/harness/session_usage_tests.rs	a_rotated_file_starts_the_fold_over	usage-incremental-fold-equals-one-pass	
src/harness/session_usage_tests.rs	a_routed_model_id_is_named_as_its_model	usage-text-and-model-naming	
src/harness/session_usage_tests.rs	a_saturated_count_is_named_without_overflow	usage-text-and-model-naming	
src/harness/session_usage_tests.rs	a_session_sums_its_subagent_transcripts_and_nothing_else	usage-session-aggregate	
src/harness/session_usage_tests.rs	a_span_is_said_as_the_local_clock	usage-wall-of	
src/harness/session_usage_tests.rs	a_spent_budget_opens_no_more_files	usage-session-aggregate	
src/harness/session_usage_tests.rs	a_subagent_behind_keeps_the_session_from_catching_up	usage-session-aggregate	
src/harness/session_usage_tests.rs	a_truncated_file_starts_the_fold_over	usage-incremental-fold-equals-one-pass	
src/harness/session_usage_tests.rs	a_walk_cut_short_is_said_on_the_glass	usage-session-aggregate	
src/harness/session_usage_tests.rs	a_wall_across_a_dst_jump_stands_its_real_time	usage-wall-of	
src/harness/session_usage_tests.rs	a_weekly_clock_time_is_its_next_occurrence	usage-wall-of	
src/harness/session_usage_tests.rs	an_oversize_line_across_reads_is_skipped_and_counted	usage-incremental-fold-equals-one-pass	
src/harness/session_usage_tests.rs	an_unchanged_file_is_not_opened	usage-read-cost	
src/harness/session_usage_tests.rs	an_unplaceable_wall_shows_nothing	usage-wall-of	
src/harness/session_usage_tests.rs	an_unreadable_subagent_is_unread_and_nothing_else	usage-session-aggregate	
src/harness/session_usage_tests.rs	another_zones_clock_keeps_its_zone	usage-wall-of	
src/harness/session_usage_tests.rs	any_split_of_the_appends_folds_to_the_one_pass_result	usage-incremental-fold-equals-one-pass	
src/harness/session_usage_tests.rs	at_the_bound_new_files_are_unread	usage-session-aggregate	
src/harness/session_usage_tests.rs	models_are_named_short_and_ordered_by_tokens	usage-text-and-model-naming	
src/harness/session_usage_tests.rs	moved_sees_a_new_file_at_any_depth	usage-moved	
src/harness/session_usage_tests.rs	moved_sees_subagent_growth_and_an_unfinished_fold	usage-moved	
src/harness/session_usage_tests.rs	moved_watches_every_followed_file	usage-moved	
src/harness/session_usage_tests.rs	no_line_buffer_is_kept_between_reads	usage-read-cost	
src/harness/session_usage_tests.rs	the_segment_says_runtime_and_tokens_at_each_grain	usage-text-and-model-naming	
src/harness/session_usage_tests.rs	the_wall_stands_until_its_reset	usage-wall-of	
src/harness/session_usage_tests.rs	token_counts_are_short_and_never_rounded_up	usage-tokens-short	
src/harness/session_usage_tests.rs	unread_is_said_with_no_tokens_shown	usage-text-and-model-naming	
src/harness/tap_tests.rs	a_reading_is_read_only_from_its_own_processes_file	tap-read-own	
src/harness/tap_tests.rs	a_silent_mark_ends_with_its_process	tap-silence-machine	
src/harness/tap_tests.rs	a_sweep_takes_the_dead_and_the_old_and_nothing_else	tap-sweep	
src/harness/tap_tests.rs	a_tap_is_silent_only_after_two_strikes_on_a_served_process	tap-silence-machine	
src/harness/tap_tests.rs	each_guard_alone_refuses_the_tap	tap-gate	
src/harness/tap_tests.rs	freshness_admits_news_and_never_an_idle_repeat	tap-freshness	
src/harness/tap_tests.rs	the_tap_value_is_exact_and_only_it_is_ours	tap-value-ours	
src/harness/tap_tests.rs	the_taps_row_reads_idle_and_keeps_the_mode_row_for_every_reader	tap-row-safe	
src/harness/tap_tests.rs	the_verb_never_writes_a_secret_it_was_handed	tap-noninterference	
src/harness/tap_tests.rs	the_verb_writes_its_reading_and_always_prints_one_line_and_exits_zero	tap-verb-total	
src/harness/timings.rs	the_fast_clock_keeps_the_production_orders_and_its_seam_is_never_inherited	harness-timings-orders	
src/harness/transcript.rs	discarded_fields_are_still_fully_validated	transcript-projection-refines-full-parse	
src/harness/transcript.rs	projections_match_full_values_including_vendor_type_drift_and_duplicates	transcript-projection-refines-full-parse	
src/harness/transcript_corpus_tests.rs	transcript_projections_answer_as_recorded	transcript-projection-refines-full-parse	
src/harness/upgrade_catalog.rs	a_hostile_value_is_not_a_model	catalog-parse-window	
src/harness/upgrade_catalog.rs	build_facts_round_trip_through_the_disk_cache	catalog-build-facts-roundtrip	
src/harness/upgrade_catalog.rs	the_280_literal_parses	catalog-parse-window	
src/harness/upgrade_catalog.rs	the_scan_finds_a_marker_across_a_chunk_boundary	catalog-scan-chunk-invariance	
src/harness/upgrade_catalog.rs	the_twin_names_its_binary_on_its_last_exec_line	catalog-twin-binary	
src/harness/codex_alab_drive_tests.rs	a_registry_vouches_only_for_a_state_root_of_its_own_kind	codex-prune-refines-model	
src/harness/codex_alab_drive_tests.rs	the_prune_collects_a_closed_tabs_settled_codex_record_and_nothing_else	codex-prune-refines-model	
src/harness/codex_alab_tests.rs	a_thread_id_is_a_hyphenated_uuid_and_nothing_else	codex-thread-ids	
src/harness/codex_alab_tests.rs	a_version_is_read_from_the_package_that_names_the_binary_never_by_running_it	codex-package-version	
src/harness/codex_alab_tests.rs	an_unknown_or_unresumable_launch_is_refused_not_guessed	codex-rewrite-argv	
src/harness/codex_alab_tests.rs	only_an_interactive_tui_is_a_tui_never_the_daemon_or_its_updater	codex-is-tui-argv	
src/harness/codex_alab_tests.rs	the_measured_manifest_parses_and_a_changed_layout_refuses	codex-package-version	
src/harness/codex_alab_tests.rs	the_relaunch_on_exit_reads_a_thread_and_its_instant	codex-thread-ids	
src/harness/codex_alab_tests.rs	the_rewrite_resumes_the_thread_with_every_kept_flag	codex-rewrite-argv	
src/harness/upgrade_drive_tests.rs	a_barred_ride_is_dropped_once_and_the_round_goes_on_without_it	model-ask-ledger	
src/harness/upgrade_drive_tests.rs	a_bash_prompt_with_typing_after_its_mark_is_never_typed_at	relaunch-prompt-read	
src/harness/upgrade_drive_tests.rs	a_break_of_background_work_takes_a_notice_and_ends_nothing	presence-settle-ladder-gates	
src/harness/upgrade_drive_tests.rs	a_build_that_failed_to_start_for_any_conversation_moves_no_agent_onto_it	failed-start-gate	
src/harness/upgrade_drive_tests.rs	a_build_that_failed_to_start_is_moved_onto_only_once_proved_to_start	failed-start-gate	
src/harness/upgrade_drive_tests.rs	a_build_that_failed_to_start_is_not_retried_for_the_conversations_hand_resume	failed-start-gate	
src/harness/upgrade_drive_tests.rs	a_build_the_conversations_record_holds_it_off_is_no_relaunch_target	owner-word-survives-restart	
src/harness/upgrade_drive_tests.rs	a_build_whose_version_timed_out_is_unread_never_not_due	due-never-drops-unread	
src/harness/upgrade_drive_tests.rs	a_caret_moved_home_over_a_half_typed_command_is_typing	relaunch-prompt-read	
src/harness/upgrade_drive_tests.rs	a_cell_reply_is_dim_only_by_its_attrs	composer-empty-caret-dim	
src/harness/upgrade_drive_tests.rs	a_continuation_on_a_screen_that_keeps_moving_is_still_typed	typed-turn-fence	
src/harness/upgrade_drive_tests.rs	a_conversation_is_under_the_home_its_transcript_is_filed_in	claude-home-resolution	
src/harness/upgrade_drive_tests.rs	a_conversation_resumed_by_hand_after_a_no_resume_stop_is_never_reopened	relaunch-adoption	
src/harness/upgrade_drive_tests.rs	a_conversation_resumed_in_another_tab_is_retabbed_at_the_attach	age-is-one-process	
src/harness/upgrade_drive_tests.rs	a_conversation_with_no_task_is_carried_on_with_nothing	task-detection-and-fresh-restart	
src/harness/upgrade_drive_tests.rs	a_crash_read_at_its_exit_is_relaunched_whatever_removes_its_record_later	exit-record-refinement	
src/harness/upgrade_drive_tests.rs	a_crashed_native_sessions_riding_model_is_the_native_builds	model-evidence-scope	
src/harness/upgrade_drive_tests.rs	a_dead_or_foreign_sibling_leaves_the_roster_whole	session-roster-integrity	
src/harness/upgrade_drive_tests.rs	a_declining_agent_is_asked_a_bounded_number_of_times_a_day	stop-rest-ladder	
src/harness/upgrade_drive_tests.rs	a_displaced_grid_never_makes_a_placeholder_a_draft	composer-empty-caret-dim	
src/harness/upgrade_drive_tests.rs	a_draft_begun_before_the_signal_vetoes_it	last-look-before-signal	
src/harness/upgrade_drive_tests.rs	a_drain_a_person_holds_past_its_bound_voids_the_answer_and_asks_again	drain-void	
src/harness/upgrade_drive_tests.rs	a_dry_runs_connection_reads_and_has_no_hands	upgrade-dry-run-no-hands	relay-read-only-connection
src/harness/upgrade_drive_tests.rs	a_due_model_rides_a_restart_made_for_another_reason	model-ask-ledger	
src/harness/upgrade_drive_tests.rs	a_failed_model_on_the_launch_line_is_dropped_by_a_relaunch_asking_none	relaunch-line-construction	
src/harness/upgrade_drive_tests.rs	a_failed_start_is_noted_against_its_build_once_by_the_one_writer	failed-start-gate	
src/harness/upgrade_drive_tests.rs	a_footer_quoted_in_the_transcript_is_no_turn_running	busy-footer-live-zone	
src/harness/upgrade_drive_tests.rs	a_fresh_restart_types_nothing_plans_no_resume_and_keeps_every_last_look	task-detection-and-fresh-restart	
src/harness/upgrade_drive_tests.rs	a_give_up_at_a_break_types_the_release_it_owes	release-owed-is-paid-once	
src/harness/upgrade_drive_tests.rs	a_gone_notice_leaves_a_new_round_pending_in_the_holders_tab	notice-owner-refinement	
src/harness/upgrade_drive_tests.rs	a_gone_notices_row_promises_no_ask_to_a_holder_already_current	notice-owner-refinement	
src/harness/upgrade_drive_tests.rs	a_hand_resume_keeps_the_stop_of_the_build_that_failed_to_start_for_it	failed-start-gate	
src/harness/upgrade_drive_tests.rs	a_hung_exit_past_its_bound_is_noted_once_and_nothing_is_forced	in-flight-orphan-pass	
src/harness/upgrade_drive_tests.rs	a_hung_exit_with_no_session_file_is_noted_once_by_the_orphan_pass	in-flight-orphan-pass	
src/harness/upgrade_drive_tests.rs	a_key_or_a_prompt_after_the_first_look_stops_the_restart_before_its_signal	last-look-before-signal	
src/harness/upgrade_drive_tests.rs	a_labelled_top_rule_is_the_input_box_the_notice_fences_on	typed-turn-fence	
src/harness/upgrade_drive_tests.rs	a_launch_is_waited_on_for_its_record_only_within_its_window	due-never-drops-unread	
src/harness/upgrade_drive_tests.rs	a_launch_noted_once_its_record_is_there_is_behind_from_the_attach	age-is-one-process	
src/harness/upgrade_drive_tests.rs	a_launch_that_cannot_be_carried_is_refused_before_the_agent_is_asked_to_wind_down	announce-preflight	
src/harness/upgrade_drive_tests.rs	a_listed_model_replaces_every_launch_model_spelling	relaunch-line-construction	
src/harness/upgrade_drive_tests.rs	a_live_relaunch_on_exit_keeps_its_wait_when_its_shell_has_the_tab	relaunch-ended-verdict	
src/harness/upgrade_drive_tests.rs	a_mark_that_names_another_row_is_no_persons_typing	relaunch-prompt-read	
src/harness/upgrade_drive_tests.rs	a_model_billed_to_usage_credits_is_offered_to_no_upgrade_under_the_key_off	credits-billed-model-target	model-availability-decision
src/harness/upgrade_drive_tests.rs	a_move_onto_a_build_that_fails_only_at_a_real_start_is_taken_back_onto_the_build_it_ran	failed-start-gate	
src/harness/upgrade_drive_tests.rs	a_native_session_is_judged_by_the_native_builds_own_offer	model-evidence-scope	
src/harness/upgrade_drive_tests.rs	a_new_upgrade_waits_for_the_last_restarts_model	model-ask-ledger	
src/harness/upgrade_drive_tests.rs	a_newer_build_landing_in_a_rest_keeps_the_process_s_history	retarget-arms	
src/harness/upgrade_drive_tests.rs	a_newer_target_keeps_the_age_and_the_owners_word_but_not_a_skip	retarget-arms	
src/harness/upgrade_drive_tests.rs	a_notice_the_host_cut_short_is_never_announced_or_typed_again	pressed-text-delivery	
src/harness/upgrade_drive_tests.rs	a_notice_the_host_stopped_stands_unsent_and_is_never_pressed_back	left-typed-never-erased	
src/harness/upgrade_drive_tests.rs	a_notice_whose_process_is_gone_is_asked_afresh_of_the_live_holder	notice-owner-refinement	
src/harness/upgrade_drive_tests.rs	a_parent_that_cannot_be_read_is_no_verdict_on_the_relaunch	relaunch-by-parent	
src/harness/upgrade_drive_tests.rs	a_pass_reads_every_claude_home_its_tabs_agents_run_under	claude-home-resolution	
src/harness/upgrade_drive_tests.rs	a_pending_round_that_owes_its_agent_a_release_is_carried_on_not_quiet	release-owed-is-paid-once	
src/harness/upgrade_drive_tests.rs	a_person_at_the_tab_owns_the_turn_end_for_their_grace	turn-end-ownership	
src/harness/upgrade_drive_tests.rs	a_pressed_notice_leaves_an_open_survey_to_the_person	pressed-text-delivery	
src/harness/upgrade_drive_tests.rs	a_pressed_notices_enter_is_guarded_on_its_own_last_row	pressed-text-delivery	
src/harness/upgrade_drive_tests.rs	a_pressed_notices_turn_waits_out_a_late_echo_as_a_hang	pressed-text-delivery	
src/harness/upgrade_drive_tests.rs	a_quiet_pending_round_types_the_release_it_owes_where_work_holds_its_restart	release-owed-is-paid-once	
src/harness/upgrade_drive_tests.rs	a_quiet_restart_is_carried_on_with_nothing	quiet-flow	
src/harness/upgrade_drive_tests.rs	a_quiet_round_an_older_build_announced_gives_up_and_releases_its_agent	release-owed-is-paid-once	
src/harness/upgrade_drive_tests.rs	a_quiet_upgrade_restarts_an_idle_conversation_with_nothing_typed_first	quiet-flow	
src/harness/upgrade_drive_tests.rs	a_quiet_wait_on_the_agents_work_owns_no_turn_end	turn-end-ownership	
src/harness/upgrade_drive_tests.rs	a_ready_answer_is_heard_at_its_turn_end_and_a_superseded_one_is_asked_again	ready-consent	
src/harness/upgrade_drive_tests.rs	a_ready_answer_is_read_when_the_tail_window_starts_inside_a_character	tail-window-bounded	
src/harness/upgrade_drive_tests.rs	a_ready_answer_the_owners_hold_superseded_is_never_signalled_after_it	ready-consent	
src/harness/upgrade_drive_tests.rs	a_record_names_its_agent_or_is_no_record	state-record-roundtrip	
src/harness/upgrade_drive_tests.rs	a_record_of_an_unknown_agent_is_passed_by_and_left_as_it_lies	wait-log-ledger-and-prune-bounds	
src/harness/upgrade_drive_tests.rs	a_refusal_with_a_model_due_is_said_once_not_every_pass	model-ask-ledger	
src/harness/upgrade_drive_tests.rs	a_refused_signal_takes_back_the_model_it_asked	model-ask-ledger	
src/harness/upgrade_drive_tests.rs	a_relative_claude_home_is_followed_by_the_relaunch_and_the_restart_in_place	claude-home-resolution	
src/harness/upgrade_drive_tests.rs	a_relative_claude_home_the_line_would_move_is_refused	claude-home-resolution	
src/harness/upgrade_drive_tests.rs	a_relaunch_handed_to_its_loop_ends_at_adoption_and_is_owed_to_its_tab	relaunch-adoption	
src/harness/upgrade_drive_tests.rs	a_relaunch_in_flight_finds_its_process_under_the_home_its_line_carried	claude-home-resolution	
src/harness/upgrade_drive_tests.rs	a_relaunch_line_waits_on_a_live_owner_under_its_agents_own_home	no-act-without-ownership-proofs	
src/harness/upgrade_drive_tests.rs	a_relaunch_on_exit_needs_an_exit_and_a_conversation_on_record	exit-record-refinement	
src/harness/upgrade_drive_tests.rs	a_relaunch_stopped_no_resume_whose_agent_came_up_is_carried_on	relaunch-adoption	
src/harness/upgrade_drive_tests.rs	a_relaunch_that_never_carries_on_is_noted_once_past_its_bound	in-flight-orphan-pass	
src/harness/upgrade_drive_tests.rs	a_relaunch_typed_at_bash_takes_the_key_its_live_shell_names	relaunch-line-dialect-and-rekey	
src/harness/upgrade_drive_tests.rs	a_relaunch_whose_parent_cannot_be_read_is_waited_for_never_failed	relaunch-adoption	
src/harness/upgrade_drive_tests.rs	a_relaunch_whose_shell_has_not_run_its_line_yet_is_waited_on	relaunch-ended-verdict	
src/harness/upgrade_drive_tests.rs	a_report_records_its_wait_and_an_act_clears_it	wait-log-ledger-and-prune-bounds	
src/harness/upgrade_drive_tests.rs	a_restart_holds_the_tab_from_its_last_look_and_gives_it_back_when_it_stops	hand-lease-bracket	
src/harness/upgrade_drive_tests.rs	a_restart_in_flight_is_stale_from_its_seen_exit_not_its_signal	in-flight-orphan-pass	
src/harness/upgrade_drive_tests.rs	a_restart_in_flight_too_long_or_without_its_shell_never_acts_on_the_tab	in-flight-orphan-pass	
src/harness/upgrade_drive_tests.rs	a_restart_in_place_is_planned_on_the_same_program_and_carried_on_once	restart-carries-login-and-tap	
src/harness/upgrade_drive_tests.rs	a_restart_in_place_records_its_model_ask_only_with_the_line_that_asks_it	model-ask-ledger	
src/harness/upgrade_drive_tests.rs	a_restart_in_place_records_the_directory_a_relative_homes_agent_files_under	claude-home-resolution	
src/harness/upgrade_drive_tests.rs	a_restart_in_place_saves_the_fallback_origin_it_decides	model-way-back	
src/harness/upgrade_drive_tests.rs	a_restart_in_place_types_back_the_login_its_process_had	restart-carries-login-and-tap	
src/harness/upgrade_drive_tests.rs	a_restart_in_place_types_the_usage_tap_under_the_switch_and_writes_no_settings	restart-carries-login-and-tap	
src/harness/upgrade_drive_tests.rs	a_restart_keeps_the_move_clock_only_of_a_record_about_the_build_it_keeps	owner-word-survives-restart	
src/harness/upgrade_drive_tests.rs	a_restart_left_between_exit_and_relaunch_is_resumed_by_the_next_sweep	in-flight-orphan-pass	
src/harness/upgrade_drive_tests.rs	a_restart_never_resumes_a_conversation_whose_transcript_went_after_its_step	task-detection-and-fresh-restart	
src/harness/upgrade_drive_tests.rs	a_restart_on_its_own_program_keeps_the_owners_hold_and_the_round_after_it_too	owner-word-survives-restart	
src/harness/upgrade_drive_tests.rs	a_restart_on_its_own_program_keeps_the_stop_of_a_build_that_failed_to_start	failed-start-gate	
src/harness/upgrade_drive_tests.rs	a_restart_records_the_model_its_ready_answer_ran_and_where_that_read_ended	model-ask-ledger	
src/harness/upgrade_drive_tests.rs	a_restart_says_the_model_before_and_confirms_the_one_after	model-ask-ledger	
src/harness/upgrade_drive_tests.rs	a_restart_that_stops_keeps_the_owners_skip_of_the_build_it_held_off	owner-word-survives-restart	
src/harness/upgrade_drive_tests.rs	a_restart_whose_agent_is_not_reaped_yet_says_when_it_ended	in-flight-orphan-pass	
src/harness/upgrade_drive_tests.rs	a_restart_whose_agent_the_orphan_pass_sees_gone_says_when	in-flight-orphan-pass	
src/harness/upgrade_drive_tests.rs	a_restart_without_a_mark_is_unconfirmed_at_once	model-ask-ledger	
src/harness/upgrade_drive_tests.rs	a_restarts_line_is_judged_by_the_block_it_was_typed_into	relaunch-ended-verdict	
src/harness/upgrade_drive_tests.rs	a_resumed_session_that_has_not_answered_is_unconfirmed	model-ask-ledger	
src/harness/upgrade_drive_tests.rs	a_retarget_is_told_by_the_arm_that_made_it	retarget-arms	
src/harness/upgrade_drive_tests.rs	a_reused_state_names_the_build_the_sweep_would_start_now	retarget-arms	
src/harness/upgrade_drive_tests.rs	a_roster_reads_every_home_whole_or_not_at_all	session-roster-integrity	
src/harness/upgrade_drive_tests.rs	a_screen_reply_is_read_with_the_cursor_made_relative_to_its_rows	screen-reply-parse	
src/harness/upgrade_drive_tests.rs	a_second_fallback_from_a_launch_with_no_model_goes_back_to_no_model	model-way-back	
src/harness/upgrade_drive_tests.rs	a_second_sweeper_does_nothing_while_the_first_holds_the_lock	sweep-lock-exclusive	
src/harness/upgrade_drive_tests.rs	a_session_upgraded_before_is_behind_anew_when_a_newer_build_lands	age-is-one-process	
src/harness/upgrade_drive_tests.rs	a_state_main_wrote_reads_with_the_owners_fields_at_their_defaults	state-record-roundtrip	
src/harness/upgrade_drive_tests.rs	a_stop_that_repeats_rests_longer_and_never_for_good	stop-rest-ladder	
src/harness/upgrade_drive_tests.rs	a_stopped_rounds_retarget_keeps_what_says_its_tui_was_told_to_wind_down	retarget-arms	
src/harness/upgrade_drive_tests.rs	a_stopped_upgrade_retargeted_keeps_its_clock_word_and_streak	retarget-arms	
src/harness/upgrade_drive_tests.rs	a_sweep_never_signals_its_own_ancestry	ancestry-guard	
src/harness/upgrade_drive_tests.rs	a_tab_is_held_by_a_halt_a_hand_a_person_or_an_unreadable_status	held-by-status	
src/harness/upgrade_drive_tests.rs	a_terminal_is_the_tabs_only_when_aterm_opened_it	terminal-owner	
src/harness/upgrade_drive_tests.rs	a_timed_out_version_probe_is_retried_on_a_back_off	version-cache-backoff	
src/harness/upgrade_drive_tests.rs	a_transcript_append_cannot_grow_a_tail_read_past_its_budget	tail-window-bounded	
src/harness/upgrade_drive_tests.rs	a_turn_begun_since_the_look_takes_no_notice_and_no_release_under_the_rows_fence	last-look-before-signal	
src/harness/upgrade_drive_tests.rs	a_version_probe_is_bounded_whole	bounded-child-probe	
src/harness/upgrade_drive_tests.rs	a_visits_own_retarget_is_said_once_in_the_ledger	retarget-arms	
src/harness/upgrade_drive_tests.rs	a_wide_prompt_glyph_never_hides_a_persons_typing	relaunch-prompt-read	
src/harness/upgrade_drive_tests.rs	a_yield_that_times_out_never_fails_the_relaunch	relaunch-prompt-read	
src/harness/upgrade_drive_tests.rs	after_a_step_the_host_parks_waits_longer_stops_or_says_it_is_broken	step-word-classification	
src/harness/upgrade_drive_tests.rs	an_account_move_is_relaunched_over_a_sign_in_menu	restart-carries-login-and-tap	login-wall-acts
src/harness/upgrade_drive_tests.rs	an_account_move_types_the_rosters_login_and_a_label_nobody_lists_is_refused	restart-carries-login-and-tap	
src/harness/upgrade_drive_tests.rs	an_age_belongs_to_one_process	age-is-one-process	
src/harness/upgrade_drive_tests.rs	an_agent_no_job_control_shell_started_is_refused_before_anything_is_typed	no-act-without-ownership-proofs	
src/harness/upgrade_drive_tests.rs	an_agent_on_a_pty_the_tab_does_not_own_is_never_driven_through_the_tab	no-act-without-ownership-proofs	
src/harness/upgrade_drive_tests.rs	an_agent_under_an_empty_claude_home_is_judged_by_the_profile_claude_code_keeps_for_it	model-evidence-scope	
src/harness/upgrade_drive_tests.rs	an_agent_under_another_claude_home_is_judged_by_its_homes_model_files	model-evidence-scope	
src/harness/upgrade_drive_tests.rs	an_agent_under_another_claude_home_is_relaunched_off_its_own_records	claude-home-resolution	
src/harness/upgrade_drive_tests.rs	an_agent_whose_host_ended_is_relaunched_in_its_reopened_tab	exit-record-refinement	
src/harness/upgrade_drive_tests.rs	an_agent_whose_records_a_worktree_split_waits_and_is_asked_nothing	claude-home-resolution	
src/harness/upgrade_drive_tests.rs	an_agents_own_home_keeps_its_records_and_its_profile	claude-home-resolution	
src/harness/upgrade_drive_tests.rs	an_announced_round_carrying_a_model_is_looked_at_with_the_build_current	due-never-drops-unread	
src/harness/upgrade_drive_tests.rs	an_announcement_is_planned_in_the_mode_the_pill_shows	continuation-words-and-mode	
src/harness/upgrade_drive_tests.rs	an_ask_that_ran_is_never_recorded_failed_when_a_person_moves_off_it	model-ask-ledger	
src/harness/upgrade_drive_tests.rs	an_attended_ready_session_is_never_signalled_without_the_owners_now	presence-settle-ladder-gates	
src/harness/upgrade_drive_tests.rs	an_attended_tab_has_a_person_at_it_and_is_said_once	presence-settle-ladder-gates	
src/harness/upgrade_drive_tests.rs	an_exit_relaunch_saves_the_fallback_origin_it_decides	model-way-back	
src/harness/upgrade_drive_tests.rs	an_exit_relaunch_types_back_the_login_the_agent_ran_under	restart-carries-login-and-tap	
src/harness/upgrade_drive_tests.rs	an_exit_relaunch_with_no_record_carries_no_choice_older_than_the_process	model-way-back	
src/harness/upgrade_drive_tests.rs	an_exit_the_harness_own_restart_made_is_never_read_as_graceful	exit-record-refinement	
src/harness/upgrade_drive_tests.rs	an_idle_look_of_another_process_counts_toward_no_lag	age-is-one-process	
src/harness/upgrade_drive_tests.rs	an_idle_screen_over_claudes_own_work_is_asked_at_its_second_look	presence-settle-ladder-gates	
src/harness/upgrade_drive_tests.rs	an_owners_now_moves_a_settling_session_and_the_wait_is_kept_until_then	presence-settle-ladder-gates	
src/harness/upgrade_drive_tests.rs	an_unnamed_process_falls_back_to_the_build_test	retarget-arms	
src/harness/upgrade_drive_tests.rs	an_unreadable_process_table_is_never_read_as_nothing_running	background-walk	
src/harness/upgrade_drive_tests.rs	an_unset_claude_home_is_the_agents_own_homes_not_the_windows	claude-home-resolution	
src/harness/upgrade_drive_tests.rs	an_unwanted_model_restart_is_forgotten_before_it_begins	forget-settled-pending	
src/harness/upgrade_drive_tests.rs	an_upgrade_carries_a_model_chosen_by_hand_when_the_list_moves_none	model-way-back	
src/harness/upgrade_drive_tests.rs	an_upgrade_in_a_claude_home_with_no_projects_restarts_the_session_afresh	task-detection-and-fresh-restart	
src/harness/upgrade_drive_tests.rs	an_upgrade_record_keeps_the_fallbacks_origin	model-way-back	
src/harness/upgrade_drive_tests.rs	an_upgrade_relaunch_that_ended_as_it_started_is_an_agent_down_at_once	relaunch-ended-verdict	
src/harness/upgrade_drive_tests.rs	an_upgrade_restart_makes_the_hand_choice_it_carries_the_fallbacks_origin	model-way-back	
src/harness/upgrade_drive_tests.rs	an_upgrade_restarts_a_conversation_with_no_task_afresh_and_announces_to_one_with_a_task	task-detection-and-fresh-restart	
src/harness/upgrade_drive_tests.rs	an_upgrade_resumes_in_the_mode_the_pill_shows_not_the_transcripts_older_one	continuation-words-and-mode	
src/harness/upgrade_drive_tests.rs	an_upgrades_record_names_the_claude_home_its_agent_runs_under	claude-home-resolution	
src/harness/upgrade_drive_tests.rs	another_homes_announcement_grows_only_its_own_agents_list	model-evidence-scope	
src/harness/upgrade_drive_tests.rs	aterms_own_words_are_read_never_taken_back	left-typed-never-erased	
src/harness/upgrade_drive_tests.rs	carry_on_is_held_for_a_person_at_the_keys	no-act-without-ownership-proofs	
src/harness/upgrade_drive_tests.rs	carry_on_rechecks_tab_ownership_at_the_turn	no-act-without-ownership-proofs	
src/harness/upgrade_drive_tests.rs	closing_a_reached_pending_record_keeps_the_owners_defer	forget-settled-pending	
src/harness/upgrade_drive_tests.rs	continuation_poll_confirms_an_empty_tail_with_the_full_screen	composer-empty-caret-dim	
src/harness/upgrade_drive_tests.rs	continuation_poll_falls_back_on_an_unreadable_or_inconclusive_tail	composer-empty-caret-dim	
src/harness/upgrade_drive_tests.rs	current_claude_skips_process_inspection_but_new_and_in_flight_do_not	host-candidates-read-order	
src/harness/upgrade_drive_tests.rs	due_is_this_tabs_upgrade_or_restart_and_nothing_unread	due-never-drops-unread	
src/harness/upgrade_drive_tests.rs	every_look_at_the_job_stands_between_a_ctrl_z_and_the_act	no-act-without-ownership-proofs	
src/harness/upgrade_drive_tests.rs	every_phase_survives_the_state_file	state-record-roundtrip	
src/harness/upgrade_drive_tests.rs	every_reader_finds_a_relative_homes_records_where_its_agent_filed_them	claude-home-resolution	
src/harness/upgrade_drive_tests.rs	host_filters_foreign_groups_before_expensive_process_reads	host-candidates-read-order	
src/harness/upgrade_drive_tests.rs	malformed_or_unreadable_sibling_vetoes_a_valid_owner	session-roster-integrity	
src/harness/upgrade_drive_tests.rs	only_a_restart_that_ended_its_agent_dates_a_persons_keys	relaunch-prompt-read	
src/harness/upgrade_drive_tests.rs	only_its_shells_foreground_job_is_ever_announced_to_or_ended	job-classification	
src/harness/upgrade_drive_tests.rs	only_the_agents_own_words_are_pressed_where_the_host_takes_cap	pressed-text-delivery	
src/harness/upgrade_drive_tests.rs	only_the_relaunch_itself_is_adopted_and_told_it_was_upgraded	relaunch-adoption	
src/harness/upgrade_drive_tests.rs	park_when_asked	§4	
src/harness/upgrade_drive_tests.rs	ready_from_tab_a_never_announces_or_terminates_tab_b	notice-owner-refinement	
src/harness/upgrade_drive_tests.rs	relaunch_rechecks_shell_tab_after_the_session_scan	no-act-without-ownership-proofs	
src/harness/upgrade_drive_tests.rs	relaunch_roster_retries_an_incomplete_or_unstatable_directory_every_poll	session-roster-cache	
src/harness/upgrade_drive_tests.rs	relaunch_roster_reuses_stable_files_but_rechecks_changes_and_in_place_writes	session-roster-cache	
src/harness/upgrade_drive_tests.rs	settled_states_of_dead_conversations_are_pruned_after_a_week_and_nothing_else	wait-log-ledger-and-prune-bounds	
src/harness/upgrade_drive_tests.rs	stale_preliminary_roster_cannot_authorize_an_announcement	no-act-without-ownership-proofs	
src/harness/upgrade_drive_tests.rs	tab_ones_recorded_shape_is_not_asked_until_nothing_runs_and_claude_idles	stop-rest-ladder	
src/harness/upgrade_drive_tests.rs	the_account_move_that_signalled_an_agent_is_named_by_its_label	carry-restart-outcome	
src/harness/upgrade_drive_tests.rs	the_agents_own_keep_awake_does_not_hold_the_restart	background-walk	
src/harness/upgrade_drive_tests.rs	the_aside_of_every_instance_is_one	roster-aside-union	
src/harness/upgrade_drive_tests.rs	the_carry_of_a_restart_already_adopted_answers_without_waiting	relaunch-adoption	
src/harness/upgrade_drive_tests.rs	the_carry_of_a_restart_whose_adopted_agent_is_gone_closes_it	relaunch-adoption	
src/harness/upgrade_drive_tests.rs	the_carry_of_an_exit_whose_move_landed_meanwhile_says_nothing	carry-restart-outcome	
src/harness/upgrade_drive_tests.rs	the_claude_notice_fences_on_its_input_box_where_the_host_takes_if_rows	typed-turn-fence	
src/harness/upgrade_drive_tests.rs	the_clis_sweep_finds_an_agent_under_another_claude_home_no_visit_recorded	claude-home-resolution	
src/harness/upgrade_drive_tests.rs	the_clis_sweep_judges_an_empty_homes_agent_a_window_recorded_by_its_login	model-evidence-scope	
src/harness/upgrade_drive_tests.rs	the_clis_sweep_reads_the_tabs_of_every_instance_it_is_given	claude-home-resolution	
src/harness/upgrade_drive_tests.rs	the_clis_sweep_waits_on_no_tab_it_only_learned_homes_from	claude-home-resolution	
src/harness/upgrade_drive_tests.rs	the_continuation_is_typed_under_a_hand_taken_or_passed	hand-lease-bracket	
src/harness/upgrade_drive_tests.rs	the_due_clock_survives_an_unknown_read_and_clears_when_nothing_is_due	model-ask-ledger	
src/harness/upgrade_drive_tests.rs	the_end_of_the_agents_work_re_arms_and_asks_in_the_same_visit	stop-rest-ladder	
src/harness/upgrade_drive_tests.rs	the_end_of_the_work_asks_no_sooner_than_its_floor_and_no_more_than_its_budget	stop-rest-ladder	
src/harness/upgrade_drive_tests.rs	the_fence_judges_the_composer_row_not_the_cursors_row	typed-turn-fence	
src/harness/upgrade_drive_tests.rs	the_fenced_notice_asks_nothing_between_its_read_and_its_turn	typed-turn-fence	
src/harness/upgrade_drive_tests.rs	the_hand_is_kept_through_every_wait_under_it	hand-lease-bracket	
src/harness/upgrade_drive_tests.rs	the_harness_own_hand_holds_nothing_against_it_and_unread_input_holds_the_tab	held-by-status	
src/harness/upgrade_drive_tests.rs	the_homes_model_caches_are_not_read_under_several_logins	model-evidence-scope	
src/harness/upgrade_drive_tests.rs	the_host_finds_and_carries_the_restart_that_ended_the_agent_that_left	carry-restart-outcome	
src/harness/upgrade_drive_tests.rs	the_hosts_looks_read_the_record_under_the_agents_own_claude_home	claude-home-resolution	
src/harness/upgrade_drive_tests.rs	the_kernel_reads_whether_a_shell_has_its_tab_back	relaunch-ended-verdict	
src/harness/upgrade_drive_tests.rs	the_live_upgrade_finds_an_agent_under_another_claude_home	claude-home-resolution	
src/harness/upgrade_drive_tests.rs	the_loops_words_in_another_conversation_are_no_turn_of_this_one	task-detection-and-fresh-restart	
src/harness/upgrade_drive_tests.rs	the_model_rule_takes_each_key_from_the_highest_document_that_sets_it	model-evidence-scope	
src/harness/upgrade_drive_tests.rs	the_notice_is_fenced_on_the_generation_it_was_judged_on	typed-turn-fence	
src/harness/upgrade_drive_tests.rs	the_notice_is_typed_under_the_harnesss_hand_and_a_refused_fence_is_retried	hand-lease-bracket	
src/harness/upgrade_drive_tests.rs	the_notice_names_what_runs_under_the_agent_by_its_command	etime-and-command-head	
src/harness/upgrade_drive_tests.rs	the_orphan_pass_leaves_a_restart_relaunched_since_its_list_was_read	in-flight-orphan-pass	
src/harness/upgrade_drive_tests.rs	the_owners_now_never_signals_a_turn_begun_since_the_look_over_a_lagging_status	last-look-before-signal	
src/harness/upgrade_drive_tests.rs	the_owners_now_probes_a_blind_build_again	failed-start-gate	
src/harness/upgrade_drive_tests.rs	the_owners_now_restarts_a_quiet_session_whose_status_only_lags	quiet-flow	
src/harness/upgrade_drive_tests.rs	the_owners_word_waives_nothing_in_a_tab_it_did_not_name	owner-word-survives-restart	
src/harness/upgrade_drive_tests.rs	the_planned_relaunch_line_carries_the_usage_tap_only_where_its_gate_holds	relaunch-line-construction	
src/harness/upgrade_drive_tests.rs	the_pre_filter_never_restarts_a_native_sessions_warm_wait	model-evidence-scope	
src/harness/upgrade_drive_tests.rs	the_real_age_conforms_to_the_age_model	age-is-one-process	
src/harness/upgrade_drive_tests.rs	the_real_classifier_conforms_to_the_upgrade_look_model	due-never-drops-unread	
src/harness/upgrade_drive_tests.rs	the_real_exit_read_conforms_to_the_model	exit-record-refinement	
src/harness/upgrade_drive_tests.rs	the_real_look_climbs_the_ladder_on_claude_code	presence-settle-ladder-gates	
src/harness/upgrade_drive_tests.rs	the_real_prompt_read_conforms_to_the_model	relaunch-prompt-read	
src/harness/upgrade_drive_tests.rs	the_relaunch_line_carries_a_rekey_and_never_waits_on_one	relaunch-line-dialect-and-rekey	
src/harness/upgrade_drive_tests.rs	the_relaunch_line_is_never_typed_into_a_program_started_at_the_prompt	relaunch-prompt-read	
src/harness/upgrade_drive_tests.rs	the_relaunch_line_is_never_typed_over_a_person_or_a_holder	no-act-without-ownership-proofs	
src/harness/upgrade_drive_tests.rs	the_relaunch_line_is_typed_under_the_hand_never_let_go	hand-lease-bracket	
src/harness/upgrade_drive_tests.rs	the_relaunch_line_moves_the_model_within_its_family_first_and_keeps_the_rest	relaunch-line-construction	
src/harness/upgrade_drive_tests.rs	the_relaunch_line_waits_for_a_person_typing_at_the_prompt	relaunch-prompt-read	
src/harness/upgrade_drive_tests.rs	the_relaunch_on_exit_refuses_a_line_that_would_move_a_relative_home	claude-home-resolution	
src/harness/upgrade_drive_tests.rs	the_relaunch_snapshot_reads_a_relative_or_empty_home_where_its_agent_runs	claude-home-resolution	
src/harness/upgrade_drive_tests.rs	the_release_is_the_next_act_only_where_nothing_else_is	release-is-next	
src/harness/upgrade_drive_tests.rs	the_reset_line_is_read_from_the_named_transcripts_bounded_tail	tail-window-bounded	
src/harness/upgrade_drive_tests.rs	the_restart_last_look_keeps_unknown_identity_and_background_work_fenced	last-look-before-signal	
src/harness/upgrade_drive_tests.rs	the_roster_names_another_supervisors_tabs_and_the_off_ones_never_the_windows_own	roster-aside-union	
src/harness/upgrade_drive_tests.rs	the_settings_chain_reads_no_recycled_pids_settings_flag	model-evidence-scope	
src/harness/upgrade_drive_tests.rs	the_settle_is_measured_from_the_agents_verdict_not_from_a_repaint	presence-settle-ladder-gates	
src/harness/upgrade_drive_tests.rs	the_shells_own_block_says_whether_the_line_ran	relaunch-ended-verdict	
src/harness/upgrade_drive_tests.rs	the_standing_wall_is_read_from_the_named_transcripts_bounded_tail	tail-window-bounded	
src/harness/upgrade_drive_tests.rs	the_start_probe_runs_the_build_with_its_help_flag_bounded	bounded-child-probe	
src/harness/upgrade_drive_tests.rs	the_sweep_that_types_the_continuation_does_not_wait_for_the_answer	model-ask-ledger	
src/harness/upgrade_drive_tests.rs	the_transcript_is_found_under_the_live_processs_own_config_dir	claude-home-resolution	
src/harness/upgrade_drive_tests.rs	the_transcript_that_confirms_a_wall_is_found_from_the_process	claude-home-resolution	
src/harness/upgrade_drive_tests.rs	the_turn_ends_task_read_is_the_agents_own_records	claude-home-resolution	
src/harness/upgrade_drive_tests.rs	the_upgrade_ledger_is_cut_back_to_its_newest_rows	wait-log-ledger-and-prune-bounds	
src/harness/upgrade_drive_tests.rs	the_muse_restarts_log_lag_owns_its_turn_end_for_the_settles_looks	turn-end-ownership	
src/harness/upgrade_drive_tests.rs	the_upgrade_owns_turn_ends_only_while_its_step_says_so	turn-end-ownership	
src/harness/upgrade_drive_tests.rs	the_upgrade_view_counts_an_agent_under_another_claude_home	claude-home-resolution	
src/harness/upgrade_drive_tests.rs	the_upgrades_own_relaunch_types_back_the_login_the_agent_runs_under	restart-carries-login-and-tap	
src/harness/upgrade_drive_tests.rs	the_wait_log_keeps_each_change_of_wait_with_the_facts_that_decided_it	wait-log-ledger-and-prune-bounds	
src/harness/upgrade_drive_tests.rs	the_walls_tail_is_the_transcript_its_agent_writes_now	claude-home-resolution	
src/harness/upgrade_drive_tests.rs	the_way_back_from_a_fallback_returns_to_the_launched_model_across_a_memory_restart	model-way-back	
src/harness/upgrade_drive_tests.rs	the_way_back_prefers_the_persons_model_over_the_notices_alias	model-way-back	
src/harness/upgrade_drive_tests.rs	the_window_hosts_limit_and_wall_readers_read_the_agents_own_claude_home	claude-home-resolution	
src/harness/upgrade_drive_tests.rs	the_windows_step_answers_with_the_tabs_own_word_before_another_tabs	pick-tab-word	
src/harness/upgrade_drive_tests.rs	typeahead_before_the_first_read_is_never_the_mark	relaunch-prompt-read	
src/harness/upgrade_drive_tests.rs	typing_that_wraps_at_the_bottom_of_the_screen_is_still_typing	relaunch-prompt-read	
src/harness/upgrade_drive_tests.rs	what_holds_the_move_is_kept_by_its_start	age-is-one-process	
src/harness/upgrade_drive_tests.rs	what_the_tabs_own_supervisor_typed_is_no_task	task-detection-and-fresh-restart	
src/harness/upgrade_drive_tests.rs	whether_a_conversation_has_a_task_is_read_on_from_where_the_last_read_ended	task-detection-and-fresh-restart	
src/harness/upgrade_drive_tests.rs	who_roster_exposes_a_unique_foreground_group_without_a_window_hop	host-roster-parse	
src/harness/upgrade_ladder_claude_tests.rs	the_real_ladder_is_the_derived_machine_on_the_claude_lane	ladder-refinement	
src/harness/upgrade_ladder_claude_tests.rs	the_still_run_counts_claude_looks_at_the_same_words_and_not_repaints	ladder-refinement	
src/harness/upgrade_login_wall_tests.rs	a_give_up_taken_back_counts_no_ask_twice	login-wall-spends-nothing	
src/harness/upgrade_login_wall_tests.rs	a_notice_answered_only_by_the_wall_never_reached_the_model	login-wall-reader	
src/harness/upgrade_login_wall_tests.rs	a_ready_to_the_current_marker_restarts_under_every_gate	login-wall-spends-nothing	
src/harness/upgrade_login_wall_tests.rs	a_session_at_the_login_wall_is_never_asked_and_no_clock_runs	login-wall-spends-nothing	
src/harness/upgrade_login_wall_tests.rs	the_clock_is_held_through_the_login_wall	login-wall-spends-nothing	
src/harness/upgrade_login_wall_tests.rs	the_incident_is_waited_out_and_its_last_notice_asked_again	login-wall-spends-nothing	
src/harness/upgrade_login_wall_tests.rs	the_login_as_2_1_283_writes_it_lifts_the_wall	login-wall-reader	
src/harness/upgrade_login_wall_tests.rs	the_login_wall_is_read_from_the_screen_and_the_transcript	login-wall-reader	
src/harness/upgrade_login_wall_tests.rs	the_state_0_93_0_left_is_read_and_recovered	login-wall-spends-nothing	
src/harness/upgrade_models.rs	a_marked_settings_file_keeps_its_allowlist_and_default	user-model-settings-parse	
src/harness/upgrade_models.rs	a_model_chosen_by_hand_is_carried_only_where_it_is_not_the_launchs	hand-chosen-model	
src/harness/upgrade_models.rs	a_model_command_older_than_the_process_is_no_hand_choice	hand-chosen-model	
src/harness/upgrade_models.rs	a_model_record_round_trips_and_refuses_garbage	model-record-roundtrip	
src/harness/upgrade_models.rs	a_pick_of_what_the_launch_alias_names_is_no_hand_choice	hand-chosen-model	
src/harness/upgrade_models.rs	a_plan_bills_fable_to_usage_credits_only_where_its_profile_says	credits-billed-model-target	
src/harness/upgrade_models.rs	a_quoted_model_result_is_text_not_a_choice	live-model-reader	
src/harness/upgrade_models.rs	a_record_without_the_due_clock_parses_as_nothing_due	model-record-roundtrip	
src/harness/upgrade_models.rs	a_resumed_process_runs_its_launch_model_until_it_answers	live-model-at-launch-until-answer	
src/harness/upgrade_models.rs	availability_follows_the_served_catalog_and_the_deny_lists	model-availability-decision	
src/harness/upgrade_models.rs	claude_json_yields_only_model_facts	claude-json-model-facts-only	
src/harness/upgrade_models.rs	family_versions	model-family-version	
src/harness/upgrade_models.rs	the_allowlist_limits_the_target_and_the_settings_parse_fail_closed	offered-set-and-list-target	
src/harness/upgrade_models.rs	the_live_model_is_a_later_model_command_else_the_last_answer	live-model-reader	
src/harness/upgrade_models.rs	the_model_rule_moves_within_the_family_first	model-due-rule	
src/harness/upgrade_models.rs	the_reason_a_move_is_said_with_is_read_from_the_two_ids	move-why-from-ids	
src/harness/upgrade_models.rs	the_resolver_moves_to_the_newest_of_the_same_family_and_nowhere_else	family-successor-newest-strictly-newer	
src/harness/upgrade_models.rs	the_seed_round_trips_and_a_malformed_file_is_refused	priority-list-roundtrip	
src/harness/upgrade_models.rs	utc_stamps_and_the_last_answer	utc-and-lstart-parse	
src/harness/upgrade_models.rs	what_is_offered_is_what_the_installed_build_knows_and_the_person_allows	offered-set-and-list-target	
src/harness/upgrade_models.rs	with_nothing_newer_in_its_family_the_list_moves_only_a_model_nobody_chose	model-due-rule	
src/harness/upgrade_progress_tests.rs	the_quiet_upgrades_read_records_what_holds_its_restart	reducer-refines-progress-machine	
src/harness/upgrade_progress_tests.rs	the_real_quiet_view_is_the_derived_machine_read_quietly	classifier-refines-progress-machine	
src/harness/upgrade_progress_tests.rs	the_real_reducer_is_the_derived_machine	reducer-refines-progress-machine	
src/harness/upgrade_progress_tests.rs	the_real_view_is_the_derived_machine	classifier-refines-progress-machine	
src/harness/upgrade_progress_tests.rs	the_refused_notices_tell_is_an_instant_the_view_names	next-change-complete	
src/harness/upgrade_queued_tests.rs	a_limit_notice_a_grace_past_its_named_reset_is_moved_as_any_idle_point	visit-shell-refines-pure-reducer	
src/harness/upgrade_queued_tests.rs	a_limit_row_naming_no_reset_leaves_the_view_no_reset	visit-shell-refines-pure-reducer	
src/harness/upgrade_queued_tests.rs	a_notice_queued_behind_the_limit_is_never_asked_again_behind_it	visit-shell-refines-pure-reducer	
src/harness/upgrade_queued_tests.rs	the_give_up_0_93_0_spent_behind_the_limit_rests_then_waits_out_the_limit	visit-shell-refines-pure-reducer	
src/harness/upgrade_queued_tests.rs	the_real_queue_conforms_to_the_limit_queue_model	limit-queue-refines-model	
src/harness/upgrade_stall_tests.rs	a_direction_since_the_last_answer_is_a_turn_typed_by_someone_else	ready-answer-reader	
src/harness/upgrade_stall_tests.rs	a_dropped_release_leaves_no_owed_wait_behind	visit-shell-refines-pure-reducer	
src/harness/upgrade_stall_tests.rs	a_gave_up_record_restarts_on_a_late_ready_or_releases_its_agent	visit-shell-refines-pure-reducer	
src/harness/upgrade_stall_tests.rs	a_give_up_spends_the_owners_now	visit-shell-refines-pure-reducer	
src/harness/upgrade_stall_tests.rs	a_late_ready_whose_restart_waits_keeps_its_gates_word	visit-shell-refines-pure-reducer	
src/harness/upgrade_stall_tests.rs	a_limit_episode_holds_the_upgrades_clocks_for_the_window	upgrade-clocks	
src/harness/upgrade_stall_tests.rs	a_limited_tab_is_never_typed_into_and_never_given_up_on	visit-shell-refines-pure-reducer	
src/harness/upgrade_stall_tests.rs	a_ready_agent_under_a_stale_status_is_never_signalled_and_is_asked_again	visit-shell-refines-pure-reducer	
src/harness/upgrade_stall_tests.rs	a_release_follows_only_the_process_the_notice_reached	release-ownership	
src/harness/upgrade_stall_tests.rs	a_release_is_dropped_where_it_is_no_longer_the_upgrades	visit-shell-refines-pure-reducer	
src/harness/upgrade_stall_tests.rs	a_stop_is_stamped_and_a_rearm_is_a_new_round	stop-rest-ladder	
src/harness/upgrade_stall_tests.rs	a_stopped_round_rests_then_a_new_one_asks_again_naming_what_runs	visit-shell-refines-pure-reducer	
src/harness/upgrade_stall_tests.rs	a_void_begins_the_rest_again_by_the_ladder_of_its_stop	stop-rest-ladder	
src/harness/upgrade_stall_tests.rs	a_voided_ready_owes_one_release_typed_once	visit-shell-refines-pure-reducer	
src/harness/upgrade_stall_tests.rs	the_incident_replays_to_a_restart_never_a_stall	release-ownership	
src/harness/upgrade_stall_tests.rs	the_owners_hold_releases_the_agent_and_forgets_its_answers	release-ownership	
src/harness/upgrade_stall_tests.rs	the_real_upgrade_conforms_to_the_never_strands_model	upgrade-refines-never-strands-model	
src/harness/upgrade_status_tests.rs	a_build_that_fails_to_start_is_one_fault_per_build	aterms-own-fault	
src/harness/upgrade_status_tests.rs	a_carried_on_restart_reads_restarting_while_its_model_is_waited_for	row-of-record	
src/harness/upgrade_status_tests.rs	a_claude_stall_never_prints_its_wait_word	wait-class-total-and-stable	
src/harness/upgrade_status_tests.rs	a_codex_upgrade_is_listed_held_and_hurried_like_claudes	row-of-record	
src/harness/upgrade_status_tests.rs	a_day_behind_is_news_whatever_the_rounds_do	own-work-news	
src/harness/upgrade_status_tests.rs	a_digest_line_is_keyed_by_kind_and_tab_and_says_no_restart	need-and-mark-text-bounds	
src/harness/upgrade_status_tests.rs	a_finished_moves_row_carries_its_relaunch_instant	row-of-record	
src/harness/upgrade_status_tests.rs	a_host_that_has_just_begun_or_resumed_watching_flags_nothing_unwatched	unwatched-predicate	
src/harness/upgrade_status_tests.rs	a_lagging_status_under_the_quiet_flow_is_no_comfort_and_now_moves_it	stall-mark-and-remedy	
src/harness/upgrade_status_tests.rs	a_limit_whose_named_reset_has_passed_is_told	attention-horizons	
src/harness/upgrade_status_tests.rs	a_move_the_person_holds_is_noted_for_their_footer	person-hold-footer-note	
src/harness/upgrade_status_tests.rs	a_notice_aterm_could_not_type_is_told_as_aterms_own	aterms-own-fault	
src/harness/upgrade_status_tests.rs	a_person_held_announced_upgrade_is_a_record_never_a_mark	attention-horizons	
src/harness/upgrade_status_tests.rs	a_quiet_give_up_its_own_work_held_is_a_wait_the_owners_now_does_not_move	stall-mark-and-remedy	
src/harness/upgrade_status_tests.rs	a_quiet_wait_turns_a_fault_line_at_its_own_horizon	attention-horizons	
src/harness/upgrade_status_tests.rs	a_rearmed_round_forgets_when_the_old_restarts_agent_was_seen_gone	row-of-record	
src/harness/upgrade_status_tests.rs	a_rearmed_rounds_first_looks_are_still_its_rest	own-work-news	
src/harness/upgrade_status_tests.rs	a_record_an_older_build_wrote_is_aged_from_its_minting_until_a_visit_settles_it	own-work-news	
src/harness/upgrade_status_tests.rs	a_record_nothing_has_checked_reads_unwatched_past_the_wake_grace	unwatched-predicate	
src/harness/upgrade_status_tests.rs	a_record_with_no_recorded_tab_is_watched_in_its_holders_tab	view-host-protocol	
src/harness/upgrade_status_tests.rs	a_record_with_no_recorded_tab_reads_the_claim_on_its_holders_tab	aside-precedence	
src/harness/upgrade_status_tests.rs	a_refusal_a_person_or_a_peer_caused_is_not_blamed_on_aterm	attention-horizons	
src/harness/upgrade_status_tests.rs	a_relaunched_agent_that_reaches_no_prompt_is_a_log_line_then_an_interrupt	attention-horizons	
src/harness/upgrade_status_tests.rs	a_relaunchs_record_is_no_upgrade_of_the_tab	row-vetting	
src/harness/upgrade_status_tests.rs	a_restart_under_way_past_its_bound_reads_stuck_and_says_what_it_waits_on	stall-mark-and-remedy	
src/harness/upgrade_status_tests.rs	a_resume_by_hand_says_which_login_it_runs_under	resume-command-inverts-relaunch-line	
src/harness/upgrade_status_tests.rs	a_row_read_off_a_live_status_names_its_tab_for_the_host_to_follow	view-host-protocol	
src/harness/upgrade_status_tests.rs	a_row_served_late_is_judged_with_the_awake_clock_as_it_stands	unwatched-predicate	
src/harness/upgrade_status_tests.rs	a_row_stands_only_while_its_conversation_is_live_and_behind_in_its_tab	row-vetting	
src/harness/upgrade_status_tests.rs	a_row_that_crosses_a_horizon_is_a_changed_row	next-change-complete	
src/harness/upgrade_status_tests.rs	a_session_asked_again_and_again_is_news	own-work-news	
src/harness/upgrade_status_tests.rs	a_stall_whose_words_carry_no_age_never_wakes_for_it	next-change-complete	
src/harness/upgrade_status_tests.rs	a_state_file_from_either_older_writer_is_read_with_its_missing_fields_at_default	state-record-roundtrip	
src/harness/upgrade_status_tests.rs	a_state_with_no_recorded_tab_stands_in_the_tab_its_holder_is_in	row-vetting	
src/harness/upgrade_status_tests.rs	a_stop_that_asks_again_marks_nothing_and_one_that_repeats_carries_its_stall	stall-mark-and-remedy	
src/harness/upgrade_status_tests.rs	a_stopped_refusal_is_typed_for_the_window_and_a_sentence_for_the_shell	need-and-mark-text-bounds	
src/harness/upgrade_status_tests.rs	a_stopped_round_says_when_its_next_round_starts	row-of-record	
src/harness/upgrade_status_tests.rs	a_tab_another_supervisor_claims_reads_its_claim_never_aterms_own_fault	aside-precedence	
src/harness/upgrade_status_tests.rs	a_tab_whose_agent_is_not_in_front_is_never_told_aterms_own_fault	aside-precedence	
src/harness/upgrade_status_tests.rs	a_tab_whose_autopilot_is_off_reads_its_off_never_aterms_own_fault	aside-precedence	
src/harness/upgrade_status_tests.rs	a_token_word_reads_as_the_cut_reads_it	status-line-and-json-fields	
src/harness/upgrade_status_tests.rs	a_whole_round_never_flips_the_owners_line_at_its_acts	own-work-news	
src/harness/upgrade_status_tests.rs	a_word_fenced_to_a_gone_notices_tab_holds_neither_the_view_nor_the_drive	row-of-record	
src/harness/upgrade_status_tests.rs	a_word_for_a_target_that_moved_on_lands_on_nothing	owner-word-ask	
src/harness/upgrade_status_tests.rs	a_word_on_a_gave_up_upgrade_forgets_its_answers	owner-word-ask	
src/harness/upgrade_status_tests.rs	an_announced_round_under_the_quiet_flow_reads_overdue_at_six_hours	stall-mark-and-remedy	
src/harness/upgrade_status_tests.rs	an_ask_writes_the_owners_word_under_the_lock_and_refuses_what_it_cannot_hold	owner-word-ask	
src/harness/upgrade_status_tests.rs	an_overdue_move_names_its_cause_by_class	wait-class-total-and-stable	
src/harness/upgrade_status_tests.rs	an_owners_now_quiets_an_overdue_stall_only_for_a_bounded_time	stall-mark-and-remedy	
src/harness/upgrade_status_tests.rs	an_owners_word_that_lands_wakes_the_window	owner-word-ask	
src/harness/upgrade_status_tests.rs	an_unreached_row_reads_claudes_live_status_never_over_a_recorded_word	row-vetting	
src/harness/upgrade_status_tests.rs	an_unwatched_record_is_told_as_aterms_own_fault	aterms-own-fault	
src/harness/upgrade_status_tests.rs	an_upgrade_that_cannot_run_is_one_owner_sentence	aterms-own-fault	
src/harness/upgrade_status_tests.rs	aterms_own_fault_is_one_log_line_never_a_digest_line_or_a_mark	aterms-own-fault	
src/harness/upgrade_status_tests.rs	each_kind_of_stall_names_the_remedy_that_moves_it	stall-mark-and-remedy	
src/harness/upgrade_status_tests.rs	every_owners_word_arms_a_new_round	owner-word-ask	
src/harness/upgrade_status_tests.rs	every_wait_has_a_horizon_and_none_interrupts_by_age	attention-horizons	
src/harness/upgrade_status_tests.rs	looks_at_the_same_work_do_not_change_the_row	own-work-news	
src/harness/upgrade_status_tests.rs	next_attention_at_names_every_change_by_time	next-change-complete	
src/harness/upgrade_status_tests.rs	only_a_stalled_tab_is_marked_and_its_mark_is_lowered_when_it_recovers	view-host-protocol	
src/harness/upgrade_status_tests.rs	only_an_agent_the_upgrade_ended_interrupts	interrupt-only-for-an-ended-agent	
src/harness/upgrade_status_tests.rs	only_an_upgrade_that_will_not_move_on_its_own_is_stalled	stall-mark-and-remedy	
src/harness/upgrade_status_tests.rs	records_that_cannot_be_read_whole_hand_the_window_nothing	view-host-protocol	
src/harness/upgrade_status_tests.rs	status_rows_come_from_the_state_files_and_filter_by_tab	row-vetting	
src/harness/upgrade_status_tests.rs	status_says_what_holds_the_move_and_what_the_owner_is_told	status-line-and-json-fields	
src/harness/upgrade_status_tests.rs	the_agents_own_work_is_news_only_after_a_day_and_a_round	own-work-news	
src/harness/upgrade_status_tests.rs	the_agents_own_work_is_the_rounds_not_the_last_looks	own-work-news	
src/harness/upgrade_status_tests.rs	the_ask_count_survives_a_retarget_and_the_state_file	own-work-news	
src/harness/upgrade_status_tests.rs	the_awake_clock_is_the_machines_last_wake_or_boot	awake-clock-probe	
src/harness/upgrade_status_tests.rs	the_column_names_the_state_the_target_the_reason_and_the_age	row-of-record	
src/harness/upgrade_status_tests.rs	the_host_sends_its_tabs_rows_only_when_they_change	view-host-protocol	
src/harness/upgrade_status_tests.rs	the_interrupts_action_follows_its_reason_and_keeps_the_launchs_flags	resume-command-inverts-relaunch-line	
src/harness/upgrade_status_tests.rs	the_manual_resume_keeps_its_flags_under_every_line_the_harness_builds	resume-command-inverts-relaunch-line	
src/harness/upgrade_status_tests.rs	the_own_work_line_does_not_move_with_the_holder_count	own-work-news	
src/harness/upgrade_status_tests.rs	the_own_work_line_names_the_goal_the_programs_and_the_break	need-and-mark-text-bounds	
src/harness/upgrade_status_tests.rs	the_owner_is_told_which_processes_hold_the_move	status-line-and-json-fields	
src/harness/upgrade_status_tests.rs	the_status_json_says_how_loud_the_owner_is_told_and_why	status-line-and-json-fields	
src/harness/upgrade_status_tests.rs	the_status_line_says_the_rung	status-line-and-json-fields	
src/harness/upgrade_status_tests.rs	the_view_marks_a_record_nothing_checks_at_an_instant_it_names	unwatched-predicate	
src/harness/upgrade_status_tests.rs	the_view_names_a_records_watch_and_lists_it_due_once_past	view-host-protocol	
src/harness/upgrade_status_tests.rs	the_view_names_the_instant_a_person_stops_being_named	next-change-complete	
src/harness/upgrade_status_tests.rs	the_view_names_the_instant_it_next_changes	next-change-complete	
src/harness/upgrade_status_tests.rs	what_time_alone_changes_is_a_changed_row_at_a_named_instant	next-change-complete	
src/harness/upgrade_tests.rs	a_conversation_has_a_task_from_someone_elses_first_prompt	transcript-tasked	
src/harness/upgrade_tests.rs	a_conversation_with_no_task_is_restarted_afresh_never_announced_to	next-step-safety	
src/harness/upgrade_tests.rs	a_deferral_holds_until_it_runs_out_and_a_skip_holds_only_its_version	next-step-safety	
src/harness/upgrade_tests.rs	a_direction_after_the_ready_ends_it	ready-answer-reader	
src/harness/upgrade_tests.rs	a_hold_stands_only_while_it_is_sampled	upgrade-clocks	
src/harness/upgrade_tests.rs	a_line_the_tty_could_cut_is_refused	relaunch-line-shape-quote-bound	
src/harness/upgrade_tests.rs	a_notice_the_limit_answered_is_queued_until_the_model_takes_it	notice-fate-and-queue	
src/harness/upgrade_tests.rs	a_now_request_waives_settling_and_never_the_draft_or_the_ready_answer	next-step-safety	
src/harness/upgrade_tests.rs	a_pasted_notice_is_the_notice_and_a_pasted_message_a_direction	ready-answer-reader	
src/harness/upgrade_tests.rs	a_provider_arn_is_a_model_id	model-outcome-reader	
src/harness/upgrade_tests.rs	a_quiet_round_an_older_build_announced_still_ends_and_owes_its_release	quiet-step-contract	
src/harness/upgrade_tests.rs	a_quiet_round_is_not_held_for_ever_by_a_status_its_idle_screen_contradicts	quiet-step-contract	
src/harness/upgrade_tests.rs	a_quiet_upgrade_restarts_only_a_genuinely_idle_session_and_types_nothing	quiet-step-contract	
src/harness/upgrade_tests.rs	a_ready_answer_counts_only_while_it_is_the_agents_last_word	ready-answer-reader	
src/harness/upgrade_tests.rs	a_ready_answer_is_the_marker_on_a_line_of_its_own	ready-answer-reader	
src/harness/upgrade_tests.rs	a_ready_before_the_latest_notice_is_no_answer_to_it	ready-answer-reader	
src/harness/upgrade_tests.rs	a_ready_on_the_screen_holds_the_point_until_its_row_is_written	next-step-safety	
src/harness/upgrade_tests.rs	a_session_is_attended_by_its_own_person_stamp	upgrade-clocks	
src/harness/upgrade_tests.rs	a_skip_stays_skipped_and_a_deferral_lets_a_stopped_round_go	next-step-safety	
src/harness/upgrade_tests.rs	a_standing_wall_with_no_limit_type_takes_it_from_its_text_and_hostile_rows_cost_one_key	standing-quota-wall	
src/harness/upgrade_tests.rs	a_superseded_ready_is_asked_again_at_the_next_point	next-step-safety	
src/harness/upgrade_tests.rs	an_attended_tab_is_neither_typed_into_nor_signalled_without_the_owners_now	next-step-safety	
src/harness/upgrade_tests.rs	an_empty_composer_is_blank_or_a_placeholder_and_nothing_else	composer-empty	
src/harness/upgrade_tests.rs	claudes_status_is_read_against_its_idle_screen_and_the_work_under_it	next-step-safety	
src/harness/upgrade_tests.rs	every_short_argv_resumes_with_exactly_the_values_claude_parsed	rewrite-argv-refines-commander	
src/harness/upgrade_tests.rs	every_stopped_round_rests_then_rearms	next-step-safety	
src/harness/upgrade_tests.rs	only_a_box_and_a_draft_are_a_persons_hold	next-step-safety	
src/harness/upgrade_tests.rs	the_agent_word_round_trips_and_an_unknown_word_is_no_agent	owner-words-roundtrip	
src/harness/upgrade_tests.rs	the_copies_of_a_queued_notice_are_counted_until_the_model_takes_one	notice-fate-and-queue	
src/harness/upgrade_tests.rs	the_dialogs_result_line_is_read_under_its_own_command_and_only_since_the_claim	reset-dialog-line	
src/harness/upgrade_tests.rs	the_incidents_ready_is_heard_once_written_and_superseded_by_the_keep_going	ready-answer-reader	
src/harness/upgrade_tests.rs	the_ladder_relaxes_the_settle_and_the_person_near_and_never_a_floor	next-step-safety	
src/harness/upgrade_tests.rs	the_last_words_print_ignores_the_live_zone_and_follows_the_words	upgrade-clocks	
src/harness/upgrade_tests.rs	the_launch_model_is_the_model_flag_the_rewrite_keeps	launch-model-and-resume-by-the-rewrites-parse	
src/harness/upgrade_tests.rs	the_launch_resume_names_the_conversation_the_process_kept	launch-model-and-resume-by-the-rewrites-parse	
src/harness/upgrade_tests.rs	the_limit_fact_is_read_by_the_supervisors_own_recogniser	screen-limit-recogniser	
src/harness/upgrade_tests.rs	the_limit_stands_until_the_reset_its_row_names	notice-fate-and-queue	
src/harness/upgrade_tests.rs	the_measured_session_file_parses_and_a_changed_shape_refuses	session-file-parse	
src/harness/upgrade_tests.rs	the_model_is_the_last_assistant_turns_and_the_first_is_the_resumed_ones	model-outcome-reader	
src/harness/upgrade_tests.rs	the_notice_keeps_a_pipeline_a_long_path_would_push_out	running-clause-held-list	
src/harness/upgrade_tests.rs	the_notice_lists_what_runs_under_the_agent_in_one_line	running-clause-held-list	
src/harness/upgrade_tests.rs	the_notice_names_the_oldest_processes_first	running-clause-held-list	
src/harness/upgrade_tests.rs	the_ready_answer_is_an_assistant_line_never_the_announcement	ready-answer-reader	
src/harness/upgrade_tests.rs	the_reducer_takes_the_step_the_drain_model_allows	reducer-refines-drain-bound-model	
src/harness/upgrade_tests.rs	the_relaunch_line_heals_a_frozen_shell_and_quotes_every_word	relaunch-line-shape-quote-bound	
src/harness/upgrade_tests.rs	the_relaunch_line_takes_the_key_first_in_the_shells_own_dialect	relaunch-line-shape-quote-bound	
src/harness/upgrade_tests.rs	the_release_is_typed_where_a_notice_could_be_and_never_over_a_ready	next-step-safety	
src/harness/upgrade_tests.rs	the_request_word_round_trips_and_an_unknown_word_asks_for_nothing	owner-words-roundtrip	
src/harness/upgrade_tests.rs	the_restarts_gate_waits_at_a_break_whatever_else_lets_it_go	next-step-safety	
src/harness/upgrade_tests.rs	the_resumed_model_is_read_only_from_the_new_builds_rows	model-outcome-reader	
src/harness/upgrade_tests.rs	the_rewrite_carries_the_flags_and_resumes_the_conversation	rewrite-argv-refines-commander	
src/harness/upgrade_tests.rs	the_rewrite_sheds_only_aterms_own_tap	rewrite-argv-refines-commander	
src/harness/upgrade_tests.rs	the_standing_quota_wall_is_the_limit_row_that_stands_and_only_a_quota_rejection	standing-quota-wall	
src/harness/upgrade_tests.rs	the_supervisors_own_turns_by_its_ledger_are_no_task	transcript-tasked	
src/harness/upgrade_tests.rs	the_target_is_the_newest_build_never_sideways_and_managed_on_a_tie	choose-target-newest-managed-tie	
src/harness/upgrade_tests.rs	the_wait_word_names_claudes_status_only_for_not_idle	owner-words-roundtrip	
src/harness/upgrade_tests.rs	the_work_end_reducer_is_bound_to_its_model	reducer-refines-work-end-model	
src/harness/upgrade_tests.rs	versions_order_numerically_and_refuse_suffixes	version-order	
src/harness/upgrade_wake.rs	a_fallback_tick_tells_a_side_listener_only_when_its_marker_changed	activation-wake	
src/harness/upgrade_wake.rs	a_proc_stat_line_names_an_exited_process_by_its_state	proc-stat-exited	
src/harness/upgrade_wake.rs	a_repointed_native_link_wakes_the_host_and_its_neighbours_do_not	activation-wake	
src/harness/upgrade_wake.rs	a_side_mark_tells_its_listener_and_ends_no_wait	activation-wake	
src/harness/upgrade_wake.rs	a_zombie_has_exited	wait-exit	
src/harness/upgrade_wake.rs	an_existing_prefix_gets_its_notice_dir_and_no_recheck	activation-wake	
src/harness/upgrade_wake.rs	an_exit_is_seen_as_it_happens	wait-exit	
src/harness/upgrade_wake.rs	first_install_rechecks_a_missing_path_after_a_lost_ancestor_notification	activation-wake	
src/harness/upgrade_wake.rs	first_install_wakes_even_when_the_package_parent_did_not_exist	activation-wake	
src/harness/upgrade_wake.rs	marker_wakes_a_parked_host_and_unrelated_writes_do_not	activation-wake	
src/harness/upgrade_wake.rs	the_owners_word_wakes_a_parked_host	activation-wake	
src/harness/upgrade_wake.rs	the_trigger_ends_a_parked_wait_at_once	activation-wake	
src/harness/upgrade_watch_tests.rs	a_record_with_no_recorded_tab_is_watched_where_the_read_finds_it	visit-shell-refines-pure-reducer	
src/harness/upgrade_watch_tests.rs	a_refused_notice_counts_and_never_moves_the_step_clock	progress-stamp-single-writer	
src/harness/upgrade_watch_tests.rs	a_told_watch_stamps_no_look_before_due_and_a_later_point_clears_the_guard	visit-shell-refines-pure-reducer	
src/harness/upgrade_watch_tests.rs	a_word_the_acts_own_last_check_recorded_stands_over_a_read_short_of_it	visit-shell-refines-pure-reducer	
src/harness/upgrade_watch_tests.rs	every_record_owed_a_step_has_a_deadline_the_legacy_shapes_included	deadline-totality	
src/harness/upgrade_watch_tests.rs	tab_ones_record_of_the_27th_is_past_due	deadline-totality	
src/harness/upgrade_watch_tests.rs	the_progress_stamp_moves_iff_the_key_moves_and_a_rollback_never_stamps	progress-stamp-single-writer	
src/harness/upgrade_watch_tests.rs	the_real_watch_is_the_derived_look	watch-refines-progress-look	
src/harness/upgrade_watch_tests.rs	the_watch_keeps_an_announced_round_whose_model_rides_the_restart_in_place	retarget-in-place	
src/harness/upgrade_watch_tests.rs	the_watch_keeps_no_reset_for_a_limit_row_that_names_none	visit-shell-refines-pure-reducer	
src/harness/upgrade_watch_tests.rs	the_watch_reads_the_limit_at_the_second_it_is_given	visit-shell-refines-pure-reducer	
src/harness/upgrade_watch_tests.rs	the_watch_retargets_and_holds_tab_ones_record_off_any_point_with_no_hands	visit-shell-refines-pure-reducer	
src/harness/usage.rs	a_limit_notice_row_is_kept_with_its_time	limit-notice-rows	
src/harness/usage.rs	a_loading_or_failed_panel_is_unsettled	usage-panel-read-phase	
src/harness/usage.rs	a_notice_names_its_window_or_none	limit-notice-rows	
src/harness/usage.rs	a_panel_whose_tail_is_under_the_fold_is_no_sample	usage-panel-read-phase	
src/harness/usage.rs	a_reset_beyond_its_windows_bound_is_dropped_and_its_figure_kept	usage-panel-sample	
src/harness/usage.rs	a_settings_panel_on_another_tab_is_not_the_usage_tab	usage-panel-read-phase	
src/harness/usage.rs	a_settled_panels_sample_places_each_reset_in_the_panels_zone	usage-panel-sample	
src/harness/usage.rs	a_streamed_subagent_message_counts_its_final_output_once	transcript-fold-max-merge	
src/harness/usage.rs	a_synthetic_row_never_names_the_sessions_model	last-model	
src/harness/usage.rs	a_whole_panel_missing_the_session_or_the_week_is_unsettled	usage-panel-read-phase	
src/harness/usage.rs	a_whole_panel_with_nothing_unsettled_on_it_is_settled	usage-panel-read-phase	
src/harness/usage.rs	a_window_that_was_not_read_gives_no_figure_and_none_gives_no_sample	usage-panel-sample	
src/harness/usage.rs	an_enrich_error_under_the_windows_is_unsettled	usage-panel-read-phase	
src/harness/usage.rs	hud_line_flattens_control_characters_in_labels_and_model_ids	hud-line	
src/harness/usage.rs	hud_line_is_capped_at_1024_bytes_on_a_char_boundary	hud-line	
src/harness/usage.rs	model_short_names_the_family	hud-line	
src/harness/usage.rs	numbers_are_read_as_integer_or_float_and_junk_is_absent	json-number-readers	
src/harness/usage.rs	panel_reader_never_guesses_a_measured_key_for_an_unmeasured_title	usage-panel-windows	
src/harness/usage.rs	panel_reader_refuses_what_is_not_a_panel	usage-panel-windows	
src/harness/usage.rs	panel_reader_takes_both_measured_layouts_and_the_fable_window	usage-panel-windows	
src/harness/usage.rs	panel_reader_takes_the_copied_text_form	usage-panel-windows	
src/harness/usage.rs	projected_usage_preserves_the_live_limit_wall_and_its_lift	limit-notice-rows	
src/harness/usage.rs	rfc3339_utc_matches_known_instants	rfc3339-utc-civil	
src/harness/usage.rs	the_account_model_is_the_newest_main_chain_row_of_its_transcript	last-model	
src/harness/usage.rs	the_byte_prefilter_never_drops_a_row_the_parser_would_have_summed	transcript-fold-classification	
src/harness/usage.rs	the_header_guard_matches_the_measured_header_and_no_copy_of_it	usage-panel-read-phase	
src/harness/usage.rs	the_inline_renderers_panel_is_told_from_the_fullscreen_one	usage-panel-read-phase	
src/harness/usage.rs	the_measured_screens_with_the_tail_under_the_fold_are_no_sample	usage-panel-read-phase	
src/harness/usage.rs	the_served_time_is_the_newest_served_response	served-at	
src/harness/usage.rs	the_session_sum_names_the_main_transcripts_model	last-model	
src/harness/usage.rs	transcript_differing_repeat_raises_to_the_largest_and_counts_once	transcript-fold-max-merge	
src/harness/usage.rs	transcript_distinct_ids_sum_per_model	transcript-fold-max-merge	
src/harness/usage.rs	transcript_duplicate_message_ids_are_summed_once	transcript-fold-max-merge	
src/harness/usage.rs	transcript_malformed_lines_are_counted_unparsed	transcript-fold-classification	
src/harness/usage.rs	transcript_missing_model_files_under_unknown_and_missing_fields_read_zero	transcript-fold-max-merge	
src/harness/usage.rs	transcript_model_count_is_bounded_into_other	transcript-fold-bounds	
src/harness/usage.rs	transcript_non_assistant_rows_are_ignored	transcript-fold-classification	
src/harness/usage.rs	transcript_rows_without_id_or_with_a_huge_id_are_summed_and_counted	transcript-fold-max-merge	
src/harness/usage.rs	transcript_rows_without_usage_are_counted_not_summed	transcript-fold-classification	
src/harness/usage.rs	transcript_seen_set_is_bounded_and_the_bound_is_what_it_says	transcript-fold-bounds	
src/harness/usage.rs	transcript_sidechain_row_is_summed_and_counted	transcript-fold-max-merge	
src/harness/usage.rs	transcript_very_long_line_is_skipped_and_counted	transcript-fold-classification	
src/harness/usage.rs	usage_json_is_schema_1_and_round_trips_through_the_parser	usage-json-schema-1	
src/harness/usage.rs	view_shown_is_the_first_active_account_else_the_first	usage-view-shown	
src/harness/usage.rs	working_time_is_the_union_of_the_main_chains_turns	working-time-union	
src/lib.rs	a_read_only_connection_sends_the_read_verbs_alone	relay-read-only-connection	
src/lib.rs	a_silent_ctl_failure_still_says_something	ctl-failure-words	
src/lib.rs	relay_client_bounds_a_line_even_when_the_last_read_contains_its_newline	relay-read-bounds	
src/lib.rs	relay_client_counted_body_has_one_aggregate_allowance	relay-read-bounds	
src/lib.rs	relay_client_frames_a_payload_byte_for_byte	relay-framing-one-line	
src/lib.rs	relay_client_reads_the_host_gone_line_as_the_connection_closing	relay-reply-classification	
src/lib.rs	relay_client_refuses_overflowing_counts_instead_of_reading_them_as_status	reply-count-grammar	
src/lib.rs	the_own_client_runs_the_ctl_verb_first	drive-own-ctl	
src/ready.rs	the_byte_is_ready_a_bare_close_is_closed_and_a_held_end_is_a_hang	ready-fd-signal	
src/supervise/answer.rs	a_roles_question_has_a_box_token_and_another_box_none	answer-box-token	
src/supervise/answer.rs	a_multi_select_toggles_what_differs_then_presses_the_button	answer-plan-contract	
src/supervise/answer.rs	a_priority_word_that_labels_an_option_answers_that_option	answer-plan-contract	
src/supervise/answer.rs	a_single_select_answer_names_one_option_or_is_refused	answer-plan-contract	
src/supervise/answer.rs	recommended_on_a_multi_select_plans_the_whole_tab	answer-plan-contract	
src/supervise/answer.rs	the_box_token_names_one_screen	answer-box-token	
src/supervise/approvals.rs	a_reset_row_taken_and_not_ended_is_owed_and_says_whether_it_was_entered	ledger-readers-are-row-projections	
src/supervise/approvals.rs	a_row_carries_the_hash_of_the_whole_command_and_a_cut_copy	ledger-row-json	
src/supervise/approvals.rs	the_journal_sits_beside_the_ledger_and_both_are_bounded	ledger-paths-and-bound	
src/supervise/approvals.rs	the_ledger_lives_under_the_state_root_by_sid_and_appends	ledger-paths-and-bound	
src/supervise/approvals.rs	the_probes_rows_are_no_typed_act_and_say_whether_a_panel_is_owed	ledger-readers-are-row-projections	
src/supervise/approvals.rs	typed_at_reads_the_sessions_typed_rows_only	ledger-readers-are-row-projections	
src/supervise/approvals.rs	typed_texts_are_what_the_sessions_loop_typed	ledger-readers-are-row-projections	
src/supervise/autopilot_loop_tests.rs	a_click_through_land_to_off_inside_the_settle_types_nothing	autopilot-word-edge	
src/supervise/autopilot_loop_tests.rs	a_point_inside_the_settle_waits_for_the_word_that_stands	autopilot-word-edge	
src/supervise/autopilot_loop_tests.rs	a_switch_off_land_mid_turn_types_no_instruction_at_the_point	autopilot-word-edge	never-type-over-a-person
src/supervise/autopilot_loop_tests.rs	a_switch_to_land_at_rest_in_a_back_off_types_land_after_the_settle	autopilot-word-edge	
src/supervise/autopilot_loop_tests.rs	a_switch_to_land_mid_turn_types_nothing_until_the_point_and_then_lands_after_the_settle	autopilot-word-edge	never-type-over-a-person
src/supervise/autopilot_loop_tests.rs	a_switch_while_the_turn_sleeps_on_the_log_is_noted_before_its_point	autopilot-word-edge	never-type-over-a-person
src/supervise/autopilot_loop_tests.rs	a_write_that_leaves_the_word_wakes_no_wait_at_rest	autopilot-word-edge	
src/supervise/autopilot_loop_tests.rs	an_off_park_wakes_on_land_and_types_it_at_once	autopilot-word-edge	
src/supervise/autopilot_loop_tests.rs	an_older_hosts_waits_carry_no_word	autopilot-word-edge	
src/supervise/background.rs	the_fact_says_only_a_token_task_a_plain_exe_and_a_known_wait	stall-ladder	
src/supervise/background.rs	the_told_fact_says_no_exe_and_only_a_token_task_and_a_known_wait	facts-no-progress-episode	
src/supervise/blocks.rs	a_messages_rows_stay_in_its_block	blocks-partition	
src/supervise/blocks.rs	each_head_opens_its_kind_of_block	blocks-partition	
src/supervise/blocks.rs	the_calls_the_phase_reader_reads_open_tool_blocks	blocks-partition	
src/supervise/blocks.rs	the_rows_above_the_first_head_are_judged_by_their_indent	blocks-views	
src/supervise/blocks.rs	the_views_keep_what_was_said	blocks-views	
src/supervise/claim.rs	a_busy_reply_names_the_other_holder	claim-verb-classes	
src/supervise/claim.rs	only_writes_are_refused_to_a_loop_that_watches	claim-verb-classes	
src/supervise/classify_tests.rs	a_backgrounded_command_is_a_head_and_a_redirect_ampersand_is_not	classify-sound	
src/supervise/classify_tests.rs	a_backtick_inside_double_quotes_still_runs	classify-sound	
src/supervise/classify_tests.rs	a_bash_indirection_is_not_literal	classify-sound	
src/supervise/classify_tests.rs	a_brace_glued_to_a_word_closes_the_group	classify-sound	
src/supervise/classify_tests.rs	a_brace_or_glob_word_before_printfs_format_may_be_its_v_option	classify-sound	
src/supervise/classify_tests.rs	a_closing_word_redirect_and_an_unopened_close_stay_reads	classify-sound	
src/supervise/classify_tests.rs	a_default_assignment_is_judged_as_an_assignment	classify-sound	
src/supervise/classify_tests.rs	a_dollar_form_inside_a_braced_operand_is_refused_as_it_is_bare	classify-sound	
src/supervise/classify_tests.rs	a_for_loop_over_a_zsh_arithmetic_variable_is_not_a_read	classify-sound	
src/supervise/classify_tests.rs	a_git_argument_this_check_cannot_read_is_refused_before_the_double_dash	classify-sound	
src/supervise/classify_tests.rs	a_glob_qualifier_that_can_run_a_command_is_not_a_read	classify-sound	
src/supervise/classify_tests.rs	a_grep_value_option_takes_its_value_not_the_pattern	approval-safe-bash-sound	
src/supervise/classify_tests.rs	a_less_variable_assignment_is_a_hazard	classify-sound	
src/supervise/classify_tests.rs	a_parameter_zsh_runs_is_a_hazard	classify-sound	
src/supervise/classify_tests.rs	a_plain_line_admits_only_the_expansions_zsh_takes_no_further_than_bash	plain-zsh-line-grammar	classify-sound
src/supervise/classify_tests.rs	a_plain_line_expands_nothing	plain-zsh-line-grammar	
src/supervise/classify_tests.rs	a_program_word_is_judged_only_at_a_head	classify-sound	
src/supervise/classify_tests.rs	a_python_path_cannot_step_out_of_the_allowlist	python-allowlist	
src/supervise/classify_tests.rs	a_redirect_on_a_left_out_segment_is_judged	classify-except-rm	
src/supervise/classify_tests.rs	a_redirect_or_a_dash_before_the_name_does_not_hide_it	classify-sound	
src/supervise/classify_tests.rs	a_reserved_word_behind_an_assignment_or_a_redirect_is_a_program	classify-sound	
src/supervise/classify_tests.rs	a_subscript_on_a_special_parameter_is_refused	classify-sound	
src/supervise/classify_tests.rs	a_substitution_is_a_word_of_the_command_around_it	classify-sound	
src/supervise/classify_tests.rs	a_tied_array_is_assigned_as_its_scalar	classify-sound	
src/supervise/classify_tests.rs	a_tool_that_takes_no_abbreviation_is_read_by_whole_names	approval-safe-bash-sound	
src/supervise/classify_tests.rs	a_unicode_escape_in_a_printf_format_may_be_a_conversion	classify-sound	
src/supervise/classify_tests.rs	a_word_after_a_closing_brace_is_a_command	classify-sound	
src/supervise/classify_tests.rs	a_wrapper_cannot_launder_the_program_it_runs	classify-sound	
src/supervise/classify_tests.rs	an_arithmetic_evaluation_is_refused_whole	classify-sound	
src/supervise/classify_tests.rs	an_awk_or_sed_program_from_a_parameter_is_not_read	classify-sound	
src/supervise/classify_tests.rs	an_expansion_in_an_option_slot_is_refused_quoted_or_not	classify-sound	
src/supervise/classify_tests.rs	an_extended_glob_word_may_become_a_flag_or_a_name	classify-sound	
src/supervise/classify_tests.rs	an_option_after_time_is_the_time_programs_own	classify-sound	
src/supervise/classify_tests.rs	arithmetic_that_can_expand_a_subscript_is_not_a_read	classify-sound	
src/supervise/classify_tests.rs	aterm_forms_are_judged_by_their_full_subform	classify-sound	
src/supervise/classify_tests.rs	awk_programs_and_sed_scripts_are_read_not_dropped	classify-sound	
src/supervise/classify_tests.rs	an_awk_program_that_runs_or_writes_in_any_spelling_is_no_read	classify-sound	
src/supervise/classify_tests.rs	mark_is_not_a_read_only_ctl_verb	classify-sound	
src/supervise/classify_tests.rs	rg_hostname_bin_runs_a_program	classify-flags-on-argv	
src/supervise/classify_tests.rs	sed_script_options_cannot_hide_a_write	classify-sound	sed-script-and-alarm-idiom
src/supervise/classify_tests.rs	a_sed_bracket_delimiter_does_not_end_the_regex_early	classify-sound	sed-script-and-alarm-idiom
src/supervise/classify_tests.rs	sysctl_sets_the_kernel_from_a_file	classify-flags-on-argv	
src/supervise/classify_tests.rs	a_bare_redirection_with_no_command_is_not_read_only	classify-sound	
src/supervise/classify_tests.rs	a_file_read_by_substitution_or_a_subshell_redirect_is_read_only	classify-sound	split-segments
src/supervise/classify_tests.rs	git_remote_show_contacts_the_remote_only_when_it_names_one	classify-flags-on-argv	approval-safe-bash-sound
src/supervise/classify_tests.rs	the_v6_spellings_of_a_write_or_a_hazard_are_each_refused	classify-sound	classify-flags-on-argv,sed-script-and-alarm-idiom
src/supervise/classify_tests.rs	an_abbreviated_write_option_is_the_write	classify-sound	
src/supervise/classify_tests.rs	a_quoted_or_escaped_write_flag_is_still_a_write	classify-flags-on-argv	classify-sound
src/supervise/classify_tests.rs	quoting_a_data_word_that_is_not_a_flag_stays_a_read	classify-flags-on-argv	classify-precise-on-read-grammar
src/supervise/classify_tests.rs	a_quoted_flag_on_the_rest_of_an_rm_line_is_still_a_write	classify-flags-on-argv	classify-except-rm
src/supervise/classify_tests.rs	a_find_exec_programs_write_flag_is_a_write	classify-flags-on-argv	classify-sound
src/supervise/classify_tests.rs	a_quoted_git_verb_or_flag_is_still_a_write	classify-flags-on-argv	classify-sound
src/supervise/classify_tests.rs	a_brace_or_glob_word_that_may_become_a_write_flag_is_refused	classify-expansion-into-a-flag	classify-sound
src/supervise/classify_tests.rs	comments_heredocs_and_quoting_that_only_read_stay_reads	classify-precise-on-read-grammar	
src/supervise/classify_tests.rs	a_hash_after_an_escaped_blank_or_newline_opens_no_comment	prelex	
src/supervise/classify_tests.rs	danger_tokens_anywhere_fail_the_line	classify-sound	
src/supervise/classify_tests.rs	git_branch_creation_and_remote_set_branches_are_writes	classify-sound	
src/supervise/classify_tests.rs	git_grep_pager_spellings_and_git_help_are_not_read_only	classify-sound	
src/supervise/classify_tests.rs	git_reads_of_a_chosen_repository_or_through_its_drivers_are_refused	classify-sound	
src/supervise/classify_tests.rs	glob_matches_like_fnmatch	glob-match-fnmatch	
src/supervise/classify_tests.rs	output_file_flags_of_the_read_tools_are_writes	classify-sound	
src/supervise/classify_tests.rs	program_words_resolve_quotes_and_split_where_the_segments_do	program-words	
src/supervise/classify_tests.rs	quotes_hide_redirect_glyphs_but_not_substitutions	classify-sound	
src/supervise/classify_tests.rs	sed_capital_i_edits_in_place	classify-sound	
src/supervise/classify_tests.rs	segment_heads_must_be_known_reads	classify-sound	
src/supervise/classify_tests.rs	split_segments_sees_every_separator	split-segments	
src/supervise/classify_tests.rs	strip_quotes_keeps_shape	strip-quotes-shape	
src/supervise/classify_tests.rs	the_added_reads_are_reads_and_their_neighbours_are_not	classify-sound	
src/supervise/classify_tests.rs	the_alarm_idiom_is_a_wrapper_not_a_program	classify-sound	
src/supervise/classify_tests.rs	the_alarm_idiom_is_exactly_alarm_then_exec	sed-script-and-alarm-idiom	
src/supervise/classify_tests.rs	the_measured_bypasses_are_not_read_only	classify-sound	
src/supervise/classify_tests.rs	the_python_allowlist_is_opt_in	python-allowlist	
src/supervise/classify_tests.rs	the_session_corpus_classifies_as_the_manager_did	classify-sound	
src/supervise/classify_tests.rs	the_worker_verbs_are_judged_by_their_form	classify-sound	
src/supervise/classify_tests.rs	the_write_forms_of_system_reads_are_refused	classify-sound	
src/supervise/classify_tests.rs	what_a_wrapper_or_time_runs_is_never_a_keyword	classify-sound	
src/supervise/classify_tests.rs	whole_long_option_names_keep_flags_from_hiding_file_operands	approval-safe-bash-sound	
src/supervise/classify_tests.rs	zsh_arithmetic_that_can_run_a_command_is_not_a_read	classify-sound	
src/supervise/config.rs	the_autocompact_offer_key_reads_a_size_or_off	config-power-lattice	
src/supervise/config.rs	a_disk_knob_under_harness_is_the_disk_readers_not_an_unknown_key	config-retired-and-agent-keys	
src/supervise/config.rs	a_file_that_is_not_toml_is_read_inside_its_harness_section	config-line-reader-agrees	
src/supervise/config.rs	a_file_that_is_not_toml_reads_an_agent_table_too	config-line-reader-agrees	
src/supervise/config.rs	a_misplaced_agent_table_only_limits	config-misplaced-only-limits	
src/supervise/config.rs	a_misplaced_harness_key_limits_but_never_grants	config-misplaced-only-limits	
src/supervise/config.rs	a_misread_question_switch_changes_nothing	config-reader-never-grants	
src/supervise/config.rs	a_retired_key_written_false_is_read_as_the_limit_it_named	config-retired-and-agent-keys	
src/supervise/config.rs	a_value_it_cannot_read_keeps_its_default_and_only_an_off_limits	config-reader-never-grants	
src/supervise/config.rs	accounts_reads_its_three_words_and_only_limits	config-reader-never-grants	
src/supervise/config.rs	against_default_names_each_limit_and_never_a_text_as_one	config-texts-round-trip	
src/supervise/config.rs	an_agent_switched_off_is_a_limit_and_named_as_one	config-power-lattice	
src/supervise/config.rs	an_agent_table_switches_that_agent_off_and_nothing_else	config-retired-and-agent-keys	
src/supervise/config.rs	answer_questions_false_wins_over_any_priority_in_either_order	config-reader-never-grants	
src/supervise/config.rs	autopilot_reads_its_three_words_and_only_limits	config-reader-never-grants	
src/supervise/config.rs	each_key_of_an_agent_table_is_said_on_its_own	config-line-reader-agrees	
src/supervise/config.rs	every_spelling_toml_reads_is_the_key	config-reader-never-grants	
src/supervise/config.rs	from_path_reads_the_file_and_a_missing_one_is_the_defaults	config-from-path-and-hosted	
src/supervise/config.rs	limit_reset_ships_off_and_only_the_root_table_turns_it_on	config-reader-never-grants	
src/supervise/config.rs	occupancy_ships_observe_and_only_the_root_table_turns_prompt_on	config-reader-never-grants	
src/supervise/config.rs	only_enabled_is_per_agent	config-retired-and-agent-keys	
src/supervise/config.rs	question_priority_takes_its_three_words_and_a_refused_one_changes_nothing	config-reader-never-grants	
src/supervise/config.rs	texts_are_the_keys_and_read_back_as_the_policy	config-texts-round-trip	
src/supervise/config.rs	the_default_is_every_power_and_every_key_limits_it	config-power-lattice	
src/supervise/config.rs	the_memory_ceiling_reads_auto_off_or_gibibytes_and_limits_to_off	config-reader-never-grants	
src/supervise/config.rs	the_stuck_break_note_ships_off_and_only_the_root_table_grants_it	config-reader-never-grants	stuck-break-line
src/supervise/config.rs	usage_credits_ships_off_and_only_the_root_table_turns_it_on	config-reader-never-grants	
src/supervise/config.rs	usage_notes_ships_off_and_only_the_root_table_turns_it_on	config-reader-never-grants	
src/supervise/config.rs	usage_tap_ships_on_and_the_root_tables_off_limits_it	config-reader-never-grants	
src/supervise/escalate.rs	a_long_subject_and_reason_are_clipped_and_the_whole_fits_256_bytes	escalate-attention-text	
src/supervise/escalate.rs	a_question_dialogs_badge_names_its_question	escalate-kind-and-subject	
src/supervise/escalate.rs	a_question_read_whole_is_badged_by_its_question	escalate-kind-and-subject	
src/supervise/escalate.rs	a_status_field_is_read_off_the_ok_line	escalate-kind-and-subject	
src/supervise/escalate.rs	an_rm_breaker_boxs_attention_fits_the_keyed_cap_and_stays_closed	escalate-attention-text	
src/supervise/escalate.rs	every_rm_breaker_kind_is_badged_rm_breaker	escalate-kind-and-subject	
src/supervise/escalate.rs	the_attention_names_the_kind_the_subject_and_the_reason	escalate-attention-text	
src/supervise/escalate.rs	the_memory_escalation_names_its_conversation_whole_or_not_at_all	escalate-attention-text	
src/supervise/facts_loop_tests.rs	a_fact_that_would_not_fit_leaves_the_prompt_alone_and_stays_untold	fact-rides-turn-end	facts-no-progress-episode
src/supervise/facts_loop_tests.rs	a_hung_call_is_told_once_per_episode_across_both_doors	fact-rides-turn-end	stuck-break-line,facts-no-progress-episode
src/supervise/facts_loop_tests.rs	a_planted_word_reaches_no_write_at_either_door	facts-no-progress-episode	fact-rides-turn-end,stuck-break-line
src/supervise/facts_loop_tests.rs	a_prompt_left_in_the_composer_says_its_fact_not_submitted	fact-rides-turn-end	
src/supervise/facts_loop_tests.rs	a_restart_does_not_retell_a_told_call	facts-no-progress-episode	stuck-break-line
src/supervise/facts_loop_tests.rs	facts_off_tells_none_at_either_door	fact-rides-turn-end	stuck-break-line
src/supervise/facts_loop_tests.rs	the_stuck_fact_goes_after_the_hosts_notice_and_before_the_due_nudge	stuck-break-line	
src/supervise/facts_loop_tests.rs	the_stuck_line_waits_out_the_pace_of_a_nudge_the_door_typed	stuck-break-line	
src/supervise/facts_loop_tests.rs	the_stuck_lines_pace_counts_lines_that_went_across_a_restart	stuck-break-line	
src/supervise/facts_loop_tests.rs	with_the_key_off_a_break_is_mains_exactly	stuck-break-line	
src/supervise/facts_tests.rs	a_no_progress_episode_is_told_once_until_a_fresh_reading_no_longer_names_it	facts-no-progress-episode	
src/supervise/facts_tests.rs	a_seed_holds_a_told_call_by_its_own_key_alone	facts-no-progress-episode	
src/supervise/facts_tests.rs	an_episode_key_folds_the_group_and_the_leaders_birth	facts-no-progress-episode	
src/supervise/journal.rs	a_record_round_trips_through_its_json	journal-line-grammar-and-json	
src/supervise/journal.rs	each_printed_line_is_read_into_its_fields	journal-line-grammar-and-json	
src/supervise/journal.rs	every_word_is_read_by_its_own_name	journal-line-grammar-and-json	
src/supervise/ladder.rs	a_doubling_back_off_doubles_to_its_cap	doubling-backoff	
src/supervise/ladder.rs	a_ladder_clamps_to_its_last_step	ladder-clamp	
src/supervise/ledger_tests.rs	a_carried_turn_the_log_holds_is_placed_by_it	ledger-gather	
src/supervise/ledger_tests.rs	a_history_record_is_read_whole	ledger-gather	
src/supervise/ledger_tests.rs	a_ledger_an_update_could_not_carry_is_not_blamed_on_the_person	ledger-gather	
src/supervise/ledger_tests.rs	a_loop_journals_only_what_the_ledger_reads	ledger-mark-of-word	
src/supervise/ledger_tests.rs	a_malformed_since_is_refused_not_a_panic	ledger-since	
src/supervise/ledger_tests.rs	a_missing_source_is_named_and_the_rest_prints	ledger-gather	
src/supervise/ledger_tests.rs	a_reply_the_archive_lost_says_so	ledger-gather	
src/supervise/ledger_tests.rs	a_report_renders_its_view	blocks-views	
src/supervise/ledger_tests.rs	a_turn_whose_row_is_missing_is_named_in_the_span_that_holds_it	ledger-gather	
src/supervise/ledger_tests.rs	an_unread_log_guesses_no_time	ledger-gather	
src/supervise/ledger_tests.rs	every_journal_word_has_its_own_mark	ledger-mark-of-word	
src/supervise/ledger_tests.rs	every_mark_has_its_own_legend_row	ledger-html-assets	
src/supervise/ledger_tests.rs	since_drops_what_came_before_it	ledger-since	
src/supervise/ledger_tests.rs	since_takes_ms_or_a_time_word	ledger-since	
src/supervise/ledger_tests.rs	the_html_form_is_one_self_contained_page	ledger-render-escapes	
src/supervise/ledger_tests.rs	the_ledger_joins_the_four_sources	ledger-gather	
src/supervise/ledger_tests.rs	the_ledger_reads_the_log_and_no_clock	ledger-gather	
src/supervise/ledger_tests.rs	the_ledger_renders_its_goldens	ledger-render-escapes	
src/supervise/ledger_tests.rs	the_md_form_is_tables	ledger-render-escapes	
src/supervise/ledger_tests.rs	the_page_colours_each_mark_as_its_legend_row_does	ledger-html-assets	
src/supervise/ledger_tests.rs	without_the_log_no_working_time_is_claimed	ledger-gather	
src/supervise/limit.rs	a_bare_time_is_todays_or_tomorrows_and_midnight_is_12am	limit-zone-and-reset-grammar	
src/supervise/limit.rs	a_dated_reset_across_a_dst_change_is_placed_with_the_offset_there	limit-wall-placement	
src/supervise/limit.rs	a_dated_time_is_placed_in_its_zone	limit-reset-placement	
src/supervise/limit.rs	a_goal_echo_that_continues_by_itself_is_resumes_by_itself	limit-zone-and-reset-grammar	
src/supervise/limit.rs	a_passed_date_is_passed_and_the_year_left_out_is_the_nearest	limit-reset-placement	
src/supervise/limit.rs	a_reset_across_a_dst_jump_is_placed_in_real_time	limit-wall-placement	
src/supervise/limit.rs	a_reset_that_names_its_year_parses	limit-zone-and-reset-grammar	
src/supervise/limit.rs	a_rules_file_becomes_one_line	limit-zone-and-reset-grammar	
src/supervise/limit.rs	a_walls_bare_clock_is_the_first_occurrence_after_its_print	limit-wall-placement	
src/supervise/limit.rs	a_zones_offset_is_read_at_the_instant	limit-wall-placement	
src/supervise/limit.rs	both_spellings_parse_with_the_zone	limit-zone-and-reset-grammar	
src/supervise/limit.rs	the_auto_continue_shapes_are_a_time_today_or_tomorrow_or_a_minute	limit-zone-and-reset-grammar	
src/supervise/limit.rs	the_local_zone_is_the_c_librarys_and_no_zone_is_made_up	limit-zone-and-reset-grammar	
src/supervise/memory.rs	a_higher_ceiling_or_off_is_the_limit	memory-ceiling-order	
src/supervise/memory.rs	a_measure_past_the_ceiling_warns_and_acts_only_in_trouble_on_the_dominant_agent	memory-judgement	
src/supervise/memory.rs	a_warning_episode_ends_an_eighth_under_the_ceiling	memory-judgement	
src/supervise/memory.rs	the_automatic_ceiling_moves_with_the_available_memory	memory-ceiling-formula	
src/supervise/memory.rs	the_dominant_consumer_is_the_largest_and_a_fifth_of_the_memory_in_use	memory-judgement	
src/supervise/memory.rs	the_key_reads_auto_off_and_gibibytes	mode-word-parsers	
src/supervise/memory.rs	the_machine_is_in_trouble_at_critical_or_at_warn_while_it_swaps	memory-judgement	
src/supervise/memory.rs	the_verdict_moves_with_trouble_rank_and_restart_not_with_the_numbers	memory-judgement	
src/supervise/occupancy/door_tests.rs	a_successor_paces_by_the_lines_typed_at_a_break_alone	stuck-break-line	
src/supervise/occupancy/door_tests.rs	a_successor_seeds_the_no_progress_rows_by_their_kind	facts-no-progress-episode	
src/supervise/occupancy/door_tests.rs	the_stuck_line_holds_in_order_and_waits_for_the_upgrades_notice	stuck-break-line	
src/supervise/policy/approval_tests.rs	a_barred_row_shown_as_the_description_is_still_judged	approval-command-readings	
src/supervise/policy/approval_tests.rs	a_box_more_than_one_tool_draws_is_held_under_a_bare_rule_for_any_of_them	approval-owner-review-held	
src/supervise/policy/approval_tests.rs	a_box_taller_than_the_pane_is_pressed_by_no_level	approval-whole-read	
src/supervise/policy/approval_tests.rs	a_box_taller_than_the_screen_in_a_bypass_session_is_declined_with_its_flagged_rm	approval-whole-read	
src/supervise/policy/approval_tests.rs	a_box_that_offers_no_way_to_give_a_reason_is_never_declined	approval-amend-input	
src/supervise/policy/approval_tests.rs	a_box_that_runs_on_another_machine_is_not_proven_by_local_checks	approval-safe-bash-sound	
src/supervise/policy/approval_tests.rs	a_box_the_owners_own_settings_send_to_a_person_is_never_pressed	approval-owner-review-held	
src/supervise/policy/approval_tests.rs	a_box_whose_calls_message_reaches_off_the_screen_is_held	approval-owner-review-held	
src/supervise/policy/approval_tests.rs	a_box_whose_footer_wraps_is_decided_by_its_whole_footer	approval-command-readings	
src/supervise/policy/approval_tests.rs	a_box_whose_head_is_off_the_screen_is_held_because_its_tail_names_no_box	approval-whole-read	approval-owner-review-held
src/supervise/policy/approval_tests.rs	a_command_the_classifier_cannot_name_is_never_pressed	approval-safe-bash-sound	
src/supervise/policy/approval_tests.rs	a_credits_consent_is_taken_only_under_usage_credits	approval-dialog-rules	approval-never-spends
src/supervise/policy/approval_tests.rs	a_default_to_no_tool_box_is_pressed_by_moving_the_focus	approval-guarded-press	
src/supervise/policy/approval_tests.rs	a_dialog_ink_wrapped_is_answered_by_its_own_rule	approval-dialog-rules	
src/supervise/policy/approval_tests.rs	a_dialog_whose_label_wraps_at_a_name_or_inside_a_word_gets_the_whole_answer	approval-dialog-rules	
src/supervise/policy/approval_tests.rs	a_digit_is_never_pressed_into_an_open_amend_input	approval-amend-input	
src/supervise/policy/approval_tests.rs	a_file_box_the_agents_bare_ask_rule_sends_is_held_at_every_level	approval-owner-review-held	
src/supervise/policy/approval_tests.rs	a_fixed_refusal_label_under_the_focus_is_no_open_input	approval-amend-input	
src/supervise/policy/approval_tests.rs	a_footerless_setup_dialog_is_declined_or_escalated_never_idle	approval-not-a-box	
src/supervise/policy/approval_tests.rs	a_git_read_is_approved_only_where_its_config_runs_nothing	approval-git-read	
src/supervise/policy/approval_tests.rs	a_git_read_is_judged_in_the_workers_environment	approval-git-read	
src/supervise/policy/approval_tests.rs	a_git_read_is_judged_where_the_bash_tool_stands	approval-git-read	
src/supervise/policy/approval_tests.rs	a_git_read_whose_bash_tool_settings_change_git_escalates	approval-git-read	
src/supervise/policy/approval_tests.rs	a_grep_or_rg_value_option_is_no_pattern_to_the_secret_check	approval-safe-bash-sound	
src/supervise/policy/approval_tests.rs	a_held_message_half_drawn_in_a_narrow_pane_never_focuses_a_piece_of_its_deny	approval-one-shot-only	
src/supervise/policy/approval_tests.rs	a_long_option_flag_protects_its_files_and_exempts_its_pattern	approval-safe-bash-sound	
src/supervise/policy/approval_tests.rs	a_muse_note_open_under_the_recommended_word_is_a_persons	approval-dialog-rules	
src/supervise/policy/approval_tests.rs	a_not_ruled_out_why_aterm_cannot_read_is_refused_in_its_own_words	approval-breaker-naming	approval-level-monotone
src/supervise/policy/approval_tests.rs	a_note_free_folder_box_is_held_to_the_proof_gaps_as_the_no_rule_note_is	approval-no-rule-read-only-sound	approval-safe-bash-sound
src/supervise/policy/approval_tests.rs	a_plan_is_approved_on_its_least_power_yes	approval-one-shot-only	
src/supervise/policy/approval_tests.rs	a_read_box_with_no_folder_grant_is_the_owners_review	approval-owner-review-held	
src/supervise/policy/approval_tests.rs	a_read_only_box_is_approved_outside_bypass_and_never_in_it	approval-safe-bash-sound	
src/supervise/policy/approval_tests.rs	a_read_outside_cwd_is_approved_under_an_allowed_root_unless_it_is_a_secret	approval-read-rule	
src/supervise/policy/approval_tests.rs	a_read_through_a_symlink_is_judged_where_it_leads	approval-read-rule	
src/supervise/policy/approval_tests.rs	a_removal_is_rm_or_rmdir_as_a_word_in_any_case	approval-breaker-read-only-sound	
src/supervise/policy/approval_tests.rs	a_reset_confirmation_is_never_answered_yes_at_any_level	approval-never-spends	
src/supervise/policy/approval_tests.rs	a_row_that_cannot_be_a_wrap_ends_the_command	approval-command-readings	
src/supervise/policy/approval_tests.rs	a_sed_edit_box_under_a_bash_specifier_rule_or_bash_hook_is_held_at_every_level	approval-owner-review-held	
src/supervise/policy/approval_tests.rs	a_shell_startup_the_models_do_not_assume_proves_no_rm_and_reads_nothing	approval-safe-bash-sound	
src/supervise/policy/approval_tests.rs	a_tall_box_under_a_not_ruled_out_breaker_is_declined_with_a_split	approval-whole-read	
src/supervise/policy/approval_tests.rs	a_tall_box_under_a_why_aterm_cannot_read_is_declined_with_a_plainer_split	approval-whole-read	
src/supervise/policy/approval_tests.rs	a_title_whole_on_its_row_leaves_the_box_its_body	approval-command-readings	
src/supervise/policy/approval_tests.rs	a_tmpdir_that_holds_too_much_is_no_scratch_root	approval-roots	
src/supervise/policy/approval_tests.rs	a_too_many_box_is_held_to_the_proof_gaps	approval-breaker-read-only-sound	approval-no-rule-read-only-sound
src/supervise/policy/approval_tests.rs	a_trace_names_the_rule_that_decided	approval-trace-names-decider	
src/supervise/policy/approval_tests.rs	a_transcript_candidate_cannot_remove_a_possible_tools_owner_hold	approval-owner-review-held	box-command-pending-call
src/supervise/policy/approval_tests.rs	a_trust_dialog_escalates_off_the_sessions_folder_or_its_roots	approval-trust-dialog	
src/supervise/policy/approval_tests.rs	a_trust_dialog_for_the_folder_its_starter_trusted_is_answered_under_no_root	approval-trust-dialog	
src/supervise/policy/approval_tests.rs	a_wide_character_row_never_hides_a_wrapped_tail	approval-command-readings	
src/supervise/policy/approval_tests.rs	a_worker_path_entry_the_worker_writes_is_refused_before_the_probe_runs_it	approval-git-read	
src/supervise/policy/approval_tests.rs	a_wrapped_non_shell_owner_review_is_never_pressed	approval-owner-review-held	
src/supervise/policy/approval_tests.rs	a_write_on_a_second_row_is_not_laundered_by_the_space_join	approval-safe-bash-sound	
src/supervise/policy/approval_tests.rs	a_yes_is_never_taken_by_its_label_under_a_rest_that_opens_with_no_capital	approval-one-shot-only	
src/supervise/policy/approval_tests.rs	agys_first_run_is_pressed_by_no_level_and_its_survey_is_no_box	approval-dialog-rules	approval-not-a-box
src/supervise/policy/approval_tests.rs	an_answer_kept_for_good_by_its_label_is_never_taken_under_a_lowercase_rest	approval-dialog-rules	
src/supervise/policy/approval_tests.rs	an_arithmetic_assignment_is_no_read_in_a_plain_bash_box	approval-safe-bash-sound	classify-sound
src/supervise/policy/approval_tests.rs	an_edit_after_a_cut_output_keeps_a_possible_tools_owner_hold	approval-owner-review-held	
src/supervise/policy/approval_tests.rs	an_edit_shaped_box_keeps_every_possible_tools_owner_hold	approval-owner-review-held	
src/supervise/policy/approval_tests.rs	an_rm_breaker_on_scratch_is_approved_in_bypass_under_its_row_guard	approval-rm-breaker-sound	
src/supervise/policy/approval_tests.rs	an_rm_breaker_through_a_link_out_of_scratch_escalates	approval-rm-breaker-sound	
src/supervise/policy/approval_tests.rs	an_unproven_rm_breaker_under_safe_is_approved_with_a_warning	approval-level-monotone	
src/supervise/policy/approval_tests.rs	another_harnesss_trust_dialog_is_answered_only_in_its_workers_trusted_folder	approval-trust-dialog	
src/supervise/policy/approval_tests.rs	claude_code_2_1_304s_reset_kinds_still_spend	approval-never-spends	
src/supervise/policy/approval_tests.rs	claude_code_2_1_304s_spend_limit_nudge_and_buy_screen_are_purchases	approval-never-spends	
src/supervise/policy/approval_tests.rs	continue_without_is_a_refusal_never_a_consent	approval-dialog-rules	approval-never-spends
src/supervise/policy/approval_tests.rs	every_rm_breaker_kind_is_named_and_full_power_presses_each	approval-level-monotone	
src/supervise/policy/approval_tests.rs	full_power_accepts_a_consent_and_never_buys	approval-never-spends	
src/supervise/policy/approval_tests.rs	full_power_answers_agy_boxes_by_role	approval-one-shot-only	
src/supervise/policy/approval_tests.rs	full_power_answers_each_dialog_by_its_own_rule	approval-dialog-rules	
src/supervise/policy/approval_tests.rs	full_power_answers_every_permission_box_with_its_one_shot_allow	approval-one-shot-only	
src/supervise/policy/approval_tests.rs	full_power_answers_muse_boxes_by_role	approval-one-shot-only	
src/supervise/policy/approval_tests.rs	full_power_confirms_the_model_switch_the_person_typed	approval-dialog-rules	
src/supervise/policy/approval_tests.rs	full_power_declines_a_grant_only_box_and_leaves_an_unsound_one	approval-one-shot-only	
src/supervise/policy/approval_tests.rs	full_power_leaves_muses_tool_action_and_unix_socket_boxes_to_a_person	approval-one-shot-only	
src/supervise/policy/approval_tests.rs	full_power_never_takes_a_standing_grant	approval-one-shot-only	
src/supervise/policy/approval_tests.rs	full_power_presses_a_card_mode_tool_box	approval-one-shot-only	
src/supervise/policy/approval_tests.rs	full_power_presses_bash_under_every_header_form	approval-one-shot-only	
src/supervise/policy/approval_tests.rs	full_power_presses_every_permission_kind_under_its_judged_row	approval-one-shot-only	
src/supervise/policy/approval_tests.rs	full_power_presses_the_measured_touch_box_whose_no_is_garbled	approval-level-monotone	
src/supervise/policy/approval_tests.rs	full_power_trusts_any_folder_and_names_the_backstops_grants	approval-trust-dialog	
src/supervise/policy/approval_tests.rs	full_power_with_a_model_fallback_waits_at_the_limit_and_never_buys_credits	approval-never-spends	
src/supervise/policy/approval_tests.rs	muses_several_questions_multi_select_and_review_are_a_persons	approval-dialog-rules	
src/supervise/policy/approval_tests.rs	no_rule_ever_spends_a_limit_reset	approval-never-spends	
src/supervise/policy/approval_tests.rs	only_a_claude_sessions_box_is_judged	approval-not-a-box	
src/supervise/policy/approval_tests.rs	only_the_one_shot_allow_is_ever_pressed	approval-one-shot-only	
src/supervise/policy/approval_tests.rs	queued_edit_rows_do_not_remove_a_possible_tools_owner_hold	approval-owner-review-held	
src/supervise/policy/approval_tests.rs	the_2_1_281_breaker_with_its_auto_deny_countdown_is_the_rm_rules	approval-rm-breaker-sound	
src/supervise/policy/approval_tests.rs	the_advisory_word_refuses_a_secret_and_a_proof_gap	approval-safe-bash-sound	
src/supervise/policy/approval_tests.rs	the_audits_unsound_rm_approvals_escalate_and_full_power_still_presses_them	approval-level-monotone	
src/supervise/policy/approval_tests.rs	the_chrome_install_offer_is_declined_with_not_now_and_nothing_else	approval-dialog-rules	approval-one-shot-only
src/supervise/policy/approval_tests.rs	the_chrome_setup_panel_is_declined_in_every_phase	approval-dialog-rules	approval-one-shot-only
src/supervise/policy/approval_tests.rs	the_claude_tmp_root_is_the_uids_own	approval-roots	
src/supervise/policy/approval_tests.rs	the_decline_text_is_one_typeable_line	approval-decline-text	
src/supervise/policy/approval_tests.rs	the_default_trusts_no_folder	approval-trust-dialog	
src/supervise/policy/approval_tests.rs	the_footer_names_the_mode_and_a_2_1_280_box_hides_it	approval-footer-mode	
src/supervise/policy/approval_tests.rs	the_chooser_is_pressed_by_no_rule	approval-never-spends	
src/supervise/policy/approval_tests.rs	the_grant_row_is_taken_by_its_own_rule_and_only_with_its_key_on	approval-limit-dialog	
src/supervise/policy/approval_tests.rs	the_autocompact_offer_chooses_the_accept_row_at_400k	approval-autocompact-offer	
src/supervise/policy/approval_tests.rs	the_autocompact_offer_steps_the_size_before_any_enter	approval-autocompact-offer	
src/supervise/policy/approval_tests.rs	the_autocompact_offer_is_a_persons_unless_read_whole_and_on	approval-autocompact-offer	
src/supervise/policy/approval_tests.rs	the_auto_mode_setup_offer_is_put_off_with_not_now_at_every_level	approval-auto-mode-setup	
src/supervise/policy/approval_tests.rs	a_look_alike_of_the_auto_mode_setup_offer_is_not_pressed_by_its_rule	approval-auto-mode-setup	
src/supervise/policy/approval_tests.rs	the_lab_boxes_are_answered_as_measured	approval-one-shot-only	approval-owner-review-held,approval-breaker-naming
src/supervise/policy/approval_tests.rs	the_limit_dialog_in_the_fullscreen_panel_waits_for_its_dated_reset	approval-limit-dialog	
src/supervise/policy/approval_tests.rs	the_limit_options_dialog_chooses_the_wait_row_by_its_label	approval-limit-dialog	
src/supervise/policy/approval_tests.rs	the_limit_options_dialog_whose_title_wraps_chooses_the_wait_row	approval-command-readings	
src/supervise/policy/approval_tests.rs	the_limit_options_dialog_without_a_wait_row_is_escalated	approval-limit-dialog	
src/supervise/policy/approval_tests.rs	the_measured_decline_takes_one_guarded_keystroke_per_state	approval-guarded-press	
src/supervise/policy/approval_tests.rs	the_measured_trust_dialog_is_answered_for_the_sessions_folder_under_a_trust_root	approval-trust-dialog	
src/supervise/policy/approval_tests.rs	the_mktemp_guards_exit_and_colon_prove_at_the_box	approval-rm-breaker-sound	
src/supervise/policy/approval_tests.rs	the_owners_subagent_rm_box_is_pressed_at_default_power	approval-level-monotone	
src/supervise/policy/approval_tests.rs	the_mcp_server_dialog_is_a_persons_at_every_level	approval-dialog-rules	approval-one-shot-only
src/supervise/policy/approval_tests.rs	the_press_gate_reads_a_kept_answer_by_its_role_its_label_or_its_rows	approval-one-shot-only	
src/supervise/policy/approval_tests.rs	the_press_gate_refuses_each_kept_mcp_answer_under_any_title	approval-one-shot-only	
src/supervise/policy/approval_tests.rs	the_reset_rule_takes_only_the_one_reset_on_offer_and_never_a_fable_row	approval-limit-dialog	
src/supervise/policy/approval_tests.rs	the_rest_of_an_rm_breaker_line_must_read	approval-rm-breaker-sound	
src/supervise/policy/approval_tests.rs	the_session_paused_dialog_switches_to_the_fallback	approval-dialog-rules	
src/supervise/policy/approval_tests.rs	a_too_many_box_is_judged_by_its_screens_readings_alone	approval-breaker-read-only-sound	
src/supervise/policy/approval_tests.rs	a_reading_that_turns_on_a_blank_the_screen_cannot_show_proves_no_box	approval-safe-bash-sound	approval-breaker-read-only-sound
src/supervise/policy/approval_tests.rs	readings_read_only_names_the_classifiers_refusal_before_the_blanks	approval-safe-bash-sound	
src/supervise/policy/approval_tests.rs	a_box_whose_awk_or_sort_writes_in_a_spelling_it_reads_is_not_approved	approval-safe-bash-sound	classify-sound
src/supervise/policy/approval_tests.rs	every_measured_barred_box_is_judged_by_its_screen_alone	approval-breaker-read-only-sound	
src/supervise/policy/approval_tests.rs	the_no_rule_note_alone_is_lifted_by_the_read_only_proof_under_its_own_rule	approval-no-rule-read-only-sound	approval-network-free
src/supervise/policy/approval_tests.rs	every_other_vendor_note_on_a_bash_box_stays_with_a_person	approval-no-rule-read-only-sound	approval-safe-bash-sound
src/supervise/policy/approval_tests.rs	the_no_rule_note_stays_with_a_person_over_what_it_may_stand_for	approval-no-rule-read-only-sound	approval-safe-bash-sound
src/supervise/policy/approval_tests.rs	a_reading_that_may_open_a_network_connection_proves_no_box	approval-network-free	approval-safe-bash-sound
src/supervise/policy/approval_tests.rs	a_reading_that_holds_a_brace_expansion_proves_no_box	approval-brace-free	approval-breaker-read-only-sound
src/supervise/policy/approval_tests.rs	a_bash_read_of_a_secret_is_not_read_only	approval-safe-bash-sound	
src/supervise/policy/approval_tests.rs	a_non_secret_read_beside_it_stays_read_only	approval-safe-bash-sound	
src/supervise/policy/approval_tests.rs	a_git_remote_show_that_names_no_remote_is_a_read	approval-safe-bash-sound	
src/supervise/policy/approval_tests.rs	the_too_many_breaker_over_a_proven_read_is_approved_under_its_rule	approval-breaker-read-only-sound	approval-level-monotone
src/supervise/policy/approval_tests.rs	the_twin_scan_never_lists_inside_another_apps_data	approval-read-rule	
src/supervise/policy/approval_tests.rs	trust_roots_are_read_from_the_config_spelling	approval-roots	
src/supervise/policy/approval_tests.rs	under_the_owners_order_the_reset_rule_takes_the_first_listed_kind_on_offer	approval-limit-dialog	
src/supervise/policy/approval_tests.rs	under_zsh_only_a_plain_line_is_a_proven_read	approval-safe-bash-sound	
src/supervise/policy/approval_tests.rs	with_no_trust_root_only_the_system_roots_are_read	approval-read-rule	
src/supervise/policy/box_call_tests.rs	a_first_turns_call_written_before_its_prompt_row_still_waits	box-command-pending-call	
src/supervise/policy/box_call_tests.rs	a_call_row_with_no_uuid_is_ended_by_a_later_prompt	box-command-pending-call	
src/supervise/policy/box_command_tests.rs	a_reading_whose_words_turn_on_a_blank_the_screen_cannot_show_proves_nothing	box-command-blanks-as-drawn	
src/supervise/policy/claude_settings_tests.rs	a_hook_matcher_matches_bash_as_claude_code_matches_it	hook-matcher	
src/supervise/policy/claude_settings_tests.rs	a_linked_worktree_reads_the_main_worktrees_local_settings	settings-chain-order	
src/supervise/policy/claude_settings_tests.rs	a_managed_preference_domain_is_a_document_this_reader_cannot_see	managed-preferences	
src/supervise/policy/claude_settings_tests.rs	a_rule_names_its_tool_bare_or_with_a_specifier	rule-tool-parse	
src/supervise/policy/claude_settings_tests.rs	a_settings_file_not_there_in_a_file_provider_domain_sets_nothing	settings-file-read-and-fence	
src/supervise/policy/claude_settings_tests.rs	a_settings_file_that_cannot_be_read_is_reported_not_skipped	owner-asks-extraction	
src/supervise/policy/claude_settings_tests.rs	a_settings_file_under_a_file_provider_domain_is_reported_never_opened	settings-file-read-and-fence	
src/supervise/policy/claude_settings_tests.rs	an_agent_whose_directory_is_not_known_has_its_project_reported	settings-chain-order	
src/supervise/policy/claude_settings_tests.rs	an_empty_or_marked_settings_file_reads_as_claude_code_reads_it	settings-parse	
src/supervise/policy/claude_settings_tests.rs	provider_home_stage	§4	
src/supervise/policy/claude_settings_tests.rs	the_agents_own_settings_chain_is_read_for_bare_ask_rules	settings-chain-order	
src/supervise/policy/claude_settings_tests.rs	the_approval_lane_reads_its_settings_in_a_file_provider_domain_of_this_home	settings-file-read-and-fence	
src/supervise/policy/claude_settings_tests.rs	the_chain_is_read_where_claude_code_reads_it	settings-chain-order	
src/supervise/policy/claude_settings_tests.rs	the_chains_pre_tool_use_hooks_are_read_with_their_matchers	owner-asks-extraction	
src/supervise/policy/claude_settings_tests.rs	the_managed_file_and_its_drop_ins_open_the_chain	settings-chain-order	
src/supervise/policy/claude_settings_tests.rs	the_routes_fence_opens_no_git_file_in_a_file_provider_domain	settings-file-read-and-fence	
src/supervise/policy/exception_tests.rs	a_bucket_no_reset_ends_is_an_exception_only_while_the_tab_cannot_switch	exception-for-table	
src/supervise/policy/exception_tests.rs	a_cleared_goal_names_its_cause	exception-for-table	
src/supervise/policy/exception_tests.rs	a_full_context_three_compacts_leave_full_is_an_exception_with_the_way_back	exception-for-table	
src/supervise/policy/exception_tests.rs	a_lost_logins_notice_is_what_the_point_decides	exception-for-table	
src/supervise/policy/exception_tests.rs	a_persons_own_login_is_theirs	exception-for-table	
src/supervise/policy/exception_tests.rs	a_spend_wall_with_no_reset_says_so_once_and_the_action_is_the_strings	exception-for-table	
src/supervise/policy/exception_tests.rs	a_wait_is_never_an_exception	exception-for-table	
src/supervise/policy/exception_tests.rs	a_wall_that_answers_three_tries_an_hour_past_its_reset_is_futile	exception-for-table	
src/supervise/policy/exception_tests.rs	the_sign_in_is_told_where_it_is_the_owners	exception-for-table	
src/supervise/policy/git_config_tests.rs	a_clean_repository_loads_nothing_that_runs	git-view-refines-git	
src/supervise/policy/git_config_tests.rs	a_directory_that_does_not_exist_is_read_at_its_nearest_ancestor	git-view-refines-git	
src/supervise/policy/git_config_tests.rs	a_directory_this_check_cannot_resolve_is_refused	git-reads-placement	
src/supervise/policy/git_config_tests.rs	a_filter_driver_counts_only_where_the_attributes_select_it	git-view-refines-git	
src/supervise/policy/git_config_tests.rs	a_git_behind_a_wrapper_is_still_placed	git-reads-placement	
src/supervise/policy/git_config_tests.rs	a_git_read_runs_from_every_directory_the_shell_may_stand_in	git-reads-placement	
src/supervise/policy/git_config_tests.rs	a_git_read_runs_in_the_cwd_and_every_directory_the_line_names	git-reads-placement	
src/supervise/policy/git_config_tests.rs	a_git_this_check_cannot_place_is_refused	git-reads-placement	
src/supervise/policy/git_config_tests.rs	a_line_without_git_has_no_git_reads	git-reads-placement	
src/supervise/policy/git_config_tests.rs	a_partial_clone_is_a_hazard	git-hazard	
src/supervise/policy/git_config_tests.rs	a_signing_program_is_a_hazard_only_on_a_read_that_displays_signatures	git-hazard	
src/supervise/policy/git_config_tests.rs	a_submodules_own_config_is_seen	git-view-refines-git	
src/supervise/policy/git_config_tests.rs	a_worker_path_that_cannot_name_the_git_is_refused	git-program-on-path	
src/supervise/policy/git_config_tests.rs	an_executable_post_index_change_hook_is_seen	git-view-refines-git	
src/supervise/policy/git_config_tests.rs	an_included_file_and_the_global_config_are_seen	git-view-refines-git	
src/supervise/policy/git_config_tests.rs	an_index_hook_is_a_hazard	git-hazard	
src/supervise/policy/git_config_tests.rs	each_dangerous_key_in_the_repository_is_seen	git-view-refines-git	
src/supervise/policy/git_config_tests.rs	each_key_that_runs_a_program_on_a_read_is_a_hazard	git-hazard	
src/supervise/policy/git_config_tests.rs	keys_that_cannot_run_on_this_read_are_not_hazards	git-hazard	
src/supervise/policy/git_config_tests.rs	the_check_never_runs_the_fsmonitor_it_finds	git-view-refines-git	
src/supervise/policy/git_config_tests.rs	the_claude_code_directory_is_resolved_as_claude_code_resolves_it	worker-var-and-claude-dir	
src/supervise/policy/git_config_tests.rs	the_first_entry_of_a_variable_wins	worker-var-and-claude-dir	
src/supervise/policy/git_config_tests.rs	the_git_on_the_workers_path_is_the_one_read	git-view-refines-git	
src/supervise/policy/git_config_tests.rs	the_workers_external_diff_is_a_hazard	git-view-refines-git	
src/supervise/policy/git_config_tests.rs	the_workers_git_config_variables_are_seen	git-view-refines-git	
src/supervise/policy/git_config_tests.rs	what_a_git_read_does_beyond_reading_is_read_off_the_line	git-reads-flags	
src/supervise/policy/git_config_tests.rs	what_only_git_config_reads_does_not_hide_the_effective_config	git-view-refines-git	
src/supervise/policy/guard.rs	a_copy_or_a_swapped_box_does_not_satisfy_the_guard	row-guard-exact	
src/supervise/policy/guard.rs	a_long_row_is_guarded_by_an_anchored_prefix_that_fits	row-guard-exact	
src/supervise/policy/guard.rs	a_row_guard_matches_its_row_and_nothing_else_on_the_box	row-guard-exact	
src/supervise/policy/guard.rs	punctuation_and_bars_are_literal	row-guard-exact	
src/supervise/policy/guard.rs	the_gen_fence_is_added_only_where_the_server_says_it_can	key-args-gen-fence	
src/supervise/policy/guard.rs	the_guard_of_a_trimmed_read_matches_the_row_the_server_tests	row-guard-exact	
src/supervise/policy/login_tests.rs	a_subscription_session_chooses_the_subscription_row_where_the_dialog_opens_it	login-wall-acts	
src/supervise/policy/login_tests.rs	an_unknown_console_or_platform_method_is_the_owners	login-wall-acts	
src/supervise/policy/login_tests.rs	each_dialog_state_gets_its_act	login-wall-acts	
src/supervise/policy/login_tests.rs	the_approval_policy_answers_only_the_picker_of_its_own_login	login-wall-acts	
src/supervise/policy/login_tests.rs	the_success_enter_goes_once	login-wall-acts	
src/supervise/policy/login_tests.rs	the_dialogs_esc_is_fenced_on_its_cancel_row	login-wall-acts	
src/supervise/policy/login_tests.rs	the_picker_gate_backs_out_only_of_its_own_login_nobody_needs	login-wall-acts	
src/supervise/policy/login_tests.rs	the_picker_aterm_opened_and_nobody_needs_is_backed_out_of	login-wall-acts	
src/supervise/policy/login_tests.rs	no_measured_claude_login_screen_is_a_sign_in_menu	login-wall-acts	
src/supervise/policy/login_tests.rs	the_dialog_short_of_its_success_stands	login-notices	
src/supervise/policy/notice_tests.rs	every_notice_has_four_parts_each_one_sentence	login-notices	
src/supervise/policy/notice_tests.rs	every_notice_is_one_the_message_engine_admits	login-notices	
src/supervise/policy/notice_tests.rs	a_step_that_names_a_command_ends_with_it	login-notices	
src/supervise/policy/notice_tests.rs	one_sentence_refuses_two_sentences_whatever_opens_them	login-notices	
src/supervise/policy/notice_tests.rs	placeholders_are_known_and_every_one_is_filled	login-notices	
src/supervise/policy/notice_tests.rs	what_happened_holds_no_placeholder	login-notices	
src/supervise/policy/notice_tests.rs	ids_are_unique_words_and_codes_unique_letter_pairs	login-notices	
src/supervise/policy/notice_tests.rs	what_aterm_did_is_aterms_and_the_expectation_asks_nothing	login-notices	
src/supervise/policy/notice_tests.rs	a_label_is_named_whole_where_it_fits_and_never_cut_in_a_command	login-notices	
src/supervise/policy/outlook_tests.rs	a_session_on_usage_credits_waits_for_the_subscriptions_reset	credits-hold	
src/supervise/policy/outlook_tests.rs	a_wall_is_answered_by_the_harness_or_by_nobody	answer-of-outlook	
src/supervise/policy/outlook_tests.rs	every_wall_kind_has_an_outlook	outlook-table	
src/supervise/policy/question_tests.rs	a_cut_question_is_escalated	decide-routes-questions	
src/supervise/policy/question_tests.rs	a_persons_text_hands_the_question_over	question-recommended-decision	
src/supervise/policy/question_tests.rs	a_refusal_is_no_and_never_a_word_that_starts_with_it	question-destructive-refusal	
src/supervise/policy/question_tests.rs	a_vendor_answer_is_kept_only_as_a_whole_label	question-recommended-decision	
src/supervise/policy/question_tests.rs	an_unmarked_destructive_question_is_answered_by_its_refusal_or_is_the_persons	question-destructive-refusal	
src/supervise/policy/question_tests.rs	an_unread_question_is_escalated	question-recommended-decision	
src/supervise/policy/question_tests.rs	answer_questions_is_independent_of_approve	decide-routes-questions	
src/supervise/policy/question_tests.rs	answer_questions_off_hands_every_question_over	decide-routes-questions	
src/supervise/policy/question_tests.rs	focus_on_the_chat_row_is_moved_up	question-recommended-decision	
src/supervise/policy/question_tests.rs	focus_on_the_free_text_row_is_moved_up_never_answered	question-recommended-decision	
src/supervise/policy/question_tests.rs	multiselect_toggles_each_recommended_then_advances	question-recommended-decision	
src/supervise/policy/question_tests.rs	multiselect_with_a_foreign_check_hands_over	question-recommended-decision	
src/supervise/policy/question_tests.rs	neither_type_something_nor_chat_is_ever_chosen	question-target-invariant	
src/supervise/policy/question_tests.rs	no_marked_option_answers_option_one	question-recommended-decision	
src/supervise/policy/question_tests.rs	no_visible_focus_is_never_answered	question-recommended-decision	
src/supervise/policy/question_tests.rs	the_incident_question_is_answered_with_its_recommended_option	question-recommended-decision	
src/supervise/policy/question_tests.rs	the_incidents_unmarked_publish_question_is_escalated	question-destructive-refusal	
src/supervise/policy/question_tests.rs	the_preview_form_is_focused_then_entered	question-recommended-decision	
src/supervise/policy/question_tests.rs	the_recommended_option_wins_over_the_focus	question-recommended-decision	
src/supervise/policy/question_tests.rs	the_review_tab_submits	question-recommended-decision	
src/supervise/policy/question_tests.rs	tier1_the_question_decider_chooses_only_what_the_model_enables	question-decider-refines-model	
src/supervise/policy/question_text_tests.rs	a_multi_select_is_typed_then_submitted_by_its_button_never_in_the_field	choose-text-table	
src/supervise/policy/question_text_tests.rs	a_multi_select_text_its_dialog_would_draw_over_is_never_typed	choose-text-table	
src/supervise/policy/question_text_tests.rs	a_multi_select_text_keeps_its_rows_only_where_the_reflow_cannot_drop_one	fit-and-reflow	
src/supervise/policy/question_text_tests.rs	a_single_select_is_typed_then_entered_and_anything_else_is_the_persons	choose-text-table	
src/supervise/policy/question_text_tests.rs	a_tab_the_text_cannot_go_to_is_answered_with_its_recommended_option_and_says_so	choose-text-table	
src/supervise/policy/question_text_tests.rs	decide_answers_by_the_contexts_priority	decide-routes-questions	
src/supervise/policy/question_text_tests.rs	the_fit_estimate_is_at_or_above_the_measured_wraps	fit-and-reflow	
src/supervise/policy/question_text_tests.rs	the_policy_texts_keep_their_rules	policy-text-rules	
src/supervise/policy/question_text_tests.rs	tier1_text_mode_decider_chooses_only_what_the_model_enables	question-decider-refines-model	
src/supervise/policy/reset.rs	a_host_that_answers_nothing_takes_no_reset	reset-go-and-host-defaults	
src/supervise/policy/reset.rs	a_short_wall_is_reset_only_where_the_offer_expires_within_a_day	confirm-verdict	
src/supervise/policy/reset.rs	enter_only_at_the_exact_confirmation_of_the_row_taken	confirm-verdict	
src/supervise/policy/rm_breaker_tests.rs	a_brace_expanded_word_moves_the_words_the_rule_reads	rm-resolver-sound	
src/supervise/policy/rm_breaker_tests.rs	a_builtin_that_changes_how_words_read_is_not_followed	rm-resolver-sound	
src/supervise/policy/rm_breaker_tests.rs	a_command_that_can_assign_what_it_names_is_no_plain_consumer	rm-shell-tables-match-shells	
src/supervise/policy/rm_breaker_tests.rs	a_conditional_or_scoped_assignment_is_unknown	rm-resolver-sound	
src/supervise/policy/rm_breaker_tests.rs	a_cwd_through_a_users_link_keeps_its_target_root	rm-on-disk	
src/supervise/policy/rm_breaker_tests.rs	a_dash_format_with_no_numeric_conversion_reads_no_arithmetic	rm-resolver-complete-fragment	
src/supervise/policy/rm_breaker_tests.rs	a_directory_the_line_just_made_is_proven_wherever_it_is	rm-resolver-complete-fragment	
src/supervise/policy/rm_breaker_tests.rs	a_globbed_word_moves_the_words_the_rule_reads	rm-resolver-sound	
src/supervise/policy/rm_breaker_tests.rs	a_mktemp_that_fails_leaves_its_variable_empty	rm-resolver-sound	
src/supervise/policy/rm_breaker_tests.rs	a_name_the_shell_maintains_is_not_the_lines	rm-resolver-sound	
src/supervise/policy/rm_breaker_tests.rs	a_named_descriptor_redirect_assigns_its_name	rm-resolver-sound	
src/supervise/policy/rm_breaker_tests.rs	a_path_word_with_an_arithmetic_operator_names_its_variables	rm-resolver-sound	
src/supervise/policy/rm_breaker_tests.rs	a_plain_commands_words_name_nothing	rm-resolver-complete-fragment	
src/supervise/policy/rm_breaker_tests.rs	a_printf_format_is_read_after_its_unicode_escapes	rm-resolver-sound	
src/supervise/policy/rm_breaker_tests.rs	a_printf_format_with_no_numeric_conversion_reads_no_arithmetic	rm-resolver-complete-fragment	
src/supervise/policy/rm_breaker_tests.rs	a_quoted_substitution_under_another_command_forgets	rm-resolver-sound	
src/supervise/policy/rm_breaker_tests.rs	a_quoted_test_operand_names_nothing	rm-resolver-complete-fragment	
src/supervise/policy/rm_breaker_tests.rs	a_quoted_word_printf_reads_as_data_names_nothing	rm-resolver-complete-fragment	
src/supervise/policy/rm_breaker_tests.rs	a_relative_operand_and_pwd_escalate	rm-resolver-sound	
src/supervise/policy/rm_breaker_tests.rs	a_root_is_judged_where_it_leads_through_roots_links_only	rm-on-disk	
src/supervise/policy/rm_breaker_tests.rs	a_subscript_on_a_special_parameter_escalates	rm-resolver-sound	
src/supervise/policy/rm_breaker_tests.rs	a_value_zsh_arithmetic_reads_is_read_too	rm-resolver-sound	
src/supervise/policy/rm_breaker_tests.rs	a_variable_another_command_names_is_unknown_after_it	rm-resolver-sound	
src/supervise/policy/rm_breaker_tests.rs	a_variable_from_the_environment_is_not_proven	rm-resolver-sound	
src/supervise/policy/rm_breaker_tests.rs	a_variable_named_only_by_an_option_letter_or_a_path_is_kept	rm-resolver-complete-fragment	
src/supervise/policy/rm_breaker_tests.rs	an_absolute_glob_moves_no_word_the_rule_reads	rm-resolver-complete-fragment	
src/supervise/policy/rm_breaker_tests.rs	an_assignment_in_a_backgrounded_list_is_not_the_shells	rm-resolver-sound	
src/supervise/policy/rm_breaker_tests.rs	an_assignment_in_front_of_a_declaring_builtin_is_unknown_after_it	rm-resolver-sound	
src/supervise/policy/rm_breaker_tests.rs	an_extended_glob_word_moves_the_words_the_rule_reads	rm-resolver-sound	
src/supervise/policy/rm_breaker_tests.rs	an_external_read_only_program_names_nothing	rm-shell-tables-match-shells	
src/supervise/policy/rm_breaker_tests.rs	an_rm_head_is_an_rm_in_any_case	rm-resolver-sound	
src/supervise/policy/rm_breaker_tests.rs	an_rm_through_a_symbolic_link_is_judged_where_it_leads	rm-on-disk	
src/supervise/policy/rm_breaker_tests.rs	an_unknown_word_forgets_only_where_it_can_name_a_variable	rm-resolver-sound	
src/supervise/policy/rm_breaker_tests.rs	every_name_whose_assignment_does_not_read_back_is_kept	rm-shell-tables-match-shells	
src/supervise/policy/rm_breaker_tests.rs	every_zsh_modifier_letter_is_not_text	rm-resolver-sound	
src/supervise/policy/rm_breaker_tests.rs	literal_chains_resolve_and_every_operand_is_judged	rm-resolver-complete-fragment	
src/supervise/policy/rm_breaker_tests.rs	scratch_root_patterns	scratch-root-patterns	
src/supervise/policy/rm_breaker_tests.rs	what_the_resolver_will_not_follow	rm-resolver-sound	
src/supervise/policy/rm_breaker_tests.rs	where_an_operand_may_point	rm-resolver-sound	
src/supervise/policy/rm_breaker_tests.rs	zsh_expansions_are_not_text	rm-resolver-sound	
src/supervise/policy/shell_startup_tests.rs	a_bash_snapshot_is_judged_against_bash_3_2s_defaults	snapshot-parse	
src/supervise/policy/shell_startup_tests.rs	a_default_zsh_snapshot_fails_no_line_and_its_aliases_fail_the_lines_naming_them	startup-admits	
src/supervise/policy/shell_startup_tests.rs	a_function_is_named_by_any_quoting_of_its_name	startup-admits	
src/supervise/policy/shell_startup_tests.rs	a_hook_or_function_that_runs_unnamed_fails_every_line	snapshot-parse	
src/supervise/policy/shell_startup_tests.rs	a_prompt_or_history_hook_is_an_ordinary_function	startup-admits	
src/supervise/policy/shell_startup_tests.rs	a_word_is_named_only_whole	names-word	
src/supervise/policy/shell_startup_tests.rs	an_alias_fails_only_a_line_that_may_run_it	startup-admits	
src/supervise/policy/shell_startup_tests.rs	an_option_the_models_do_not_assume_fails_every_line_naming_it	snapshot-parse	
src/supervise/policy/shell_startup_tests.rs	every_snapshot_of_the_tools_shell_is_judged_and_none_is_unknown	startup-read	
src/supervise/policy/shell_startup_tests.rs	what_runs_before_every_command_besides_the_snapshot_is_read	startup-read	
src/supervise/policy/turn_end_tests.rs	a_bucket_no_relaunch_moved_off_is_waited_on_when_its_countdown_ends	te-model-bucket	
src/supervise/policy/turn_end_tests.rs	a_bucket_waited_on_is_taken_up_by_the_loop_that_follows	te-model-bucket	
src/supervise/policy/turn_end_tests.rs	a_buckets_own_act_is_held_while_usage_credits_carry_the_session	credits-hold	
src/supervise/policy/turn_end_tests.rs	a_carry_ends_where_the_task_passes_to_a_person	te-person-exchange	
src/supervise/policy/turn_end_tests.rs	a_certificate_or_proxy_refusal_is_waited_out_never_asked	te-api-wall-ladder	
src/supervise/policy/turn_end_tests.rs	a_check_in_on_a_carried_task_stays_on_the_ladder	te-person-exchange	
src/supervise/policy/turn_end_tests.rs	a_choice_asked_in_prose_is_answered	te-answer-policy	
src/supervise/policy/turn_end_tests.rs	a_compaction_that_runs_long_and_fails_does_not_start_the_ladder_over	te-context-compact	
src/supervise/policy/turn_end_tests.rs	a_connection_measured_down_types_nothing_for_hours_and_resumes_once_on_its_edge_up	te-api-wall-ladder	
src/supervise/policy/turn_end_tests.rs	a_countdown_the_vendor_dropped_is_placed_as_a_cancelled_one	te-left-notice	
src/supervise/policy/turn_end_tests.rs	a_countdowns_record_in_a_resumed_history_is_no_countdown	te-left-notice	
src/supervise/policy/turn_end_tests.rs	a_credits_screen_decides_as_the_same_screen_without_its_notice	credits-hold	
src/supervise/policy/turn_end_tests.rs	a_cut_measured_down_stays_the_connections_outage_in_the_loop_that_follows	te-api-wall-ladder	
src/supervise/policy/turn_end_tests.rs	a_destructive_act_anywhere_in_a_choice_is_named	te-classify-said	
src/supervise/policy/turn_end_tests.rs	a_destructive_question_in_any_shape_is_answered_never_continued	te-answer-policy	
src/supervise/policy/turn_end_tests.rs	a_done_check_answered_with_progress_is_continued	te-done-check-finished	
src/supervise/policy/turn_end_tests.rs	a_done_report_under_land_gets_the_instruction_not_the_check	te-land-policy	
src/supervise/policy/turn_end_tests.rs	a_fable_limit_relaunches_on_opus_and_back_at_its_reset_never_by_model	te-model-bucket	
src/supervise/policy/turn_end_tests.rs	a_fact_rides_the_continuation_whole_and_never_an_answer	fact-rides-turn-end	
src/supervise/policy/turn_end_tests.rs	a_first_point_no_read_saw_busy_is_never_left_to_a_person	te-person-exchange	
src/supervise/policy/turn_end_tests.rs	a_full_context_is_compacted_then_continued_and_compacted_again_on_the_ladder	te-context-compact	
src/supervise/policy/turn_end_tests.rs	a_hot_loop_of_one_wall_rule_is_held_to_twelve_an_hour	te-wall-breaker	
src/supervise/policy/turn_end_tests.rs	a_land_acts_reply_is_landed_however_long_it_worked	te-land-policy	
src/supervise/policy/turn_end_tests.rs	a_landing_and_a_persons_own_exchange_compose	te-land-policy	
src/supervise/policy/turn_end_tests.rs	a_latch_ends_where_a_new_wall_of_the_same_class_begins	te-account-move	
src/supervise/policy/turn_end_tests.rs	a_lifted_loops_kept_carry_holds_inside_the_exchange_bound	te-person-exchange	
src/supervise/policy/turn_end_tests.rs	a_limit_the_vendor_continued_by_itself_gets_no_usage_resume	te-left-notice	
src/supervise/policy/turn_end_tests.rs	a_limit_whose_notice_left_the_screen_waits_for_the_reset_it_named	te-left-notice	
src/supervise/policy/turn_end_tests.rs	a_login_another_tab_signs_in_is_waited_on_and_delivered_again_once	login-wall-acts	
src/supervise/policy/turn_end_tests.rs	a_long_message_whose_head_scrolled_off_is_still_judged	te-classify-said	
src/supervise/policy/turn_end_tests.rs	a_loop_started_as_off_lifted_keeps_the_carry_for_the_turn_it_attached_to	te-person-exchange	
src/supervise/policy/turn_end_tests.rs	a_lost_login_after_a_move_waits_for_an_account_and_types_nothing	te-login-after-move	
src/supervise/policy/turn_end_tests.rs	a_lost_login_before_any_turn_is_asked_is_the_old_process_s	te-login-after-move	
src/supervise/policy/turn_end_tests.rs	a_lost_login_is_its_logins_own_end_only_with_no_move_behind_it	te-renewable-login	
src/supervise/policy/turn_end_tests.rs	a_lost_login_is_waited_at_only_where_a_move_could_be_made	te-login-after-move	
src/supervise/policy/turn_end_tests.rs	a_lost_login_no_point_decided_is_decided_as_the_wall_it_left	te-login-wall	
src/supervise/policy/turn_end_tests.rs	a_lost_login_over_a_done_task_is_signed_in	te-login-wall	
src/supervise/policy/turn_end_tests.rs	a_lost_login_right_after_a_move_is_answered_by_the_next_account	te-login-after-move	
src/supervise/policy/turn_end_tests.rs	a_lost_login_whose_move_could_not_be_made_is_signed_in_where_it_stands	te-login-after-move	
src/supervise/policy/turn_end_tests.rs	a_lost_login_whose_row_left_is_told_where_it_is_asked_about	te-login-after-move	
src/supervise/policy/turn_end_tests.rs	a_lost_login_with_no_account_to_wait_for_is_signed_in_where_it_stands	te-login-after-move	
src/supervise/policy/turn_end_tests.rs	a_lost_logins_stamps_go_back_with_a_sleep	te-login-wall	
src/supervise/policy/turn_end_tests.rs	a_memory_wall_restarts_the_agent_and_never_types	te-memory-wall	
src/supervise/policy/turn_end_tests.rs	a_model_bucket_is_accepted_under_consent_never_under_a_box_and_waits_out_a_second	te-model-bucket	
src/supervise/policy/turn_end_tests.rs	a_model_bucket_moves_account_first_and_the_fallback_is_its_otherwise	te-account-move	
src/supervise/policy/turn_end_tests.rs	a_model_choice_only_leaves_a_bucket_for_a_known_different_family	te-model-bucket	
src/supervise/policy/turn_end_tests.rs	a_models_usage_credits_relaunch_onto_the_fallback_or_hold_until_the_process_changes	credits-hold	
src/supervise/policy/turn_end_tests.rs	a_move_a_later_loop_takes_up_owes_its_resume_only_at_the_notice_it_left	te-move-answer-and-resume	
src/supervise/policy/turn_end_tests.rs	a_move_ahead_that_was_refused_is_asked_again_an_hour_on	te-account-move	
src/supervise/policy/turn_end_tests.rs	a_move_made_at_a_wall_owes_the_walls_resume	te-move-answer-and-resume	
src/supervise/policy/turn_end_tests.rs	a_move_not_made_gets_the_walls_breaker_and_no_act_past_it	te-wall-breaker	
src/supervise/policy/turn_end_tests.rs	a_move_over_an_unanswered_move_owes_what_that_move_owed	te-move-answer-and-resume	
src/supervise/policy/turn_end_tests.rs	a_move_that_could_not_be_made_is_not_asked_again_while_the_wall_stands	te-account-move	
src/supervise/policy/turn_end_tests.rs	a_persons_declined_question_is_held_for_the_grace	te-person-grace	
src/supervise/policy/turn_end_tests.rs	a_persons_exchange_on_a_wall_gets_its_retry_and_stays_theirs	te-person-exchange	
src/supervise/policy/turn_end_tests.rs	a_persons_exchange_on_a_wall_keeps_its_work_across_an_off_park	te-person-exchange	
src/supervise/policy/turn_end_tests.rs	a_persons_exchange_retried_on_a_wall_and_unseen_after_an_off_park_is_never_left	te-person-exchange	
src/supervise/policy/turn_end_tests.rs	a_persons_interrupt_holds_the_turn_for_the_grace	te-person-grace	
src/supervise/policy/turn_end_tests.rs	a_persons_keystroke_holds_every_act_for_the_grace	te-person-grace	
src/supervise/policy/turn_end_tests.rs	a_persons_own_exchange_is_left_to_them_whatever_it_says	te-person-exchange	
src/supervise/policy/turn_end_tests.rs	a_persons_own_exchange_is_theirs_under_every_autopilot	te-person-exchange	
src/supervise/policy/turn_end_tests.rs	a_persons_own_turn_of_work_at_an_outage_starts_its_ladder_anew	te-api-wall-ladder	
src/supervise/policy/turn_end_tests.rs	a_plain_turn_end_moves_ahead_of_the_wall_around_its_own_act	te-account-move	
src/supervise/policy/turn_end_tests.rs	a_point_the_upgrade_owns_is_never_continued_at_the_grace_lapse	te-upgrade-ownership	
src/supervise/policy/turn_end_tests.rs	a_prompt_not_answered_yet_leaves_a_left_limits_wait_standing	te-left-notice	
src/supervise/policy/turn_end_tests.rs	a_question_in_prose_is_answered_by_the_priority_word	te-answer-policy	
src/supervise/policy/turn_end_tests.rs	a_question_under_land_gets_the_land_answer	te-land-policy	
src/supervise/policy/turn_end_tests.rs	a_refused_request_an_overlong_reply_and_a_one_off_cut_keep_their_acts	te-api-wall-ladder	
src/supervise/policy/turn_end_tests.rs	a_relaunch_where_the_buckets_notice_left_knows_the_bucket	te-model-bucket	
src/supervise/policy/turn_end_tests.rs	a_relaunchs_start_up_is_no_turn_asked_under_the_new_login	te-move-answer-and-resume	
src/supervise/policy/turn_end_tests.rs	a_remembered_tail_reads_as_a_fresh_read_of_it	te-last-tail-memo	
src/supervise/policy/turn_end_tests.rs	a_renewed_login_is_followed_and_a_renewable_one_signed_in	te-renewable-login	
src/supervise/policy/turn_end_tests.rs	a_renewable_logins_end_waits_on_its_rows_own_sign_in	te-renewable-login	
src/supervise/policy/turn_end_tests.rs	a_report_or_a_question_only_the_person_can_answer_is_no_choice	te-classify-said	
src/supervise/policy/turn_end_tests.rs	a_restarted_loop_keeps_the_usage_act_it_already_made	te-usage-limit-wait	
src/supervise/policy/turn_end_tests.rs	a_resume_advised_for_the_resets_own_clearing_types_nothing_more	te-limit-reset-continuation	
src/supervise/policy/turn_end_tests.rs	a_resumed_conversations_older_vendor_continuation_ends_no_wait	te-left-notice	
src/supervise/policy/turn_end_tests.rs	a_retry_that_led_to_real_work_ends_the_episode	te-api-wall-ladder	
src/supervise/policy/turn_end_tests.rs	a_roster_rows_sign_in_menu_waits_on_its_own_sign_in	te-renewable-login	
src/supervise/policy/turn_end_tests.rs	a_seeded_resume_is_owed_while_the_notice_it_left_stands_unserved	te-move-answer-and-resume	
src/supervise/policy/turn_end_tests.rs	a_session_held_at_its_wall_waits_for_whatever_comes_first	te-account-move	
src/supervise/policy/turn_end_tests.rs	a_session_makes_at_most_four_account_moves_an_hour	te-account-move	
src/supervise/policy/turn_end_tests.rs	a_sign_in_menu_after_a_move_is_the_moved_to_logins_own	te-login-after-move	login-wall-acts
src/supervise/policy/turn_end_tests.rs	a_sleep_is_time_on_the_policys_pauses_and_resets_and_not_on_the_workers_clock	te-sleep-adjust	
src/supervise/policy/turn_end_tests.rs	a_sleep_never_reopens_the_hot_loop_breakers	te-sleep-adjust	
src/supervise/policy/turn_end_tests.rs	a_sonnet_bucket_under_the_generic_banner_relaunches_on_the_fallback	te-model-bucket	
src/supervise/policy/turn_end_tests.rs	a_stop_that_names_a_destructive_act_is_answered_reversibly	te-answer-policy	
src/supervise/policy/turn_end_tests.rs	a_superseded_login_follows_the_renewed_one_at_an_idle_turn_end	te-renewable-login	
src/supervise/policy/turn_end_tests.rs	a_superseded_logins_held_follow_is_waited_for_and_asks_nothing	te-renewable-login	
src/supervise/policy/turn_end_tests.rs	a_sustained_overload_is_tried_blind_on_its_doubling_ladder_and_never_held	te-api-wall-ladder	
src/supervise/policy/turn_end_tests.rs	a_taskless_tabs_roster_login_end_is_still_told	te-renewable-login	
src/supervise/policy/turn_end_tests.rs	a_told_move_is_held_by_a_later_loop_whatever_its_age	te-move-answer-and-resume	
src/supervise/policy/turn_end_tests.rs	a_told_move_is_told_served_where_the_worker_works_past_it	te-move-answer-and-resume	
src/supervise/policy/turn_end_tests.rs	a_told_moves_first_point_after_a_relaunch_is_its_start_up	te-move-answer-and-resume	
src/supervise/policy/turn_end_tests.rs	a_turn_of_someone_elses_keeps_the_land_episode	te-land-policy	
src/supervise/policy/turn_end_tests.rs	a_turn_read_running_before_the_loop_is_a_persons_only_by_a_key_inside_it	te-person-exchange	
src/supervise/policy/turn_end_tests.rs	a_turn_that_ran_since_the_move_pays_the_resume_whatever_it_ended_on	te-move-answer-and-resume	
src/supervise/policy/turn_end_tests.rs	a_typed_act_or_work_seen_since_pays_the_resets_continuation	te-limit-reset-continuation	
src/supervise/policy/turn_end_tests.rs	a_usage_wall_with_an_account_to_move_to_is_a_relaunch_under_it	te-account-move	
src/supervise/policy/turn_end_tests.rs	a_waited_lost_login_whose_row_leaves_is_still_decided_as_that_wall	te-login-after-move	
src/supervise/policy/turn_end_tests.rs	a_wall_inside_a_land_act_or_the_done_check_keeps_the_act_it_cut	te-land-policy	
src/supervise/policy/turn_end_tests.rs	a_wall_is_its_own_rules_even_where_the_upgrade_owns_the_turn_ends	te-upgrade-ownership	
src/supervise/policy/turn_end_tests.rs	a_wall_no_reset_ends_is_retried_on_the_entitlement_ladder	te-usage-limit-wait	
src/supervise/policy/turn_end_tests.rs	a_wall_only_usage_credits_lift_is_not_continued_under_the_key_off	usage-wall-wait	credits-hold
src/supervise/policy/turn_end_tests.rs	a_wall_past_its_reset_is_probed_before_the_account_is_moved	te-account-move	
src/supervise/policy/turn_end_tests.rs	a_wall_read_before_its_point_is_tracked_and_the_act_judged_as_before	te-left-notice	
src/supervise/policy/turn_end_tests.rs	a_wall_split_job_is_landed_and_a_split_exchange_is_left_under_land	te-land-policy	
src/supervise/policy/turn_end_tests.rs	a_walls_act_never_types_the_screens_words	te-wall-text-untrusted	
src/supervise/policy/turn_end_tests.rs	advice_to_resume_types_the_walls_continuation_once_a_clearing	te-account-move	
src/supervise/policy/turn_end_tests.rs	advice_to_wait_cuts_the_walls_wait_short_and_never_lengthens_it	te-account-move	
src/supervise/policy/turn_end_tests.rs	after_an_off_park_a_persons_turn_is_judged_by_the_exchange_bound	te-person-exchange	
src/supervise/policy/turn_end_tests.rs	an_account_move_is_asked_ahead_of_the_models_return	te-account-move	
src/supervise/policy/turn_end_tests.rs	an_api_error_is_tried_blind_on_the_outage_ladder_and_retries_off_escalate	te-api-wall-ladder	
src/supervise/policy/turn_end_tests.rs	an_irreversible_decision_never_gets_the_owners_answer_text	te-answer-policy	
src/supervise/policy/turn_end_tests.rs	an_off_park_ends_the_carry_so_a_persons_greeting_after_it_is_theirs	te-person-exchange	
src/supervise/policy/turn_end_tests.rs	an_outage_under_land_resumes_with_lands_instruction_until_it_was_given	te-api-wall-ladder	
src/supervise/policy/turn_end_tests.rs	at_a_limit_whose_notice_left_the_screen_the_roster_is_asked_as_at_the_wall	te-account-move	
src/supervise/policy/turn_end_tests.rs	aterms_own_words_left_in_the_prompt_are_never_sent_as_a_draft	te-draft-handling	
src/supervise/policy/turn_end_tests.rs	autopilot_off_decides_nothing_where_cruise_acts	te-autopilot-off	
src/supervise/policy/turn_end_tests.rs	continue_off_limits_the_land_instruction_but_not_its_answer	te-land-policy	
src/supervise/policy/turn_end_tests.rs	destructive_words_match_their_inflections	te-destructive-word	
src/supervise/policy/turn_end_tests.rs	each_end_of_a_limit_reset_owes_one_continuation	te-limit-reset-continuation	
src/supervise/policy/turn_end_tests.rs	a_retired_token_line_is_a_done_claim	te-done-check-finished	
src/supervise/policy/turn_end_tests.rs	lands_instruction_typed_as_an_outages_resume_begins_the_hold_at_its_row	te-land-policy	
src/supervise/policy/turn_end_tests.rs	lands_instruction_waits_no_back_off_and_the_rest_wait_theirs	te-land-policy	te-short-streak-backoff
src/supervise/policy/turn_end_tests.rs	leaving_land_for_an_off_park_types_the_instruction_again	te-land-policy	
src/supervise/policy/turn_end_tests.rs	no_account_is_asked_about_while_the_resets_continuation_is_owed	te-limit-reset-continuation	
src/supervise/policy/turn_end_tests.rs	nothing_is_typed_under_a_box_a_survey_a_draft_or_a_non_agent_screen	te-continue-at-idle	
src/supervise/policy/turn_end_tests.rs	only_an_api_walls_wait_is_the_next_try	te-retry-wait-flag	
src/supervise/policy/turn_end_tests.rs	only_the_check_itself_can_end_the_task	te-done-check-finished	
src/supervise/policy/turn_end_tests.rs	open_work_reads_heads_and_status_words_and_never_a_negation	te-done-check-finished	
src/supervise/policy/turn_end_tests.rs	open_work_reads_three_rounds_of_real_reports_as_labelled	te-done-check-finished	
src/supervise/policy/turn_end_tests.rs	open_work_reads_work_left_mid_sentence_and_a_blocker_named_after_it	te-done-check-finished	
src/supervise/policy/turn_end_tests.rs	the_accounts_news_ends_a_latch_at_a_wall_and_ahead_of_one	te-account-move	
src/supervise/policy/turn_end_tests.rs	the_allow_list_takes_close_variants_and_nothing_else	te-suggestion-allowed	
src/supervise/policy/turn_end_tests.rs	the_continuation_a_walls_act_owes_is_paid_where_the_upgrade_owns_the_point	te-upgrade-ownership	
src/supervise/policy/turn_end_tests.rs	the_decider_reads_a_probe_as_its_otherwise_at_every_account_fixture	te-probe-transparent	
src/supervise/policy/turn_end_tests.rs	the_dialog_aterm_opened_over_a_roster_row_is_backed_out_of	login-wall-acts	login-notices,login-lifecycle-accounts
src/supervise/policy/turn_end_tests.rs	a_dialog_still_drawn_after_aterms_esc_gets_the_esc_again	login-wall-acts	login-notices
src/supervise/policy/turn_end_tests.rs	a_persons_login_after_aterms_back_out_is_left_alone	login-wall-acts	login-notices
src/supervise/policy/turn_end_tests.rs	a_dialog_whose_login_came_back_elsewhere_is_backed_out_of	login-wall-acts	login-notices
src/supervise/policy/turn_end_tests.rs	the_done_check_is_a_continuation_the_owner_can_limit	te-done-check-finished	
src/supervise/policy/turn_end_tests.rs	the_exchange_a_lost_login_swallowed_is_the_persons_through_its_sign_in	te-login-wall	
src/supervise/policy/turn_end_tests.rs	the_first_turn_after_a_move_says_whether_the_account_serves	te-move-answer-and-resume	
src/supervise/policy/turn_end_tests.rs	the_host_is_asked_only_where_its_advice_could_decide	te-account-point	
src/supervise/policy/turn_end_tests.rs	the_limit_a_cancel_left_answers_a_persons_turn_asked_since_the_last_look	te-left-notice	
src/supervise/policy/turn_end_tests.rs	the_limit_a_cancel_left_answers_the_act_it_was_not_typed_over	te-left-notice	
src/supervise/policy/turn_end_tests.rs	the_login_dialog_is_carried_once_and_never_relooped	te-login-wall	
src/supervise/policy/turn_end_tests.rs	the_probes_gate_is_true_at_a_plain_idle_point_and_false_with_any_condition_flipped	te-probe-gate	
src/supervise/policy/turn_end_tests.rs	the_reset_jitter_is_added_to_the_grace	te-usage-limit-wait	
src/supervise/policy/turn_end_tests.rs	the_resets_continuation_keeps_resume_limits_and_a_persons_grace	te-limit-reset-continuation	
src/supervise/policy/turn_end_tests.rs	the_rules_ride_in_the_continuation	te-continue-at-idle	
src/supervise/policy/turn_end_tests.rs	a_sign_in_left_stands_while_the_row_is_off_the_screen	te-login-after-move	
src/supervise/policy/turn_end_tests.rs	the_vendor_firing_its_countdown_is_waited_for_never_doubled	te-usage-limit-wait	
src/supervise/policy/turn_end_tests.rs	the_vendors_continuation_of_a_bucket_ends_its_wait_under_its_own_notice	te-model-bucket	
src/supervise/policy/turn_end_tests.rs	the_workers_suggestion_is_accepted_once	te-continue-at-idle	
src/supervise/policy/turn_end_tests.rs	usage_credits_hold_the_policys_own_acts_until_the_window_resets	credits-hold	
src/supervise/policy/turn_end_tests.rs	usage_credits_move_to_the_advised_account_first	credits-hold	
src/supervise/policy/turn_end_tests.rs	usage_credits_with_a_reset_ahead_hold_where_the_notice_is_gone	credits-hold	
src/supervise/policy/turn_end_tests.rs	usage_credits_with_no_reset_tell_the_owner_once_and_probe_at_the_bound	credits-hold	
src/supervise/policy/turn_end_tests.rs	whats_next_after_finished_work_is_a_done_report	te-done-check-finished	
src/supervise/policy/turn_end_tests.rs	with_resume_limits_off_a_limit_whose_notice_left_gets_nothing	te-left-notice	
src/supervise/policy/turn_end_tests.rs	wrapped_stops_sign_off_asks_and_destructive_offers_are_not_continued	te-classify-said	
src/supervise/report_tests.rs	a_cut_page_and_the_main_screen_are_incomplete	report-finish-assess	
src/supervise/report_tests.rs	a_final_view_of_a_headless_message_keeps_its_words_and_its_done_row	blocks-views	report-render
src/supervise/report_tests.rs	a_gap_after_the_mark_counts_only_after_the_start	report-finish-assess	
src/supervise/report_tests.rs	a_long_paste_opens_at_the_last_pasted_row	report-find-start	
src/supervise/report_tests.rs	a_lost_or_gone_session_is_an_error	report-gather-protocol	
src/supervise/report_tests.rs	a_message_that_starts_with_a_number_is_found	report-parse-history	
src/supervise/report_tests.rs	a_numbered_draft_in_the_composer_is_not_the_start	transcript-end-positional	
src/supervise/report_tests.rs	a_paste_row_that_does_not_fit_the_turn_is_not_the_start	report-find-start	
src/supervise/report_tests.rs	a_queued_message_under_the_status_row_is_not_a_marker	report-find-start	
src/supervise/report_tests.rs	a_turn_that_scrolled_off_is_joined_back_from_its_user_row	report-join	
src/supervise/report_tests.rs	a_wrapped_user_block_on_the_screen_is_the_start	transcript-end-positional	
src/supervise/report_tests.rs	an_archive_that_is_off_is_no_archive	report-parse-offscreen	
src/supervise/report_tests.rs	an_old_host_reports_from_the_screen_alone	report-parse-offscreen	
src/supervise/report_tests.rs	another_origin_is_an_archive_reset	report-finish-assess	
src/supervise/report_tests.rs	indented_code_above_an_idle_composer_survives	transcript-end-positional	
src/supervise/report_tests.rs	since_counts_every_row_after_it	report-join	
src/supervise/report_tests.rs	the_done_row_that_ends_a_turn_is_kept	transcript-end-positional	
src/supervise/report_tests.rs	the_join_skips_the_reshown_rows_under_a_pinned_header	report-join	
src/supervise/report_tests.rs	the_paste_fallback_never_claims_an_older_turns_paste	report-find-start	
src/supervise/report_tests.rs	the_readers_match_the_wire	report-mark-roundtrip	
src/supervise/report_tests.rs	the_transcript_ends_at_the_live_zone	transcript-end-positional	
src/supervise/report_tests.rs	the_views_keep_the_worker_words_and_drop_the_tool_rows	blocks-views	report-render
src/supervise/reset_loop_tests.rs	a_claim_that_is_no_longer_the_sessions_keeps_the_reset	reset-host-dialogue	
src/supervise/reset_loop_tests.rs	a_confirmation_of_another_kind_than_the_row_is_kept	reset-keys-safety	
src/supervise/reset_loop_tests.rs	a_confirmation_with_the_focus_on_no_is_escaped_never_arrowed	reset-keys-safety	
src/supervise/reset_loop_tests.rs	a_deadline_is_placed_in_the_wall_notices_zone_where_the_loop_holds_one	reset-place	
src/supervise/reset_loop_tests.rs	a_gate_of_the_loops_shut_passes_the_reset_over_before_the_host_is_asked	reset-gates-equivalence	
src/supervise/reset_loop_tests.rs	a_key_off_successor_escapes_a_reworded_consent_and_never_takes_its_yes	reset-successor-escape-only	
src/supervise/reset_loop_tests.rs	a_kind_drawn_beside_the_row_as_the_arrow_lands_is_never_entered	reset-keys-safety	
src/supervise/reset_loop_tests.rs	a_key_off_successor_escapes_checking_past_its_bound_and_ends_untold	reset-successor-escape-only	
src/supervise/reset_loop_tests.rs	a_key_off_successor_waits_out_resetting_and_owes_nothing	reset-successor-escape-only	
src/supervise/reset_loop_tests.rs	a_key_off_successors_escape_that_does_not_take_is_tried_again_and_recorded_once	reset-successor-escape-only	
src/supervise/reset_loop_tests.rs	a_pending_line_is_never_keyed_but_checking_past_its_bound	reset-keys-safety	
src/supervise/reset_loop_tests.rs	a_person_holds_every_key_of_the_reset	reset-keys-safety	
src/supervise/reset_loop_tests.rs	a_person_keying_before_the_rows_first_key_holds_the_row	reset-keys-safety	
src/supervise/reset_loop_tests.rs	a_row_older_than_a_step_can_be_is_escaped_and_ended_untold	reset-successor-escape-only	
src/supervise/reset_loop_tests.rs	a_row_press_with_no_answer_is_closed_and_never_entered	reset-books	
src/supervise/reset_loop_tests.rs	a_stop_in_any_wait_of_the_step_ends_it_with_nothing_written_after	stop-semantics	
src/supervise/reset_loop_tests.rs	a_successor_escapes_a_box_with_nothing_of_the_confirmation_left	reset-successor-escape-only	
src/supervise/reset_loop_tests.rs	a_successor_escapes_a_titled_confirmation_with_no_box_until_its_enter_is_written	reset-successor-escape-only	
src/supervise/reset_loop_tests.rs	a_successor_escapes_the_confirmation_and_never_enters_it	reset-successor-escape-only	
src/supervise/reset_loop_tests.rs	a_walled_or_countless_spend_is_said_and_booked_as_used	reset-books	
src/supervise/reset_loop_tests.rs	an_attended_loop_takes_no_step_up	reset-successor-escape-only	
src/supervise/reset_loop_tests.rs	an_enter_whose_answer_never_came_is_on_the_books	reset-books	
src/supervise/reset_loop_tests.rs	no_enter_follows_an_escape_whatever_the_confirmation_then_reads	reset-keys-safety	
src/supervise/reset_loop_tests.rs	the_box_the_row_opened_is_the_steps_whatever_it_is_reworded_to	reset-keys-safety	
src/supervise/reset_loop_tests.rs	the_early_use_confirmation_is_escaped_and_one_continuation_is_owed	reset-books	
src/supervise/reset_loop_tests.rs	the_enter_is_judged_on_the_wall_as_the_host_reads_it_then	reset-keys-safety	
src/supervise/reset_loop_tests.rs	the_step_never_escapes_an_answer_kept_for_good_by_its_label	reset-keys-safety	
src/supervise/reset_loop_tests.rs	the_step_never_escapes_an_mcp_server_dialog	reset-keys-safety	
src/supervise/reset_loop_tests.rs	the_whole_path_is_three_keys_and_the_hosts_no_is_todays_wait_row	reset-books	
src/supervise/run_box_key_tests.rs	a_box_asked_again_after_work_waits_the_settle_again	box-settle	
src/supervise/run_box_key_tests.rs	a_box_drawn_as_one_row_has_no_anchor	box-ask-refines-model	
src/supervise/run_box_key_tests.rs	a_redrawn_box_under_the_settle_is_asked_once_after_the_settle	box-settle	
src/supervise/run_box_key_tests.rs	a_retried_long_command_is_asked_again	box-ask-refines-model	
src/supervise/run_box_key_tests.rs	the_box_key_changes_for_another_box	box-key-normal-form	
src/supervise/run_box_key_tests.rs	the_box_key_holds_across_a_redraw_a_rewrap_a_reflow_a_focus_move_and_a_ticking_countdown	box-key-normal-form	
src/supervise/run_box_key_tests.rs	the_box_key_is_the_readers	box-ask-refines-model	
src/supervise/run_box_key_tests.rs	tier1_the_box_is_asked_once_per_request	box-ask-refines-model	
src/supervise/run_engine_tests.rs	a_bare_clock_time_is_placed_on_the_day_the_transcript_names	limit-notice-reset-placement	
src/supervise/run_engine_tests.rs	unknown_reset_allowances_never_become_a_generic_question_answer	approval-never-spends	question-answer-refines-model
src/supervise/run_engine_tests.rs	a_bash_box_is_judged_under_the_workers_shell_startup	worker-env-binding	
src/supervise/run_engine_tests.rs	a_box_is_pressed_only_once_it_has_shown_the_settle	box-settle	
src/supervise/run_engine_tests.rs	a_box_on_a_shell_is_never_pressed	reader-by-program	
src/supervise/run_engine_tests.rs	a_box_swapped_between_the_read_and_the_press_is_not_pressed	press-fence-and-guard	
src/supervise/run_engine_tests.rs	a_box_taller_than_the_screen_is_declined_in_the_loop_and_nobody_is_asked	decline-keys-refines-model	
src/supervise/run_engine_tests.rs	a_break_after_the_loops_own_taken_act_is_offered_to_the_host	idle-point-classification	
src/supervise/run_engine_tests.rs	a_check_the_supervisor_did_not_make_hands_the_multiselect_over	question-answer-refines-model	
src/supervise/run_engine_tests.rs	a_choice_is_told_at_the_first_busy_read_and_before_a_stop	told-choice	
src/supervise/run_engine_tests.rs	a_controllers_answer_moves_the_focus_and_presses_enter_on_its_choice	controller-answer	
src/supervise/run_engine_tests.rs	a_controllers_answer_to_a_muse_question_moves_the_focus_and_presses_enter	controller-answer
src/supervise/run_engine_tests.rs	a_controllers_answer_to_a_muse_question_outlasts_its_ticking_head	controller-answer
src/supervise/run_engine_tests.rs	a_controllers_recommended_on_a_multi_select_answers_the_whole_tab	controller-answer	
src/supervise/run_engine_tests.rs	a_decline_goes_no_further_once_a_readable_box_with_the_same_note_stands_there	decline-keys-refines-model	
src/supervise/run_engine_tests.rs	a_decline_is_one_fenced_guarded_keystroke_per_fresh_read	decline-keys-refines-model	
src/supervise/run_engine_tests.rs	a_decline_keystroke_on_a_stale_screen_is_refused_and_nothing_follows_it	decline-keys-refines-model	
src/supervise/run_engine_tests.rs	a_decline_keystroke_the_box_never_draws_is_handed_over_and_never_resent	decline-keys-refines-model	
src/supervise/run_engine_tests.rs	a_decline_keystroke_whose_answer_never_came_is_never_written_again	decline-keys-refines-model	
src/supervise/run_engine_tests.rs	a_decline_the_screen_does_not_confirm_is_never_entered	decline-keys-refines-model	
src/supervise/run_engine_tests.rs	a_deferred_fallback_is_retried_at_the_reset_the_grace_and_the_sessions_share	model-bucket-and-fallback-timing	
src/supervise/run_engine_tests.rs	a_dialog_a_person_keyed_after_the_loop_is_not_told_as_its_choice	told-choice	
src/supervise/run_engine_tests.rs	a_dialog_held_by_the_policy_is_answered_when_handed_back	question-priority-resolution	
src/supervise/run_engine_tests.rs	a_dialog_the_policy_held_to_its_end_is_not_told_as_its_choice	told-choice	
src/supervise/run_engine_tests.rs	a_fallback_the_host_keeps_deferring_gives_way_to_the_continuation_at_the_reset	model-bucket-and-fallback-timing	
src/supervise/run_engine_tests.rs	a_fenced_press_on_a_moved_screen_is_decided_again_from_a_fresh_read	press-fence-and-guard	
src/supervise/run_engine_tests.rs	a_file_box_the_agents_own_ask_rule_sends_is_held_in_the_loop	loop-applies-decision	
src/supervise/run_engine_tests.rs	a_four_tab_question_is_answered_tab_by_tab_and_submitted	question-answer-refines-model	
src/supervise/run_engine_tests.rs	a_git_box_is_judged_in_the_workers_own_environment	worker-env-binding	
src/supervise/run_engine_tests.rs	a_git_box_reads_the_transcripts_under_the_workers_claude_config_dir	worker-env-binding	
src/supervise/run_engine_tests.rs	a_halted_survey_dismissal_parks_and_tries_again	hold-and-refusal-parking	
src/supervise/run_engine_tests.rs	a_host_without_the_claim_is_supervised_as_before	claim-refines-model	
src/supervise/run_engine_tests.rs	a_hosted_loop_behind_another_claim_ends_and_presses_nothing	claim-refines-model	
src/supervise/run_engine_tests.rs	a_hosted_loop_ledgers_where_its_host_says_and_nowhere_else	ledger-one-row-per-decision	
src/supervise/run_engine_tests.rs	a_hosts_same_wait_said_again_at_one_point_is_journaled_once	host-turn-end-protocol	
src/supervise/run_engine_tests.rs	a_launch_answers_no_box_but_its_trust_dialog	never-type-over-a-person	
src/supervise/run_engine_tests.rs	a_launch_answers_one_dialog_under_autopilot_and_yields_to_the_person	never-type-over-a-person	
src/supervise/run_engine_tests.rs	a_launch_missing_any_premise_keeps_the_persons_grace	never-type-over-a-person	
src/supervise/run_engine_tests.rs	a_launch_the_server_witnesses_is_answered_within_the_grace	never-type-over-a-person	
src/supervise/run_engine_tests.rs	a_launch_answers_the_trust_dialog_and_hands_the_mcp_dialog_to_a_person	approval-dialog-rules	approval-trust-dialog
src/supervise/run_engine_tests.rs	a_limit_episode_is_told_to_the_host_and_waited_out_over_its_turn_ends	host-turn-end-protocol	
src/supervise/run_engine_tests.rs	a_limit_notice_left_past_its_reset_and_its_grace_is_an_idle_point	limit-notice-reset-placement	
src/supervise/run_engine_tests.rs	a_hold_on_the_autocompact_offer_parks_and_sends_nothing_more	autocompact-offer-loop	
src/supervise/run_engine_tests.rs	the_autocompact_offer_is_answered_at_400k_without_the_owner	autocompact-offer-loop	
src/supervise/run_engine_tests.rs	the_autocompact_offer_steps_to_400k_before_its_enter_and_off_is_the_owners	autocompact-offer-loop	
src/supervise/run_engine_tests.rs	the_autocompact_offer_waits_its_own_settle_before_any_key	autocompact-offer-loop	
src/supervise/run_engine_tests.rs	the_auto_mode_setup_offer_is_put_off_at_every_level_and_the_turn_continued	auto-mode-setup-loop	
src/supervise/run_engine_tests.rs	the_auto_mode_setup_offer_waits_out_a_persons_typing	auto-mode-setup-loop	
src/supervise/run_engine_tests.rs	a_look_alike_of_the_auto_mode_setup_offer_is_not_pressed	auto-mode-setup-loop	
src/supervise/run_engine_tests.rs	the_auto_mode_setup_offer_waits_its_own_settle_before_any_key	auto-mode-setup-loop	
src/supervise/run_engine_tests.rs	a_live_limit_naming_no_reset_is_never_spent_by_an_earlier_notices_reset	limit-notice-reset-placement	
src/supervise/run_engine_tests.rs	a_loop_addressing_its_session_binds_the_worker_by_that_id	session-group	
src/supervise/run_engine_tests.rs	a_measure_acted_on_restarts_an_agent_idle_behind_a_wall	memory-ceiling-decision	
src/supervise/run_engine_tests.rs	a_measure_past_the_ceiling_warns_and_restarts_only_when_the_mac_is_in_trouble	memory-ceiling-decision	
src/supervise/run_engine_tests.rs	a_memory_banner_is_the_hosts_restart_and_escalated_only_when_it_cannot_be	memory-ceiling-decision	
src/supervise/run_engine_tests.rs	a_memory_wall_no_longer_acted_on_hands_the_limit_back	memory-ceiling-decision	
src/supervise/run_engine_tests.rs	a_model_bucket_is_relaunched_on_the_fallback_and_back_never_by_model	model-bucket-and-fallback-timing	
src/supervise/run_engine_tests.rs	a_model_buckets_relaunch_keeps_its_way_back_when_the_transcript_moves_on	model-bucket-and-fallback-timing	
src/supervise/run_engine_tests.rs	a_model_buckets_relaunch_not_made_keeps_the_decisions_verdict	model-bucket-and-fallback-timing	
src/supervise/run_engine_tests.rs	a_model_fallback_the_host_keeps_deferring_is_asked_again_and_nothing_is_typed_blind	model-bucket-and-fallback-timing	
src/supervise/run_engine_tests.rs	a_person_keying_the_session_gets_the_decline_s_box	decline-keys-refines-model	
src/supervise/run_engine_tests.rs	a_person_the_server_sees_at_the_write_gets_the_question_first	question-answer-refines-model	
src/supervise/run_engine_tests.rs	a_person_typing_in_the_free_text_row_gets_the_question	question-answer-refines-model	
src/supervise/run_engine_tests.rs	a_person_who_keys_before_the_survey_press_skips_it_and_costs_no_try	press-fence-and-guard	
src/supervise/run_engine_tests.rs	a_person_who_keys_during_the_first_key_wait_gets_the_dialog_first	question-answer-refines-model	
src/supervise/run_engine_tests.rs	a_persons_key_between_the_read_and_the_write_skips_the_question_key	question-answer-refines-model	
src/supervise/run_engine_tests.rs	a_plain_idle_point_the_memory_escalated_is_handed_back_when_the_mac_recovers	memory-ceiling-decision	
src/supervise/run_engine_tests.rs	a_point_read_before_the_agent_drew_itself_is_waited_on_by_its_content	wake-protocol	
src/supervise/run_engine_tests.rs	a_press_that_did_not_land_is_tried_again_at_full_power	press-landing-and-retry	
src/supervise/run_engine_tests.rs	a_question_asked_a_third_time_is_answered_again_after_a_pause	question-answer-refines-model	
src/supervise/run_engine_tests.rs	a_question_drawn_over_a_draft_is_answered_and_the_draft_left_alone	question-answer-refines-model	
src/supervise/run_engine_tests.rs	a_question_key_is_sent_again_only_after_the_screen_held_still_and_counted_when_written	question-answer-refines-model	
src/supervise/run_engine_tests.rs	a_question_key_refused_busy_input_unread_is_backed_off_then_answered	hold-and-refusal-parking	
src/supervise/run_engine_tests.rs	a_question_on_a_host_without_the_generation_fence_is_handed_over	question-answer-refines-model	
src/supervise/run_engine_tests.rs	a_rating_card_above_the_tail_is_dismissed	survey-press-refines-model	
src/supervise/run_engine_tests.rs	a_read_box_under_a_ticking_screen_is_approved_within_one_wake	wake-protocol	
src/supervise/run_engine_tests.rs	a_refused_memory_restart_keeps_the_persons_row_until_the_host_is_asked_again	memory-ceiling-decision	
src/supervise/run_engine_tests.rs	a_refused_restart_at_the_memory_banner_keeps_the_persons_row_when_the_measure_comes_back	memory-ceiling-decision	
src/supervise/run_engine_tests.rs	a_restart_that_typed_its_carry_on_typed_a_harness_turn	host-turn-end-protocol	
src/supervise/run_engine_tests.rs	a_restarted_loop_at_a_wall_its_predecessor_continued_past_waits_out_the_back_off	model-bucket-and-fallback-timing	
src/supervise/run_engine_tests.rs	a_retired_key_takes_its_safe_rule_away_in_the_loop	loop-applies-decision	
src/supervise/run_engine_tests.rs	a_return_to_the_bucket_model_the_host_defers_never_holds_the_continuation	model-bucket-and-fallback-timing	
src/supervise/run_engine_tests.rs	a_session_its_host_says_nobody_asked_anything_gets_nothing_typed	host-turn-end-protocol	
src/supervise/run_engine_tests.rs	a_session_naming_no_mode_gets_the_verdict_wait_back_at_its_first_busy_read	loop-applies-decision	
src/supervise/run_engine_tests.rs	a_stale_attention_is_reconciled_at_the_start	attention-ownership	
src/supervise/run_engine_tests.rs	a_stop_between_the_look_and_the_press_writes_nothing	stop-semantics	
src/supervise/run_engine_tests.rs	a_stop_in_any_wait_ends_the_hosted_run_before_another_begins	stop-semantics	
src/supervise/run_engine_tests.rs	a_survey_that_left_under_typed_text_is_not_said_dismissed	survey-press-refines-model	
src/supervise/run_engine_tests.rs	a_taken_box_redrawn_before_it_leaves_is_pressed_once	press-landing-and-retry	
src/supervise/run_engine_tests.rs	a_ticking_default_no_box_whose_enter_is_not_taken_is_pressed_once	press-landing-and-retry	
src/supervise/run_engine_tests.rs	a_too_many_box_reads_no_transcript_and_waits_for_none	approval-breaker-read-only-sound	
src/supervise/run_engine_tests.rs	a_trust_dialog_under_a_shell_with_no_osc7_is_decided_by_the_agents_own_folder	loop-applies-decision	
src/supervise/run_engine_tests.rs	a_turn_end_the_host_let_go_is_decided_again	host-turn-end-protocol	
src/supervise/run_engine_tests.rs	a_turn_the_host_typed_is_no_short_turn_of_the_workers	host-turn-end-protocol	
src/supervise/run_engine_tests.rs	a_turn_the_host_typed_never_holds_the_hosts_next_step	host-turn-end-protocol	
src/supervise/run_engine_tests.rs	a_watch_claims_the_session_renews_and_releases_it	claim-refines-model	
src/supervise/run_engine_tests.rs	an_attended_look_answers_a_launch_on_its_own_reads	never-type-over-a-person	
src/supervise/run_engine_tests.rs	an_escalated_box_a_person_answered_is_cleared_while_the_worker_stays_busy	attention-ownership	
src/supervise/run_engine_tests.rs	an_exception_is_journaled_once_and_cleared_when_the_worker_works	exception-lifecycle	
src/supervise/run_engine_tests.rs	an_exception_stands_while_its_wall_is_out_of_view_and_ends_when_the_agent_works	exception-lifecycle	
src/supervise/run_engine_tests.rs	an_idle_point_read_before_the_footer_waits_for_the_footer_frame	wake-protocol	
src/supervise/run_engine_tests.rs	an_inline_repl_above_the_tail_is_the_hosts_idle_point	tail-reread-equivalence	
src/supervise/run_engine_tests.rs	behind_another_supervisors_claim_the_loop_only_watches	claim-refines-model	
src/supervise/run_engine_tests.rs	each_live_row_rule_alone_reads_the_fresh_panes_dialog_whole	tail-reread-equivalence	
src/supervise/run_engine_tests.rs	err_halted_parks_until_the_hold_lifts_and_the_loop_goes_on	hold-and-refusal-parking	
src/supervise/run_engine_tests.rs	every_cut_of_the_tail_across_a_fresh_panes_box_is_answered	tail-reread-equivalence	
src/supervise/run_engine_tests.rs	every_decision_is_a_ledger_row	ledger-one-row-per-decision	
src/supervise/run_engine_tests.rs	full_power_ledgers_the_decisions_subject	ledger-one-row-per-decision	
src/supervise/run_engine_tests.rs	full_power_presses_the_same_box_every_time_it_comes_back	press-landing-and-retry	
src/supervise/run_engine_tests.rs	park_as_worker	§4	
src/supervise/run_engine_tests.rs	run_hosted_runs_until_it_is_stopped_and_clears_its_badge	stop-semantics	
src/supervise/run_engine_tests.rs	the_free_text_row_is_left_by_arrows_then_enter_never_a_digit	question-answer-refines-model	
src/supervise/run_engine_tests.rs	the_host_is_told_a_turn_ran_only_when_one_did	host-turn-end-protocol	
src/supervise/run_engine_tests.rs	the_host_reads_its_usage_tap_at_each_decided_idle_turn_end	host-turn-end-protocol	
src/supervise/run_engine_tests.rs	the_host_reads_its_usage_tap_at_busy_reads_and_never_at_an_idle_one	host-turn-end-protocol	
src/supervise/run_engine_tests.rs	the_host_takes_no_step_at_the_login_wall_and_steps_once_it_is_back	idle-point-classification	
src/supervise/run_engine_tests.rs	the_hosts_default_presses_a_write_and_ledgers_it_unproven	loop-applies-decision	
src/supervise/run_engine_tests.rs	the_hosts_step_is_taken_at_the_idle_point_and_the_loop_runs_on	host-turn-end-protocol	
src/supervise/run_engine_tests.rs	the_limit_options_dialog_waits_for_the_reset_by_itself	limit-dialog-wait-row	
src/supervise/run_engine_tests.rs	the_loop_says_which_guard_withheld_the_point_the_host_asked_for	idle-point-classification	
src/supervise/run_engine_tests.rs	the_mcp_server_dialogs_badge_says_every_answer_is_kept_at_every_level	approval-dialog-rules	
src/supervise/run_engine_tests.rs	the_owners_subagent_rm_box_is_pressed_once_by_the_hosts_default	loop-applies-decision	
src/supervise/run_engine_tests.rs	the_probes_gate_reads_a_refused_wall_as_it_was_decided	usage-probe-refines-model	
src/supervise/run_engine_tests.rs	the_rm_breaker_is_approved_under_a_scratch_root_and_escalated_otherwise	loop-applies-decision	
src/supervise/run_engine_tests.rs	the_same_box_back_after_its_declines_is_declined_again_after_a_pause	doubling-backoff	
src/supervise/run_engine_tests.rs	the_sessions_foreground_group_is_read_off_the_roster	session-group	
src/supervise/run_engine_tests.rs	the_sessions_word_decides_its_question_dialog_over_the_switch	question-priority-resolution	
src/supervise/run_engine_tests.rs	the_text_press_refuses_a_host_that_cannot_fence_it_and_an_enter_it_did_not_earn	question-text-safety	
src/supervise/run_engine_tests.rs	the_too_many_breaker_over_a_proven_read_is_ledgered_under_its_rule	approval-breaker-read-only-sound	
src/supervise/run_engine_tests.rs	the_trust_dialog_is_answered_by_a_confirmed_focus_move_then_enter	focus-choice-refines-model	
src/supervise/run_engine_tests.rs	the_trust_dialog_is_never_entered_unconfirmed	focus-choice-refines-model	
src/supervise/run_engine_tests.rs	tier1_a_decline_keystroke_goes_only_where_the_model_allows	decline-keys-refines-model	
src/supervise/run_engine_tests.rs	tier1_a_downward_move_and_a_multiselect_go_only_where_the_model_allows	question-answer-refines-model	
src/supervise/run_engine_tests.rs	tier1_a_question_key_goes_only_where_the_model_allows	question-answer-refines-model	
src/supervise/run_engine_tests.rs	tier1_enter_goes_only_on_a_confirmed_focus	focus-choice-refines-model	
src/supervise/run_engine_tests.rs	tier1_the_cards_0_goes_only_where_the_model_allows	survey-press-refines-model	
src/supervise/run_engine_tests.rs	tier1_the_cards_0_never_goes_over_a_draft_or_a_box	survey-press-refines-model	
src/supervise/run_engine_tests.rs	tier1_the_cards_0_tells_a_homed_draft_from_the_dim_suggestion	survey-press-refines-model	
src/supervise/run_engine_tests.rs	tier1_the_cards_0_waits_out_a_persons_hand	survey-press-refines-model	
src/supervise/run_engine_tests.rs	tier1_the_cards_dropped_0_is_backspaced_where_the_model_allows	survey-press-refines-model	
src/supervise/run_engine_tests.rs	tier1_the_cards_taken_0_is_never_backspaced_under_its_debounce	survey-press-refines-model	
src/supervise/run_engine_tests.rs	tier1_the_claim_is_renewed_before_every_press_and_none_goes_under_anothers	claim-refines-model	
src/supervise/run_engine_tests.rs	tier1_the_probes_writes_go_only_where_the_model_allows	usage-probe-refines-model	
src/supervise/run_engine_tests.rs	tier1_the_resets_keys_go_only_where_the_model_allows	reset-step-refines-model	
src/supervise/run_engine_tests.rs	trouble_that_comes_behind_a_limit_decides_the_standing_point_again	memory-ceiling-decision	
src/supervise/run_engine_tests.rs	trouble_that_comes_while_idle_decides_the_standing_point_again	memory-ceiling-decision	
src/supervise/run_engine_tests.rs	unattended_a_dialog_that_takes_no_key_is_tried_on_the_back_off_and_never_handed_over	question-answer-refines-model	
src/supervise/run_engine_tests.rs	unattended_a_screen_that_never_holds_still_is_waited_on_the_back_off_never_keyed_again	question-answer-refines-model	
src/supervise/run_question_text_tests.rs	a_checked_option_hands_a_multi_select_over_under_a_text_priority	question-text-safety	
src/supervise/run_question_text_tests.rs	a_controllers_option_over_text_in_the_field_is_pressed_and_a_multi_selects_is_refused_first	controller-answer	
src/supervise/run_question_text_tests.rs	a_fallback_begun_on_a_tight_pane_is_finished_when_the_pane_grows	question-priority-resolution	
src/supervise/run_question_text_tests.rs	a_fresh_supervisor_over_a_typed_field_hands_it_over_without_a_key	question-text-safety	
src/supervise/run_question_text_tests.rs	a_lone_single_select_is_answered_by_the_long_term_text	question-text-safety	
src/supervise/run_question_text_tests.rs	a_multi_select_text_that_reads_back_unticked_is_handed_over_at_once	question-text-safety	
src/supervise/run_question_text_tests.rs	a_persons_input_after_the_text_leaves_the_enter_but_not_the_told_choice	told-choice	
src/supervise/run_question_text_tests.rs	a_persons_key_between_the_read_and_the_send_skips_the_policy_text	question-text-refines-model	
src/supervise/run_question_text_tests.rs	a_persons_text_in_the_field_is_theirs_under_a_text_priority	question-text-safety	
src/supervise/run_question_text_tests.rs	a_preview_under_long_term_is_answered_with_its_recommended_option_and_told_so	question-priority-resolution	
src/supervise/run_question_text_tests.rs	a_question_asked_a_third_time_under_a_text_word_waits_its_pause_before_its_text	question-text-safety	
src/supervise/run_question_text_tests.rs	a_question_asked_again_after_its_dialog_went_is_written_afresh	question-text-safety	
src/supervise/run_question_text_tests.rs	a_text_in_doubt_is_remembered_and_never_written_again_one_not_written_is_not	question-text-safety	
src/supervise/run_question_text_tests.rs	a_text_that_disappears_after_it_read_back_is_handed_over_never_rewritten	question-text-safety	
src/supervise/run_question_text_tests.rs	a_text_that_reads_back_short_twice_is_handed_over_never_written_again	question-text-safety	
src/supervise/run_question_text_tests.rs	a_text_the_dialog_never_took_is_handed_over_never_written_twice	question-text-safety	
src/supervise/run_question_text_tests.rs	a_window_that_cannot_fence_a_typed_answer_gets_the_recommended_option_said	question-priority-resolution	
src/supervise/run_question_text_tests.rs	a_word_changed_after_the_first_key_hands_the_dialog_over	question-priority-resolution	
src/supervise/run_question_text_tests.rs	an_enter_never_goes_after_the_field_changed_under_the_moves	question-text-safety	
src/supervise/run_question_text_tests.rs	an_unknown_session_word_hands_the_dialog_over_under_a_text_global	question-priority-resolution	
src/supervise/run_question_text_tests.rs	the_fit_is_measured_against_the_pane_not_the_tail_read	question-priority-resolution	
src/supervise/run_question_text_tests.rs	the_loops_own_text_unticked_on_a_multi_select_is_handed_over	question-text-safety	
src/supervise/run_question_text_tests.rs	the_sessions_short_term_word_answers_over_a_global_recommended	question-priority-resolution	
src/supervise/run_question_text_tests.rs	tier1_a_tab_that_falls_back_goes_only_where_the_model_allows	question-text-refines-model	
src/supervise/run_question_text_tests.rs	tier1_the_policy_text_goes_only_where_the_model_allows	question-text-refines-model	
src/supervise/run_question_text_tests.rs	two_questions_and_their_review_are_answered_by_text_then_submitted	question-text-safety	
src/supervise/run_tests.rs	a_0_that_landed_in_the_composer_is_backspaced_not_dismissed	stray-digit-cleanup	
src/supervise/run_tests.rs	a_box_the_loop_does_not_approve_is_escalated_once_and_cleared_when_it_goes	escalation-badge	
src/supervise/run_tests.rs	a_box_under_a_dead_footer_is_read_again_at_once	await-turn-wait-protocol	
src/supervise/run_tests.rs	a_budget_spent_in_an_outage_is_the_timeout	budget-bound	
src/supervise/run_tests.rs	a_busy_read_before_a_lost_connection_still_counts	review-point-once	
src/supervise/run_tests.rs	a_busy_read_presses_a_card_once_and_leaves_the_ladder_to_the_idle_look	survey-press-gate	
src/supervise/run_tests.rs	a_busy_row_the_guard_could_not_read_is_still_up	busy-up	
src/supervise/run_tests.rs	a_connection_that_never_comes_back_ends_when_the_window_lapses	outage-ride-out	
src/supervise/run_tests.rs	a_connection_turned_away_is_ridden_out	outage-ride-out	
src/supervise/run_tests.rs	a_continuing_shortly_notice_is_a_reset_a_minute_off	limit-episode	
src/supervise/run_tests.rs	a_count_that_started_over_with_no_request_failing_is_seen_moved	review-point-once	
src/supervise/run_tests.rs	a_dead_footer_above_the_live_zone_holds_no_wait	await-turn-wait-protocol	
src/supervise/run_tests.rs	a_draft_in_the_composer_holds_the_continuation	limit-episode	
src/supervise/run_tests.rs	a_draft_of_more_than_one_row_is_typed	typed-draft	
src/supervise/run_tests.rs	a_frameless_screen_still_changing_is_busy_until_it_settles	await-turn-wait-protocol	
src/supervise/run_tests.rs	a_guard_that_matches_no_row_of_the_parsed_box_is_handed_over	approval-press-safety	
src/supervise/run_tests.rs	a_headerless_box_under_a_tall_transcript_is_named_by_its_own_first_row	escalation-badge	
src/supervise/run_tests.rs	a_journal_that_fails_is_said_once_and_stops_nothing	journal-mirrors-output	
src/supervise/run_tests.rs	a_jump_of_30_points_is_a_compaction_and_a_box_is_not	context-watch	
src/supervise/run_tests.rs	a_late_vendor_timer_takes_the_0_inside_the_landing_time_and_is_never_backspaced	§4	
src/supervise/run_tests.rs	a_later_outage_is_a_new_one_with_its_own_window	outage-ride-out	
src/supervise/run_tests.rs	a_limit_escalates_once_and_extends_the_budget_once	limit-episode	
src/supervise/run_tests.rs	a_look_the_host_scheduled_ends_the_idle_wait_step_at_its_instant	host-look-wake	
src/supervise/run_tests.rs	a_loop_that_sees_no_survey_tells_the_host_once	survey-host-told	
src/supervise/run_tests.rs	a_narrowed_pane_is_not_a_compaction	context-watch	
src/supervise/run_tests.rs	a_new_point_that_looks_like_the_last_one_is_still_reported	review-point-once	
src/supervise/run_tests.rs	a_persons_grace_holds_the_0_on_busy_reads_and_says_so_once	survey-press-gate	
src/supervise/run_tests.rs	a_point_is_new_when_its_transcript_moved_whatever_the_loop_saw	review-point-once	
src/supervise/run_tests.rs	a_press_in_flight_is_decided_again_from_a_fresh_read	approval-press-safety	
src/supervise/run_tests.rs	a_read_that_keeps_coming_back_is_approved_twice_then_handed_over	approval-press-safety	
src/supervise/run_tests.rs	a_refusal_naming_a_tighter_cap_is_sent_again_cut_to_it	escalation-badge	
src/supervise/run_tests.rs	a_refused_backspace_keeps_the_0_owed_until_the_server_takes_it	§4	
src/supervise/run_tests.rs	a_refused_escalation_is_journaled_and_the_loop_goes_on	escalation-badge	
src/supervise/run_tests.rs	a_request_not_served_is_told_from_a_server_answer	ctl-reply-lost	
src/supervise/run_tests.rs	a_reset_further_off_than_a_week_extends_nothing	limit-extend	
src/supervise/run_tests.rs	a_retry_that_hits_the_wall_again_is_the_same_episode	limit-episode	
src/supervise/run_tests.rs	a_screen_without_a_composer_gets_no_continuation	limit-episode	
src/supervise/run_tests.rs	a_skipped_0_on_a_survey_still_open_is_handed_over	survey-press-gate	
src/supervise/run_tests.rs	a_skipped_guard_is_not_an_approval	approval-press-safety	
src/supervise/run_tests.rs	a_skipped_survey_press_says_nothing	survey-press-gate	
src/supervise/run_tests.rs	a_span_is_read_once_and_the_continuations_back_off_ten_then_thirty	§4	
src/supervise/run_tests.rs	a_span_the_grid_no_longer_has_is_read_again	await-turn-wait-protocol	
src/supervise/run_tests.rs	a_standing_approval_attention_survives_the_episode_close	escalation-badge	
src/supervise/run_tests.rs	a_survey_back_after_a_busy_spell_is_a_new_appearance	survey-report-once	
src/supervise/run_tests.rs	a_survey_waits_while_a_box_is_up	survey-press-gate	
src/supervise/run_tests.rs	a_survey_whose_0_did_not_take_is_tried_again_until_it_goes	survey-press-gate	
src/supervise/run_tests.rs	a_tail_whose_live_zone_starts_at_its_first_row_is_still_scoped	await-turn-wait-protocol	
src/supervise/run_tests.rs	a_withheld_card_first_read_busy_is_told_at_the_idle_look_and_ended_when_it_goes	survey-host-told	
src/supervise/run_tests.rs	a_worked_point_closes_the_episode_and_types_nothing	limit-episode	
src/supervise/run_tests.rs	after_an_outage_the_point_is_reported_again_and_waited_past_on_the_new_seq	review-point-once	
src/supervise/run_tests.rs	an_answered_box_loses_its_badge_at_the_first_busy_read	escalation-badge	
src/supervise/run_tests.rs	an_auto_continue_notice_closes_at_the_first_busy_read_and_nothing_is_typed	limit-episode	
src/supervise/run_tests.rs	an_auto_continue_notice_is_never_continued_even_past_its_time	limit-episode	
src/supervise/run_tests.rs	an_await_timeout_is_a_step_not_an_error	await-turn-wait-protocol	
src/supervise/run_tests.rs	an_event_summary_is_what_the_worker_said_not_the_survey	render-phase-grammar	
src/supervise/run_tests.rs	an_outage_that_keeps_coming_back_is_one_window	outage-ride-out	
src/supervise/run_tests.rs	an_unknown_card_without_the_vendors_mark_is_said_and_never_pressed	survey-press-gate	
src/supervise/run_tests.rs	at_the_reset_one_continuation_goes_with_the_rules_and_the_answer_closes_the_episode	limit-episode	
src/supervise/run_tests.rs	await_turn_and_supervise_ride_it_out_with_the_lines_on_stderr	outage-ride-out	
src/supervise/run_tests.rs	await_turn_on_the_per_request_client_rides_out_exit_75	outage-ride-out	
src/supervise/run_tests.rs	context_warn_0_says_neither_line	context-watch	
src/supervise/run_tests.rs	dismiss_surveys_presses_one_guarded_0	survey-press-gate	
src/supervise/run_tests.rs	every_kind_is_dismissed_over_a_workflow_wait_on_the_busy_read	survey-press-gate	
src/supervise/run_tests.rs	exit_75_is_a_request_not_served	ctl-reply-lost	
src/supervise/run_tests.rs	guard_errors_are_retried_or_surfaced_never_approved	approval-press-safety	
src/supervise/run_tests.rs	no_such_session_is_final_outside_an_outage_and_not_yet_inside_one	outage-ride-out	
src/supervise/run_tests.rs	nothing_happens_after_the_budget	budget-bound	
src/supervise/run_tests.rs	a_boxs_classify_word_is_the_read_only_rules_verdict_on_its_readings	render-phase-grammar	approval-safe-bash-sound
src/supervise/run_tests.rs	render_prompt_prints_the_notes_after_the_description_and_before_classify	render-phase-grammar	
src/supervise/run_tests.rs	a_bash_box_not_read_whole_is_never_called_read_only	render-phase-grammar	
src/supervise/run_tests.rs	reply_readers_match_the_wire	ctl-reply-readers	
src/supervise/run_tests.rs	resume_never_continues_what_the_policy_limited	limit-episode	
src/supervise/run_tests.rs	supervise_auto_approves_reads_guarded_and_hands_over_writes	approval-press-safety	
src/supervise/run_tests.rs	supervise_hands_a_limit_notice_over_like_idle	supervise-result-and-log-split	
src/supervise/run_tests.rs	supervise_journals_what_watch_would_have_printed	journal-mirrors-output	
src/supervise/run_tests.rs	supervise_watches_one_run	context-watch	
src/supervise/run_tests.rs	the_approval_line_is_sanitized_and_cut_by_cells	approval-line-cells	
src/supervise/run_tests.rs	the_context_watch_is_inclusive_and_needs_the_frame	context-watch	
src/supervise/run_tests.rs	the_watch_reads_claude_codes_mode_row_and_its_compaction	context-watch	
src/supervise/run_tests.rs	the_watch_reads_the_screen_another_read_left_at_rest	context-watch	
src/supervise/run_tests.rs	the_watch_reads_the_screen_at_rest	context-watch	
src/supervise/run_tests.rs	the_drafts_card_is_never_pressed_or_called_a_survey	survey-press-gate	
src/supervise/run_tests.rs	the_notice_again_after_a_continuation_is_the_same_episode_and_backs_off	limit-episode	
src/supervise/run_tests.rs	the_same_box_after_the_worker_worked_is_a_new_ask	escalation-badge	
src/supervise/run_tests.rs	the_same_box_is_never_pressed_twice_on_an_unchanged_screen	approval-press-safety	
src/supervise/run_tests.rs	the_screen_leaving_the_notice_is_continued_at_once	limit-episode	
src/supervise/run_tests.rs	the_survey_guard_matches_the_open_survey_and_no_copy_of_it	survey-guard-unique	
src/supervise/run_tests.rs	the_wake_at_a_host_look_steps_the_host_and_decides_no_turn_end	host-look-wake	
src/supervise/run_tests.rs	under_approve_none_every_prompt_is_handed_over	approval-press-safety	
src/supervise/run_tests.rs	watch_journals_every_line_it_prints	journal-mirrors-output	
src/supervise/run_tests.rs	watch_lines_are_cut_at_160_characters	line-clip-and-exit-reason	
src/supervise/run_tests.rs	watch_rides_out_a_lost_connection_to_the_next_event	outage-ride-out	
src/supervise/run_tests.rs	watch_says_a_descent_once_and_the_compaction_once	context-watch	
src/supervise/run_tests.rs	watch_says_a_survey_once_when_it_appears	survey-report-once	
src/supervise/screen.rs	a_status_stamp_reads_as_the_one_person_fact	human-input-stamp	
src/supervise/screen.rs	a_tail_reads_first_row_places_the_cursor	screen-cursor-index	
src/supervise/screen.rs	json_reader_handles_the_value_vocabulary	json-reader-grammar	
src/supervise/screen.rs	only_a_whole_person_launch_witness_is_read	parse-text-json	never-type-over-a-person
src/supervise/screen.rs	parses_the_servers_text_json_shape	parse-text-json	
src/supervise/screen.rs	rejects_what_is_not_a_screen	parse-text-json	
src/supervise/screen.rs	the_persons_stamp_is_read_and_its_absence_is_unknown	parse-text-json	
src/supervise/screen.rs	the_screen_generation_is_read_and_a_malformed_one_is_none	parse-text-json	
src/supervise/stall.rs	a_stalled_or_stopped_input_is_a_stall_and_every_other_word_is_not	stall-status-read	
src/supervise/stall.rs	the_stall_facts_name_the_wait_the_backlog_and_the_size	frozen-text	
src/supervise/stall_tests.rs	a_continuation_over_a_worker_already_stalled_types_nothing	stall-hold	
src/supervise/stall_tests.rs	a_continuation_warned_input_unread_reads_status_at_the_next_look	stall-hold	
src/supervise/stall_tests.rs	a_frozen_worker_is_held_and_its_badge_withdrawn	stall-hold	
src/supervise/stall_tests.rs	a_thawed_worker_has_its_point_decided_again	stall-hold	
src/supervise/stall_tests.rs	a_wait_that_never_read_status_probes_and_holds	stall-probe-cost	
src/supervise/stall_tests.rs	a_warning_is_read_off_the_first_line_the_server_wrote	ctl-reply-readers	
src/supervise/stall_tests.rs	a_worker_already_stalled_when_the_loop_looks_is_neither_pressed_nor_badged	stall-hold	
src/supervise/stall_tests.rs	a_worker_frozen_past_the_bound_is_ended_for_its_relaunch	stall-remedy-refines-model	
src/supervise/stall_tests.rs	a_worker_frozen_past_the_bound_is_warned_about_and_left_running	stall-remedy-refines-model	frozen-text
src/supervise/stall_tests.rs	the_host_is_told_the_stall_it_holds_and_that_it_lifted	stall-hold	
src/supervise/stall_tests.rs	the_remedy_is_the_servers_restart_and_its_refusals_decide	stall-remedy-refines-model	frozen-text
src/supervise/stall_tests.rs	the_windows_host_asks_a_host_that_does_not_measure_input_once	stall-probe-cost	
src/supervise/stall_tests.rs	the_windows_host_raises_no_badge_over_a_worker_frozen_under_its_point	stall-hold	
src/supervise/stall_tests.rs	the_windows_host_withdraws_the_badge_of_a_worker_that_freezes_under_it	stall-hold	
src/supervise/stall_tests.rs	tier1_the_real_remedy_signals_exactly_where_the_model_terms	stall-remedy-refines-model	
src/supervise/transport.rs	a_dropped_connection_is_lost_and_the_next_request_dials_again	relay-connection	
src/supervise/transport.rs	a_host_that_hangs_up_is_found_again_by_its_graph_entry	relay-connection	
src/supervise/tap_clock_loop_tests.rs	a_cut_session_walls_wait_is_timed_by_the_taps_reset	tap-wall-wait	tap-clock-step-says,tap-clock-timed
src/supervise/tap_clock_loop_tests.rs	a_left_walls_wait_is_retimed_by_the_tap_read_while_it_waits	tap-wall-wait	tap-clock-step-says,tap-clock-timed
src/supervise/tap_clock_loop_tests.rs	a_spend_wall_is_never_timed_by_the_tap	tap-wall-wait	tap-clock-window-of-wall
src/supervise/tap_clock_loop_tests.rs	a_successor_reads_a_cut_wall_met_again_afresh	tap-wall-wait	tap-clock-step-says
src/supervise/tap_clock_loop_tests.rs	a_successor_sets_its_own_tap_against_the_notices_reset	tap-wall-wait	tap-clock-timed
src/supervise/tap_clock_loop_tests.rs	a_successor_waits_to_the_taps_reset_over_a_transcript_limits_row	tap-wall-wait	tap-clock-timed
src/supervise/tap_clock_loop_tests.rs	a_successors_own_tap_reading_at_a_cut_wall_is_taken_alone	tap-wall-wait	tap-clock-timed
src/supervise/tap_clock_loop_tests.rs	a_tap_showing_the_window_rolled_over_ends_a_wait_with_no_reset	tap-wall-wait	tap-clock-step-says,tap-clock-timed
src/supervise/tap_clock_loop_tests.rs	a_transcript_limit_is_timed_by_the_tap_against_its_rows_reset	tap-wall-wait	tap-clock-timed
src/supervise/tap_clock_loop_tests.rs	a_wall_met_again_is_read_afresh_by_the_tap	tap-wall-wait	tap-clock-step-says
src/supervise/tap_clock_loop_tests.rs	the_notice_and_the_tap_take_the_taps_reset_or_the_later_with_a_ledger_row	tap-wall-wait	tap-clock-step-says,tap-clock-timed
src/supervise/tap_clock_loop_tests.rs	the_taps_reading_during_the_wait_decides_the_wall_again	tap-wall-wait	tap-clock-step-says,tap-clock-timed
src/supervise/tap_clock_tests.rs	a_full_window_read_since_the_wall_times_it_to_its_reset	tap-clock-step-says	
src/supervise/tap_clock_tests.rs	a_full_window_shown_rolled_over_ends_a_wait_with_no_reset	tap-clock-step-says	
src/supervise/tap_clock_tests.rs	a_repeat_written_after_the_walls_first_read_times_it	tap-clock-step-says	
src/supervise/tap_clock_tests.rs	a_wall_timed_by_two_windows_takes_the_later_full_reset	tap-clock-step-says	
src/supervise/tap_clock_tests.rs	a_window_is_full_only_where_the_vendors_figure_is_100	tap-clock-step-says	
src/supervise/tap_clock_tests.rs	each_wall_is_timed_by_its_own_window	tap-clock-window-of-wall	
src/supervise/tap_clock_tests.rs	the_tap_and_the_notice_give_the_tap_within_the_slack_and_else_the_later	tap-clock-timed	
src/supervise/transport.rs	a_refused_preferred_token_falls_back_to_the_one_beside_the_socket	relay-connection	
src/supervise/transport.rs	a_session_no_host_answers_for_rides_a_connection_of_its_own	relay-connection	
src/supervise/transport.rs	a_worker_on_another_instance_gets_its_own_connection	relay-connection	
src/supervise/transport.rs	an_absent_socket_is_lost	relay-connection	
src/supervise/transport.rs	replies_render_as_aterm_ctl_prints_them	relay-render	
src/supervise/transport.rs	the_interrupter_ends_a_parked_request	relay-interrupter	
src/supervise/transport.rs	the_interrupter_hangs_up_a_connection_its_server_half_closed	relay-interrupter	
src/supervise/transport.rs	the_interrupter_hangs_up_every_connection	relay-interrupter	
src/supervise/transport.rs	the_reply_wait_follows_an_awaits_own_timeout	reply-wait	
src/supervise/turn_end_loop_tests.rs	the_auto_mode_setup_offer_over_a_continuation_is_answered_and_the_continuation_typed_again	auto-mode-setup-loop	
src/supervise/turn_end_loop_tests.rs	the_auto_mode_setup_offer_over_the_probe_leaves_nothing_typed	auto-mode-setup-loop	
src/supervise/turn_end_loop_tests.rs	a_529_waits_its_rung_then_tries_its_one_line_exactly_once	api-wall-acts	
src/supervise/turn_end_loop_tests.rs	a_back_off_the_mac_slept_through_is_over_once_the_wake_settles	sleep-wake-settle	
src/supervise/turn_end_loop_tests.rs	a_background_call_that_makes_no_progress_is_named_at_stalled_and_in_the_exception	stall-ladder	
src/supervise/turn_end_loop_tests.rs	a_bare_clock_reset_is_placed_for_the_wall_it_belongs_to	limit-wall-placement	
src/supervise/turn_end_loop_tests.rs	a_bucket_whose_countdown_ended_is_waited_on_across_a_loop_restart	left-limit-tracking	
src/supervise/turn_end_loop_tests.rs	a_call_named_after_an_unnamed_rung_is_told_at_once_not_a_pace_later	stall-ladder	
src/supervise/turn_end_loop_tests.rs	a_claimed_probe_types_usage_samples_the_settled_panel_and_closes_it	usage-probe-gates	
src/supervise/turn_end_loop_tests.rs	a_clear_at_an_armed_limit_closes_the_ledgers_wall_and_says_why	left-limit-tracking	
src/supervise/turn_end_loop_tests.rs	a_clearing_the_loop_before_answered_is_not_answered_again	account-ledger-readback	
src/supervise/turn_end_loop_tests.rs	a_context_no_compact_lowers_is_one_exception_across_the_rungs_until_it_is_lowered	api-wall-acts	
src/supervise/turn_end_loop_tests.rs	a_continuation_a_refusal_turned_away_is_typed_once_it_can_be	refused-write-retried	
src/supervise/turn_end_loop_tests.rs	a_dialog_no_reader_names_climbs_to_a_record_and_a_line_and_its_end_withdraws_both	stall-ladder	
src/supervise/turn_end_loop_tests.rs	a_due_probe_that_is_not_run_says_which_gate_shut_once_a_point	usage-probe-gates	
src/supervise/turn_end_loop_tests.rs	a_due_the_sleep_fell_short_of_falls_at_its_wall_time	sleep-wake-settle	
src/supervise/turn_end_loop_tests.rs	a_fable_limit_with_no_host_to_relaunch_is_waited_out_never_by_model	usage-wall-wait	
src/supervise/turn_end_loop_tests.rs	a_fenced_host_writes_the_continuation_only_on_the_judged_screen	act-write-protocol	
src/supervise/turn_end_loop_tests.rs	a_fenced_press_a_hold_turned_away_is_pressed_once_it_lifts	refused-write-retried	
src/supervise/turn_end_loop_tests.rs	a_finished_task_opened_again_is_asked_about_at_the_limit_its_turn_ends_on	account-advice-asks	
src/supervise/turn_end_loop_tests.rs	a_folded_panel_is_no_sample_and_its_grid_size_is_not_probed_again	usage-probe-panel-close	
src/supervise/turn_end_loop_tests.rs	a_half_erased_panel_after_the_escape_gets_no_second_escape	usage-probe-panel-close	
src/supervise/turn_end_loop_tests.rs	a_handed_over_idle_escalation_survives_the_restart	stop-and-handover	
src/supervise/turn_end_loop_tests.rs	a_held_box_is_answered_once_the_persons_grace_has_passed	never-type-over-a-person	
src/supervise/turn_end_loop_tests.rs	a_host_that_answers_a_short_wait_is_asked_again_when_it_runs_out	account-advice-asks	
src/supervise/turn_end_loop_tests.rs	a_limit_whose_notice_left_is_waited_on_across_a_loop_restart	left-limit-tracking	
src/supervise/turn_end_loop_tests.rs	a_long_texts_guard_never_matches_a_shells_row	composer-guard	
src/supervise/turn_end_loop_tests.rs	a_loop_started_on_an_old_told_move_waits_at_its_lost_login	login-lifecycle-accounts	
src/supervise/turn_end_loop_tests.rs	a_loop_stopped_mid_probe_presses_nothing_and_the_next_loop_closes_the_panel	stop-and-handover	
src/supervise/turn_end_loop_tests.rs	a_loop_that_starts_after_another_keeps_its_bound_and_its_latch	account-ledger-readback	
src/supervise/turn_end_loop_tests.rs	a_loop_that_takes_up_a_move_made_at_a_wall_carries_it_on_past_the_notice	account-ledger-readback	
src/supervise/turn_end_loop_tests.rs	a_loop_watching_behind_another_claim_asks_no_escape_of_an_owed_panel	usage-probe-gates	
src/supervise/turn_end_loop_tests.rs	a_lost_login_left_inside_the_own_acts_window_types_login	login-wall-acts	
src/supervise/turn_end_loop_tests.rs	a_lost_login_no_read_saw_busy_is_struck_before_the_host_is_asked	account-move-answer	
src/supervise/turn_end_loop_tests.rs	a_lost_login_types_login_and_raises_no_badge	login-wall-acts	
src/supervise/turn_end_loop_tests.rs	a_lost_login_with_no_account_left_types_login_and_asks_its_sign_in_now	login-lifecycle-accounts	
src/supervise/turn_end_loop_tests.rs	a_model_switch_is_read_back_by_a_loop_that_starts_after_it	model-switch-carry	
src/supervise/turn_end_loop_tests.rs	a_move_ahead_of_a_wall_types_nothing_and_its_point_is_decided_as_with_no_move	account-move-execution	
src/supervise/turn_end_loop_tests.rs	a_move_ahead_the_host_defers_never_holds_the_continuation	account-move-execution	
src/supervise/turn_end_loop_tests.rs	a_move_made_at_a_wall_is_carried_on_past_the_limit_it_left	account-move-execution	
src/supervise/turn_end_loop_tests.rs	a_move_seeded_from_the_ledger_is_answered_once_across_loops	account-ledger-readback	
src/supervise/turn_end_loop_tests.rs	a_one_login_machine_waits_on_its_rows_sign_in_and_types_no_login	login-lifecycle-accounts	
src/supervise/turn_end_loop_tests.rs	a_panel_closed_under_the_read_is_no_new_point_and_the_probes_go_on	usage-probe-panel-close	
src/supervise/turn_end_loop_tests.rs	a_panel_that_never_opens_gets_no_escape	usage-probe-panel-close	
src/supervise/turn_end_loop_tests.rs	a_panel_that_never_settles_is_closed_with_no_sample	usage-probe-panel-close	
src/supervise/turn_end_loop_tests.rs	a_panel_with_a_turn_running_under_it_gets_no_escape_and_no_sample	usage-probe-panel-close	
src/supervise/turn_end_loop_tests.rs	a_persons_keystroke_holds_the_loop_for_the_grace	never-type-over-a-person	
src/supervise/turn_end_loop_tests.rs	a_persons_question_answered_at_length_is_left_to_them	turn-end-decision	
src/supervise/turn_end_loop_tests.rs	a_point_after_a_sleep_a_request_saw_is_held_only_until_the_wakes_settle	sleep-wake-settle	
src/supervise/turn_end_loop_tests.rs	a_point_decided_again_says_not_new	usage-probe-gates	
src/supervise/turn_end_loop_tests.rs	a_probe_books_no_act_and_moves_none_of_the_policys_memory	usage-probe-memory-neutral	
src/supervise/turn_end_loop_tests.rs	a_question_in_prose_is_answered_by_the_sessions_priority_word	turn-end-decision	
src/supervise/turn_end_loop_tests.rs	a_refusals_episode_ends_at_a_taken_write_and_at_a_lifted_hold	refusal-news-episode	
src/supervise/turn_end_loop_tests.rs	a_refusals_episode_is_its_holder_and_ends_after_a_quiet_spell	refusal-news-episode	
src/supervise/turn_end_loop_tests.rs	a_refused_escape_is_sent_again_and_never_a_second_usage	usage-probe-panel-close	
src/supervise/turn_end_loop_tests.rs	a_renewed_login_nothing_relaunches_the_tab_onto_is_never_renewed_again	login-lifecycle-accounts	
src/supervise/turn_end_loop_tests.rs	a_reprinted_wall_is_not_sent_to_tomorrow_at_once	usage-wall-wait	
src/supervise/turn_end_loop_tests.rs	a_reset_the_sleep_carried_past_waits_for_the_settle_though_a_repaint_wakes_the_loop	sleep-wake-settle	
src/supervise/turn_end_loop_tests.rs	a_resume_at_an_armed_bucket_types_nothing_into_it	left-limit-tracking	
src/supervise/turn_end_loop_tests.rs	a_resume_at_an_armed_limit_types_nothing_into_it	left-limit-tracking	
src/supervise/turn_end_loop_tests.rs	a_resume_at_an_armed_limit_waits_on_it_as_a_cancelled_one	left-limit-tracking	
src/supervise/turn_end_loop_tests.rs	a_resume_the_limit_answers_inside_its_window_is_waited_on_to_the_new_reset	left-limit-tracking	
src/supervise/turn_end_loop_tests.rs	a_sample_that_shows_the_account_low_moves_the_session_at_the_same_point	usage-probe-gates	
src/supervise/turn_end_loop_tests.rs	a_session_limit_whose_reset_has_passed_is_continued	usage-wall-wait	
src/supervise/turn_end_loop_tests.rs	a_sessions_jitter_is_stable_bounded_and_spread	resume-jitter	
src/supervise/turn_end_loop_tests.rs	a_sleep_between_two_steps_of_the_wait_is_seen_by_the_wait_itself	sleep-wake-settle	
src/supervise/turn_end_loop_tests.rs	a_stall_kept_up_climbs_to_a_record_and_then_an_exception_and_its_end_withdraws_both	stall-ladder	
src/supervise/turn_end_loop_tests.rs	a_stall_that_ends_tells_the_host_the_point_that_stood_under_it	stall-ladder	
src/supervise/turn_end_loop_tests.rs	a_successor_closes_only_a_panel_the_ledger_says_the_harness_opened	usage-probe-panel-close	
src/supervise/turn_end_loop_tests.rs	a_suggestion_on_a_grid_drawn_one_row_high_is_not_a_draft_at_a_wall	never-type-over-a-person	
src/supervise/turn_end_loop_tests.rs	a_torn_read_after_the_accept_key_is_read_again	act-write-protocol	
src/supervise/turn_end_loop_tests.rs	a_turn_end_after_work_is_continued_once_and_a_stop_phrase_escalates	turn-end-decision	
src/supervise/turn_end_loop_tests.rs	a_turn_served_under_the_remembered_walls_notice_is_no_background_work	left-limit-tracking	
src/supervise/turn_end_loop_tests.rs	a_unix_second_reads_back_and_stamps_as_itself_at_any_phase_of_the_clock	clock-unix-roundtrip	
src/supervise/turn_end_loop_tests.rs	a_usage_resume_typed_into_the_account_a_session_left_is_not_taken_up	account-ledger-readback	
src/supervise/turn_end_loop_tests.rs	a_usage_the_server_turns_away_or_skips_is_not_written_and_the_act_goes_on	usage-probe-left-typed	
src/supervise/turn_end_loop_tests.rs	a_wait_inside_the_wakes_settle_steps_to_its_fall	sleep-wake-settle	
src/supervise/turn_end_loop_tests.rs	a_wait_is_told_at_its_wall_second_and_again_only_when_that_second_moves	wait-told	
src/supervise/turn_end_loop_tests.rs	a_wait_on_the_hosts_advice_is_decided_again_when_its_stamp_moves	account-advice-asks	
src/supervise/turn_end_loop_tests.rs	a_wall_read_inside_the_own_acts_window_is_placed_on_its_print	usage-wall-wait	
src/supervise/turn_end_loop_tests.rs	a_wall_with_an_account_to_move_to_is_the_hosts_relaunch	account-move-execution	
src/supervise/turn_end_loop_tests.rs	a_weekly_wall_hit_in_the_evening_that_resets_in_the_morning_is_waited_for	usage-wall-wait	
src/supervise/turn_end_loop_tests.rs	a_weekly_wall_still_standing_after_a_typed_line_is_never_typed_into_again_before_its_reset	usage-wall-wait	
src/supervise/turn_end_loop_tests.rs	an_account_move_made_closes_the_model_switch_for_the_loop_after_it	model-switch-carry	
src/supervise/turn_end_loop_tests.rs	an_account_move_made_is_no_typed_act_of_the_budgets	typing-budget	
src/supervise/turn_end_loop_tests.rs	an_edge_at_the_wake_types_nothing_before_the_settle	sleep-wake-settle	
src/supervise/turn_end_loop_tests.rs	an_enter_of_the_text_left_typed_that_does_not_take_is_written_again	usage-probe-left-typed	
src/supervise/turn_end_loop_tests.rs	an_idle_screen_or_a_question_ends_the_wall_memory	left-limit-tracking	
src/supervise/turn_end_loop_tests.rs	an_off_session_has_the_harnesss_usage_panel_closed_before_the_park	usage-probe-panel-close	
src/supervise/turn_end_loop_tests.rs	an_off_session_with_a_stall_suspected_parks_and_leaves_the_panel_owed	usage-probe-panel-close	
src/supervise/turn_end_loop_tests.rs	an_off_sessions_one_close_promises_no_second_escape_and_waits_on_no_hold	usage-probe-panel-close	
src/supervise/turn_end_loop_tests.rs	an_off_sessions_refused_escape_is_its_one_close	usage-probe-panel-close	
src/supervise/turn_end_loop_tests.rs	an_older_cancelled_limit_resumed_inside_the_acts_window_is_no_days_wait	left-limit-tracking	
src/supervise/turn_end_loop_tests.rs	an_ordinary_or_escalated_point_never_asks_for_the_reach_again	api-wall-acts	
src/supervise/turn_end_loop_tests.rs	an_outage_in_the_read_leaves_the_panel_owed_and_the_same_loop_closes_it	usage-probe-panel-close	
src/supervise/turn_end_loop_tests.rs	an_owed_panels_repaint_is_never_the_worker_working	usage-probe-panel-close	
src/supervise/turn_end_loop_tests.rs	an_undated_cancelled_limit_is_placed_within_a_session_window	limit-wall-placement	
src/supervise/turn_end_loop_tests.rs	an_unknown_composer_is_never_typed_over_or_submitted	never-type-over-a-person	
src/supervise/turn_end_loop_tests.rs	an_up_measure_mid_wait_continues_the_outage_within_one_step	api-wall-acts	
src/supervise/turn_end_loop_tests.rs	another_drivers_hand_holds_the_loop	never-type-over-a-person	
src/supervise/turn_end_loop_tests.rs	claude_codes_idle_compaction_is_no_turn	turn-end-decision	
src/supervise/turn_end_loop_tests.rs	news_of_the_accounts_clears_a_latch_and_no_news_asks_nothing	account-advice-asks	
src/supervise/turn_end_loop_tests.rs	news_of_the_accounts_ends_a_sessions_wait_at_its_limit	account-advice-asks	
src/supervise/turn_end_loop_tests.rs	one_sign_in_per_login_however_many_tabs_share_it	login-wall-acts	
src/supervise/turn_end_loop_tests.rs	the_act_check_is_told_to_the_host_as_no_plan	wait-told	
src/supervise/turn_end_loop_tests.rs	the_budget_is_stretched_to_the_reset_the_wait_holds_for	usage-wall-wait	
src/supervise/turn_end_loop_tests.rs	the_host_is_asked_for_its_advice_once_a_decision	account-advice-asks	
src/supervise/turn_end_loop_tests.rs	the_host_is_asked_on_the_folded_point_where_a_latch_has_ended	account-advice-asks	
src/supervise/turn_end_loop_tests.rs	the_host_is_told_once_what_answered_the_move	account-move-answer	
src/supervise/turn_end_loop_tests.rs	the_hosts_background_step_at_the_wake_waits_for_the_settle	sleep-wake-settle	
src/supervise/turn_end_loop_tests.rs	the_hosts_background_step_is_withheld_on_usage_credits	credits-hold	
src/supervise/turn_end_loop_tests.rs	the_hosts_step_at_the_wake_waits_for_the_settle	sleep-wake-settle	
src/supervise/turn_end_loop_tests.rs	the_ledger_reads_a_sessions_account_moves_back	account-ledger-readback	
src/supervise/turn_end_loop_tests.rs	the_login_expired_row_gets_login_once_and_the_login_back_is_continued	login-wall-acts	
src/supervise/turn_end_loop_tests.rs	the_login_success_gets_one_fenced_enter	login-wall-acts	
src/supervise/turn_end_loop_tests.rs	the_loop_holds_its_own_turns_on_usage_credits_until_the_notices_reset	credits-hold	
src/supervise/turn_end_loop_tests.rs	the_loop_keeps_a_reset_ahead_and_a_models_credits_where_the_screen_draws_none	credits-hold	
src/supervise/turn_end_loop_tests.rs	the_loop_reads_the_wall_of_a_tall_panes_short_transcript	live-zone-read	
src/supervise/turn_end_loop_tests.rs	the_loop_says_once_why_a_due_probe_was_not_run	usage-probe-gates	
src/supervise/turn_end_loop_tests.rs	the_no_break_space_composer_takes_the_fenced_continuation	act-write-protocol	
src/supervise/turn_end_loop_tests.rs	the_owners_clearing_continues_a_session_at_its_wall_once	account-move-execution	
src/supervise/turn_end_loop_tests.rs	the_dialogs_cancel_is_one_fenced_esc	login-wall-acts	
src/supervise/turn_end_loop_tests.rs	the_picker_of_its_own_login_gets_the_method_the_login_is	login-wall-acts	
src/supervise/turn_end_loop_tests.rs	the_picker_of_its_own_login_over_a_roster_row_is_backed_out_of	login-wall-acts	login-notices
src/supervise/turn_end_loop_tests.rs	a_row_named_under_its_own_handoff_backs_out_of_it	login-wall-acts	login-notices
src/supervise/turn_end_loop_tests.rs	a_row_named_under_its_own_picker_backs_out_of_it	login-wall-acts	login-notices
src/supervise/turn_end_loop_tests.rs	a_picker_still_drawn_after_aterms_esc_gets_the_esc_again_after_its_pause	login-wall-acts	login-notices
src/supervise/turn_end_loop_tests.rs	the_carrier_delivers_its_turn_again_where_the_login_came_back_elsewhere	login-wall-acts	login-notices
src/supervise/turn_end_loop_tests.rs	a_login_back_already_delivered_is_not_backed_out_of_at_the_picker	login-wall-acts	
src/supervise/turn_end_loop_tests.rs	a_lost_login_whose_row_is_discovered_types_no_login	login-wall-acts	login-lifecycle-accounts
src/supervise/turn_end_loop_tests.rs	the_plan_is_cleared_when_the_try_is_typed_not_at_the_next_point	wait-told	
src/supervise/turn_end_loop_tests.rs	the_point_read_again_after_an_account_relaunch_tells_nothing	account-move-answer	
src/supervise/turn_end_loop_tests.rs	the_probe_is_not_typed_again_over_its_own_text_left_typed	usage-probe-left-typed	
src/supervise/turn_end_loop_tests.rs	the_renewal_edge_keeps_what_the_decision_read	login-lifecycle-accounts	
src/supervise/turn_end_loop_tests.rs	the_sign_ins_notice_outlasts_its_dialogs_busy_reads	login-notices	
src/supervise/turn_end_loop_tests.rs	the_stamp_a_loop_first_reads_is_no_news_to_its_latch	account-ledger-readback	
src/supervise/turn_end_loop_tests.rs	the_typing_budget_counts_an_earlier_loops_acts_from_the_ledger	typing-budget	
src/supervise/turn_end_loop_tests.rs	the_wait_on_a_left_limit_wakes_on_a_persons_short_turn	left-limit-tracking	
src/supervise/turn_end_loop_tests.rs	the_wait_places_a_reset_across_a_dst_change_with_the_offset_there	limit-wall-placement	
src/supervise/turn_end_loop_tests.rs	the_wall_cache_is_dropped_when_the_worker_has_worked	usage-wall-wait	
src/supervise/turn_end_loop_tests.rs	the_wall_waited_on_is_written_to_the_ledger_and_its_end_too	left-limit-tracking	
src/supervise/turn_end_loop_tests.rs	the_workers_suggestion_is_accepted_with_the_accept_key_and_a_fenced_enter	act-write-protocol	
src/supervise/turn_end_loop_tests.rs	tier1_the_real_loop_remembers_a_hidden_wall_exactly_where_the_model_does	left-limit-tracking	
src/supervise/turn_end_loop_tests.rs	under_accounts_off_nothing_is_told_or_asked	account-advice-asks	
src/supervise/turn_end_loop_tests.rs	usage_left_typed_is_a_record_that_raises_nothing_and_the_panel_stays_owed	usage-probe-left-typed	
src/supervise/turn_end_loop_tests.rs	usage_left_typed_is_sent_as_the_probes_own_and_its_panel_is_closed	usage-probe-left-typed	
src/supervise/wall_confirm_loop_tests.rs	a_cancelled_limit_answers_the_act_only_by_a_notice_written_since_it	left-limit-tracking	
src/supervise/wall_confirm_loop_tests.rs	a_left_limits_wait_ends_on_its_own_rows_alone	left-limit-tracking	
src/supervise/wall_confirm_loop_tests.rs	a_limit_still_stands_after_a_persons_local_commands	wall-confirm-gate	
src/supervise/wall_confirm_loop_tests.rs	a_persons_turn_the_limit_answers_cancelled_before_any_read_is_read_from_the_transcript	left-limit-tracking	
src/supervise/wall_confirm_loop_tests.rs	a_persons_turn_the_transcript_holds_ends_a_left_limits_wait	left-limit-tracking	
src/supervise/wall_confirm_loop_tests.rs	a_point_decided_again_on_an_unmoved_screen_asks_the_transcript_again	wall-confirm-gate	
src/supervise/wall_confirm_loop_tests.rs	a_refused_limit_decided_again_is_an_idle_point_and_said_once	wall-confirm-gate	
src/supervise/wall_confirm_loop_tests.rs	a_refused_wall_is_said_again_once_it_went_away	wall-confirm-gate	
src/supervise/wall_confirm_loop_tests.rs	a_refused_wall_met_again_after_a_live_turn_is_said_again	wall-confirm-gate	
src/supervise/wall_confirm_loop_tests.rs	a_transcript_that_lags_the_screen_is_read_again	wall-confirm-gate	
src/supervise/wall_confirm_loop_tests.rs	a_wall_in_a_tools_output_the_transcript_refuses_is_no_wall	wall-confirm-gate	
src/supervise/wall_confirm_loop_tests.rs	a_wall_the_transcript_confirms_is_acted_on	wall-confirm-gate	
src/supervise/wall_confirm_loop_tests.rs	a_wall_the_transcript_names_otherwise_is_refused	wall-confirm-gate	
src/supervise/wall_confirm_loop_tests.rs	a_wall_whose_call_is_above_the_rows_read_is_judged_on_the_whole_screen	wall-confirm-gate	
src/supervise/wall_confirm_loop_tests.rs	a_whole_read_that_fails_or_reads_another_wall_counts_as_under_a_call	wall-confirm-gate	
src/supervise/wall_confirm_loop_tests.rs	an_act_over_a_cancelled_limit_answered_by_the_same_reset_is_read_from_the_transcript	left-limit-tracking	
src/supervise/wall_confirm_loop_tests.rs	the_books_read_after_an_act_asks_the_transcript_nothing	wall-confirm-gate	
src/supervise/wall_confirm_loop_tests.rs	the_seam_spares_the_banner_and_a_codex_and_reads_a_refused_limit_on_its_frame	wall-confirm-gate	
src/supervise/wall_confirm_loop_tests.rs	with_no_transcript_a_hooks_words_under_its_own_rows_are_not_acted_on	wall-confirm-gate	
src/supervise/wall_confirm_loop_tests.rs	with_no_transcript_a_programs_words_behind_a_vendor_label_are_not_acted_on	wall-confirm-gate	
src/supervise/wall_confirm_loop_tests.rs	with_no_transcript_a_wall_under_a_tools_call_is_not_acted_on	wall-confirm-gate	
src/supervise/wall_confirm_tests.rs	a_local_commands_rows_are_no_turn_and_no_notice	transcript-ends-on	
src/supervise/wall_confirm_tests.rs	a_notice_the_table_does_not_name_is_classed_by_its_error_word	transcript-confirms-class	
src/supervise/wall_confirm_tests.rs	a_tools_output_confirms_no_wall	transcript-confirms-class	
src/supervise/wall_confirm_tests.rs	a_transcript_cut_before_its_notice_is_lagging	transcript-lagging	
src/supervise/wall_confirm_tests.rs	every_wall_claude_code_writes_confirms_its_own_kind_and_no_other	transcript-confirms-class	
src/supervise/wall_confirm_tests.rs	only_the_conversations_own_entries_end_a_notice	transcript-ends-on	
src/supervise/wall_confirm_tests.rs	the_files_order_decides_but_for_the_conversations_first_row	transcript-ends-on	
tests/conformance_footer_model.rs	the_bind_catches_each_old_reader	footer-model-refines-ClaudeFooterModel	
tests/conformance_footer_model.rs	the_footer_names_the_running_model_from_every_state_within_five_actions	footer-model-refines-ClaudeFooterModel	
tests/conformance_login_wall.rs	a_person_told_to_sign_a_roster_row_in_breaks_its_law	login-wall-refines-HarnessLoginWall	
tests/conformance_login_wall.rs	a_person_told_to_close_aterms_dialog_breaks_its_law	login-wall-refines-HarnessLoginWall	
tests/conformance_login_wall.rs	a_roster_rows_sign_in_menu_conforms_to_the_login_wall_model	login-wall-refines-HarnessLoginWall	
tests/conformance_login_wall.rs	the_real_code_conforms_to_the_login_wall_model	login-wall-refines-HarnessLoginWall	
tests/conformance_upgrade_models/ladder.rs	the_incidents_cold_only_rule_is_caught_by_the_bind	upgrade-model-ladder-refines-model	
tests/conformance_upgrade_models/ladder.rs	the_real_ladder_is_the_models_ladder_at_every_reachable_state	upgrade-model-ladder-refines-model	
tests/conformance_upgrade_models/priority.rs	upgrade_priority_binds_catch_a_pre_fix_writer_and_a_head_reader	upgrade-model-priority-refines-model	
tests/conformance_upgrade_models/priority.rs	upgrade_priority_ingest_admits_only_what_the_model_admits	upgrade-model-priority-refines-model	
tests/conformance_upgrade_models/priority.rs	upgrade_priority_writers_and_reader_are_the_models_on_every_reachable_state	upgrade-model-priority-refines-model	
tests/conformance_upgrade_models/switch.rs	upgrade_model_switch_binds_catch_every_pre_fix_shape	upgrade-model-switch-refines-model	
tests/conformance_upgrade_models/switch.rs	upgrade_model_switch_is_the_models_on_every_reachable_state	upgrade-model-switch-refines-model	
tests/conformance_upgrade_models/switch.rs	upgrade_model_switch_refuses_the_opposite_decision	upgrade-model-switch-refines-model	
tests/conformance_usage_tap.rs	the_real_tap_takes_the_models_steps_on_every_reachable_state	usage-tap-refines-HarnessUsageTap	
tests/footer_window_allocations.rs	each_window_the_footer_walks_back_is_allocated_once	footer-window-allocation	
tests/supervise_conformance_account_rotation.rs	a_canary_that_never_answers_is_replaced_by_the_next_session_after_its_hold	account-rotation-refines-model	
tests/supervise_conformance_account_rotation.rs	a_real_history_from_an_empty_state_stays_in_step_with_the_model	account-rotation-refines-model	
tests/supervise_conformance_account_rotation.rs	a_strike_goes_back_to_an_account_that_never_served_as_unproven	account-rotation-refines-model	
tests/supervise_conformance_account_rotation.rs	six_rotations_in_an_hour_are_the_bound_and_the_seventh_wall_is_waited_out	account-rotation-refines-model	
tests/supervise_conformance_account_rotation.rs	the_defects_and_todays_rule_disagree_with_the_real_code	account-rotation-refines-model	
tests/supervise_conformance_account_rotation.rs	the_first_turn_under_the_new_login_is_the_models_served_or_its_strike	account-rotation-refines-model	
tests/supervise_conformance_account_rotation.rs	the_sessions_own_bounds_part_from_the_model_only_toward_todays_rule	account-rotation-refines-model	
tests/supervise_conformance_account_rotation.rs	tier1_the_real_advice_and_transactions_are_the_models_at_every_reachable_state	account-rotation-refines-model	
tests/supervise_conformance_condition_resume.rs	each_buggy_resume_breaks_its_law	turn-end-refines-SupervisorConditionResume	
tests/supervise_conformance_condition_resume.rs	tier1_a_loop_that_ignores_the_person_or_the_wall_disagrees_where_the_model_holds	turn-end-refines-SupervisorConditionResume	
tests/supervise_conformance_condition_resume.rs	tier1_the_real_decider_resumes_past_a_clearing_exactly_where_the_model_says	turn-end-refines-SupervisorConditionResume	
tests/supervise_conformance_credits_hold.rs	tier1_a_decider_that_ignores_the_hold_disagrees_where_the_model_holds	turn-end-refines-SupervisorCreditsHold	
tests/supervise_conformance_credits_hold.rs	tier1_the_real_decider_holds_usage_credits_exactly_where_the_model_says	turn-end-refines-SupervisorCreditsHold	
tests/supervise_conformance_cruise.rs	tier1_the_buggy_model_is_refuted_by_the_real_decider	turn-end-refines-SupervisorCruise	
tests/supervise_conformance_cruise.rs	tier1_the_real_decider_defends_every_claim_and_never_rests	turn-end-refines-SupervisorCruise	
tests/supervise_conformance_land.rs	tier1_the_buggy_model_and_three_tellers_are_refuted	turn-end-refines-SupervisorLand	
tests/supervise_conformance_land.rs	tier1_the_real_decider_lands_only_on_a_clean_defense_and_never_goes_quiet	turn-end-refines-SupervisorLand	
tests/supervise_conformance_model_switch.rs	the_guessed_return_and_the_old_half_hour_disagree_with_the_real_code	model-switch-carry	
tests/supervise_conformance_model_switch.rs	tier1_the_real_decider_switches_and_returns_exactly_where_the_model_allows	model-switch-carry	
tests/supervise_conformance_outage_wait.rs	a_ladder_with_no_doubling_or_no_cap_or_a_second_resume_disagrees_with_the_real_decider	turn-end-refines-SupervisorOutageWait	
tests/supervise_conformance_outage_wait.rs	each_buggy_dial_breaks_its_law_along_a_path_the_model_keeps_it_on	turn-end-refines-SupervisorOutageWait	
tests/supervise_conformance_outage_wait.rs	the_models_words_read_as_the_walls_they_stand_for	turn-end-refines-SupervisorOutageWait	
tests/supervise_conformance_outage_wait.rs	tier1_the_real_decider_waits_out_an_outage_exactly_as_the_model_says	turn-end-refines-SupervisorOutageWait	
tests/supervise_conformance_outage_wait.rs	todays_typed_retry_disagrees_with_the_real_decider	turn-end-refines-SupervisorOutageWait	
tests/supervise_conformance_turn_end.rs	tier1_the_real_decider_types_exactly_where_the_model_allows	turn-end-refines-SupervisorTurnEnd	
tests/supervise_conformance_usage_wall.rs	the_loop_that_ties_no_continuation_to_its_limit_ends_the_wait_on_a_resume_at_an_armed_wall	turn-end-refines-SupervisorUsageWall	
tests/supervise_conformance_usage_wall.rs	tier1_a_limit_met_inside_the_own_acts_take_window_is_tracked_where_the_model_says	turn-end-refines-SupervisorUsageWall	
tests/supervise_conformance_usage_wall.rs	the_loop_without_the_tap_types_into_a_cut_wall_on_its_back_off	turn-end-refines-SupervisorUsageWall	
tests/supervise_conformance_usage_wall.rs	tier1_the_real_reader_and_decider_continue_a_usage_wall_exactly_where_the_model_allows	turn-end-refines-SupervisorUsageWall	
tests/supervise_conformance_usage_wall.rs	tier1_the_tap_times_a_cut_wall_exactly_where_the_model_acts	turn-end-refines-SupervisorUsageWall	tap-clock-step-says
tests/transcript_allocations.rs	transcript_readers_keep_allocation_proportional_to_input	§4	
tests/vendor_corpus.rs	every_captured_permission_request_is_pressed_at_full_power	approval-full-power-presses-every-bash-box	
tests/vendor_corpus.rs	the_painted_usage_panel_yields_its_windows	usage-panel-windows	
src/supervise/woken_turn_loop_tests.rs	a_turn_the_mac_slept_through_in_the_vendors_retry_is_pressed_esc_once_the_api_is_up	woken-turn-esc	
src/supervise/woken_turn_loop_tests.rs	a_woken_turn_is_pressed_nothing_where_any_condition_fails	woken-turn-esc	
src/supervise/woken_turn_loop_tests.rs	a_draft_in_the_composer_is_never_pressed_away	woken-turn-esc	
src/supervise/woken_turn_loop_tests.rs	the_vendors_own_waits_and_a_short_countdown_are_pressed_nothing	woken-turn-esc	
src/supervise/woken_turn_loop_tests.rs	a_no_response_try_that_crossed_the_sleep_is_cut_once_the_api_is_up	woken-turn-esc	
src/supervise/woken_turn_loop_tests.rs	a_no_response_retry_first_read_since_the_wake_is_left_to_the_vendors_wait	woken-turn-esc	
src/supervise/woken_turn_loop_tests.rs	claude_code_2_1_310s_measured_network_retry_rows_are_pressed	woken-turn-esc	
src/supervise/woken_turn_loop_tests.rs	a_skipped_or_yielded_press_leaves_the_wakes_esc_for_a_later_read	woken-turn-esc	
src/supervise/woken_turn_loop_tests.rs	the_wakes_one_esc_is_never_pressed_twice	woken-turn-esc	
src/supervise/woken_turn_loop_tests.rs	the_point_the_harnesss_esc_stopped_is_no_persons_esc	woken-turn-esc	sleep-wake-settle
src/supervise/woken_turn_loop_tests.rs	the_vendors_continuation_the_harnesss_esc_stopped_gets_the_limits_resume	woken-turn-esc	
src/supervise/woken_turn_loop_tests.rs	only_claude_codes_retry_row_is_pressed	woken-turn-esc	
src/supervise/woken_turn_loop_tests.rs	the_harnesss_mark_lasts_only_until_claude_code_could_take_the_key	woken-turn-esc	
```

## 3. Coverage

- 2840 `#[test]` functions in the crate; 2758 read by the chunks; 82 in six files read by no chunk:
  - `src/harness/accounts_logins_tests.rs` (15)
  - `src/harness/cli_renew_tests.rs` (17)
  - `src/harness/open_url_tests.rs` (3)
  - `src/harness/stats_tests.rs` (17)
  - `src/supervise/autopilot_loop_tests.rs` (28)
  - `src/supervise/remembered_limit_tests.rs` (2)
  These six files need their own recovery pass before the crate's tests can all be retired.
- Of the 2758 read: 2359 map to one of the 635 specs as primary; 363 map to the 80 pending chunk-6
  groups (§1b); 36 are in §4 (becomes-spec-after-fix: 4, delete: 15, move-to-bench: 14, re-check: 3).
- The largest collapses (primary tests): `classify-sound` (40), `visit-shell-refines-pure-reducer` (28), `claude-home-resolution` (26), `left-limit-tracking` (25), `next-step-safety` (24), `codex-visit-exit-gate` (23), `question-answer-refines-model` (22).
- Many loop-level specs (`no-act-without-ownership-proofs`, `visit-shell-refines-pure-reducer`,
  `codex-visit-exit-gate`, the reset and turn-end loop specs) retire dozens of end-to-end tests each, but only once
  their pure kernels below them are proved and gap 8 (effects) and gap 10 (model→code refinement) close.
- Specs with an empty test count (for example `prelex`, `numeric-format`)
  are implied by tests another spec retires; they are lemmas of those specs.

### Gap histogram (audit §7 row → number of the 624 specs it blocks)

Gap 0 (the 2026-09-17 seal) blocks every row. Specs blocked by gap 0 alone: `disk-age-days`, `model-ladder-monotone-bounded`.

Measured 2026-10-16 on seal `150efe4a`: gap 0 is closed, but neither of those two specs can be stated yet. Their tests stay.

- **Strict lane refuses the whole crate before verifying anything.** `targo trust check -p aterm-agent` says:
  `verified Targo refuses unit aterm-agent ... in-process proc-macro dependencies [aterm-serde-derive ...] share rustc's
  argv, memory, stderr, and artifact filesystem and can forge compiler-message/TRUSTJSON proof transport ...
  Evidence-grade verification currently enforces a no-proc-macro TCB boundary`. That is a new crate-wide gap: no row
  in this crate can reach `Certified` until the `#[derive(Serialize, Deserialize)]` uses are expanded or removed, or
  Trust isolates proc macros.
- **Advisory lane (`--allow-l0-gaps`, diagnostic only).** No `trust_import_*` is minted for `harness::disk::age_days`,
  under either `trust_import_aterm_agent__age_days` or `trust_import_aterm_agent__harness__disk__age_days`. The island
  is refused with `AutoBoundHeadOverApplied { name: "...age_days", ..., remaining_args: 2 }`, which means E6 does not admit
  the body (`saturating_sub`, `Ord::max`, a signed `/`). A method-call `ensures` (`result == now.saturating_sub(then).max(0) / 86400`)
  is not lowered either (`SpecEnsuresUnparseable`). `harness::upgrade_models::model_moves_now` returns `Option<&'static str>`,
  and no import is minted for it (`AutoBoundHeadOverApplied` on
  `trust_import_aterm_agent__harness__upgrade_models__model_moves_now`). That is the gap-1 `&str`/enum carrier gap.
  `model_to` takes `&ModelVerdict` and `Option<&str>` and returns `Option<String>`, so it is gap 1 as well.
- **Even an admitted body would not reach `Certified` here.** Every fn in the crate also carries
  `UnmodeledThreadSpawnEffect`: Trust's panic path reaches `harness::align::capture_bounded_impl` → `std::thread::spawn`.
  It also carries the `GlobalAlloc` refusal (`aterm_objc` holds `global_asm!`).

| gap | specs blocked |
|---|---|
| 1 | 565 |
| 2 | 201 |
| 3 | 408 |
| 4 | 95 |
| 5 | 201 |
| 6 | 3 |
| 7 | 330 |
| 8 | 254 |
| 9 | 1 |
| 10 | 88 |
| 11 | 97 |
| 12 | 108 |
| 13 | 83 |
| 14 | 4 |
| 16 | 1 |
| 17 | 1 |

The chunk-6 groups are not in the histogram: their gaps were not delivered.

## 4. Tests that are not spec samples

Status 2026-10-16: deleted `federation_queue_is_bounded_and_backpressures_at_capacity`, `token_path_uses_the_shared_convention_not_a_stem_swap`, `a_fake_instance_stops_when_its_request_log_is_dropped`, `two_callers_of_one_tag_never_share_a_scratch_path`, `a_kinds_reason_takes_its_article` and `one_placed_reset_stamps_as_its_own_second_across_a_turning_second`; the other rows stand.

Status 2026-10-17: the rows of those six are gone, and so is the **move-to-bench** row of `the_idle_wait_wakes_on_the_servers_verdict_not_on_every_repaint`, which the audit deleted as a timing-window test. The other rows stand.

| test | why | action |
|---|---|---|
| `src/harness/align_tests.rs::escaped_holder_stage` | Not a test: the fixture stage the escape test re-executes as its child. | **delete (move to a dev-only bin stage until align-capture-lifecycle-refines-model is certified)** |
| `src/harness/lights.rs::every_recorded_screen_reads_definitely_or_not_at_all` | Drift canary over a vendor fixture directory, not a universal property. | **move-to-bench** |
| `src/harness/lights.rs::every_captured_footer_screen_of_every_build_is_read` | Vendor-build drift canary over claude-<v>-footer-*.txt. | **move-to-bench** |
| `src/harness/footer_tests.rs::every_recorded_screen_has_a_rule_the_footer_can_write` | Corpus check over aterm-phase's fixtures; the general halves are footer-rule-run and footer-fit-rule-ladder. | **move-to-bench** |
| `src/harness/upgrade_drive_tests.rs::park_when_asked` | Not a test: the parked stand-in agent's body, re-executed under PARK_ENV. | **move-to-bench** |
| `src/supervise/policy/claude_settings_tests.rs::provider_home_stage` | Chunk 6 grouping marked it non-spec (a fixture stage); reason not delivered. | **re-check (chunk 6)** |
| `src/supervise/run_engine_tests.rs::park_as_worker` | Not a test: the StandIn subprocess body. | **move-to-bench** |
| `src/supervise/run_tests.rs::a_span_is_read_once_and_the_continuations_back_off_ten_then_thirty` | Samples limit-episode but carries a 500 ms real-clock latency budget. | **becomes-spec-after-fix** |
| `src/supervise/run_tests.rs::a_late_vendor_timer_takes_the_0_inside_the_landing_time_and_is_never_backspaced` | Outcome depends on real-time scale; restate on a clock model (survey-zero-timing). | **becomes-spec-after-fix** |
| `src/supervise/run_tests.rs::a_refused_backspace_keeps_the_0_owed_until_the_server_takes_it` | Its busy-sink row is vacuous (OR'ed assertion); fix the row, then stray-digit-cleanup covers it. | **becomes-spec-after-fix** |
| `tests/transcript_allocations.rs::transcript_readers_keep_allocation_proportional_to_input` | Allocation-budget benchmark. | **move-to-bench** |

## 5. Implied specs that look wrong, or disagree with the code

1. Fixed 2026-10-16: `event_record`/`err_record` escape addressing fields as well as bodies; `federated_addresses_cannot_break_ndjson` covers peer-provided channel identities.
5. Fixed 2026-10-22: `src/harness/accounts_tests.rs::the_signed_out_labels_are_the_enrolled_ones_in_roster_order` samples `signed_out` excluding rows with an email.
6. Several cli and disk tests date fixtures against the real wall clock (`now_after_write`, `idle_now`), contrary to the module docs' clock-independence claim.
7. Fixed 2026-10-22: `src/harness/disk_target_tests.rs::Parked` forks directly via `libc::fork` + `libc::_exit` without a bare `std::process::Command` or `pre_exec` error pipe.
8. `src/harness/footer_tests.rs::the_tail_takes_the_newest_main_thread_model_and_effort`: scan() decides effort from any non-sidechain row with an `effort` key, contradicting TailFacts::effort's doc; the fixture cannot tell which is intended.
10. `src/harness/transcript_corpus_tests.rs::transcript_projections_answer_as_recorded`: its Err halves pin aterm_json's Display text (prose pinning).
11. `src/harness/footer_tests.rs::tail_cache_reuses_only_one_process_and_one_unchanged_file` sleeps 20 ms with no effect on what is asserted; drop the sleep.
13. Chunk 3's manifest said 332 tests; the files hold 333 `#[test]` (305+9+3+5+11). The extra is `park_when_asked`, a stand-in process body.
14. `src/harness/upgrade_drive_tests.rs::who_roster_exposes_a_unique_foreground_group_without_a_window_hop`: parse_host_roster never checks the `OK <n>` count against the rows.
16. `src/harness/upgrade_drive_tests.rs::the_notice_names_what_runs_under_the_agent_by_its_command` mixes the pure parse_etime/command_head contracts, the background walk and pinned gave_up_words prose.
17. Many end-to-end upgrade_drive tests pin ledger prose as well as step words (e.g. a_drain_a_person_holds_past_its_bound_voids_the_answer_and_asks_again, a_visits_own_retarget_is_said_once_in_the_ledger); the spec is the step word and the row count.
18. Tests calling `seed_builds` read the developer machine's installed Claude Code via atpkg, so their result varies by machine.
20. `src/harness/upgrade_drive_tests.rs::the_real_classifier_conforms_to_the_upgrade_look_model`: the aterm-gui half of the bind fakes `due` from the model's `readable` bit, so the classifier rests on this test alone.
22. `src/harness/upgrade_drive_tests.rs::a_declining_agent_is_asked_a_bounded_number_of_times_a_day` claims a mutation-checked negative control that the test does not contain.
23. `src/harness/upgrade_wake.rs::marker_wakes_a_parked_host_and_unrelated_writes_do_not`: its negative controls are timing-dependent (50 ms sleeps, 150 ms recv_timeout), against AGENTS.md's one-minute hang-detector rule.
24. `src/harness/upgrade_wake.rs::{the_trigger_ends_a_parked_wait_at_once,first_install_rechecks_a_missing_path_after_a_lost_ancestor_notification,an_exit_is_seen_as_it_happens}` tell a push from the fallback by elapsed-time ratios.
26. `src/harness/upgrade_tests.rs::a_line_the_tty_could_cut_is_refused` asserts two definitional equalities (MAX_LINE and HOST).
27. `src/harness/upgrade_tests.rs::every_short_argv_resumes_with_exactly_the_values_claude_parsed`: the oracle is a test-only transcription of Commander 2.1.281; it must become the Clean reference model and nothing ties it to the shipping bundle.
28. `src/harness/upgrade_status_tests.rs::a_token_word_reads_as_the_cut_reads_it`: the oracle is a copy of word()'s own slow path.
29. `src/harness/upgrade_progress_tests.rs`: the header says four tests; the file holds five.
30. `src/harness/upgrade_status_tests.rs::the_awake_clock_is_the_machines_last_wake_or_boot` reads the live machine and partly tests a cfg(test) seam.
31. The `baked` fixture in upgrade_models.rs sets native_1m for every model, so model_of_display's [1m]-append branch is never sampled.
32. `src/harness/upgrade_queued_tests.rs` fixture `limit_row` uses max(1791046800, now+86400), so the exercised case changes once the real date passes the recorded reset.
33. `src/supervise/answer.rs::the_box_token_names_one_screen` asserts pairwise distinctness of a 32-bit folded hash, which is not a universal property; the spec claims only determinism.
34. `src/supervise/config.rs::a_value_it_cannot_read_keeps_its_default_and_only_an_off_limits`: `"off"` for continue_per_hour and human_grace_s changes nothing, contradicting the doc's 'an off is still the limit'; state the numeric-key exemption or fix the code.
36. `src/supervise/limit.rs::the_local_zone_is_the_c_librarys_and_no_zone_is_made_up` depends on the runner's zone (a bound, not a value); its `date +%z` oracle is `aterm_uds::zone`'s.
37. `src/supervise/ledger_tests.rs::without_a_clock_*` (two tests) are near-duplicates over the same branch.
39. `src/supervise/approvals.rs::a_row_carries_the_hash_of_the_whole_command_and_a_cut_copy` includes a sha256("abc") known-vector check of the sha2 dependency.
40. `src/supervise/ledger_html.rs` and the HTML ledger test rely on cfg(test) checkers (is_self_contained, well_formed); the spec they imply is checked only by test code.
41. Chunk 6 (`src/supervise/policy/*`, 366 tests: turn_end_tests 212, rm_breaker_tests 47, git_config_tests 27, question_tests 23, claude_settings_tests 16, question_text_tests 12, shell_startup_tests 10, exception_tests 8, guard.rs 6, reset.rs 3, outlook_tests 2) returned no specs. Its 80 test groups are listed as `pending` rows; their statements, functions and gaps must be recovered again.
42. `src/supervise/reset_loop_tests.rs` (whole file): every screen is composed from the 2.1.304 bundle's text (2.1.287's until 2026-10-19); nothing has been seen live, so the reset specs pin a reading of the bundle.
43. Many reset_loop tests name a 'NEGATIVE CONTROL (seen by mutation)' that is not executed; each mutant must be checked against the spec explicitly.
44. `src/supervise/reset_loop_tests.rs::a_deadline_is_placed_in_the_wall_notices_zone_where_the_loop_holds_one`: without a lookup, a January date gets September's PDT offset (16:30Z instead of 17:30Z); documented as intended but wrong about wall-clock reality.
46. `src/supervise/run_engine_tests.rs::a_read_box_under_a_ticking_screen_is_approved_within_one_wake` claims latency but asserts an exact ten-request trace.
47. Several run_box_key tests replay scripts the tier1 bind already walks; they add only badge/escalated/cleared counts.
48. The reset successor tests assert ≥ 1 s real-time graces; not flaky, but tie the spec to real sleeps instead of an injected clock.
49. `src/supervise/run_engine_tests.rs::a_question_asked_a_third_time_is_answered_again_after_a_pause` mixes a loop scenario with a pure reask_pause check that belongs on doubling-backoff.
51. `src/supervise/stall.rs::rss_text` computes `(mb*10+512)/1024` on a server-supplied u64: it overflows for huge mb; a no-panic spec fails until it saturates or widens.
52. `src/supervise/run_tests.rs::guard_errors_are_retried_or_surfaced_never_approved`: `(2..=4).contains(&presses)` under a 1.2 s budget is timing-dependent; so are the survey settle/landing tests.
53. `src/supervise/run_tests.rs::a_box_the_loop_does_not_approve_is_escalated_once_and_cleared_when_it_goes` says the badge is read before unset; a_standing_approval_attention_survives_the_episode_close asserts nothing is read first; the first doc looks stale.
55. `src/supervise/transport.rs::the_interrupter_ends_a_parked_request` builds its socket dir from temp_dir()+pid instead of the crate's ScratchDir and leaks it on failure.
56. Chunk 9's manifest said 270 tests; its 22 files hold 278 (`tests/agent_suite.rs` holds none: its one match is in a //! comment).
57. `src/supervise/turn_end_loop_tests.rs::a_sessions_jitter_is_stable_bounded_and_spread`: the 'spread over ≥ 8 seconds' assertions sample FNV dispersion, not the function's spec.
60. Many turn_end_loop tests assert exact journal/badge strings; that prose goes when the spec replaces them.
61. `tests/transcript_allocations.rs`: named proportional, mostly asserts absolute caps.
63. `tests/conformance_footer_model.rs` walks only states within 5 actions of the start; the spec is over all of them.
64. Conformance negative-control tests (Buggy knobs, old-reader reconstructions) stay under their refinement spec as its mutation control, not as specs.
65. `src/supervise/turn_end_loop_tests.rs::tier1_the_real_loop_remembers_a_hidden_wall_exactly_where_the_model_does` is a Tier-1 bind living in the unit-test file instead of tests/.

## 6. Proof order (easiest high-value first)

### 6.1 Blocked by gap 0 alone (start here)

1. **`disk-age-days`**: `max(0, now−t) div 86400` on `i64`. Straight-line, no loop, no carrier. Retires
   `age_days_never_reads_a_future_mtime_as_old` and is the age comparison under the whole disk tick.
2. **`model-ladder-monotone-bounded`**: `model_moves_now` / `model_to` are branches over two bools and a
   `u64`. Proves monotonicity in the wait and that every due move lands, and is the core that
   `upgrade-model-ladder-refines-model` (gaps 3, 10) only re-states against its model.

### 6.2 Pure arithmetic and tables next (gaps 1 and/or 3 only)

3. **Scalar clocks, gap 3 only**: `acc-wall-stands-until`, `acc-sample-windows`, `acc-probe-interval`,
   `footer-floor-of`, `relaunch-held-wait-table`. Saturating `i64`/`u64` arithmetic with small array indexes;
   together they decide when an account is limited, low or due a probe, which the advice specs rest on.
4. **Finite tables decidable by evaluation (gap 1)**: `acc-words-table`, `acc-wakes-table`, `disk-class-table`,
   `disk-free-warn`. Each is an enum ↔ word table; once enum/`&str` carriers land
   (gap 1) they are discharged by enumeration.
5. **Small state machines (gaps 2, 3)**: `lights-state-and-fold`, `tap-freshness`, `netstate-schedule`,
   `netstate-reach-freshness`, `tap-silence-machine`. `&mut self` on small structs with `Instant`/`u64`
   arithmetic; they are the first users of certified `&mut self` views (gap 2).
6. **The decision cores of the big loops (gaps 1, 3)**: `ladder-clamp`, `doubling-backoff`,
   `memory-ceiling-formula`, `memory-ceiling-order`, `acc-condition-table`, `acc-advise-rule1-guards`,
   `step-word-classification`, `held-by-status`. Short pure functions over strings and enums, each the lemma
   under many end-to-end tests.
7. **Then the refinements whose models already exist (gaps 3, 10)**:
   `upgrade-model-ladder-refines-model`, `reducer-refines-drain-bound-model`, `reducer-refines-work-end-model`.
   Pure reducers bound to `ty_model!` machines; the conformance tests are already the projection.

Prove the chunk-6 groups' statements (§1b) before ranking them; `te-*`, `rm-*` and `git-*` groups sit under
the turn-end, rm-breaker and git-read specs in §1 and are likely high value.

### 6.3 Everything else, by blocking-gap set (fewest first)

- **0 only** (2): `disk-age-days`, `model-ladder-monotone-bounded`
- **1** (4): `acc-words-table`, `acc-wakes-table`, `disk-free-warn`, `disk-class-table`
- **3** (6): `acc-wall-stands-until`, `acc-sample-windows`, `acc-probe-interval`, `footer-floor-of`, `relaunch-held-wait-table`, `harness-timings-orders`
- **8** (7): `drive-own-ctl`, `open-regular-nonblocking`, `kernel-start-spelling`, `awake-clock-probe`, `queue-lock-and-epoch-fence`, `queue-fs-hardening`, `upgrade-dry-run-no-hands`
- **1,2** (3): `acc-probe-claim-spacing`, `acc-canary-hold-take`, `screen-cursor-index`
- **1,3** (34): `acc-condition-table`, `acc-advise-rule1-guards`, `acc-advise-resume`, `acc-judged-and-footer-label`, `cli-parse-defer`, `disk-human-bytes`, `relaunch-resumed-prompt-one-line`, `relaunch-quiet-outcome`, `relaunch-account-login`, `relaunch-cause-classes`, `relaunch-carried-word`, `usage-tokens-short`, `move-why-from-ids`, `ride-bar`, `live-model-at-launch-until-answer`, `session-file-parse`, `quiet-step-contract`, `harness-prompts-shape`, `owner-words-roundtrip`, `wait-class-total-and-stable`, `row-of-record`, `person-hold-footer-note`, `proc-stat-exited`, `json-number-readers`, `usage-view-shown`, `ctl-failure-words`, `ladder-clamp`, `doubling-backoff`, `ledger-clock-words`, `memory-ceiling-formula`, `memory-ceiling-order`, `approval-breaker-naming`, `transcript-confirms-class`, `transcript-lagging`
- **1,4** (1): `continuation-words-and-mode`
- **1,5** (1): `ledger-html-assets`
- **1,7** (2): `acc-label-grammar`, `acc-row-for-login-exact`
- **1,8** (15): `acc-roster-file-read`, `acc-enrolled`, `acc-store-load-save`, `acc-profile-configs-dir`, `acc-roster-beside`, `cli-upgrade-sweep-gates`, `restart-stamp-file`, `restart-stamp-lookup-prune`, `restart-stamp-consume`, `tap-verb-total`, `tap-read-own`, `forget-settled-pending`, `ready-consent`, `config-from-path-and-hosted`, `relay-read-only-connection`
- **2,3** (2): `lights-state-and-fold`, `tap-freshness`
- **2,4** (1): `self-claimed-relaunch-route`
- **2,11** (1): `moved-rule`
- **3,7** (1): `lights-mode-pill-table`
- **3,8** (2): `job-classification`, `relaunch-by-parent`
- **3,10** (1): `upgrade-model-ladder-refines-model`
- **3,12** (1): `lanes-table`
- **7,8** (2): `disk-tree-nofollow`, `disk-target-reclaim-only-incremental`
- **8,13** (12): `align-capture-bounded`, `acc-store-lock`, `cli-login-runners`, `disk-target-reclaim-locking`, `disk-target-reclaim-lru-whole`, `disk-whole-guards-at-removal`, `restart-stamp-take-if`, `sweep-lock-exclusive`, `wait-exit`, `bounded-child-output`, `relay-interrupter`, `ready-fd-signal`
- **1,2,3** (10): `acc-note-wall-episode`, `acc-pin-unpin`, `acc-note-observations`, `acc-note-reading-max`, `acc-reset-claim`, `settle-ask-takeback`, `release-ownership`, `retarget-in-place`, `served-at`, `last-model`
- **1,2,5** (2): `netstate-leases-bounded`, `session-roster-cache`
- **1,2,8** (7): `acc-record-gone`, `acc-drop-stale-sessions`, `footer-path-branch-follow`, `usage-read-cost`, `left-typed-never-erased`, `queue-manage-with-baseline-atomic`, `usage-probe-memory-neutral`
- **1,3,4** (4): `drive-classify-verb`, `acc-advise-rule2-follow-and-canary`, `acc-advise-rule4-early`, `tap-gate`
- **1,3,5** (16): `relaunch-cold-login`, `step-word-classification`, `turn-end-ownership`, `release-is-next`, `model-due-rule`, `next-step-safety`, `deadline-totality`, `attention-horizons`, `interrupt-only-for-an-ended-agent`, `aterms-own-fault`, `stall-mark-and-remedy`, `claim-verb-classes`, `zsh-hazard-tables`, `config-retired-and-agent-keys`, `mode-word-parsers`, `approval-footer-mode`
- **1,3,6** (1): `wait-text-format`
- **1,3,7** (66): `drive-parse-sub-positionals`, `drive-top-parse-help-routing`, `drive-phase-reply`, `acc-stamp-cap`, `acc-signed-out-list`, `acc-next-usable-opening-hold`, `acc-rotation-breaker`, `acc-reset-allowed`, `cli-grammar`, `cli-upgrade-lines`, `disk-report-json-headline`, `disk-parse-df`, `footer-model-display`, `footer-model-name`, `footer-project-slug`, `footer-branch-of-head`, `footer-home-and-elide-path`, `footer-segments-order`, `footer-set-facts-typesetting`, `truncate-bytes`, `one-line-shortcut-equivalence`, `netwatch-route-of`, `relaunch-program-identity`, `relaunch-one-shot`, `relaunch-target-build`, `relaunch-permission-mode`, `relaunch-line-drop-order`, `tap-value-ours`, `catalog-parse-window`, `catalog-twin-binary`, `codex-rewrite-argv`, `codex-is-tui-argv`, `codex-thread-ids`, `pick-tab-word`, `model-family-version`, `model-availability-decision`, `claude-json-model-facts-only`, `live-model-reader`, `family-successor-newest-strictly-newer`, `offered-set-and-list-target`, `model-record-roundtrip`, `hand-chosen-model`, `user-model-settings-parse`, `version-order`, `choose-target-newest-managed-tie`, `launch-model-and-resume-by-the-rewrites-parse`, `running-clause-held-list`, `composer-empty`, `upgrade-clocks`, `model-outcome-reader`, `transcript-tasked`, `notice-fate-and-queue`, `reset-dialog-line`, `status-line-and-json-fields`, `need-and-mark-text-bounds`, `working-time-union`, `limit-notice-rows`, `hud-line`, `usage-json-schema-1`, `queue-codec-round-trip`, `strip-quotes-shape`, `approval-decline-text`, `stall-status-read`, `line-clip-and-exit-reason`, `resume-jitter`, `transcript-ends-on`
- **1,3,8** (3): `drive-dial-guard`, `row-vetting`, `clock-unix-roundtrip`
- **1,3,12** (12): `acc-state-unread-classification`, `acc-process-login-only-weakens`, `footer-lstart-utc`, `footer-card-fill`, `lights-read-screen`, `netwatch-settings-of`, `transcript-conversation-quota`, `catalog-build-facts-roundtrip`, `screen-limit-recogniser`, `relay-reply-classification`, `memory-fact-banner`, `approval-trace-names-decider`
- **1,3,13** (3): `cli-auth-check-env`, `claude-dir-of`, `resume-never-cut`
- **1,4,5** (1): `model-way-back`
- **1,4,8** (10): `cli-usage-account-attribution`, `cli-disk-verb-report`, `cli-accounts-add`, `cli-login-secrecy`, `cli-accounts-use`, `cli-accounts-schema-refusal`, `relaunch-line-dialect-and-rekey`, `carry-restart-outcome`, `restart-carries-login-and-tap`, `quiet-flow`
- **1,4,11** (1): `cli-limits-read-once`
- **1,5,7** (1): `footer-rule-run`
- **1,7,8** (14): `drive-journal-sid`, `acc-ledger-file`, `cli-usage-transcript-pick`, `cli-ledger-verb`, `disk-discover-roots`, `disk-discover-xpy-rule`, `disk-discover-budget-and-confinement`, `footer-rows-back-window-invariance`, `footer-git-head-resolution`, `footer-shell-cwds`, `tap-sweep`, `session-roster-integrity`, `wait-log-ledger-and-prune-bounds`, `footer-window-allocation`
- **1,7,12** (1): `cli-harness-switch-misplaced-off`
- **1,8,11** (1): `cli-retired-verbs`
- **1,8,13** (4): `cli-disk-write-ahead-journal`, `cli-upgrade-owner-status`, `disk-session-gone`, `relaunch-ended-verdict`
- **1,8,16** (1): `tap-noninterference`
- **2,3,5** (3): `netstate-schedule`, `netstate-reach-freshness`, `tap-silence-machine`
- **2,3,13** (1): `unwatched-predicate`
- **2,4,7** (2): `beside-memory-remedy`, `beside-remedies-under-hold`
- **2,8,13** (1): `relay-connection`
- **3,5,12** (1): `rfc3339-utc-civil`
- **8,10,13** (1): `align-capture-lifecycle-refines-model`
- **8,13,17** (1): `bounded-child-probe`
- **1,2,3,4** (3): `acc-begin-rotation-cas`, `acc-strike-undo-exact`, `memory-judgement`
- **1,2,3,5** (4): `owner-word-survives-restart`, `progress-stamp-single-writer`, `own-work-news`, `config-power-lattice`
- **1,2,3,7** (3): `acc-reset-outcome`, `transcript-fold-bounds`, `ctl-reply-readers`
- **1,2,3,8** (2): `aside-precedence`, `owner-word-ask`
- **1,2,4,8** (4): `announce-preflight`, `drain-void`, `failed-start-gate`, `model-ask-ledger`
- **1,2,5,8** (1): `login-wall-spends-nothing`
- **1,2,7,8** (2): `usage-moved`, `task-detection-and-fresh-restart`
- **1,2,7,11** (1): `supervise-result-and-log-split`
- **1,2,8,10** (1): `codex-restart-record-closure`
- **1,2,8,11** (2): `relay-framing-one-line`, `exception-lifecycle`
- **1,2,8,12** (1): `relaunch-snapshot-follow`
- **1,2,8,13** (4): `hand-lease-bracket`, `in-flight-orphan-pass`, `activation-wake`, `stop-and-handover`
- **1,2,12,13** (1): `busy-up`
- **1,3,4,5** (1): `acc-advise-past-probe`
- **1,3,4,7** (3): `relaunch-promise-iff-plan`, `resume-command-flags`, `frozen-text`
- **1,3,4,8** (1): `pressed-text-delivery`
- **1,3,4,12** (2): `approval-trust-dialog`, `approval-not-a-box`
- **1,3,5,6** (1): `retarget-arms`
- **1,3,5,7** (38): `drive-flag-ownership`, `footer-launch-facts`, `relaunch-line-env-quoting`, `usage-text-and-model-naming`, `held-by-status`, `host-roster-parse`, `etime-and-command-head`, `terminal-owner`, `login-wall-reader`, `priority-list-roundtrip`, `ready-answer-reader`, `next-change-complete`, `resume-command-inverts-relaunch-line`, `reply-count-grammar`, `blocks-views`, `glob-match-fnmatch`, `python-allowlist`, `split-segments`, `program-words`, `prelex`, `sed-script-and-alarm-idiom`, `numeric-format`, `config-line-reader-agrees`, `journal-line-grammar-and-json`, `ledger-since`, `ledger-render-escapes`, `limit-zone-and-reset-grammar`, `mail-row-and-principal`, `approval-roots`, `report-mark-roundtrip`, `report-find-start`, `report-finish-assess`, `report-render`, `session-group`, `human-input-stamp`, `reply-wait`, `composer-guard`, `approval-full-power-presses-every-bash-box`
- **1,3,5,10** (4): `reducer-refines-drain-bound-model`, `reducer-refines-work-end-model`, `reducer-refines-progress-machine`, `classifier-refines-progress-machine`
- **1,3,5,12** (6): `footer-session-entry-parse`, `tap-row-safe`, `screen-reply-parse`, `escalate-kind-and-subject`, `approval-amend-input`, `approval-limit-dialog`
- **1,3,5,13** (1): `usage-wall-of`
- **1,3,7,8** (8): `disk-report-classification`, `disk-target-assess-witness`, `disk-hogs`, `ancestry-guard`, `queue-recovery-is-replay`, `ledger-paths-and-bound`, `typing-budget`, `credits-billed-model-target`
- **1,3,7,10** (1): `upgrade-model-priority-refines-model`
- **1,3,7,11** (2): `acc-process-login-of-env`, `standing-quota-wall`
- **1,3,7,12** (9): `cli-disk-config-fail-closed`, `footer-last-cwd`, `busy-footer-live-zone`, `utc-and-lstart-parse`, `usage-panel-read-phase`, `usage-panel-sample`, `answer-box-token`, `approval-line-cells`, `render-phase-grammar`
- **1,3,7,13** (1): `usage-panel-windows`
- **1,3,8,10** (1): `watch-refines-progress-look`
- **1,3,8,13** (1): `cli-disk-tick`
- **1,4,5,8** (1): `presence-settle-ladder-gates`
- **1,4,7,8** (1): `cli-accounts-report`
- **1,4,8,11** (1): `cli-accounts-login-table`
- **1,4,8,12** (4): `resume-line-of-pid`, `typed-turn-fence`, `claude-home-resolution`, `worker-env-binding`
- **1,5,7,8** (2): `disk-target-dedupe-nested`, `catalog-scan-chunk-invariance`
- **1,7,8,10** (1): `codex-prune-refines-model`
- **1,7,8,12** (1): `codex-package-version`
- **1,7,10,12** (1): `ladder-refinement`
- **1,8,10,12** (1): `exit-record-refinement`
- **1,8,11,12** (2): `ledger-one-row-per-decision`, `reader-by-program`
- **2,3,5,7** (1): `relaunch-ended-at-once-shell-look`
- **2,3,5,10** (1): `relaunch-on-exit-refines-model`
- **3,5,7,13** (1): `relaunch-exit-record-look`
- **1,2,3,4,8** (1): `acc-record-wall-decision`
- **1,2,3,4,11** (1): `model-read-step-reads-tail-iff-pending`
- **1,2,3,5,7** (5): `acc-state-bounds-invariant`, `stop-rest-ladder`, `transcript-fold-max-merge`, `transcript-fold-classification`, `config-texts-round-trip`
- **1,2,3,5,8** (2): `composer-empty-caret-dim`, `refusal-news-episode`
- **1,2,3,5,10** (1): `upgrade-refines-never-strands-model`
- **1,2,3,7,8** (1): `account-ledger-readback`
- **1,2,3,7,10** (7): `turn-end-refines-SupervisorTurnEnd`, `turn-end-refines-SupervisorCruise`, `turn-end-refines-SupervisorLand`, `turn-end-refines-SupervisorOutageWait`, `turn-end-refines-SupervisorUsageWall`, `turn-end-refines-SupervisorCreditsHold`, `turn-end-refines-SupervisorConditionResume`
- **1,2,3,8,11** (2): `wait-told`, `sleep-wake-settle`
- **1,2,3,8,13** (1): `view-host-protocol`
- **1,2,4,8,11** (2): `relaunch-adoption`, `report-gather-protocol`
- **1,2,4,8,13** (1): `last-look-before-signal`
- **1,2,5,7,8** (4): `footer-tail-cache-equals-fresh-read`, `usage-incremental-fold-equals-one-pass`, `usage-session-aggregate`, `relay-read-bounds`
- **1,2,5,8,13** (1): `version-cache-backoff`
- **1,2,5,10,12** (1): `age-is-one-process`
- **1,2,7,8,11** (2): `hold-and-refusal-parking`, `journal-mirrors-output`
- **1,2,7,11,12** (1): `survey-report-once`
- **1,2,7,11,13** (1): `survey-host-told`
- **1,2,8,10,11** (2): `reset-books`, `attention-ownership`
- **1,3,4,5,7** (3): `acc-append-row-roundtrip`, `acc-advise-rule3-wall`, `background-walk`
- **1,3,4,5,12** (2): `approval-whole-read`, `approval-dialog-rules`
- **1,3,4,7,8** (1): `limit-extend`
- **1,3,5,6,7** (1): `state-record-roundtrip`
- **1,3,5,7,8** (3): `ledger-readers-are-row-projections`, `approval-read-rule`, `approval-git-read`
- **1,3,5,7,10** (2): `limit-queue-refines-model`, `classify-except-rm`
- **1,3,5,7,12** (20): `acc-roster-admission`, `acc-state-json-roundtrip`, `acc-event-line-roundtrip`, `footer-tail-floor-attribution`, `footer-mode-row-recognizer`, `transcript-projection-refines-full-parse`, `relaunch-line-shape-quote-bound`, `answer-plan-contract`, `ledger-row-json`, `blocks-partition`, `escalate-attention-text`, `approval-one-shot-only`, `approval-guarded-press`, `approval-command-readings`, `report-parse-history`, `transcript-end-positional`, `parse-text-json`, `ctl-reply-lost`, `survey-guard-unique`, `parked-card-keeps-busy`
- **1,3,5,7,13** (5): `drive-flag-values`, `acc-candidates-order`, `plain-zsh-line-grammar`, `limit-reset-placement`, `report-parse-offscreen`
- **1,3,5,7,14** (2): `rewrite-argv-refines-commander`, `json-reader-grammar`
- **1,3,7,8,10** (2): `footer-model-refines-ClaudeFooterModel`, `usage-tap-refines-HarnessUsageTap`
- **1,3,7,8,11** (1): `live-zone-read`
- **1,3,7,8,13** (2): `disk-pressure-pass`, `disk-whole-pass`
- **1,3,7,10,13** (1): `upgrade-model-switch-refines-model`
- **1,3,7,11,12** (2): `relay-render`, `typed-draft`
- **1,3,7,11,13** (1): `disk-apply-guards`
- **1,3,7,12,13** (1): `cli-limits-view`
- **1,4,5,8,12** (1): `model-evidence-scope`
- **1,5,7,8,13** (1): `roster-aside-union`
- **2,4,7,8,11** (1): `budget-bound`
- **2,7,8,10,11** (1): `box-settle`
- **2,7,8,11,13** (2): `stop-semantics`, `host-look-wake`
- **1,2,3,4,5,10** (1): `acc-advice-commits`
- **1,2,3,4,5,12** (1): `approval-level-monotone`
- **1,2,3,4,8,13** (1): `visit-shell-refines-pure-reducer`
- **1,2,3,5,7,12** (2): `config-reader-never-grants`, `config-misplaced-only-limits`
- **1,2,3,5,7,13** (2): `priority-admit-monotone`, `reset-place`
- **1,2,3,5,8,11** (1): `limit-notice-reset-placement`
- **1,2,3,7,8,10** (2): `model-switch-carry`, `login-wall-refines-HarnessLoginWall`
- **1,2,3,7,8,11** (6): `stray-digit-cleanup`, `stall-ladder`, `account-advice-asks`, `login-lifecycle-accounts`, `account-move-answer`, `autopilot-word-edge`
- **1,2,3,7,8,13** (1): `refused-write-retried`
- **1,2,3,7,12,13** (1): `context-watch`
- **1,2,4,5,8,13** (1): `release-owed-is-paid-once`
- **1,2,4,7,8,11** (2): `controller-answer`, `model-bucket-and-fallback-timing`
- **1,2,4,7,8,13** (2): `no-act-without-ownership-proofs`, `ledger-gather`
- **1,2,4,8,10,11** (2): `limit-dialog-wait-row`, `memory-ceiling-decision`
- **1,2,4,8,10,12** (1): `notice-owner-refinement`
- **1,2,5,7,8,10** (1): `footer-process-session-switch-model`
- **1,2,5,7,8,11** (1): `stall-probe-cost`
- **1,2,5,7,8,13** (1): `footer-fold-is-one-session`
- **1,2,5,8,10,11** (3): `reset-successor-escape-only`, `reset-gates-equivalence`, `told-choice`
- **1,2,7,8,10,11** (6): `wake-protocol`, `focus-choice-refines-model`, `press-landing-and-retry`, `claim-refines-model`, `decline-keys-refines-model`, `usage-probe-refines-model`
- **1,3,4,5,7,8** (1): `relaunch-line-construction`
- **1,3,4,5,7,12** (2): `approval-never-spends`, `approval-safe-bash-sound`
- **1,3,4,5,7,13** (1): `host-candidates-read-order`
- **1,3,5,7,8,11** (1): `tail-window-bounded`
- **1,3,5,7,8,12** (2): `footer-chose-model-since`, `approval-owner-review-held`
- **1,3,5,7,8,13** (1): `limit-wall-placement`
- **1,3,5,7,10,14** (2): `classify-sound`, `classify-precise-on-read-grammar`
- **1,3,5,7,11,12** (1): `box-key-normal-form`
- **1,3,5,7,12,13** (2): `footer-fit-rule-ladder`, `report-join`
- **2,4,7,8,10,11** (1): `stall-remedy-refines-model`
- **2,7,8,10,11,13** (1): `survey-zero-timing`
- **1,2,3,5,7,8,10** (1): `login-wall-acts`
- **1,2,3,5,7,8,11** (6): `wall-confirm-gate`, `act-write-protocol`, `never-type-over-a-person`, `usage-probe-gates`, `usage-probe-left-typed`, `credits-hold`
- **1,2,3,5,7,10,12** (1): `footer-tail-fold-refines-model`
- **1,2,3,5,7,10,13** (1): `account-rotation-refines-model`
- **1,2,3,5,8,10,11** (1): `question-priority-resolution`
- **1,2,3,7,8,10,11** (1): `account-move-execution`
- **1,2,3,7,8,11,13** (1): `usage-probe-panel-close`
- **1,2,4,7,8,10,12** (1): `relaunch-prompt-read`
- **1,2,4,7,8,11,13** (2): `outage-ride-out`, `stall-hold`
- **1,2,4,8,9,10,11** (1): `loop-applies-decision`
- **1,2,4,8,10,11,13** (1): `host-turn-end-protocol`
- **1,2,5,7,8,10,11** (2): `reset-keys-safety`, `question-text-safety`
- **1,2,5,7,8,11,12** (1): `press-fence-and-guard`
- **1,2,5,8,10,11,12** (1): `idle-point-classification`
- **1,2,7,8,10,11,13** (1): `survey-press-refines-model`
- **1,3,4,5,7,8,10** (1): `approval-rm-breaker-sound`
- **1,4,5,8,10,12,13** (1): `due-never-drops-unread`
- **1,2,3,4,7,8,11,12** (1): `escalation-badge`
- **1,2,3,5,7,8,10,11** (3): `turn-end-decision`, `api-wall-acts`, `left-limit-tracking`
- **1,2,3,5,7,8,10,13** (1): `usage-wall-wait`
- **1,2,3,5,7,8,11,12** (2): `survey-press-gate`, `review-point-once`
- **1,2,4,7,8,10,11,12** (3): `box-ask-refines-model`, `question-text-refines-model`, `question-answer-refines-model`
- **1,2,4,7,8,10,11,13** (1): `reset-host-dialogue`
- **1,2,5,7,8,10,11,12** (1): `tail-reread-equivalence`
- **1,2,3,4,5,7,8,11,12** (2): `await-turn-wait-protocol`, `approval-press-safety`
- **1,2,3,4,7,8,10,11,12** (1): `limit-episode`
- **1,2,3,4,7,8,10,11,13** (1): `reset-step-refines-model`
