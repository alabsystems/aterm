<!-- Copyright 2026 Andrew Yates -->
<!-- SPDX-License-Identifier: Apache-2.0 -->

# aterm-pty: spec recovery from its 157 unit tests

> Status 2026-10-16, after the merge with the tests-and-gates audit: the rows naming 58 tests it
> deleted are gone, and the spec rows no longer cite those tests. Counts elsewhere in this file
> were taken before.

> Status 2026-10-16: this is the crate's migration worklist. Each of the 49 specs below is
> proved through Trust/Clean about the compiled Rust function, then the tests it subsumes are
> deleted in the same commit. Nothing has been proved or deleted yet. The 4 tests in §4 sample no
> spec; act on them now.

Source: `crates/aterm-pty` (read only). I read every `#[test]` (src/lib.rs 5,
src/unix_tests.rs 75, src/unix_shell_identity_tests.rs 8, src/unix_writeall_conformance_tests.rs 2,
src/windows/{cmdline 8, mod 18, resize_guard 14, shell 16, winpath 2}, tests/windows_smoke.rs 9 =
157) and the code each one reaches: `src/unix.rs` (spawn, pty pair, exec-status channel, reap,
gather), `src/lib.rs`, `src/windows/*`, and `aterm_types::env_sanitize::is_ai_env_key`. Nothing was
compiled or run. The claims below come from reading the code. File paths in the tables are relative
to `crates/aterm-pty/`; other paths are repo-relative.

The census said 158 tests. There are 157 `#[test]` fns: `tests/windows_smoke.rs` line 18 is a `//!`
doc line containing `#[test]`, and the counter included it.

Result: **49 specs**, covering **153 tests**. **4 tests are not spec samples** (§4).
One of the specs (`windows-createprocess-axiom`) is a platform axiom, not a theorem about aterm code.

## 0. Notation used in the statements

- `Plat`: the platform model that gap 8 (Trust's platform lane, Lean `PlatformSpec`) must supply. A
  per-process fd table whose entries are (object, `FD_CLOEXEC`, `O_NONBLOCK`); objects are pty pairs
  (master/slave with termios, winsize, input queue `IQ`, output queue `OQ`), pipes, fifos, sockets and
  tty devices. A process table (pid, ppid, pgid, sid, ctty, start-time `birth`, zombie flag). Syscalls
  are transitions on this state.
- `inherit(P, execve)`: the fds of P without `FD_CLOEXEC`. `dup`/`dup2`/`F_DUPFD` clear the flag;
  `F_DUPFD_CLOEXEC` and `O_CLOEXEC` set it at creation.
- `A-unix`: the POSIX/XNU/Linux facts the tests measure. They become platform axioms, not theorems
  about aterm code: `posix_openpt` and `open` honor `O_CLOEXEC`; the `read(2)` EOF/`EBADF`/zero-length
  results; canonical-mode `FIONREAD` counts only complete lines; raw mode with `VMIN` > queued reads 0;
  with `ISIG` set, `VINTR`/`VQUIT`/`VSUSP` are never queued and flush `IQ`; `ISIG`/`IXON`/`IEXTEN`/`IGNCR`
  each consume a byte; a Darwin fifo EOF is seen by `select` and not by `poll`/`kqueue`; a kernel-default
  pty is `ICANON|ECHO|OPOST`.
- `A-win`: the same for Windows: `CreateProcessW` `lpApplicationName` rules (spec
  `windows-createprocess-axiom`), ConPTY/conhost behavior, and the MSYS2 runtime dropping the record
  after a size event (measured in the `resize_guard` module docs).
- `eff(a,c,l)`: the first of a, c, l that is `Some` and non-empty (POSIX `LC_ALL` > `LC_CTYPE` > `LANG`).
- `upsert(E,(k,v))`: replace the value of the least i with `E[i].0 == k`, else append `(k,v)`.
- `lossy(x)`: `to_string_lossy`. `lower_ascii`: ASCII lowercase. `le16`: little-endian u16 bytes.
- `crt_parse`: a Clean model of the MSVCRT/`CommandLineToArgvW` command-line parser. It does not exist
  yet; `msvcrt-cmdline-roundtrip` needs it.
- `tail_after(t,x)` = `(InputStream{tail: t, switched: false}.after(x)).tail`.
- `pack(r,c)`: `ResizeGuard`'s packed u32 size word.
- Ticks: `reap_shell_scheduled`'s loop index. The test-only counters (`REAP_POLLS`, `REAP_KILL_TICK`,
  `BRIDGE_POLLS`, `BUDGET_ENDED`, `BUDGET_FOR_TEST`) are `cfg(test)` thread-locals
  that expose ghost facts the specs state. Delete them with the tests that read them once the specs are
  proved.

### Pure cores to factor out first (PLAN step 4)

- (a) `GatherStep`: a pure transition `(filled, spins, elapsed, hot, parser_idle, idle_wait_us, event) →
  continue | probe | at_gap | park_idle | park_bridge | deliver`, events being read n>0 / 0 / EINTR /
  EAGAIN / err, an `at_gap` answer, or a park result. `drain_more_nonblocking_with_idle_wait_after_gap`
  becomes a loop over it.
- (b) `read_or_wake`'s outcome as a pure function of the poll `revents`.
- (c) `tty_echo_of(&termios)` and `passes_every_byte_of(&termios)`, split from the `tcgetattr` call.
- (d) `read_exec_status_now`'s classification of `(n, byte, errkind)`.
- (e) `hangup_shell`/`reap_shell_scheduled` decisions over an injected kernel oracle (verify, waitpid,
  tcgetpgrp, killpg).
- (f) The `resize_guard` `InputTail`/`InputStream`/`HandshakeScan`/`write_with` logic is platform-neutral
  pure code inside `cfg(windows)`. Moving it to a module that is not cfg-gated lets Trust check it on
  the native target.

### Gap `W`: a Trust check of a Windows target

Every `src/windows/*` and `tests/windows_smoke.rs` spec (16 specs) is `cfg(windows)`. No row in
audit §7 covers a Trust check of `x86_64-pc-windows-msvc`, and AGENTS.md allows stock Rust only for
cross targets. The blocking-gaps column marks these specs `W`. Either refactor (f) moves the pure part
to native code (that clears `W` for `input-tail-tracker`, `handshake-scan`, `resize-guard-write-protocol`
and, with the same move, `resize-signal-encode`, `msvcrt-cmdline-roundtrip`, `env-block-shape`, `path-union`
and `windows-shell-name-pure`), or audit §7 gains a new row for a Trust Windows lane.

## 1. Specs (TSV)

Columns: `id`, `kind`, `functions`, `statement`, `subsumes (primary tests; secondary in §2)`,
`difficulty (Rust features the proof must handle)`, `blocking_gaps` (audit §7 row numbers, plus `W`).
Gap 0 (a seal that publishes Clean rows) blocks every spec and is listed on each one.

