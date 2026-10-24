<!-- Copyright 2026 Andrew Yates -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# aterm-vitals: specs behind the stuck-process engine's 81 tests

> Status: specs for the stuck-process engine (`src/jobs/`, design
> `docs/DESIGN-system-awareness-2026-10-16.md` §11 and `docs/DESIGN-hung-background-call-2026-10-20.md`
> D2). Each spec below is held by the tests named in its `subsumes` column; none is proved through
> Trust/Clean yet. The crate's Deck modules (`model.rs`, `trend.rs`, `words.rs`, `ink.rs`) carry 30
> additional tests listed in §3.

Source: `crates/aterm-vitals/src/jobs/` (`mod.rs`, `units.rs`, `step.rs`, `wait.rs`, `wire.rs`), with the
tests in `src/jobs/tests.rs`, `tests/stuck_scenarios.rs` and `tests/conformance_stuck_verdict.rs`, and the
scripted machine they share in `tests/support/{world,scenarios}.rs`.

Result: **22 specs** over **81 tests**. Every test is a spec sample (§4).

## 0. Notation used in the statements

- `U` = `src/jobs/tests.rs`, `S` = `tests/stuck_scenarios.rs`, `C` = `tests/conformance_stuck_verdict.rs`.
- `W` = the scripted machine (`tests/support/world.rs`): processes with counter rates, descriptors and an
  output file; tabs with their OSC 133 block, terminal mode word and input gate; a clock the test steps.
  `W.tick` does what the host does each probe tick: sweep, `Jobs::plan`, read what the plan names,
  `Jobs::absorb`.
- `φ(q, μ)` = `phi_milli(q, μ)` = ⌊q · 10⁹ / (max(μ, 1) · 2,302,585)⌋, φ × 1000 (ln 10 × 10⁶ in the
  denominator).
- `Table` = the §18 item 8 expectation table, one `Scenario` per row and shape in
  `tests/support/scenarios.rs`, plus the shapes an interactive shell adds (`+` rows).
- `row.wire()` = the `sys jobs` row; `exe_word(s)` = `s` when `s` matches `^[A-Za-z0-9._+-]{1,32}$`, else
  `-`.

## 1. Specs (TSV)

Columns: `id`, `kind`, `functions`, `statement`, `subsumes`, `difficulty` (the Rust features a proof must
handle), `blocking_gaps` (rows of `docs/AUDIT-trust-for-test-migration-2026-10-15.md` §7).

