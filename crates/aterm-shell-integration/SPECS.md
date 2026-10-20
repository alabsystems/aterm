<!-- Copyright 2026 Andrew Yates -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# aterm-shell-integration: spec recovery from its 166 unit tests

> Status 2026-10-16, after the merge with the tests-and-gates audit: the rows naming 1 test it
> deleted are gone. Counts elsewhere in this file were taken before.

> Status 2026-10-16: this is the crate's migration worklist. Each spec below is proved through
> Trust/Clean, and then the tests it subsumes are deleted. The 6 checks in §4 sample no spec and can
> be deleted now. Nothing has been deleted yet.
>
> Added later on 2026-10-16: `sh-pwsh-keys` (the PowerShell readline key layer), sampled by the 6
> tests in `src/tests_pwsh_keys.rs`. Its §1 and §2 rows and its place in the tables below are
> written; the headline counts (166 tests, 45 specs, 160 covered) predate it.

Source: `crates/aterm-shell-integration` (read only). The `#[test]` fns are src/lib.rs 2,
src/tests.rs 129, src/tests_loader.rs 21, src/tests_tty.rs 12, tests/prepare_fish_xdg_default.rs 1
and tests/prepare_zsh_unset_zdotdir.rs 1, for 166 in all. `grep -c '#\[test\]'` reports 167 because
`tests/shell_suite.rs` mentions `#[test]` in a doc comment but defines no test. Nothing was compiled or run.
The claims below come from reading the code.

Result: **45 specs** cover **160 tests**. **6 tests are not spec samples** (§4). 20 specs are about
compiled Rust in `src/lib.rs` and retire 40 tests as their primary spec. 25 specs (`sh-*`) are about the
embedded shell programs and retire 120 tests.

## 0. Notation used in the statements

**Two layers.** Specs whose id does not start with `sh-` are about compiled Rust in `src/lib.rs`, and
Trust/Clean can prove them about the compiled fn. Specs whose id starts with `sh-` are about the
**embedded shell programs**: `scripts::{ZSH,BASH,FISH,POWERSHELL}`, `ZSH_WRAPPER`, `BASH_WRAPPER` and
`WSL_LAUNCH_SH`. Each one says what a real zsh, bash, fish, pwsh or cmd does when it runs those bytes.
No Rust fn body implements these properties, so a Trust proof about a compiled fn cannot discharge them.
They need a shell-language semantics: a Clean model of the POSIX-sh/zsh/bash/fish/PowerShell subset the
scripts use, plus a refinement from that model to the real interpreters. Audit §7 has no row for this.
This file calls it gap **SH** and proposes it as a new §7 row. The nearest existing rows are 8 (effects:
process, pty, fs, env) and 10 (model→code refinement), so every `sh-*` row lists `SH,8,10`. Until SH
exists, the `sh-*` specs stay written down here and their live tests stay.

Gap 0 (a seal that publishes Clean rows) is a prerequisite for every row, so the blocking-gaps column
leaves it out. No spec here is blocked by gap 0 alone, because every Rust fn in the crate handles at least
`&str`, `String`, `Vec`, `Option` or the filesystem.

- `last_seg(p)`: the suffix of p after its last `/` or `\` (all of p if it has neither).
- `lc`: ASCII lowercase. `strip1(x,s)`: s with one trailing x removed, if present.
- `PH` = `NONCE_PLACEHOLDER` = `"@ATERM_SHELL_NONCE@"`. `TITLE` = `CMD_PROMPT_TITLE` = `"$e]0;$P$e\"`.
- `OURS` = `WSLENV_ENTRIES` = `["ATERM_SHELL_INTEGRATION_DIR/p","ATERM_SHELL_NONCE","ATERM_WSL_CWD"]`.
  `name(e)` is the prefix of e before its first `/`, and `names(OURS)` = {name(o) | o ∈ OURS}.