```tsv
id	kind	functions	statement	subsumes	difficulty	blocking_gaps
winsize-pixels	contract	winsize_pixels	∀ rows, cols: u16, px: Option<(u16,u16)>: winsize_pixels(rows,cols,px) = (0,0) if px = None or px = Some((w,h)) with w = 0 ∨ h = 0; else (min(cols·w, 65535), min(rows·h, 65535)), computed in u32 without wrapping. Total; never panics.	-	let-else on Option; u16→u32 widening multiply; u16::try_from(u32).unwrap_or	0,3
write-step-classify	contract	classify_write_result, WriteStep	∀ r: isize, e: bool: classify_write_result(r,e) = Retry if r < 0 ∧ e; Stop if r < 0 ∧ ¬e; Stop if r = 0 (any e); Advance(r as usize) if r > 0 (the cast is exact under r > 0).	-	enum return (Copy, PartialEq); isize→usize cast under a guard	0,1,3
resize-signal-encode	contract	windows::encode_resize_signal, windows::coord	∀ rows, cols: u16: encode_resize_signal(rows,cols) = le16(8) ++ le16(clamp(cols,1,32767)) ++ le16(clamp(rows,1,32767)) (6 bytes, columns before rows). coord(c,r) = COORD{x: clamp(c,1,32767) as i16, y: clamp(r,1,32767) as i16}; both components > 0.	mod::resize_signal_packet_is_opcode_then_columns_then_rows, mod::resize_signal_is_not_transposed, mod::resize_signal_clamps_into_positive_coord_range	[u8;6] with copy_from_slice into subranges; to_le_bytes; clamp; u16↔i16 casts; cfg(windows)	0,3,W
fifo-dir-in	contract	fifo_dir_in	∀ tmpdir: Option<OsString>: let d = tmpdir.map_or(b"/tmp", as_bytes); fifo_dir_in(tmpdir) = d ++ (if d.last() = Some('/') then [] else ['/']). The result always ends in '/' and never comes from an OS lookup. (See §5: an empty TMPDIR gives "/".)	unix_tests::an_unset_tmpdir_puts_the_status_fifo_in_tmp	Option<OsString> map_or_else; Vec<u8> last/push; OsStr as_bytes; cfg(macos)	0,1,3
ptsname-r-error	contract	ptsname_r_error	∀ rc: ptsname_r_error(rc).raw_os_error() = Some(rc) if rc > 0, else the errno current at the call. It never reads errno when rc > 0.	unix_tests::ptsname_r_errors_come_from_its_return_value	io::Error::from_raw_os_error vs last_os_error (errno as ghost state)	0,1,8
utf8-locale-codeset	contract	is_utf8_locale	∀ s: &str: is_utf8_locale(s) ⇔ '.' ∈ s ∧ lower_ascii(filter(≠'-', takeWhile(≠'@', s[rfind('.')+1..]))) = "utf8". So UTF-8/UTF8/utf-8/utf8, with or without an @modifier, qualify; C, POSIX, "", a name with no codeset, and ISO8859-x do not. Never panics (the slice index is in bounds and on a char boundary).	unix_tests::is_utf8_locale_classifies_codeset	&str rfind, get(range), split('@').next, chars().filter().map().collect::<String>(), String equality	0,1,3,5,7
spawn-locale-resolve	contract	resolve_spawn_locale (unix), SPAWN_UTF8_LOCALE	∀ a, c, l: Option<&str>, e = eff(a,c,l): resolve_spawn_locale(a,c,l) = [] if e = Some(x) ∧ is_utf8_locale(x); else [("LC_CTYPE", SPAWN_UTF8_LOCALE)] ++ (a non-empty ? [("LC_ALL", "")] : []). An empty string counts as unset at every level. Side condition (const): is_utf8_locale(SPAWN_UTF8_LOCALE) on both cfg arms (en_US.UTF-8 on macOS, C.UTF-8 elsewhere).	-	Option<&str> filter/or/is_some_and; nested fn; vec! of (String,String); cfg!-selected const	0,1,5
child-env-build	contract	build_child_env, aterm_types::env_sanitize::is_ai_env_key	∀ inherited I: [(OsString,OsString)], env_add A: build_child_env(I,A) = foldl(upsert, D, [(OsString::from(k), OsString::from(v)) | (k,v) ← A]), D = [(k,v) ∈ I | ¬is_ai_env_key(k)] in encounter order. Corollaries: (a) every output key is an A key or a non-denied I key; (b) a denied I key that is not an A key never appears; (c) if D holds ≤ 1 entry with key k ∈ keys(A), the output holds exactly one, carrying the value of the LAST A entry for k; (d) kept I pairs keep their relative order. Key equality is OsString byte equality (case-sensitive on every platform). is_ai_env_key classifies the lossy spelling, exact-case on unix and ASCII-case-insensitive on windows (aterm-types' own spec, cited): a non-UTF-8 key with a denied prefix is denied.	lib::build_child_env_drops_denylisted_and_keeps_overrides, lib::a_non_utf8_key_with_a_denied_prefix_is_still_denied, lib::build_child_env_replaces_the_parents_agent_home_with_the_identitys, lib::build_child_env_denies_mixed_case_keys_on_windows, lib::build_child_env_stays_case_sensitive_on_unix	impl Iterator generic parameter (fn is #[cfg_attr(trust_verify, trust::skip)]); Vec<(OsString,OsString)> push; iter_mut with &mut tuple field write and break; nested for loops; OsString equality, to_string_lossy; cross-crate call with a cfg(windows) branch	0,1,2,5,7,11,12,13
child-ctype-always-utf8	refinement	resolve_spawn_locale, build_child_env, is_utf8_locale	∀ a, c, l: Option<String>, ∀ I whose only locale keys are LC_ALL=a, LC_CTYPE=c, LANG=l (each at most once): with child = build_child_env(I, resolve_spawn_locale(a,c,l)), eff(child[LC_ALL], child[LC_CTYPE], child[LANG]) = Some(x) with is_utf8_locale(x). Also resolve_spawn_locale(a,c,l) = [] ⇔ eff(a,c,l) is already UTF-8 (no clobber). Needs ¬is_ai_env_key for LC_ALL, LC_CTYPE, LANG. aterm_models_session::spawn_locale_model is the abstract spec; this binds it to the two compiled fns and replaces the 4×4×4 sample loop.	unix_tests::spawn_locale_conformance_child_always_utf8_ctype	composition of two Vec/OsString fns; a quantified env; refinement to a ty_model	0,1,4,5,7,10,12
windows-stub-contracts	contract	windows::resolve_spawn_locale, windows::set_nonblocking	On Windows: (a) ∀ a, c, l: resolve_spawn_locale(a,c,l) = []. (b) ∀ m, b: set_nonblocking(m,b) = if b then Err(kind Unsupported) else Ok(()). Neither touches SESSIONS.	windows_smoke::resolve_spawn_locale_is_an_empty_stub, windows_smoke::set_nonblocking_true_is_unsupported	Vec::new; io::Error construction; cfg(windows)	0,1,W
input-tail-tracker	table-property	windows::resize_guard::InputTail::{step,lead}, InputStream::after	(a) Homomorphism: ∀ s, x, y: s.after(x ++ y) = s.after(x).after(y); s.after([]) = s. (b) step equals a reference VT-input boundary automaton on its whole finite domain (states Ground, Utf8(1..3), Escape, Csi, Ss3, Str × 256 bytes): ESC ⇒ Escape and CAN/SUB ⇒ Ground from every state; Ground ⇒ lead(b) (0xC2..DF ⇒ Utf8(1), 0xE0..EF ⇒ Utf8(2), 0xF0..F4 ⇒ Utf8(3), else Ground); Utf8(n) on a continuation byte ⇒ Utf8(n−1) or Ground at n = 1, else lead(b); Escape: '[' ⇒ Csi, 'O' ⇒ Ss3, ] P _ ^ X ⇒ Str, else lead(b); Ss3 ⇒ lead(b); Csi: final 0x40..0x7E ⇒ Ground, else Csi; Str: BEL ⇒ Ground, else Str. (c) switched(s.after(x)) = switched(s) ∨ ∃ i: tail_after(s.tail, x[..i]) = Csi ∧ x[i] = '_' (sticky).	resize_guard::the_input_tail_follows_the_sequences_aterm_writes, resize_guard::the_switch_follows_completed_win32_records_only	Copy enum with a u8 payload; match on (enum, u8 range) with guards; fold over a slice; finite, so (b) is decidable by evaluation; cfg(windows) (refactor (f))	0,1,3,7,W
handshake-scan	equivalence	windows::resize_guard::HandshakeScan::feed, ResizeGuard::note_output	∀ chunks c1..cn: the first j at which feed(cj) returns true is the least j such that WIN32_INPUT_REQUEST occurs in (c1 ++ … ++ cj)[..HANDSHAKE_WINDOW] ending inside cj; if none, feed never returns true. Independent of where the stream is cut. The restart rule matched' = [b = ESC] is exactly the KMP failure function, since ESC occurs in the pattern only at index 0. note_output sets records once, permanently, at that j and ignores later chunks.	resize_guard::the_request_is_found_across_read_boundaries_inside_the_window	&mut self struct fields; slice index by a field; early return in a for loop; Mutex + AtomicBool; cfg(windows)	0,1,2,7,13,W
guard-record-constants	table-property	windows::resize_guard::GUARD_RECORD, ESCAPE_KEY_RECORDS, aterm_types::keyboard::encode_key	GUARD_RECORD = ESC '[' "16;42;0;0;0;1" '_' (Vk = VK_SHIFT, Sc = 42, Uc = 0, Kd = 0 key-up, Cs = 0, Rc = 1). InputStream::default().after(GUARD_RECORD) = {tail: Ground, switched: true}. ESCAPE_KEY_RECORDS = encode_key(Named(Escape), Modifiers::empty(), KeyboardMode::WIN32_INPUT).	resize_guard::the_guard_record_is_a_shift_key_up, resize_guard::the_seam_escape_is_the_key_encoders_escape	const byte slices; cross-crate const-evaluated equality with encode_key; cfg(windows)	0,1,12,W
resize-guard-write-protocol	contract	windows::resize_guard::ResizeGuard::{new,note_resize,write_with,settle,nudge}	State (size, pending, records, input). new(Some((r,c))): size = pack(r,c); new(None): size = 0. note_resize(r,c): size := pack(r,c), and iff old size ≠ pack(r,c): pending := GUARDED_WRITES (2), an OpenConsole guard owes the client, and it returns Some(n), n the count of such armings (None otherwise). write_with(bytes, write), tail = input.tail: escape ⇔ records ∧ input.switched ∧ tail = Ground ∧ bytes = [ESC]; payload = escape ? ESCAPE_KEY_RECORDS : bytes; guarded ⇔ bytes ≠ [] ∧ tail = Ground ∧ records ∧ pending > 0 (pending decremented when guarded); wire = (guarded ? GUARD_RECORD : []) ++ payload, written by exactly one write call. On Ok(w): input := input.after(wire[..min(w,|wire|)]); reply = escape ? [landed = |payload|] : landed, landed = w − |prefix| saturating (guard bytes never counted; a lone ESC counts 1 only once its whole pair lands). On Err: pending re-incremented if guarded; error returned. Corollaries: no record without records; never inside an escape sequence or a UTF-8 character; the guard is owed until written; the first two boundary writes after a size change carry it. write_with_client(bytes, writer, write, client, runtime_in_tab) adds: for an OpenConsole guard, the first non-empty Input write after a size change first settles: settle(Input, client) takes the client turn, never while holding the input lock, and calls client(unpack(size)) once when it is owed, so a Writer::Reply write (which never settles) proceeds while the client runs and an Input write that comes meanwhile waits for it; a Writer::Reply write (the terminal's answer to a query, inside as_reply) is never guarded, never calls client, and leaves pending and the client owed to the next Input write; for an Inbox guard with records and no escape rewrite, when astral_as_records(tail, bytes) rewrites and runtime_in_tab() (asked only then) holds, payload = that rewrite, else bytes: each four-byte UTF-8 character starting at a Ground boundary becomes its surrogate pair's records (both key-downs, then both key-ups, Vk = Sc = Cs = 0), a write ending inside one stops before it when anything precedes it (reply = that prefix's length), and such a rewritten wire is written to the end within the call (reply = the caller bytes it carries). nudge(n, runtime_in_tab, client, write) does nothing unless n is the latest arming; for an OpenConsole guard it is settle(Input, client), true iff client was called; for an Inbox guard, when records ∧ pending = GUARDED_WRITES ∧ runtime_in_tab() (asked only then) ∧ tail = Ground, it sets pending := GUARDED_WRITES − 1 and writes GUARD_RECORD whole through write (input := input.after(GUARD_RECORD); on Err with nothing landed pending is restored) and answers true; else false.	resize_guard::a_resize_guards_the_next_two_writes_and_reports_only_caller_bytes, resize_guard::an_astral_character_goes_to_conhost_as_its_surrogate_records, resize_guard::only_an_astral_character_at_a_boundary_to_an_asking_conhost_is_rewritten, resize_guard::astral_records_go_only_to_a_tab_running_the_runtime, resize_guard::a_write_cut_inside_an_astral_character_leaves_it_for_the_next, resize_guard::a_rewritten_write_is_finished_across_short_writes, resize_guard::an_openconsole_resize_runs_the_client_once_before_the_next_write, resize_guard::an_unchanged_size_writes_no_record, resize_guard::an_unknown_starting_size_arms_on_the_first_resize, resize_guard::no_record_without_the_win32_input_request, resize_guard::the_record_never_splits_an_escape_sequence, resize_guard::the_record_never_splits_a_utf8_character, resize_guard::a_failed_write_keeps_the_record_owed, resize_guard::a_lone_esc_after_the_switch_is_the_escape_key, resize_guard::only_a_lone_esc_at_a_boundary_is_rewritten, resize_guard::a_reply_after_a_resize_leaves_the_openconsole_client_to_the_next_input, resize_guard::a_reply_after_a_resize_carries_no_record_and_leaves_both_owed, resize_guard::a_reply_goes_past_an_input_waiting_for_the_openconsole_client, resize_guard::a_nudge_sends_an_inbox_conhost_the_record_no_input_carried, resize_guard::a_nudge_sends_nothing_where_no_record_is_owed, resize_guard::a_nudge_runs_the_openconsole_client_the_input_did_not	Mutex guard held across an FnOnce write closure; a second Mutex never held with it; AtomicU64 arming count with compare_exchange; AtomicU32 swap; AtomicU8 fetch_update with checked_sub; AtomicBool; slice concat; io::Result; cfg(windows)	0,1,2,4,13,W
open-ptmx-redrive	contract	open_ptmx, pty_step_failed, XNU_EREDRIVEOPEN, PTMX_REDRIVES	∀ open oracle (FnMut(flags) -> fd, setting errno on −1): the first call passes O_RDWR|O_NOCTTY|O_CLOEXEC; the first fd ≥ 0 is returned as Ok(fd). After −1/EINVAL while O_CLOEXEC is set, it calls once more with only O_CLOEXEC cleared. On macOS, after −1/XNU_EREDRIVEOPEN (−6) it redrives with the same flags while redrives < PTMX_REDRIVES; once exhausted (PTMX_REDRIVES+1 losing calls) it returns Err whose message starts "posix_openpt", names EREDRIVEOPEN and carries raw errno −6. Any other errno returns Err at once with message "posix_openpt: …", the same raw errno and ErrorKind. At most PTMX_REDRIVES + 2 calls.	-	impl FnMut closure parameter; loop with counter; errno global read (last_os_error); custom error type with source(); cfg(macos) match arms with guards	0,1,4,7,8,13
exec-failure-message	contract	exec_failure	∀ target, from, runs_program, cwd, with p = lossy(target): p empty ⇒ (runs_program ? "the command" : "the shell path") ++ " is empty" ++ from. No '/' in p ⇒ "`p` " ++ (runs_program ? "is not on PATH" : "is not a full path") ++ from. Else at = (p relative ∧ cwd = Some(d) ? d/p : p) and the result is "`p` " ++ why ++ from, why = NotFound ⇒ "does not exist"; other stat error ⇒ "could not be run"; a directory ⇒ "is a folder"; mode & 0o111 = 0 ⇒ "is not executable"; else "could not be run". The fs is a stat oracle.	-	CStr to_string_lossy; Path join/is_relative; fs::metadata (effect oracle); format!	0,1,3,8
argv-nul-refusal	contract	argv_cstrings, argv_nul_refusal, program_nul_refusal, spawn_shell_with_pid_cell_px	∀ list name Ln, args: argv_cstrings(Ln,args) = Ok(v) ⇔ no args[i] contains 0, and then |v| = |args| ∧ ∀ i: v[i].as_bytes() = args[i].as_bytes() (non-UTF-8 bytes included). Otherwise Err(argv_nul_refusal(Ln, i0)), i0 = the least index holding a NUL: kind InvalidInput, message containing "argument {i0} " and "NUL". Spawn uses it for exec_command, argv_override and shell_args, so the execve argv equals the given bytes and no argument is dropped or shifted. When the shell would run (exec_command = None) and shell_override holds a NUL, spawn returns Err(InvalidInput) with program_nul_refusal("the shell (set by --shell …)") before forking; an exec_command is never refused on account of the shell.	unix_tests::a_command_argument_reaches_the_program_byte_for_byte	enumerate loop; CString::new error path; Vec<CString>; OsStr bytes; format!	0,1,4,7,8
sandbox-wrap-argv	contract	build_sandbox_wrap, spawn_shell_with_pid_cell_px (sandbox_wrap)	∀ wrapper w, profile sbpl, prog, argv: access(w, X_OK) ≠ 0 ⇒ Err(NotFound) with "fail-closed" in the message, and the caller spawns nothing. w or sbpl holds a NUL ⇒ Err(Other or InvalidInput). Else Ok((w, ["sandbox-exec", "-p", sbpl, prog] ++ argv[1..])). spawn with sandbox_wrap = Some(p) execs exactly this target and argv, so under A-unix (Seatbelt enforces p) the program runs confined.	unix_tests::build_sandbox_wrap_fails_closed_when_wrapper_missing, unix_tests::build_sandbox_wrap_produces_correct_argv_shape, unix_tests::wrapped_spawn_enforces_network_deny_via_seatbelt	CString::new; access FFI; Vec::with_capacity(min); iter().skip(1).cloned()	0,1,7,8
tty-echo-decode	contract	tty_echo, TtyEcho::swallows_input, TtySignals::signals, tty_passes_every_byte	tty_echo(fd) = None if fd < 0 or tcgetattr fails; else with t = tcgetattr(fd), Some(TtyEcho{echo: ECHO ∈ c_lflag, canonical: ICANON ∈ c_lflag, signals: ISIG ∈ c_lflag ? Some(TtySignals{intr,quit,susp} = c_cc[VINTR|VQUIT|VSUSP] with VDISABLE ↦ None) : None}); VDISABLE is 0xff off Linux and 0 on Linux. swallows_input(e) ⇔ ¬e.echo ∧ e.canonical. signals(s,b) ⇔ Some(b) ∈ {s.intr, s.quit, s.susp}. tty_passes_every_byte(fd) = None as above, else Some(c_lflag ∩ (ICANON|ISIG|IEXTEN) = ∅ ∧ c_iflag ∩ (IXON|IGNCR) = ∅). Under A-unix the master reflects the slave's termios.	unix_tests::tty_passes_every_byte_only_in_raw_mode	tcgetattr FFI; bit masks; closure over a termios array; Option::then; array contains; refactor (c) leaves a pure decoder	0,1,3,8
exec-status-verdict	contract	read_exec_status_now, wait_for_exec_status, EXEC_STATUS_SLICE, EXEC_STATUS_BUDGET	read_exec_status_now(rd) issues 1-byte reads until a non-EINTR result: n = 1 ⇒ Some(FailedBeforeExec(byte)); n = 0 ⇒ Some(Execed); EAGAIN/EWOULDBLOCK ⇒ None; other errno ⇒ Some(ChannelBroke(err)) (refactor (d)). ∀ budget: wait_for_exec_status(rd, budget) returns the first Some of read_exec_status_now calls interleaved with wait_readable_briefly(rd, min(left, EXEC_STATUS_SLICE)); NoVerdict only when every read, including one final read at or after start+budget, answered None; it returns by start + budget + one slice + one read. Corollary (A-unix): a buffered byte is returned by the first read whoever else holds a write end; a third holder can only suppress EOF, and then the wait ends by the budget as NoVerdict.	unix_tests::a_stranger_holding_the_write_end_cannot_block_the_status_wait_forever	loop with continue on EINTR; io::Error kind match; enum with an io::Error payload; Instant/Duration saturating arithmetic; read FFI	0,1,7,8
hangup-shell-target	contract	hangup_shell, hangup	∀ identity id, master m: id.pid ≤ 1 ⇒ Nothing, no signal. verify(id) ⇒ SIGHUP to process group id.pid, return Shell(id.pid). Else m < 0 ⇒ Nothing. Else f = tcgetpgrp(m): f ≤ 1 ⇒ Nothing; otherwise SIGHUP to group f, return Foreground(f). At most one signal, never to a pid that failed verify; since only a process in the terminal's session can be its foreground group, a stranger holding a recycled pid is never signalled (refactor (e)).	unix_shell_identity_tests::teardown_leaves_a_setsid_sleep_holding_the_recycled_pid_alive	enum return; killpg/tcgetpgrp FFI; call to verify	0,1,8
shell-identity-verify	contract	ShellIdentity::verify, record_spawned_shell, record_adopted_shell, process_birth, is_unreaped_child	verify(id) ⇔ id.pid > 1 ∧ (process_birth(id.pid) = Some(n) ∧ id.birth = Some(r) ? n = r : id.own_child ∧ is_unreaped_child(id.pid)). record_spawned_shell(p) = {p, process_birth(p), own_child: true}. record_adopted_shell(p,m) = {p, birth: (p leads m's tty session, via tcgetsid) ? process_birth(p) : None, own_child: false}. Under Plat (a zombie reads no birth; a reissued pid has a different start time; an unreaped child's pid is not reissued) verify holds for the recorded live shell and for its unreaped own-child zombie, and fails for a reissued pid, an adopted pid that never led the pty, and an own child after it is reaped.	unix_shell_identity_tests::adoption_records_a_birth_only_for_the_ptys_session_leader, unix_shell_identity_tests::an_unreaped_own_child_zombie_still_verifies_and_is_reaped	&self method; match on an Option pair; proc_pidinfo / /proc reads and waitid(WNOWAIT) FFI	0,2,8
reap-schedule	contract	reap_shell, reap_shell_scheduled, straggler_settled, aterm_uds::reaper::hand_off	∀ id, kill_at, deadline: reap_shell_scheduled(id, kill_at, deadline, g) returns at once if id.pid ≤ 1. For each tick t < deadline: ¬verify(id) ⇒ return, no signal, no further poll (a pid failing verify at tick 0 costs 0 polls); waitpid(WNOHANG) = pid ⇒ return; t = kill_at ∧ verify(id) just before ⇒ killpg(pid, SIGKILL), only at that tick; at t = kill_at give back the grace slot; then sleep one REAP_POLL. After the deadline, own_child ∧ verify(id) ⇒ hand off to the straggler reaper, which waits only while it verifies, so an own child is never left a zombie. reap_shell uses REAP_KILL_AT (25) < REAP_DEADLINE (200): a verified HUP-ignoring shell dies by SIGKILL at tick REAP_KILL_AT, and the loop ends between REAP_KILL_AT and REAP_DEADLINE polls.	unix_shell_identity_tests::a_verified_hup_ignoring_shell_is_still_hung_up_and_force_killed, unix_shell_identity_tests::a_shell_stuck_exiting_on_unread_output_is_reaped_not_left_a_zombie, unix_shell_identity_tests::a_shell_alive_past_the_deadline_is_reaped_when_it_exits	bounded for loop with early returns; waitpid/killpg FFI; a move closure handed to a cross-crate reaper thread; cfg(test) tick counters	0,7,8,12,13
reap-grace-accounting	invariant	InGrace::{enter,leave,drop}, reap_shell_in_background, wait_for_reaps, IN_GRACE, GRACE_LEFT	Invariant: IN_GRACE = the number of reap_shell* calls neither settled nor past their SIGKILL tick. enter increments; leave or drop decrements exactly once per slot. reap_shell_in_background(id) with id.pid > 1 enters before spawning its thread. wait_for_reaps(b) = true iff IN_GRACE reaches 0 within b. So a process that calls wait_for_reaps and then exits never cuts a pending SIGKILL escalation short.	unix_shell_identity_tests::a_quit_after_wait_for_reaps_leaves_no_hup_ignoring_shell	static Mutex<usize> + Condvar; a Drop guard; thread spawn; timed wait	0,2,8,13
read-or-wake-outcome	contract	read_or_wake, exit_fired, WAKELESS_POLL_TIMEOUT_MS	W = wake_rd, X = exit_rd (negative = absent). W ≥ 0: parks in poll with no timeout on {master, W, X}; returns Wake iff W became readable (checked before X and master); Exited iff exit_fired(X) (POLLIN|POLLHUP) and no wake; a watch error is never read as an exit; otherwise reads master (Data/Eof); never returns while none of the three is ready. W < 0: polls {master, X} for WAKELESS_POLL_TIMEOUT_MS ∈ (0, 1000]; Idle on timeout or EINTR, Exited if X fired, Eof on other poll errors, else a read of master; never blocks unboundedly. Refactor (b) leaves a pure revents → outcome function.	unix_tests::read_or_wake_answers_exited_when_the_shell_exits_while_the_slave_is_held	pollfd arrays; poll FFI; enum ReadOutcome; liveness under an environment model	0,1,8,13
drain-gather-policy	refinement	drain_more_nonblocking, drain_more_nonblocking_with_idle_wait_after_gap, bridge_poll, idle_refill_wait, batch_budget, idle_poll_us, HOT_BATCH_BUDGET, IDLE_POLL_DEFAULT_US	∀ environment trace (read results, clock, interactive_pending, parse_in_flight, at_gap answers, park results): (1) returns filled' with filled ≤ filled' ≤ buf.len(); buf[filled..filled'] is the in-order concatenation of the bytes its reads returned, nothing else written. (2) Reads only master; at wake_rd it only polls, so a queued wake byte is never consumed. (3) At each dry gap (EAGAIN), hot = interactive_pending(), budget = hot ? HOT_BATCH_BUDGET : 3 ms, in order: filled < 1024 ⇒ deliver; elapsed ≥ budget ⇒ deliver (that budget ended the gather); < 16 immediate probes since the last refill ⇒ probe; else exactly one park step: if hot ∨ parse_in_flight = 0 then (idle_wait_us = 0 ⇒ deliver without at_gap; else at_gap(filled) once, true ⇒ deliver, else idle_refill_wait for exactly idle_wait_us µs, never rounded to ms, a signal handler that runs meanwhile resuming it for the time left); otherwise at_gap(filled) once, true ⇒ deliver, else bridge_poll for BRIDGE_POLL_MS (1 ms). (4) Park Refill ⇒ reset probes, continue; Deliver (quiet timeout, POLLHUP, wake readable) ⇒ deliver. (5) read 0 (EOF) or a hard error ⇒ deliver; EINTR ⇒ retry. (6) A refill inside a park joins the same batch. (7) When at_gap answers true, the bytes queued at that gap stay unread. (8) A hot gather never takes a bridge poll. (9) drain_more_nonblocking passes idle_poll_us() = the env knob, else IDLE_POLL_DEFAULT_US (50) ∈ (0, 1000). Refactor (a) leaves a pure GatherStep to prove first.	unix_tests::drain_more_nonblocking_delivers_interactive_burst_on_first_quiet_read, unix_tests::drain_more_nonblocking_hot_caps_a_continuous_stream_at_the_hot_budget, unix_tests::drain_more_nonblocking_gathers_saturated_burst_and_survives_eof, unix_tests::drain_more_nonblocking_fence_delivers_before_a_park, unix_tests::drain_more_nonblocking_idle_wait_bridges_refill_into_same_batch, unix_tests::a_signal_does_not_end_an_armed_idle_wait	while loop with a mutable cursor and spin counter; &mut [u8] slicing; impl FnMut / &mut dyn FnMut seam; fn() -> bool; Option<&AtomicUsize> load; Instant clock; OnceLock env read; poll/select/read FFI; cfg(test) thread_local instrumentation	0,1,2,4,7,8,13
write-all-no-silent-drop	refinement	write_all, classify_write_result	∀ fd, bytes, and every sequence of write(2) outcomes: the projection off = bytes.len() − data.len(), done = data.is_empty() of write_all's loop is a behavior of the WriteAll model (Init off = 0; Progress / Interrupted), re-authored with ty_model!. (a) off never decreases and the bytes written are exactly bytes[..off] in order; (b) write_all returns with off = bytes.len() unless some write returned 0 or −1 with an errno other than EINTR; (c) EINTR never advances off and never ends the loop. Corollary over a draining pipe reader: the reader receives exactly bytes.	-	while loop over a shrinking slice (data = &data[n..]); errno read; write(2) FFI; the classify call; refinement to a model	0,4,7,8,10
blocking-write-parks	contract	write_some_blocking	∀ fd, non-empty bytes: write_some_blocking(fd, bytes) never returns Err(kind WouldBlock): EAGAIN ⇒ park in poll(POLLOUT) and retry; EINTR ⇒ retry; returns Ok(n) with 0 < n ≤ bytes.len() once a write succeeds, or the first other error.	-	loop; errno; poll/write FFI	0,7,8
read-wrapper-refines-read2	refinement	read	∀ fd, buf: read(fd, buf) performs exactly one read(2)(fd, buf.as_mut_ptr(), buf.len()) and returns its result unchanged. A-unix corollaries: 0 at EOF once the last writer closed and the pipe is drained; −1/EBADF on an invalid fd (EBADF ≠ EINTR); 0 for an empty buf, with queued bytes left unconsumed.	-	unsafe FFI with a &mut [u8] pointer and length (a dangling pointer for an empty slice)	0,8
wake-pipe	contract	make_wake_pipe, wake	make_wake_pipe() = Some((r,w)) ⇒ r ≠ w, both FD_CLOEXEC and O_NONBLOCK (set at creation by pipe2 on Linux). wake(w) issues exactly one write(2) of 1 byte and returns; w is O_NONBLOCK, so a full pipe gives EAGAIN and wake never blocks.	-	pipe/pipe2/fcntl FFI; cfg(linux) arms; Option tuple	0,8
wait-readable-briefly	contract	wait_readable_briefly	∀ fd, slice: 0 ≤ fd < FD_SETSIZE ⇒ exactly one select(fd+1, {fd}, slice); else one nanosleep(slice). Never writes outside the fd_set. Under A-unix (select reports a Darwin fifo whose last writer closed as readable; poll and kqueue do not) it returns when the fifo's last writer closes, before the slice expires. It returns within slice in every case.	-	FD_SET bitmap bounds; timeval/timespec from Duration; select/nanosleep FFI	0,8
exec-status-carrier-cloexec	contract	open_exec_status_channel, open_exec_status_fifo, exec_status_fifo_path, fifo_ends_are_one_private_object, note_racy_exec_status_carrier, RACY_EXEC_STATUS_CARRIERS	open_exec_status_channel() = Ok((rd, wr, k)) ⇒ rd ≠ wr, both FD_CLOEXEC, rd O_NONBLOCK, and rd and wr are the two ends of one object no other path names. k ≠ RacyPipe ⇒ every descriptor got O_CLOEXEC in its creating syscall (pipe2 on Linux; open of an unlinked fifo on macOS), so no Plat state lets an execve on any thread inherit either end. k = RacyPipe only after the atomic route failed, and then RACY_EXEC_STATUS_CARRIERS rises by exactly 1. The macOS naming path does not call confstr or reach libsystem_notify (a call-graph property of exec_status_fifo_path / exec_status_fifo_dir). A spawn on a host where the atomic route works leaves RACY_EXEC_STATUS_CARRIERS unchanged.	-	mkfifo/open/unlink/getentropy/pipe2/fcntl FFI; AtomicU64; CString paths; cfg arms; a Trust reachability query for the call-graph clause	0,8,13
pty-pair-contract	contract	open_pty_pair_cloexec, open_pty_pair, open_pts_slave, pts_name, fd_is_cloexec_or_make_it	open_pty_pair_cloexec(termp, winp) = Ok((m,s)) ⇒ m ≠ s, both live tty fds of one pty pair (s is m's peer: a byte written to s is read from m, and no other pair's slave reaches m), both FD_CLOEXEC from the creating syscall (posix_openpt(O_CLOEXEC); then TIOCGPTPEER(O_CLOEXEC) on Linux, falling back to the path only on EINVAL/ENOTTY, or open(pts, O_CLOEXEC|O_NOCTTY)); the fcntl fallback runs only if the platform ignored the flag; the slave does not become the caller's ctty; termp then winp are applied to s (TCSAFLUSH, then TIOCSWINSZ) before return. So no execve by any thread in any Plat interleaving inherits m or s. Err(e) ⇒ the caller's fd table equals the one before the call (the master is closed on a later step's failure), and e names its step (e.g. "open of the slave: ") with that step's errno and ErrorKind. open_pty_pair() = open_pty_pair_cloexec(None, None). A-unix: posix_openpt and open honor O_CLOEXEC; the plain flag word and openpty leave the fd inheritable.	-	posix_openpt/grantpt/unlockpt/ioctl/open/tcsetattr FFI; CString; cfg(linux/macos) arms; fail-closed cleanup paths; a frame condition over the fd table	0,8
fd-flag-ops	contract	set_cloexec, dup_fd, fd_is_tty, spawn_shell (master flags)	set_cloexec(fd, on) = Ok ⇒ FD_CLOEXEC(fd) = on, other descriptor flags unchanged; under A-unix a master then survives execve iff ¬on, and still drives its slave. dup_fd(fd) = Ok(o) ⇒ o is a new fd onto the same object with FD_CLOEXEC set at creation (F_DUPFD_CLOEXEC; A-unix: plain dup clears it). fd_is_tty(fd) ⇔ isatty(fd) = 1 (true for master and slave; false for a pipe end or −1). Every master spawn_shell* returns has FD_CLOEXEC.	-	fcntl/isatty FFI; OwnedFd	0,8
winsize-applied	contract	resize, resize_with_cell_px, size, open_pty_pair_cloexec (winp), spawn_shell_with_pid_cell_px	In Plat, ∀ master m, r, c, px: after resize_with_cell_px(m,r,c,px) the pair's winsize is (r, c, winsize_pixels(r,c,px)) and TIOCGWINSZ on either end reads it; resize(m,r,c) leaves pixels (0,0). size(m) = Some((ws_row, ws_col)) when TIOCGWINSZ(m) succeeds, else None (size(−1) = None). spawn_shell_with_pid_cell_px(rows, cols, …, px) = Ok(sh) ⇒ the winsize is (rows, cols, winsize_pixels(rows,cols,px)) before the child's first instruction (applied to the slave in the parent before fork).	-	ioctl FFI on a libc::winsize struct; unsafe; fork/exec	0,8
spawn-termios	contract	build_spawn_termios, kernel_default_termios, spawn_termios, spawn_shell_with_pid_cell_px	∀ termios t: build_spawn_termios(t) = t with c_iflag |= IUTF8 and input/output speed B230400; every other field unchanged (c_oflag, c_lflag, c_cc, and c_cflag on macOS, where speed lives in c_ispeed/c_ospeed). kernel_default_termios() = Some(the termios of a fresh pty opened with none), by A-unix ICANON|ECHO|OPOST. When the probe is Some, every successful spawn's slave carries build_spawn_termios(probe) before the child runs, and the master reads it back.	unix_tests::spawn_termios_deltas_are_exactly_iutf8_and_speed	libc::termios by value with field |=; cfsetspeed FFI (&mut); OnceLock-style probe; cfg(macos) field semantics	0,1,2,8
spawn-child-process-shape	contract	spawn_shell_with_pid_cell_px, spawn_shell_with_pid, spawn_shell	spawn_shell_with_pid_cell_px(rows, cols, …) = Ok(sh) ⇒ the exec'd child's open descriptors are exactly {0,1,2}, each the pair's slave without FD_CLOEXEC; every parent descriptor at any number (including above 65 536) is closed in the child. sh.pid = getpgid(sh.pid) = getsid(sh.pid) = tcgetpgrp(sh.master); the child's ctty is the slave pts_name(sh.master) names; its first winsize read is (rows, cols). The parent holds no slave copy, so poll(sh.master) reports POLLHUP|POLLERR|POLLNVAL once the child's session dies and is reaped, and not while the child lives.	-	fork/login_tty/execve in an async-signal-safe child; aterm_uds::spawnfd descriptor strip (cross crate); rlimit; process-table reasoning	0,8,12
spawn-fail-closed	contract	spawn_shell_with_pid_cell_px, spawn_shell, wait_for_exec_status, exec_failure	∀ input to spawn_shell*: (a) cap.tier < Trusted ⇒ Err(PermissionDenied), no fork. (b) The child's sandbox apply fails ⇒ status byte 'S', _exit(126), result Err(PermissionDenied) with "fail-closed" in the message. (c) The child's execve fails ⇒ status byte 'E', _exit(127), result Err(Other) with text exec_failure(target, from, runs_program, cwd). (d) NoVerdict ⇒ Err(TimedOut). Every Err after the fork closes the master and reaps the child, so no master for an unconfined or un-exec'd shell is ever returned. Ok only on Execed (EOF from the close-on-exec write end).	unix_tests::sandbox_apply_failure_in_child_fails_closed_no_unconfined_shell, unix_tests::under_tier_spawn_cap_is_denied_before_forking, unix_tests::bogus_exec_command_takes_child_exec_failure_path	aterm_cap::require (cross crate); fork; the status-byte protocol; io::Error kinds; cleanup on every error path	0,1,8,12
queue-probes	contract	input_queue_len, output_queue_len, flush_input_queue, queue_len	Off macOS: input_queue_len and output_queue_len = None for every fd (even a pty master); flush_input_queue = None off a tty, Some(0) after TCIFLUSH. On macOS: queue_len(fd, req) = None if fd < 0, ¬isatty(fd), the ioctl fails, or the count is negative; else Some(count); input_queue_len uses FIONREAD, output_queue_len TIOCOUTQ; flush_input_queue(fd) = Some(input_queue_len read just before) after tcflush(TCIFLUSH), which leaves OQ unchanged; None off a tty. The counts rest on A-unix axioms (stated, not proved): canonical FIONREAD counts complete lines only; raw counts every unread byte and falls as the slave reads; raw with VMIN > queued and VTIME 0 reads 0; with ISIG set the signal characters are never queued and flush IQ (a remapped or VDISABLEd character moves or disables that); the master's FIONREAD never includes OQ.	unix_tests::flush_input_queue_discards_the_unread_input_and_nothing_else	ioctl FFI with a c_int out-param; usize::try_from; cfg(macos) arms; line-discipline axioms	0,8
msvcrt-cmdline-roundtrip	round-trip	windows::cmdline::build_command_line, append_quoted	∀ argv: build_command_line(argv) ends in exactly one 0 unit; element i is emitted unquoted iff non-empty and free of space, tab and '"'; elements are joined by single spaces. Inside quotes a run of n backslashes before '"' becomes 2n+1 backslashes plus '"', a trailing run becomes 2n, other backslashes stay literal. Round trip: ∀ argv with '"' ∉ argv[0]: crt_parse(build_command_line(argv) minus its NUL) = argv (the precondition is needed because MSVCRT parses argv[0] without backslash escapes; §5).	cmdline::plain_args_are_unquoted_and_space_joined, cmdline::spaces_tabs_and_empty_args_get_quoted, cmdline::embedded_quotes_are_backslash_escaped, cmdline::backslashes_before_a_quote_double_but_elsewhere_stay_literal, cmdline::plain_backslash_paths_stay_verbatim_when_unquoted	Vec<u16> building; nested while loops with an index; encode_wide; repeat_n; needs a Clean crt_parse model; cfg(windows)	0,1,5,7,W
env-block-shape	contract	windows::cmdline::build_env_block, fold_upper	∀ pairs, K = the distinct ASCII-uppercase-folded keys: build_env_block(pairs) = concat over k ∈ sort(K) by folded UTF-16 order of (key ++ '=' ++ value ++ [0]) for the LAST pair whose folded key is k, then [0]; for empty pairs, [0,0]. Every block ends in a double NUL and holds one entry per case-insensitive key.	cmdline::env_block_sorts_case_insensitively_and_double_nul_terminates, cmdline::env_block_dedupes_case_insensitively_last_wins, cmdline::empty_env_block_is_still_double_nul	Vec<(Vec<u16>,usize)> with iter_mut().find; sort_by; encode_wide; cfg(windows)	0,1,2,5,7,W
path-union	contract	windows::winpath::union_paths	∀ first, extra: S = [trim(s) | s ← split(';', lossy(first)) ++ (extra ? split(';', lossy(extra)) : [])] minus empty segments; union_paths(first, extra) = join(';', the first occurrence of each class of S under key(s) = lower_ascii(trim_end_matches(s, '\\')), in order). Each kept segment keeps its first spelling; no empty segment; no two segments with equal keys.	winpath::union_dedupes_and_preserves_order, winpath::union_registry_only	String split/trim/to_ascii_lowercase; Vec<String> contains; OsString push; cfg(windows)	0,1,5,7,W
windows-shell-name-pure	contract	windows::shell::is_path_like, is_wsl_shell, parse_pathext	is_path_like(s) ⇔ '\\' ∈ s ∨ '/' ∈ s ∨ ':' ∈ s. is_wsl_shell(p) ⇔ Path(p).file_stem() is UTF-8 and equals "wsl" ignoring ASCII case. parse_pathext(raw) = [trim(e) | e ← split(';', r)] filtered to len > 1 ∧ starts_with('.'), where r = raw if Some and not blank after trim, else ".COM;.EXE;.BAT;.CMD".	shell::path_like_classifier, shell::is_wsl_shell_matches_the_stem_case_insensitively, shell::parse_pathext_defaults_and_drops_junk	&str contains/split/trim; Path file_stem; Vec<String> collect; cfg(windows)	0,1,5,7,W
windows-shell-resolution	contract	windows::shell::classify_shell_name, resolve_shell_name, resolve_program_windows, search_path, discover_shell, select_shell_with_origin, select_shell	Parameterized by a SearchPathW oracle sp(name, ext) and an fs oracle. classify_shell_name(n) = Verbatim(n) if is_path_like(lossy(n)); else Resolved(h) if discover_shell(n) = Some(h); else Resolved(h) if sp(n, ".exe") = Some(h); else Unresolved. discover_shell = None for every alias outside {bash, git-bash, gitbash, wsl, nu, nushell}. Collapse law: resolve_shell_name(n) = match classify_shell_name(n) { Resolved(p) | Verbatim(p) ⇒ p, Unresolved ⇒ n }. resolve_program_windows(n) = n if empty or path-like; else the first hit of sp(n, ".exe") then sp(n, e) for e ∈ parse_pathext(%PATHEXT%) minus .exe; else n. select_shell_with_origin(ov): ov Some and non-empty ⇒ (resolve_shell_name(ov), Override); else the first of sp(pwsh) → Pwsh, sp(powershell) → PowerShell, non-empty %COMSPEC% → Comspec, else ("cmd.exe", CmdLiteral). Some("") behaves as None; select_shell = .0 of the pair.	shell::search_path_finds_cmd_and_misses_nonsense, shell::resolve_program_passes_paths_verbatim_and_falls_back_verbatim, shell::resolve_program_finds_cmd_and_bat_shims_via_pathext, shell::discover_shell_unknown_alias_is_none, shell::classify_shell_name_is_the_resolution_the_spawn_performs, shell::select_shell_with_origin_names_the_arm_select_shell_took, shell::path_like_names_bypass_discovery	enum ShellResolution with an OsString payload; SearchPathW FFI as an oracle; env var reads; let-chains; cfg(windows)	0,1,7,8,W
windows-spawn-target	contract	windows::shell::resolve_spawn_target	∀ input: exec_command = Some(cmd), cmd non-empty ⇒ ((cmd[0] UTF-8 ? resolve_program_windows(cmd[0]) : cmd[0]), cmd verbatim), whatever argv_override and shell_args are. Else, shell = select_shell(shell_override): argv_override = Some(ov) ⇒ argv = ov when shell_args is empty or ¬is_wsl_shell(shell), else [ov[0]] ++ shell_args ++ ov[1..]; else shell_args non-empty ⇒ [shell] ++ shell_args; else [shell]. The program is shell in every non-exec case.	shell::exec_command_takes_precedence_and_argv_is_verbatim, shell::shell_override_selects_and_resolves_the_named_shell, shell::shell_args_shape_the_interactive_argv, shell::wsl_shell_args_survive_the_integration_argv_override, shell::non_wsl_argv_override_still_replaces_argv_entirely	Option<&[T]> filter; Vec<OsString> extend with iterator chains; calls to the resolution fns; cfg(windows)	0,1,4,7,W
windows-spawn-fail-closed	contract	windows::spawn_shell, windows::spawn_shell_with_pid	∀ input: spawn cap tier < Trusted ⇒ Err(PermissionDenied) before CreateProcessW. Sandbox cap tier < Trusted ⇒ Err(PermissionDenied) with "fail-closed", |SESSIONS| unchanged. sandbox_wrap = Some(_) ⇒ Err(Unsupported) with "fail-closed". A NUL in shell_override when the shell would run ⇒ Err(InvalidInput) naming "the shell (set by --shell" before any process, on each of the argv_override, shell_args and bare-shell arms. CreateProcessW not-found ⇒ Err(NotFound), no session registered. On every Err no process and no registry entry remain.	mod::under_tier_spawn_cap_is_denied_before_any_process, mod::under_tier_sandbox_cap_is_denied_fail_closed_no_session, mod::sandbox_wrap_demand_fails_closed_unsupported, mod::a_shell_holding_a_nul_refuses_the_spawn, windows_smoke::bogus_exec_command_fails_closed_no_session	aterm_cap gates (cross crate); a global Mutex<HashMap> registry; io::Error kinds; CreateProcessW FFI; cfg(windows)	0,1,8,12,W
windows-session-registry	invariant	windows::session_by_pid, backend_is_conpty, adopt_handoff, release_handoff_handles, close_master, read, write_some, write_all, resize, hangup, reap, exit_code, set_focus_boost	(a) session_by_pid(p) = Some(s) iff p > 1 and s is the UNIQUE registered session with s.pid = p (None when 0 or ≥ 2 claim p); every pid-keyed operation (hangup, reap, exit_code) on such a p is its documented miss (no-op or None). (b) backend_is_conpty(k) ⇔ k ∈ SESSIONS. (c) k ∉ SESSIONS ⇒ read(k,_) = −1, write_some(k,_) = Err(NotFound); write_all, resize, set_focus_boost, hangup, reap and close_master are no-ops; exit_code = None. (d) adopt_handoff(_,_,signal,client): client = 0 ⇒ Err(InvalidInput), SESSIONS unchanged; else registers a session with hpc = job = conhost = 0, signal = the given pipe, pid = GetProcessId(client) or 0 without query rights; its waiter blocks until the close event or the client's exit; resize writes encode_resize_signal to the signal pipe (nothing when signal = 0); hangup(pid) fires the close event, after which the waiter closes the signal pipe; close_master unregisters it. (e) release_handoff_handles(hs) calls CloseHandle exactly once on each nonzero element and never on 0.	mod::adopt_handoff_without_a_client_process_is_refused_and_registers_nothing, mod::backend_is_conpty_follows_the_registry, mod::adopted_session_has_no_hpcon_no_job_and_keeps_the_signal_pipe, mod::hangup_tears_down_an_adopted_session_through_the_signal_pipe, mod::a_pid_claimed_by_two_live_sessions_resolves_to_neither, mod::release_handoff_handles_closes_the_live_slots_and_skips_the_zeros, mod::adopted_session_tolerates_a_process_handle_with_no_query_rights, mod::fabricated_master_keys_get_defined_miss_semantics	global Mutex<HashMap<i32, Arc<WinSession>>>; Arc; per-field Mutexes; a waiter thread with WaitForMultipleObjects; Win32 handle FFI; cfg(windows)	0,1,2,8,13,W
conpty-session-lifecycle	contract	windows::spawn_shell_with_pid, read, write_all, settle_input, resize, hangup, reap, close_master, exit_code, set_focus_boost, ResizeGuard (wired into write/read)	Under A-win (ConPTY/conhost), ∀ successful spawn sh: (a) sh.master ≥ 0 ∧ sh.pid > 1. (b) The child's output appears in read(sh.master) in order. (c) After the child exits the waiter calls ClosePseudoConsole and read returns ≤ 0 in bounded time; reap(pid) returns within its budget; exit_code(pid) = Some(the child's code); after close_master read returns < 0 and the key is a registry miss. (d) hangup(pid) on a live interactive shell yields EOF. (e) resize may run concurrently with a parked read and with writes, without deadlock or lost output. (f) set_focus_boost(m, true) sets the shell root's priority class to ABOVE_NORMAL; false restores NORMAL. (g) A bare .cmd shim resolved through PATHEXT launches. (h) With records negotiated, input written before any resize reaches a console reader unchanged; the first boundary write after a size change reaches it behind exactly one Shift key-up record; a lone ESC after the switch reaches it as Escape down+up, never held into Alt+key; a character beyond U+FFFF written as UTF-8 reaches it, while a program of the MSYS2/Cygwin runtime runs in the session's tab (in its job, or below its first process in the process tree: console_guard::runtime_in_tab), as its surrogate pair's key-downs, then key-ups, with no virtual key, and otherwise as the text it is. (i) A write inside as_reply(f) (on f's thread, until f returns or unwinds; nesting keeps it) after a size change reaches the reader with no record, and the next write outside it carries the one owed. (j) In a tab where a program of the MSYS2/Cygwin runtime runs, a size change that no input write follows within NUDGE_AFTER (50 ms) reaches the reader as one Shift key-up record by itself, and the next input write carries the second record owed.	windows_smoke::conpty_spawns_cmd_echo_and_reaps, windows_smoke::conpty_write_path_echoes_interactively, windows_smoke::bare_cmd_shim_resolves_via_pathext_and_launches, windows_smoke::resize_is_thread_safe_against_parked_reader, windows_smoke::hangup_ends_an_interactive_shell_with_eof, windows_smoke::focus_boost_actuates_shell_priority_and_restores, mod::the_first_write_after_a_resize_reaches_the_reader_behind_one_shift_key_up, mod::a_lone_esc_after_the_guard_record_reaches_the_reader_as_escape, mod::an_astral_character_reaches_the_reader_as_its_surrogate_records, mod::a_reply_after_a_resize_passes_the_guard_and_the_next_input_carries_it, mod::the_reply_mark_covers_the_closure_and_no_more, mod::a_resize_no_input_follows_sends_the_record_by_itself	ConPTY/CreatePseudoConsole/ReadFile/WriteFile FFI; a waiter thread; cross-thread Mutexes; liveness against an OS model; cfg(windows)	0,8,13,W
windows-drain-available	contract	windows::drain_available, read	Under A-win, ∀ master m, buf, filled ≤ buf.len(): drain_available(m, buf, filled) never blocks. It returns filled when m names no registered session. Otherwise it repeats a PeekNamedPipe of the session's output pipe and a ReadFile of min(available, buf.len() − fill) while bytes are waiting and the buffer has room, and returns the new fill: the pipe's next bytes in order (several writes' worth in one call), each passed to the session's ResizeGuard as read passes them. What does not fit stays queued for the next read; an empty or broken pipe ends it, and the next blocking read reports an end.	mod::drain_available_collects_what_the_pipe_holds_and_never_blocks	PeekNamedPipe/ReadFile FFI on a session's pipe; cfg(windows)	0,8,W
windows-programs-outside-job	contract	windows::outside_job::{end_once, select}, console_guard::below (Unread::LeftOut), hangup, close_master, waiter, end_programs_outside_jobs	Under A-win, ∀ spawned session s (s.job ≠ 0): the first of hangup(s.pid), close_master(s.master), the waiter's wake (the shell's exit or the close event, before it closes the pseudoconsole) and end_programs_outside_jobs() (each session in the registry) terminates, before it returns and on its own thread, exactly the processes X reached from s's first process P0 by a chain P0, P1, …, Pn = X in one Toolhelp snapshot where each Pi (i ≥ 1) opens with PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_TERMINATE, its kernel-kept parent (NtQueryInformationProcess) is the snapshot's Pi−1, start(Pi) ≥ start(Pi−1) (start(P0) read through the session's own handle), and s.job does not hold X (a failed IsProcessInJob counts as held); each handle is held from its open to the end, so every Pi−1 still names the process vouched for. Nothing else is touched: no process older than P0, none outside its tree, none an adopted session (job = 0) runs. Later asks of the same session do nothing, and one made while it runs returns after it (outside_job_ended, a Mutex held across the sweep). select(root, root_born, tree, open, held) is that set over injected lookups: below(root, tree, born, LeftOut) with born(root) = root_born and born(p) = open(p).born when open(p).parent = tree's parent of p, else None (left out with its subtree), minus the processes held says the job holds.	outside_job::the_programs_below_the_shell_outside_its_job_are_taken, outside_job::a_pid_reused_after_the_snapshot_is_left_with_its_tree, outside_job::an_orphan_of_an_earlier_holder_of_the_shells_pid_is_left, outside_job::a_process_that_cannot_be_opened_is_left_with_its_tree, outside_job::a_close_ends_the_programs_the_shell_runs_outside_its_job, outside_job::a_shell_that_exits_ends_the_programs_it_runs_outside_its_job, outside_job::a_quit_ends_the_programs_every_shell_runs_outside_its_job, console_guard::the_strict_walk_leaves_out_what_it_cannot_vouch_for	generic H; RefCell<HashMap> captured by an impl Fn closure passed by value; Toolhelp/OpenProcess/NtQueryInformationProcess/IsProcessInJob/TerminateProcess FFI; a Mutex<bool> held across an effectful call; a cfg(test) thread_local job seam (JOB_LETS_GO); cfg(windows)	0,1,4,7,8,13,W
windows-createprocess-axiom	axiom	(platform) CreateProcessW lpApplicationName; cited by windows::spawn_shell_with_pid and the shell validator	A-win axiom, not a theorem about aterm code: CreateProcessW(lpApplicationName = p) succeeds iff p is a full path (either separator) to an existing executable image or batch file (.cmd/.bat by full path runs). No default extension is appended (…\\cmd fails ERROR_FILE_NOT_FOUND); no PATH search ("cmd.exe" fails); a '"' in p fails ERROR_INVALID_NAME; %VAR% is never expanded. It belongs in the platform lane's PlatformSpec so the specs above can cite it; the test stays only as a host probe that validates the axiom.	shell::create_process_application_name_contract	OS FFI behavior only	0,8,W
```