```tsv
id	kind	functions	statement	subsumes	difficulty	blocking_gaps
JB-PHI	contract	phi_milli	∀ q, μ: phi_milli(q, μ) = φ(q, μ), saturating at u64::MAX. The first whole second φ reaches 3000 and 8000 is 70 s and 185 s at μ = 10 s, 415 s and 1,106 s at μ = 60 s, 829 s and 2,211 s at μ = 120 s (the design's §11.7 table: 69 s, 3 min 4 s; 6 min 55 s, 18 min 25 s; 13 min 49 s, 36 min 50 s); φ(9 s, 10 s) = 390.	U::phi_crosses_the_design_table_at_its_times	u128 arithmetic, try_from	0,1,3
JB-ROWNUM	contract	wire::phi_text, wire::age_words	phi_text prints φ × 1000 with two decimals below 10, one below 100, none above, trailing zeros and point trimmed ("0", "0.43", "8.27", "15.2", "130"); age_words prints whole seconds, minutes, or hours and minutes.	U::row_numbers_print_as_the_examples_do	format!, str trimming	0,1,3,5
JB-CMDCLASS	contract	command_class	command_class(c, k) = the basename of c's first word that is not a NAME=value assignment, cut to 32 bytes on a char boundary, then a space and k's word.	U::the_command_class_is_the_first_words_basename_plus_the_kind	split_whitespace, rsplit, char boundaries	0,1,3,5,7
JB-CLASSTABLE	refinement	ClassTable::learn, ClassTable::mu	Each class keeps the last CLASS_RING medians; mu(c) is their lower median once there are OWN_MIN; the table holds at most CLASS_TABLE classes, dropping the least recently taught.	U::the_class_table_learns_medians_and_drops_the_least_recently_used	HashMap, VecDeque, a logical clock	0,1,2,3
JB-TIERB	table-property	map_leaf_frame	The first word of a sampled leaf frame, `$…` and `_nocancel` suffixes and leading underscores stripped, maps by the §11.8 table (timer, lock, read, event, mutex, children, ipc for `mach_msg*`); any other symbol, a program's own included, is Unparsed.	U::tier_b_maps_libsystem_leaf_frames_only	&str matching	0,1,3,5
JB-TIERA-QUEUE	contract	wait::name_job	A leaf asleep on an event queue waits on what the queue holds: a read on a regular file is follow (with its fd), a read on a listening socket is listen (with its port), a queue whose every entry is a read or write on an idle connection is tcp, timer entries alone are timer, anything else (or unlisted entries, lane L6 off) is event-loop. A queue nobody sleeps on names nothing.	U::a_sleeping_kqueue_names_follow_listen_timer_or_an_event_loop	slices of enums, closures	0,1,3,13
JB-TIERA-TCP	contract	wait::name_job	tcp names a leaf's wait only when the connection is what it waits on: never while a sleeping queue holds anything besides idle connections, never while the leaf holds a listening socket (it serves: listen), and otherwise when it holds an ESTABLISHED or SYN_SENT connection with 0 bytes queued. An idle dev server with a keep-alive client never reads stuck.	U::a_connection_names_the_wait_only_when_nothing_else_does; S::an_idle_dev_server_with_a_keep_alive_client_listens_and_never_reads_stuck; S::a_connection_is_the_wait_when_the_queue_waits_on_it_alone	slices of enums, SocketAddr	0,1,3,13
JB-TIERA-PIPE	contract	wait::name_job	A reader flagged PIPE_WANTR (lane L7) waits on the pipe's writer and counts as that writer does (followed 8 deep, or as the writer's job counts); a detached job whose writer is its own consumer serves (legit); a writer blocked on a full peer buffer is pipe-full and counts as the reader; without L7 a pipe names nothing; a thread uninterruptible ≥ 10 s is kernel.	U::a_pipe_counts_as_its_writer_and_a_full_one_as_its_reader; U::the_consumers_pipe_is_serving_and_a_long_d_state_is_kernel; S::a_detached_job_reading_its_consumers_pipe_serves; S::without_lane_l7_a_pipe_reads_unnamed_and_the_lane_says_so	recursion with a depth bound, HashMap scans	0,1,3,13
JB-TIERA-LINUX	contract	wait::is_sleep_wchan, wait::is_event_wchan, wait::name_job	A Linux leaf's public wait channel names a sleep timer, an event wait event-loop, an inotify read follow, a pipe read pipe and a pipe write pipe-full.	U::linux_wait_channels_name_sleep_and_event_waits	&str matching	0,1,3,5
JB-TIERA-TTY	contract	wait::name_job	Only a job holding the terminal, with the input queue empty and a mode word stamped after its last output, gets a tty name: canonical without echo is tty-password, non-canonical is tty-raw; the shell alone in front of its block is tty-builtin-read.	U::the_tty_names_read_the_fresh_mode_word_only_for_the_foreground_job	Option filters	0,1,3
JB-TIERA-PROGRAMCHECK	contract	wait::name_job, wait::rank, wait::standing_of, step::classify_tick	A leaf whose reading says macOS's program check holds it (JobProc.program_check, the host's AtStart::Held) waits on program-check before anything its descriptors say; the name ranks first among a job's leaves, comes before the terminal mode's tty-raw and tty-password (a held leaf reads no terminal), and is legitimate (φ 0, never quiet or stuck, no_progress never) from the reading that names it, with no two-reading debounce (a normal start reads held for under 0.3 ms); and a reader blocked on a pipe whose writer is held reads pipe and counts as legitimate through it. Under Fault::IgnoreProgramCheck alone the leaf reads unnamed, and the same call goes quiet.	S::every_scenario_of_the_expectation_table_reads_as_the_table_says; C::ignoring_the_program_check_is_caught_by_held_wait_never_quiet	a bool guard, recursion through a pipe's peer	0,1,3,13
JB-WORDS	table-property	Verdict, Class	The verdict set is exactly the nine §11.7 words, the class set the nine §11.5 words, each round-trips through from_word, `done` aliases exited, and only `exited` is in both.	S::verdict_and_class_words_are_closed_and_share_only_exited	const tables	0,1,3
JB-EXE	invariant	wire::is_exe_word, wire::exe_word, wire::person_name, JobRow::wire, JobRow::said, Findings::wire_lines	Every name a row or finding prints is exe_word(name), and task= is exe_word(task); the person's sentence drops control and format characters and cuts names to 32 characters. So no byte a binary's author chose (an escape sequence, a bidi override, a quote, `=`) reaches what an agent reads. The name is the executable's basename where the reader gave one, else p_comm.	S::a_process_name_prints_only_as_a_safe_word; S::foreign_names_never_reach_the_row_or_the_sentence_raw; S::the_row_names_the_executable_not_its_cut_short_name; S::a_held_finding_prints_one_row_with_its_age	char predicates, iterator filters	0,1,3,5
JB-ROW	contract	JobRow::wire, Jobs::absorb	The §11.10 example rows A–G, run in W, print word for word as the design writes them (field order: id, kind, consumer, promoted, task, pgid, exe, class, wait fields, named, stop, input, cpu_mc, zombies, quiet_ms, phi, mu, exit, verdict, no_progress), and `no_progress=1` prints after `verdict=` exactly when the row's no_progress holds (the table test holds every row of every scenario to it).	S::example_a_password_prompt_in_a_background_tab_waits_for_you; S::example_b_an_agent_call_hung_on_a_silent_tcp_peer_is_stuck; S::example_c_a_cargo_lock_wait_is_capped_until_named_then_fine_behind_a_running_holder; S::example_d_tail_f_in_an_agent_foreground_call_never_returns; S::example_e_a_spinning_full_screen_program_with_an_unread_key_is_the_gates_stuck; S::example_f_a_held_stop_under_an_agent_call_is_stuck_and_a_parked_one_is_parked; S::example_g_a_zombie_pile_under_a_logging_server_is_running_with_its_zombies_counted	the whole engine over W	0,1,2,3,13
JB-TABLE	table-property	Jobs::absorb	Every Table scenario, in each shape it names, reads the verdict, class, wait, φ and first-crossing time its row states at 60 s and 20 min (stretch 1 and the 60 s prior unless the row says otherwise). A full-screen program in front with no key queued reads tty-raw however often it wakes (never POLLING); a typed sleep-poll loop is one POLLING job that reaches stuck at 18 min 25 s × stretch.	S::every_scenario_of_the_expectation_table_reads_as_the_table_says; S::a_typed_sleep_poll_loop_is_one_polling_job_that_reaches_stuck; S::a_full_screen_program_waking_often_with_no_key_queued_reads_keys_not_polling	the whole engine over W	0,1,2,3,13
JB-UNITS-BLOCK	invariant	units::discover, Jobs::absorb	An open OSC 133 block is one job keyed by (tab, block): its members are the shell, the shell's own group and each group in front while it is open, so ids never repeat within a tab; it exits at D with D's status (the tab's ended block's, when the next command's block replaced it before the tick) and never orphans the shell, and its exited row names the last member other than the shell that ran in it. Ctrl-Z makes the stopped group a background job keeping its id and quiet clock, and a command typed ahead of the stop, in front before D was read, is the next block's own job; `fg` makes it the next block's foreground job again (one job, the block its key from then on), and that block's D closes it. A block first learned a tick after its group in front was swept is that group's job too, never a second one beside it, keeping its `j<pgid>` id. A block resumes only the job its group in front belongs to, never by a member of the shell's own group (a process substitution, an orphan's survivor), and nothing while its own command the shell started is stopped behind the group in front (`fg %1` read before the stop's D). A block under which the last group in front (other than the shell's, also when a tick saw the shell take the terminal back before D; never one a tick saw in front while a command the shell started was stopped outside it, unless the job's previous pass saw it there too, as bash moving on through a list does; a debugger's traced child is no such stop) of a job whose block ended in a stop (Block::stopped) is in front again resumes that job, a stop and `fg` no tick saw; a block typed ahead of one that ended otherwise resumes nothing, unless the group in front is the job's own (its key's: a parked or background job a block resumed, a `fg` typed ahead and read after the pass that saw the resume). A block that resumes a job takes its group in front as the job's last (a `cmd &` job `fg` resumed). A block's own job none of whose command lives on outside the unit (in the shell's tree, read this pass: a server launchd adopted, or a command that exited before the pass read it, does not) gives way to the job its group in front is (`fg`'s block, read before the shell hands the terminal over; a command list that ends in `fg`; a `fg` typed ahead and read before the last block's D), which takes the block and what the block's job held of it; a block's job that never had a group of its own in front (`fg`'s) leaves no row, and one that had exits with its row, its block's status once D is read, and its survivors an orphan's. While its own command lives on outside the group in front (a debugger whose child it put in front, a nested shell that brought one of its jobs forward), the block's job takes that job in, members and all, and that job leaves no row. A block that ended in a stop while a member other than the shell lives parks its job in the background as Ctrl-Z does (Ctrl-Z and `bg` no tick saw). A stop is parked whenever job control took the terminal from the stopped member's group (the shell has it, or the next command does), and held only while that group is in front; a member stopped in another group while one of the job's own groups runs in front (a debugger at its prompt, the command bash moved on to) is no stop of the job's, which is judged by what runs in front. A job that ran in the background keeps its command when a block resumes it. A job whose block a newer one replaced hands the newer block's job the members it holds of it (the shell, a group the next command put in front before `D` was read). The shell joining a job is a baseline, never a birth, and a member leaving it alive (the shell at a stop, one a newer block took) is no reap. A background job is judged by its lines whatever the mode word says, since the word is the foreground reader's (the line editor's, raw at its prompt). No job is credited the tab's output over a tick in which the shell wrote a mark of its own (JobTab::shell_ms after the tick's floor; a block's own `C`, its started_ms, is its job's floor at every tick; a job that ran in the background or stopped, since the shell's notice of `fg` follows `fg`'s `C`, takes none of the tab's output stamped up to FG_NOTICE_MS past the resuming tick's read of the group in front (JobTick::read_ms, never before its clock), which may be the shell's notice of `fg`, and takes what it prints after that at its next tick however late).	S::every_command_of_a_block_is_one_job_with_one_id; S::fg_makes_a_parked_job_the_next_blocks_foreground_job_again; S::a_block_learned_after_its_group_in_front_is_that_groups_one_job; S::a_sole_background_job_is_judged_by_its_lines_under_the_line_editors_raw_word; S::a_command_followed_by_another_between_two_ticks_keeps_its_status; S::an_exited_commands_row_names_its_program_not_the_shell_left_behind; S::a_lone_background_job_is_not_credited_with_a_tick_the_shell_wrote_in; S::a_stop_and_fg_no_tick_saw_keep_the_one_job_and_its_quiet_clock; S::the_next_command_typed_ahead_is_its_own_blocks_job; S::a_command_typed_ahead_of_one_no_tick_saw_in_front_is_its_own_blocks_job; S::ctrl_z_and_bg_no_tick_saw_park_the_one_job; S::fg_never_names_the_job_it_resumes; S::a_blocks_own_c_fed_after_a_pass_never_voids_its_jobs_lines; S::a_background_job_fg_resumed_keeps_its_group_through_a_stop_and_fg_no_tick_saw; S::a_stop_and_fg_no_tick_saw_keep_a_later_command_of_a_block_its_jobs; S::fgs_notice_after_the_resuming_pass_clock_is_none_of_the_jobs_progress; S::fgs_block_read_before_the_shell_hands_the_terminal_over_resumes_the_parked_job; S::a_survivor_in_the_shells_own_group_never_hides_the_job_fg_resumes; S::a_parked_jobs_process_in_the_shells_own_group_never_takes_the_next_block; S::a_debuggers_child_it_puts_in_front_is_its_blocks; S::a_debuggers_traced_stop_keeps_it_the_group_fg_resumes; S::a_stopped_member_out_of_front_is_no_held_stop; S::a_typed_ahead_fg_read_before_the_last_blocks_d_resumes_the_parked_job; S::a_command_list_ending_in_fg_gives_way_past_a_survivor_its_recipe_left; S::fg_of_another_job_read_before_a_stops_d_never_merges_it_into_the_stopped_one; S::bash_moving_on_past_a_stopped_command_keeps_the_next_one_its_blocks; S::ctrl_z_carries_the_jobs_id_and_quiet_clock_into_the_background; S::an_exited_block_keeps_its_row_and_status_for_a_minute	BTreeMap/HashMap bookkeeping across ticks	0,1,2,3,13
JB-UNITS-DETACHED	invariant	units::discover, Jobs::absorb	A child that leads its own session with no terminal is a job whose consumer is its parent; it stays an agent call when its leader exits unreaped; what outlives a job is an orphan with consumer nobody, no φ and findings only; a reused pid with a new start is a new job, and an exited job never shares its id with a live one.	S::a_call_whose_leader_exits_leaves_an_orphan_with_findings_only; S::a_call_whose_leader_exits_unreaped_reads_exited_as_the_call; S::a_reused_pid_is_a_new_job	maps keyed by (pid, start)	0,1,2,3,13
JB-STEP	refinement	step::classify_tick	An episode opens at the first suspect verdict and says each rank once until progress, exit or a legit class held 2 ticks closes it; the shown wait name changes only after 2 agreeing ticks; program-check alone shows and counts at once (JB-TIERA-PROGRAMCHECK); a job whose command is known (a shell job's leader or block command, or the command a call's join names; an agent's call no join names has none, its leader being the agent's shell wrapper) with ≥ 3 progress intervals teaches its command class at exit, and μ is max(10 s, own, else its class (none without a command), else 60 s).	S::an_episode_opens_once_and_a_new_one_after_progress; S::the_shown_wait_name_changes_after_two_agreeing_ticks; S::a_completed_job_teaches_its_command_class; S::an_agent_calls_class_is_the_command_its_join_names_never_its_shell_wrapper	a state machine over a struct	0,1,2,3
JB-DEMAND	contract	units::demand_of	Demand is Fast while any live row runs, is stuck or is suspect, Slow when the live rows are legitimate waits not all the person's, and Idle when nothing runs or only the person is waited on; a program-check finding or an open backed-up episode moves no Demand (the host reads them with no window).	S::demand_is_fast_for_suspects_slow_for_legit_waits_and_idle_for_the_person; S::a_held_program_leaves_demand_to_the_jobs; S::a_program_at_the_persons_raw_read_parks_demand_even_while_it_writes_files; S::a_tab_is_parked_only_while_every_live_job_waits_at_the_persons_read; S::a_full_screen_program_logging_to_its_own_file_parks_demand	iterator over rows	0,1,3
JB-FINDINGS	contract	Jobs::findings, Jobs::note_held	Findings count stopped processes and zombies (first seen ≥ 10 s, grouped by parent, with their rate) for every user, D-state ≥ 10 s and own-uid processes against the per-user limit for aterm's user. Own-uid processes macOS's program check holds (JobTick.held, members of a job or not) are findings each held since the first reading that saw its (pid, exec) held, so an exec starts a new time, with its member's tab (none outside every job) and, of the ones aterm started, the count and the oldest. One backed-up episode opens at the first reading where a key held now was held at the reading before or three are held at once, and closes at the second reading in a row with none held; the closing view alone says it drained, open to its last held reading.	S::findings_count_stopped_d_state_and_the_process_table; S::held_rows_count_from_their_first_sighting_and_open_one_episode_when_backed_up	maps across ticks	0,1,2,3
JB-MODEL	refinement	step::classify_tick, Jobs::absorb	Each covered Table run, projected tick by tick onto (cls, q band, v), is a run of the StuckVerdict model (aterm_models_misc::stuck_verdict_model, ty_model!). Each injected defect (accrue on a legit wait, reset the age on a refused read, let unnamed reach stuck, never raise a named wait, count CR redraws, idle churn or logical-write noise as progress, ignore the program check) makes some run leave the model or miss its flag. The model's held input is read from each view's program-check findings (the job waits, and its waiting process is one of them), never from the row: Hold on the first such tick, Release on the first after.	C::every_covered_scenario_is_a_run_of_the_model; C::accruing_on_a_legit_wait_is_caught_by_legit_wait_never_accrues; C::resetting_the_age_on_a_refused_read_is_caught_by_refused_reading_is_not_zero_progress; C::letting_unnamed_reach_stuck_is_caught_by_unnamed_never_enters_stuck; C::never_raising_a_named_wait_is_caught_by_flagged_on_time; C::a_cr_redraw_counted_as_progress_never_flags_the_spinner_over_a_silent_socket; C::churn_without_work_counted_as_progress_never_flags_the_sleep_poll_loop; C::logical_write_noise_counted_as_progress_never_flags_the_spinner; C::ignoring_the_program_check_is_caught_by_held_wait_never_quiet	model interpretation, fault injection	0,1,2,3,13
JB-NOPROGRESS	contract	step::classify_tick, Jobs::absorb, JobRow::wire	A row says no_progress exactly when it is an agent's call (kind agent-call; consumer agent-fg, agent-bg or agent) at verdict quiet with φ × 1000 ≥ PHI_STUCK_MILLI and Σ ΔCPU over its members across the current suspect run × 60,000 < the run's quiet_ms × NO_PROGRESS_CPU_NS_PER_MIN (1 ms a minute, the design's first bound). The sum restarts with the suspect clock (progress, a legit class held 2 ticks, a new job); a refused reading in the run, or the baseline reading after one, makes the run's CPU unknown and the word false until the clock restarts. It is a word beside the verdict, never a verdict: the verdict stays quiet and unnamed never reaches stuck. In W: deadlock.setsid (I4's shape) reads it from 1,106 s on and is still quiet at 26 h; deadlock_eperm.setsid only in its third, clean run, from 3,506 s; trickle.setsid (3 ms a minute), every10.setsid (it prints), spin.setsid (busy) and sleep.pty (the person's) never; every other row of the table that says it is a quiet agent's call at φ ≥ 8.	S::every_scenario_of_the_expectation_table_reads_as_the_table_says; C::every_covered_scenario_is_a_run_of_the_model; C::letting_unnamed_reach_stuck_is_caught_by_unnamed_never_enters_stuck	u128 arithmetic, an Option sum carried across ticks, the whole engine over W	0,1,2,3,13
```