- `pev(p)` = `path_env_value(p)` = `String::from_utf8_lossy(p.as_os_str().as_encoded_bytes())`.
- `SET` = `script_set()`: the 10 (relative path, bytes) pairs. `ADDR` = `script_set_address()`.
- `Match(d)` ⇔ ∀(r,b)∈SET: `fs::read(d/r) = Ok(b)`.
- `pct(s)`: the OSC 7 percent encoding. Bytes in `[A-Za-z0-9_.~/-]` are kept. Every other byte becomes
  `%XX` in uppercase hex.
- `enc633(s)`: the OSC 633;E encoding. `\` becomes `\\`, `;` becomes `\x3b`, a space becomes `\x20`, and
  each byte in 0x00..=0x1F or 0x7F becomes `\xNN` in lowercase hex. Every other byte is kept.
- `Frame(p)` = ESC `]` p BEL. `suffix(n)` = `";id="++n` if n≠"" else "".
- `Session(env)` ⇔ `ATERM_CHILD` or `ATERM_SESSION_ID` is non-empty.
- `Bare(sh)`: the same interpreter run with no integration sourced. `Obs\OSC`: a shell's stdout/stderr
  with every OSC frame removed.

**Refactors that shrink the effect surface (gap 8).** Each one turns an effectful spec into a pure one
that the gap-1/3/5 lanes can prove:
- Factor `prepare_zsh`, `prepare_fish`, `prepare_wsl` and `prepare_cmd` into pure functions of the
  inherited value, as `cmd_prompt` already is: `zsh_env(base, Option<zdotdir>)`, `fish_env(base,
  Option<xdg>)`, `wsl_env(base, existing)`. This moves `injection-shape` and `cmd-prompt` off gap 8.
- Factor `collect_old_script_sets` into a pure selector over `[(name, is_dir, mtime)]` plus a delete loop.
  This moves the selection half of `gc-policy` off gap 8.
- Make `prepare_into` skip `ensure_scripts` for `Cmd`, which reads no file (see §5.1).

## 1. Specs (TSV)

Columns: `id`, `kind`, `functions`, `statement`, `subsumes` (primary and secondary tests; the primary
choice is in §2), `rust_features` (what the proof must handle), and `blocking_gaps` (audit §7 row
numbers, with `SH` for the proposed shell-semantics row).

The table holds one row, `sh-pwsh-keys`. The rows for the other 45 specs that §2 and the tables
below name (the Rust specs on `src/lib.rs` and the `sh-*` shell-program specs) were never written
into this file; §2's `primary`/`also` ids and the tables are what remains of them. The session
specs this crate's file once carried are `crates/aterm-session/SPECS.md`'s.

```tsv
id	kind	functions	statement	subsumes	rust_features	blocking_gaps
sh-pwsh-keys	contract	scripts::POWERSHELL (KEY BINDINGS: __aterm_setup_keybindings and its gated call)	Let P be Windows PowerShell 5.1 or pwsh 7 with PSReadLine loaded before the script, B₀ its Get-PSReadLineKeyHandler -Bound map (key string → function, compared ordinally) just before, B₀⁺ that map with the first key k₁ of every bound two-key chord "k₁,k₂" also mapped (to ChordFirstKey: PSReadLine dispatches k₁ to the chord, while -Bound lists only "k₁,k₂"), and B₁ the -Bound map after sourcing POWERSHELL. Rows = the 21 (chord, f, s) of READLINE_ROWS; f_P = f, except Ctrl+u's is BackwardKillInput if P has it, else BackwardKillLine, and Ctrl+e's is, if P has AcceptSuggestion, the script-block handler AcceptSuggestionAndEndOfLine (-Bound lists its BriefDescription; at each press it reads the buffer and caret with GetBufferState, calls AcceptSuggestion only when the caret is at the end of the buffer, then EndOfLine: pressed at the end it accepts a showing inline suggestion, pressed inside the line it only moves to the end and a second press accepts, zsh-autosuggestions' rule, and with no suggestion showing it is EndOfLine), else EndOfLine. If Session(env) ∧ ATERM_DISABLE_READLINE_KEYS is empty ∧ EditMode = Windows: B₁ = B₀[c ↦ f_P for every row with P having f_P ∧ B₀⁺(c) ∈ {unbound, s}], and every other key keeps B₀'s function; so a key the profile bound to anything but s (another function, a script block, or the first key of a chord) keeps the profile's binding, while a profile's binding of c to s itself cannot be told from stock and is replaced. Otherwise (¬Session, the opt-out set, or EditMode Emacs/Vi): B₁ = B₀. With PSReadLine absent nothing is bound. In every case the load succeeds and writes nothing to stderr. Pressed at a live prompt (P in a ConPTY in a session, the history predictor inline where P has AcceptSuggestion, the PRESS_SEEDS lines in history): each PRESSES line typed and ended by Enter is submitted as its accepted column gives it where P has AcceptSuggestion and as its plain column otherwise, so Ctrl+e then Enter from inside the line submits what was typed. Static half: the call follows the PSConsoleHostReadLine shim, runs once at load, reads -Bound (never -Chord, absent from PSReadLine 2.0.0) into an Ordinal Dictionary, and its row table is exactly READLINE_ROWS, binding none of Tab, Ctrl+z/v/c/C/x, Ctrl+Left/Right, Ctrl+Backspace/Delete, Escape, Home, End; the Ctrl+e script block reads no variable of the function, whose scope is gone when a key press runs it, and calls AcceptSuggestion only under the caret test.	tests_pwsh_keys::test_powershell_layers_readline_chords_over_windows_mode, tests_pwsh_keys::test_powershell_binds_the_readline_chords_in_an_aterm_tab, tests_pwsh_keys::test_powershell_readline_layer_keeps_a_profiles_own_bindings, tests_pwsh_keys::test_powershell_readline_layer_leaves_emacs_and_vi_alone, tests_pwsh_keys::test_powershell_readline_layer_honours_the_opt_out_and_stays_out_of_other_terminals, tests_pwsh_keys::test_powershell_ctrl_e_accepts_a_suggestion_only_at_the_end_of_the_line	PowerShell semantics of dot-sourcing, function scope ($ErrorActionPreference local), try/catch, .NET reflection (GetMethod) and Dictionary with an ordinal comparer, hashtable splatting, a script block run at key-press time in a later scope; PSReadLine's key-handler table as a finite map; quantification over the user's prior bindings B₀; a live PSReadLine ReadLine (console key events through ConPTY, the inline suggestion as render-time state, the history queue)	SH,8,10
```

### Distribution by kind (46)

| kind | count | ids |
|---|---|---|
| contract | 26 | wsl-cwd-env, typed-rekey-text, lf-only, merge-wslenv, cmd-prompt, augment-with-nonce, body-extract, script-set-address, injection-shape, prepare-into-unknown, ensure-scripts-writes-set, ensure-cached-memo, install-script-set, gc-policy, cache-dir, sh-osc7-report, sh-633e-and-frame, sh-marks, sh-bash-preexec-capture, sh-mux, sh-rekey-channel, sh-typed-rekey, sh-typed-upgrade, sh-pwsh-prompt, sh-pwsh-utf8, sh-pwsh-keys |
| equivalence | 6 | path-env-value-lossy, sh-osc7-encoder, sh-fish-empty-is-unset, sh-prompt-render, sh-fish-user-prompt, sh-user-semantics-preserved |
| invariant | 6 | content-address-isolation, scratch-suffix-unique, sh-nonce-capture, sh-bash-ps1-one-133b, sh-tty-sanity, sh-zsh-line-init-marks |
| table-property | 2 | detect-shell, script-const-lints |
| refinement | 2 | sh-body-pointer, sh-windows-launchers |
| other | 4 | sh-load-completes, sh-wrappers, sh-path-front, sh-live-managed |

### Blocking-gap histogram (specs blocked by each gap)

| gap | specs | which |
|---|---|---|
| 1 (non-scalar carriers) | 15 | detect-shell, wsl-cwd-env, typed-rekey-text, lf-only, merge-wslenv, cmd-prompt, augment-with-nonce, body-extract, script-const-lints, path-env-value-lossy, script-set-address, injection-shape, ensure-scripts-writes-set, gc-policy, sh-body-pointer |
| 2 (certified &self/&mut views) | 2 | augment-with-nonce, ensure-cached-memo |
| 3 (E6 body widening) | 11 | detect-shell, wsl-cwd-env, typed-rekey-text, lf-only, merge-wslenv, cmd-prompt, body-extract, path-env-value-lossy, script-set-address, injection-shape, gc-policy |
| 5 (quantified clauses) | 7 | detect-shell, wsl-cwd-env, merge-wslenv, cmd-prompt, augment-with-nonce, body-extract, script-const-lints |
| 7 (loops) | 8 | lf-only, merge-wslenv, augment-with-nonce, script-const-lints, path-env-value-lossy, script-set-address, ensure-scripts-writes-set, gc-policy |
| 8 (effects) | 36 | every sh-* spec (26), plus cmd-prompt, injection-shape, prepare-into-unknown, ensure-scripts-writes-set, ensure-cached-memo, install-script-set, content-address-isolation, gc-policy, scratch-suffix-unique, cache-dir |
| 10 (model→code refinement) | 26 | every sh-* spec (26) |
| 12 (cross-crate citation) | 2 | script-set-address, cache-dir |
| 13 (threads, closures) | 6 | script-set-address, ensure-cached-memo, install-script-set, content-address-isolation, scratch-suffix-unique, sh-tty-sanity |
| SH (shell-language semantics, proposed) | 26 | every sh-* spec (26) |

Gaps 0, 4, 6, 9, 11 and 14–17 appear in no row. 4 (callee-ensures composition) and 14 (Clean elaborator) will still be met while the proofs are written.

## 2. Test → spec map (TSV)

`primary` is the spec that retires the test. `also` lists the other specs the same test samples. A test in §4 shows `(none; …)`.

```tsv
file	test	primary	also
src/tests.rs	test_encoders_answer_in_reply_and_keep_the_users	sh-osc7-encoder	sh-633e-and-frame
src/tests.rs	test_bash_on_msys_and_cygwin_reports_the_windows_drive_form	sh-osc7-report	
src/tests.rs	test_bash_preexec_skips_prompt_command_array_siblings	sh-bash-preexec-capture	
src/tests.rs	test_detect_posix_shells	detect-shell	
src/tests.rs	test_cwd_tab_title_strips_control_chars	sh-633e-and-frame	
src/tests.rs	test_prepare_unknown_returns_none	prepare-into-unknown	
src/tests.rs	test_fish_integration_loads_under_an_empty_guard_override	sh-fish-empty-is-unset	
src/tests.rs	test_fish_empty_prompt_style_falls_through_to_the_user_prompt	sh-fish-empty-is-unset	
src/tests.rs	test_fish_empty_disable_prompt_titles_still_emits_titles	sh-fish-empty-is-unset	
src/tests.rs	test_fish_empty_prompt_colors_fall_back_to_defaults	sh-fish-empty-is-unset	
src/tests.rs	test_bash_children_never_inherit_the_nonce_through_ps1	sh-nonce-capture	
src/tests.rs	test_zsh_unsets_shell_nonce_env_after_source	sh-nonce-capture	
src/tests.rs	test_prepare_powershell_dot_sources_script	injection-shape	
src/tests.rs	test_powershell_script_is_ascii	script-const-lints	
src/tests.rs	test_powershell_speaks_utf8_only_inside_an_aterm_tab	sh-pwsh-utf8	
src/tests.rs	test_powershell_unsets_shell_nonce_env_after_source	sh-nonce-capture	
src/tests.rs	test_powershell_prompt_emits_osc7_and_prompt_marks	sh-pwsh-prompt	
src/tests.rs	test_powershell_osc7_path_percent_encodes_windows_paths	sh-osc7-encoder	
src/tests.rs	test_cache_dir_windows_uses_localappdata	cache-dir	
src/tests.rs	test_cache_dir_is_the_per_user_cache_outside_the_containment_write_roots	cache-dir	
src/tests.rs	test_merge_wslenv_is_append_safe_and_idempotent	merge-wslenv	
src/tests.rs	test_wsl_cwd_env_only_crosses_a_posix_path	wsl-cwd-env	
src/tests.rs	test_prepare_cmd_wraps_the_prompt_with_marks_and_cwd	cmd-prompt	
src/tests.rs	test_posix_scripts_are_written_lf_only	lf-only	ensure-scripts-writes-set
src/tests.rs	test_cmd_prompt_emits_real_osc_133_marks	sh-windows-launchers	
src/tests.rs	test_wsl_launcher_emits_real_osc_133_marks	sh-windows-launchers	
src/tests.rs	test_fish_prompt_reasserts_its_own_cwd_before_the_prompt_start	sh-osc7-report	
src/tests.rs	test_fish_fallback_prompt_survives_a_missing_hostname	sh-prompt-render	
src/tests.rs	test_zsh_already_running_session_shell_picks_up_the_managed_dirs_live	sh-live-managed	
src/tests.rs	test_bash_live_pickup_holds_under_set_u	sh-live-managed	
src/tests.rs	test_fish_already_running_session_shell_picks_up_the_managed_dirs_live	sh-live-managed	
src/tests.rs	a_live_bash_prompt_carries_one_133b_however_many_prompts	sh-bash-ps1-one-133b	
src/tests.rs	a_foreign_build_cannot_change_what_this_builds_tabs_source	content-address-isolation	
src/tests.rs	scratch_suffixes_drawn_at_once_never_collide	scratch-suffix-unique	
src/tests.rs	old_script_folders_are_collected_and_young_or_newest_ones_kept	gc-policy	
src/tests_loader.rs	the_script_set_carries_each_body_beside_its_loader	ensure-scripts-writes-set	
src/tests_loader.rs	a_real_zsh_sources_the_body_alone_cleanly	sh-body-pointer	
src/tests_loader.rs	a_live_zsh_takes_a_newer_body_at_its_next_prompt	sh-body-pointer	
src/tests_loader.rs	a_live_bash_takes_a_newer_body_at_its_next_prompt	sh-body-pointer	
src/tests_loader.rs	a_live_zsh_from_before_loaders_is_upgraded_by_the_typed_line	sh-typed-upgrade	
src/tests_loader.rs	a_live_bash_from_before_loaders_is_upgraded_by_the_typed_line	sh-typed-upgrade	
src/tests_loader.rs	the_key_only_line_takes_the_first_line_of_a_three_line_file	sh-typed-rekey	
src/tests_loader.rs	a_nested_zsh_is_not_integrated_by_the_typed_line	sh-typed-upgrade	
src/tests_loader.rs	a_nested_bash_is_not_integrated_by_the_typed_line	sh-typed-upgrade	
src/tests_loader.rs	a_live_fish_takes_a_newer_body_at_its_next_prompt	sh-body-pointer	
src/tests_loader.rs	a_live_fish_from_before_loaders_is_upgraded_by_the_typed_line	sh-typed-upgrade	
src/tests_loader.rs	a_nested_fish_is_not_integrated_by_the_typed_line	sh-typed-upgrade	
src/tests_loader.rs	a_live_zsh_keeps_the_users_key_bindings_across_a_newer_body	sh-body-pointer	
src/tests_loader.rs	a_live_fish_keeps_the_users_key_bindings_across_a_newer_body	sh-body-pointer	
src/tests_loader.rs	a_live_zsh_with_a_line_init_hook_is_upgraded_without_a_widget_loop	sh-typed-upgrade	
src/tests_loader.rs	a_live_fish_keeps_the_users_own_prompt_through_the_typed_upgrade	sh-typed-upgrade	
src/tests_loader.rs	a_live_fish_with_its_own_prompt_takes_a_newer_body	sh-body-pointer	
src/tests_loader.rs	a_live_fish_wrapped_prompt_reads_the_commands_status	sh-fish-user-prompt	
src/tests_loader.rs	a_live_fish_with_a_prompt_style_gives_the_users_prompt_back	sh-fish-user-prompt	
src/tests_loader.rs	a_live_fish_with_its_own_prompt_keeps_its_prompt_marks	sh-fish-user-prompt	
src/tests_tty.rs	an_integrated_zsh_keeps_its_tty_when_a_raw_program_dies	sh-tty-sanity	
src/tests_tty.rs	an_integrated_zsh_whose_rc_broke_the_tty_runs_its_first_command_sane	sh-tty-sanity	
src/tests_tty.rs	an_integrated_zsh_with_its_own_stty_is_repaired_not_frozen	sh-tty-sanity	
src/tests_tty.rs	a_running_zsh_with_a_broken_tty_is_repaired_by_the_body_the_loader_delivers	sh-tty-sanity	
src/tests_tty.rs	an_integrated_bash_repairs_its_tty_after_a_raw_program_fails	sh-tty-sanity	
src/tests_tty.rs	an_integrated_zsh_with_err_return_stays_frozen_after_a_failing_stty	sh-tty-sanity	
src/tests_tty.rs	an_integrated_zsh_refreezes_when_a_thawed_command_is_interrupted	sh-tty-sanity	
src/tests_tty.rs	an_integrated_zsh_marks_every_prompt_whatever_its_rc_does_to_zle_line_init	sh-zsh-line-init-marks	
src/tests_tty.rs	an_integrated_zsh_under_xtrace_traces_the_users_commands_and_not_aterms	sh-user-semantics-preserved	
src/tests_tty.rs	an_integrated_bash_under_xtrace_traces_the_users_commands_and_not_aterms	sh-user-semantics-preserved	
src/tests_tty.rs	an_integrated_bash_keeps_the_users_err_trap_errexit_and_functrace_as_bare_bash_does	sh-user-semantics-preserved	
src/tests_tty.rs	an_integrated_bash_keeps_the_users_last_argument	sh-user-semantics-preserved	
tests/prepare_fish_xdg_default.rs	prepare_fish_xdg_includes_default_fallback	injection-shape	
tests/prepare_zsh_unset_zdotdir.rs	prepare_zsh_sets_unset_zdotdir_when_empty	injection-shape	
src/tests_pwsh_keys.rs	test_powershell_layers_readline_chords_over_windows_mode	sh-pwsh-keys	
src/tests_pwsh_keys.rs	test_powershell_binds_the_readline_chords_in_an_aterm_tab	sh-pwsh-keys	
src/tests_pwsh_keys.rs	test_powershell_readline_layer_keeps_a_profiles_own_bindings	sh-pwsh-keys	
src/tests_pwsh_keys.rs	test_powershell_readline_layer_leaves_emacs_and_vi_alone	sh-pwsh-keys	
src/tests_pwsh_keys.rs	test_powershell_readline_layer_honours_the_opt_out_and_stays_out_of_other_terminals	sh-pwsh-keys	
src/tests_pwsh_keys.rs	test_powershell_ctrl_e_accepts_a_suggestion_only_at_the_end_of_the_line	sh-pwsh-keys	
```

## 3. Coverage

- The biggest collapses are `sh-nonce-capture` (17 primary), `sh-live-managed` (12), `sh-typed-upgrade` (8),
  `injection-shape` (7), `sh-body-pointer` (7), `sh-tty-sanity` (7), `script-const-lints` (6) and
  `sh-osc7-report` (6).
- Only `path-env-value-lossy` has no primary test. Its one sample,
  `test_prepare_wsl_env_carries_path_translated_wslenv`, is retired by `injection-shape`, which
  calls `pev`. The spec is kept because `injection-shape` is stated in terms of `pev`.
- A test listed under both layers in §2 (such as `the_typed_rekey_sets_the_globals_the_marks_read`,
  `test_prepare_wsl_argv_execs_the_bash_rcfile` or `every_posix_script_is_a_loader_around_one_body`)
  is deleted only after **both** its Rust spec and its `sh-*` spec are proved. Until then, split it, so
  that its Rust half goes with the Rust proof and its live-shell half stays.
- Of the 160 covered tests, 40 can be retired by Rust proofs alone once their gaps close. The other 120
  wait on SH.

## 4. Tests that are not spec samples

Status 2026-10-16: deleted `test_scripts_contain_prompt_override`, `test_spawned_shells_never_read_the_developers_home`, `test_bash_fallback_unnonced_when_env_missing`, `test_powershell_script_references_shell_nonce_env` and `test_bash_prompt_command_guard_prevents_spurious_capture`. `test_bash_default_mode_embeds_133b_in_ps1` was not in this deletion batch and remains.

| test | why | action |
|---|---|---|

Actions: 6 delete, 0 become-spec, 0 keep.

## 5. Implied specs that look wrong, or disagree with the code

1. **`test_prepare_cmd_wraps_the_prompt_with_marks_and_cwd` depends on the test process's `PROMPT`.**
   `prepare_cmd` reads `std::env::var("PROMPT")`. If the inherited PROMPT is non-default, or already
   carries `]133;A` (as it does when the suite runs inside an aterm cmd tab, where the nonce has already
   been substituted), the `$P$G` and `133;A;id={nonce}` assertions fail. `prepare_into(Cmd)` also writes
   the whole script set to disk, although cmd reads none of it. The implied spec belongs on the pure
   `cmd_prompt` and `augment_with_nonce` (`cmd-prompt`, `augment-with-nonce`).
2. **Two memo tests compare against the wrong bytes.** `test_prepare_cached_skips_rewrite_and_self_heals`
   and `test_prepare_cached_rewrites_on_base_change` compare the bytes on disk with `scripts::ZSH`, but
   the bytes written are `lf_only(scripts::ZSH)`. On a CRLF checkout, the case `lf_only` exists for, they
   fail. `a_foreign_build_cannot_change_what_this_builds_tabs_source` makes the comparison correctly,
   with `lf_only`.
3. **`lf_only` does not remove every CR.** `test_posix_scripts_are_written_lf_only` says no CR reaches
   disk, but `lf_only` replaces only `"\r\n"`, so a lone `\r` survives. The property holds only because
   the constants contain no lone CR. `lf-only` states that as a separate const fact.
4. **The content-address isolation claim is too strong.**
   `a_foreign_build_cannot_change_what_this_builds_tabs_source` says this build's tabs are safe
   "whatever a foreign build wrote". With a warm memo, `ensure_cached` only stats
   `root/<ADDR>/aterm_shell_integration.zsh`, so a write *into* `root/<own address>` is served without
   being re-verified. The true spec (`content-address-isolation`) needs the precondition that foreign
   writes stay outside `root/<ADDR>`. A content-addressed foreign build satisfies it; a person editing
   the folder does not. Only a process's first prepare (the cold path) replaces a damaged folder.
5. **The cache-dir containment check often passes vacuously.**
   `test_cache_dir_is_the_per_user_cache_outside_the_containment_write_roots` returns early when the
   base cannot be canonicalized or already lies under a Containment write root (HOME or XDG_CACHE_HOME
   under /tmp, which is common on CI), so the containment half is often never checked.
6. **A stale `cfg(windows)` gate.** `test_detect_powershell` gates its Windows-path case on
   `cfg(windows)`, but `ShellType::detect` splits on both `/` and `\` on every host
   (`test_detect_wsl_and_cmd` checks backslash paths everywhere). The case could run on every host.
7. **Substring pins sample behaviour weakly.** `test_prepare_wsl_argv_execs_the_bash_rcfile` and the
   static pins of POWERSHELL, FISH and ZSH are substring checks on embedded code, so a comment that
   contains the needle satisfies them. The fish tests had to work around this by filtering out comment
   lines. `script-const-lints` restates the meaningful ones as decidable predicates over non-comment
   lines.
8. **Skip equals pass.** The live tests in `src/tests_loader.rs` and `src/tests_tty.rs` skip silently
   (eprintln and return) where zsh, fish or `script` is missing, and several zsh lanes run only on
   macOS. On such hosts a green suite says nothing about the `sh-*` specs. `resolve_test_shell`'s own
   comment documents this hazard. `src/tests_pwsh_keys.rs` skips its pwsh 7 half where no pwsh
   resolves, but writes the SKIP to the stderr handle, past libtest's capture, so a green run there
   says that pwsh did not run.

## 6. Proof order (easiest high-value first)

No spec is blocked by gap 0 alone (§0). The order below starts with pure `&str`/`Option` functions
that need only gap 1 (string carriers) and gap 3 (calls, enum matches), so the first work on Trust here
is the non-scalar carrier.

1. **`wsl-cwd-env`** (1,3,5). This is an enum compare, `Option ?` and two `starts_with` calls, with no
   loop and no allocation beyond `to_string`. It is the smallest test of the `&str`/`Option<(String,String)>`
   carrier, and it retires 1 test.
2. **`typed-rekey-text`** (1,3). Each arm is a single `format!` with one interpolated `&str`, and the
   statement is equality with a literal template. It retires the Rust half of
   `the_typed_rekey_sets_the_globals_the_marks_read`, and it is the Rust half of `sh-typed-rekey` and
   `sh-typed-upgrade` (16 tests downstream).
3. **`detect-shell`** (1,3,5). It is a finite string table after `last_seg`/`lc`/`strip1`. It retires 3
   tests and makes the stale cfg gate of §5.6 moot.
4. **`script-set-address`, the `is_address`/`nibble_to_hex` half** (`nibble_to_hex` is a `u8` range
   match, which seal `150efe4a` already admits, plus an `as char` cast, which is gap 3). Do this before
   the SHA-256 half, which waits on gaps 12 and 13.
5. **`cmd-prompt` pure half** (1,3,5) and **`augment-with-nonce`** (1,2,5,7). Together they retire 4 cmd
   tests and the augment test, and they fix §5.1 once `prepare_cmd` takes PROMPT as a parameter.
6. **`lf-only`**, **`body-extract`** and **`script-const-lints`** (1,3/5,7). These are const evaluation
   over the `include_str!` constants. They retire 8 tests and give the static halves of five `sh-*`
   specs. They need efficient kernel string reduction over tens of KB.
7. **`merge-wslenv`** (1,3,5,7). This is a split/filter/join loop with an idempotence corollary.
8. **`injection-shape`** after the gap-8 refactor in §0 (then 1,3 only). It retires 7 tests.
9. Effectful Rust: `prepare-into-unknown`, `ensure-scripts-writes-set`, `gc-policy` (pure selector
   first), `cache-dir`, then `ensure-cached-memo`, `install-script-set`, `scratch-suffix-unique` and
   `content-address-isolation` (8, 13).
10. `sh-*` specs once gap SH exists. Start with the pure encoders (`sh-osc7-encoder`, `sh-633e-and-frame`,
    and `sh-marks`), which are byte functions in shell, then `sh-nonce-capture` (17 tests),
    `sh-wrappers`, `sh-path-front`, and last the temporal, pty-bound specs (`sh-live-managed`,
    `sh-body-pointer`, `sh-typed-upgrade`, `sh-tty-sanity`, `sh-zsh-line-init-marks`,
    `sh-user-semantics-preserved`).