### Distribution by kind (49)

| kind | count |
|---|---|
| contract | 38 |
| refinement | 4 |
| table-property | 2 |
| invariant | 2 |
| equivalence | 1 |
| round-trip | 1 |
| axiom | 1 |

### Gap histogram (specs blocked by each gap)

| gap | specs |
|---|---|
| 0 | 49 |
| 1 | 31 |
| 2 | 9 |
| 3 | 8 |
| 4 | 7 |
| 5 | 8 |
| 7 | 19 |
| 8 | 31 |
| 10 | 2 |
| 11 | 1 |
| 12 | 7 |
| 13 | 11 |
| W | 16 |

No spec is blocked only by gap 0. The nearest are `winsize-pixels` (0, 3), `resize-signal-encode`
(0, 3, W), `write-step-classify` and `fifo-dir-in` (0, 1, 3), `spawn-locale-resolve` (0, 1, 5) and
`ptsname-r-error` (0, 1, 8, with errno as the only effect). Gap 8 (effects) blocks the most specs:
every effectful shell waits on the platform lane once its pure core is out.

### Proof dependency order

- `utf8-locale-codeset` → `spawn-locale-resolve` → (`child-env-build`) → `child-ctype-always-utf8`.
- `write-step-classify` → `write-all-no-silent-drop`.
- `winsize-pixels` → `winsize-applied` → `spawn-child-process-shape`.
- `exec-failure-message`, `exec-status-verdict`, `wait-readable-briefly`, `exec-status-carrier-cloexec`
  → `spawn-fail-closed`.
