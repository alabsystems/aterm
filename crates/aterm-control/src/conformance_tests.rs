// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// The clipboard-less host answers `ERR unsupported` for `copy` — the
/// capability flag is load-bearing, not decorative.
#[test]
fn a_host_without_a_clipboard_refuses_copy() {
    let host = MemoryHost::without_clipboard(7);
    for o in run_all(&host, 7) {
        assert!(o.passed(), "{}: {:?}", o.check, o.failure);
    }
    assert_eq!(selection::cmd_copy(&host, 7), "ERR unsupported\n");
}

/// A sid the host does not resolve is refused by every verb, rather than
/// answered with whatever session the host does have.
#[test]
fn an_unresolved_sid_is_refused() {
    let host = MemoryHost::new(7);
    for reply in [
        selection::cmd_blocks(&host, 9, ""),
        selection::cmd_blocks_json(&host, 9, ""),
        selection::cmd_blocktext(&host, 9, "0"),
        selection::cmd_wait(&host, 9, "0"),
        selection::cmd_select(&host, 9, "clear"),
        selection::cmd_selection(&host, 9),
        selection::cmd_copy(&host, 9),
    ] {
        assert_eq!(reply, "ERR no such session\n");
    }
}

/// `blocks --json` gives where the shell's `133;B` put the input's start
/// (the column after the prompt), whatever was typed after it, which tells
/// typeahead from the prompt.
#[test]
fn blocks_json_says_the_column_the_input_starts_at() {
    let host = MemoryHost::new(7);
    host.feed(b"\x1b]133;A\x07~/proj % ");
    let before = selection::cmd_blocks_json(&host, 7, "1");
    assert!(
        before.contains("\"state\":\"prompt\"") && before.contains("\"cmd\":null,\"cmdcol\":null,"),
        "{before}"
    );
    host.feed(b"\x1b]133;B\x07rm -rf build");
    let after = selection::cmd_blocks_json(&host, 7, "1");
    assert!(after.contains("\"state\":\"entering\""), "{after}");
    assert!(after.contains("\"cmdcol\":9,\"out\":null"), "{after}");
}

/// `blocks --json` carries a cwd (OSC 7) and a command line (OSC 633 E)
/// that a program chose, and a person may read the reply raw on a
/// terminal: no character a terminal acts on or reorders text around
/// (C0, DEL, C1, bidi, U+2028/9) reaches the reply's bytes — each is a
/// `\uXXXX` escape — and a JSON reader still decodes both fields to exactly
/// what the program sent.
#[test]
fn blocks_json_escapes_what_a_terminal_acts_on_and_decodes_back() {
    let host = MemoryHost::new(0);
    host.feed(
        b"\x1b]7;file://evilhost/tmp/CWD%1B]52;c;Q1dE%07%1B[2K%0DFAKE%C2%9B31m%E2%80%AEdcba\
          %E2%80%A8%7F\x07",
    );
    host.feed(b"\x1b]133;A\x07$ \x1b]133;B\x07");
    host.feed("\x1b]633;E;CMD\u{9b}2J\u{9d}0 pwned\u{9c}\u{202e}txt.exe\x07".as_bytes());
    host.feed(b"\x1b]133;C\x07out\r\n\x1b]133;D;0\x07");
    let reply = selection::cmd_blocks_json(&host, 0, "");
    let body = reply.strip_prefix("OK 1\n").expect("one JSON row");
    let body = body.strip_suffix('\n').expect("one JSON row");
    let active: Vec<String> = body
        .chars()
        .filter(|&c| {
            matches!(c as u32, 0..=0x1f | 0x7f..=0x9f | 0x61c | 0x200e | 0x200f)
                || matches!(c as u32, 0x2028..=0x202e | 0x2066..=0x2069)
        })
        .map(|c| format!("U+{:04X}", c as u32))
        .collect();
    assert!(active.is_empty(), "raw {active:?} in {body:?}");
    assert!(
        body.contains("FAKE\\u009b31m\\u202edcba\\u2028\\u007f"),
        "{body}"
    );
    let value: aterm_json::Value = aterm_json::from_str(body).expect("valid JSON");
    let block = &value["blocks"][0];
    assert_eq!(
        block["cwd"].as_str(),
        Some("/tmp/CWD\u{1b}]52;c;Q1dE\u{7}\u{1b}[2K\rFAKE\u{9b}31m\u{202e}dcba\u{2028}\u{7f}")
    );
    assert_eq!(
        block["cmdline"].as_str(),
        Some("CMD\u{9b}2J\u{9d}0 pwned\u{9c}\u{202e}txt.exe")
    );
}