## 2. Test → spec map (TSV; all 81)

```tsv
file	test	primary	also
U	phi_crosses_the_design_table_at_its_times	JB-PHI	
U	row_numbers_print_as_the_examples_do	JB-ROWNUM	JB-PHI
U	the_command_class_is_the_first_words_basename_plus_the_kind	JB-CMDCLASS	
U	the_class_table_learns_medians_and_drops_the_least_recently_used	JB-CLASSTABLE	
U	tier_b_maps_libsystem_leaf_frames_only	JB-TIERB	
U	a_sleeping_kqueue_names_follow_listen_timer_or_an_event_loop	JB-TIERA-QUEUE	
U	a_connection_names_the_wait_only_when_nothing_else_does	JB-TIERA-TCP	JB-TIERA-QUEUE
U	a_pipe_counts_as_its_writer_and_a_full_one_as_its_reader	JB-TIERA-PIPE	
U	the_consumers_pipe_is_serving_and_a_long_d_state_is_kernel	JB-TIERA-PIPE	
U	linux_wait_channels_name_sleep_and_event_waits	JB-TIERA-LINUX	
U	the_tty_names_read_the_fresh_mode_word_only_for_the_foreground_job	JB-TIERA-TTY	
S	example_a_password_prompt_in_a_background_tab_waits_for_you	JB-ROW	JB-TIERA-TTY, JB-DEMAND
S	example_b_an_agent_call_hung_on_a_silent_tcp_peer_is_stuck	JB-ROW	JB-TIERA-TCP
S	example_c_a_cargo_lock_wait_is_capped_until_named_then_fine_behind_a_running_holder	JB-ROW	JB-STEP, JB-CLASSTABLE
S	example_d_tail_f_in_an_agent_foreground_call_never_returns	JB-ROW	JB-TIERA-QUEUE
S	example_e_a_spinning_full_screen_program_with_an_unread_key_is_the_gates_stuck	JB-ROW	
S	example_f_a_held_stop_under_an_agent_call_is_stuck_and_a_parked_one_is_parked	JB-ROW	JB-UNITS-BLOCK
S	example_g_a_zombie_pile_under_a_logging_server_is_running_with_its_zombies_counted	JB-ROW	JB-FINDINGS
S	every_scenario_of_the_expectation_table_reads_as_the_table_says	JB-TABLE	every JB-TIERA-* spec, JB-NOPROGRESS, JB-ROW
S	a_call_whose_leader_exits_leaves_an_orphan_with_findings_only	JB-UNITS-DETACHED	JB-FINDINGS
S	a_call_whose_leader_exits_unreaped_reads_exited_as_the_call	JB-UNITS-DETACHED	
S	an_exited_block_keeps_its_row_and_status_for_a_minute	JB-UNITS-BLOCK	JB-EXE
S	ctrl_z_carries_the_jobs_id_and_quiet_clock_into_the_background	JB-UNITS-BLOCK	
S	demand_is_fast_for_suspects_slow_for_legit_waits_and_idle_for_the_person	JB-DEMAND	
S	a_held_program_leaves_demand_to_the_jobs	JB-DEMAND	JB-FINDINGS
S	a_program_at_the_persons_raw_read_parks_demand_even_while_it_writes_files	JB-DEMAND	JB-TIERA-TTY
S	a_tab_is_parked_only_while_every_live_job_waits_at_the_persons_read	JB-DEMAND	JB-TIERA-TTY
S	a_full_screen_program_logging_to_its_own_file_parks_demand	JB-DEMAND	JB-TIERA-TTY
S	findings_count_stopped_d_state_and_the_process_table	JB-FINDINGS	
S	held_rows_count_from_their_first_sighting_and_open_one_episode_when_backed_up	JB-FINDINGS	
S	a_held_finding_prints_one_row_with_its_age	JB-EXE	JB-FINDINGS
S	a_detached_job_reading_its_consumers_pipe_serves	JB-TIERA-PIPE	
S	without_lane_l7_a_pipe_reads_unnamed_and_the_lane_says_so	JB-TIERA-PIPE	
S	the_shown_wait_name_changes_after_two_agreeing_ticks	JB-STEP	
S	a_reused_pid_is_a_new_job	JB-UNITS-DETACHED	
S	an_episode_opens_once_and_a_new_one_after_progress	JB-STEP	
S	a_completed_job_teaches_its_command_class	JB-STEP	JB-CLASSTABLE
S	an_agent_calls_class_is_the_command_its_join_names_never_its_shell_wrapper	JB-STEP	JB-UNITS-DETACHED
S	verdict_and_class_words_are_closed_and_share_only_exited	JB-WORDS	
S	every_command_of_a_block_is_one_job_with_one_id	JB-UNITS-BLOCK	JB-EXE
S	a_typed_sleep_poll_loop_is_one_polling_job_that_reaches_stuck	JB-TABLE	JB-UNITS-BLOCK
S	fg_makes_a_parked_job_the_next_blocks_foreground_job_again	JB-UNITS-BLOCK	JB-TIERA-TTY
S	a_block_learned_after_its_group_in_front_is_that_groups_one_job	JB-UNITS-BLOCK	
S	a_sole_background_job_is_judged_by_its_lines_under_the_line_editors_raw_word	JB-UNITS-BLOCK	JB-TIERA-TTY
S	a_command_followed_by_another_between_two_ticks_keeps_its_status	JB-UNITS-BLOCK	
S	an_exited_commands_row_names_its_program_not_the_shell_left_behind	JB-UNITS-BLOCK	JB-EXE
S	a_lone_background_job_is_not_credited_with_a_tick_the_shell_wrote_in	JB-UNITS-BLOCK	
S	a_stop_and_fg_no_tick_saw_keep_the_one_job_and_its_quiet_clock	JB-UNITS-BLOCK	
S	the_next_command_typed_ahead_is_its_own_blocks_job	JB-UNITS-BLOCK	
S	a_command_typed_ahead_of_one_no_tick_saw_in_front_is_its_own_blocks_job	JB-UNITS-BLOCK	
S	ctrl_z_and_bg_no_tick_saw_park_the_one_job	JB-UNITS-BLOCK	
S	fg_never_names_the_job_it_resumes	JB-UNITS-BLOCK	
S	a_blocks_own_c_fed_after_a_pass_never_voids_its_jobs_lines	JB-UNITS-BLOCK	
S	a_background_job_fg_resumed_keeps_its_group_through_a_stop_and_fg_no_tick_saw	JB-UNITS-BLOCK	
S	a_stop_and_fg_no_tick_saw_keep_a_later_command_of_a_block_its_jobs	JB-UNITS-BLOCK	
S	fgs_notice_after_the_resuming_pass_clock_is_none_of_the_jobs_progress	JB-UNITS-BLOCK	
S	fgs_block_read_before_the_shell_hands_the_terminal_over_resumes_the_parked_job	JB-UNITS-BLOCK	
S	a_survivor_in_the_shells_own_group_never_hides_the_job_fg_resumes	JB-UNITS-BLOCK	
S	a_parked_jobs_process_in_the_shells_own_group_never_takes_the_next_block	JB-UNITS-BLOCK	
S	a_debuggers_child_it_puts_in_front_is_its_blocks	JB-UNITS-BLOCK	
S	a_debuggers_traced_stop_keeps_it_the_group_fg_resumes	JB-UNITS-BLOCK	
S	a_stopped_member_out_of_front_is_no_held_stop	JB-UNITS-BLOCK	JB-STEP
S	a_typed_ahead_fg_read_before_the_last_blocks_d_resumes_the_parked_job	JB-UNITS-BLOCK	
S	a_command_list_ending_in_fg_gives_way_past_a_survivor_its_recipe_left	JB-UNITS-BLOCK	
S	fg_of_another_job_read_before_a_stops_d_never_merges_it_into_the_stopped_one	JB-UNITS-BLOCK	
S	bash_moving_on_past_a_stopped_command_keeps_the_next_one_its_blocks	JB-UNITS-BLOCK	
S	an_idle_dev_server_with_a_keep_alive_client_listens_and_never_reads_stuck	JB-TIERA-TCP	JB-TABLE
S	a_connection_is_the_wait_when_the_queue_waits_on_it_alone	JB-TIERA-TCP	
S	a_full_screen_program_waking_often_with_no_key_queued_reads_keys_not_polling	JB-TABLE	JB-TIERA-TTY
S	a_process_name_prints_only_as_a_safe_word	JB-EXE	
S	foreign_names_never_reach_the_row_or_the_sentence_raw	JB-EXE	
S	the_row_names_the_executable_not_its_cut_short_name	JB-EXE	
C	every_covered_scenario_is_a_run_of_the_model	JB-MODEL	JB-TABLE, JB-NOPROGRESS
C	accruing_on_a_legit_wait_is_caught_by_legit_wait_never_accrues	JB-MODEL	
C	resetting_the_age_on_a_refused_read_is_caught_by_refused_reading_is_not_zero_progress	JB-MODEL	
C	letting_unnamed_reach_stuck_is_caught_by_unnamed_never_enters_stuck	JB-MODEL	JB-NOPROGRESS
C	never_raising_a_named_wait_is_caught_by_flagged_on_time	JB-MODEL	
C	a_cr_redraw_counted_as_progress_never_flags_the_spinner_over_a_silent_socket	JB-MODEL	
C	churn_without_work_counted_as_progress_never_flags_the_sleep_poll_loop	JB-MODEL	
C	logical_write_noise_counted_as_progress_never_flags_the_spinner	JB-MODEL	
C	ignoring_the_program_check_is_caught_by_held_wait_never_quiet	JB-MODEL	JB-TIERA-PROGRAMCHECK
```