- `open-ptmx-redrive` → `pty-pair-contract` → `fd-flag-ops`, `spawn-termios`, `spawn-child-process-shape`.
- `shell-identity-verify` → `hangup-shell-target`, `reap-schedule` → `reap-grace-accounting`.
- `wake-pipe` → `read-or-wake-outcome`, `drain-gather-policy`.
- `input-tail-tracker`, `handshake-scan`, `guard-record-constants` → `resize-guard-write-protocol`
  → `conpty-session-lifecycle` (h).
- `windows-shell-name-pure` → `windows-shell-resolution` → `windows-spawn-target`.
- `resize-signal-encode` → `windows-session-registry` (d).

## 2. Test → spec map (TSV; all 157)

`primary` is the spec that retires the test. `also` lists the other specs the same test samples.
A test in §4 has primary `-` and the §4 action in `also`.

```tsv
file	test	primary	also
src/lib.rs	build_child_env_drops_denylisted_and_keeps_overrides	child-env-build	
src/lib.rs	a_non_utf8_key_with_a_denied_prefix_is_still_denied	child-env-build	
src/lib.rs	build_child_env_replaces_the_parents_agent_home_with_the_identitys	child-env-build	
src/lib.rs	build_child_env_denies_mixed_case_keys_on_windows	child-env-build	
src/lib.rs	build_child_env_stays_case_sensitive_on_unix	child-env-build	
src/unix_tests.rs	an_unset_tmpdir_puts_the_status_fifo_in_tmp	fifo-dir-in	exec-status-carrier-cloexec
src/unix_tests.rs	ptsname_r_errors_come_from_its_return_value	ptsname-r-error	
src/unix_tests.rs	is_utf8_locale_classifies_codeset	utf8-locale-codeset	
src/unix_tests.rs	spawn_locale_conformance_child_always_utf8_ctype	child-ctype-always-utf8	spawn-locale-resolve, utf8-locale-codeset, child-env-build
src/unix_tests.rs	a_command_argument_reaches_the_program_byte_for_byte	argv-nul-refusal	
src/unix_tests.rs	build_sandbox_wrap_fails_closed_when_wrapper_missing	sandbox-wrap-argv	
src/unix_tests.rs	build_sandbox_wrap_produces_correct_argv_shape	sandbox-wrap-argv	
src/unix_tests.rs	wrapped_spawn_enforces_network_deny_via_seatbelt	sandbox-wrap-argv	spawn-fail-closed
src/unix_tests.rs	tty_passes_every_byte_only_in_raw_mode	tty-echo-decode	
src/unix_tests.rs	a_stranger_holding_the_write_end_cannot_block_the_status_wait_forever	exec-status-verdict	
src/unix_tests.rs	read_or_wake_answers_exited_when_the_shell_exits_while_the_slave_is_held	read-or-wake-outcome	
src/unix_tests.rs	drain_more_nonblocking_delivers_interactive_burst_on_first_quiet_read	drain-gather-policy	
src/unix_tests.rs	drain_more_nonblocking_hot_caps_a_continuous_stream_at_the_hot_budget	drain-gather-policy	
src/unix_tests.rs	drain_more_nonblocking_gathers_saturated_burst_and_survives_eof	drain-gather-policy	
src/unix_tests.rs	drain_more_nonblocking_fence_delivers_before_a_park	drain-gather-policy	
src/unix_tests.rs	drain_more_nonblocking_idle_wait_bridges_refill_into_same_batch	drain-gather-policy	
src/unix_tests.rs	a_signal_does_not_end_an_armed_idle_wait	drain-gather-policy	
src/unix_tests.rs	spawn_termios_deltas_are_exactly_iutf8_and_speed	spawn-termios	
src/unix_tests.rs	sandbox_apply_failure_in_child_fails_closed_no_unconfined_shell	spawn-fail-closed	
src/unix_tests.rs	under_tier_spawn_cap_is_denied_before_forking	spawn-fail-closed	
src/unix_tests.rs	bogus_exec_command_takes_child_exec_failure_path	spawn-fail-closed	exec-failure-message, exec-status-verdict
src/unix_tests.rs	flush_input_queue_discards_the_unread_input_and_nothing_else	queue-probes	
src/unix_shell_identity_tests.rs	teardown_leaves_a_setsid_sleep_holding_the_recycled_pid_alive	hangup-shell-target	shell-identity-verify, reap-schedule
src/unix_shell_identity_tests.rs	adoption_records_a_birth_only_for_the_ptys_session_leader	shell-identity-verify	
src/unix_shell_identity_tests.rs	an_unreaped_own_child_zombie_still_verifies_and_is_reaped	shell-identity-verify	reap-schedule
src/unix_shell_identity_tests.rs	a_verified_hup_ignoring_shell_is_still_hung_up_and_force_killed	reap-schedule	hangup-shell-target, shell-identity-verify
src/unix_shell_identity_tests.rs	a_shell_stuck_exiting_on_unread_output_is_reaped_not_left_a_zombie	reap-schedule	
src/unix_shell_identity_tests.rs	a_shell_alive_past_the_deadline_is_reaped_when_it_exits	reap-schedule	
src/unix_shell_identity_tests.rs	a_quit_after_wait_for_reaps_leaves_no_hup_ignoring_shell	reap-grace-accounting	reap-schedule
src/unix_shell_identity_tests.rs	a_dropped_running_child_is_reaped_and_a_dropped_std_one_is_not	-	§4: relocate
src/windows/cmdline.rs	plain_args_are_unquoted_and_space_joined	msvcrt-cmdline-roundtrip	
src/windows/cmdline.rs	spaces_tabs_and_empty_args_get_quoted	msvcrt-cmdline-roundtrip	
src/windows/cmdline.rs	embedded_quotes_are_backslash_escaped	msvcrt-cmdline-roundtrip	
src/windows/cmdline.rs	backslashes_before_a_quote_double_but_elsewhere_stay_literal	msvcrt-cmdline-roundtrip	
src/windows/cmdline.rs	plain_backslash_paths_stay_verbatim_when_unquoted	msvcrt-cmdline-roundtrip	
src/windows/cmdline.rs	env_block_sorts_case_insensitively_and_double_nul_terminates	env-block-shape	
src/windows/cmdline.rs	env_block_dedupes_case_insensitively_last_wins	env-block-shape	
src/windows/cmdline.rs	empty_env_block_is_still_double_nul	env-block-shape	
src/windows/console_guard.rs	the_strict_walk_leaves_out_what_it_cannot_vouch_for	windows-programs-outside-job	
src/windows/mod.rs	resize_signal_packet_is_opcode_then_columns_then_rows	resize-signal-encode	
src/windows/mod.rs	resize_signal_is_not_transposed	resize-signal-encode	
src/windows/mod.rs	resize_signal_clamps_into_positive_coord_range	resize-signal-encode	
src/windows/mod.rs	under_tier_spawn_cap_is_denied_before_any_process	windows-spawn-fail-closed	
src/windows/mod.rs	under_tier_sandbox_cap_is_denied_fail_closed_no_session	windows-spawn-fail-closed	
src/windows/mod.rs	sandbox_wrap_demand_fails_closed_unsupported	windows-spawn-fail-closed	
src/windows/mod.rs	a_shell_holding_a_nul_refuses_the_spawn	windows-spawn-fail-closed	argv-nul-refusal
src/windows/mod.rs	adopt_handoff_without_a_client_process_is_refused_and_registers_nothing	windows-session-registry	
src/windows/mod.rs	backend_is_conpty_follows_the_registry	windows-session-registry	
src/windows/mod.rs	adopted_session_has_no_hpcon_no_job_and_keeps_the_signal_pipe	windows-session-registry	
src/windows/mod.rs	hangup_tears_down_an_adopted_session_through_the_signal_pipe	windows-session-registry	resize-signal-encode
src/windows/mod.rs	a_pid_claimed_by_two_live_sessions_resolves_to_neither	windows-session-registry	
src/windows/mod.rs	release_handoff_handles_closes_the_live_slots_and_skips_the_zeros	windows-session-registry	
src/windows/mod.rs	adopted_session_tolerates_a_process_handle_with_no_query_rights	windows-session-registry	
src/windows/mod.rs	fabricated_master_keys_get_defined_miss_semantics	windows-session-registry	
src/windows/mod.rs	the_first_write_after_a_resize_reaches_the_reader_behind_one_shift_key_up	conpty-session-lifecycle	resize-guard-write-protocol, guard-record-constants
src/windows/mod.rs	a_lone_esc_after_the_guard_record_reaches_the_reader_as_escape	conpty-session-lifecycle	resize-guard-write-protocol, guard-record-constants
src/windows/mod.rs	a_reply_after_a_resize_passes_the_guard_and_the_next_input_carries_it	conpty-session-lifecycle	resize-guard-write-protocol, guard-record-constants
src/windows/mod.rs	the_reply_mark_covers_the_closure_and_no_more	conpty-session-lifecycle	
src/windows/mod.rs	a_resize_no_input_follows_sends_the_record_by_itself	conpty-session-lifecycle	resize-guard-write-protocol, guard-record-constants
src/windows/mod.rs	drain_available_collects_what_the_pipe_holds_and_never_blocks	windows-drain-available	
src/windows/outside_job.rs	the_programs_below_the_shell_outside_its_job_are_taken	windows-programs-outside-job	
src/windows/outside_job.rs	a_pid_reused_after_the_snapshot_is_left_with_its_tree	windows-programs-outside-job	
src/windows/outside_job.rs	an_orphan_of_an_earlier_holder_of_the_shells_pid_is_left	windows-programs-outside-job	
src/windows/outside_job.rs	a_process_that_cannot_be_opened_is_left_with_its_tree	windows-programs-outside-job	
src/windows/outside_job.rs	a_close_ends_the_programs_the_shell_runs_outside_its_job	windows-programs-outside-job	conpty-session-lifecycle
src/windows/outside_job.rs	a_shell_that_exits_ends_the_programs_it_runs_outside_its_job	windows-programs-outside-job	conpty-session-lifecycle
src/windows/outside_job.rs	a_quit_ends_the_programs_every_shell_runs_outside_its_job	windows-programs-outside-job	
src/windows/resize_guard.rs	the_input_tail_follows_the_sequences_aterm_writes	input-tail-tracker	
src/windows/resize_guard.rs	the_switch_follows_completed_win32_records_only	input-tail-tracker	
src/windows/resize_guard.rs	the_request_is_found_across_read_boundaries_inside_the_window	handshake-scan	
src/windows/resize_guard.rs	the_guard_record_is_a_shift_key_up	guard-record-constants	
src/windows/resize_guard.rs	the_seam_escape_is_the_key_encoders_escape	guard-record-constants	
src/windows/resize_guard.rs	a_resize_guards_the_next_two_writes_and_reports_only_caller_bytes	resize-guard-write-protocol	
src/windows/resize_guard.rs	an_unchanged_size_writes_no_record	resize-guard-write-protocol	
src/windows/resize_guard.rs	an_unknown_starting_size_arms_on_the_first_resize	resize-guard-write-protocol	
src/windows/resize_guard.rs	no_record_without_the_win32_input_request	resize-guard-write-protocol	handshake-scan
src/windows/resize_guard.rs	the_record_never_splits_an_escape_sequence	resize-guard-write-protocol	input-tail-tracker
src/windows/resize_guard.rs	the_record_never_splits_a_utf8_character	resize-guard-write-protocol	input-tail-tracker
src/windows/resize_guard.rs	a_failed_write_keeps_the_record_owed	resize-guard-write-protocol	
src/windows/resize_guard.rs	a_lone_esc_after_the_switch_is_the_escape_key	resize-guard-write-protocol	input-tail-tracker
src/windows/resize_guard.rs	only_a_lone_esc_at_a_boundary_is_rewritten	resize-guard-write-protocol	
src/windows/resize_guard.rs	a_reply_after_a_resize_leaves_the_openconsole_client_to_the_next_input	resize-guard-write-protocol	
src/windows/resize_guard.rs	a_reply_after_a_resize_carries_no_record_and_leaves_both_owed	resize-guard-write-protocol	
src/windows/resize_guard.rs	a_reply_goes_past_an_input_waiting_for_the_openconsole_client	resize-guard-write-protocol	
src/windows/resize_guard.rs	a_nudge_sends_an_inbox_conhost_the_record_no_input_carried	resize-guard-write-protocol	
src/windows/resize_guard.rs	a_nudge_sends_nothing_where_no_record_is_owed	resize-guard-write-protocol	handshake-scan, input-tail-tracker
src/windows/resize_guard.rs	a_nudge_runs_the_openconsole_client_the_input_did_not	resize-guard-write-protocol	
src/windows/shell.rs	path_like_classifier	windows-shell-name-pure	
src/windows/shell.rs	is_wsl_shell_matches_the_stem_case_insensitively	windows-shell-name-pure	
src/windows/shell.rs	parse_pathext_defaults_and_drops_junk	windows-shell-name-pure	
src/windows/shell.rs	search_path_finds_cmd_and_misses_nonsense	windows-shell-resolution	
src/windows/shell.rs	resolve_program_passes_paths_verbatim_and_falls_back_verbatim	windows-shell-resolution	
src/windows/shell.rs	resolve_program_finds_cmd_and_bat_shims_via_pathext	windows-shell-resolution	windows-shell-name-pure
src/windows/shell.rs	discover_shell_unknown_alias_is_none	windows-shell-resolution	
src/windows/shell.rs	classify_shell_name_is_the_resolution_the_spawn_performs	windows-shell-resolution	windows-shell-name-pure
src/windows/shell.rs	select_shell_with_origin_names_the_arm_select_shell_took	windows-shell-resolution	
src/windows/shell.rs	path_like_names_bypass_discovery	windows-shell-resolution	windows-shell-name-pure
src/windows/shell.rs	exec_command_takes_precedence_and_argv_is_verbatim	windows-spawn-target	
src/windows/shell.rs	shell_override_selects_and_resolves_the_named_shell	windows-spawn-target	
src/windows/shell.rs	shell_args_shape_the_interactive_argv	windows-spawn-target	
src/windows/shell.rs	wsl_shell_args_survive_the_integration_argv_override	windows-spawn-target	windows-shell-name-pure
src/windows/shell.rs	non_wsl_argv_override_still_replaces_argv_entirely	windows-spawn-target	
src/windows/shell.rs	create_process_application_name_contract	windows-createprocess-axiom	
src/windows/winpath.rs	union_dedupes_and_preserves_order	path-union	
src/windows/winpath.rs	union_registry_only	path-union	
tests/windows_smoke.rs	resolve_spawn_locale_is_an_empty_stub	windows-stub-contracts	
tests/windows_smoke.rs	set_nonblocking_true_is_unsupported	windows-stub-contracts	
tests/windows_smoke.rs	bogus_exec_command_fails_closed_no_session	windows-spawn-fail-closed	windows-session-registry
tests/windows_smoke.rs	conpty_spawns_cmd_echo_and_reaps	conpty-session-lifecycle	
tests/windows_smoke.rs	conpty_write_path_echoes_interactively	conpty-session-lifecycle	
tests/windows_smoke.rs	bare_cmd_shim_resolves_via_pathext_and_launches	conpty-session-lifecycle	windows-shell-resolution
tests/windows_smoke.rs	resize_is_thread_safe_against_parked_reader	conpty-session-lifecycle	resize-signal-encode
tests/windows_smoke.rs	hangup_ends_an_interactive_shell_with_eof	conpty-session-lifecycle	
tests/windows_smoke.rs	focus_boost_actuates_shell_priority_and_restores	conpty-session-lifecycle	
```