/// The verbs are wired to the host's real terminal and clipboard: drive a
/// selection over live content, read it back, and copy it.
#[test]
fn select_reads_back_and_copies_live_content() {
    let host = MemoryHost::new(0);
    host.feed(b"hello world");
    assert_eq!(selection::cmd_select(&host, 0, "0 0 0 4"), "OK\n");
    let sel = selection::cmd_selection(&host, 0);
    assert!(sel.starts_with("OK 1\n") && sel.contains("hello"), "{sel}");
    assert_eq!(selection::cmd_copy(&host, 0), "OK 5\n");
    assert_eq!(host.clipboard().as_deref(), Some("hello"));
    assert!(host.redraws() >= 1, "select must ask for a repaint");
    for bad in [
        "line +0",
        "line -0",
        "line 0 junk",
        "word 0 1 junk",
        "block 0 0 0 1 junk",
        "extend 0 1 junk",
        "clear extra",
        "0 0 0 4 9",
    ] {
        assert!(
            selection::cmd_select(&host, 0, bad).starts_with("ERR usage: "),
            "`select {bad}`"
        );
    }
    for bad in ["+0 0 0 4", "-0 0 0 4", "0 +0 0 4"] {
        assert_eq!(
            selection::cmd_select(&host, 0, bad),
            "ERR bad args\n",
            "`select {bad}`"
        );
    }
    assert_eq!(selection::cmd_select(&host, 0, "line -1"), "OK\n");
}

/// `wait` parks on the host's change signal and reports the completion that
/// lands while it is parked — the event-driven path, not a poll.
#[test]
fn wait_reports_a_completion_that_lands_while_parked() {
    let host = MemoryHost::new(0);
    host.feed(b"\x1b]133;A\x07$ \x1b]133;B\x07sleep\n\x1b]133;C\x07");
    let (term, changed, parks) = host.producer();
    let h = std::thread::spawn(move || {
        aterm_spec::wait::until_within(aterm_spec::wait::MUST_HAPPEN, "the wait to park", || {
            (parks.load(Ordering::SeqCst) > 0).then_some(())
        });
        term.lock()
            .unwrap_or_else(|p| p.into_inner())
            .process(b"\x1b]133;D;0\x07");
        let (lock, cv) = &*changed;
        let mut g = lock.lock().unwrap_or_else(|p| p.into_inner());
        *g = g.wrapping_add(1);
        drop(g);
        cv.notify_all();
    });
    let resp = selection::cmd_wait(&host, 0, "60000");
    h.join().unwrap();
    assert!(
        resp.starts_with("OK complete ") && resp.contains("exit=0"),
        "{resp}"
    );
}

/// `wait <ms>` reads its `<ms>` strictly: a tail that is not a whole number of
/// milliseconds is the usage line.
#[test]
fn a_wait_ms_that_is_not_a_number_is_the_usage_line() {
    let host = MemoryHost::new(0);
    host.feed(b"\x1b]133;A\x07$ \x1b]133;B\x07true\n\x1b]133;C\x07\x1b]133;D;0\x07");
    for tail in ["2s", "1.5", "-1", "+5", "0x10", "5 6", "timeout=5"] {
        assert_eq!(
            selection::cmd_wait(&host, 0, tail),
            "ERR usage: wait [<ms>]\n",
            "`wait {tail}`"
        );
    }
    assert_eq!(
        crate::wire::wait_ms("99999999999"),
        Some(crate::wire::WAIT_MAX_MS)
    );
    assert_eq!(
        crate::wire::wait_ms(" 5"),
        None,
        "the caller trims, not the reader"
    );
}