## 3. Coverage

- 64 tests, 22 specs, all in `src/jobs/` and the two integration tests above.
- Not recovered here yet: the 30 tests of the earlier modules, `src/model.rs` (9: attribution, deltas,
  naming, cores, clusters, memory), `src/trend.rs` (9: the two averages, momentum, variance, history),
  `src/words.rs` (6: the Deck's sentences and units) and `src/ink.rs` (6: OkLab, the ramp and the scope
  ladder).
- The job probe that feeds this engine (`aterm_sysprobe::jobs`) has its own tests in that crate; the live
  check of both together is `crates/aterm-sysprobe/examples/stuck_live.rs`, a dev example run by hand.

## 4. Tests that are not spec samples

None.

## 5. Implied specs that look wrong, or disagree with the design

- JB-NOPROGRESS's bound, `NO_PROGRESS_CPU_NS_PER_MIN` (1 ms a minute), is the design's initial value (D2).
  A partial `proc_pid_rusage` reading over 60 s showed a live `targo --unverified test` call's waiting
  members (`zsh -c`, `targo`, an idle forked test child) used 0 ns and 0 wakeups; a hung test binary itself
  (libtest's main thread in `recv` after its 60 s note, and any threads finished tests left running) remains
  to be measured before relying on the bound.
- JB-NOPROGRESS sums the members' own CPU only. Children born and reaped between two readings are never
  members, so work done in them counts as zero, and a call whose work runs only in such short forks under
  a sleeping parent can say the word while it uses CPU. Adding the reaped children's CPU to the run (not to
  the bands) would close it; that widens the design's "Σ CPU over the suspect run", an owner call.