## 3. Coverage

- 157 tests read. 153 map to one of the 49 specs. 4 are not spec samples (§4).
- The biggest collapses: `drain-gather-policy` (10), `resize-guard-write-protocol` (9), `queue-probes` (8), `windows-session-registry` (8), `conpty-session-lifecycle` (8), `pty-pair-contract` (7), `windows-shell-resolution` (7), `child-env-build` (5).
- 87 tests map to specs that need no Windows lane; 66 map to the 16 `W` specs.
- Several tests measure the OS more than aterm (see §5.5). They are mapped to the spec that cites the
  axiom they measure; proving that spec retires the aterm half, and the axiom half moves to the
  platform lane's axiom-validation probes rather than staying a unit test.

## 4. Tests that are not spec samples

> Status 2026-10-16: `restore_ctrl_c_processing_is_callable_and_idempotent` is deleted; the other three rows are still open.
>
> Status 2026-10-17: `building_the_status_channel_never_kills_a_concurrent_forks_child` and
> `inlined_sandbox_exec_path_matches_policy_crate` are deleted too, and their rows are gone (§5.7 keeps
> the duplicated constant); the one row left is still open.

| test | why | action |
|---|---|---|
| src/unix_shell_identity_tests.rs::a_dropped_running_child_is_reaped_and_a_dropped_std_one_is_not | Reaches no aterm-pty code. It tests `aterm_uds::spawnfd::Child`'s drop-reaps behavior, with `std::process::Child` as a control. The spec it samples is aterm-uds's: a dropped spawnfd child is reaped once it exits. | **relocate: state the spec in `crates/aterm-uds/SPECS.md`, then delete it here** |

