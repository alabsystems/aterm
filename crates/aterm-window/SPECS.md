<!-- Copyright 2026 Andrew Yates -->
<!-- SPDX-License-Identifier: Apache-2.0 -->
<!-- Added by aterm: specification recovery for its absorbed winit implementation. -->

# aterm-window: recovered contracts and migration worklist

> Status 2026-10-16, after the merge with the tests-and-gates audit: the rows naming 10 tests it
> deleted are gone, and the spec rows no longer cite those tests. Counts elsewhere in this file
> were taken before.

> Status 2026-10-16: this is the crate's migration worklist. Each of the 17 specs below is to be
> proved (through Trust/Clean, or by rustc for the auto-trait spec), and then the tests it
> subsumes are deleted. Section4 records retired non-spec checks and one remaining migration control.
>
> 2026-10-16 (LAG-2): the macOS proxy queue (`src/platform_impl/macos/wake_queue.rs`) added one
> spec, `macos-proxy-queue`, and its 3 tests (last rows of §1 and §2). The counts below are the
> recovery's own and leave them out.
>
> 2026-10-17 (LAG-5): the same spec now covers coalesced wakes (only the send that arms the wake
> signals; the drain clears the flag before every take), with 2 more tests (§2's last rows).

The package and lib were named `winit` when this was written; since winit-core step 7
(2026-10-19) they are `aterm-window` and `aterm_window`. The inventory below is historical: the original 36
entries plus 3 audit regressions. Of those 39, the 9 compile-only auto-trait (`Send`/`Sync`) checks
and 3 of the 4 non-spec tests in §4 are since deleted, which leaves 27
(src/event.rs 2, src/keyboard/smol_str.rs 1,
src/platform_impl/linux/common/percent.rs 1, src/platform_impl/linux/headless.rs 5,
src/platform_impl/linux/wayland/clipboard.rs 10, src/platform_impl/linux/wayland/seat/data_device.rs 3,
src/platform_impl/linux/x11/dnd.rs 1, src/platform_impl/linux/x11/xsettings.rs 2,
src/platform_impl/windows/drop_handler.rs 2 = 27). It is not a current crate-wide test census.
It lists those entries and the code each one reaches. The initial inventory was read-only. The
URI/Xsettings corrections were subsequently exercised as pure host-Trust parser tests; Windows drop tests were type-checked
for both Windows architectures, not executed on Windows. These checks are not proof claims.

The historical map has **16 specs** covering **26 entries** and **1 non-spec entry** (§4).
`public-handles-send-sync` has no test left to retire.

## 0. Notation used in the statements

Paths are relative to `crates/aterm-window`.

- `PortTrace(P, op)`: the ordered list of `SelectionPort` primitive calls (`focused`, `offers(sel)`,
  `release(sel)`, `claim(sel, serial) -> bool`) that the default method `SelectionPort::copy` makes on a
  port `P`. It is a ghost trace, the way aterm-parser's `Obs` uses its sink.
- `CompositorModel M`: today the test-only `FakeSeat` in `wayland/clipboard.rs`; author it with
  `ty_model!`. State: `offered: Option<src>`, `selection: Option<(src, serial)>`, `cached: text`,
  `slot: text`, `focused`, `device`. `claim(serial)` mints a source. The source is dropped iff `¬focused`
  or `serial ≤` the serial behind the held selection. Otherwise `selection := (src, serial)` and
  `cached := slot`, and installing the new source destroys the displaced one. `destroy(src)` clears the
  selection when `src` held it. `read() = slot` if `selection` is `Some`, else `cached`.
- `HeadlessTrace(run)`: the sequence of `Event` variants that `EventLoop::{run_on_demand,pump_events}`
  pass to the callback on the Unix headless backend (`platform_impl/linux/headless.rs`).
- `Turn := NewEvents(cause) UserEvent* AboutToWait`. `InitTurn := NewEvents(Init) Resumed UserEvent* AboutToWait`.
- `dec(bytes)`: the percent decoder as a recursive function. `dec([]) = []`.
  `dec('%' h l r) = (hex(h)<<4 | hex(l)) :: dec(r)` when `hex(h)` and `hex(l)` are both `Some`.
  `dec(b r) = b :: dec(r)` otherwise. `hex: [0-9a-fA-F] → 0..=15`.
- `enc_xsettings(endian, serial, L)`: the XSETTINGS wire encoding. Byte order `'l'` or `'B'`, 3 pad bytes,
  a 4-byte serial, an `i32` count, then for each setting: type `i8`, pad 1, `name_len i16`, name, pad to 4,
  last-change serial `u32`, then the value (`i32` | `i32` len + bytes + pad to 4 | four `i16`).
- Auto-trait facts (`Send`/`Sync`) are discharged by rustc's trait solver. That spec is a non-test
  `const _` static assertion in the library, not a Clean proof.
- Gap numbers in `blocking_gaps` are the rows of the gap table in
  `AUDIT-trust-for-test-migration-2026-10-15.md` (historical internal audit) §7.
  Gap 0 (a seal that publishes Clean rows) blocks every spec proved through Clean.

## 1. Specs (TSV)

Columns: `id`, `kind`, `functions`, `statement`, `subsumes (primary tests; secondary in §2)`,
`difficulty (Rust features the proof must handle)`, `blocking_gaps`.

```tsv
id	kind	functions	statement	subsumes	difficulty	blocking_gaps
public-handles-send-sync	other (auto traits)	event_loop::EventLoopProxy, window::{Window,WindowAttributes,WindowId}, event::DeviceId, monitor::MonitorHandle	∀ T: 'static + Send: EventLoopProxy<T> is Send + Sync. Window and WindowAttributes are Send + Sync. WindowId, DeviceId and MonitorHandle are Send. This holds on every backend the crate compiles (the custom cursors, CustomCursorSource and CustomCursor, are gone: WC.5 left them off macOS, WC8-CARVE off Linux, 2026-10-21, and WC.5's Windows half cut them, 2026-10-25). Discharge: a library-level `const _: () = { const fn send<T: Send>() {} const fn sync<T: Sync>() {} … };` item in `src/lib.rs`, so rustc proves it on every build of the lib, not only under test.	none (its 9 integration tests are deleted)	TRIVIAL: auto traits, generic T, no runtime code; rustc proves it, not Clean	none
copy-outcome-placed	table-property	wayland::clipboard::CopyOutcome::placed	∀ o: CopyOutcome: placed(o) ⇔ o is not Refused(_). So it is true for Claimed and Kept, and false for Refused(NoSerial), Refused(NoDevice) and Refused(Unfocused). Decidable over the 5 values.	wayland/clipboard.rs::a_selection_the_compositor_binds_no_device_for_is_refused_and_placed_says_so	TRIVIAL: matches! on a nested Copy enum	0, 1, 3
headless-wait-deadline	contract (arithmetic)	headless::EventLoop::poll_events_with_timeout, common::poll::{wait, timeout_millis}, to be factored into pure deadline/poll_millis/cause fns (poll_millis is common::poll::timeout_millis since LW.7)	Factor out three pure fns. (a) deadline(pending, cf, timeout, start) = Some(0) if pending, else min_opt(by_cf, timeout), where by_cf is None for Wait, Some(0) for Poll and Some(r.saturating_duration_since(start)) for WaitUntil(r). (b) poll_millis(None) = -1, poll_millis(Some(0)) = 0, otherwise min(c_int::MAX, max(1, ceil(d.as_nanos()/1e6))); so poll_millis(Some(d))·1ms ≥ d unless it saturates. (c) cause(cf, now) = ResumeTimeReached ⇔ cf = WaitUntil(r) ∧ now ≥ r; Poll gives Poll; otherwise WaitCancelled. With poll(2) honouring its timeout (a platform assumption): a WaitUntil(r) turn with no proxy wake reports ResumeTimeReached and does not start before r.	headless.rs::a_due_wait_until_reports_resume_time_reached	EASY once factored: Duration/Instant arithmetic, u128 div_ceil, c_int::try_from, Option combinators. Only the poll(2) step needs gap 8.	0, 1, 3, 8
percent-decode-utf8	contract	linux::common::percent::{percent_decode_utf8,hex_val}	∀ data: &[u8]: percent_decode_utf8(data) = String::from_utf8(dec(data)) with the Utf8Error mapped. So the result is Ok(s) ⇔ dec(data) is valid UTF-8, and then s.as_bytes() = dec(data). hex_val(b) = Some(v) ⇔ b ∈ [0-9a-fA-F] with v its digit value (< 16); a table property decidable over 256 values. A '%' with fewer than two following bytes, or a non-hex digit, is copied verbatim. Corollary: ∀ s: &str, percent_decode_utf8(pct_encode(s)) = Ok(s) for an encoder that escapes any chosen subset of bytes.	common/percent.rs::percent_decode_basic_and_incomplete	EASY (hex_val: range match, evaluate 256 cases) / MEDIUM (decoder: while loop over a slice index with 3-byte lookahead, Vec<u8> push, u8 shifts and OR, String::from_utf8 needs a UTF-8 model)	0, 1, 3, 5, 7
event-map-nonuser	contract	event::Event::map_nonuser_event	∀ T, U, e: Event<T>: map_nonuser_event::<U>(e) = Err(e) ⇔ e is UserEvent(_). Otherwise it is Ok(e'), where e' has the same variant and the same payload (window_id/event, device_id/event, cause) as e, re-typed at U.	event.rs::test_map_nonuser_event	EASY in shape, heavy in data: 10-arm enum match, generic T and U, large nested enum payloads	0, 1, 3, 11
force-normalized	contract	event::Force::normalized	∀ f, m, a: f64: normalized(Normalized(f)) = f; normalized(Calibrated{force:f, max_possible_force:m, altitude_angle:None}) = f/m; normalized(Calibrated{f, m, Some(a)}) = (f / sin a) / m, in IEEE-754 f64 semantics. Corollary used by the test: when f/m is exact, the None and Some(PI/2) results agree, because sin(PI/2) rounds to 1.0. The statement is close to the body; its value is pinning the documented normalisation.	event.rs::test_force_normalize	EASY except f64::sin: f64 division, enum match with an Option field; sin is libm (needs an FFI or float model)	0, 1, 3, 8
pick-text-mime	contract	wayland::clipboard::pick_text_mime	∀ advertised: &[String]: pick_text_mime(advertised) = Some(TEXT_MIMES[k]) for the least k with TEXT_MIMES[k] ∈ advertised, or None if there is no such k. The result never depends on the order or multiplicity of advertised and is always an element of TEXT_MIMES. Priority: text/plain;charset=utf-8 > UTF8_STRING > text/plain > TEXT > STRING.	wayland/clipboard.rs::a_paste_prefers_utf8_then_the_legacy_text_types	EASY-MEDIUM: nested iterator find/any with closures, String equality against &str, const [&str;5]	0, 1, 5, 7, 13
smolstr-text-round-trip	round-trip	keyboard::smol_str::SmolStr::{new,as_str,clone}, <SmolStr as PartialEq/Ord/Hash/Debug/Display>	∀ t: &str: SmolStr::new(t).as_str() = t. The representation is Inline ⇔ t.len() ≤ INLINE_CAP (22), else Shared. as_str never panics: the expect in the Inline arm is unreachable, because new copies exactly len bytes of valid UTF-8. ∀ s: s.clone().as_str() = s.as_str(). eq, cmp, hash, Debug and Display of a SmolStr equal those of its as_str(). Layout: size_of::<SmolStr>() = 24 on 64-bit targets, as a static assertion.	keyboard/smol_str.rs::text_round_trips_on_both_sides_of_the_inline_limit	MEDIUM: &str and [u8;22] contents, enum Repr, u8::try_from, copy_from_slice, str::from_utf8 (UTF-8 model), Arc<str>, generic T: AsRef<str>, trait impls	0, 1, 3, 11, 14
selection-port-copy	contract	wayland::clipboard::SelectionPort::copy (default method), CLAIM_ORDER	∀ port P, sel, serial: (1) ¬P.focused() ⇒ PortTrace has no release or claim, and copy returns Kept if P.offers(sel), else Refused(Unfocused). (2) P.focused() ⇒ PortTrace restricted to mutators is exactly [release(sel), claim(sel, serial)], in that order, and copy returns Claimed iff that claim returned true, else Refused(NoDevice). copy never returns Refused(NoSerial).	wayland/clipboard.rs::{the_copy_a_focused_client_makes_releases_before_it_claims, an_unfocused_copy_keeps_the_selection_it_holds_instead_of_walking_the_clipboard_backwards, an_unfocused_copy_that_holds_no_selection_reports_the_refusal_rather_than_success}	MEDIUM: trait default method generic over Self with &mut self, a for loop over a const array, enum match, a ghost trace of calls to abstract trait methods	0, 2, 3, 4, 7, 11
clip-release-before-claim-lands	refinement	wayland::clipboard::SelectionPort::copy, CLAIM_ORDER, CompositorModel (ty_model!)	Under M with focused ∧ device, for every sequence of copies (text_i, serial_i), including copies that reuse one serial: after each copy, M.read() = text_i and the outcome is Claimed. Negative control: the order [Claim] alone (the install destroys the displaced source) violates this from the second copy that reuses a serial. With selection-port-copy: an unfocused copy that offers a source leaves M.read() at the slot, the newest text.	wayland/clipboard.rs::a_copy_with_no_fresh_input_behind_it_still_reaches_the_system_clipboard	MEDIUM-HARD: model-to-code refinement of a ty_model! compositor bound to a trait default method; &mut port state; String slot	0, 1, 2, 10, 11
xsettings-decode	round-trip	linux::x11::xsettings::{read_settings, Parser::{new,advance,pad,i8,i16,i32}, Setting::parse, SettingType::try_from}	(a) ∀ endian ∈ {'l','B'}, serial, well-formed setting lists L with count ≥ 0 (names and strings of any length, ints, colors): read_settings(enc_xsettings(endian, serial, L)) = Ok(it), and it yields exactly L in order, each as Ok. (b) Short header: read_settings(d) = Err(NoMoreBytes{expected:1, found:0}) ⇔ d is empty; for 1 ≤ |d| < 8 it is Err(NoMoreBytes{expected:7, found:|d|-1}); for 8 ≤ |d| < 12 it is Err(NoMoreBytes{expected:4, found:|d|-8}). (c) Parser::advance(n) either returns the next n bytes and shrinks the cursor by n, or returns Err(NoMoreBytes{n, remaining}) and leaves the cursor unchanged. pad(size, 4) advances (4 - size%4)%4 bytes. (d) A negative list count is Err(NegativeCount(count)); a malformed setting yields one final Err, then the iterator ends even when its declared count is huge. No input panics.	x11/xsettings.rs::{empty, parse_xsettings}	MEDIUM-HARD: &mut Parser cursor over &[u8] with lifetimes, split_at, try_into [u8;N], from_be/le_bytes, iter::from_fn with an FnMut closure, checked i32 → usize count, length casts i32/i16 → usize, NonZeroUsize::unwrap	0, 1, 2, 3, 5, 7, 13
parse-uri-list	contract	linux::common::percent::uri_list_entries, wayland::seat::data_device::parse_uri_list, x11::dnd::file_uri_path	Frame raw LF/CRLF-separated entries, trim ASCII list whitespace, and skip empty/comment entries before decoding. Only a raw file:// scheme with an empty authority (path starts /) is local; decode percent escapes only within that path. Encoded CRLF, #, trailing whitespace and embedded file:// stay path content. Wayland skips unsupported/invalid-UTF8 entries and returns canonicalize(path) or path; X11 errors on an unsupported/invalid entry or failed canonicalization. Nonempty hosts, including localhost, remain unsupported. Output order follows input entries.	wayland/seat/data_device.rs::{parse_uri_list_extracts_file_paths, parse_uri_list_skips_comments_hostnames_and_other_schemes, parse_uri_list_empty_is_empty}; x11/dnd.rs::uri_list_decodes_paths_after_framing_and_preserves_literal_uri_text	MEDIUM: raw-byte split/trim, strip_prefix, iterator filter, Vec<PathBuf>, composition with percent-decode-utf8, fs::canonicalize	0, 1, 4, 5, 7, 8, 13
copy-verdict	contract	wayland::clipboard::copy_verdict	∀ receiver rx: copy_verdict(rx) ⇔ rx yields some outcome o within COPY_TIMEOUT ∧ placed(o). A dropped sender with nothing queued, or an elapsed timeout, gives false. A queued outcome is consumed in FIFO order.	wayland/clipboard.rs::a_copy_off_the_loop_thread_answers_the_loop_s_verdict_and_never_guesses	EASY logic over an effect: mpsc::Receiver::recv_timeout (channel and time), Result match	0, 8, 13
wayland-clipboard-handle	contract	wayland::clipboard::WaylandClipboard::{new,copy,paste_owned,paste,send}, Shared::slot	For a handle h with loop thread L, any sel and text: (1) receiver gone ⇒ h.copy(sel, text) = false and afterwards slot(sel) = None. (2) On thread L with a successful send, h.copy returns true without waiting and slot(sel) = Some(text). (3) h.paste_owned(sel) = slot(sel) filtered to non-empty; a copy to sel leaves the other selection's slot unchanged. (4) On thread L, h.paste(sel) = h.paste_owned(sel) and sends no request. (5) slot = None (a disown) ⇒ paste_owned = None. A poisoned Mutex gives false/None and never panics.	wayland/clipboard.rs::{a_copy_with_no_loop_left_to_answer_it_is_false_and_leaves_no_own_slot_behind, the_own_slot_answers_synchronously_and_a_foreign_slot_answers_none}	MEDIUM-HARD: Arc<Mutex<Shared>> with an &mut Option<String> projection, an mpsc Sender and the clipboard's own eventfd waker (calloop's channel Sender until LW.7), thread::current().id(), Option<String> filter	0, 1, 2, 8, 13
headless-trace-grammar	invariant	linux::headless::EventLoop::{run_on_demand,pump_events,single_iteration,poll_events_with_timeout}, ActiveEventLoop::{exit,exiting,exit_code}	∀ callback, schedule of proxy sends: HeadlessTrace(run_on_demand) is a prefix of InitTurn Turn* LoopExiting, and it ends in LoopExiting whenever the run returns. Every Turn after the init turn has cause ≠ Init. Resumed occurs exactly once, second in the trace. If exit is set during turn k, no NewEvents follows turn k (so an exit on InitTurn yields exactly one NewEvents). run_on_demand returns Ok ⇔ the exit code is 0, else Err(ExitFailure(code)). pump_events resets loop_running on exit, so a later pump_events starts again with InitTurn.	headless.rs::{the_first_turn_is_init_then_resumed_and_the_last_is_loop_exiting, an_exit_on_the_init_turn_runs_no_further_turn}	HARD: &mut self loop, generic FnMut callback receiving &RootELW, Cell<ControlFlow>/Cell<Option<i32>>, Mutex<VecDeque<T>>; best stated as a ty_model! turn machine with a conformance binding	0, 2, 3, 7, 10, 11, 13
headless-proxy-delivery	contract (liveness)	linux::headless::{EventLoopProxy::send_event, EventLoop::{create_proxy,single_iteration,poll_events_with_timeout}}, linux::common::poll::{Waker::{new,wake,drain}, wait, timeout_millis}	∀ proxy p of loop l, event e: p.send_event(e) = Ok before l exits ⇒ e is delivered as exactly one UserEvent(e) in a turn that starts no earlier than the send, in FIFO order with p's other sends. A send to a loop blocked in ControlFlow::Wait ends that wait, because the eventfd becomes readable. send_event never blocks (O_NONBLOCK eventfd write; EAGAIN, a counter already at its ceiling, is a wake already pending). It returns Err(EventLoopClosed(e)) only when the queue mutex is poisoned.	headless.rs::{an_event_posted_before_the_loop_runs_is_still_delivered, an_event_posted_after_init_is_delivered_from_another_thread_under_wait}	HARD: unsafe libc eventfd/read/write/poll, Arc<OwnedFd>, Arc<Mutex<VecDeque<T>>>, cross-thread liveness	0, 2, 7, 8, 13
windows-drop-ownership	contract	windows::drop_handler::{FileDropHandler::QueryInterface, StorageMedium::drop, FileDropHandler::iterate_filenames}	QueryInterface supports IUnknown and IDropTarget with one pointer identity and AddRef per success; null arguments return E_POINTER and unknown IID returns E_NOINTERFACE with a null output. Each successful GetData medium is released exactly once through ReleaseStgMedium, honoring pUnkForRelease on hover, drop and wrong-type early return.	windows/drop_handler.rs::{queries_preserve_interface_identity_and_own_each_successful_reference, storage_medium_releases_its_declared_owner_once}	HARD: raw COM pointers, repr(C) identity, ownership/refcounts, extern callbacks and OLE release	0, 2, 8
windows-ime-events	contract	windows::ime::{ime_events,composing_text_and_cursor,composed_text,CompositionSource,LazyImeContext}, windows::event_loop (the WM_IME_STARTCOMPOSITION, WM_IME_COMPOSITION and WM_IME_ENDCOMPOSITION arms)	The window procedure's IME arms are ime_events(msg, lparam, allowed, state, source), with the window's state written back before the events are sent and the input context taken only when the composition is read. START with allowed: state = Enabled, [Enabled]. COMPOSITION with allowed and state ≠ Disabled: [Preedit('', None)] when lparam = 0; with GCS_RESULTSTR and a result r: state = Enabled, [Preedit('', None), Commit(r)]; then with GCS_COMPSTR and a composition c: state = Preedit, [Preedit(c, range)] where range is the targeted clause (ATTR_TARGET_CONVERTED/NOTCONVERTED) in UTF-8 offsets, else the caret. END with allowed or state ≠ Disabled: [Preedit('', None), Commit(r)] first when state = Preedit and a result r is readable, then state = Disabled, [Disabled]. Otherwise []. So a Japanese conversion commits once, and the Hangul IME's result sent again after END (state Disabled) commits nothing more.	windows/recorded.rs::{an_input_methods_result_is_committed_once, the_hangul_ime_commits_once}	EASY-MEDIUM: a three-state machine over &mut ImeState, u32 flag tests, a trait-object source of UTF-16 bytes	0, 8
windows-key-replay	contract	windows::keyboard::{KeyEventBuilder::process_message, replay, key_state, mapping_layout}, windows::keyboard_layout::LayoutCache::{get_current_layout, get_agnostic_mods}, windows::recorded::{replay, Typist, layouts}	While a recorded sequence is replayed on a thread (tests and debug builds only), the key path's reads of Windows (PeekMessageW for the next key message, GetKeyboardState, GetKeyState, GetKeyboardLayout) answer from the recording, and MapVirtualKeyExW maps with the thread's own layout; outside a replay they read Windows. Replayed: de-DE AltGr+Q (a left Ctrl press, then the right Alt, then Q and its WM_CHAR '@') yields the AltGraph press and Character('@') with text '@', no Ctrl key event, and no modifiers while AltGr is down; a dead key yields Dead(accent) with no text and the next key its composed character (de-DE ^ then e: 'ê'), Space the accent as Space's text, and a key that does not compose both characters as text on its own key (US-International ' then s: Character('s'), text "'s"); Alt 0 2 3 3 on the keypad yields Unidentified(VK_NUMPADn) digits and, after Alt's release, Commit('é'); VK_PACKET with a surrogate pair yields one character of text on Unidentified(VK_PACKET).	windows/recorded.rs::{altgr_drops_its_fake_ctrl_and_types_the_character_bare, a_dead_key_composes_with_the_next_key, an_alt_code_commits_its_character, a_packet_key_carries_its_text}	HARD: a thread-local replay standing in for user32 reads, the PendingEventQueue re-entrancy, Mutex<LayoutCache>, hand-recorded layout tables (not the shipped DLLs')	0, 2, 8
windows-attention-flash	contract	windows::window::{attention_flash, Window::request_user_attention}	attention_flash(h, Some(Informational)) = FLASHWINFO{cbSize = its size (32 on a 64-bit target), hwnd h, FLASHW_TRAY | FLASHW_TIMERNOFG, count 0, timeout 0}: the taskbar button flashes until the window comes forward; Some(Critical) is FLASHW_ALL | FLASHW_TIMERNOFG with count u32::MAX; None is FLASHW_STOP. request_user_attention makes no FlashWindowEx call for the active window.	windows/window.rs::a_bell_flashes_the_taskbar_button_until_the_window_comes_forward	TRIVIAL: an Option match building a repr(C) struct	0
macos-proxy-queue	contract (liveness)	macos::wake_queue::{channel, Sender::send, Receiver::{try_next, has_pending, refill}, UnfairMutex::{lock, try_lock}}, macos::event_loop::EventLoopProxy::send_event	∀ senders, events: try_next hands out every sent event exactly once, in send order; an event sent during a drain comes after the events already taken. A send after the Receiver is dropped returns Err(event). The receiver never waits on a sender: while a sender holds the queue's lock, try_next and has_pending return without that sender's event, and send_event signals the loop's wake source after its push, so the loop comes round again and takes it. Wakes are coalesced: send_event signals only when its send armed the wake flag (clear → set, after its push), and the receiver clears the flag before every take, whatever the take finds, so a sender that held the lock through a drain still signals; the AppKit loop wakes itself once more when the flag is set at the end of its BeforeWaiting turn. (std::mpsc's try_recv waits on a sender part-way through a write by spinning and then calling sched_yield. On macOS that depresses the main thread's priority for one 10 ms quantum.)	macos/wake_queue.rs::{events_arrive_oldest_first_across_senders_and_drains, a_send_after_the_receiver_is_gone_hands_its_event_back, a_drain_returns_at_once_while_a_sender_holds_the_lock, only_the_send_that_arms_the_wake_owes_a_signal, a_drain_that_finds_the_lock_held_still_clears_the_flag}	MEDIUM: Arc<UnfairMutex<VecDeque<T>>> (os_unfair_lock over FFI, a !Send guard) with try_lock, RefCell, cross-thread liveness tied to CFRunLoopSourceSignal	0, 2, 8, 13
```

Test paths in `subsumes` are shortened: `wayland/…`, `common/…`, `x11/…` and `headless.rs` sit under
`src/platform_impl/linux/`; `event.rs` and `keyboard/…` under `src/`.

### Distribution by kind (17)

| kind | count | ids |
|---|---|---|
| contract | 11 | headless-wait-deadline, percent-decode-utf8, event-map-nonuser, force-normalized, pick-text-mime, selection-port-copy, parse-uri-list, copy-verdict, wayland-clipboard-handle, headless-proxy-delivery, windows-drop-ownership |
| round-trip | 2 | smolstr-text-round-trip, xsettings-decode |
| table-property | 1 | copy-outcome-placed |
| refinement | 1 | clip-release-before-claim-lands |
| invariant | 1 | headless-trace-grammar |
| other | 1 | public-handles-send-sync (auto traits, rustc) |

### Gap histogram (specs blocked by each audit §7 gap)

| gap | specs | notes |
|---|---|---|
| none | 1 | public-handles-send-sync (rustc, no Clean) |
| 0 | 16 | every Clean-proved spec |
| 1 | 11 | non-scalar carriers: &str, String, Vec, Option, enums |
| 3 | 9 | E6 body widening: enum matches, calls, casts, shifts |
| 7 | 7 | loops and iterators |
| 13 | 7 | closures, threads, channels |
| 2 | 7 | &self/&mut self views |
| 8 | 7 | effects: libm sin, fs::canonicalize, mpsc, poll(2), pipes |
| 11 | 5 | generics |
| 5 | 4 | quantified clauses |
| 4 | 2 | callee-ensures composition |
| 10 | 2 | model→code refinement |
| 14 | 1 | Clean elaborator coverage |

### Proof dependency order

`copy-outcome-placed` → `selection-port-copy` → `clip-release-before-claim-lands` (with the `FakeSeat`
negative control) → `copy-verdict` and `wayland-clipboard-handle`. `percent-decode-utf8` →
`parse-uri-list`. The proposed pure `headless-wait-deadline` factor → `headless-trace-grammar` →
`headless-proxy-delivery`. `public-handles-send-sync`, `smolstr-text-round-trip`, `event-map-nonuser`,
`force-normalized`, `pick-text-mime` and `xsettings-decode` stand alone.

## 2. Historical test → spec map (TSV)

`primary` is the spec that retires the test. `also` lists the other specs the same test samples.

```tsv
file	test	primary	also
src/event.rs	test_map_nonuser_event	event-map-nonuser	
src/event.rs	test_force_normalize	force-normalized	
src/keyboard/smol_str.rs	text_round_trips_on_both_sides_of_the_inline_limit	smolstr-text-round-trip	
src/platform_impl/linux/common/percent.rs	percent_decode_basic_and_incomplete	percent-decode-utf8	parse-uri-list
src/platform_impl/linux/headless.rs	the_first_turn_is_init_then_resumed_and_the_last_is_loop_exiting	headless-trace-grammar	
src/platform_impl/linux/headless.rs	an_exit_on_the_init_turn_runs_no_further_turn	headless-trace-grammar	
src/platform_impl/linux/headless.rs	an_event_posted_before_the_loop_runs_is_still_delivered	headless-proxy-delivery	headless-trace-grammar
src/platform_impl/linux/headless.rs	an_event_posted_after_init_is_delivered_from_another_thread_under_wait	headless-proxy-delivery	headless-trace-grammar
src/platform_impl/linux/headless.rs	a_due_wait_until_reports_resume_time_reached	headless-wait-deadline	headless-trace-grammar (due-deadline transition only)
src/platform_impl/linux/wayland/clipboard.rs	the_copy_a_focused_client_makes_releases_before_it_claims	selection-port-copy	(wire-trace assertions, §5)
src/platform_impl/linux/wayland/clipboard.rs	an_unfocused_copy_keeps_the_selection_it_holds_instead_of_walking_the_clipboard_backwards	selection-port-copy	clip-release-before-claim-lands
src/platform_impl/linux/wayland/clipboard.rs	an_unfocused_copy_that_holds_no_selection_reports_the_refusal_rather_than_success	selection-port-copy	copy-outcome-placed
src/platform_impl/linux/wayland/clipboard.rs	a_selection_the_compositor_binds_no_device_for_is_refused_and_placed_says_so	copy-outcome-placed	selection-port-copy
src/platform_impl/linux/wayland/clipboard.rs	a_copy_with_no_fresh_input_behind_it_still_reaches_the_system_clipboard	clip-release-before-claim-lands	selection-port-copy
src/platform_impl/linux/wayland/clipboard.rs	claiming_before_releasing_loses_every_other_copy_and_leaves_the_text_before_it	NON-SPEC (tests test-only code)	becomes the negative control of clip-release-before-claim-lands
src/platform_impl/linux/wayland/clipboard.rs	a_copy_off_the_loop_thread_answers_the_loop_s_verdict_and_never_guesses	copy-verdict	copy-outcome-placed
src/platform_impl/linux/wayland/clipboard.rs	a_copy_with_no_loop_left_to_answer_it_is_false_and_leaves_no_own_slot_behind	wayland-clipboard-handle	
src/platform_impl/linux/wayland/clipboard.rs	the_own_slot_answers_synchronously_and_a_foreign_slot_answers_none	wayland-clipboard-handle	
src/platform_impl/linux/wayland/clipboard.rs	a_paste_prefers_utf8_then_the_legacy_text_types	pick-text-mime	
src/platform_impl/linux/wayland/seat/data_device.rs	parse_uri_list_extracts_file_paths	parse-uri-list	percent-decode-utf8
src/platform_impl/linux/wayland/seat/data_device.rs	parse_uri_list_skips_comments_hostnames_and_other_schemes	parse-uri-list	
src/platform_impl/linux/wayland/seat/data_device.rs	parse_uri_list_empty_is_empty	parse-uri-list	
src/platform_impl/linux/x11/dnd.rs	uri_list_decodes_paths_after_framing_and_preserves_literal_uri_text	parse-uri-list	
src/platform_impl/linux/x11/xsettings.rs	empty	xsettings-decode	
src/platform_impl/linux/x11/xsettings.rs	parse_xsettings	xsettings-decode	
src/platform_impl/windows/drop_handler.rs	queries_preserve_interface_identity_and_own_each_successful_reference	windows-drop-ownership	
src/platform_impl/windows/drop_handler.rs	storage_medium_releases_its_declared_owner_once	windows-drop-ownership	
src/platform_impl/macos/wake_queue.rs	events_arrive_oldest_first_across_senders_and_drains	macos-proxy-queue	
src/platform_impl/macos/wake_queue.rs	a_send_after_the_receiver_is_gone_hands_its_event_back	macos-proxy-queue	
src/platform_impl/macos/wake_queue.rs	a_drain_returns_at_once_while_a_sender_holds_the_lock	macos-proxy-queue	
src/platform_impl/macos/wake_queue.rs	only_the_send_that_arms_the_wake_owes_a_signal	macos-proxy-queue	
src/platform_impl/macos/wake_queue.rs	a_drain_that_finds_the_lock_held_still_clears_the_flag	macos-proxy-queue	
```

## 3. Coverage

- 36 original tests read. 32 mapped to one of the 16 specs. 4 were not spec samples (§4). 12 of the
  36 are since deleted: the 9 auto-trait checks and 3 of the 4 non-spec tests.
- The biggest collapse was `public-handles-send-sync` (9 tests across two integration files, now
  deleted; one library `const _` item, not yet added, would prove the property). Next: `selection-port-copy`, `parse-uri-list` (3 each).
- 18 of the 36 tests sit in effectful files (headless event loop, Wayland clipboard, data device). Their
  pure cores (`copy-outcome-placed`, `selection-port-copy`, `pick-text-mime`, `headless-wait-deadline`,
  `percent-decode-utf8`) can be proved before Trust's platform lane (gap 8) lands.

## 4. Tests that are not spec samples

> Status 2026-10-16: the 3 derive/prose checks in `src/error.rs` and `src/event.rs` that this
> table marked for deletion are deleted, and their rows are gone from it and from §2.

| test | why | action |
|---|---|---|
| wayland/clipboard.rs::claiming_before_releasing_loses_every_other_copy_and_leaves_the_text_before_it | Drives only the `#[cfg(test)]` `FakeSeat::claim` with the old `[Claim]`-only order, not production code. | **delete as a test; keep its content as the negative control** of `clip-release-before-claim-lands` (PLAN done-criterion 3) |

Status: the compositor negative control remains a migration candidate for folding into its conformance test.

## 5. Implied specs that look wrong, or disagree with the code

1. **Headless timing assertion removed.** The WaitUntil regression now uses an already due
   deadline and checks ResumeTimeReached without a sleep-duration assertion. Rounding and
   minimum-wait claims remain proof work; this test covers the event transition only.
2. **Headless test driver bounded.** Its on_turn callback decides when to exit; timed pumps
   provide a one-minute hang detector while preserving the requested ControlFlow. The cross-thread
   regression uses an explicit first-turn channel handoff instead of an arbitrary sleep. It proves
   delivery after Init under Wait; deterministic wake-after-kernel-block ordering remains untested.
3. **URI framing corrected.** Wayland and X11 now share raw-entry framing before decoding
   each local path. Encoded separators cannot add entries; encoded scheme/authority text cannot
   create a local URI. X11 strips its scheme once, preserving URI-looking filename content, and
   Wayland preserves encoded trailing whitespace. Nonempty hosts, including `localhost`, remain
   an explicit compatibility boundary.
4. **Xsettings count admission corrected.** Negative counts are rejected, and the iterator ends
   after the first malformed entry even with a large positive count. Both endian regressions
   cover rejection and termination alongside the existing valid-settings fixture.
5. **Event mapping samples strengthened.** The existing map_nonuser_event regression checks
   the resulting variant and payload against its input; nonzero movement/resize values make
   payload loss observable. MemoryWarning was retired with the mobile backends and is not a
   live variant to cover.
6. **Redundant clipboard order assertion removed.** The production wire-trace assertions remain;
   the duplicate CLAIM_ORDER constant assertion no longer substitutes for behavior coverage.
7. **`force-normalized` restates the body.** The spec is close to the code; it is worth proving only
   because it pins the documented normalisation, and it needs a model of `f64::sin`.

## 6. Proof order (easiest high-value first)

1. **`public-handles-send-sync`**. No Trust gap at all: add the `const _` auto-trait assertion item to
   the library. The 9 compile-only `Send`/`Sync` tests it was to retire are already deleted.
2. **`copy-outcome-placed`**. Blocked only by gap 0 plus enum carriers (gaps 1, 3); decidable by
   evaluation over 5 values. It is the lemma `copy-verdict` and `selection-port-copy` cite.
3. **`headless-wait-deadline`** (pure factor). Linear `Duration`/`Option` arithmetic plus a
   `div_ceil`, an `omega`-class proof once factored, with the due-deadline transition test retained for event ordering.
4. **`percent-decode-utf8`**, starting with the `hex_val` table half (a range match over `u8`, which seal
   `150efe4a` already admits for E6 import). The decoder half then unlocks `parse-uri-list` (4 primary tests).
5. **`selection-port-copy`** then **`clip-release-before-claim-lands`**. These are the Wayland clipboard's
   real correctness property (release before claim), with the negative control already written as
   `FakeSeat`. They need `&mut self` views (gap 2) and a `ty_model!` refinement (gap 10).
6. Then `event-map-nonuser`, `pick-text-mime`, `smolstr-text-round-trip` and `xsettings-decode` (carrier
   and loop work), and last the effectful shells: `copy-verdict`, `wayland-clipboard-handle`,
   `headless-trace-grammar` and `headless-proxy-delivery` (gaps 8 and 13), and `force-normalized` (libm).