- JB-TIERA-TCP departs from the §11.5 table's literal order (tcp before follow and listen): a connection
  names the wait only when nothing else does, because the literal order named an idle dev server with a
  keep-alive client `tcp` and read it stuck at 18 min.
- JB-TABLE's tty-raw rule departs from §11.5's rule order (POLLING, rule 8, before WAITING, rule 10) for a
  job in front with no key queued and a fresh mode word: a Claude Code that printed nothing for 40 s made
  1,109 BSD system calls a second on this Mac, which rule 8 would have read as polling and then stuck.
- JB-NOPROGRESS sits on `JobRow` and its `sys jobs` row: the design puts it on `JobFact` (system
  awareness §14.4), which is not built yet.
- JB-NOPROGRESS follows the design's "a refused reading (unknown, never zero)" for the whole run: one
  refused reading anywhere in a hang hides the word until the clock restarts, however long the hang
  lasts after it.
- JB-UNITS-BLOCK keys the shell's foreground job by its block, as §11.3 states; the first build keyed it by
  the group in front, which gave one job (and one duplicate id) per command of a block.

## 6. Proof order (easiest, high value first)

1. JB-PHI, JB-CMDCLASS, JB-TIERB, JB-WORDS: scalar arithmetic and closed tables.
2. JB-EXE: character predicates over a string; the property most worth proving, since it is the
   foreign-text boundary.
3. JB-TIERA-*: pure functions over descriptor lists.
4. JB-STEP, JB-CLASSTABLE, JB-DEMAND, JB-NOPROGRESS: state machines; JB-MODEL already binds JB-STEP to a
   checked model.
5. JB-UNITS-*, JB-ROW, JB-TABLE, JB-FINDINGS: the whole engine across ticks; they stay tests.