Every regression-style test I found (the confstr/libnotify fork hang aside) is an instance of one of
the general specs above.

## 5. Implied specs that look wrong, or disagree with the code

1. **`a_verified_hup_ignoring_shell_is_still_hung_up_and_force_killed`** asserts `took >= REAP_POLL * REAP_KILL_AT` on a stopwatch. It is a lower bound, so safe, but it pins `REAP_POLL`'s value; the property is the counted kill tick.
2. **`a_shell_stuck_exiting_on_unread_output_is_reaped_not_left_a_zombie`** gates its precondition on a fixed 300 ms sleep ("a beat for the rest of the printf"). Under load the precondition can fail, and the test reports that as red rather than vacuous.
3. **`fifo_dir_in` and an empty TMPDIR.** `an_unset_tmpdir_puts_the_status_fifo_in_tmp` pins `TMPDIR=""` ⇒ `"/"`, so the status fifo goes in the filesystem root, normally unwritable, and the spawn then degrades to the racy pipe. `resolve_spawn_locale` treats an empty value as unset, and the same POSIX convention would give `/tmp` here; `fifo-dir-in` states the code as it is, and this should be decided before it is proved. The doc's "ends in one `/`" is false for a TMPDIR ending in `//`.
4. **`spawn_locale_conformance_child_always_utf8_ctype`'s oracle disagrees with the code.** Its "independent" `looks_utf8` (ends_with `.utf-8`/`.utf8`) rejects `@modifier` forms such as `de_DE.UTF-8@euro`, which `is_utf8_locale` accepts; harmless only because the sampled UTF-8 class is `en_US.UTF-8`. `resolve_spawn_locale`'s doc says the property is bound by "the spawn_locale_* conformance tests", but there is one, with no negative control (AGENTS.md requires one for model conformance).
5. **OS measurements posing as unit tests.** `src/windows/shell.rs::create_process_application_name_contract` mostly measures the OS. Its statement is an A-win axiom (gap 8). No proof about compiled Rust discharges it; it becomes the platform lane's axiom-validation probe.
6. **`build_command_line`'s round trip has an unstated precondition.** `src/windows/cmdline.rs` claims a conforming CRT "reproduces the argv exactly". That holds only when argv[0] contains no `"`, because MSVCRT/`CommandLineToArgvW` parse argv[0] without backslash escaping while `append_quoted` applies the argument rules to it. Every sample uses a plain argv[0]. `msvcrt-cmdline-roundtrip` carries the precondition.
7. **`SANDBOX_EXEC_PATH` is kept twice.** `src/unix.rs:18` inlines `aterm_containment::SANDBOX_EXEC_PATH`, and its doc says the two must be equal; nothing checks that. Make it one definition (aterm-pty takes the constant from aterm-containment, or the policy crate re-exports the seam's).

## 6. Proof order (easiest high-value first)

No spec is blocked by gap 0 alone, so the order prefers the smallest gap sets, then pure cores that
unlock the most tests.

1. **`winsize-pixels`** (0, 3). Scalar: a let-else, a widening multiply and a saturating `try_from`. An
   `omega`-class proof once E6 admits casts and `Option` match. It is also the arithmetic half of
   `winsize-applied`.
2. **`write-step-classify`** (0, 1, 3). A three-way enum result over `(isize, bool)`. It is the
   decision `write-all-no-silent-drop` composes.
3. **`fifo-dir-in`** and **`ptsname-r-error`** (0, 1, 3 / 0, 1, 8). Tiny; `ptsname-r-error` needs only
   an errno ghost. Settle §5.3 before proving `fifo-dir-in`.
4. **`resize-signal-encode`** (0, 3, W). Clamp, casts and `to_le_bytes`; retires 3 tests. It needs refactor
   (f)'s move to native code or a Windows lane.
5. **`utf8-locale-codeset`** then **`spawn-locale-resolve`** (0, 1, 3, 5, 7 / 0, 1, 5). `&str`/`Option`
   code; with `child-env-build` they make `child-ctype-always-utf8` a composition and replace the 64-shape
   conformance loop.
6. **`input-tail-tracker`** (0, 1, 3, 7, W). The step table is finite, so (b) is decidable by evaluation;
   then **`handshake-scan`** and **`resize-guard-write-protocol`** (9 tests), all after refactor (f).
7. **`open-ptmx-redrive`** (0, 1, 4, 7, 8, 13), over the injected `open` closure plus an errno ghost.
8. **`child-env-build`** (5 tests). Needs `Vec`/`OsString` content models and the `impl Iterator`
   generic; it is `#[trust::skip]` today.
9. The pure cores (a)-(e), then everything else, which waits on gap 8 (effects).