/// When a block is already complete and none is executing, `wait` answers
/// immediately without parking.
#[test]
fn wait_returns_an_existing_completion_without_parking() {
    let host = MemoryHost::new(0);
    host.feed(b"\x1b]133;A\x07$ \x1b]133;B\x07true\n\x1b]133;C\x07\x1b]133;D;0\x07");
    for tail in ["", "0", "99999999999"] {
        let reply = selection::cmd_wait(&host, 0, tail);
        assert!(reply.starts_with("OK complete "), "`wait {tail}`: {reply}");
    }
}

/// `blocks`/`blocktext` over real OSC-133 content, so `check_blocks` sees a
/// non-empty block list.
#[test]
fn the_block_checks_hold_on_a_populated_session() {
    let host = MemoryHost::new(0);
    host.feed(
        b"\x1b]133;A\x07$ \x1b]633;E;echo hi\x07\x1b]133;B\x07echo hi\n\x1b]133;C\x07hi\n\x1b]133;D;0\x07",
    );
    let listed = selection::cmd_blocks(&host, 0, "");
    assert!(listed.starts_with("OK 1\n"), "{listed}");
    assert!(listed.contains("cmdline=echo%20hi"), "{listed}");
    let txt = selection::cmd_blocktext(&host, 0, "0");
    assert!(txt.starts_with("OK ") && txt.contains("hi"), "{txt}");
    assert_eq!(
        selection::cmd_blocktext(&host, 0, "+0"),
        "ERR usage: blocktext <id>\n"
    );
    for o in [check_blocks(&host, 0), check_blocks_json(&host, 0)] {
        assert!(o.passed(), "{}: {:?}", o.check, o.failure);
    }
}

/// A block's cwd reaches the wire through [`SessionHost::native_cwd`] in
/// both `blocks` forms: the default prints the engine's RFC 8089 path
/// verbatim; a host may name paths its own way (the Windows GUI prints
/// `C:\Users\x` for pwsh's `file:///C:/Users//x`).
#[test]
fn a_block_cwd_is_printed_through_the_hosts_native_cwd() {
    /// [`MemoryHost`] with only the path naming changed, marked so the
    /// assertion cannot pass by coincidence on any platform.
    struct Renaming(MemoryHost);
    impl SessionHost for Renaming {
        fn capabilities(&self) -> HostCapabilities {
            self.0.capabilities()
        }
        fn sessions(&self) -> Vec<SessionEntry> {
            self.0.sessions()
        }
        fn resolve(&self, selector: Selector<'_>) -> Option<u64> {
            self.0.resolve(selector)
        }
        fn with_terminal<R>(&self, sid: u64, f: impl FnOnce(&Terminal) -> R) -> Option<R> {
            self.0.with_terminal(sid, f)
        }
        fn with_terminal_mut<R>(&self, sid: u64, f: impl FnOnce(&mut Terminal) -> R) -> Option<R> {
            self.0.with_terminal_mut(sid, f)
        }
        fn request_redraw(&self, sid: u64) {
            self.0.request_redraw(sid);
        }
        fn subscribe(&self, sid: u64) -> Box<dyn ChangeWait + '_> {
            self.0.subscribe(sid)
        }
        fn clipboard_set(&self, text: &str) -> bool {
            self.0.clipboard_set(text)
        }
        fn native_cwd<'p>(&self, reported: &'p str) -> std::borrow::Cow<'p, str> {
            std::borrow::Cow::Owned(format!("native:{reported}"))
        }
    }

    let prompt = b"\x1b]7;file:///C:/Users//x\x07\x1b]133;A\x07$ ";
    let plain = MemoryHost::new(0);
    plain.feed(prompt);
    let listed = selection::cmd_blocks(&plain, 0, "");
    assert!(listed.contains(" cwd=/C:/Users//x "), "{listed}");
    let json = selection::cmd_blocks_json(&plain, 0, "");
    assert!(json.contains("\"cwd\":\"/C:/Users//x\""), "{json}");

    let renaming = Renaming(MemoryHost::new(0));
    renaming.0.feed(prompt);
    let listed = selection::cmd_blocks(&renaming, 0, "");
    assert!(listed.contains(" cwd=native:/C:/Users//x "), "{listed}");
    let json = selection::cmd_blocks_json(&renaming, 0, "");
    assert!(json.contains("\"cwd\":\"native:/C:/Users//x\""), "{json}");
}
